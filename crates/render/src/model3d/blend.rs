//! Blender's `.blend` files, read without Blender: every mesh object, placed
//! where it is in the scene (its location, rotation and scale, and its
//! parents'), coloured by its materials (the Principled BSDF's base colour,
//! or the viewport colour).
//!
//! A `.blend` is Blender's memory written out in blocks, with a description
//! of every structure in it (the "SDNA") at the end, so fields are found by
//! name and the file reads the same whichever version wrote it: 2.5 to 5.x,
//! 32- or 64-bit, little- or big-endian, plain, gzipped (before 3.0) or
//! zstd-compressed (3.0 on). Meshes are read as Blender keeps them: from 5.0
//! as attributes (`position`, `.corner_vert`, `material_index`), from 3.5
//! as custom-data layers of the same names, and before that as `MVert`,
//! `MPoly` and `MLoop` arrays (or, before 2.63, `MFace`s). Modifiers aren't
//! applied (the mesh is as modelled), and curves, text and other non-mesh
//! objects are left out.

use super::{add_mesh, mul, part_index, quat, rot, scale, translate, weld, Model, VExtra, DEFAULT_COLOR, I4, M4};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// The most a file is read into memory as (after decompressing).
const MAX_BYTES: u64 = 2 << 30;
/// The most vertices, faces or corners one mesh may have.
const MAX_ITEMS: usize = 50_000_000;
/// The deepest chain of parents followed.
const MAX_PARENTS: usize = 64;

pub(super) fn load_blend(path: &Path) -> Result<Model, String> {
    let raw = std::fs::read(path).map_err(|e| format!("couldn't open it ({e})"))?;
    parse_blend(&raw)
}

pub(super) fn parse_blend(raw: &[u8]) -> Result<Model, String> {
    let data = decompress(raw)?;
    let f = File::parse(data)?;
    f.model()
}

/// The file's bytes, unpacked if it was saved compressed.
fn decompress(raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.starts_with(b"BLENDER") {
        return Ok(raw.to_vec());
    }
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(raw).take(MAX_BYTES).read_to_end(&mut out).map_err(|e| format!("couldn't unpack it ({e})"))?;
        return Ok(out);
    }
    if raw.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        // (The decoder is someone else's code: a fault in it is a bad file, not a crash.)
        return std::panic::catch_unwind(|| unzstd(raw)).unwrap_or_else(|_| Err("couldn't unpack it".into()));
    }
    Err("not a Blender file".into())
}

/// Zstandard frames one after another (Blender writes several, and a seek table
/// in a skippable frame at the end).
fn unzstd(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut input = raw;
    let mut out = Vec::new();
    while input.len() >= 8 {
        let &[a, b, c, d, e, f, g, h, ..] = input else { break };
        let magic = u32::from_le_bytes([a, b, c, d]);
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            let n = u32::from_le_bytes([e, f, g, h]) as usize;
            input = input.get(8usize.saturating_add(n)..).unwrap_or(&[]);
            continue;
        }
        let mut dec = ruzstd::decoding::StreamingDecoder::new(&mut input).map_err(|e| format!("couldn't unpack it ({e})"))?;
        let room = MAX_BYTES.saturating_sub(out.len() as u64);
        (&mut dec).take(room).read_to_end(&mut out).map_err(|e| format!("couldn't unpack it ({e})"))?;
        if out.len() as u64 >= MAX_BYTES {
            return Err("it's too big to read".into());
        }
    }
    Ok(out)
}

/// One structure's field: its type, name, where it is in the structure and how big.
#[derive(Clone, Debug)]
struct Field {
    ty: usize,
    /// Without `*`, `[n]` or brackets.
    name: String,
    offset: usize,
    size: usize,
    pointer: bool,
    /// How many (1, or the product of its `[n]`s).
    count: usize,
}

#[derive(Clone, Debug)]
struct Struct {
    ty: usize,
    fields: Vec<Field>,
}

#[derive(Clone, Copy, Debug)]
struct Block {
    code: [u8; 4],
    sdna: usize,
    old: u64,
    /// Where its data is in the file, and how long.
    at: usize,
    len: usize,
    /// The data-block (ID) it belongs to: the last block before it that wasn't `DATA`.
    group: usize,
}

/// A structure somewhere in the file: where it starts, and which it is.
#[derive(Clone, Copy, Debug)]
struct Rec {
    at: usize,
    st: usize,
    /// The block it's in (for finding what its pointers point to).
    block: usize,
}

struct File {
    data: Vec<u8>,
    big_endian: bool,
    ptr: usize,
    blocks: Vec<Block>,
    types: Vec<String>,
    tlen: Vec<usize>,
    structs: Vec<Struct>,
    struct_of: HashMap<String, usize>,
    /// Blocks by (data-block, address), and by address alone.
    in_group: HashMap<(usize, u64), usize>,
    anywhere: HashMap<u64, usize>,
}

