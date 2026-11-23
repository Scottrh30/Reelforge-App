//! NVIDIA Maxine Video Effects (RTX graphics cards): AI background removal, background
//! blur, super resolution, upscaling, compression-artefact reduction and noise removal,
//! run on the card's tensor cores.
//!
//! The effects come with NVIDIA's free **Video Effects SDK redistributable** (one
//! installer per RTX generation, from NVIDIA's Broadcast download centre); Reelforge
//! finds it where that installs (`NV_VIDEO_EFFECTS_PATH`, else
//! `C:\Program Files\NVIDIA Corporation\NVIDIA Video Effects`) and loads its two
//! libraries at run time (`NVVideoEffects.dll`, `NVCVImage.dll`), as it does Blackmagic's
//! RAW runtime. Nothing of NVIDIA's ships with Reelforge. Windows only (where the
//! redistributable is); elsewhere [`available`] says no.
//!
//! The calls are NVIDIA's published C API (the Video Effects SDK and NvCVImage API
//! guides): an effect is made by name, given its model folder, a CUDA stream and its
//! input and output images on the card, loaded, then run frame by frame. Pictures go to
//! and from the card with `NvCVImage_Transfer`, which also converts between 8-bit and
//! 0–1 float, chunky and planar.

use anyhow::{bail, Result};
use std::path::PathBuf;

/// What to do to each frame.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NvEffect {
    /// AI green screen: the person (or people) kept, the rest see-through. `quality`:
    /// the slower, better model.
    RemoveBackground { quality: bool },
    /// The background blurred behind the person, `strength` 0–1.
    BlurBackground { strength: f32 },
    /// Super resolution, `scale` 2 or 4 (or 1.5, 3): detail made where there's none.
    /// `clean`: for compressed, lossy footage (removes more artefacts); else for
    /// clean footage (enhances more).
    SuperResolution { scale: f32, clean: bool },
    /// A lighter upscale with sharpening (`strength` 0–1).
    Upscale { scale: f32, strength: f32 },
    /// Compression artefacts (blocking, ringing) taken out. `heavy`: for low-bitrate video.
    ArtifactReduction { heavy: bool },
    /// Low-light camera noise taken out (`strong`: more of it).
    Denoise { strong: bool },
    /// RTX Video Super Resolution (see [`crate::rtxvideo`]): AI upscaling that also cleans
    /// up compression, `scale` 1–4, `quality` 1–4.
    RtxSuperResolution { scale: f32, quality: u32 },
    /// RTX Video HDR: SDR made into HDR10 (BT.2020, PQ), with this look.
    RtxHdr { look: crate::rtxvideo::HdrLook },
}

impl NvEffect {
    /// How much bigger the result is.
    pub fn scale(self) -> f32 {
        match self {
            NvEffect::SuperResolution { scale, .. } | NvEffect::Upscale { scale, .. } | NvEffect::RtxSuperResolution { scale, .. } => scale,
            _ => 1.0,
        }
    }

    /// The result has see-through parts (and wants a format that keeps them).
    pub fn alpha(self) -> bool {
        matches!(self, NvEffect::RemoveBackground { .. })
    }

    /// One of RTX Video's effects (rather than a Video Effects SDK one).
    pub fn rtx(self) -> bool {
        matches!(self, NvEffect::RtxSuperResolution { .. } | NvEffect::RtxHdr { .. })
    }

    /// The result is HDR10 (and wants a 10-bit HDR format).
    pub fn hdr(self) -> bool {
        matches!(self, NvEffect::RtxHdr { .. })
    }

    /// What it needs is there.
    pub fn installed(self) -> bool {
        if self.rtx() {
            crate::rtxvideo::available()
        } else {
            available()
        }
    }

    pub fn label(self) -> String {
        match self {
            NvEffect::RemoveBackground { .. } => "background removed".into(),
            NvEffect::BlurBackground { .. } => "background blurred".into(),
            NvEffect::SuperResolution { scale, .. } => format!("super resolution {scale}x"),
            NvEffect::Upscale { scale, .. } => format!("upscaled {scale}x"),
            NvEffect::ArtifactReduction { .. } => "artefacts reduced".into(),
            NvEffect::Denoise { .. } => "denoised".into(),
            NvEffect::RtxSuperResolution { scale, .. } => format!("RTX super resolution {scale}x"),
            NvEffect::RtxHdr { .. } => "RTX HDR".into(),
        }
    }

