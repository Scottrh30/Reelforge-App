//! Picture buffers and per-macroblock state.

use std::sync::{Arc, OnceLock};

/// A sample: `u8` for 8-bit streams, `u16` for 9 to 12 bits (High 10, High 4:2:2).
pub trait Pel: Copy + Default + Send + Sync + 'static + PartialEq + std::fmt::Debug {
    /// 8-bit samples (whose arithmetic fits 16-bit lanes).
    const NARROW: bool;
    fn i(self) -> i32;
    /// From a value already in range.
    fn p(v: i32) -> Self;
    /// Eight consecutive samples as i16 (deblocking lanes); None if `s` is too short.
    fn get8(s: &[Self]) -> Option<[i16; 8]>;
    fn put8(d: &mut [Self], v: [i16; 8]);
    /// An empty buffer with room for `n` samples (output planes).
    fn alloc(n: usize) -> Vec<Self>;
    fn of(f: &AnyFrame) -> Option<&Arc<Frame<Self>>>;
}

impl Pel for u8 {
    const NARROW: bool = true;
    #[inline(always)]
    fn i(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn p(v: i32) -> Self {
        v as u8
    }
    #[inline(always)]
    fn get8(s: &[u8]) -> Option<[i16; 8]> {
        // One little-endian word: byte k = sample k.
        let w = u64::from_le_bytes(*s.first_chunk::<8>()?);
        Some(std::array::from_fn(|k| ((w >> (8 * k)) & 0xff) as i16))
    }
    #[inline(always)]
    fn put8(d: &mut [u8], v: [i16; 8]) {
        let w = (0..8).fold(0u64, |acc, k| acc | ((v[k] as u8 as u64) << (8 * k)));
        if let Some(d) = d.first_chunk_mut::<8>() {
            *d = w.to_le_bytes();
        }
    }
    fn alloc(n: usize) -> Vec<u8> {
        plane_allocator()(n)
    }
    fn of(f: &AnyFrame) -> Option<&Arc<Frame<u8>>> {
        match f {
            AnyFrame::Eight(f) => Some(f),
            AnyFrame::Deep(_) => None,
        }
    }
}

impl Pel for u16 {
    const NARROW: bool = false;
    #[inline(always)]
    fn i(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn p(v: i32) -> Self {
        v as u16
    }
    #[inline(always)]
    fn get8(s: &[u16]) -> Option<[i16; 8]> {
        let s = s.first_chunk::<8>()?;
        Some(s.map(|v| v as i16))
    }
    #[inline(always)]
    fn put8(d: &mut [u16], v: [i16; 8]) {
        if let Some(d) = d.first_chunk_mut::<8>() {
            *d = v.map(|v| v as u16);
        }
    }
    fn alloc(n: usize) -> Vec<u16> {
        Vec::with_capacity(n)
    }
    fn of(f: &AnyFrame) -> Option<&Arc<Frame<u16>>> {
        match f {
            AnyFrame::Deep(f) => Some(f),
            AnyFrame::Eight(_) => None,
        }
    }
}

/// Sample format of a picture: chroma format (0 = monochrome, 1 = 4:2:0, 2 = 4:2:2, 3 =
/// 4:4:4) and bit depths. (Monochrome pictures keep 4:2:0 chroma planes, mid grey.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fmt {
    pub cf: u8,
    pub bd_y: u8,
    pub bd_c: u8,
}

impl Default for Fmt {
    fn default() -> Self {
        Fmt { cf: 1, bd_y: 8, bd_c: 8 }
    }
}

impl Fmt {
    /// Chroma lines per macroblock (8 or 16).
    #[inline]
    pub fn mbch(self) -> usize {
        if self.cf >= 2 { 16 } else { 8 }
    }
    /// Chroma samples across a macroblock (8 or 16).
    #[inline]
    pub fn mbcw(self) -> usize {
        if self.cf == 3 { 16 } else { 8 }
    }
    #[inline]
    pub fn max_y(self) -> i32 {
        (1 << self.bd_y) - 1
    }
    #[inline]
    pub fn max_c(self) -> i32 {
        (1 << self.bd_c) - 1
    }
    /// Whether macroblocks carry chroma syntax of their own (a chroma prediction mode,
    /// chroma bits in coded_block_pattern, chroma residual): 4:2:0 and 4:2:2, not
    /// monochrome or 4:4:4 (whose Cb and Cr are coded as luma is).
    #[inline]
    pub fn chroma_syntax(self) -> bool {
        matches!(self.cf, 1 | 2)
    }
    /// Samples wider than 8 bits.
    pub fn deep(self) -> bool {
        self.bd_y > 8 || self.bd_c > 8
    }
}

/// Planar sample buffers (MB-aligned dimensions, stride == width).
#[derive(Clone)]
pub struct Planes<P: Pel = u8> {
    pub y: Vec<P>,
    pub cb: Vec<P>,
    pub cr: Vec<P>,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
}

impl<P: Pel> Planes<P> {
    pub fn new(width: usize, height: usize, fmt: Fmt) -> Self {
        let (cw, ch) = (width / 16 * fmt.mbcw(), height / 16 * fmt.mbch());
        let mid = P::p(1 << (fmt.bd_c - 1));
        Planes { y: vec![P::default(); width * height], cb: vec![mid; cw * ch], cr: vec![mid; cw * ch], width, height, cwidth: cw, cheight: ch }
    }
    pub fn gray(width: usize, height: usize, fmt: Fmt) -> Self {
        let mut p = Self::new(width, height, fmt);
        p.y.fill(P::p(1 << (fmt.bd_y - 1)));
        p
    }
}

/// Macroblock coding category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MbKind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    IPcm,
    PSkip,
    BSkip,
    BDirect16x16,
    /// Any other P or B inter macroblock.
    Inter,
}

