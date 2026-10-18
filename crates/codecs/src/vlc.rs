//! Variable-length (Huffman-style) code tables, decoded with one table lookup.

use crate::bits::BitReader;

/// A lookup table over the next `max_bits` bits: each entry is the symbol
/// and its code length (0 = no code starts with these bits).
pub struct Vlc {
    max_bits: u32,
    table: Vec<(u16, u8)>,
}

impl Vlc {
    /// Build from parallel lists of codes and their lengths; symbol `i` is
    /// `symbols[i]` if given, else `i`. Codes longer than 16 bits aren't supported.
    pub fn new(codes: &[u32], lens: &[u8], symbols: Option<&[u16]>) -> Vlc {
        let max_bits = lens.iter().copied().max().unwrap_or(1).max(1) as u32;
        assert!(max_bits <= 16, "VLC codes longer than 16 bits");
        let mut table = vec![(0u16, 0u8); 1 << max_bits];
        for (i, (&code, &len)) in codes.iter().zip(lens).enumerate() {
            if len == 0 {
                continue;
            }
            let sym = symbols.map_or(i as u16, |s| s[i]);
            let shift = max_bits - len as u32;
            let start = (code as usize) << shift;
            for e in &mut table[start..start + (1 << shift)] {
                *e = (sym, len);
            }
        }
        Vlc { max_bits, table }
    }

    /// Read one symbol (None if the bits aren't a valid code).
    #[inline]
    pub fn read(&self, r: &mut BitReader) -> Option<u16> {
        let (sym, len) = self.table[r.peek(self.max_bits) as usize];
        if len == 0 {
            return None;
        }
        r.skip(len as usize);
        Some(sym)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_prefix_code() {
        // a=0, b=10, c=110, d=111
        let v = Vlc::new(&[0, 2, 6, 7], &[1, 2, 3, 3], None);
        let d = [0b0101_1011, 0b1000_0000];
        let mut r = BitReader::new(&d);
        let got: Vec<u16> = (0..5).map(|_| v.read(&mut r).unwrap()).collect();
        assert_eq!(got, vec![0, 1, 2, 3, 0]);
    }
}
