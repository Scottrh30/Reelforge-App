//! Variable-length (Huffman-style) code tables, decoded with one table lookup.

use crate::bits::BitReader;

/// The first table looks at this many bits; longer codes go on to a small
/// second table (keeps the tables small enough to stay in the CPU's cache).
const ROOT_BITS: u32 = 9;

/// A two-level lookup table. Entries are packed in a u32:
/// a symbol: `sym << 8 | len` (len 1–16, the bits to skip at this level);
/// a second table: `1 << 31 | offset << 8 | bits`; no code: 0.
pub struct Vlc {
    root_bits: u32,
    table: Vec<u32>,
}

const SUB: u32 = 1 << 31;

impl Vlc {
    /// Build from parallel lists of codes and their lengths; symbol `i` is
    /// `symbols[i]` if given, else `i`. Codes longer than 16 bits aren't supported.
    pub fn new(codes: &[u32], lens: &[u8], symbols: Option<&[u16]>) -> Vlc {
        let max_bits = lens.iter().copied().max().unwrap_or(1).max(1) as u32;
        assert!(max_bits <= 16, "VLC codes longer than 16 bits");
        let root_bits = max_bits.min(ROOT_BITS);
        let mut table = vec![0u32; 1 << root_bits];
        let sym = |i: usize| symbols.map_or(i as u16, |s| s[i]) as u32;
        // Short codes fill the root directly.
        for (i, (&code, &len)) in codes.iter().zip(lens).enumerate() {
            let len = len as u32;
            if len == 0 || len > root_bits {
                continue;
            }
            let shift = root_bits - len;
            let start = (code as usize) << shift;
            for e in &mut table[start..start + (1 << shift)] {
                *e = sym(i) << 8 | len;
            }
        }
        // Long codes: one second table per root prefix, as deep as its longest code.
        let mut depth = std::collections::BTreeMap::<u32, u32>::new();
        for (&code, &len) in codes.iter().zip(lens) {
            let len = len as u32;
            if len > root_bits {
                let d = depth.entry(code >> (len - root_bits)).or_default();
                *d = (*d).max(len - root_bits);
            }
        }
        for (&prefix, &bits) in &depth {
            let offset = table.len() as u32;
            table[prefix as usize] = SUB | offset << 8 | bits;
            table.resize(table.len() + (1 << bits), 0);
        }
        for (i, (&code, &len)) in codes.iter().zip(lens).enumerate() {
            let len = len as u32;
            if len <= root_bits {
                continue;
            }
            let e = table[(code >> (len - root_bits)) as usize];
            let (offset, bits) = (((e & !SUB) >> 8) as usize, e & 0xFF);
            let rest = len - root_bits;
            let low = code & ((1 << rest) - 1);
            let shift = bits - rest;
            let start = offset + ((low as usize) << shift);
            for e in &mut table[start..start + (1 << shift)] {
                *e = sym(i) << 8 | rest;
            }
        }
        Vlc { root_bits, table }
    }

    /// Read one symbol (None if the bits aren't a valid code).
    #[inline(always)]
    pub fn read(&self, r: &mut BitReader) -> Option<u16> {
        let mut e = self.table[r.peek(self.root_bits) as usize];
        if e & SUB != 0 {
            r.skip(self.root_bits as usize);
            e = self.table[((e & !SUB) >> 8) as usize + r.peek(e & 0xFF) as usize];
        }
        let len = e & 0xFF;
        if len == 0 {
            return None;
        }
        r.skip(len as usize);
        Some((e >> 8) as u16)
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

    #[test]
    fn long_codes_use_a_second_table() {
        // Codes of 1, 3, 10, 12, 12 and 16 bits (a prefix code): 0, 100, 1010000000,
        // 101000000100, 101000000101, 1111111111111111.
        let codes = [0b0, 0b100, 0b1010000000, 0b101000000100, 0b101000000101, 0xFFFF];
        let lens = [1, 3, 10, 12, 12, 16];
        let v = Vlc::new(&codes, &lens, Some(&[10, 11, 12, 13, 14, 15]));
        // Write them all twice, then decode.
        let mut bits = String::new();
        for _ in 0..2 {
            for (c, l) in codes.iter().zip(lens) {
                bits += &format!("{:0w$b}", c, w = l as usize);
            }
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = (0..bits.len() / 8).map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap()).collect();
        let mut r = BitReader::new(&bytes);
        let got: Vec<u16> = (0..12).map(|_| v.read(&mut r).unwrap()).collect();
        assert_eq!(got, [10, 11, 12, 13, 14, 15, 10, 11, 12, 13, 14, 15]);
        // Nothing starts 1011.
        let d = [0b1011_0000, 0];
        assert_eq!(v.read(&mut BitReader::new(&d)), None);
    }
}
