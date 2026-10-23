//! Variable-length (Huffman-style) code tables, decoded with one table lookup.

use crate::bits::BitReader;

/// The first table looks at this many bits; longer codes go on to smaller
/// tables (keeps the tables small enough to stay in the CPU's cache).
const ROOT_BITS: u32 = 9;
/// The most bits a table below the root looks at; longer codes chain on.
const SUB_BITS: u32 = 8;

/// A multi-level lookup table. Entries are packed in a u32:
/// a symbol: `sym << 8 | len` (len 1–32, the bits to skip at this level);
/// a further table: `1 << 31 | offset << 8 | bits`; no code: 0.
pub struct Vlc {
    root_bits: u32,
    table: Vec<u32>,
}

const SUB: u32 = 1 << 31;

/// Fill the `bits`-bit table at `base` with `entries` (code, length, symbol),
/// whose codes are measured from this table's first bit.
fn fill(table: &mut Vec<u32>, base: usize, bits: u32, entries: &[(u32, u32, u32)]) {
    let mut groups = std::collections::BTreeMap::<u32, Vec<(u32, u32, u32)>>::new();
    for &(code, len, sym) in entries {
        if len <= bits {
            let shift = bits - len;
            let start = base + ((code as usize) << shift);
            for e in &mut table[start..start + (1 << shift)] {
                *e = sym << 8 | len;
            }
        } else {
            let rest = len - bits;
            groups.entry(code >> rest).or_default().push((code & ((1u64 << rest) - 1) as u32, rest, sym));
        }
    }
    for (prefix, sub) in groups {
        let depth = sub.iter().map(|e| e.1).max().unwrap_or(1).min(SUB_BITS);
        let offset = table.len();
        table[base + prefix as usize] = SUB | (offset as u32) << 8 | depth;
        table.resize(offset + (1 << depth), 0);
        fill(table, offset, depth, &sub);
    }
}

impl Vlc {
    /// Build from parallel lists of codes and their lengths; symbol `i` is
    /// `symbols[i]` if given, else `i`. Codes can be up to 32 bits long.
    pub fn new(codes: &[u32], lens: &[u8], symbols: Option<&[u16]>) -> Vlc {
        let max_bits = lens.iter().copied().max().unwrap_or(1).max(1) as u32;
        assert!(max_bits <= 32, "VLC codes longer than 32 bits");
        let root_bits = max_bits.min(ROOT_BITS);
        let entries: Vec<(u32, u32, u32)> = codes
            .iter()
            .zip(lens)
            .enumerate()
            .filter(|(_, (_, &l))| l > 0)
            .map(|(i, (&c, &l))| (c, l as u32, symbols.map_or(i as u16, |s| s[i]) as u32))
            .collect();
        let mut table = vec![0u32; 1 << root_bits];
        fill(&mut table, 0, root_bits, &entries);
        Vlc { root_bits, table }
    }

    /// Read one symbol (None if the bits aren't a valid code).
    #[inline(always)]
    pub fn read(&self, r: &mut BitReader) -> Option<u16> {
        let mut e = self.table[r.peek(self.root_bits) as usize];
        if e & SUB != 0 {
            r.skip(self.root_bits as usize);
            e = self.table[((e & !SUB) >> 8) as usize + r.peek(e & 0xFF) as usize];
            while e & SUB != 0 {
                r.skip(SUB_BITS as usize);
                e = self.table[((e & !SUB) >> 8) as usize + r.peek(e & 0xFF) as usize];
            }
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

    #[test]
    fn codes_longer_than_two_tables_chain_on() {
        // 1, 2, 20, 26 and 26 bits: 0, 10, 111 then 17 zeros, 11 then 22 zeros then 10 or 11.
        let codes = [0b0, 0b10, 0b111 << 17, (0b11 << 24) | 0b10, (0b11 << 24) | 0b11];
        let lens = [1, 2, 20, 26, 26];
        let v = Vlc::new(&codes, &lens, None);
        let mut bits = String::new();
        for i in [3usize, 0, 4, 2, 1, 3] {
            bits += &format!("{:0w$b}", codes[i], w = lens[i] as usize);
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = (0..bits.len() / 8).map(|i| u8::from_str_radix(&bits[i * 8..i * 8 + 8], 2).unwrap()).collect();
        let mut r = BitReader::new(&bytes);
        let got: Vec<u16> = (0..6).map(|_| v.read(&mut r).unwrap()).collect();
        assert_eq!(got, [3, 0, 4, 2, 1, 3]);
    }
}
