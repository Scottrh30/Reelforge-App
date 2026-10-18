//! Reading a byte buffer bit by bit, most significant bit first (the order
//! every codec here uses).

/// An MSB-first bit reader. Reading past the end gives zeros and sets `overrun`.
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
    pub overrun: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, overrun: false }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> isize {
        (self.data.len() * 8) as isize - self.pos as isize
    }

    /// The next 32 bits, without moving.
    #[inline]
    pub fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let v = if byte + 8 <= self.data.len() {
            u64::from_be_bytes(self.data[byte..byte + 8].try_into().unwrap())
        } else {
            let mut b = [0u8; 8];
            if byte < self.data.len() {
                let n = (self.data.len() - byte).min(8);
                b[..n].copy_from_slice(&self.data[byte..byte + n]);
            }
            u64::from_be_bytes(b)
        };
        ((v << (self.pos & 7)) >> 32) as u32
    }

    /// The next `n` bits (0–32), without moving.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.peek32() >> (32 - n)
        }
    }

    #[inline]
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
        if self.pos > self.data.len() * 8 {
            self.overrun = true;
        }
    }

    /// Read `n` bits (0–32) as an unsigned number.
    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.skip(n as usize);
        v
    }

    #[inline]
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
        self.pos = (self.pos + 7) & !7;
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
            ((k + 1) / 2) as i32
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
        assert_eq!(r.read(7), 0b0_1100_01);
        assert_eq!(r.read(6), 0b01_0011);
        assert_eq!(r.read(8), 0xFF);
        assert!(!r.overrun);
        assert_eq!(r.read(4), 0);
        assert!(r.overrun);
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