impl File {
    fn parse(data: Vec<u8>) -> Result<File, String> {
        let head = data.get(..12).ok_or("not a Blender file")?;
        if !head.starts_with(b"BLENDER") {
            return Err("not a Blender file".into());
        }
        // Before 5.0: "BLENDER", pointer size ('_' 4, '-' 8), endianness ('v' little,
        // 'V' big), version. From 5.0: "BLENDER", the header's length (2 digits), '-',
        // the format (2 digits), endianness, version; blocks with 64-bit lengths.
        let (hl, ptr, big_endian, large) = match head.get(7) {
            Some(b'_') => (12, 4, head.get(8) == Some(&b'V'), false),
            Some(b'-') => (12, 8, head.get(8) == Some(&b'V'), false),
            Some(c) if c.is_ascii_digit() => {
                let hl: usize = std::str::from_utf8(data.get(7..9).ok_or("not a Blender file")?).ok().and_then(|s| s.parse().ok()).ok_or("not a Blender file")?;
                let format: usize = std::str::from_utf8(data.get(10..12).ok_or("not a Blender file")?).ok().and_then(|s| s.parse().ok()).ok_or("not a Blender file")?;
                if format != 1 {
                    return Err(format!("this Blender file format ({format}) is newer than Reelforge reads: save it from Blender 5 or earlier"));
                }
                let endian = data.get(12).copied().unwrap_or(b'v');
                (hl, 8, endian == b'V', true)
            }
            _ => return Err("not a Blender file".into()),
        };
        let mut f = File { data, big_endian, ptr, blocks: Vec::new(), types: Vec::new(), tlen: Vec::new(), structs: Vec::new(), struct_of: HashMap::new(), in_group: HashMap::new(), anywhere: HashMap::new() };
        let mut off = hl;
        let mut group = 0usize;
        let mut dna = None;
        while let Some(code) = f.data.get(off..off.saturating_add(4)).and_then(|c| <[u8; 4]>::try_from(c).ok()) {
            let (sdna, old, len, hs) = if large {
                (f.u(off + 4, 4)?, f.u(off + 8, 8)?, f.u(off + 16, 8)?, 32)
            } else {
                (f.u(off + 8 + ptr, 4)?, f.u(off + 8, ptr)?, f.u(off + 4, 4)?, 16 + ptr)
            };
            let at = off.checked_add(hs).ok_or("damaged")?;
            let len = usize::try_from(len).map_err(|_| "damaged")?;
            let end = at.checked_add(len).filter(|&e| e <= f.data.len()).ok_or("the file is cut short")?;
            if &code == b"ENDB" {
                break;
            }
            let index = f.blocks.len();
            if &code != b"DATA" {
                group = index;
            }
            if &code == b"DNA1" {
                dna = Some((at, len));
            }
            f.blocks.push(Block { code, sdna: sdna as usize, old, at, len, group });
            off = end;
        }
        let (at, len) = dna.ok_or("the file has no structure list (is it damaged?)")?;
        f.read_dna(at, len)?;
        for (i, b) in f.blocks.iter().enumerate() {
            f.in_group.entry((b.group, b.old)).or_insert(i);
            f.anywhere.entry(b.old).or_insert(i);
        }
        Ok(f)
    }

    /// An unsigned number of `n` bytes at `at`, in the file's byte order.
    fn u(&self, at: usize, n: usize) -> Result<u64, String> {
        let b = self.data.get(at..at.saturating_add(n)).filter(|b| b.len() == n).ok_or("the file is cut short")?;
        let mut v = 0u64;
        for (i, &x) in b.iter().enumerate() {
            let shift = if self.big_endian { (n - 1 - i) * 8 } else { i * 8 };
            v |= (x as u64) << shift;
        }
        Ok(v)
    }
    fn i(&self, at: usize, n: usize) -> Option<i64> {
        let v = self.u(at, n).ok()?;
        let bits = n * 8;
        Some(if bits < 64 && v >> (bits - 1) & 1 == 1 { (v as i64) - (1i64 << bits) } else { v as i64 })
    }
    fn f32_at(&self, at: usize) -> Option<f32> {
        Some(f32::from_bits(self.u(at, 4).ok()? as u32))
    }

