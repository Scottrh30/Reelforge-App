//! Decoded picture buffer: reference marking (8.2.5), reference list construction (8.2.4) and output
//! ordering / bumping (C.4).
//!
//! The buffer only does bookkeeping on picture headers; what a picture is (`F`: the software
//! decoder's [`crate::picture::FrameRef`], a hardware decoder's surface) is up to the decoder.
//!
//! Each entry is a frame buffer: a decoded frame, a pair of fields decoded one after the other
//! (field pictures), or a single field. References are marked per field, so field pictures can
//! refer to (and mark) one field of a frame; frame pictures use frames whose two fields are both
//! marked alike.

use crate::error::{Result, ensure};
use crate::slice::{Mmco, RefPicListMod, SliceHeader, SliceType};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefMark {
    Unused,
    Short,
    Long,
}

/// Output-related metadata attached to a decoded frame.
#[derive(Clone, Debug)]
pub struct OutputMeta {
    pub pts: i64,
    pub key: bool,
    pub crop: (u32, u32, u32, u32),
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub sar: (u16, u16),
    /// Decoded without deblocking (draft mode, non-reference picture).
    pub draft: bool,
}

pub struct DpbEntry<F> {
    /// The frame (for field pictures, the frame both fields go into).
    pub frame: F,
    /// The pictures the fields were decoded into, when coded as field pictures (top, bottom).
    pub fields: [Option<F>; 2],
    /// PicOrderCnt of the frame: the smaller of its decoded fields'.
    pub poc: i32,
    /// TopFieldOrderCnt / BottomFieldOrderCnt (i32::MAX for a field not decoded).
    pub field_poc: [i32; 2],
    pub frame_num: u32,
    pub long_term_frame_idx: u32,
    /// Marking of the top and bottom field.
    pub marks: [RefMark; 2],
    /// Fields decoded: bit 0 top, bit 1 bottom.
    pub decoded: u8,
    pub needed_for_output: bool,
    pub non_existing: bool,
    /// A first field whose second field may still follow: not output yet.
    pub open: bool,
    pub meta: Arc<OutputMeta>,
}

impl<F> DpbEntry<F> {
    /// The marking of the frame as a whole (frame decoding uses frames whose fields are both
    /// short-term or both long-term references).
    pub fn mark(&self) -> RefMark {
        if self.marks[0] == self.marks[1] { self.marks[0] } else { RefMark::Unused }
    }
    fn any_ref(&self) -> bool {
        self.marks.iter().any(|m| *m != RefMark::Unused)
    }
    fn has(&self, m: RefMark) -> bool {
        self.marks.contains(&m)
    }
    fn unmark(&mut self, m: RefMark) {
        for x in &mut self.marks {
            if *x == m {
                *x = RefMark::Unused;
            }
        }
    }
}

pub struct Dpb<F> {
    pub entries: Vec<DpbEntry<F>>,
    /// None = "no long-term frame indices".
    pub max_long_term_frame_idx: Option<u32>,
    pub capacity: usize,
    pub max_reorder: usize,
}

/// A picture leaving the DPB for output.
pub struct Output<F> {
    pub frame: F,
    pub poc: i32,
    pub meta: Arc<OutputMeta>,
}

/// One entry of a field reference list: a frame buffer and the field of it.
type FieldRef<'a, F> = (&'a DpbEntry<F>, usize);

impl<F: Clone> Dpb<F> {
    pub fn new() -> Self {
        Dpb { entries: Vec::new(), max_long_term_frame_idx: None, capacity: 16, max_reorder: 16 }
    }

    fn frame_num_wrap(e: &DpbEntry<F>, cur_frame_num: u32, max_frame_num: u32) -> i32 {
        if e.frame_num > cur_frame_num { e.frame_num as i32 - max_frame_num as i32 } else { e.frame_num as i32 }
    }

    /// numShortTerm + numLongTerm (8.2.5.3): frame buffers with a short-term field plus those
    /// with a long-term field.
    fn num_refs(&self) -> usize {
        self.entries.iter().map(|e| e.has(RefMark::Short) as usize + e.has(RefMark::Long) as usize).sum()
    }

    /// Remove entries not needed for output and not used for reference.
    fn prune(&mut self) {
        self.entries.retain(|e| e.needed_for_output || e.any_ref() || e.open);
    }