    /// The output size for a `w` × `h` input (even numbers, when scaled).
    pub fn out_size(self, w: u32, h: u32) -> (u32, u32) {
        let s = self.scale().clamp(1.0, 4.0);
        if s <= 1.0 {
            return (w, h);
        }
        let even = |v: f32| (((v.round() as u32) / 2) * 2).max(2);
        (even(w as f32 * s), even(h as f32 * s))
    }

    /// Why this effect can't take a `w` × `h` picture (NVIDIA's limits), if it can't.
    pub fn refuses(self, w: u32, h: u32) -> Option<String> {
        match self {
            NvEffect::RemoveBackground { .. } | NvEffect::BlurBackground { .. } if w < 512 || h < 288 => Some("background removal needs a picture at least 512 × 288".into()),
            NvEffect::Denoise { .. } if !(80..=1080).contains(&h) => Some("noise removal takes pictures from 80 to 1080 lines".into()),
            NvEffect::RtxSuperResolution { scale, .. } if !(1.0..=4.0).contains(&scale) || scale <= 1.0 => Some(format!("RTX super resolution scales from 1× to 4×, not {scale}×")),
            NvEffect::SuperResolution { scale, .. } | NvEffect::Upscale { scale, .. } if ![1.5, 2.0, 3.0, 4.0, 4.0 / 3.0].iter().any(|s| (s - scale).abs() < 0.01) => Some(format!("it can't scale {scale}×")),
            _ => None,
        }
    }
}

/// The folder the redistributable installed (with its libraries and models), if it's there.
pub fn install_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let dir = std::env::var_os("NV_VIDEO_EFFECTS_PATH").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files\NVIDIA Corporation\NVIDIA Video Effects"));
    dir.join("NVVideoEffects.dll").is_file().then_some(dir)
}

/// NVIDIA's Video Effects are installed (the menu offers them).
pub fn available() -> bool {
    install_dir().is_some()
}

/// Where to get them.
pub const DOWNLOAD: &str = "https://www.nvidia.com/en-us/geforce/broadcasting/broadcast-sdk/resources/";

#[cfg(windows)]
pub use imp::Runner;

/// Without the redistributable's platform there's nothing to run.
#[cfg(not(windows))]
pub struct Runner;

#[cfg(not(windows))]
impl Runner {
    pub fn new(_fx: NvEffect, _w: u32, _h: u32) -> Result<Runner> {
        bail!("NVIDIA's Video Effects run on Windows only")
    }
    pub fn process(&mut self, _rgba: &[u8]) -> Result<Vec<u8>> {
        bail!("NVIDIA's Video Effects run on Windows only")
    }
}

/// RGBA → BGR, and back (alpha from `a`, or opaque).
pub fn rgba_to_bgr(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect()
}