    fn read_dna(&mut self, at: usize, len: usize) -> Result<(), String> {
        let bad = || "the file's structure list is damaged".to_string();
        let end = at + len;
        let mut q = at;
        let tag = |f: &File, q: &mut usize, t: &[u8; 4]| -> Result<(), String> {
            *q = (*q + 3) & !3;
            if f.data.get(*q..*q + 4) != Some(&t[..]) {
                return Err(bad());
            }
            *q += 4;
            Ok(())
        };
        let strings = |f: &File, q: &mut usize| -> Result<Vec<String>, String> {
            let n = f.u(*q, 4)? as usize;
            *q += 4;
            let mut v = Vec::with_capacity(n.min(100_000));
            for _ in 0..n {
                let rest = f.data.get(*q..end).ok_or_else(bad)?;
                let z = rest.iter().position(|&c| c == 0).ok_or_else(bad)?;
                v.push(String::from_utf8_lossy(rest.get(..z).unwrap_or(&[])).into_owned());
                *q += z + 1;
            }
            Ok(v)
        };
        if self.data.get(q..q + 4) != Some(&b"SDNA"[..]) {
            return Err(bad());
        }
        q += 4;
        tag(self, &mut q, b"NAME")?;
        let names = strings(self, &mut q)?;
        tag(self, &mut q, b"TYPE")?;
        self.types = strings(self, &mut q)?;
        tag(self, &mut q, b"TLEN")?;
        self.tlen = (0..self.types.len()).map(|i| self.u(q + i * 2, 2).map(|v| v as usize)).collect::<Result<_, _>>()?;
        q += self.types.len() * 2;
        tag(self, &mut q, b"STRC")?;
        let n = self.u(q, 4)? as usize;
        q += 4;
        for s in 0..n.min(100_000) {
            let ty = self.u(q, 2)? as usize;
            let nf = self.u(q + 2, 2)? as usize;
            q += 4;
            let mut fields = Vec::with_capacity(nf);
            let mut offset = 0usize;
            for _ in 0..nf {
                let (ft, fname) = (self.u(q, 2)? as usize, self.u(q + 2, 2)? as usize);
                q += 4;
                let raw = names.get(fname).ok_or_else(bad)?;
                let pointer = raw.starts_with('*') || raw.starts_with("(*");
                let count = raw.split('[').skip(1).filter_map(|d| d.split(']').next()?.trim().parse::<usize>().ok()).try_fold(1usize, |a, d| a.checked_mul(d)).unwrap_or(1);
                let base: String = raw.trim_start_matches(['*', '(']).chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                // A function pointer, `(*name)()`, is one pointer.
                let one = if pointer { self.ptr } else { self.tlen.get(ft).copied().ok_or_else(bad)? };
                let size = one.saturating_mul(count);
                fields.push(Field { ty: ft, name: base, offset, size, pointer, count });
                offset = offset.saturating_add(size);
            }
            if let Some(name) = self.types.get(ty) {
                self.struct_of.entry(name.clone()).or_insert(s);
            }
            self.structs.push(Struct { ty, fields });
        }
        Ok(())
    }

    // ---- reading structures ----

    fn rec(&self, block: usize) -> Option<Rec> {
        let b = self.blocks.get(block)?;
        (b.sdna < self.structs.len()).then_some(Rec { at: b.at, st: b.sdna, block })
    }
    fn size_of(&self, st: usize) -> usize {
        self.structs.get(st).and_then(|s| self.tlen.get(s.ty)).copied().unwrap_or(0)
    }
    fn field(&self, r: Rec, name: &str) -> Option<&Field> {
        self.structs.get(r.st)?.fields.iter().find(|f| f.name == name)
    }
    fn has(&self, r: Rec, name: &str) -> bool {
        self.field(r, name).is_some()
    }
    /// A whole-number field (its `i`th, for an array), whatever size it's stored as.
    fn int_n(&self, r: Rec, name: &str, i: usize) -> Option<i64> {
        let f = self.field(r, name)?;
        if f.pointer || i >= f.count {
            return None;
        }
        let one = f.size / f.count.max(1);
        let ty = self.types.get(f.ty)?.as_str();
        if matches!(ty, "float" | "double") || !(1..=8).contains(&one) {
            return None;
        }
        let at = r.at + f.offset + i * one;
        if ty.starts_with('u') || ty == "char" {
            self.u(at, one).ok().map(|v| v as i64)
        } else {
            self.i(at, one)
        }
    }
    fn int(&self, r: Rec, name: &str) -> Option<i64> {
        self.int_n(r, name, 0)
    }
    fn float(&self, r: Rec, name: &str, i: usize) -> Option<f32> {
        let f = self.field(r, name)?;
        if i >= f.count || f.pointer {
            return None;
        }
        match self.types.get(f.ty)?.as_str() {
            "float" => self.f32_at(r.at + f.offset + i * 4),
            "double" => Some(f64::from_bits(self.u(r.at + f.offset + i * 8, 8).ok()?) as f32),
            _ => None,
        }
    }
    fn floats<const N: usize>(&self, r: Rec, name: &str) -> Option<[f32; N]> {
        let mut v = [0.0; N];
        for (i, x) in v.iter_mut().enumerate() {
            *x = self.float(r, name, i)?;
        }
        Some(v)
    }
    fn ptr(&self, r: Rec, name: &str) -> Option<u64> {
        let f = self.field(r, name)?;
        f.pointer.then(|| self.u(r.at + f.offset, self.ptr).ok()).flatten().filter(|&p| p != 0)
    }
    /// A structure inside this one.
    fn sub(&self, r: Rec, name: &str) -> Option<Rec> {
        let f = self.field(r, name)?;
        if f.pointer {
            return None;
        }
        let st = *self.struct_of.get(self.types.get(f.ty)?)?;
        Some(Rec { at: r.at + f.offset, st, block: r.block })
    }
    /// The block a pointer from block `from` points at: in the same data-block
    /// first (5.x numbers each data-block's pointers on their own), then anywhere.
    fn deref(&self, from: usize, addr: u64) -> Option<usize> {
        let g = self.blocks.get(from)?.group;
        self.in_group.get(&(g, addr)).or_else(|| self.anywhere.get(&addr)).copied()
    }
    fn target(&self, r: Rec, name: &str) -> Option<usize> {
        self.deref(r.block, self.ptr(r, name)?)
    }
    /// A block's `i`th element, as the structure the block says it holds (or `st`).
    fn element(&self, block: usize, st: Option<usize>, i: usize) -> Option<Rec> {
        let b = self.blocks.get(block)?;
        let st = st.or_else(|| (b.sdna < self.structs.len()).then_some(b.sdna))?;
        let size = self.size_of(st);
        let at = b.at.checked_add(i.checked_mul(size)?)?;
        (size > 0 && at + size <= b.at + b.len).then_some(Rec { at, st, block })
    }
    fn bytes(&self, block: usize) -> &[u8] {
        self.blocks.get(block).and_then(|b| self.data.get(b.at..b.at + b.len)).unwrap_or(&[])
    }
    /// The pointer at position `i` of an array of pointers in `block`.
    fn ptr_in(&self, block: usize, i: usize) -> Option<u64> {
        let b = self.blocks.get(block)?;
        let at = b.at + i * self.ptr;
        (at + self.ptr <= b.at + b.len).then(|| self.u(at, self.ptr).ok()).flatten().filter(|&p| p != 0)
    }
    fn string_field(&self, r: Rec, name: &str) -> String {
        let Some(f) = self.field(r, name) else { return String::new() };
        let s = self.data.get(r.at + f.offset..r.at + f.offset + f.size).unwrap_or(&[]);
        let z = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(s.get(..z).unwrap_or(&[])).into_owned()
    }
    /// A data-block's name (without the two letters Blender puts before it).
    fn id_name(&self, r: Rec) -> String {
        let Some(id) = self.sub(r, "id") else { return String::new() };
        let n = self.string_field(id, "name");
        n.get(2..).unwrap_or("").to_string()
    }
    fn c_string(&self, block: usize) -> String {
        let s = self.bytes(block);
        let z = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(s.get(..z).unwrap_or(&[])).into_owned()
    }

