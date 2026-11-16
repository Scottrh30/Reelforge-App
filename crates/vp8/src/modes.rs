//! Per-macroblock modes and motion vectors from the first partition (RFC 6386 sections 11, 16
//! and 17).

use crate::boolcoder::BoolDecoder;
use crate::header::{Header, Probs};
use crate::tables::{BMODE_PROB, KF_BMODE_PROB, KF_UV_MODE_PROB, KF_YMODE_PROB, MBSPLITS, MBSPLIT_PROBS, MODE_CONTEXTS, SUB_MV_REF_PROB};

// Macroblock luma modes.
pub const DC_PRED: u8 = 0;
pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const TM_PRED: u8 = 3;
pub const B_PRED: u8 = 4;
pub const NEARESTMV: u8 = 5;
pub const NEARMV: u8 = 6;
pub const ZEROMV: u8 = 7;
pub const NEWMV: u8 = 8;
pub const SPLITMV: u8 = 9;

// Subblock intra modes, in the order the probability tables use.
pub const B_DC: u8 = 0;
pub const B_TM: u8 = 1;
pub const B_VE: u8 = 2;
pub const B_HE: u8 = 3;

// Reference frames.
pub const INTRA: u8 = 0;
pub const LAST: u8 = 1;
pub const GOLDEN: u8 = 2;
pub const ALTREF: u8 = 3;

const YMODE_TREE: [i8; 8] = [-(DC_PRED as i8), 2, 4, 6, -(V_PRED as i8), -(H_PRED as i8), -(TM_PRED as i8), -(B_PRED as i8)];
const KF_YMODE_TREE: [i8; 8] = [-(B_PRED as i8), 2, 4, 6, -(DC_PRED as i8), -(V_PRED as i8), -(H_PRED as i8), -(TM_PRED as i8)];
const UV_MODE_TREE: [i8; 6] = [-(DC_PRED as i8), 2, -(V_PRED as i8), 4, -(H_PRED as i8), -(TM_PRED as i8)];
// DC, TM, VE, HE, LD, RD, VR, VL, HD, HU = 0..9.
const BMODE_TREE: [i8; 18] = [0, 2, -1, 4, -2, 6, 8, 12, -3, 10, -5, -6, -4, 14, -7, 16, -8, -9];
const SMALL_MV_TREE: [i8; 14] = [2, 8, 4, 6, 0, -1, -2, -3, 10, 12, -4, -5, -6, -7];

/// A motion vector in the units the bitstream's values are doubled to (eighths of a luma pixel;
/// luma vectors are always even, i.e. quarter-pixel).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    fn is_zero(self) -> bool {
        self == Mv::ZERO
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MbInfo {
    pub ymode: u8,
    pub uv_mode: u8,
    pub ref_frame: u8,
    pub segment: u8,
    /// Signalled as having no coefficients (later also set when it turned out to have none).
    pub skip: bool,
    pub mv: Mv,
    /// Subblock intra modes (B_PRED) or the implied ones of the macroblock's mode.
    pub bmodes: [u8; 16],
    /// Subblock vectors (SPLITMV; for others all equal `mv`).
    pub bmvs: [Mv; 16],
}

impl Default for MbInfo {
    fn default() -> Self {
        MbInfo { ymode: DC_PRED, uv_mode: DC_PRED, ref_frame: INTRA, segment: 0, skip: false, mv: Mv::ZERO, bmodes: [B_DC; 16], bmvs: [Mv::ZERO; 16] }
    }
}

/// The subblock mode a neighbour presents as context on key frames.
fn implied_bmode(ymode: u8) -> u8 {
    match ymode {
        V_PRED => B_VE,
        H_PRED => B_HE,
        TM_PRED => B_TM,
        _ => B_DC,
    }
}