pub fn bgr_to_rgba(bgr: &[u8], a: Option<&[u8]>) -> Vec<u8> {
    bgr.chunks_exact(3).enumerate().flat_map(|(i, p)| [p[2], p[1], p[0], a.and_then(|a| a.get(i).copied()).unwrap_or(255)]).collect()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::anyhow;
    use std::ffi::{c_char, c_void, CString};
    use std::ptr::null_mut;

    type Status = i32;
    type Handle = *mut c_void;
    type Stream = *mut c_void;

    // NvCVImage_PixelFormat, ComponentType, layout and memory (the NvCVImage API guide).
    const BGR: i32 = 5;
    const RGBA: i32 = 6;
    const A: i32 = 2;
    const U8: i32 = 1;
    const F32: i32 = 7;
    const CHUNKY: u32 = 0;
    const PLANAR: u32 = 1;
    const CPU: u32 = 0;
    const GPU: u32 = 1;

    /// NvCVImage, as the API defines it.
    #[repr(C)]
    struct Image {
        width: u32,
        height: u32,
        pitch: i32,
        pixel_format: i32,
        component_type: i32,
        pixel_bytes: u8,
        component_bytes: u8,
        num_components: u8,
        planar: u8,
        gpu_mem: u8,
        colorspace: u8,
        reserved: [u8; 2],
        pixels: *mut c_void,
        delete_ptr: *mut c_void,
        delete_proc: Option<unsafe extern "C" fn(*mut c_void)>,
        buffer_bytes: u64,
    }

    impl Image {
        fn empty() -> Image {
            Image { width: 0, height: 0, pitch: 0, pixel_format: 0, component_type: 0, pixel_bytes: 0, component_bytes: 0, num_components: 0, planar: 0, gpu_mem: 0, colorspace: 0, reserved: [0; 2], pixels: null_mut(), delete_ptr: null_mut(), delete_proc: None, buffer_bytes: 0 }
        }
    }

    struct Api {
        _vfx: libloading::Library,
        _cv: libloading::Library,
        create: unsafe extern "C" fn(*const c_char, *mut Handle) -> Status,
        destroy: unsafe extern "C" fn(Handle),
        set_image: unsafe extern "C" fn(Handle, *const c_char, *mut Image) -> Status,
        set_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> Status,
        set_u32: unsafe extern "C" fn(Handle, *const c_char, u32) -> Status,
        set_f32: unsafe extern "C" fn(Handle, *const c_char, f32) -> Status,
        set_stream: unsafe extern "C" fn(Handle, *const c_char, Stream) -> Status,
        load: unsafe extern "C" fn(Handle) -> Status,
        run: unsafe extern "C" fn(Handle, i32) -> Status,
        stream_create: unsafe extern "C" fn(*mut Stream) -> Status,
        stream_destroy: unsafe extern "C" fn(Stream),
        alloc_state: Option<unsafe extern "C" fn(Handle, *mut *mut c_void) -> Status>,
        free_state: Option<unsafe extern "C" fn(Handle, *mut c_void) -> Status>,
        set_states: Option<unsafe extern "C" fn(Handle, *const c_char, *mut *mut c_void) -> Status>,
        img_alloc: unsafe extern "C" fn(*mut Image, u32, u32, i32, i32, u32, u32, u32) -> Status,
        img_init: unsafe extern "C" fn(*mut Image, u32, u32, i32, *mut c_void, i32, i32, u32, u32) -> Status,
        img_free: unsafe extern "C" fn(*mut Image),
        transfer: unsafe extern "C" fn(*const Image, *mut Image, f32, Stream, *mut Image) -> Status,
        error: Option<unsafe extern "C" fn(Status) -> *const c_char>,
    }

    // (The libraries and their functions stay loaded; the handles are only used from
    // the thread running the job.)
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    fn api() -> Result<&'static Api> {
        static API: std::sync::OnceLock<std::result::Result<Api, String>> = std::sync::OnceLock::new();
        API.get_or_init(|| load().map_err(|e| format!("{e:#}"))).as_ref().map_err(|e| anyhow!("{e}"))
    }

    fn load() -> Result<Api> {
        let dir = install_dir().ok_or_else(|| anyhow!("NVIDIA's Video Effects aren't installed (get the Video Effects SDK redistributable for your RTX card from {DOWNLOAD})"))?;
        let open = |name: &str| -> Result<libloading::Library> {
            const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
            // SAFETY: NVIDIA's runtime, loaded from where its installer put it; its
            // companion libraries (CUDA, TensorRT) sit beside it.
            unsafe { Ok(libloading::os::windows::Library::load_with_flags(dir.join(name), LOAD_WITH_ALTERED_SEARCH_PATH).map_err(|e| anyhow!("loading {name}: {e}"))?.into()) }
        };
        let cv = open("NVCVImage.dll")?;
        let vfx = open("NVVideoEffects.dll")?;
        // SAFETY: the symbols have the signatures NVIDIA publishes for them.
        unsafe {
            macro_rules! sym {
                ($lib:expr, $name:literal) => {
                    *$lib.get(concat!($name, "\0").as_bytes()).map_err(|e| anyhow!("{}: {e}", $name))?
                };
            }
            macro_rules! opt {
                ($lib:expr, $name:literal) => {
                    $lib.get(concat!($name, "\0").as_bytes()).ok().map(|s| *s)
                };
            }
            Ok(Api {
                create: sym!(vfx, "NvVFX_CreateEffect"),
                destroy: sym!(vfx, "NvVFX_DestroyEffect"),
                set_image: sym!(vfx, "NvVFX_SetImage"),
                set_string: sym!(vfx, "NvVFX_SetString"),
                set_u32: sym!(vfx, "NvVFX_SetU32"),
                set_f32: sym!(vfx, "NvVFX_SetF32"),
                set_stream: sym!(vfx, "NvVFX_SetCudaStream"),
                load: sym!(vfx, "NvVFX_Load"),
                run: sym!(vfx, "NvVFX_Run"),
                stream_create: sym!(vfx, "NvVFX_CudaStreamCreate"),
                stream_destroy: sym!(vfx, "NvVFX_CudaStreamDestroy"),
                alloc_state: opt!(vfx, "NvVFX_AllocateState"),
                free_state: opt!(vfx, "NvVFX_DeallocateState"),
                set_states: opt!(vfx, "NvVFX_SetStateObjectHandleArray"),
                img_alloc: sym!(cv, "NvCVImage_Alloc"),
                img_init: sym!(cv, "NvCVImage_Init"),
                img_free: sym!(cv, "NvCVImage_Dealloc"),
                transfer: sym!(cv, "NvCVImage_Transfer"),
                error: opt!(cv, "NvCV_GetErrorStringFromCode"),
                _vfx: vfx,
                _cv: cv,
            })
        }
    }

    fn cs(s: &str) -> CString {
        CString::new(s).unwrap_or_default()
    }

    /// One effect, set up for one picture size.
    struct Fx {
        api: &'static Api,
        h: Handle,
        state: *mut c_void,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            // SAFETY: made by NvVFX_CreateEffect (and its state by NvVFX_AllocateState).
            unsafe {
                if let (false, Some(free)) = (self.state.is_null(), self.api.free_state) {
                    free(self.h, self.state);
                }
                (self.api.destroy)(self.h);
            }
        }
    }

    /// An image on the card or in memory, freed with the effect.
    struct Img {
        api: &'static Api,
        im: Box<Image>,
        owned: bool,
    }

    impl Drop for Img {
        fn drop(&mut self) {
            if self.owned {
                // SAFETY: allocated by NvCVImage_Alloc.
                unsafe { (self.api.img_free)(&mut *self.im) }
            }
        }
    }

    /// NVIDIA's effect at work on frames of one size.
    pub struct Runner {
        api: &'static Api,
        effect: NvEffect,
        w: u32,
        h: u32,
        stream: Stream,
        /// The main effect, and for a background blur the green screen that finds the background.
        fx: Option<Fx>,
        matte: Option<Fx>,
        src: Img,
        dst: Img,
        _mask: Option<Img>,
        tmp: Img,
    }

    impl Drop for Runner {
        fn drop(&mut self) {
            // The effects go before the stream they run on.
            self.fx.take();
            self.matte.take();
            // SAFETY: made by NvVFX_CudaStreamCreate, and nothing uses it now.
            unsafe { (self.api.stream_destroy)(self.stream) }
        }
    }

    impl Runner {
        pub fn new(effect: NvEffect, w: u32, h: u32) -> Result<Runner> {
            if let Some(why) = effect.refuses(w, h) {
                bail!("{why}");
            }
            if effect.rtx() {
                bail!("RTX Video isn't one of the Video Effects");
            }
            let api = api()?;
            let models = install_dir().map(|d| d.join("models")).unwrap_or_default();
            let mut stream: Stream = null_mut();
            api.check(unsafe { (api.stream_create)(&mut stream) }, "making a CUDA stream")?;
            let (ow, oh) = effect.out_size(w, h);
            let (fmt, ty, layout) = match effect {
                NvEffect::RemoveBackground { .. } | NvEffect::BlurBackground { .. } => (BGR, U8, CHUNKY),
                NvEffect::Upscale { .. } => (RGBA, U8, CHUNKY),
                _ => (BGR, F32, PLANAR),
            };
            let mut src = api.alloc(w, h, fmt, ty, layout, GPU)?;
            let mut dst = match effect {
                NvEffect::RemoveBackground { .. } => api.alloc(w, h, A, U8, CHUNKY, GPU)?,
                _ => api.alloc(ow, oh, fmt, ty, layout, GPU)?,
            };
            let tmp = Img { api, im: Box::new(Image::empty()), owned: true };
            let mk = |names: &[&str]| -> Result<Fx> {
                let mut last = 0;
                for n in names {
                    let mut h: Handle = null_mut();
                    // SAFETY: a C string for the effect's name, and somewhere to put its handle.
                    let r = unsafe { (api.create)(cs(n).as_ptr(), &mut h) };
                    if r == 0 && !h.is_null() {
                        return Ok(Fx { api, h, state: null_mut() });
                    }
                    last = r;
                }
                Err(anyhow!("this NVIDIA Video Effects install hasn't the {} effect ({})", names.first().copied().unwrap_or(""), api.describe(last)))
            };
            // (The names as NVIDIA's header spells them, then as its documentation does.)
            let names: &[&str] = match effect {
                NvEffect::RemoveBackground { .. } => &["GreenScreen", "Green Screen"],
                NvEffect::BlurBackground { .. } => &["BackgroundBlur", "Background Blur"],
                NvEffect::SuperResolution { .. } => &["SuperRes", "Super Res"],
                NvEffect::Upscale { .. } => &["Upscale"],
                NvEffect::ArtifactReduction { .. } => &["ArtifactReduction", "Artifact Reduction"],
                NvEffect::Denoise { .. } => &["Denoising"],
                NvEffect::RtxSuperResolution { .. } | NvEffect::RtxHdr { .. } => bail!("RTX Video isn't one of the Video Effects"),
            };
            let mut fx = mk(names)?;
            let mut matte = None;
            let mut mask = None;
            let common = |f: &Fx| -> Result<()> {
                api.check(unsafe { (api.set_string)(f.h, cs("ModelDir").as_ptr(), cs(&models.to_string_lossy()).as_ptr()) }, "setting its model folder")?;
                api.check(unsafe { (api.set_stream)(f.h, cs("CudaStream").as_ptr(), stream) }, "setting its CUDA stream")
            };
            common(&fx)?;
            // SAFETY (below): NVIDIA's setters, with C strings and images that outlive the effect.
            match effect {
                NvEffect::RemoveBackground { quality } => unsafe {
                    api.check((api.set_u32)(fx.h, cs("Mode").as_ptr(), if quality { 0 } else { 1 }), "setting its mode")?;
                    api.check((api.set_image)(fx.h, cs("SrcImage0").as_ptr(), &mut *src.im as *mut Image), "giving it the picture")?;
                    api.check((api.set_image)(fx.h, cs("DstImage0").as_ptr(), &mut *dst.im as *mut Image), "giving it the matte")?;
                },
                NvEffect::BlurBackground { strength } => unsafe {
                    // The green screen finds the background first.
                    let gs = mk(&["GreenScreen", "Green Screen"])?;
                    common(&gs)?;
                    let mut m = api.alloc(w, h, A, U8, CHUNKY, GPU)?;
                    api.check((api.set_u32)(gs.h, cs("Mode").as_ptr(), 0), "setting the matte's mode")?;
                    api.check((api.set_image)(gs.h, cs("SrcImage0").as_ptr(), &mut *src.im as *mut Image), "giving it the picture")?;
                    api.check((api.set_image)(gs.h, cs("DstImage0").as_ptr(), &mut *m.im as *mut Image), "giving it the matte")?;
                    api.check((api.load)(gs.h), "loading the background model")?;
                    api.check((api.set_f32)(fx.h, cs("Strength").as_ptr(), strength.clamp(0.0, 1.0)), "setting its strength")?;
                    api.check((api.set_image)(fx.h, cs("SrcImage0").as_ptr(), &mut *src.im as *mut Image), "giving it the picture")?;
                    api.check((api.set_image)(fx.h, cs("SrcImage1").as_ptr(), &mut *m.im as *mut Image), "giving it the matte")?;
                    api.check((api.set_image)(fx.h, cs("DstImage0").as_ptr(), &mut *dst.im as *mut Image), "giving it the result")?;
                    matte = Some(gs);
                    mask = Some(m);
                },
                _ => unsafe {
                    match effect {
                        NvEffect::SuperResolution { clean, .. } => api.check((api.set_u32)(fx.h, cs("Mode").as_ptr(), if clean { 0 } else { 1 }), "setting its mode")?,
                        NvEffect::Upscale { strength, .. } => api.check((api.set_f32)(fx.h, cs("Strength").as_ptr(), strength.clamp(0.0, 1.0)), "setting its strength")?,
                        NvEffect::ArtifactReduction { heavy } => api.check((api.set_u32)(fx.h, cs("Mode").as_ptr(), heavy as u32), "setting its mode")?,
                        NvEffect::Denoise { strong } => api.check((api.set_f32)(fx.h, cs("Strength").as_ptr(), if strong { 1.0 } else { 0.0 }), "setting its strength")?,
                        _ => {}
                    }
                    api.check((api.set_image)(fx.h, cs("SrcImage0").as_ptr(), &mut *src.im as *mut Image), "giving it the picture")?;
                    api.check((api.set_image)(fx.h, cs("DstImage0").as_ptr(), &mut *dst.im as *mut Image), "giving it the result")?;
                },
            }
            api.check(unsafe { (api.load)(fx.h) }, "loading its model (is the redistributable for this card's generation installed?)")?;
            // Noise removal remembers earlier frames: its state, made once loaded.
            if let (NvEffect::Denoise { .. }, Some(alloc), Some(set)) = (effect, api.alloc_state, api.set_states) {
                let mut st: *mut c_void = null_mut();
                api.check(unsafe { alloc(fx.h, &mut st) }, "making its state")?;
                fx.state = st;
                let mut arr = [st];
                api.check(unsafe { set(fx.h, cs("State").as_ptr(), arr.as_mut_ptr()) }, "giving it its state")?;
            }
            Ok(Runner { api, effect, w, h, stream, fx: Some(fx), matte, src, dst, _mask: mask, tmp })
        }

        /// One frame (RGBA, the size given) → the result (RGBA, [`NvEffect::out_size`];
        /// see-through outside the person for a background removal).
        pub fn process(&mut self, rgba: &[u8]) -> Result<Vec<u8>> {
            let (w, h) = (self.w, self.h);
            if rgba.len() < (w * h * 4) as usize {
                bail!("the frame is the wrong size");
            }
            let api = self.api;
            let float = matches!(self.src.im.component_type, F32);
            // In: as 8-bit, chunky, in memory → the card (as 0–1 float where it wants that).
            let mut host = match self.effect {
                NvEffect::Upscale { .. } => rgba.to_vec(),
                _ => rgba_to_bgr(rgba),
            };
            let (hfmt, hbytes) = if matches!(self.effect, NvEffect::Upscale { .. }) { (RGBA, 4) } else { (BGR, 3) };
            let hin = api.wrap(w, h, hbytes, hfmt, host.as_mut_ptr())?;
            api.check(unsafe { (api.transfer)(&*hin.im, &mut *self.src.im, if float { 1.0 / 255.0 } else { 1.0 }, null_mut(), &mut *self.tmp.im) }, "sending the frame to the card")?;
            if let Some(m) = &self.matte {
                api.check(unsafe { (api.run)(m.h, 0) }, "finding the background")?;
            }
            let fx = self.fx.as_ref().ok_or_else(|| anyhow!("the effect is gone"))?;
            // SAFETY: a loaded effect with its images set.
            api.check(unsafe { (api.run)(fx.h, 0) }, "running the effect")?;
            drop(hin);
            let (ow, oh) = match self.effect {
                NvEffect::RemoveBackground { .. } => (w, h),
                e => e.out_size(w, h),
            };
            match self.effect {
                NvEffect::RemoveBackground { .. } => {
                    let mut a = vec![0u8; (w * h) as usize];
                    let mut hout = api.wrap(w, h, 1, A, a.as_mut_ptr())?;
                    api.check(unsafe { (api.transfer)(&*self.dst.im, &mut *hout.im, 1.0, null_mut(), &mut *self.tmp.im) }, "reading the matte back")?;
                    drop(hout);
                    Ok(rgba.chunks_exact(4).zip(&a).flat_map(|(p, &k)| [p[0], p[1], p[2], k]).collect())
                }
                NvEffect::Upscale { .. } => {
                    let mut out = vec![0u8; (ow * oh * 4) as usize];
                    let mut hout = api.wrap(ow, oh, 4, RGBA, out.as_mut_ptr())?;
                    api.check(unsafe { (api.transfer)(&*self.dst.im, &mut *hout.im, 1.0, null_mut(), &mut *self.tmp.im) }, "reading the result back")?;
                    drop(hout);
                    Ok(out.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect())
                }
                _ => {
                    let mut bgr = vec![0u8; (ow * oh * 3) as usize];
                    let mut hout = api.wrap(ow, oh, 3, BGR, bgr.as_mut_ptr())?;
                    api.check(unsafe { (api.transfer)(&*self.dst.im, &mut *hout.im, if float { 255.0 } else { 1.0 }, null_mut(), &mut *self.tmp.im) }, "reading the result back")?;
                    drop(hout);
                    Ok(bgr_to_rgba(&bgr, None))
                }
            }
        }
    }

    impl Api {
        fn describe(&self, s: Status) -> String {
            let text = self.error.and_then(|f| {
                // SAFETY: NVIDIA's own static error strings.
                let p = unsafe { f(s) };
                (!p.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
            });
            text.unwrap_or_else(|| format!("error {s}"))
        }

        fn check(&self, s: Status, what: &str) -> Result<()> {
            if s == 0 {
                Ok(())
            } else {
                Err(anyhow!("NVIDIA Video Effects: {what}: {}", self.describe(s)))
            }
        }

        fn alloc(&'static self, w: u32, h: u32, fmt: i32, ty: i32, layout: u32, mem: u32) -> Result<Img> {
            let mut im = Box::new(Image::empty());
            // SAFETY: an empty image for NVIDIA to fill in, freed by Img's drop.
            self.check(unsafe { (self.img_alloc)(&mut *im, w, h, fmt, ty, layout, mem, 1) }, "making room on the card")?;
            Ok(Img { api: self, im, owned: true })
        }

        /// An image over our own memory (8-bit chunky, `bytes` per pixel); not freed by NVIDIA.
        fn wrap(&'static self, w: u32, h: u32, bytes: u32, fmt: i32, pixels: *mut u8) -> Result<Img> {
            let mut im = Box::new(Image::empty());
            // SAFETY: `pixels` holds w × h × bytes, and outlives the image (dropped first by callers).
            self.check(unsafe { (self.img_init)(&mut *im, w, h, (w * bytes) as i32, pixels as *mut c_void, fmt, U8, CHUNKY, CPU) }, "describing the frame")?;
            Ok(Img { api: self, im, owned: false })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_limits_and_colour_order() {
        let sr = NvEffect::SuperResolution { scale: 2.0, clean: true };
        assert_eq!(sr.out_size(960, 540), (1920, 1080));
        assert_eq!(NvEffect::Upscale { scale: 1.5, strength: 0.4 }.out_size(1279, 719), (1918, 1078));
        assert_eq!(NvEffect::Denoise { strong: false }.out_size(640, 360), (640, 360));
        assert!(NvEffect::RemoveBackground { quality: true }.refuses(320, 240).is_some());
        assert!(NvEffect::RemoveBackground { quality: true }.refuses(1920, 1080).is_none());
        assert!(NvEffect::Denoise { strong: true }.refuses(3840, 2160).is_some());
        assert!(NvEffect::SuperResolution { scale: 2.5, clean: true }.refuses(640, 360).is_some());
        assert!(NvEffect::RemoveBackground { quality: false }.alpha() && !sr.alpha());
        let px = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let bgr = rgba_to_bgr(&px);
        assert_eq!(bgr, vec![30, 20, 10, 70, 60, 50]);
        assert_eq!(bgr_to_rgba(&bgr, Some(&[1, 2])), vec![10, 20, 30, 1, 50, 60, 70, 2]);
        assert_eq!(bgr_to_rgba(&bgr, None)[3], 255);
    }

    #[test]
    fn without_the_redistributable_it_says_so() {
        if available() {
            return;
        }
        let e = Runner::new(NvEffect::Denoise { strong: false }, 640, 360).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!e.is_empty());
    }
}