    // ---- the scene ----

    fn model(&self) -> Result<Model, String> {
        let mut out = Model { z_up: true, ..Default::default() };
        let mut meshes: HashMap<usize, Option<Mesh>> = HashMap::new();
        let mut objects = 0;
        for (i, b) in self.blocks.iter().enumerate() {
            if &b.code != b"OB\0\0" {
                continue;
            }
            let Some(ob) = self.rec(i) else { continue };
            // (1: a mesh.)
            if self.int(ob, "type") != Some(1) {
                continue;
            }
            // Turned off for rendering: a helper, left out.
            if self.int(ob, "restrictflag").is_some_and(|f| f & 4 != 0) {
                continue;
            }
            let Some(me) = self.target(ob, "data") else { continue };
            let mesh = meshes.entry(me).or_insert_with(|| self.mesh(me)).clone();
            let Some(mesh) = mesh else { continue };
            objects += 1;
            let world = self.world(ob, 0);
            let name = self.id_name(ob);
            let mats = self.materials(ob, me);
            self.add(&mut out, &mesh, &mats, &name, &world);
        }
        if objects == 0 {
            return Err("the file has no mesh objects to show".into());
        }
        out.normals.clear();
        Ok(weld(out))
    }

    /// An object's place in the world: its own transform under its parent's.
    fn world(&self, ob: Rec, depth: usize) -> M4 {
        // Files before 4.2 keep the matrix Blender last worked out (with constraints).
        for name in ["obmat", "object_to_world"] {
            if self.has(ob, name) {
                if let Some(m) = self.matrix(ob, name) {
                    return m;
                }
            }
        }
        let local = self.local(ob);
        match self.target(ob, "parent").and_then(|p| self.rec(p)).filter(|_| depth < MAX_PARENTS) {
            Some(parent) => {
                let pinv = self.matrix(ob, "parentinv").unwrap_or(I4);
                mul(&mul(&self.world(parent, depth + 1), &pinv), &local)
            }
            None => local,
        }
    }

