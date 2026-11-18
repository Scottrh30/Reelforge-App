//! Reading a byte buffer bit by bit, most significant bit first (the order
//! every codec here uses).

/// An MSB-first bit reader. Reading past the end gives zeros and sets `overrun`.
///
/// The next bits are kept left-aligned in a 64-bit register, refilled four
/// bytes at a time, so looking ahead is a shift rather than a memory load:
/// after every move at least 32 bits are in the register (zeros past the end).
#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// The next bits, left-aligned (bit 63 is the next one).
    cache: u64,
    /// How many bits of `cache` are loaded (the rest are zero).
    bits: u32,
    /// The next byte of `data` to load.
    next: usize,
    /// Zero bits made up past the end of the data.
    padding: usize,
    pub overrun: bool,
}

/// Near the end of the data: what bytes are left, then zeros.
#[cold]
#[inline(never)]
fn refill_tail(data: &[u8], mut cache: u64, mut bits: u32, mut next: usize, mut padding: usize) -> (u64, u32, usize, usize) {
    while bits <= 56 && next < data.len() {
        cache |= (data[next] as u64) << (56 - bits);
        bits += 8;
        next += 1;
    }
    if bits <= 32 {
        // Out of data: the rest of the register is zeros.
        padding += (64 - bits) as usize;
        bits = 64;
    }
    (cache, bits, next, padding)
}

/// The register state at bit `pos`: (cache, bits, next, padding, past the end).
#[cold]
#[inline(never)]
fn seek(data: &[u8], pos: usize) -> (u64, u32, usize, usize, bool) {
    let mut r = BitReader { data, cache: 0, bits: 0, next: (pos / 8).min(data.len()), padding: 0, overrun: pos > data.len() * 8 };
    let byte_pos = r.next * 8;
    r.refill();
    let extra = pos - byte_pos;
    if extra > 0 {
        // Within the register now (at most 7 bits, or past the end: zeros).
        let n = extra.min(r.bits as usize) as u32;
        r.cache = if n >= 64 { 0 } else { r.cache << n };
        r.bits -= n;
        r.padding += extra - n as usize;
        r.refill();
    }
    (r.cache, r.bits, r.next, r.padding, r.overrun)
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut r = BitReader { data, cache: 0, bits: 0, next: 0, padding: 0, overrun: false };
        r.refill();
        r
    }

    /// Top the register up to more than 32 bits.
    #[inline(always)]
    fn refill(&mut self) {
        if self.bits <= 32 {
            if let Some(b) = self.data.get(self.next..self.next + 4) {
                let w = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64;
                self.cache |= w << (32 - self.bits);
                self.bits += 32;
                self.next += 4;
            } else {
                // By value, not through `self`: a pointer to the reader passed
                // to a call would keep it in memory instead of in registers.
                (self.cache, self.bits, self.next, self.padding) = refill_tail(self.data, self.cache, self.bits, self.next, self.padding);
            }
        }
    }

    pub fn position(&self) -> usize {
        self.next * 8 + self.padding - self.bits as usize
    }

    pub fn bits_left(&self) -> isize {
        (self.data.len() * 8) as isize - self.position() as isize
    }

    /// Jump to bit `pos`.
    #[inline(always)]
    fn seek(&mut self, pos: usize) {
        let (cache, bits, next, padding, over) = seek(self.data, pos);
        (self.cache, self.bits, self.next, self.padding) = (cache, bits, next, padding);
        self.overrun |= over;
    }

    /// The next 32 bits, without moving.
    #[inline(always)]
    pub fn peek32(&self) -> u32 {
        (self.cache >> 32) as u32
    }

    /// The next `n` bits (0–32), without moving.
    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.cache >> (64 - n)) as u32
        }
    }

    #[inline(always)]
    pub fn skip(&mut self, n: usize) {
        if n < self.bits as usize {
            self.cache <<= n;
            self.bits -= n as u32;
            self.refill();
            if self.padding > 0 && self.position() > self.data.len() * 8 {
                self.overrun = true;
            }
        } else {
            let to = self.position() + n;
            self.seek(to);
        }
    }

    /// Read `n` bits (0–32) as an unsigned number.
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n as usize);
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.read(1) == 1
    }

    /// Read `n` bits as a two's-complement signed number.
    pub fn read_signed(&mut self, n: u32) -> i32 {
        let v = self.read(n) as i32;
        (v << (32 - n)) >> (32 - n)
    }

    /// Skip to the next byte boundary.
    pub fn align(&mut self) {
        let p = self.position();
        self.skip((8 - p % 8) % 8);
    }

    /// Exp-Golomb, unsigned (ue(v)).
    pub fn ue(&mut self) -> u32 {
        let zeros = self.peek32().leading_zeros();
        if zeros >= 32 {
            self.overrun = true;
            self.skip(32);
            return 0;
        }
        self.skip(zeros as usize);
        (self.read(zeros + 1)) - 1
    }

    /// Exp-Golomb, signed (se(v)).
    pub fn se(&mut self) -> i32 {
        let k = self.ue();
        if k & 1 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_bytes() {
        let d = [0b1010_1100, 0b0101_0011, 0xFF];
        let mut r = BitReader::new(&d);
        assert_eq!(r.read(3), 0b101);
        assert_eq!(r.read(7), 0b011_0001);
        assert_eq!(r.read(6), 0b01_0011);
        assert_eq!(r.read(8), 0xFF);
        assert!(!r.overrun);
        assert_eq!(r.read(4), 0);
        assert!(r.overrun);
    }

    #[test]
    fn long_reads_skips_and_the_end() {
        let d: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37) ^ 0x5A).collect();
        // Reference: bit i of the data.
        let bit = |i: usize| if i < d.len() * 8 { (d[i / 8] >> (7 - i % 8)) & 1 } else { 0 } as u32;
        let want = |at: usize, n: u32| (0..n as usize).fold(0u32, |v, k| (v << 1) | bit(at + k));
        let mut r = BitReader::new(&d);
        let mut at = 0usize;
        for (k, n) in [1u32, 7, 13, 32, 3, 0, 31, 17, 9, 32, 5, 11, 32, 32, 2].iter().cycle().take(40).enumerate() {
            if k % 7 == 3 {
                // A long skip.
                r.skip(45);
                at += 45;
            }
            assert_eq!(r.position(), at);
            assert_eq!(r.peek(*n), want(at, *n), "peek {n} at {at}");
            assert_eq!(r.read(*n), want(at, *n), "read {n} at {at}");
            at += *n as usize;
            assert_eq!(r.overrun, at > d.len() * 8, "overrun at {at}");
            assert_eq!(r.bits_left(), (d.len() * 8) as isize - at as isize);
        }
        let mut r = BitReader::new(&d[..3]);
        r.skip(3);
        r.align();
        assert_eq!((r.position(), r.read(8)), (8, d[1] as u32));
        let mut r = BitReader::new(&[]);
        assert_eq!((r.read(32), r.overrun), (0, true));
    }

    #[test]
    fn exp_golomb() {
        // 1 | 010 | 011 | 00100 | 00101 -> 0, 1, 2, 3, 4
        let d = [0b1010_0110, 0b0100_0010, 0b1000_0000];
        let mut r = BitReader::new(&d);
        assert_eq!((r.ue(), r.ue(), r.ue(), r.ue(), r.ue()), (0, 1, 2, 3, 4));
        let mut r = BitReader::new(&d);
        assert_eq!((r.se(), r.se(), r.se(), r.se(), r.se()), (0, 1, -1, 2, -2));
    }
}
