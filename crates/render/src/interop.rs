//! Zero-copy video: frames the hardware decoder left on the graphics card.
//!
//! The decoder (Direct3D 11, in the media crate) copies each frame within
//! video memory into a shared NV12/P010 texture and signals a shared fence.
//! Here the texture is opened in Direct3D 12 (wgpu's backend on Windows), each
//! plane wrapped as an ordinary wgpu texture (`R8Uint` + `Rg8Uint`, or the
//! 16-bit pair for P010), and the usual YUV-to-RGB pass reads them, after the
//! queue waits on the fence. Nothing goes through main memory.
//!
//! Any failure is reported to the caller, which switches zero-copy off
//! (`reelforge_core::frame::zero_copy::fail`) and draws the frame the old way.

use crate::yuv::YuvConverter;
use reelforge_core::frame::{GpuFrame, YuvLayout};
use std::collections::{HashMap, VecDeque};
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource};

/// Shared textures kept open at once (older ones are closed).
const MAX_OPEN: usize = 64;

struct Opened {
    group: (wgpu::BindGroup, wgpu::Buffer),
    // The views and textures stay alive with the bind group, but are kept here too
    // so the D3D12 resource is released when this is dropped.
    _views: [wgpu::TextureView; 2],
    _textures: [wgpu::Texture; 2],
}

pub(crate) struct Importer {
    luid: u64,
    opened: HashMap<u64, Opened>,
    order: VecDeque<u64>,
    fences: HashMap<u64, ID3D12Fence>,
}

/// A LUID as one number (the same packing the media crate uses).
pub(crate) fn luid_u64(l: windows::Win32::Foundation::LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

fn d3d12(device: &wgpu::Device) -> Option<ID3D12Device> {
    // SAFETY: only the raw device handle is cloned (a COM reference); wgpu's own
    // state isn't touched.
    unsafe { device.as_hal::<Dx12>() }.map(|d| d.raw_device().clone())
}

impl Importer {
    /// Ready if `device` runs on Direct3D 12 (the Windows default).
    pub fn new(device: &wgpu::Device) -> Option<Self> {
        let d = d3d12(device)?;
        // SAFETY: a plain query on a live device.
        let luid = luid_u64(unsafe { d.GetAdapterLuid() });
        Some(Self { luid, opened: HashMap::new(), order: VecDeque::new(), fences: HashMap::new() })
    }

    /// The graphics adapter the compositor runs on.
    pub fn adapter(&self) -> u64 {
        self.luid
    }

    /// Convert `g` into `target` straight from its shared texture.
    pub fn convert(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, yuv: &YuvConverter, g: &GpuFrame, target: &wgpu::TextureView) -> Result<(), String> {
        if g.adapter != self.luid {
            return Err("the video decoder is on a different graphics adapter from the compositor".into());
        }
        if !matches!(g.layout, YuvLayout::Nv12 | YuvLayout::P010) {
            return Err(format!("unexpected shared frame layout {:?}", g.layout));
        }
        if let std::collections::hash_map::Entry::Vacant(v) = self.opened.entry(g.texture_id) {
            v.insert(Self::open(device, yuv, g)?);
            self.order.push_back(g.texture_id);
            while self.order.len() > MAX_OPEN {
                if let Some(old) = self.order.pop_front() {
                    self.opened.remove(&old);
                }
            }
        }
        let fence = self.fence(device, g.fence)?;
        {
            // SAFETY: staging a wait on the queue wgpu submits to; the fence is a
            // live shared fence the decoder signals once the frame is copied in.
            let q = unsafe { queue.as_hal::<Dx12>() }.ok_or("the compositor isn't on Direct3D 12")?;
            q.add_wait_fence(fence, g.fence_value);
        }
        let o = self.opened.get(&g.texture_id).ok_or("the shared frame's texture was let go")?;
        yuv.convert_bound(device, queue, &o.group, &g.params(), g.layout, [g.width, g.height], target, g.layout.deep());
        // The decoder mustn't reuse the texture until the GPU has read it.
        let hold = g.hold.clone();
        queue.on_submitted_work_done(move || drop(hold));
        Ok(())
    }

    fn open(device: &wgpu::Device, yuv: &YuvConverter, g: &GpuFrame) -> Result<Opened, String> {
        let d = d3d12(device).ok_or("the compositor isn't on Direct3D 12")?;
        let mut res: Option<ID3D12Resource> = None;
        // SAFETY: `g.texture` is an NT handle of a shared texture made in this
        // process by the decoder, kept open while `g.hold` lives (it does: we hold `g`).
        unsafe { d.OpenSharedHandle(HANDLE(g.texture as usize as *mut _), &mut res) }.map_err(|e| format!("couldn't open the shared video texture in Direct3D 12 ({e})"))?;
        let res = res.ok_or("Direct3D 12 returned no texture")?;
        let wide = g.layout == YuvLayout::P010;
        let (f0, f1) = if wide { (wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint) } else { (wgpu::TextureFormat::R8Uint, wgpu::TextureFormat::Rg8Uint) };
        let [aw, ah] = g.alloc;
        let make = |plane: u32, format: wgpu::TextureFormat, w: u32, h: u32| -> wgpu::Texture {
            let size = wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 };
            // SAFETY: `res` is a live 2D NV12/P010 resource of this device (opened
            // above) of `alloc` size; each wrapper covers one plane with a format
            // that plane allows for shader reads. The two wrappers are only ever
            // read (never written or copied), together, so their state tracking
            // can't conflict. The resource sits in the COMMON state, which D3D12
            // promotes to a shader-read state on first use.
            let hal = unsafe { wgpu::hal::dx12::Device::texture_from_raw(res.clone(), format, wgpu::TextureDimension::D2, size, 1, 1) }.with_plane_slice(plane);
            let desc = wgpu::TextureDescriptor {
                label: Some(if plane == 0 { "shared video frame: luma" } else { "shared video frame: chroma" }),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            };
            // SAFETY: as above; the texture holds a picture already (not uninitialised).
            unsafe { device.create_texture_from_hal::<Dx12>(hal, &desc, wgpu::TextureUses::RESOURCE) }
        };
        let textures = [make(0, f0, aw, ah), make(1, f1, aw.div_ceil(2), ah.div_ceil(2))];
        let views = [textures[0].create_view(&Default::default()), textures[1].create_view(&Default::default())];
        let group = yuv.bind_planes(device, &views[0], &views[1]);
        Ok(Opened { group, _views: views, _textures: textures })
    }

    fn fence(&mut self, device: &wgpu::Device, handle: u64) -> Result<ID3D12Fence, String> {
        if let Some(f) = self.fences.get(&handle) {
            return Ok(f.clone());
        }
        let d = d3d12(device).ok_or("the compositor isn't on Direct3D 12")?;
        let mut f: Option<ID3D12Fence> = None;
        // SAFETY: an NT handle of a shared fence made in this process by the decoder,
        // which keeps it open for as long as it exists.
        unsafe { d.OpenSharedHandle(HANDLE(handle as usize as *mut _), &mut f) }.map_err(|e| format!("couldn't open the decoder's shared fence ({e})"))?;
        let f = f.ok_or("Direct3D 12 returned no fence")?;
        self.fences.insert(handle, f.clone());
        Ok(f)
    }
}