    /// A `float [4][4]` field (Blender's are column-major, like ours).
    fn matrix(&self, r: Rec, name: &str) -> Option<M4> {
        let v: [f32; 16] = self.floats(r, name)?;
        let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = v;
        let mat = [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]];
        (v.iter().all(|x| x.is_finite()) && v.iter().any(|&x| x != 0.0)).then_some(mat)
    }

    /// Location, rotation and scale (with their deltas), as Blender combines them.
    fn local(&self, ob: Rec) -> M4 {
        let v3 = |n: &str, d: [f32; 3]| self.floats::<3>(ob, n).filter(|v| v.iter().all(|x| x.is_finite())).unwrap_or(d);
        let loc = v3("loc", [0.0; 3]);
        let dloc = v3("dloc", [0.0; 3]);
        let size = v3("size", [1.0; 3]);
        let dscale = if self.has(ob, "dscale") { v3("dscale", [1.0; 3]) } else { [1.0; 3] };
        let r = match self.int(ob, "rotmode").unwrap_or(1) {
            0 => {
                let q = |n: &str| self.floats::<4>(ob, n).filter(|q| q.iter().any(|x| *x != 0.0) && q.iter().all(|x| x.is_finite())).unwrap_or([1.0, 0.0, 0.0, 0.0]);
                let (a, d) = (q("quat"), q("dquat"));
                // (Blender's are w, x, y, z.)
                mul(&quat_wxyz(d), &quat_wxyz(a))
            }
            -1 => {
                let axis = v3("rotAxis", [0.0, 1.0, 0.0]);
                let angle = self.float(ob, "rotAngle", 0).filter(|a| a.is_finite()).unwrap_or(0.0);
                let daxis = v3("drotAxis", [0.0, 1.0, 0.0]);
                let dangle = self.float(ob, "drotAngle", 0).filter(|a| a.is_finite()).unwrap_or(0.0);
                mul(&axis_angle(daxis, dangle), &axis_angle(axis, angle))
            }
            order => {
                let (e, de) = (v3("rot", [0.0; 3]), v3("drot", [0.0; 3]));
                mul(&euler(de, order), &euler(e, order))
            }
        };
        let t = [loc[0] + dloc[0], loc[1] + dloc[1], loc[2] + dloc[2]];
        let s = [size[0] * dscale[0], size[1] * dscale[1], size[2] * dscale[2]];
        mul(&mul(&translate(t), &r), &scale(s))
    }

    /// The colour and name of each of the object's material slots.
    fn materials(&self, ob: Rec, me: usize) -> Vec<(String, [f32; 4])> {
        let Some(mesh) = self.rec(me) else { return Vec::new() };
        let n = self.int(mesh, "totcol").unwrap_or(0).clamp(0, 32767) as usize;
        let mesh_mats = self.target(mesh, "mat");
        let ob_mats = self.target(ob, "mat");
        let bits = self.target(ob, "matbits");
        (0..n)
            .map(|i| {
                // The object's own material where it says so, else the mesh's.
                let on_object = bits.and_then(|b| self.bytes(b).get(i).copied()).is_some_and(|b| b != 0);
                let list = if on_object { ob_mats.or(mesh_mats) } else { mesh_mats };
                let from = if on_object && ob_mats.is_some() { ob.block } else { me };
                let ma = list.and_then(|l| self.ptr_in(l, i)).and_then(|p| self.deref(from, p)).and_then(|b| self.rec(b));
                match ma {
                    Some(ma) => (self.id_name(ma), self.material_colour(ma)),
                    None => (String::new(), DEFAULT_COLOR),
                }
            })
            .collect()
    }

    /// A material's colour: its Principled BSDF's (or Diffuse BSDF's) base colour,
    /// or the viewport colour when the node's is untouched.
    fn material_colour(&self, ma: Rec) -> [f32; 4] {
        let viewport = [self.float(ma, "r", 0), self.float(ma, "g", 0), self.float(ma, "b", 0)];
        let viewport = match viewport {
            [Some(r), Some(g), Some(b)] if [r, g, b].iter().all(|v| v.is_finite()) => Some([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), 1.0]),
            _ => None,
        };
        let default = [0.8, 0.8, 0.8, 1.0];
        match (self.node_colour(ma), viewport) {
            (Some(n), Some(v)) if close(n, default) && !close(v, default) => v,
            (Some(n), _) => n,
            (None, Some(v)) => v,
            (None, None) => DEFAULT_COLOR,
        }
    }

    fn node_colour(&self, ma: Rec) -> Option<[f32; 4]> {
        let tree = self.rec(self.target(ma, "nodetree")?)?;
        let nodes = self.sub(tree, "nodes")?;
        let mut node = self.target(nodes, "first");
        for _ in 0..10_000 {
            let n = self.rec(node?)?;
            let id = self.string_field(n, "idname");
            let socket = match id.as_str() {
                "ShaderNodeBsdfPrincipled" => Some("Base Color"),
                "ShaderNodeBsdfDiffuse" => Some("Color"),
                _ => None,
            };
            if let Some(want) = socket {
                if let Some(c) = self.socket_colour(n, want) {
                    return Some(c);
                }
            }
            node = self.target(n, "next");
        }
        None
    }

    fn socket_colour(&self, node: Rec, want: &str) -> Option<[f32; 4]> {
        let inputs = self.sub(node, "inputs")?;
        let mut s = self.target(inputs, "first");
        for _ in 0..1000 {
            let sock = self.rec(s?)?;
            if self.string_field(sock, "name") == want || self.string_field(sock, "identifier") == want.replace(' ', "_") {
                let v = self.rec(self.target(sock, "default_value")?)?;
                let [r, g, b, _]: [f32; 4] = self.floats(v, "value")?;
                return [r, g, b].iter().all(|x| x.is_finite()).then(|| [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0), 1.0]);
            }
            s = self.target(sock, "next");
        }
        None
    }

    // ---- meshes ----

    /// A mesh's named arrays: from 5.0's attribute storage and the custom-data
    /// layers before it, by name (and MVert, MPoly, MLoop layers as `#mvert`…).
    fn arrays(&self, me: Rec) -> HashMap<String, Array> {
        let mut v = HashMap::new();
        if let Some(store) = self.sub(me, "attribute_storage") {
            let n = self.int(store, "dna_attributes_num").unwrap_or(0).clamp(0, 10_000) as usize;
            if let Some(list) = self.target(store, "dna_attributes") {
                let st = self.struct_of.get("Attribute").copied();
                for i in 0..n {
                    let Some(a) = self.element(list, st, i) else { break };
                    let Some(name) = self.target(a, "name").map(|b| self.c_string(b)) else { continue };
                    // (Storage 0: an array; 1: one value for all.)
                    if self.int(a, "storage_type") != Some(0) {
                        continue;
                    }
                    let Some(arr) = self.target(a, "data").and_then(|b| self.rec(b)) else { continue };
                    let Some(block) = self.target(arr, "data") else { continue };
                    v.insert(name, Array { block });
                }
            }
        }
        for cd in ["vdata", "pdata", "ldata", "fdata"] {
            let Some(c) = self.sub(me, cd) else { continue };
            let n = self.int(c, "totlayer").unwrap_or(0).clamp(0, 10_000) as usize;
            let Some(layers) = self.target(c, "layers") else { continue };
            let st = self.struct_of.get("CustomDataLayer").copied();
            for i in 0..n {
                let Some(l) = self.element(layers, st, i) else { break };
                let Some(block) = self.target(l, "data") else { continue };
                let ty = self.int(l, "type").unwrap_or(-1);
                let name = self.string_field(l, "name");
                let key = match (name.is_empty(), ty) {
                    // (Unnamed layers by type: MVert, MPoly, MLoop.)
                    (_, 0) => "#mvert".to_string(),
                    (_, 25) => "#mpoly".to_string(),
                    (_, 26) => "#mloop".to_string(),
                    (false, _) => name,
                    (true, _) => continue,
                };
                v.entry(key).or_insert(Array { block });
            }
        }
        v
    }

    fn mesh(&self, block: usize) -> Option<Mesh> {
        let me = self.rec(block)?;
        let count = |n: &str| self.int(me, n).map(|v| (v.max(0) as usize).min(MAX_ITEMS));
        let nv = count("totvert").or_else(|| count("verts_num")).unwrap_or(0);
        let np = count("totpoly").or_else(|| count("faces_num")).unwrap_or(0);
        let arrays = self.arrays(me);
        let raw = |name: &str| arrays.get(name).map(|a| self.bytes(a.block));
        let f32s = |b: &[u8], i: usize| -> Option<f32> { self.f32_at_slice(b, i) };
        let i32s = |b: &[u8], i: usize| -> Option<i64> { self.i32_at_slice(b, i) };

        // Points.
        let mut pos: Vec<[f32; 3]> = Vec::new();
        if let Some(b) = raw("position") {
            pos = (0..nv.min(b.len() / 12)).map_while(|i| Some([f32s(b, i * 3)?, f32s(b, i * 3 + 1)?, f32s(b, i * 3 + 2)?])).collect();
        } else if let Some(mv) = self.target(me, "mvert").or_else(|| arrays.get("#mvert").map(|a| a.block)) {
            pos = (0..nv).map_while(|i| self.element(mv, None, i).and_then(|r| self.floats::<3>(r, "co"))).collect();
        }
        if pos.is_empty() {
            return None;
        }

        // Faces: (first corner, corners, material).
        let mut faces: Vec<(usize, usize, i64)> = Vec::new();
        let mut corners: Vec<u32> = Vec::new();
        let mat_index = raw("material_index");
        if let Some(off) = self.target(me, "poly_offset_indices").or_else(|| self.target(me, "face_offset_indices")) {
            let b = self.bytes(off);
            let offs: Vec<i64> = (0..=np.min(b.len() / 4).saturating_sub(0)).map_while(|i| i32s(b, i)).collect();
            for (f, w) in offs.windows(2).enumerate() {
                let &[a, e] = w else { break };
                let (Ok(a), Ok(e)) = (usize::try_from(a), usize::try_from(e)) else { break };
                let m = mat_index.and_then(|b| i32s(b, f)).unwrap_or(0);
                faces.push((a, e.saturating_sub(a), m));
            }
        } else if let Some(mp) = self.target(me, "mpoly").or_else(|| arrays.get("#mpoly").map(|a| a.block)) {
            for f in 0..np {
                let Some(r) = self.element(mp, None, f) else { break };
                let (Some(a), Some(n)) = (self.int(r, "loopstart"), self.int(r, "totloop")) else { break };
                let m = mat_index.and_then(|b| i32s(b, f)).or_else(|| self.int(r, "mat_nr")).unwrap_or(0);
                faces.push((a.max(0) as usize, n.max(0) as usize, m));
            }
        }
        if !faces.is_empty() {
            let nl = count("totloop").or_else(|| count("corners_num")).unwrap_or(0);
            if let Some(b) = raw(".corner_vert") {
                corners = (0..nl.min(b.len() / 4)).map_while(|i| i32s(b, i).map(|v| v.max(0) as u32)).collect();
            } else if let Some(ml) = self.target(me, "mloop").or_else(|| arrays.get("#mloop").map(|a| a.block)) {
                corners = (0..nl).map_while(|i| self.element(ml, None, i).and_then(|r| self.int(r, "v")).map(|v| v.max(0) as u32)).collect();
            }
        } else if let Some(mf) = self.target(me, "mface") {
            // Before 2.63: triangles and quads (a quad's fourth corner isn't 0).
            let nf = count("totface").unwrap_or(0);
            for f in 0..nf {
                let Some(r) = self.element(mf, None, f) else { break };
                let v: Vec<u32> = ["v1", "v2", "v3", "v4"].iter().filter_map(|n| self.int(r, n)).map(|v| v.max(0) as u32).collect();
                let n = if v.get(3).is_some_and(|&x| x != 0) { 4 } else { 3 };
                faces.push((corners.len(), n, self.int(r, "mat_nr").unwrap_or(0)));
                corners.extend(v.iter().take(n));
            }
        }
        if faces.is_empty() || corners.is_empty() {
            return None;
        }
        Some(Mesh { pos, faces, corners })
    }

    fn f32_at_slice(&self, b: &[u8], i: usize) -> Option<f32> {
        let &[p, q, r, t] = b.get(i * 4..i * 4 + 4)? else { return None };
        let a = [p, q, r, t];
        let v = if self.big_endian { f32::from_be_bytes(a) } else { f32::from_le_bytes(a) };
        // (Not a number: the point goes to the middle, the rest still read.)
        Some(if v.is_finite() { v } else { 0.0 })
    }
    fn i32_at_slice(&self, b: &[u8], i: usize) -> Option<i64> {
        let &[p, q, r, t] = b.get(i * 4..i * 4 + 4)? else { return None };
        let a = [p, q, r, t];
        Some(if self.big_endian { i32::from_be_bytes(a) } else { i32::from_le_bytes(a) } as i64)
    }

    /// Adds an object's mesh: each face fanned into triangles, coloured by its material.
    fn add(&self, out: &mut Model, mesh: &Mesh, mats: &[(String, [f32; 4])], object: &str, world: &M4) {
        let mut pos = Vec::new();
        let mut colors = Vec::new();
        let mut extra = Vec::new();
        let n = mesh.pos.len();
        for &(start, count, mat) in &mesh.faces {
            let Some(poly) = start.checked_add(count).and_then(|e| mesh.corners.get(start..e)) else { continue };
            if poly.len() < 3 || poly.iter().any(|&v| v as usize >= n) {
                continue;
            }
            let slot = usize::try_from(mat).ok().and_then(|m| mats.get(m));
            let (name, colour) = match slot {
                Some((name, c)) if !name.is_empty() => (name.as_str(), *c),
                Some((_, c)) => (object, *c),
                None => (object, DEFAULT_COLOR),
            };
            let part = part_index(&mut out.parts, name);
            let Some((&first, rest)) = poly.split_first() else { continue };
            for pair in rest.windows(2) {
                let &[b, c] = pair else { continue };
                for v in [first, b, c] {
                    pos.push(mesh.pos.get(v as usize).copied().unwrap_or([0.0; 3]));
                    colors.push(colour);
                    extra.push(VExtra { part, ..VExtra::NONE });
                }
            }
        }
        let seq: Vec<u32> = (0..pos.len() as u32).collect();
        add_mesh(out, world, &pos, None, &colors, &extra, &seq);
    }
}

