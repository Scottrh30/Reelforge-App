//! The boolean entropy decoder (RFC 6386 section 7).
//!
//! `value` keeps the decoder's window left-aligned in a 64-bit word: its top 8 bits are the
//! spec's `value`, followed by `count` more input bits already loaded. Reading past the end of
//! the data reads zero bits, so a damaged partition gives wrong pictures, never a crash.

pub struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    count: i32,
    range: u32,
}

impl<'a> BoolDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = BoolDecoder { data, pos: 0, value: 0, count: -8, range: 255 };
        d.fill();
        d
    }

    #[inline]
    fn fill(&mut self) {
        let mut shift = 64 - 8 - (self.count + 8);
        while shift >= 0 {
            if let Some(&b) = self.data.get(self.pos) {
                self.value |= u64::from(b) << shift;
                self.pos += 1;
            }
            // Past the end: zero bits.
            self.count += 8;
            shift -= 8;
        }
    }

    /// One bool whose probability of being 0 is `p / 256`.
    #[inline(always)]
    pub fn read(&mut self, p: u8) -> bool {
        if self.count < 0 {
            self.fill();
        }
        let split = 1 + (((self.range - 1) * u32::from(p)) >> 8);
        let big = u64::from(split) << 56;
        let bit = self.value >= big;
        if bit {
            self.range -= split;
            self.value -= big;
        } else {
            self.range = split;
        }
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.value <<= shift;
        self.count -= shift as i32;
        bit
    }

    #[inline]
    pub fn bit(&mut self) -> bool {
        self.read(128)
    }

    /// An `n`-bit unsigned literal, most significant bit first.
    pub fn literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bit());
        }
        v
    }

    /// An `n`-bit magnitude followed by a sign bit.
    pub fn signed(&mut self, n: u32) -> i32 {
        let v = self.literal(n) as i32;
        if self.bit() { -v } else { v }
    }

    /// An optional signed value: a flag, then `signed(n)`; 0 when the flag is clear.
    pub fn opt_signed(&mut self, n: u32) -> i32 {
        if self.bit() { self.signed(n) } else { 0 }
    }

    /// A value coded with `tree` (pairs of node indices, leaves as -value) and `probs` (one per
    /// node pair).
    #[inline]
    pub fn tree(&mut self, tree: &[i8], probs: &[u8]) -> u8 {
        let mut i = 0usize;
        loop {
            let p = probs.get(i >> 1).copied().unwrap_or(128);
            let next = tree.get(i + usize::from(self.read(p))).copied().unwrap_or(0);
            if next <= 0 {
                return next.unsigned_abs();
            }
            i = next as usize;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal boolean encoder (RFC 6386 section 7.3) to round-trip against.
    struct Enc {
        out: Vec<u8>,
        range: u32,
        bottom: u32,
        bit_count: i32,
    }

    impl Enc {
        fn new() -> Self {
            Enc { out: Vec::new(), range: 255, bottom: 0, bit_count: 24 }
        }
        fn add_one(&mut self) {
            let mut i = self.out.len();
            while i > 0 {
                i -= 1;
                if self.out[i] == 255 {
                    self.out[i] = 0;
                } else {
                    self.out[i] += 1;
                    break;
                }
            }
        }
        fn write(&mut self, p: u8, bit: bool) {
            let split = 1 + (((self.range - 1) * p as u32) >> 8);
            if bit {
                self.bottom = self.bottom.wrapping_add(split);
                self.range -= split;
            } else {
                self.range = split;
            }
            while self.range < 128 {
                self.range <<= 1;
                if self.bottom & (1 << 31) != 0 {
                    self.add_one();
                }
                self.bottom <<= 1;
                self.bit_count -= 1;
                if self.bit_count == 0 {
                    self.out.push((self.bottom >> 24) as u8);
                    self.bottom &= (1 << 24) - 1;
                    self.bit_count = 8;
                }
            }
        }
        fn finish(mut self) -> Vec<u8> {
            for _ in 0..32 {
                self.write(128, false);
            }
            self.out
        }
    }

    #[test]
    fn round_trips_bits_and_literals() {
        let mut e = Enc::new();
        let mut want = Vec::new();
        let mut x = 12345u32;
        for i in 0..5000 {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            let p = ((x >> 16) % 255 + 1) as u8;
            let bit = (x >> 8) % 3 == 0;
            e.write(p, bit);
            want.push((p, bit));
            if i % 97 == 0 {
                for k in (0..7).rev() {
                    e.write(128, (i >> k) & 1 != 0);
                }
            }
        }
        let data = e.finish();
        let mut d = BoolDecoder::new(&data);
        for (i, &(p, bit)) in want.iter().enumerate() {
            assert_eq!(d.read(p), bit, "bit {i}");
            if i % 97 == 0 {
                assert_eq!(d.literal(7), (i as u32) & 127);
            }
        }
    }

    #[test]
    fn empty_input_reads_zeros() {
        let mut d = BoolDecoder::new(&[]);
        for _ in 0..1000 {
            assert!(!d.read(1));
        }
        assert_eq!(d.literal(16), 0);
    }
}