    /// Output the picture with the smallest POC (C.4.5.3 "bumping"). Returns false if none waits.
    fn bump(&mut self, out: &mut Vec<Output<F>>) -> bool {
        let Some(i) = self.entries.iter().enumerate().filter(|(_, e)| e.needed_for_output && !e.open).min_by_key(|(_, e)| e.poc).map(|(i, _)| i) else {
            return false;
        };
        let e = &mut self.entries[i];
        e.needed_for_output = false;
        out.push(Output { frame: e.frame.clone(), poc: e.poc, meta: e.meta.clone() });
        self.prune();
        true
    }

    /// Output everything in POC order and clear non-reference entries.
    pub fn flush(&mut self, out: &mut Vec<Output<F>>) {
        while self.bump(out) {}
        self.prune();
    }

    /// IDR: mark all references unused; output (or drop) prior pictures.
    pub fn idr(&mut self, no_output_of_prior_pics: bool, out: &mut Vec<Output<F>>) {
        for e in &mut self.entries {
            e.marks = [RefMark::Unused; 2];
            e.open = false;
        }
        if no_output_of_prior_pics {
            self.entries.clear();
        } else {
            self.flush(out);
        }
        self.max_long_term_frame_idx = None;
    }

    /// The first field waiting for its second one, if any.
    pub fn open_entry(&self) -> Option<&DpbEntry<F>> {
        self.entries.iter().find(|e| e.open)
    }

    /// No second field is coming for the waiting first field: it's output on its own.
    pub fn close_open_field(&mut self, out: &mut Vec<Output<F>>) {
        let mut closed = false;
        for e in &mut self.entries {
            closed |= e.open;
            e.open = false;
        }
        if closed {
            self.bump_reorder(out);
        }
    }

    /// Low-delay output: honour max_num_reorder_frames.
    fn bump_reorder(&mut self, out: &mut Vec<Output<F>>) {
        while self.entries.iter().filter(|e| e.needed_for_output && !e.open).count() > self.max_reorder {
            if !self.bump(out) {
                break;
            }
        }
    }

