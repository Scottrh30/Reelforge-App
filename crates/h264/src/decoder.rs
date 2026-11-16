//! Top-level decoder: NAL dispatch, picture boundaries, POC, DPB and output.
//!
//! The calling thread parses headers and does all DPB / reference list bookkeeping (which only needs
//! header information). Macroblock decoding of each picture runs as a job — on a thread pool with the
//! `threads` feature — that publishes finished macroblock rows into the picture's shared [`Frame`];
//! jobs of later pictures block per row on the reference data they need, so several pictures decode
//! concurrently (frame-level parallelism).

use crate::dpb::{Dpb, Output, OutputMeta, RefMark};
use crate::error::{Error, Result, ensure, invalid, unsupported};
use crate::params::{Pps, Sps};
use crate::picture::{AnyFrame, Fmt, Frame, MbKind, MbState, Pel, Planes, RefPic, field_key, frame_key};
use crate::slice::{NalHeader, Poc, PocState, SliceHeader, SliceType, is_new_picture, nal_type};
use crate::slicedec::{PicState, SliceDecoder};
use crate::transform::LevelScale;
use crate::{ColorInfo, Picture, Plane};
use reelforge_bitstream::{BitReader, annexb_nals, length_prefixed_nals, unescape_rbsp};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// One slice of a picture job.
struct SliceJob {
    sh: SliceHeader,
    pps: Arc<Pps>,
    sps: Arc<Sps>,
    rbsp: Vec<u8>,
    /// RefPicList0/1.
    refs: [Vec<RefEntry>; 2],
    ls: Arc<LevelScale>,
}

/// An entry of a reference list as the job needs it (see [`RefPic`]).
#[derive(Clone)]
struct RefEntry {
    frame: AnyFrame,
    long_term: bool,
    read_par: Option<usize>,
    parity: Option<usize>,
    poc: i32,
    field_poc: [i32; 2],
    key: u32,
}

/// A field picture being collected: its parity, the frame both fields go into, and for the
/// second field the picture the first one was decoded into.
#[derive(Clone)]
struct FieldPic {
    par: usize,
    full: AnyFrame,
    first: Option<AnyFrame>,
}

/// A first field waiting for its second one.
struct OpenField {
    full: AnyFrame,
    field: AnyFrame,
    par: usize,
    frame_num: u32,
    is_ref: bool,
}

/// A picture whose slices are being collected.
struct PendingPic {
    /// What it decodes into: the frame, or for a field picture the field's own picture.
    frame: AnyFrame,
    field: Option<FieldPic>,
    sps: Arc<Sps>,
    first: SliceHeader,
    poc: Poc,
    pts: i64,
    key: bool,
    slices: Vec<SliceJob>,
}

/// Counters describing what the decoder has seen (useful to check test coverage).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub pictures: u64,
    pub slices_cavlc: u64,
    pub slices_cabac: u64,
    pub slices_i: u64,
    pub slices_p: u64,
    pub slices_b: u64,
    pub mb_i4x4: u64,
    pub mb_i8x8: u64,
    pub mb_i16x16: u64,
    pub mb_pcm: u64,
    pub mb_p_skip: u64,
    pub mb_b_skip: u64,
    pub mb_b_direct16x16: u64,
    pub mb_inter: u64,
    /// Inter macroblocks using the 8x8 transform.
    pub mb_inter_8x8_transform: u64,
    pub mmco_ops: u64,
    pub long_term_marks: u64,
    pub frame_num_gaps: u64,
    pub weighted_slices: u64,
    pub temporal_direct_slices: u64,
    /// Non-reference pictures decoded in draft mode (deblocking skipped).
    pub draft_pictures: u64,
}

/// State shared with decoding jobs.
#[derive(Default)]
struct Shared {
    error: Mutex<Option<Error>>,
    stats: Mutex<DecodeStats>,
    /// Recycled picture buffers.
    pool: Mutex<Vec<PicState<u8>>>,
    pool16: Mutex<Vec<PicState<u16>>>,
}