/// A named array of a mesh: the block its values are in.
#[derive(Clone, Copy, Debug)]
struct Array {
    block: usize,
}

#[derive(Clone, Debug)]
struct Mesh {
    pos: Vec<[f32; 3]>,
    faces: Vec<(usize, usize, i64)>,
    corners: Vec<u32>,
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).take(3).all(|(x, y)| (x - y).abs() < 1e-4)
}

fn quat_wxyz(q: [f32; 4]) -> M4 {
    let [w, x, y, z] = q;
    let l = (w * w + x * x + y * y + z * z).sqrt();
    if l.is_nan() || l <= 1e-12 {
        return I4;
    }
    quat([x / l, y / l, z / l, w / l])
}

fn axis_angle(axis: [f32; 3], angle: f32) -> M4 {
    let l = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if l.is_nan() || l <= 1e-12 || angle == 0.0 {
        return I4;
    }
    let (s, c) = (angle / 2.0).sin_cos();
    quat([axis[0] / l * s, axis[1] / l * s, axis[2] / l * s, c])
}

/// Blender's Euler turns (radians) in rotation mode `order` (1 XYZ … 6 ZYX):
/// the first axis named turns first.
fn euler(e: [f32; 3], order: i64) -> M4 {
    let seq: [usize; 3] = match order {
        2 => [0, 2, 1],
        3 => [1, 0, 2],
        4 => [1, 2, 0],
        5 => [2, 0, 1],
        6 => [2, 1, 0],
        _ => [0, 1, 2],
    };
    seq.iter().fold(I4, |m, &a| mul(&rot(a, e.get(a).copied().unwrap_or(0.0).to_degrees()), &m))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::excessive_precision)]
    const CHILD: [[f32; 3]; 4] = [[-0.08718174695968628, -1.9262444972991943, 3.936856985092163], [1.6679832935333252, -0.9673933982849121, 3.936856985092163], [-0.5345438122749329, -1.107353687286377, 5.705835819244385], [1.2206213474273682, -0.14850261807441711, 5.705835819244385]];

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// Two files saved by Blender (4.2: custom-data layers, kept matrices; 5.3: attribute
    /// storage, per-data-block pointers, no matrices): a red cube, a scaled and turned
    /// cylinder with blue sides and green caps (node colours), and a plane parented to it
    /// with a quaternion turn. (Stripped down to their objects, meshes and materials.)
    #[test]
    fn reads_blender_4_and_5_files() {
        // (Where Blender itself puts the plane's corners.)
        let child = CHILD;
        for f in ["scene-4.2.blend", "scene-5.3.blend"] {
            let m = parse_blend(&fixture(f)).unwrap();
            assert_eq!(m.indices.len() / 3, 12 + 16 + 12 + 2, "{f}");
            assert_eq!(m.parts, ["Red", "Blue", "Green", "Child"], "{f}");
            assert!(m.z_up);
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for p in &m.positions {
                for k in 0..3 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
            let (want_lo, want_hi) = ([-2.1721766, -2.3823545, -1.5017135], [4.0, 2.3823545, 5.705_836]);
            for k in 0..3 {
                assert!((lo[k] - want_lo[k]).abs() < 1e-4 && (hi[k] - want_hi[k]).abs() < 1e-4, "{f}: {lo:?} {hi:?}");
            }
            for c in child {
                assert!(m.positions.iter().any(|p| (0..3).all(|k| (p[k] - c[k]).abs() < 1e-4)), "{f}: the plane's corner {c:?}");
            }
            // Colours by part: the cube's viewport red, the cylinder's node blue and green.
            let colour_of = |part: &str| {
                let i = m.parts.iter().position(|p| p == part).unwrap() as u16;
                m.colors[m.extra.iter().position(|x| x.part == i).unwrap()]
            };
            assert_eq!(colour_of("Red"), [1.0, 0.0, 0.0, 1.0]);
            assert_eq!(colour_of("Blue"), [0.0, 0.0, 1.0, 1.0]);
            assert_eq!(colour_of("Green"), [0.0, 1.0, 0.0, 1.0]);
            assert_eq!(colour_of("Child"), DEFAULT_COLOR);
            let greens = m.extra.iter().filter(|x| x.part == 2).count();
            assert!(greens >= 16, "the caps are green: {greens}");
        }
    }

    #[test]
    fn reads_it_plain_gzipped_or_zstd() {
        let zstd = fixture("scene-5.3.blend");
        let plain = decompress(&zstd).unwrap();
        assert!(plain.starts_with(b"BLENDER17-01v0503"));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &plain).unwrap();
        let gz = gz.finish().unwrap();
        for bytes in [&zstd, &plain, &gz] {
            assert_eq!(parse_blend(bytes).unwrap().indices.len(), 42 * 3);
        }
    }

    /// Damaged files are errors, never a crash: cut short anywhere, bytes changed,
    /// or not a Blender file at all.
    #[test]
    fn damaged_files_do_no_harm() {
        let plain = decompress(&fixture("scene-4.2.blend")).unwrap();
        for cut in (0..plain.len()).step_by(997) {
            let _ = parse_blend(plain.get(..cut).unwrap());
        }
        let mut x = 0x2545F491u32;
        for _ in 0..200 {
            let mut d = plain.clone();
            for _ in 0..20 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let i = x as usize % d.len();
                d[i] = (x >> 8) as u8;
            }
            let _ = parse_blend(&d);
        }
        assert!(parse_blend(b"").is_err());
        assert!(parse_blend(b"BLENDER-v279").is_err());
        assert!(parse_blend(b"BLENDER99-07v0600xxxx").is_err());
        assert!(parse_blend(&[0x28, 0xb5, 0x2f, 0xfd, 1, 2, 3]).is_err());
    }
}