/// The macroblock info grid with a border row above and a border column to the left (border
/// entries stay at their defaults: intra, DC, zero vectors).
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub info: Vec<MbInfo>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let n = (cols + 1).saturating_mul(rows + 1);
        Grid { cols, rows, info: vec![MbInfo::default(); n] }
    }
    #[inline]
    pub fn idx(&self, row: usize, col: usize) -> usize {
        (row + 1) * (self.cols + 1) + col + 1
    }
    #[inline]
    pub fn at(&self, row: usize, col: usize) -> &MbInfo {
        // Indices come from the decode loops, always in range; a default keeps it total.
        static DEFAULT: MbInfo = MbInfo { ymode: DC_PRED, uv_mode: DC_PRED, ref_frame: INTRA, segment: 0, skip: false, mv: Mv::ZERO, bmodes: [B_DC; 16], bmvs: [Mv::ZERO; 16] };
        self.info.get(self.idx(row, col)).unwrap_or(&DEFAULT)
    }
    /// Neighbours by offset from (row, col) of the grid with its border: `dr`, `dc` ∈ {-1, 0}.
    #[inline]
    fn nb(&self, row: usize, col: usize, dr: usize, dc: usize) -> MbInfo {
        let i = (row + 1 - dr) * (self.cols + 1) + col + 1 - dc;
        self.info.get(i).copied().unwrap_or_default()
    }
}

pub struct ModeCtx<'a> {
    pub h: &'a Header,
    pub probs: &'a Probs,
    pub key: bool,
    /// Indexed by reference frame.
    pub sign_bias: [bool; 4],
}

fn read_mv_component(bd: &mut BoolDecoder, p: &[u8; 19]) -> i16 {
    const IS_SHORT: usize = 0;
    const SIGN: usize = 1;
    const SHORT: usize = 2;
    const BITS: usize = 9;
    let mut x: i32;
    if bd.read(p[IS_SHORT]) {
        x = 0;
        for i in 0..3 {
            x += i32::from(bd.read(p[BITS + i])) << i;
        }
        for i in (4..10).rev() {
            x += i32::from(bd.read(p[BITS + i])) << i;
        }
        if x & 0xfff0 == 0 || bd.read(p[BITS + 3]) {
            x += 8;
        }
    } else {
        x = i32::from(bd.tree(&SMALL_MV_TREE, &p[SHORT..SHORT + 7]));
    }
    if x != 0 && bd.read(p[SIGN]) {
        x = -x;
    }
    x as i16
}

fn read_mv(bd: &mut BoolDecoder, probs: &Probs) -> Mv {
    let row = read_mv_component(bd, &probs.mv[0]).wrapping_mul(2);
    let col = read_mv_component(bd, &probs.mv[1]).wrapping_mul(2);
    Mv { row, col }
}