/// H.264 decoder.
pub struct Decoder {
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb<AnyFrame>,
    poc_state: PocState,
    prev_ref_frame_num: u32,
    pending: Option<PendingPic>,
    open_field: Option<OpenField>,
    /// The waiting first field was an IDR picture (its frame is a key frame).
    pending_key: bool,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    ls_cache: Vec<(Arc<Pps>, Arc<LevelScale>)>,
    /// Pictures leaving the DPB, converted once their decoding job has finished.
    out_queue: VecDeque<Output<AnyFrame>>,
    shared: Arc<Shared>,
    #[cfg(feature = "threads")]
    /// Jobs go to the process-wide worker pool (shared by every open decoder),
    /// or run on the calling thread when this is `None`.
    pool: Option<SharedPool>,
    in_flight: VecDeque<AnyFrame>,
    max_in_flight: usize,
    threads: usize,
    /// Draft mode: skip deblocking of non-reference pictures ([`Decoder::set_draft`]).
    draft: bool,
}

/// The process-wide worker pool (rayon's global one, sized to the cores), so
/// clips playing together share the cores instead of each starting its own
/// threads. Frame jobs only ever wait on frames sent before them, and jobs
/// start in the order sent, so sharing can't deadlock.
#[cfg(feature = "threads")]
struct SharedPool;

#[cfg(feature = "threads")]
impl SharedPool {
    fn spawn_fifo(&self, job: impl FnOnce() + Send + 'static) {
        rayon::spawn_fifo(job);
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// The worker threads [`Decoder::new`] uses: the available cores (at most 16) with the `threads`
/// feature on native targets, 1 otherwise.
pub fn default_threads() -> usize {
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    }
    #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
    {
        1
    }
}

impl Decoder {
    /// A decoder using all available cores (with the `threads` feature).
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    /// A decoder using up to `threads` worker threads (1 = decode on the calling thread).
    pub fn with_threads(threads: usize) -> Self {
        crate::cavlc::init_tables();
        #[cfg(feature = "threads")]
        let pool = (threads > 1 && cfg!(not(target_arch = "wasm32"))).then_some(SharedPool);
        #[cfg(feature = "threads")]
        let workers = if pool.is_some() { threads } else { 1 };
        #[cfg(not(feature = "threads"))]
        let workers = 1;
        Decoder {
            spss: vec![None; 32],
            ppss: vec![None; 256],
            nal_length_size: None,
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            prev_ref_frame_num: 0,
            pending: None,
            open_field: None,
            pending_key: false,
            next_id: 1,
            active_sps: None,
            ls_cache: Vec::new(),
            out_queue: VecDeque::new(),
            shared: Arc::new(Shared::default()),
            #[cfg(feature = "threads")]
            pool,
            in_flight: VecDeque::new(),
            max_in_flight: threads.max(1) + 2,
            threads: workers,
            draft: false,
        }
    }

    /// Worker threads decoding pictures (1: on the calling thread).
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Draft mode (off by default) for reduced-resolution playback: pictures submitted from now
    /// on that are non-reference pictures (`nal_ref_idc` 0) skip the deblocking filter (8.7).
    /// Nothing predicts from a non-reference picture (and deblocking never changes the motion
    /// data used for direct prediction), so every other picture stays bit-exact; the draft
    /// pictures themselves are approximate and flagged [`Picture::draft`]. Never use it for
    /// export or for a frame shown while paused.
    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub fn draft(&self) -> bool {
        self.draft
    }

    /// Configure from an `avcC` (AVCDecoderConfigurationRecord) box payload.
    pub fn from_avcc(avcc: &[u8]) -> Result<Self> {
        let mut d = Decoder::new();
        d.configure_avcc(avcc)?;
        Ok(d)
    }

    /// Parse an `avcC` record: SPS/PPS and the NAL length size used by [`Decoder::decode`].
    pub fn configure_avcc(&mut self, avcc: &[u8]) -> Result<()> {
        ensure!(avcc.len() >= 7, "avcC too short");
        ensure!(avcc[0] == 1, "unsupported avcC version {}", avcc[0]);
        self.nal_length_size = Some((avcc[4] & 3) as usize + 1);
        let mut pos = 5;
        let read_sets = |pos: &mut usize, count: usize, d: &mut Decoder| -> Result<()> {
            for _ in 0..count {
                ensure!(*pos + 2 <= avcc.len(), "avcC truncated");
                let len = u16::from_be_bytes([avcc[*pos], avcc[*pos + 1]]) as usize;
                *pos += 2;
                ensure!(*pos + len <= avcc.len(), "avcC truncated");
                d.handle_nal(&avcc[*pos..*pos + len], 0)?;
                *pos += len;
            }
            Ok(())
        };
        let nsps = (avcc[pos] & 0x1f) as usize;
        pos += 1;
        read_sets(&mut pos, nsps, self)?;
        ensure!(pos < avcc.len(), "avcC truncated");
        let npps = avcc[pos] as usize;
        pos += 1;
        read_sets(&mut pos, npps, self)
    }