impl MbKind {
    #[inline]
    pub fn is_intra(self) -> bool {
        matches!(self, MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16 | MbKind::IPcm)
    }
}

/// Per-macroblock state kept for the whole picture (neighbour derivations, deblocking, co-located data).
#[derive(Clone, Copy)]
pub struct MbState {
    /// Slice number within the picture; `u32::MAX` = not (yet) decoded.
    pub slice_num: u32,
    pub kind: MbKind,
    pub transform_8x8: bool,
    /// coded_block_pattern: bits 0..3 luma, bits 4..5 chroma.
    pub cbp: u8,
    /// QPY (negative below 0 for streams deeper than 8 bits).
    pub qp: i8,
    /// QPc for Cb / Cr (used by deblocking).
    pub qpc: [i8; 2],
    pub intra_chroma_mode: u8,
    /// Intra4x4PredMode / Intra8x8PredMode per 4x4 block (raster order).
    pub intra_modes: [u8; 16],
    /// CAVLC: TotalCoeff per luma 4x4 block (raster). CABAC: coded_block_flag.
    pub nnz: [u8; 16],
    /// The same for the Cb and Cr blocks of 4:4:4 (coded as luma is).
    pub nnz444: [[u8; 16]; 2],
    /// Same for chroma AC blocks [Cb, Cr][raster 2 wide × 2 (4:2:0) or 4 (4:2:2) tall].
    pub nnz_c: [[u8; 8]; 2],
    /// CABAC coded_block_flag of DC blocks: bit0 luma (I16x16), bit1 Cb, bit2 Cr.
    pub cbf_dc: u8,
    /// Luma 4x4 blocks (raster bit) with non-zero coefficients, for deblocking bS = 2.
    pub nz_mask: u16,
    pub ref_idx: [[i8; 4]; 2],
    /// Motion vectors per 4x4 block (raster order).
    pub mv: [[[i16; 2]; 16]; 2],
    /// |mvd| per 4x4 block (raster), saturated, for CABAC context selection.
    pub mvd: [[[u8; 2]; 16]; 2],
    /// Bit per 8x8 block predicted in direct mode.
    pub direct8x8: u8,
    /// A field macroblock (MBAFF frames).
    pub field: bool,
}

impl MbState {
    /// TotalCoeff / coded_block_flag per 4x4 block of colour plane `p` (0 luma; 1, 2 the Cb
    /// and Cr of 4:4:4).
    #[inline]
    pub fn nnz_of(&self, p: usize) -> &[u8; 16] {
        match p {
            0 => &self.nnz,
            1 => &self.nnz444[0],
            _ => &self.nnz444[1],
        }
    }
    #[inline]
    pub fn nnz_of_mut(&mut self, p: usize) -> &mut [u8; 16] {
        match p {
            0 => &mut self.nnz,
            1 => &mut self.nnz444[0],
            _ => &mut self.nnz444[1],
        }
    }
    #[inline]
    pub fn kind_is_skip(&self) -> bool {
        matches!(self.kind, MbKind::PSkip | MbKind::BSkip)
    }
}