    /// Sliding window marking (8.2.5.3).
    fn sliding_window(&mut self, cur_frame_num: u32, max_frame_num: u32, max_num_ref_frames: usize) {
        let limit = max_num_ref_frames.max(1);
        while self.num_refs() >= limit {
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.has(RefMark::Short) && !e.open)
                .min_by_key(|(_, e)| Self::frame_num_wrap(e, cur_frame_num, max_frame_num))
                .map(|(i, _)| i);
            match victim {
                Some(i) => self.entries[i].unmark(RefMark::Short),
                None => break,
            }
        }
        self.prune();
    }

    /// Insert "non-existing" frames for a frame_num gap (8.2.5.2). `make_frame` gives each one's
    /// picture and POC.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_frame_num_gap(
        &mut self,
        prev_ref_frame_num: u32,
        frame_num: u32,
        max_frame_num: u32,
        max_num_ref_frames: usize,
        make_frame: &mut dyn FnMut(u32) -> (F, i32, Arc<OutputMeta>),
        poc_state: &mut dyn FnMut(u32),
    ) {
        let mut unused = (prev_ref_frame_num + 1) % max_frame_num;
        let mut guard = 0;
        while unused != frame_num && guard < max_frame_num {
            self.sliding_window(unused, max_frame_num, max_num_ref_frames);
            let (frame, poc, meta) = make_frame(unused);
            poc_state(unused);
            self.entries.push(DpbEntry {
                poc,
                frame,
                fields: [None, None],
                field_poc: [poc, poc],
                frame_num: unused,
                long_term_frame_idx: 0,
                marks: [RefMark::Short; 2],
                decoded: 3,
                needed_for_output: false,
                non_existing: true,
                open: false,
                meta,
            });
            unused = (unused + 1) % max_frame_num;
            guard += 1;
        }
    }

    /// Initial reference picture lists (8.2.4.2) + modification (8.2.4.3) for a frame slice, each
    /// entry made by `to_ref` from the DPB entry it refers to.
    pub fn build_ref_lists<R>(&self, sh: &SliceHeader, cur_poc: i32, max_frame_num: u32, to_ref: impl Fn(&DpbEntry<F>) -> R) -> Result<[Vec<R>; 2]> {
        let mut lists: [Vec<R>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type.is_intra() {
            return Ok(lists);
        }
        let cur_fn = sh.frame_num;
        let pic_num = |e: &DpbEntry<F>| Self::frame_num_wrap(e, cur_fn, max_frame_num);
        let short: Vec<&DpbEntry<F>> = self.entries.iter().filter(|e| e.mark() == RefMark::Short).collect();
        let mut long: Vec<&DpbEntry<F>> = self.entries.iter().filter(|e| e.mark() == RefMark::Long).collect();
        long.sort_by_key(|e| e.long_term_frame_idx);
        let mut entry_lists: [Vec<&DpbEntry<F>>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type != SliceType::B {
            let mut s = short.clone();
            s.sort_by_key(|e| std::cmp::Reverse(pic_num(e)));
            entry_lists[0] = s.into_iter().chain(long.iter().copied()).collect();
        } else {
            let mut before: Vec<&DpbEntry<F>> = short.iter().copied().filter(|e| e.poc < cur_poc).collect();
            let mut after: Vec<&DpbEntry<F>> = short.iter().copied().filter(|e| e.poc > cur_poc).collect();
            before.sort_by_key(|e| std::cmp::Reverse(e.poc));
            after.sort_by_key(|e| e.poc);
            entry_lists[0] = before.iter().chain(after.iter()).chain(long.iter()).copied().collect();
            entry_lists[1] = after.iter().chain(before.iter()).chain(long.iter()).copied().collect();
            if entry_lists[1].len() > 1 && entry_lists[0].len() == entry_lists[1].len() {
                let same = entry_lists[0].iter().zip(entry_lists[1].iter()).all(|(a, b)| std::ptr::eq(*a, *b));
                if same {
                    entry_lists[1].swap(0, 1);
                }
            }
        }
        let nlists = if sh.slice_type == SliceType::B { 2 } else { 1 };
        for l in 0..nlists {
            let n = sh.num_ref_idx_active[l] as usize;
            let mut list: Vec<Option<&DpbEntry<F>>> = entry_lists[l].iter().copied().map(Some).collect();
            list.truncate(n);
            list.resize(n, None);
            let find_short = |pn: i32| short.iter().copied().find(|e| pic_num(e) == pn);
            let find_long = |v: u32| long.iter().copied().find(|e| e.long_term_frame_idx == v);
            modify_list(&mut list, &sh.ref_pic_list_mod[l], max_frame_num as i32, cur_fn as i32, find_short, find_long, |a, b| std::ptr::eq(a, b))?;
            // Fill missing entries (should not happen in conforming streams) with the first valid entry.
            let fallback = list.iter().flatten().next().copied().or_else(|| self.entries.iter().rfind(|e| !e.open));
            lists[l] = list.iter().filter_map(|e| e.or(fallback)).map(&to_ref).collect();
        }
        Ok(lists)
    }

    /// The same for a field slice of parity `par` (8.2.4.2.2, 8.2.4.2.4, 8.2.4.2.5): lists of
    /// fields, `to_ref` getting the frame buffer and the field of it.
    pub fn build_ref_lists_field<R>(&self, sh: &SliceHeader, par: usize, cur_poc: i32, max_frame_num: u32, to_ref: impl Fn(&DpbEntry<F>, usize) -> R) -> Result<[Vec<R>; 2]> {
        let mut lists: [Vec<R>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type.is_intra() {
            return Ok(lists);
        }
        let cur_fn = sh.frame_num;
        let fnw = |e: &DpbEntry<F>| Self::frame_num_wrap(e, cur_fn, max_frame_num);
        let short: Vec<&DpbEntry<F>> = self.entries.iter().filter(|e| e.has(RefMark::Short)).collect();
        let mut long: Vec<&DpbEntry<F>> = self.entries.iter().filter(|e| e.has(RefMark::Long)).collect();
        long.sort_by_key(|e| e.long_term_frame_idx);
        // 8.2.4.2.5: fields taken alternately, starting with the current parity, from the
        // ordered frame buffers.
        let alternate = |frames: &[&'_ DpbEntry<F>], m: RefMark| -> Vec<(usize, usize)> {
            let mut out = Vec::new();
            let (mut i, mut j) = (0usize, 0usize);
            let has = |k: usize, p: usize| frames.get(k).is_some_and(|e| e.marks[p] == m);
            loop {
                while i < frames.len() && !has(i, par) {
                    i += 1;
                }
                while j < frames.len() && !has(j, par ^ 1) {
                    j += 1;
                }
                if i >= frames.len() && j >= frames.len() {
                    break;
                }
                if i < frames.len() {
                    out.push((i, par));
                    i += 1;
                }
                if j < frames.len() {
                    out.push((j, par ^ 1));
                    j += 1;
                }
            }
            out
        };
        fn pick<'e, F>(frames: &[&'e DpbEntry<F>], at: Vec<(usize, usize)>) -> Vec<FieldRef<'e, F>> {
            at.into_iter().filter_map(|(k, p)| Some((*frames.get(k)?, p))).collect()
        }
        let long_fields = pick(&long, alternate(&long, RefMark::Long));
        let mut field_lists: [Vec<FieldRef<'_, F>>; 2] = [Vec::new(), Vec::new()];
        if sh.slice_type != SliceType::B {
            let mut s = short.clone();
            s.sort_by_key(|e| std::cmp::Reverse(fnw(e)));
            field_lists[0] = pick(&s, alternate(&s, RefMark::Short)).into_iter().chain(long_fields.iter().copied()).collect();
        } else {
            // POC of a frame buffer: its decoded fields' smaller one; at or before the current
            // field first in list 0 (8.2.4.2.4)
            let mut before: Vec<&DpbEntry<F>> = short.iter().copied().filter(|e| e.poc <= cur_poc).collect();
            let mut after: Vec<&DpbEntry<F>> = short.iter().copied().filter(|e| e.poc > cur_poc).collect();
            before.sort_by_key(|e| std::cmp::Reverse(e.poc));
            after.sort_by_key(|e| e.poc);
            let l0: Vec<&DpbEntry<F>> = before.iter().chain(after.iter()).copied().collect();
            let l1: Vec<&DpbEntry<F>> = after.iter().chain(before.iter()).copied().collect();
            field_lists[0] = pick(&l0, alternate(&l0, RefMark::Short)).into_iter().chain(long_fields.iter().copied()).collect();
            field_lists[1] = pick(&l1, alternate(&l1, RefMark::Short)).into_iter().chain(long_fields.iter().copied()).collect();
            if field_lists[1].len() > 1 && field_lists[0].len() == field_lists[1].len() {
                let same = field_lists[0].iter().zip(field_lists[1].iter()).all(|(a, b)| std::ptr::eq(a.0, b.0) && a.1 == b.1);
                if same {
                    field_lists[1].swap(0, 1);
                }
            }
        }
        // 8.2.4.1: PicNum 2 * FrameNumWrap + 1 for the current parity, 2 * FrameNumWrap for
        // the other; LongTermPicNum alike from LongTermFrameIdx.
        let pic_num = |(e, p): FieldRef<'_, F>| 2 * fnw(e) + (p == par) as i32;
        let lt_pic_num = |(e, p): FieldRef<'_, F>| 2 * e.long_term_frame_idx as i32 + (p == par) as i32;
        let all_short: Vec<FieldRef<'_, F>> = short.iter().flat_map(|e| (0..2).filter(|p| e.marks[*p] == RefMark::Short).map(move |p| (*e, p))).collect();
        let all_long: Vec<FieldRef<'_, F>> = long.iter().flat_map(|e| (0..2).filter(|p| e.marks[*p] == RefMark::Long).map(move |p| (*e, p))).collect();
        let nlists = if sh.slice_type == SliceType::B { 2 } else { 1 };
        for l in 0..nlists {
            let n = sh.num_ref_idx_active[l] as usize;
            let mut list: Vec<Option<FieldRef<'_, F>>> = field_lists[l].iter().copied().map(Some).collect();
            list.truncate(n);
            list.resize(n, None);
            let find_short = |pn: i32| all_short.iter().copied().find(|f| pic_num(*f) == pn);
            let find_long = |v: u32| all_long.iter().copied().find(|f| lt_pic_num(*f) == v as i32);
            let max_pic_num = 2 * max_frame_num as i32;
            let curr_pic_num = 2 * cur_fn as i32 + 1;
            modify_list(&mut list, &sh.ref_pic_list_mod[l], max_pic_num, curr_pic_num, find_short, find_long, |a, b| std::ptr::eq(a.0, b.0) && a.1 == b.1)?;
            let fallback = list.iter().flatten().next().copied().or_else(|| field_lists[l].first().copied()).or_else(|| all_short.first().copied()).or_else(|| all_long.first().copied());
            lists[l] = list.iter().filter_map(|e| e.or(fallback)).map(|(e, p)| to_ref(e, p)).collect();
        }
        Ok(lists)
    }

    /// Adaptive memory control (8.2.5.4) for a frame (`field` None) or the field of parity
    /// `field`. Returns true if MMCO 5 was executed.
    fn apply_mmcos(&mut self, mmcos: &[Mmco], cur_frame_num: u32, max_frame_num: u32, field: Option<usize>, cur_long: &mut Option<u32>) -> bool {
        let mut had5 = false;
        // the picture numbers of a frame, or of a field (8.2.4.1)
        let (curr_pic_num, field_shift) = match field {
            None => (cur_frame_num as i32, 0),
            Some(_) => (2 * cur_frame_num as i32 + 1, 1),
        };
        let pic_num = |e: &DpbEntry<F>, p: usize| (Self::frame_num_wrap(e, cur_frame_num, max_frame_num) << field_shift) + field.map_or(0, |c| (c == p) as i32);
        let lt_pic_num = |e: &DpbEntry<F>, p: usize| ((e.long_term_frame_idx as i32) << field_shift) + field.map_or(0, |c| (c == p) as i32);
        // the field(s) a picture number names: both for frames
        let parities = |p: usize| if field.is_some() { p..p + 1 } else { 0..2 };
        for m in mmcos {
            match m.op {
                1 => {
                    let pn = curr_pic_num - (m.difference_of_pic_nums_minus1 as i32 + 1);
                    for e in &mut self.entries {
                        for p in 0..2 {
                            let hit = if field.is_some() { e.marks[p] == RefMark::Short } else { e.mark() == RefMark::Short };
                            if hit && pic_num(e, p) == pn {
                                for q in parities(p) {
                                    e.marks[q] = RefMark::Unused;
                                }
                            }
                        }
                    }
                }
                2 => {
                    for e in &mut self.entries {
                        for p in 0..2 {
                            let hit = if field.is_some() { e.marks[p] == RefMark::Long } else { e.mark() == RefMark::Long };
                            if hit && lt_pic_num(e, p) == m.long_term_pic_num as i32 {
                                for q in parities(p) {
                                    e.marks[q] = RefMark::Unused;
                                }
                            }
                        }
                    }
                }
                3 => {
                    let pn = curr_pic_num - (m.difference_of_pic_nums_minus1 as i32 + 1);
                    let target = self.entries.iter().enumerate().find_map(|(i, e)| {
                        (0..2).find(|&p| {
                            let hit = if field.is_some() { e.marks[p] == RefMark::Short } else { e.mark() == RefMark::Short };
                            hit && pic_num(e, p) == pn
                        })
                        .map(|p| (i, p))
                    });
                    // LongTermFrameIdx moves to the target: other holders lose it (a field
                    // keeps it when the other field of its frame is the target)
                    for (i, e) in self.entries.iter_mut().enumerate() {
                        if e.long_term_frame_idx == m.long_term_frame_idx && target.is_none_or(|(t, _)| t != i) {
                            e.unmark(RefMark::Long);
                        }
                    }
                    if let Some((i, p)) = target {
                        let e = &mut self.entries[i];
                        if e.has(RefMark::Long) && e.long_term_frame_idx != m.long_term_frame_idx {
                            e.unmark(RefMark::Long);
                        }
                        for q in parities(p) {
                            e.marks[q] = RefMark::Long;
                        }
                        e.long_term_frame_idx = m.long_term_frame_idx;
                    }
                }
                4 => {
                    let max = m.max_long_term_frame_idx_plus1;
                    for e in &mut self.entries {
                        if e.has(RefMark::Long) && (max == 0 || e.long_term_frame_idx > max - 1) {
                            e.unmark(RefMark::Long);
                        }
                    }
                    self.max_long_term_frame_idx = if max == 0 { None } else { Some(max - 1) };
                }
                5 => {
                    for e in &mut self.entries {
                        e.marks = [RefMark::Unused; 2];
                    }
                    self.max_long_term_frame_idx = None;
                    had5 = true;
                }
                6 => {
                    // (the first field of the current frame keeps it)
                    for e in &mut self.entries {
                        if e.has(RefMark::Long) && e.long_term_frame_idx == m.long_term_frame_idx && !e.open {
                            e.unmark(RefMark::Long);
                        }
                    }
                    *cur_long = Some(m.long_term_frame_idx);
                }
                _ => {}
            }
        }
        self.prune();
        had5
    }

    /// Reference marking of the current picture (8.2.5.1): its mark and LongTermFrameIdx, and
    /// whether MMCO 5 ran. `second_of_short`: the second field of a pair whose first field is
    /// a short-term reference (no sliding window then).
    fn mark_current(&mut self, sh: &SliceHeader, max_frame_num: u32, max_num_ref_frames: usize, field: Option<usize>, second_of_short: bool) -> (RefMark, u32, bool) {
        if sh.nal_ref_idc == 0 {
            return (RefMark::Unused, 0, false);
        }
        if sh.idr {
            return if sh.long_term_reference {
                self.max_long_term_frame_idx = Some(0);
                (RefMark::Long, 0, false)
            } else {
                self.max_long_term_frame_idx = None;
                (RefMark::Short, 0, false)
            };
        }
        let mut cur_long = None;
        let mut had5 = false;
        if sh.adaptive_ref_pic_marking {
            had5 = self.apply_mmcos(&sh.mmcos, sh.frame_num, max_frame_num, field, &mut cur_long);
        } else if !second_of_short {
            self.sliding_window(sh.frame_num, max_frame_num, max_num_ref_frames);
        }
        match cur_long {
            Some(i) => (RefMark::Long, i, had5),
            None => {
                // make room if MMCOs left the DPB over-full (non-conforming streams)
                if !second_of_short && self.num_refs() >= max_num_ref_frames.max(1) && sh.adaptive_ref_pic_marking {
                    self.sliding_window(sh.frame_num, max_frame_num, max_num_ref_frames);
                }
                (RefMark::Short, 0, had5)
            }
        }
    }

    /// Reference marking for the just-decoded picture and insertion into the DPB with output bumping.
    /// Returns pictures ready for output.
    #[allow(clippy::too_many_arguments)]
    pub fn store_picture(
        &mut self,
        sh: &SliceHeader,
        frame: F,
        poc: i32,
        field_poc: [i32; 2],
        max_frame_num: u32,
        max_num_ref_frames: usize,
        meta: Arc<OutputMeta>,
        out: &mut Vec<Output<F>>,
    ) {
        let (mark, long_idx, had5) = self.mark_current(sh, max_frame_num, max_num_ref_frames, None, false);
        if had5 {
            // All earlier pictures are output before the current one (C.4.4).
            self.flush(out);
        }
        let frame_num = if had5 { 0 } else { sh.frame_num };
        // Bumping until there is space (C.4.5.1 / C.4.5.2).
        if mark == RefMark::Unused {
            loop {
                let waiting = self.entries.len();
                if waiting < self.capacity {
                    break;
                }
                // non-reference picture with the smallest POC is output directly
                let min_poc = self.entries.iter().filter(|e| e.needed_for_output && !e.open).map(|e| e.poc).min();
                if min_poc.is_none_or(|m| poc < m) {
                    out.push(Output { frame, poc, meta });
                    return;
                }
                if !self.bump(out) {
                    break;
                }
            }
        } else {
            self.make_room(out);
        }
        self.entries.push(DpbEntry {
            frame,
            fields: [None, None],
            poc,
            field_poc,
            frame_num,
            long_term_frame_idx: long_idx,
            marks: [mark; 2],
            decoded: 3,
            needed_for_output: true,
            non_existing: false,
            open: false,
            meta,
        });
        self.bump_reorder(out);
    }

    fn make_room(&mut self, out: &mut Vec<Output<F>>) {
        while self.entries.len() >= self.capacity {
            if !self.bump(out) {
                break;
            }
        }
    }

    /// The same for a field picture of parity `par` decoded into `field_pic`: the second field
    /// of the waiting first field (`second`) joins its frame buffer, any other starts a new one
    /// (with `frame`, the frame both fields go into) and waits for its second field.
    #[allow(clippy::too_many_arguments)]
    pub fn store_field(
        &mut self,
        sh: &SliceHeader,
        par: usize,
        frame: F,
        field_pic: F,
        poc: i32,
        second: bool,
        max_frame_num: u32,
        max_num_ref_frames: usize,
        meta: Arc<OutputMeta>,
        out: &mut Vec<Output<F>>,
    ) {
        let open = self.entries.iter().position(|e| e.open);
        let second = second && open.is_some();
        let second_of_short = second && open.is_some_and(|i| self.entries[i].marks[par ^ 1] == RefMark::Short);
        // (a second field never repeats the IDR's emptying of the DPB)
        let (mark, long_idx, had5) = if second && sh.idr {
            (if sh.nal_ref_idc == 0 { RefMark::Unused } else if sh.long_term_reference { RefMark::Long } else { RefMark::Short }, 0, false)
        } else {
            self.mark_current(sh, max_frame_num, max_num_ref_frames, Some(par), second_of_short)
        };
        if had5 {
            // earlier pictures are output first (C.4.4); a waiting first field stays
            while self.bump(out) {}
        }
        if second && let Some(i) = self.entries.iter().position(|e| e.open) {
            let e = &mut self.entries[i];
            e.fields[par] = Some(field_pic);
            e.field_poc[par] = poc;
            e.poc = e.field_poc[0].min(e.field_poc[1]);
            e.decoded |= 1 << par;
            e.marks[par] = mark;
            if mark == RefMark::Long {
                e.long_term_frame_idx = long_idx;
            }
            if had5 {
                e.frame_num = 0;
            }
            e.open = false;
            self.prune();
            self.bump_reorder(out);
            return;
        }
        self.make_room(out);
        let mut field_poc = [i32::MAX; 2];
        field_poc[par] = poc;
        let mut fields = [None, None];
        fields[par] = Some(field_pic);
        let mut marks = [RefMark::Unused; 2];
        marks[par] = mark;
        self.entries.push(DpbEntry {
            frame,
            fields,
            poc,
            field_poc,
            frame_num: if had5 { 0 } else { sh.frame_num },
            long_term_frame_idx: long_idx,
            marks,
            decoded: 1 << par,
            needed_for_output: true,
            non_existing: false,
            open: true,
            meta,
        });
    }
}