    /// Length of NAL length prefixes (from avcC), or None for Annex-B input.
    pub fn nal_length_size(&self) -> Option<usize> {
        self.nal_length_size
    }

    /// Decode one access unit (all NAL units of one picture, Annex-B or length-prefixed per
    /// configuration; a byte stream with several complete access units is accepted too). Returns
    /// pictures that became ready for output, in output order; `pts` travels with the picture of
    /// this access unit through reordering.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let nals = match self.nal_length_size {
            Some(n) => length_prefixed_nals(data, n)?,
            None => annexb_nals(data),
        };
        let mut result = Ok(());
        for nal in nals {
            if let Err(e) = self.handle_nal(nal, pts) {
                result = Err(e);
                break;
            }
        }
        // The access unit is complete: hand its picture to a decoding job.
        self.submit_pending();
        if let Some(e) = self.take_error() {
            result = result.and(Err(e));
        }
        result?;
        // Return finished pictures without blocking on frames still being decoded (bounded latency).
        let max_queue = self.max_in_flight * 2;
        let mut pics = Vec::new();
        while let Some(o) = self.out_queue.front() {
            if !o.frame.is_published(o.frame.mb_h() - 1) && self.out_queue.len() <= max_queue {
                break;
            }
            let Some(o) = self.out_queue.pop_front() else { break };
            pics.push(make_picture(&o));
        }
        Ok(pics)
    }

    /// Output all remaining pictures (end of stream).
    pub fn flush(&mut self) -> Vec<Picture> {
        self.submit_pending();
        self.close_open_field();
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.emit(outs);
        let pics = self.out_queue.drain(..).map(|o| make_picture(&o)).collect();
        while let Some(f) = self.in_flight.pop_front() {
            f.wait_complete();
        }
        pics
    }

    /// Statistics accumulated so far (macroblock counts include finished pictures only).
    pub fn stats(&self) -> DecodeStats {
        self.shared.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn stat(&self, f: impl FnOnce(&mut DecodeStats)) {
        if let Ok(mut s) = self.shared.stats.lock() {
            f(&mut s);
        }
    }

    /// First error reported by a decoding job since the last call (also returned by
    /// [`Decoder::decode`]); useful after [`Decoder::flush`].
    pub fn take_error(&mut self) -> Option<Error> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.is_empty() {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal[0])?;
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let sps = Sps::parse(&unescape_rbsp(&nal[1..]))?;
                let id = sps.id as usize;
                self.spss[id] = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let rbsp = unescape_rbsp(&nal[1..]);
                let spss: Vec<Option<Sps>> = self.spss.iter().map(|s| s.as_ref().map(|s| (**s).clone())).collect();
                let pps = Pps::parse(&rbsp, &spss)?;
                let id = pps.id as usize;
                self.ppss[id] = Some(Arc::new(pps));
            }
            nal_type::SLICE | nal_type::IDR => self.handle_slice(nal, hdr, pts)?,
            nal_type::SLICE_DPA | nal_type::SLICE_DPB | nal_type::SLICE_DPC => {
                return unsupported("data partitioning (Extended profile)");
            }
            nal_type::END_SEQ | nal_type::END_STREAM => self.submit_pending(),
            // SEI, AUD, filler, SPS extension, prefix NAL, subset SPS, auxiliary and extension slices: skipped.
            _ => {}
        }
        Ok(())
    }

    fn level_scale(&mut self, pps: &Arc<Pps>) -> Arc<LevelScale> {
        if let Some((_, ls)) = self.ls_cache.iter().find(|(p, _)| Arc::ptr_eq(p, pps)) {
            return ls.clone();
        }
        let ls: Arc<LevelScale> = Arc::from(LevelScale::new(&pps.scaling));
        self.ls_cache.retain(|(p, _)| self.ppss.iter().flatten().any(|q| Arc::ptr_eq(p, q)));
        self.ls_cache.push((pps.clone(), ls.clone()));
        ls
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let rbsp = unescape_rbsp(&nal[1..]);
        let ppss = &self.ppss;
        let spss = &self.spss;
        let mut found: Option<(Arc<Pps>, Arc<Sps>)> = None;
        let (sh, _, _) = SliceHeader::parse(&rbsp, hdr, |id| {
            let pps = ppss[id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
            let sps = spss[pps.sps_id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
            found = Some((pps.clone(), sps.clone()));
            Ok((&**pps, &**sps))
        })?;
        let Some((pps, sps)) = found else {
            return Err(Error::MissingParameterSet("PPS".into()));
        };
        sps.check_supported()?;
        if pps.num_slice_groups > 1 {
            return unsupported("slice groups (FMO)");
        }
        if sps.mb_adaptive_frame_field && !sh.field_pic && pps.constrained_intra_pred {
            return unsupported("constrained intra prediction in MBAFF frames");
        }
        if matches!(sh.slice_type, SliceType::Sp | SliceType::Si) {
            return unsupported("SP/SI slices");
        }
        if sh.redundant_pic_cnt > 0 {
            return Ok(()); // redundant slices are ignored
        }
        let new_pic = match &self.pending {
            None => true,
            Some(p) => is_new_picture(&p.first, &sh, &sps) || sh.first_mb_in_slice == 0 || !Arc::ptr_eq(&p.sps, &sps),
        };
        if new_pic {
            self.submit_pending();
            if sh.first_mb_in_slice != 0 && self.active_sps.is_none() {
                return invalid("stream does not start with the first slice of a picture");
            }
            self.start_picture(&sh, &sps, pts)?;
        }
        let ls = self.level_scale(&pps);
        let Some(pending) = self.pending.as_mut() else {
            return invalid("slice without a started picture");
        };
        let refs = match &pending.field {
            None => self.dpb.build_ref_lists(&sh, pending.poc.frame(), sps.max_frame_num(), |e| RefEntry {
                frame: e.frame.clone(),
                long_term: e.mark() == RefMark::Long,
                read_par: None,
                parity: None,
                poc: e.poc,
                field_poc: e.field_poc,
                key: frame_key(e.frame.id()),
            })?,
            Some(fp) => {
                let cur_poc = if fp.par == 0 { pending.poc.top } else { pending.poc.bottom };
                self.dpb.build_ref_lists_field(&sh, fp.par, cur_poc, sps.max_frame_num(), |e, p| {
                    // a field picture's own picture, else the field's lines of the frame
                    let (frame, read_par) = match &e.fields[p] {
                        Some(f) => (f.clone(), None),
                        None => (e.frame.clone(), Some(p)),
                    };
                    RefEntry {
                        frame,
                        long_term: e.marks[p] == RefMark::Long,
                        read_par,
                        parity: Some(p),
                        poc: e.field_poc[p],
                        field_poc: e.field_poc,
                        key: field_key(e.frame.id(), p),
                    }
                })?
            }
        };
        if !sh.slice_type.is_intra() {
            ensure!(!refs[0].is_empty(), "no reference pictures available for inter slice");
            if sh.slice_type.is_b() {
                ensure!(!refs[1].is_empty(), "empty RefPicList1 in B slice");
            }
        }
        let weighted = (pps.weighted_pred && sh.slice_type.is_p()) || (pps.weighted_bipred_idc != 0 && sh.slice_type.is_b());
        let temporal = sh.slice_type.is_b() && !sh.direct_spatial_mv_pred;
        let (cabac, st, mmcos, lt) = (
            pps.entropy_coding_mode,
            sh.slice_type,
            sh.mmcos.len() as u64,
            sh.long_term_reference as u64 + sh.mmcos.iter().filter(|m| m.op == 3 || m.op == 6).count() as u64,
        );
        pending.slices.push(SliceJob { sh, pps, sps, rbsp, refs, ls });
        self.stat(|s| {
            if cabac {
                s.slices_cabac += 1;
            } else {
                s.slices_cavlc += 1;
            }
            match st {
                SliceType::I | SliceType::Si => s.slices_i += 1,
                SliceType::P | SliceType::Sp => s.slices_p += 1,
                SliceType::B => s.slices_b += 1,
            }
            s.weighted_slices += weighted as u64;
            s.temporal_direct_slices += temporal as u64;
            s.mmco_ops += mmcos;
            s.long_term_marks += lt;
        });
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pts: i64) -> Result<()> {
        // Activate SPS; a resolution change flushes the DPB.
        let changed = match &self.active_sps {
            None => true,
            Some(a) => a.width() != sps.width() || a.height() != sps.height() || a.max_dpb_frames() != sps.max_dpb_frames() || a.fmt() != sps.fmt(),
        };
        // a first field followed by anything but its second field stays on its own
        let second = sh.field_pic
            && self.open_field.as_ref().is_some_and(|o| o.frame_num == sh.frame_num && o.par != sh.bottom_field as usize && o.is_ref == (sh.nal_ref_idc != 0))
            && !changed;
        if !second {
            self.close_open_field();
        }
        if changed && self.active_sps.is_some() {
            let mut outs = Vec::new();
            self.dpb.flush(&mut outs);
            self.emit(outs);
            self.dpb.entries.clear();
        }
        self.active_sps = Some(sps.clone());
        self.dpb.capacity = sps.max_dpb_frames();
        self.dpb.max_reorder = sps.max_num_reorder_frames().min(self.dpb.capacity);
        let mb_w = sps.pic_width_in_mbs as usize;
        let mb_h = sps.frame_height_in_mbs() as usize;
        let fmt = sps.fmt();
        if second {
            // the second field of a frame: same frame_num, nothing to infer
        } else if sh.idr {
            let mut outs = Vec::new();
            self.dpb.idr(sh.no_output_of_prior_pics, &mut outs);
            self.emit(outs);
            self.prev_ref_frame_num = 0;
        } else {
            let max = sps.max_frame_num();
            if sh.frame_num != self.prev_ref_frame_num && sh.frame_num != (self.prev_ref_frame_num + 1) % max {
                // frame_num gap (8.2.5.2): insert "non-existing" frames. Their samples are never used by
                // conforming streams; copy the latest decoded frame for concealment.
                let meta = Arc::new(output_meta(sps, pts, false));
                let next_id = &mut self.next_id;
                let poc_state = &mut self.poc_state;
                let last = self.dpb.entries.iter().filter(|e| !e.non_existing).max_by_key(|e| e.frame.id()).map(|e| e.frame.clone());
                let mut make = |_fnum: u32| {
                    let id = *next_id;
                    *next_id += 1;
                    let frame = match &last {
                        Some(f) if fmt.deep() => AnyFrame::Deep(Arc::new(gap_frame(id, u16::of(f), mb_w, mb_h, fmt))),
                        Some(f) => AnyFrame::Eight(Arc::new(gap_frame(id, u8::of(f), mb_w, mb_h, fmt))),
                        None if fmt.deep() => AnyFrame::Deep(Arc::new(gap_frame(id, None, mb_w, mb_h, fmt))),
                        None => AnyFrame::Eight(Arc::new(gap_frame(id, None, mb_w, mb_h, fmt))),
                    };
                    (frame, 0, meta.clone())
                };
                let mut upd = |fnum: u32| poc_state.update_gap_frame(fnum, sps);
                self.shared.stats.lock().map(|mut s| s.frame_num_gaps += 1).ok();
                self.dpb.fill_frame_num_gap(self.prev_ref_frame_num, sh.frame_num, max, sps.max_num_ref_frames as usize, &mut make, &mut upd);
                self.prev_ref_frame_num = (sh.frame_num + max - 1) % max;
            }
        }
        let poc = self.poc_state.compute(sh, sps);
        // MMCO5: the picture's POC becomes relative to itself (tempPicOrderCnt, 8.2.1).
        let frame_poc = if sh.has_mmco5() { 0 } else { poc.frame() };
        let id = self.next_id;
        self.next_id += 1;
        let field_poc = if sh.has_mmco5() { [poc.top - poc.frame(), poc.bottom - poc.frame()] } else { [poc.top, poc.bottom] };
        let new_frame = |id: u32, poc: i32, mb_h: usize, field_poc: [i32; 2]| {
            if fmt.deep() {
                AnyFrame::Deep(Arc::new(Frame::new(id, poc, mb_w, mb_h, fmt).with_field_poc(field_poc)))
            } else {
                AnyFrame::Eight(Arc::new(Frame::new(id, poc, mb_w, mb_h, fmt).with_field_poc(field_poc)))
            }
        };
        let (frame, field) = if sh.field_pic {
            // a field picture decodes into a picture of its own, half the frame's height
            let par = sh.bottom_field as usize;
            let fpoc = field_poc[par];
            let pic = new_frame(id, fpoc, mb_h / 2, [fpoc, fpoc]);
            let fp = match (&self.open_field, second) {
                (Some(o), true) => FieldPic { par, full: o.full.clone(), first: Some(o.field.clone()) },
                _ => {
                    let fid = self.next_id;
                    self.next_id += 1;
                    FieldPic { par, full: new_frame(fid, fpoc, mb_h, [fpoc, fpoc]), first: None }
                }
            };
            (pic, Some(fp))
        } else {
            (new_frame(id, frame_poc, mb_h, field_poc), None)
        };
        let key = sh.idr || (second && self.pending_key);
        self.pending = Some(PendingPic { frame, field, sps: sps.clone(), first: sh.clone(), poc, pts, key, slices: Vec::new() });
        Ok(())
    }

    /// Start decoding the pending picture and do its reference marking / DPB insertion.
    fn submit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let PendingPic { frame, field, sps, first, poc, pts, key, slices } = p;
        let draft = self.draft && first.nal_ref_idc == 0;
        // the second field puts the frame together once both fields are done
        let combine = field.as_ref().and_then(|f| f.first.clone().map(|first| (f.full.clone(), first, f.par)));
        self.dispatch(frame.clone(), slices, draft, combine);
        self.poc_state.update(&first, &poc);
        if first.nal_ref_idc != 0 {
            self.prev_ref_frame_num = if first.has_mmco5() { 0 } else { first.frame_num };
        }
        let meta = Arc::new(OutputMeta { draft, ..output_meta(&sps, pts, key) });
        let mut outs = Vec::new();
        let (max_fn, max_refs) = (sps.max_frame_num(), sps.max_num_ref_frames as usize);
        match field {
            None => {
                let fpoc = frame.poc();
                let field_poc = match &frame {
                    AnyFrame::Eight(f) => f.field_poc,
                    AnyFrame::Deep(f) => f.field_poc,
                };
                self.dpb.store_picture(&first, frame, fpoc, field_poc, max_fn, max_refs, meta, &mut outs);
            }
            Some(fp) => {
                let second = fp.first.is_some();
                let fpoc = frame.poc();
                if !second {
                    self.pending_key = key;
                    self.open_field = Some(OpenField { full: fp.full.clone(), field: frame.clone(), par: fp.par, frame_num: first.frame_num, is_ref: first.nal_ref_idc != 0 });
                } else {
                    self.open_field = None;
                }
                self.dpb.store_field(&first, fp.par, fp.full, frame, fpoc, second, max_fn, max_refs, meta, &mut outs);
            }
        }
        self.emit(outs);
    }

    /// The waiting first field gets no second field: its frame is made from it alone.
    fn close_open_field(&mut self) {
        let Some(o) = self.open_field.take() else { return };
        let job = move || match (&o.full, &o.field) {
            (AnyFrame::Eight(full), AnyFrame::Eight(f)) => full.publish_from_fields([f, f]),
            (AnyFrame::Deep(full), AnyFrame::Deep(f)) => full.publish_from_fields([f, f]),
            (AnyFrame::Eight(full), _) => full.publish_from_fields([full, full]),
            (AnyFrame::Deep(full), _) => full.publish_from_fields([full, full]),
        };
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool {
            pool.spawn_fifo(job);
        } else {
            job();
        }
        #[cfg(not(feature = "threads"))]
        job();
        let mut outs = Vec::new();
        self.dpb.close_open_field(&mut outs);
        self.emit(outs);
    }

    fn dispatch(&mut self, frame: AnyFrame, slices: Vec<SliceJob>, draft: bool, combine: Option<(AnyFrame, AnyFrame, usize)>) {
        let shared = self.shared.clone();
        let keep = frame.clone();
        let job = move || {
            match &frame {
                AnyFrame::Eight(f) => run_job(f.clone(), slices, &shared, &shared.pool, draft),
                AnyFrame::Deep(f) => run_job(f.clone(), slices, &shared, &shared.pool16, draft),
            }
            if let Some((full, first, par)) = combine {
                combine_fields(&full, &first, &frame, par);
            }
        };
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool {
            self.in_flight.retain(|f| !f.is_published(f.mb_h() - 1));
            while self.in_flight.len() >= self.max_in_flight {
                if let Some(f) = self.in_flight.pop_front() {
                    f.wait_complete();
                }
            }
            self.in_flight.push_back(keep);
            pool.spawn_fifo(job);
            return;
        }
        job();
    }

    fn emit(&mut self, outs: Vec<Output<AnyFrame>>) {
        self.out_queue.extend(outs);
    }
}