impl Default for MbState {
    fn default() -> Self {
        MbState {
            slice_num: u32::MAX,
            kind: MbKind::None,
            transform_8x8: false,
            cbp: 0,
            qp: 0,
            qpc: [0; 2],
            intra_chroma_mode: 0,
            intra_modes: [2; 16],
            nnz: [0; 16],
            nnz444: [[0; 16]; 2],
            nnz_c: [[0; 8]; 2],
            cbf_dc: 0,
            nz_mask: 0,
            ref_idx: [[-1; 4]; 2],
            mv: [[[0; 2]; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            direct8x8: 0,
            field: false,
        }
    }
}

/// One published macroblock row of a decoded frame: final (deblocked) samples plus the motion data
/// needed as co-located information for direct prediction.
pub struct FrameRow<P: Pel = u8> {
    /// 16 luma lines.
    pub y: Box<[P]>,
    /// 8 (4:2:0) or 16 (4:2:2) lines per chroma component.
    pub cb: Box<[P]>,
    pub cr: Box<[P]>,
    /// Per 4x4 block: index mb_x * 16 + raster.
    pub mv: [Box<[[i16; 2]]>; 2],
    /// Per 8x8 block: index mb_x * 4 + b8.
    pub ref_idx: [Box<[i8]>; 2],
    /// Unique id of the referenced picture per 8x8 block (u32::MAX = none).
    pub ref_id: [Box<[u32]>; 2],
    pub intra: Box<[bool]>,
    /// Field macroblocks (MBAFF), per MB.
    pub field: Box<[bool]>,
}

/// A decoded (or in-progress) frame as stored in the DPB and referenced by later pictures.
///
/// Rows are published in order by the thread decoding the picture; readers block (per row) until the
/// data they need is available, which lets several pictures decode concurrently.
pub struct Frame<P: Pel = u8> {
    pub id: u32,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub mb_w: usize,
    pub fmt: Fmt,
    /// TopFieldOrderCnt and BottomFieldOrderCnt (field macroblocks of MBAFF frames weight
    /// and predict by fields).
    pub field_poc: [i32; 2],
    /// log2 of the chroma lines per MB row (3 or 4).
    mbch_log2: u32,
    rows: Box<[OnceLock<FrameRow<P>>]>,
}

impl<P: Pel> Frame<P> {
    pub fn new(id: u32, poc: i32, mb_w: usize, mb_h: usize, fmt: Fmt) -> Self {
        let mbch = fmt.mbch();
        let mbch_log2 = mbch.trailing_zeros();
        Frame {
            id,
            poc,
            width: mb_w * 16,
            height: mb_h * 16,
            cwidth: mb_w * fmt.mbcw(),
            cheight: mb_h * mbch,
            mb_w,
            fmt,
            field_poc: [poc, poc],
            mbch_log2,
            rows: (0..mb_h).map(|_| OnceLock::new()).collect(),
        }
    }

    pub fn with_field_poc(mut self, field_poc: [i32; 2]) -> Self {
        self.field_poc = field_poc;
        self
    }

    pub fn mb_h(&self) -> usize {
        self.rows.len()
    }

    /// Publish MB row `r` (ignored if already published).
    pub fn publish(&self, r: usize, row: FrameRow<P>) {
        let _ = self.rows[r].set(row);
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows[r].get().is_some()
    }

    /// MB row `r`, waiting until it has been published.
    #[inline]
    pub fn row(&self, r: usize) -> &FrameRow<P> {
        #[cfg(feature = "threads")]
        {
            self.rows[r].wait()
        }
        #[cfg(not(feature = "threads"))]
        {
            self.rows[r].get().expect("reference row decoded before use")
        }
    }

    /// Wait until the whole frame is available.
    pub fn wait_complete(&self) {
        for r in 0..self.rows.len() {
            self.row(r);
        }
    }

    /// Build a published row from a contiguous plane set (rows of MB row `r`) and motion data.
    pub fn make_row(planes: &Planes<P>, r: usize, mbs: &[MbState], ref_ids: &dyn Fn(&MbState, usize, usize, i8) -> u32) -> FrameRow<P> {
        let w = planes.width;
        let cw = planes.cwidth;
        let mb_w = w / 16;
        let mbch = planes.cheight / (planes.height / 16).max(1);
        let y = planes.y[r * 16 * w..(r + 1) * 16 * w].into();
        let cb = planes.cb[r * mbch * cw..(r + 1) * mbch * cw].into();
        let cr = planes.cr[r * mbch * cw..(r + 1) * mbch * cw].into();
        let row_mbs = &mbs[r * mb_w..(r + 1) * mb_w];
        let mut mv = [vec![[0i16; 2]; mb_w * 16], vec![[0i16; 2]; mb_w * 16]];
        let mut ref_idx = [vec![-1i8; mb_w * 4], vec![-1i8; mb_w * 4]];
        let mut ref_id = [vec![u32::MAX; mb_w * 4], vec![u32::MAX; mb_w * 4]];
        let mut intra = vec![true; mb_w];
        let mut field = vec![false; mb_w];
        for (x, st) in row_mbs.iter().enumerate() {
            if st.slice_num == u32::MAX {
                continue;
            }
            intra[x] = st.kind.is_intra();
            field[x] = st.field;
            for l in 0..2 {
                mv[l][x * 16..x * 16 + 16].copy_from_slice(&st.mv[l]);
                for b in 0..4 {
                    let ri = st.ref_idx[l][b];
                    ref_idx[l][x * 4 + b] = ri;
                    if ri >= 0 {
                        ref_id[l][x * 4 + b] = ref_ids(st, r, l, ri);
                    }
                }
            }
        }
        let [mv0, mv1] = mv;
        let [ri0, ri1] = ref_idx;
        let [id0, id1] = ref_id;
        FrameRow { y, cb, cr, mv: [mv0.into(), mv1.into()], ref_idx: [ri0.into(), ri1.into()], ref_id: [id0.into(), id1.into()], intra: intra.into(), field: field.into() }
    }

    /// A frame with every row published from `planes` and no motion (intra), e.g. "non-existing"
    /// frames inferred for frame_num gaps.
    pub fn from_planes(id: u32, poc: i32, planes: &Planes<P>, fmt: Fmt) -> Self {
        let (mb_w, mb_h) = (planes.width / 16, planes.height / 16);
        let f = Frame::new(id, poc, mb_w, mb_h, fmt);
        let blank = vec![MbState::default(); mb_w * mb_h];
        for r in 0..mb_h {
            f.publish(r, Frame::make_row(planes, r, &blank, &|_, _, _, _| u32::MAX));
        }
        f
    }

    /// Publish the rows of a frame coded as two field pictures from the pictures its fields
    /// were decoded into (top, bottom; the same one twice for a field without a pair): the
    /// lines interleaved, and the fields' motion kept as an MBAFF frame of field macroblock
    /// pairs keeps it (row 2y + p is row y of field p), all field macroblocks.
    pub fn publish_from_fields(&self, fields: [&Frame<P>; 2]) {
        let (w, cw, mbch) = (self.width, self.cwidth, 1usize << self.mbch_log2);
        let ok = fields.iter().all(|f| f.width == w && f.cwidth == cw && f.mbch_log2 == self.mbch_log2 && f.mb_h() * 2 >= self.mb_h());
        for r in 0..self.mb_h() {
            if self.is_published(r) {
                continue;
            }
            if !ok {
                let gray = Planes::gray(w, 16 * self.mb_h(), self.fmt);
                let blank = vec![MbState::default(); self.mb_w * self.mb_h()];
                self.publish(r, Frame::make_row(&gray, r, &blank, &|_, _, _, _| u32::MAX));
                continue;
            }
            let lines = |n: usize, per: usize, plane: fn(&FrameRow<P>) -> &[P], width: usize| {
                let mut v = Vec::with_capacity(n * width);
                for i in 0..n {
                    let line = n * r + i;
                    let (f, fl) = (fields[line & 1], line >> 1);
                    let row = f.row(fl / per);
                    let o = (fl % per) * width;
                    v.extend_from_slice(&plane(row)[o..o + width]);
                }
                v.into_boxed_slice()
            };
            let y = lines(16, 16, |r| &r.y, w);
            let cb = lines(mbch, mbch, |r| &r.cb, cw);
            let cr = lines(mbch, mbch, |r| &r.cr, cw);
            let src = fields[r & 1].row(r >> 1);
            self.publish(
                r,
                FrameRow {
                    y,
                    cb,
                    cr,
                    mv: src.mv.clone(),
                    ref_idx: src.ref_idx.clone(),
                    ref_id: src.ref_id.clone(),
                    intra: src.intra.clone(),
                    field: vec![true; self.mb_w].into_boxed_slice(),
                },
            );
        }
    }

    /// Copy the w x h luma window at (x0, y0) (clamped to the picture) into `out` (stride `os`).
    #[inline]
    pub fn luma_window(&self, x0: i32, y0: i32, w: usize, h: usize, out: &mut [P], os: usize) {
        let (width, height) = (self.width, self.height);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height as i32 - 1) as usize;
            let row = self.row(yy >> 4);
            let line = &row.y[(yy & 15) * width..(yy & 15) * width + width];
            copy_line::<24, P>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Copy the w x h luma window at (x0, y0) of the field of parity `par` (0 top, 1 bottom;
    /// clamped to the field) into `out` (stride `os`).
    pub fn luma_window_field(&self, par: usize, x0: i32, y0: i32, w: usize, h: usize, out: &mut [P], os: usize) {
        let (width, fh) = (self.width, self.height / 2);
        for r in 0..h {
            let yy = 2 * (y0 + r as i32).clamp(0, fh as i32 - 1) as usize + par;
            let row = self.row(yy >> 4);
            let line = &row.y[(yy & 15) * width..(yy & 15) * width + width];
            copy_line::<24, P>(line, x0, &mut out[r * os..], w);
        }
    }

    /// The same for chroma component `c`.
    #[allow(clippy::too_many_arguments)]
    pub fn chroma_window_field(&self, c: usize, par: usize, x0: i32, y0: i32, w: usize, h: usize, out: &mut [P], os: usize) {
        let (cw, fh) = (self.cwidth, self.cheight / 2);
        for r in 0..h {
            let yy = 2 * (y0 + r as i32).clamp(0, fh as i32 - 1) as usize + par;
            let row = self.row(yy >> self.mbch_log2);
            let plane = if c == 0 { &row.cb } else { &row.cr };
            let o = (yy & ((1 << self.mbch_log2) - 1)) * cw;
            let line = &plane[o..o + cw];
            copy_line::<16, P>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Copy the w x h window of chroma component `c` at (x0, y0) (clamped) into `out`.
    #[inline]
    pub fn chroma_window(&self, c: usize, x0: i32, y0: i32, w: usize, h: usize, out: &mut [P], os: usize) {
        let (cw, ch) = (self.cwidth, self.cheight);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch as i32 - 1) as usize;
            let row = self.row(yy >> self.mbch_log2);
            let plane = if c == 0 { &row.cb } else { &row.cr };
            let o = (yy & ((1 << self.mbch_log2) - 1)) * cw;
            let line = &plane[o..o + cw];
            copy_line::<16, P>(line, x0, &mut out[r * os..], w);
        }
    }

    /// Crop and copy the frame into planar buffers (waits for completion).
    pub fn copy_cropped(&self, crop: (usize, usize, usize, usize)) -> (Vec<P>, Vec<P>, Vec<P>) {
        let (cx, cy, cw, ch) = crop;
        let mut y = P::alloc(cw * ch);
        for r in cy..cy + ch {
            let row = self.row(r >> 4);
            y.extend_from_slice(&row.y[(r & 15) * self.width + cx..(r & 15) * self.width + cx + cw]);
        }
        let (ccx, ccw) = if self.fmt.cf == 3 { (cx, cw) } else { (cx / 2, cw.div_ceil(2)) };
        let (ccy, cch) = if self.fmt.cf >= 2 { (cy, ch) } else { (cy / 2, ch.div_ceil(2)) };
        let mut u = P::alloc(ccw * cch);
        let mut v = P::alloc(ccw * cch);
        for r in ccy..ccy + cch {
            let row = self.row(r >> self.mbch_log2);
            let o = (r & ((1 << self.mbch_log2) - 1)) * self.cwidth + ccx;
            u.extend_from_slice(&row.cb[o..o + ccw]);
            v.extend_from_slice(&row.cr[o..o + ccw]);
        }
        (y, u, v)
    }
}

static PLANE_ALLOCATOR: std::sync::OnceLock<fn(usize) -> Vec<u8>> = std::sync::OnceLock::new();

/// Where output planes come from: an empty buffer with room for the given number of samples.
/// The host sets it once to recycle the planes of pictures it is done with; the first call wins.
pub fn set_plane_allocator(alloc: fn(usize) -> Vec<u8>) {
    let _ = PLANE_ALLOCATOR.set(alloc);
}

fn plane_allocator() -> fn(usize) -> Vec<u8> {
    PLANE_ALLOCATOR.get().copied().unwrap_or(Vec::with_capacity)
}

/// Copy `w` samples of `line` starting at x0 into `out`; when possible a fixed-size block of `N`
/// bytes is copied instead (cheaper than a variable-length copy; `out` must then have room for `N`).
#[inline(always)]
fn copy_line<const N: usize, P: Pel>(line: &[P], x0: i32, out: &mut [P], w: usize) {
    if w <= N
        && x0 >= 0
        && let Some(src) = line.get(x0 as usize..).and_then(|l| l.first_chunk::<N>())
        && let Some(dst) = out.first_chunk_mut::<N>()
    {
        *dst = *src;
    } else {
        copy_clamped(line, x0, &mut out[..w]);
    }
}

/// Copy `out.len()` samples of `line` starting at x0, replicating edge samples outside the line.
#[inline(always)]
fn copy_clamped<P: Pel>(line: &[P], x0: i32, out: &mut [P]) {
    let w = out.len();
    if x0 >= 0 && x0 as usize + w <= line.len() {
        out.copy_from_slice(&line[x0 as usize..x0 as usize + w]);
    } else {
        let max = line.len() as i32 - 1;
        for (c, o) in out.iter_mut().enumerate() {
            *o = line[(x0 + c as i32).clamp(0, max) as usize];
        }
    }
}

pub type FrameRef<P = u8> = Arc<Frame<P>>;

/// A frame of either sample size, as the DPB keeps them.
#[derive(Clone)]
pub enum AnyFrame {
    Eight(Arc<Frame<u8>>),
    Deep(Arc<Frame<u16>>),
}

impl AnyFrame {
    pub fn id(&self) -> u32 {
        match self {
            AnyFrame::Eight(f) => f.id,
            AnyFrame::Deep(f) => f.id,
        }
    }
    pub fn mb_h(&self) -> usize {
        match self {
            AnyFrame::Eight(f) => f.mb_h(),
            AnyFrame::Deep(f) => f.mb_h(),
        }
    }
    pub fn poc(&self) -> i32 {
        match self {
            AnyFrame::Eight(f) => f.poc,
            AnyFrame::Deep(f) => f.poc,
        }
    }
    pub fn fmt(&self) -> Fmt {
        match self {
            AnyFrame::Eight(f) => f.fmt,
            AnyFrame::Deep(f) => f.fmt,
        }
    }
    pub fn is_published(&self, r: usize) -> bool {
        match self {
            AnyFrame::Eight(f) => f.is_published(r),
            AnyFrame::Deep(f) => f.is_published(r),
        }
    }
    pub fn wait_complete(&self) {
        match self {
            AnyFrame::Eight(f) => f.wait_complete(),
            AnyFrame::Deep(f) => f.wait_complete(),
        }
    }
}

/// An entry of RefPicList0/1 for the current slice: a frame, or a field (field pictures).
#[derive(Clone)]
pub struct RefPic<P: Pel = u8> {
    /// The picture the samples come from: the frame, or the picture a field was decoded into.
    pub frame: FrameRef<P>,
    pub long_term: bool,
    /// Read field `p` of `frame`'s interleaved lines (a field of a frame-coded picture).
    pub read_par: Option<usize>,
    /// The field this reference is (0 top, 1 bottom); None for a frame.
    pub parity: Option<usize>,
    /// PicOrderCnt of the reference (the field's, for a field) and of the frame's two fields.
    pub poc: i32,
    pub field_poc: [i32; 2],
    /// Identity of the referenced frame or field ([`frame_key`] / [`field_key`]).
    pub key: u32,
}

impl<P: Pel> RefPic<P> {
    pub fn poc(&self) -> i32 {
        self.poc
    }
    pub fn id(&self) -> u32 {
        self.key
    }
}

/// The identity of frame `id` as a reference picture (frames' ids are unique; fields of a
/// frame share its id).
#[inline]
pub fn frame_key(id: u32) -> u32 {
    id.wrapping_mul(4) | 3
}

/// The identity of field `par` (0 top, 1 bottom) of frame `id`.
#[inline]
pub fn field_key(id: u32, par: usize) -> u32 {
    id.wrapping_mul(4) | (1 + (par as u32 & 1))
}

/// The field `par` of the frame or field a key names.
#[inline]
pub fn key_field(key: u32, par: usize) -> u32 {
    (key & !3) | (1 + (par as u32 & 1))
}

/// The frame a key names (of a field: the frame it belongs to).
#[inline]
pub fn key_frame(key: u32) -> u32 {
    key | 3
}
