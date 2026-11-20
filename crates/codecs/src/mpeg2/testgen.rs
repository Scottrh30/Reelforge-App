//! A test-stream generator: valid but random MPEG-2 streams that use the
//! parts of the syntax FFmpeg's encoder never produces (field pictures, P
//! fields referring to the other field of the same frame, 16×8 and
//! dual-prime motion, skipped macroblocks), so the decoder can be checked
//! against FFmpeg's decoder on them. Content is noise-like; only agreement
//! between decoders matters. Motion vectors are kept inside the picture.

use super::tables::*;

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn new() -> Bits {
        Bits { out: Vec::new(), acc: 0, n: 0 }
    }
    fn put(&mut self, v: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn align(&mut self) {
        while self.n != 0 {
            self.put(0, 1);
        }
    }
    fn start(&mut self, code: u8) {
        self.align();
        self.out.extend_from_slice(&[0, 0, 1, code]);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
}

/// Options for one generated stream.
#[derive(Clone, Copy)]
pub struct Options {
    /// Field pictures (else frame pictures with field motion / field DCT).
    pub field_pictures: bool,
    /// 1 = 4:2:0, 2 = 4:2:2.
    pub chroma_format: u32,
    pub seed: u64,
    /// Debugging: B macroblocks limited to (directions 1/2/3, motion type, coded); intra disallowed.
    pub b_only: Option<(u32, u32, bool)>,
}

const W: u32 = 128;
const H: u32 = 96;

fn write_mv(b: &mut Bits, delta: i32, f_code: u32) {
    let shift = f_code - 1;
    if delta == 0 {
        b.put(MV_CODES[0], MV_BITS[0] as u32);
        return;
    }
    let a = delta.unsigned_abs() - 1;
    let code = (a >> shift) + 1;
    b.put(MV_CODES[code as usize], MV_BITS[code as usize] as u32);
    b.put((delta < 0) as u32, 1);
    if shift > 0 {
        b.put(a & ((1 << shift) - 1), shift);
    }
}

/// Encode a vector component `v` against predictor `pred` (range-wrapped like the decoder).
fn mv(b: &mut Bits, v: i32, pred: i32, f_code: u32) {
    let range = 32 << (f_code - 1);
    let mut d = v - pred;
    while d < -range / 2 {
        d += range;
    }
    while d >= range / 2 {
        d -= range;
    }
    write_mv(b, d, f_code);
}

fn write_coeff(b: &mut Bits, run: u32, level: i32, intra_vlc: bool) {
    let (codes, bits): (&[u32], &[u8]) = if intra_vlc { (&B15_CODES, &B15_BITS) } else { (&B14_CODES, &B14_BITS) };
    let found = (0..111).find(|&i| AC_RUN[i] as u32 == run && AC_LEVEL[i] as i32 == level.abs());
    match found {
        Some(i) => {
            b.put(codes[i], bits[i] as u32);
            b.put((level < 0) as u32, 1);
        }
        None => {
            b.put(codes[111], bits[111] as u32);
            b.put(run, 6);
            b.put((level & 0xFFF) as u32, 12);
        }
    }
}

fn write_block(b: &mut Bits, rng: &mut Rng, intra: bool, intra_vlc: bool, comp: usize, dc_pred: &mut [i32; 3]) {
    let mut first = true;
    if intra {
        // A small DC change (keeps the picture near mid-grey).
        let target = 128 + rng.range(-40, 40);
        let diff = target - dc_pred[comp];
        dc_pred[comp] = target;
        let size = if diff == 0 { 0 } else { 32 - diff.unsigned_abs().leading_zeros() };
        if comp == 0 {
            b.put(DC_LUM_CODES[size as usize], DC_LUM_BITS[size as usize] as u32);
        } else {
            b.put(DC_CHROMA_CODES[size as usize], DC_CHROMA_BITS[size as usize] as u32);
        }
        if size > 0 {
            let v = if diff > 0 { diff } else { diff + (1 << size) - 1 };
            b.put(v as u32, size);
        }
        first = false;
    }
    let n = rng.below(6) + if intra { 0 } else { 1 };
    let mut pos = if intra { 0 } else { -1 };
    for _ in 0..n {
        let run = rng.below(6) as i32;
        if pos + run + 1 > 63 {
            break;
        }
        pos += run + 1;
        let mut level = rng.range(1, 6);
        if rng.below(5) == 0 {
            level = rng.range(7, 20); // forces escapes too (small enough never to saturate)
        }
        if rng.below(2) == 0 {
            level = -level;
        }
        if first && !intra && run == 0 && level.abs() == 1 {
            b.put(1, 1);
            b.put((level < 0) as u32, 1);
        } else {
            write_coeff(b, run as u32, level, intra && intra_vlc);
        }
        first = false;
    }
    // End of block.
    if intra && intra_vlc {
        b.put(B15_CODES[112], B15_BITS[112] as u32);
    } else {
        b.put(B14_CODES[112], B14_BITS[112] as u32);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pt {
    I,
    P,
    B,
}

/// Generate a stream of `frames` frames (decode order I P B B P B B …).
pub fn generate(opt: Options, frames: usize) -> Vec<u8> {
    let mut rng = Rng(opt.seed);
    let mut b = Bits::new();
    // Sequence header + extension.
    b.start(0xB3);
    b.put(W, 12);
    b.put(H, 12);
    b.put(1, 4);
    b.put(3, 4); // 25 fps
    b.put(1000, 18);
    b.put(1, 1);
    b.put(100, 10);
    b.put(0, 1);
    b.put(0, 1);
    b.put(0, 1);
    b.start(0xB5);
    b.put(1, 4);
    b.put(if opt.chroma_format == 2 { 0x85 } else { 0x48 }, 8);
    b.put(0, 1); // interlaced sequence
    b.put(opt.chroma_format, 2);
    b.put(0, 2);
    b.put(0, 2);
    b.put(0, 12);
    b.put(1, 1);
    b.put(0, 8);
    b.put(0, 1);
    b.put(0, 2);
    b.put(0, 5);
    b.start(0xB8);
    b.put(0, 25);
    b.put(1, 1); // closed GOP
    b.put(0, 1);

    let mbw = (W / 16) as i32;
    let types: Vec<Pt> = (0..frames).map(|i| if i == 0 { Pt::I } else if i % 3 == 1 { Pt::P } else { Pt::B }).collect();
    for (fi, &pt) in types.iter().enumerate() {
        let structures: Vec<u32> = if opt.field_pictures { vec![1, 2] } else { vec![3] };
        for (si, &structure) in structures.iter().enumerate() {
            // The second field of an I frame is a P field (predicting from the first).
            let kind = if pt == Pt::I && si == 1 { Pt::P } else { pt };
            // That P field has only the first field to refer to (no earlier frame).
            let only_first = pt == Pt::I && si == 1;
            b.start(0x00);
            b.put(fi as u32 & 0x3FF, 10);
            b.put(match kind {
                Pt::I => 1,
                Pt::P => 2,
                Pt::B => 3,
            }, 3);
            b.put(0xFFFF, 16);
            if kind != Pt::I {
                b.put(0, 1);
                b.put(7, 3);
            }
            if kind == Pt::B {
                b.put(0, 1);
                b.put(7, 3);
            }
            b.put(0, 1);
            let f_code = 2u32;
            let intra_vlc = rng.below(2) == 1;
            let alt_scan = rng.below(2) == 1;
            let q_scale_type = rng.below(2) == 1;
            let frame_pred_frame_dct = false;
            b.start(0xB5);
            b.put(8, 4);
            for d in 0..2 {
                for _ in 0..2 {
                    let used = match kind {
                        Pt::I => false,
                        Pt::P => d == 0,
                        Pt::B => true,
                    };
                    b.put(if used { f_code } else { 15 }, 4);
                }
            }
            b.put(0, 2); // dc precision 8
            b.put(structure, 2);
            b.put(1, 1); // top field first
            b.put(frame_pred_frame_dct as u32, 1);
            b.put(0, 1); // concealment
            b.put(q_scale_type as u32, 1);
            b.put(intra_vlc as u32, 1);
            b.put(alt_scan as u32, 1);
            b.put(0, 1);
            b.put(0, 1);
            b.put(0, 1); // progressive frame
            b.put(0, 1);

            let field = structure != 3;
            let rows = if field { (H / 32) as i32 } else { (H / 16) as i32 };
            let nblocks = if opt.chroma_format == 2 { 8 } else { 6 };
            for row in 0..rows {
                b.start(row as u8 + 1);
                b.put(rng.range(2, 6) as u32, 5);
                b.put(0, 1);
                let mut dc = [128i32; 3];
                let mut pmv = [[[0i32; 2]; 2]; 2];
                let mut x = 0;
                let mut first = true;
                while x < mbw {
                    // Skip a macroblock now and then in P pictures.
                    let mut inc = 1;
                    if kind == Pt::P && !only_first && !first && x + 1 < mbw && rng.below(5) == 0 {
                        inc = 2;
                        pmv = [[[0; 2]; 2]; 2];
                        dc = [128; 3];
                    }
                    if first {
                        inc = 1;
                    }
                    let mb_x = x + inc - 1;
                    b.put(MBINCR_CODES[(inc - 1) as usize], MBINCR_BITS[(inc - 1) as usize] as u32);
                    first = false;
                    let interior = mb_x > 0 && mb_x < mbw - 1 && row > 0 && row < rows - 1;
                    let choice = rng.below(10);
                    match kind {
                        Pt::I => {
                            b.put(1, 1);
                            if !field {
                                b.put(rng.below(2), 1); // dct type
                            }
                            pmv = [[[0; 2]; 2]; 2];
                            for n in 0..nblocks {
                                write_block(&mut b, &mut rng, true, intra_vlc, if n < 4 { 0 } else { 1 + (n & 1) }, &mut dc);
                            }
                        }
                        // Edge macroblocks of B pictures are intra (their vectors could point outside).
                        _ if (!interior && (kind == Pt::B || choice < 4 || only_first)) || (choice == 0 && !(kind == Pt::B && opt.b_only.is_some())) => {
                            // Intra.
                            let (c, l) = if kind == Pt::P { (P_TYPES[0].0, P_TYPES[0].1) } else { (B_TYPES[0].0, B_TYPES[0].1) };
                            b.put(c, l as u32);
                            if !field {
                                b.put(rng.below(2), 1);
                            }
                            pmv = [[[0; 2]; 2]; 2];
                            for n in 0..nblocks {
                                write_block(&mut b, &mut rng, true, intra_vlc, if n < 4 { 0 } else { 1 + (n & 1) }, &mut dc);
                            }
                        }
                        Pt::P if !only_first && (!interior || choice < 3) => {
                            // Coded, no motion compensation.
                            b.put(P_TYPES[1].0, P_TYPES[1].1 as u32);
                            if !field {
                                b.put(rng.below(2), 1);
                            }
                            pmv[0] = [[0; 2]; 2];
                            dc = [128; 3];
                            write_coded(&mut b, &mut rng, nblocks, &mut dc);
                        }
                        _ => {
                            // Motion compensated (forward / backward / both), coded or not.
                            let mut dirs = if kind == Pt::P { 1 } else { 1 + rng.below(3) };
                            let mut coded = rng.below(3) != 0;
                            if let (Pt::B, Some((d, _, c))) = (kind, opt.b_only) {
                                dirs = d;
                                coded = c;
                            }
                            let (c, l) = if kind == Pt::P {
                                if coded { (P_TYPES[3].0, P_TYPES[3].1) } else { (P_TYPES[2].0, P_TYPES[2].1) }
                            } else {
                                let i = match (dirs, coded) {
                                    (2, false) => 1,
                                    (2, true) => 2,
                                    (1, false) => 3,
                                    (1, true) => 4,
                                    (_, false) => 5,
                                    (_, true) => 6,
                                };
                                (B_TYPES[i].0, B_TYPES[i].1)
                            };
                            b.put(c, l as u32);
                            // Motion type: field 1, 16x8 (field pics) or frame (frame pics) 2, dual prime 3 (P only, one direction).
                            let mut mt = if only_first { 1 + rng.below(2) } else if kind == Pt::P { 1 + rng.below(3) } else { 1 + rng.below(2) };
                            if let (Pt::B, Some((_, m, _))) = (kind, opt.b_only) {
                                mt = m;
                            }
                            b.put(mt, 2);
                            if !field && coded {
                                b.put(rng.below(2), 1);
                            }
                            for d in 0..2 {
                                let has = if kind == Pt::P { d == 0 } else { dirs & (1 << d) != 0 };
                                if !has {
                                    continue;
                                }
                                let small = |rng: &mut Rng| rng.range(-6, 6);
                                match (field, mt) {
                                    (false, 2) => {
                                        let v = [small(&mut rng), small(&mut rng)];
                                        for k in 0..2 {
                                            mv(&mut b, v[k], pmv[d][0][k], f_code);
                                            pmv[d][0][k] = v[k];
                                            pmv[d][1][k] = v[k];
                                        }
                                    }
                                    (false, 1) => {
                                        for j in 0..2 {
                                            b.put(rng.below(2), 1);
                                            let v = [small(&mut rng), small(&mut rng)];
                                            mv(&mut b, v[0], pmv[d][j][0], f_code);
                                            pmv[d][j][0] = v[0];
                                            mv(&mut b, v[1], pmv[d][j][1] >> 1, f_code);
                                            pmv[d][j][1] = v[1] * 2;
                                        }
                                    }
                                    (true, 1) => {
                                        b.put(if only_first { 0 } else { rng.below(2) }, 1);
                                        let v = [small(&mut rng), small(&mut rng)];
                                        for k in 0..2 {
                                            mv(&mut b, v[k], pmv[d][0][k], f_code);
                                            pmv[d][0][k] = v[k];
                                            pmv[d][1][k] = v[k];
                                        }
                                    }
                                    (true, 2) => {
                                        for j in 0..2 {
                                            b.put(if only_first { 0 } else { rng.below(2) }, 1);
                                            let v = [small(&mut rng), small(&mut rng)];
                                            for k in 0..2 {
                                                mv(&mut b, v[k], pmv[d][j][k], f_code);
                                                pmv[d][j][k] = v[k];
                                            }
                                        }
                                    }
                                    (_, _) => {
                                        // Dual prime.
                                        let my_shift = !field as u32;
                                        let mxv = small(&mut rng);
                                        mv(&mut b, mxv, pmv[d][0][0], f_code);
                                        pmv[d][0][0] = mxv;
                                        pmv[d][1][0] = mxv;
                                        let dmx = rng.range(-1, 1);
                                        put_dmv(&mut b, dmx);
                                        let myv = small(&mut rng);
                                        mv(&mut b, myv, pmv[d][0][1] >> my_shift, f_code);
                                        let dmy = rng.range(-1, 1);
                                        put_dmv(&mut b, dmy);
                                        pmv[d][0][1] = myv << my_shift;
                                        pmv[d][1][1] = myv << my_shift;
                                    }
                                }
                            }
                            dc = [128; 3];
                            if coded {
                                write_coded(&mut b, &mut rng, nblocks, &mut dc);
                            }
                        }
                    }
                    x = mb_x + 1;
                }
            }
        }
    }
    b.start(0xB7);
    b.out
}

fn put_dmv(b: &mut Bits, v: i32) {
    match v {
        0 => b.put(0, 1),
        1 => b.put(0b10, 2),
        _ => b.put(0b11, 2),
    }
}

fn write_coded(b: &mut Bits, rng: &mut Rng, nblocks: usize, dc: &mut [i32; 3]) {
    let cbp = 1 + rng.below(63);
    b.put(CBP_CODES[cbp as usize], CBP_BITS[cbp as usize] as u32);
    let mut full = cbp;
    if nblocks == 8 {
        let extra = rng.below(4);
        b.put(extra, 2);
        full = (cbp << 2) | extra;
    }
    for n in 0..nblocks {
        if full & (1 << (nblocks - 1 - n)) != 0 {
            write_block(b, rng, false, false, if n < 4 { 0 } else { 1 + (n & 1) }, dc);
        }
    }
}

/// An MPEG-1 stream of D pictures (DC only: every 8×8 block one flat value) of
/// `w`×`h` (multiples of 16), and the planes each picture must decode to
/// (Y, Cb, Cr, tight). No encoder makes D pictures and FFmpeg's decoder
/// skips them, but their decoding is exact: the DC value is the block.
pub fn generate_d_pictures(w: usize, h: usize, frames: usize) -> (Vec<u8>, Vec<[Vec<u8>; 3]>) {
    let (mbw, mbh) = (w / 16, h / 16);
    let mut b = Bits::new();
    b.start(0xb3);
    b.put(w as u32, 12);
    b.put(h as u32, 12);
    b.put(1, 4); // square pixels
    b.put(3, 4); // 25 fps
    b.put(0x3ffff, 18);
    b.put(1, 1);
    b.put(20, 10);
    b.put(0, 3); // not constrained, no matrices
    b.start(0xb8);
    b.put(0, 25);
    b.put(0b10, 2); // closed GOP
    let mut expect = Vec::new();
    let mut rng = Rng(7);
    for f in 0..frames {
        b.start(0x00);
        b.put(f as u32, 10);
        b.put(4, 3); // D
        b.put(0xffff, 16);
        b.put(0, 1);
        let mut planes = [vec![0u8; w * h], vec![0u8; w * h / 4], vec![0u8; w * h / 4]];
        for my in 0..mbh {
            b.start(my as u8 + 1);
            b.put(rng.range(1, 31) as u32, 5);
            b.put(0, 1);
            let mut pred = [128i32; 3];
            for mx in 0..mbw {
                b.put(1, 1); // address increment 1
                b.put(1, 1); // intra
                for blk in 0..6 {
                    let comp = if blk < 4 { 0 } else { blk - 3 };
                    // the whole range, with big steps (every DC size)
                    let v = rng.range(0, 255);
                    let diff = v - pred[comp];
                    pred[comp] = v;
                    let size = if diff == 0 { 0 } else { 32 - diff.unsigned_abs().leading_zeros() };
                    if comp == 0 {
                        b.put(DC_LUM_CODES[size as usize], DC_LUM_BITS[size as usize] as u32);
                    } else {
                        b.put(DC_CHROMA_CODES[size as usize], DC_CHROMA_BITS[size as usize] as u32);
                    }
                    if size > 0 {
                        b.put((if diff > 0 { diff } else { diff + (1 << size) - 1 }) as u32, size);
                    }
                    let (plane, pw, x0, y0) = match blk {
                        0..4 => (0, w, mx * 16 + (blk & 1) * 8, my * 16 + (blk >> 1) * 8),
                        _ => (comp, w / 2, mx * 8, my * 8),
                    };
                    for y in y0..y0 + 8 {
                        planes[plane][y * pw + x0..y * pw + x0 + 8].fill(v as u8);
                    }
                }
                b.put(1, 1); // end_of_macroblock
            }
        }
        expect.push(planes);
    }
    b.start(0xb7);
    (b.out, expect)
}