/// Decode all slices of one picture and publish its rows.
/// A "non-existing" frame for a frame_num gap: a copy of `last` when it fits, else grey.
fn gap_frame<P: Pel>(id: u32, last: Option<&Arc<Frame<P>>>, mb_w: usize, mb_h: usize, fmt: Fmt) -> Frame<P> {
    let planes = match last {
        Some(f) if f.mb_w == mb_w && f.mb_h() == mb_h && f.fmt == fmt => {
            let mut p = Planes::new(mb_w * 16, mb_h * 16, fmt);
            let (y, u, v) = f.copy_cropped((0, 0, mb_w * 16, mb_h * 16));
            p.y = y;
            p.cb = u;
            p.cr = v;
            p
        }
        _ => Planes::gray(mb_w * 16, mb_h * 16, fmt),
    };
    Frame::from_planes(id, 0, &planes, fmt)
}

/// Publish the frame of a field pair from its fields (`second` of parity `par`); the first
/// field's job was sent earlier, so waiting for it can't deadlock.
fn combine_fields(full: &AnyFrame, first: &AnyFrame, second: &AnyFrame, par: usize) {
    fn go<P: Pel>(full: &Frame<P>, first: &Frame<P>, second: &Frame<P>, par: usize) {
        let fields = if par == 1 { [first, second] } else { [second, first] };
        full.publish_from_fields(fields);
    }
    match (full, first, second) {
        (AnyFrame::Eight(a), AnyFrame::Eight(b), AnyFrame::Eight(c)) => go(a, b, c, par),
        (AnyFrame::Deep(a), AnyFrame::Deep(b), AnyFrame::Deep(c)) => go(a, b, c, par),
        (AnyFrame::Eight(a), _, _) => a.publish_from_fields([a, a]),
        (AnyFrame::Deep(a), _, _) => a.publish_from_fields([a, a]),
    }
}