/// Vector bounds of a macroblock: how far (in eighths of a pixel) it may point outside the
/// frame, used to clamp predicted vectors.
#[derive(Clone, Copy)]
struct Bounds {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

impl Bounds {
    fn new(row: usize, col: usize, rows: usize, cols: usize) -> Self {
        const MARGIN: i32 = 16 << 3;
        let c = col as i32;
        let r = row as i32;
        Bounds {
            left: -((c * 16) << 3) - MARGIN,
            right: (((cols as i32) - 1 - c) * 16 << 3) + MARGIN,
            top: -((r * 16) << 3) - MARGIN,
            bottom: (((rows as i32) - 1 - r) * 16 << 3) + MARGIN,
        }
    }
    fn clamp(self, mv: Mv) -> Mv {
        let col = i32::from(mv.col).clamp(self.left, self.right.max(self.left));
        let row = i32::from(mv.row).clamp(self.top, self.bottom.max(self.top));
        Mv { row: row as i16, col: col as i16 }
    }
}

/// Read the modes (and vectors) of the macroblock at (row, col) into the grid.
pub fn read_mb(bd: &mut BoolDecoder, cx: &ModeCtx, grid: &mut Grid, row: usize, col: usize) {
    let h = cx.h;
    let idx = grid.idx(row, col);
    let Some(mut mb) = grid.info.get(idx).copied() else { return };
    if h.update_seg_map {
        let p = &h.seg_tree_probs;
        mb.segment = if bd.read(p[0]) { 2 + u8::from(bd.read(p[2])) } else { u8::from(bd.read(p[1])) };
    } else if cx.key {
        mb.segment = 0;
    }
    mb.skip = h.skip_enabled && bd.read(h.prob_skip);
    mb.mv = Mv::ZERO;
    mb.bmvs = [Mv::ZERO; 16];
    if cx.key {
        mb.ref_frame = INTRA;
        mb.ymode = bd.tree(&KF_YMODE_TREE, &KF_YMODE_PROB);
        if mb.ymode == B_PRED {
            let above = grid.nb(row, col, 1, 0);
            let left = grid.nb(row, col, 0, 1);
            for b in 0..16 {
                let a = if b < 4 { if above.ymode == B_PRED { above.bmodes[b + 12] } else { implied_bmode(above.ymode) } } else { mb.bmodes[b - 4] };
                let l = if b & 3 == 0 { if left.ymode == B_PRED { left.bmodes[b + 3] } else { implied_bmode(left.ymode) } } else { mb.bmodes[b - 1] };
                let probs = &KF_BMODE_PROB[usize::from(a.min(9))][usize::from(l.min(9))];
                mb.bmodes[b] = bd.tree(&BMODE_TREE, probs);
            }
        } else {
            mb.bmodes = [implied_bmode(mb.ymode); 16];
        }
        mb.uv_mode = bd.tree(&UV_MODE_TREE, &KF_UV_MODE_PROB);
    } else if bd.read(h.prob_intra) {
        read_inter(bd, cx, grid, row, col, &mut mb);
    } else {
        mb.ref_frame = INTRA;
        mb.ymode = bd.tree(&YMODE_TREE, &cx.probs.ymode);
        if mb.ymode == B_PRED {
            for b in &mut mb.bmodes {
                *b = bd.tree(&BMODE_TREE, &BMODE_PROB);
            }
        } else {
            mb.bmodes = [implied_bmode(mb.ymode); 16];
        }
        mb.uv_mode = bd.tree(&UV_MODE_TREE, &cx.probs.uv_mode);
    }
    if let Some(slot) = grid.info.get_mut(idx) {
        *slot = mb;
    }
}

fn read_inter(bd: &mut BoolDecoder, cx: &ModeCtx, grid: &Grid, row: usize, col: usize, mb: &mut MbInfo) {
    let h = cx.h;
    mb.ref_frame = if bd.read(h.prob_last) { GOLDEN + u8::from(bd.read(h.prob_golden)) } else { LAST };
    let bias = |r: u8| cx.sign_bias.get(usize::from(r)).copied().unwrap_or(false);
    let my_bias = bias(mb.ref_frame);

    // Near vector search over above, left and above-left (RFC 6386 section 16.3).
    let above = grid.nb(row, col, 1, 0);
    let left = grid.nb(row, col, 0, 1);
    let above_left = grid.nb(row, col, 1, 1);
    let mut near = [Mv::ZERO; 4];
    let mut cnt = [0u8; 4];
    let mut n = 0usize; // index of the last vector added
    for (nb, weight) in [(above, 2u8), (left, 2), (above_left, 1)] {
        if nb.ref_frame == INTRA {
            continue;
        }
        if nb.mv.is_zero() {
            cnt[0] += weight;
            continue;
        }
        let mut mv = nb.mv;
        if bias(nb.ref_frame) != my_bias {
            mv = Mv { row: mv.row.wrapping_neg(), col: mv.col.wrapping_neg() };
        }
        if n == 0 || mv != near[n] {
            n += 1;
            near[n] = mv;
        }
        cnt[n] += weight;
    }
    // libvpx's quirk: the above neighbour with a zero vector counts toward CNT_INTRA only through
    // the pointer that hasn't moved; same outcome as above (cnt[0] += 2).

    if !bd.read(MODE_CONTEXTS[usize::from(cnt[0].min(5))][0]) {
        mb.ymode = ZEROMV;
        mb.mv = Mv::ZERO;
        mb.bmvs = [Mv::ZERO; 16];
        return;
    }
    // Merge above-left with nearest when there were three distinct vectors.
    if cnt[3] > 0 && near[3] == near[1] {
        cnt[1] += 1;
    }
    if cnt[2] > cnt[1] {
        cnt.swap(1, 2);
        near.swap(1, 2);
    }
    let bounds = Bounds::new(row, col, grid.rows, grid.cols);
    if !bd.read(MODE_CONTEXTS[usize::from(cnt[1].min(5))][1]) {
        mb.ymode = NEARESTMV;
        mb.mv = bounds.clamp(near[1]);
    } else if !bd.read(MODE_CONTEXTS[usize::from(cnt[2].min(5))][2]) {
        mb.ymode = NEARMV;
        mb.mv = bounds.clamp(near[2]);
    } else {
        let best = bounds.clamp(if cnt[1] >= cnt[0] { near[1] } else { near[0] });
        let splits = (u8::from(above.ymode == SPLITMV) + u8::from(left.ymode == SPLITMV)) * 2 + u8::from(above_left.ymode == SPLITMV);
        if bd.read(MODE_CONTEXTS[usize::from(splits.min(5))][3]) {
            read_split(bd, cx.probs, &above, &left, mb, best);
            mb.ymode = SPLITMV;
            mb.mv = mb.bmvs[15];
            return;
        }
        let d = read_mv(bd, cx.probs);
        mb.ymode = NEWMV;
        mb.mv = Mv { row: d.row.wrapping_add(best.row), col: d.col.wrapping_add(best.col) };
    }
    mb.bmvs = [mb.mv; 16];
}

fn read_split(bd: &mut BoolDecoder, probs: &Probs, above: &MbInfo, left: &MbInfo, mb: &mut MbInfo, best: Mv) {
    // 3: sixteen 4x4, 2: four 8x8, 0: 16x8 (top, bottom), 1: 8x16 (left, right).
    let s = if !bd.read(MBSPLIT_PROBS[0]) {
        3
    } else if !bd.read(MBSPLIT_PROBS[1]) {
        2
    } else {
        usize::from(bd.read(MBSPLIT_PROBS[2]))
    };
    let parts = [2usize, 2, 4, 16][s];
    let map = &MBSPLITS[s];
    for j in 0..parts {
        let Some(k) = map.iter().position(|&p| usize::from(p) == j) else { continue };
        let left_mv = if k & 3 == 0 { if left.ymode == SPLITMV { left.bmvs[k + 3] } else { left.mv } } else { mb.bmvs[k - 1] };
        let above_mv = if k < 4 { if above.ymode == SPLITMV { above.bmvs[k + 12] } else { above.mv } } else { mb.bmvs[k - 4] };
        let ctx = match (left_mv == above_mv, above_mv.is_zero(), left_mv.is_zero()) {
            (true, _, true) => 4,  // left and above both zero
            (true, _, false) => 3, // left == above
            (false, true, _) => 2, // above zero
            (false, false, true) => 1,
            _ => 0,
        };
        let p = &SUB_MV_REF_PROB[ctx];
        let mv = if !bd.read(p[0]) {
            left_mv
        } else if !bd.read(p[1]) {
            above_mv
        } else if !bd.read(p[2]) {
            Mv::ZERO
        } else {
            let d = read_mv(bd, probs);
            Mv { row: d.row.wrapping_add(best.row), col: d.col.wrapping_add(best.col) }
        };
        for (b, &p) in map.iter().enumerate() {
            if usize::from(p) == j {
                mb.bmvs[b] = mv;
            }
        }
    }
}