/// Reference picture list modification (8.2.4.3) of `list` (num_ref_idx_active long), with
/// picture numbers modulo `max_pic_num` from `curr_pic_num`; `find_short` / `find_long` give
/// the picture a PicNum / LongTermPicNum names, `same` tells pictures apart.
fn modify_list<T: Copy>(
    list: &mut Vec<Option<T>>,
    mods: &[RefPicListMod],
    max_pic_num: i32,
    curr_pic_num: i32,
    find_short: impl Fn(i32) -> Option<T>,
    find_long: impl Fn(u32) -> Option<T>,
    same: impl Fn(T, T) -> bool,
) -> Result<()> {
    let n = list.len();
    let mut pred = curr_pic_num;
    let mut ref_idx = 0usize;
    for m in mods {
        ensure!(ref_idx < n, "too many ref_pic_list_modification entries");
        let target = match m.idc {
            0 | 1 => {
                let d = m.value as i32 + 1;
                let no_wrap = if m.idc == 0 {
                    if pred - d < 0 { pred - d + max_pic_num } else { pred - d }
                } else if pred + d >= max_pic_num {
                    pred + d - max_pic_num
                } else {
                    pred + d
                };
                pred = no_wrap;
                let pn = if no_wrap > curr_pic_num { no_wrap - max_pic_num } else { no_wrap };
                find_short(pn)
            }
            2 => find_long(m.value),
            _ => continue,
        };
        let Some(target) = target else {
            // Missing picture: keep the list unchanged (error concealment).
            ref_idx += 1;
            continue;
        };
        list.push(None); // temporarily one longer
        for c in (ref_idx + 1..=n).rev() {
            list[c] = list[c - 1];
        }
        list[ref_idx] = Some(target);
        ref_idx += 1;
        let mut nidx = ref_idx;
        for c in ref_idx..=n {
            let keep = list[c].is_none_or(|e| !same(e, target));
            if keep {
                list[nidx] = list[c];
                nidx += 1;
            }
        }
        list.truncate(n);
    }
    Ok(())
}

impl<F: Clone> Default for Dpb<F> {
    fn default() -> Self {
        Self::new()
    }
}