fn run_job<P: Pel>(frame: Arc<Frame<P>>, slices: Vec<SliceJob>, shared: &Shared, pool: &Mutex<Vec<PicState<P>>>, draft: bool) {
    let reuse = pool.lock().ok().and_then(|mut p| p.pop());
    let mut pic = match reuse {
        Some(mut p) if p.mb_w == frame.mb_w && p.mb_h == frame.mb_h() && p.frame.fmt == frame.fmt => {
            p.reset(frame.clone());
            p
        }
        _ => PicState::new(frame.clone()),
    };
    pic.skip_deblock = draft;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut first_err = None;
        for s in &slices {
            if let Err(e) = decode_slice(s, &mut pic) {
                first_err.get_or_insert(e);
            }
        }
        pic.flush_rows(true);
        first_err
    }));
    let err = match result {
        Ok(e) => e,
        Err(_) => {
            // Make sure readers never block on this frame.
            let fallback = Planes::gray(frame.width, frame.height, frame.fmt);
            let blank = vec![MbState::default(); frame.mb_w * frame.mb_h()];
            for r in 0..frame.mb_h() {
                if !frame.is_published(r) {
                    frame.publish(r, Frame::make_row(&fallback, r, &blank, &|_, _, _, _| u32::MAX));
                }
            }
            pic = PicState::new(frame.clone());
            Some(Error::Invalid("internal error while decoding a picture".into()))
        }
    };
    if let Ok(mut s) = shared.stats.lock() {
        s.pictures += 1;
        s.draft_pictures += draft as u64;
        for st in &pic.mbs {
            match st.kind {
                MbKind::I4x4 => s.mb_i4x4 += 1,
                MbKind::I8x8 => s.mb_i8x8 += 1,
                MbKind::I16x16 => s.mb_i16x16 += 1,
                MbKind::IPcm => s.mb_pcm += 1,
                MbKind::PSkip => s.mb_p_skip += 1,
                MbKind::BSkip => s.mb_b_skip += 1,
                MbKind::BDirect16x16 => s.mb_b_direct16x16 += 1,
                MbKind::Inter => s.mb_inter += 1,
                MbKind::None => {}
            }
            if st.transform_8x8 && !st.kind.is_intra() {
                s.mb_inter_8x8_transform += 1;
            }
        }
    }
    if let (Some(e), Ok(mut slot)) = (err, shared.error.lock()) {
        slot.get_or_insert(e);
    }
    if let Ok(mut p) = pool.lock()
        && p.len() < 4
    {
        p.push(pic);
    }
}

fn decode_slice<P: Pel>(s: &SliceJob, pic: &mut PicState<P>) -> Result<()> {
    // The reference frames as this picture's sample type (a stream changes format only at an
    // IDR, which empties the DPB first).
    let mut refs: [Vec<RefPic<P>>; 2] = [Vec::new(), Vec::new()];
    for (l, list) in s.refs.iter().enumerate() {
        for e in list {
            let Some(frame) = P::of(&e.frame) else {
                return invalid("reference picture of another bit depth");
            };
            refs[l].push(RefPic { frame: frame.clone(), long_term: e.long_term, read_par: e.read_par, parity: e.parity, poc: e.poc, field_poc: e.field_poc, key: e.key });
        }
    }
    let mut sd = SliceDecoder::new(&s.sh, &s.pps, &s.sps, pic, &refs, &s.ls)?;
    if s.pps.entropy_coding_mode {
        sd.decode_cabac(&s.rbsp)
    } else {
        let mut r = BitReader::new(&s.rbsp);
        r.seek_bits(s.sh.header_bits);
        sd.decode_cavlc(&mut r)
    }
}

fn output_meta(sps: &Sps, pts: i64, key: bool) -> OutputMeta {
    let vui = sps.vui.clone().unwrap_or_default();
    OutputMeta {
        pts,
        key,
        crop: sps.crop_rect(),
        full_range: vui.full_range,
        colour_primaries: vui.colour_primaries,
        transfer_characteristics: vui.transfer_characteristics,
        matrix_coefficients: vui.matrix_coefficients,
        sar: vui.sar,
        draft: false,
    }
}

fn make_picture(o: &Output<AnyFrame>) -> Picture {
    let (cx, cy, cw, ch) = o.meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    let fmt = o.frame.fmt();
    let (y, u, v) = match &o.frame {
        AnyFrame::Eight(f) => {
            let (y, u, v) = f.copy_cropped((cx, cy, cw, ch));
            (Plane::U8(y), Plane::U8(u), Plane::U8(v))
        }
        AnyFrame::Deep(f) => {
            let (y, u, v) = f.copy_cropped((cx, cy, cw, ch));
            (Plane::U16(y), Plane::U16(u), Plane::U16(v))
        }
    };
    let ccw = cw.div_ceil(2);
    let cch = if fmt.cf == 2 { ch } else { ch.div_ceil(2) };
    Picture {
        width: cw as u32,
        height: ch as u32,
        chroma_width: ccw as u32,
        chroma_height: cch as u32,
        bit_depth: fmt.bd_y,
        bit_depth_chroma: fmt.bd_c,
        y,
        u,
        v,
        y_stride: cw,
        uv_stride: ccw,
        pts: o.meta.pts,
        poc: o.poc,
        key: o.meta.key,
        color: ColorInfo {
            full_range: o.meta.full_range,
            primaries: o.meta.colour_primaries,
            transfer: o.meta.transfer_characteristics,
            matrix: o.meta.matrix_coefficients,
        },
        sar: o.meta.sar,
        draft: o.meta.draft,
    }
}
