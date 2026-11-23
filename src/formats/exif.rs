//! EXIF: the TIFF-structured block cameras write (IFD0, the Exif IFD, the
//! GPS IFD, the interoperability IFD and IFD1 with the embedded thumbnail).
//!
//! The block is parsed into an editable model ([`Exif`]) that keeps every
//! entry's raw bytes in the original byte order, and written back as a
//! compact, valid TIFF blob. The same model serves TIFF files
//! ([`Exif::parse_tiff`]), where the IFDs are appended to the file and the
//! picture data never moves.

use super::imgutil::{self, drops, drops_icc, drops_orientation, nice_date, text_of, trim_nul};
use super::{icc, iptc, jpeg, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Field, Flag, GeoPoint, Result};
use crate::util::DateTime;
use std::collections::HashSet;

/// The prefix of EXIF data in a JPEG APP1 segment.
pub const APP1_HEADER: &[u8] = b"Exif\0\0";

const SOURCE: &str = "EXIF";

/// Which directory an entry lives in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IfdKind {
    Ifd0,
    Exif,
    Gps,
    Interop,
    Ifd1,
}

/// One directory entry; `data` holds `count` values in the block's byte order.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub tag: u16,
    pub typ: u16,
    pub count: u32,
    pub data: Vec<u8>,
    /// Where the value was stored out of line in the original (offset, length).
    pub src: Option<(u32, u32)>,
}

/// A parsed EXIF / TIFF metadata block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exif {
    /// Little-endian ("II") byte order.
    pub le: bool,
    /// The block is a whole TIFF file: the IFD chain after IFD0 holds pages,
    /// not a thumbnail, and offsets refer to the original file.
    pub tiff_mode: bool,
    pub ifd0: Vec<Entry>,
    pub exif: Vec<Entry>,
    pub gps: Vec<Entry>,
    pub interop: Vec<Entry>,
    pub ifd1: Vec<Entry>,
    /// The JPEG thumbnail referenced by IFD1.
    pub thumbnail: Option<Vec<u8>>,
    /// TIFF mode: the offset of the second page's IFD (0 when none).
    pub next_page: u32,
    /// The original IFD tables (offset, length), for zeroing in TIFF mode.
    pub tables: Vec<(u32, u32)>,
}

fn type_size(typ: u16) -> Option<u64> {
    Some(match typ {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

struct Reader<'a> {
    data: &'a [u8],
    le: bool,
    visited: HashSet<u32>,
    tables: Vec<(u32, u32)>,
}

impl Reader<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        if self.le {
            imgutil::le16(self.data, at)
        } else {
            imgutil::be16(self.data, at)
        }
    }
    fn u32(&self, at: usize) -> Option<u32> {
        if self.le {
            imgutil::le32(self.data, at)
        } else {
            imgutil::be32(self.data, at)
        }
    }

    /// The entries of the IFD at `off` and the next-IFD offset.
    fn ifd(&mut self, off: u32) -> Option<(Vec<Entry>, u32)> {
        if off < 8 || !self.visited.insert(off) {
            return None;
        }
        let at = off as usize;
        let n = self.u16(at)? as usize;
        if n == 0 || n > 4000 {
            return None;
        }
        let table_end = at + 2 + 12 * n;
        if table_end > self.data.len() {
            return None;
        }
        self.tables.push((off, (2 + 12 * n + 4) as u32));
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let e = at + 2 + 12 * i;
            let tag = self.u16(e)?;
            let typ = self.u16(e + 2)?;
            let count = self.u32(e + 4)?;
            let Some(size) = type_size(typ) else { continue };
            let total = size * count as u64;
            if total > self.data.len() as u64 {
                continue;
            }
            let (data, src) = if total <= 4 {
                (self.data[e + 8..e + 8 + total as usize].to_vec(), None)
            } else {
                let vo = self.u32(e + 8)? as usize;
                match self.data.get(vo..vo + total as usize) {
                    Some(d) => (d.to_vec(), Some((vo as u32, total as u32))),
                    None => continue,
                }
            };
            out.push(Entry {
                tag,
                typ,
                count,
                data,
                src,
            });
        }
        let next = self.u32(table_end).unwrap_or(0);
        Some((out, next))
    }
}

/// Takes a pointer entry out of a list and returns its offset.
fn take_pointer(list: &mut Vec<Entry>, tag: u16, le: bool) -> Option<u32> {
    let i = list.iter().position(|e| e.tag == tag)?;
    let e = list.remove(i);
    read_uint(&e, 0, le)
}

fn read_uint(e: &Entry, i: usize, le: bool) -> Option<u32> {
    let d = &e.data;
    let r16 = |at| {
        if le {
            imgutil::le16(d, at)
        } else {
            imgutil::be16(d, at)
        }
    };
    let r32 = |at| {
        if le {
            imgutil::le32(d, at)
        } else {
            imgutil::be32(d, at)
        }
    };
    match e.typ {
        1 | 7 => d.get(i).map(|&b| b as u32),
        3 | 8 => r16(i * 2).map(u32::from),
        4 | 9 | 13 => r32(i * 4),
        _ => None,
    }
}

impl Exif {
    /// Parses an EXIF block (a TIFF structure; a leading `Exif\0\0` is skipped).
    pub fn parse(data: &[u8]) -> Option<Exif> {
        let data = data.strip_prefix(APP1_HEADER).unwrap_or(data);
        Exif::parse_inner(data, false)
    }

    /// Parses a whole TIFF file's first IFD (and its Exif / GPS IFDs).
    pub fn parse_tiff(data: &[u8]) -> Option<Exif> {
        Exif::parse_inner(data, true)
    }

    fn parse_inner(data: &[u8], tiff_mode: bool) -> Option<Exif> {
        let le = match data.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let mut r = Reader {
            data,
            le,
            visited: HashSet::new(),
            tables: Vec::new(),
        };
        if r.u16(2)? != 42 {
            return None;
        }
        let first = r.u32(4)?;
        let (mut ifd0, next) = r.ifd(first)?;
        let mut x = Exif {
            le,
            tiff_mode,
            ..Exif::default()
        };
        if let Some(off) = take_pointer(&mut ifd0, 0x8769, le) {
            if let Some((mut ex, _)) = r.ifd(off) {
                if let Some(io) = take_pointer(&mut ex, 0xA005, le) {
                    x.interop = r.ifd(io).map(|v| v.0).unwrap_or_default();
                }
                x.exif = ex;
            }
        }
        if let Some(off) = take_pointer(&mut ifd0, 0x8825, le) {
            x.gps = r.ifd(off).map(|v| v.0).unwrap_or_default();
        }
        if tiff_mode {
            x.next_page = next;
        } else {
            // Offsets to data outside the block cannot survive a rewrite.
            ifd0.retain(|e| e.tag != 0x014A);
            if next != 0 {
                if let Some((mut ifd1, _)) = r.ifd(next) {
                    let off = take_pointer(&mut ifd1, 0x0201, le);
                    let len = take_pointer(&mut ifd1, 0x0202, le);
                    if let (Some(o), Some(l)) = (off, len) {
                        x.thumbnail = data
                            .get(o as usize..(o as usize).saturating_add(l as usize))
                            .map(|t| t.to_vec());
                    }
                    x.ifd1 = ifd1;
                }
            }
        }
        x.ifd0 = ifd0;
        x.tables = r.tables;
        Some(x)
    }

    /// A new, empty block.
    pub fn new(le: bool) -> Exif {
        Exif {
            le,
            ..Exif::default()
        }
    }

    pub fn list(&self, kind: IfdKind) -> &Vec<Entry> {
        match kind {
            IfdKind::Ifd0 => &self.ifd0,
            IfdKind::Exif => &self.exif,
            IfdKind::Gps => &self.gps,
            IfdKind::Interop => &self.interop,
            IfdKind::Ifd1 => &self.ifd1,
        }
    }

    pub fn list_mut(&mut self, kind: IfdKind) -> &mut Vec<Entry> {
        match kind {
            IfdKind::Ifd0 => &mut self.ifd0,
            IfdKind::Exif => &mut self.exif,
            IfdKind::Gps => &mut self.gps,
            IfdKind::Interop => &mut self.interop,
            IfdKind::Ifd1 => &mut self.ifd1,
        }
    }

    pub fn find(&self, kind: IfdKind, tag: u16) -> Option<&Entry> {
        self.list(kind).iter().find(|e| e.tag == tag)
    }

    pub fn remove(&mut self, kind: IfdKind, tag: u16) {
        self.list_mut(kind).retain(|e| e.tag != tag);
    }

    /// Adds or replaces an entry.
    pub fn set(&mut self, kind: IfdKind, entry: Entry) {
        let list = self.list_mut(kind);
        match list.iter_mut().find(|e| e.tag == entry.tag) {
            Some(e) => *e = entry,
            None => list.push(entry),
        }
    }

    /// Nothing left to write.
    pub fn is_empty(&self) -> bool {
        self.ifd0.is_empty()
            && self.exif.is_empty()
            && self.gps.is_empty()
            && self.interop.is_empty()
            && self.thumbnail.is_none()
    }

    pub fn uint(&self, e: &Entry, i: usize) -> Option<u32> {
        read_uint(e, i, self.le)
    }

    fn u32_at(&self, d: &[u8], at: usize) -> Option<u32> {
        if self.le {
            imgutil::le32(d, at)
        } else {
            imgutil::be32(d, at)
        }
    }

    pub fn rational(&self, e: &Entry, i: usize) -> Option<f64> {
        let n = self.u32_at(&e.data, i * 8)?;
        let d = self.u32_at(&e.data, i * 8 + 4)?;
        match e.typ {
            5 if d != 0 => Some(n as f64 / d as f64),
            10 if d != 0 => Some(n as i32 as f64 / d as i32 as f64),
            _ => None,
        }
    }

    fn put16(&self, v: u16) -> [u8; 2] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    fn put32(&self, v: u32) -> [u8; 4] {
        if self.le {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }

    /// An ASCII entry (NUL-terminated).
    pub fn ascii(tag: u16, s: &str) -> Entry {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        Entry {
            tag,
            typ: 2,
            count: data.len() as u32,
            data,
            src: None,
        }
    }

    /// An entry of raw bytes (`typ` 1 BYTE or 7 UNDEFINED).
    pub fn bytes(tag: u16, typ: u16, data: Vec<u8>) -> Entry {
        Entry {
            tag,
            typ,
            count: data.len() as u32,
            data,
            src: None,
        }
    }

    pub fn short(&self, tag: u16, v: u16) -> Entry {
        Exif::bytes(tag, 3, self.put16(v).to_vec()).with_count(1)
    }

    pub fn long(&self, tag: u16, v: u32) -> Entry {
        Exif::bytes(tag, 4, self.put32(v).to_vec()).with_count(1)
    }

    pub fn rationals(&self, tag: u16, vals: &[(u32, u32)]) -> Entry {
        let mut data = Vec::new();
        for (n, d) in vals {
            data.extend(self.put32(*n));
            data.extend(self.put32(*d));
        }
        Exif::bytes(tag, 5, data).with_count(vals.len() as u32)
    }

    /// A Windows "XP" tag: UCS-2 little-endian, NUL-terminated, type BYTE.
    pub fn xp(tag: u16, s: &str) -> Entry {
        let mut data: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        data.extend([0, 0]);
        Exif::bytes(tag, 1, data)
    }

    /// UserComment with the ASCII or UNICODE character code.
    pub fn user_comment(&self, s: &str) -> Entry {
        let mut data = Vec::new();
        if s.is_ascii() {
            data.extend(b"ASCII\0\0\0");
            data.extend(s.as_bytes());
        } else {
            data.extend(b"UNICODE\0");
            for u in s.encode_utf16() {
                data.extend(self.put16(u));
            }
        }
        Exif::bytes(0x9286, 7, data)
    }

    // ----- writing -------------------------------------------------------

    /// The block as a TIFF blob (without the `Exif\0\0` prefix).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(if self.le { b"II*\0" } else { b"MM\0*" });
        out.extend([0; 4]);
        let at = self.emit(&mut out);
        let p = self.put32(at);
        out[4..8].copy_from_slice(&p);
        out
    }

    /// The block as a JPEG APP1 payload.
    pub fn to_app1(&self) -> Vec<u8> {
        let mut out = APP1_HEADER.to_vec();
        out.extend(self.to_bytes());
        out
    }

    fn patch(&self, out: &mut [u8], pos: usize, v: u32) {
        out[pos..pos + 4].copy_from_slice(&self.put32(v));
    }

    /// Writes one IFD at the end of `out` (word aligned); returns its offset,
    /// the value positions of the entries by tag, and the next-IFD position.
    fn write_ifd(&self, out: &mut Vec<u8>, entries: &[Entry]) -> (u32, Vec<(u16, usize)>, usize) {
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        sorted.sort_by_key(|e| e.tag);
        let start = out.len();
        out.extend(self.put16(sorted.len() as u16));
        let mut slots = Vec::new();
        for e in &sorted {
            out.extend(self.put16(e.tag));
            out.extend(self.put16(e.typ));
            out.extend(self.put32(e.count));
            slots.push((e.tag, out.len()));
            let mut v = [0u8; 4];
            if e.data.len() <= 4 {
                v[..e.data.len()].copy_from_slice(&e.data);
            }
            out.extend(v);
        }
        let next_pos = out.len();
        out.extend([0; 4]);
        for (e, (_, pos)) in sorted.iter().zip(&slots) {
            if e.data.len() > 4 {
                if out.len() % 2 == 1 {
                    out.push(0);
                }
                let at = out.len() as u32;
                out.extend(&e.data);
                self.patch(out, *pos, at);
            }
        }
        (start as u32, slots, next_pos)
    }

    /// Appends all IFDs to `out` (offsets are positions in `out`) and
    /// returns the offset of IFD0.
    pub fn emit(&self, out: &mut Vec<u8>) -> u32 {
        let slot = |slots: &[(u16, usize)], tag: u16| {
            slots.iter().find(|s| s.0 == tag).map(|s| s.1).unwrap_or(0)
        };
        let mut e0 = self.ifd0.clone();
        e0.retain(|e| e.tag != 0x8769 && e.tag != 0x8825);
        if !self.exif.is_empty() || !self.interop.is_empty() {
            e0.push(self.long(0x8769, 0));
        }
        if !self.gps.is_empty() {
            e0.push(self.long(0x8825, 0));
        }
        let (ifd0_at, s0, next0) = self.write_ifd(out, &e0);
        if !self.exif.is_empty() || !self.interop.is_empty() {
            let mut ex = self.exif.clone();
            ex.retain(|e| e.tag != 0xA005);
            if !self.interop.is_empty() {
                ex.push(self.long(0xA005, 0));
            }
            let (at, s1, _) = self.write_ifd(out, &ex);
            self.patch(out, slot(&s0, 0x8769), at);
            if !self.interop.is_empty() {
                let (io, _, _) = self.write_ifd(out, &self.interop);
                self.patch(out, slot(&s1, 0xA005), io);
            }
        }
        if !self.gps.is_empty() {
            let (at, _, _) = self.write_ifd(out, &self.gps);
            self.patch(out, slot(&s0, 0x8825), at);
        }
        if self.tiff_mode {
            self.patch(out, next0, self.next_page);
        } else if let Some(thumb) = &self.thumbnail {
            let mut e1 = self.ifd1.clone();
            e1.retain(|e| e.tag != 0x0201 && e.tag != 0x0202);
            if !e1.iter().any(|e| e.tag == 0x0103) {
                e1.push(self.short(0x0103, 6));
            }
            e1.push(self.long(0x0201, 0));
            e1.push(self.long(0x0202, thumb.len() as u32));
            let (at, s1, _) = self.write_ifd(out, &e1);
            self.patch(out, next0, at);
            if out.len() % 2 == 1 {
                out.push(0);
            }
            let t = out.len() as u32;
            out.extend(thumb);
            self.patch(out, slot(&s1, 0x0201), t);
        }
        ifd0_at
    }

    /// A block holding only the orientation tag, if this one has it.
    pub fn orientation_only(&self) -> Option<Exif> {
        let e = self.find(IfdKind::Ifd0, 0x0112)?.clone();
        let mut x = Exif::new(self.le);
        x.ifd0.push(e);
        Some(x)
    }

    pub fn orientation(&self) -> Option<u32> {
        self.find(IfdKind::Ifd0, 0x0112)
            .and_then(|e| self.uint(e, 0))
    }

    /// Image width and height from IFD0 (TIFF files).
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        let w = self
            .find(IfdKind::Ifd0, 0x0100)
            .and_then(|e| self.uint(e, 0))?;
        let h = self
            .find(IfdKind::Ifd0, 0x0101)
            .and_then(|e| self.uint(e, 0))?;
        Some((w, h))
    }

    // ----- cleaning ------------------------------------------------------

    /// Removes what `opts` removes. With "everything", only the tags needed
    /// to show the picture are kept (plus orientation / colour profile when
    /// asked).
    pub fn strip(&mut self, opts: &CleanOptions) {
        let every = opts.is_everything();
        let tiff = self.tiff_mode;
        let keep = |e: &Entry| -> bool {
            match e.tag {
                0x0112 => !drops_orientation(opts),
                0x8773 => !drops_icc(opts),
                0x927C => !(opts.removes(Category::Device) || opts.removes(Category::Hidden)),
                t if every => tiff && structural(t),
                t => match spec(t) {
                    Some(s) => !drops(opts, s.cat),
                    None => true,
                },
            }
        };
        self.ifd0.retain(keep);
        self.exif.retain(keep);
        if every {
            self.exif.clear();
            self.interop.clear();
        }
        // Blocks embedded in IFD0 (TIFF files).
        let mut i = 0;
        while i < self.ifd0.len() {
            let tag = self.ifd0[i].tag;
            let new = match tag {
                0x02BC => {
                    Some(xmp::strip(&text_of(&self.ifd0[i].data), opts).map(|x| x.into_bytes()))
                }
                0x83BB => Some(iptc::strip_iim(&self.ifd0[i].data, opts)),
                0x8649 => Some(iptc::strip_irb(&self.ifd0[i].data, opts)),
                _ => None,
            };
            match new {
                Some(None) => {
                    self.ifd0.remove(i);
                    continue;
                }
                Some(Some(d)) if d != self.ifd0[i].data => {
                    let typ = if tag == 0x83BB { 7 } else { self.ifd0[i].typ };
                    self.ifd0[i] = Exif::bytes(tag, typ, d);
                }
                _ => {}
            }
            i += 1;
        }
        if drops(opts, Category::Location) {
            self.gps.clear();
        }
        if drops(opts, Category::Hidden) {
            self.thumbnail = None;
            self.ifd1.clear();
        }
        if self.thumbnail.is_none() {
            self.ifd1.clear();
        }
    }

    // ----- editing -------------------------------------------------------

    /// Applies edits to the EXIF tags. ASCII tags are written only when
    /// the value is ASCII; the Windows XP tags and UserComment carry Unicode.
    pub fn apply_edits(&mut self, edits: &[Edit]) -> Result<()> {
        let (i0, ex) = (IfdKind::Ifd0, IfdKind::Exif);
        for ed in edits {
            let v = ed.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
            match ed.key {
                EditKey::Title => {
                    self.remove(i0, 0x9C9B);
                    self.remove(ex, 0xA436);
                    if let Some(v) = v {
                        self.set(i0, Exif::xp(0x9C9B, v));
                    }
                }
                EditKey::Author => {
                    self.remove(i0, 0x013B);
                    self.remove(i0, 0x9C9D);
                    self.remove(ex, 0xA437);
                    if let Some(v) = v {
                        if v.is_ascii() {
                            self.set(i0, Exif::ascii(0x013B, v));
                        }
                        self.set(i0, Exif::xp(0x9C9D, v));
                    }
                }
                EditKey::Copyright => {
                    self.remove(i0, 0x8298);
                    if let Some(v) = v.filter(|v| v.is_ascii()) {
                        self.set(i0, Exif::ascii(0x8298, v));
                    }
                }
                EditKey::Description => {
                    self.remove(i0, 0x010E);
                    self.remove(i0, 0x9C9C);
                    self.remove(ex, 0x9286);
                    if let Some(v) = v {
                        if v.is_ascii() {
                            self.set(i0, Exif::ascii(0x010E, v));
                        }
                        self.set(i0, Exif::xp(0x9C9C, v));
                        let uc = self.user_comment(v);
                        self.set(ex, uc);
                    }
                }
                EditKey::Keywords => {
                    self.remove(i0, 0x9C9E);
                    if let Some(v) = v {
                        self.set(i0, Exif::xp(0x9C9E, &imgutil::keywords(v).join(";")));
                    }
                }
                EditKey::DateTaken => {
                    self.remove(ex, 0x9003);
                    self.remove(ex, 0x9291);
                    if let Some(v) = v {
                        let d = DateTime::parse(v).ok_or_else(|| bad_date(v))?;
                        self.set(ex, Exif::ascii(0x9003, &d.exif()));
                    }
                }
                EditKey::Gps => {
                    self.gps.clear();
                    if let Some(v) = v {
                        let g = GeoPoint::parse(v).ok_or_else(|| bad_gps(v))?;
                        self.set_gps(g);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn set_gps(&mut self, g: GeoPoint) {
        let dms = |v: f64| -> Vec<(u32, u32)> {
            let v = v.abs();
            let d = v.floor();
            let m = ((v - d) * 60.0).floor();
            let s = ((v - d) * 60.0 - m) * 60.0;
            vec![
                (d as u32, 1),
                (m as u32, 1),
                ((s * 10000.0).round() as u32, 10000),
            ]
        };
        self.gps = vec![
            Exif::bytes(0x0000, 1, vec![2, 3, 0, 0]),
            Exif::ascii(0x0001, if g.lat < 0.0 { "S" } else { "N" }),
            self.rationals(0x0002, &dms(g.lat)),
            Exif::ascii(0x0003, if g.lon < 0.0 { "W" } else { "E" }),
            self.rationals(0x0004, &dms(g.lon)),
        ];
    }

    // ----- reading -------------------------------------------------------

    /// The GPS position in decimal degrees.
    pub fn gps_point(&self) -> Option<GeoPoint> {
        let coord = |val: u16, rf: u16, neg: u8| -> Option<f64> {
            let e = self.find(IfdKind::Gps, val)?;
            let d = self.rational(e, 0)?;
            let m = self.rational(e, 1).unwrap_or(0.0);
            let s = self.rational(e, 2).unwrap_or(0.0);
            let mut v = d + m / 60.0 + s / 3600.0;
            if self.find(IfdKind::Gps, rf).and_then(|r| r.data.first()) == Some(&neg) {
                v = -v;
            }
            v.is_finite().then_some(v)
        };
        let lat = coord(0x0002, 0x0001, b'S')?;
        let lon = coord(0x0004, 0x0003, b'W')?;
        ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon))
            .then_some(GeoPoint { lat, lon })
    }

    fn ucs2(&self, d: &[u8], le: bool) -> String {
        let units: Vec<u16> = d
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            })
            .collect();
        let s = String::from_utf16_lossy(&units);
        s.trim_end_matches('\0').trim().to_string()
    }

    /// Text of a UserComment-style value (8-byte character code prefix).
    pub fn comment_text(&self, d: &[u8]) -> String {
        if d.len() < 8 {
            return text_of(d).trim().to_string();
        }
        let (head, body) = d.split_at(8);
        if head == b"UNICODE\0" {
            // Byte order: trust where the zero bytes of ASCII text fall.
            let zeros = |odd: usize| {
                body.iter()
                    .skip(odd)
                    .step_by(2)
                    .filter(|&&b| b == 0)
                    .count()
            };
            let le = match zeros(1).cmp(&zeros(0)) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => self.le,
            };
            return self.ucs2(body, le);
        }
        text_of(body).trim_matches([' ', '\0']).to_string()
    }

    /// A value as plain text.
    pub fn text(&self, e: &Entry) -> String {
        let n = e.count.min(16) as usize;
        let more = if e.count as usize > n { ", ..." } else { "" };
        match e.typ {
            2 => text_of(&e.data).trim().to_string(),
            1 | 6 | 7 => {
                let t = trim_nul(&e.data);
                if !t.is_empty() && t.iter().all(|&b| (0x20..0x7F).contains(&b)) {
                    String::from_utf8_lossy(t).trim().to_string()
                } else {
                    format!("{} bytes", e.data.len())
                }
            }
            3 | 4 | 8 | 9 => {
                let v: Vec<String> = (0..n)
                    .filter_map(|i| self.uint(e, i))
                    .map(|x| match e.typ {
                        8 => (x as u16 as i16).to_string(),
                        9 => (x as i32).to_string(),
                        _ => x.to_string(),
                    })
                    .collect();
                v.join(", ") + more
            }
            5 | 10 => {
                let v: Vec<String> = (0..n)
                    .filter_map(|i| self.rational(e, i))
                    .map(num)
                    .collect();
                v.join(", ") + more
            }
            _ => format!("{} bytes", e.data.len()),
        }
    }

    /// The fields of this block, in plain language.
    pub fn fields(&self) -> Vec<Field> {
        let mut out = Vec::new();
        for e in self.ifd0.iter().chain(&self.exif) {
            match e.tag {
                0x02BC => out.extend(xmp::fields(&text_of(&e.data), "XMP")),
                0x83BB => out.extend(iptc::iim_fields(&e.data)),
                0x8649 => out.extend(iptc::irb_fields(&e.data)),
                0x8773 => out.extend(icc::fields(&e.data)),
                0x0112 => {
                    let v = self.uint(e, 0).unwrap_or(0);
                    out.push(Field::new(
                        "exif:0x0112",
                        "Orientation",
                        orientation_text(v),
                        Category::Technical,
                        SOURCE,
                    ));
                }
                tag => {
                    if let Some(s) = spec(tag) {
                        let value = self.value_of(e);
                        if value.is_empty() {
                            continue;
                        }
                        let mut f =
                            Field::new(format!("exif:0x{tag:04X}"), s.label, value, s.cat, SOURCE);
                        f.flag = s.flag;
                        f.edit = s.edit;
                        out.push(f);
                    }
                }
            }
        }
        self.gps_fields(&mut out);
        if let Some(t) = &self.thumbnail {
            let what = match jpeg::dimensions(t) {
                Some((w, h)) => format!("JPEG picture, {w} x {h}"),
                None => format!("{} bytes", t.len()),
            };
            out.push(
                Field::new(
                    "exif:thumbnail",
                    "Embedded thumbnail",
                    what,
                    Category::Hidden,
                    SOURCE,
                )
                .flag(Flag::Hidden),
            );
        }
        out
    }

    fn value_of(&self, e: &Entry) -> String {
        let r = |i| self.rational(e, i);
        match e.tag {
            0x9C9B..=0x9C9F => {
                let s = self.ucs2(&e.data, true);
                if e.tag == 0x9C9E {
                    imgutil::keywords(&s).join("; ")
                } else {
                    s
                }
            }
            0x9286 => self.comment_text(&e.data),
            0x927C => format!(
                "{} bytes of maker data (may contain the camera serial number)",
                e.data.len()
            ),
            0xC4A5 => format!("{} bytes", e.data.len()),
            0x0132 | 0x9003 | 0x9004 => nice_date(&self.text(e)),
            0x829A => match r(0) {
                Some(t) if t > 0.0 && t < 1.0 => format!("1/{} s", (1.0 / t).round()),
                Some(t) => format!("{} s", num(t)),
                None => String::new(),
            },
            0x829D => r(0).map(|f| format!("f/{}", num(f))).unwrap_or_default(),
            0x920A => r(0).map(|f| format!("{} mm", num(f))).unwrap_or_default(),
            0x8827 => self
                .uint(e, 0)
                .map(|i| format!("ISO {i}"))
                .unwrap_or_default(),
            0x9209 => match self.uint(e, 0) {
                Some(v) if v & 1 == 1 => "Flash fired".into(),
                Some(_) => "No flash".into(),
                None => String::new(),
            },
            0xA432 => {
                let v: Vec<f64> = (0..4).filter_map(r).collect();
                match v[..] {
                    [a, b, c, d] => format!("{}-{} mm, f/{}-{}", num(a), num(b), num(c), num(d)),
                    _ => self.text(e),
                }
            }
            _ => self.text(e),
        }
    }

    fn gps_fields(&self, out: &mut Vec<Field>) {
        if self.gps.is_empty() {
            return;
        }
        let point = self.gps_point();
        let field = |id: String, label: &str, value: String| {
            Field::new(id, label, value, Category::Location, SOURCE).flag(Flag::Location)
        };
        if let Some(p) = point {
            out.push(field("exif:gps".into(), "GPS position", p.display()).edit(EditKey::Gps));
        }
        let refch = |tag: u16| -> Option<u8> {
            self.find(IfdKind::Gps, tag)
                .and_then(|e| e.data.first().copied())
        };
        for e in &self.gps {
            let r = |i| self.rational(e, i);
            let (label, value) = match e.tag {
                0x0000 | 0x0001 | 0x0003 | 0x0005 | 0x000C | 0x000E | 0x0010 | 0x0013 | 0x0015
                | 0x0017 | 0x0019 => continue,
                0x0002 | 0x0004 if point.is_some() => continue,
                0x0002 => ("GPS latitude", self.text(e)),
                0x0004 => ("GPS longitude", self.text(e)),
                0x0006 => {
                    let Some(a) = r(0) else { continue };
                    let below = refch(0x0005) == Some(1);
                    let side = if below { "below" } else { "above" };
                    ("Altitude", format!("{} m {side} sea level", num1(a)))
                }
                0x0007 => {
                    let v: Vec<f64> = (0..3).filter_map(r).collect();
                    match v[..] {
                        [h, m, s] => (
                            "GPS time (UTC)",
                            format!("{:02}:{:02}:{:02}", h as u32, m as u32, s as u32),
                        ),
                        _ => continue,
                    }
                }
                0x0008 => ("GPS satellites", self.text(e)),
                0x000B => ("GPS precision (DOP)", self.text(e)),
                0x000D => {
                    let unit = match refch(0x000C) {
                        Some(b'M') => "mph",
                        Some(b'N') => "knots",
                        _ => "km/h",
                    };
                    (
                        "Speed",
                        r(0).map(|s| format!("{} {unit}", num1(s)))
                            .unwrap_or_default(),
                    )
                }
                0x000F => (
                    "Direction of movement",
                    r(0).map(|d| format!("{}°", num1(d))).unwrap_or_default(),
                ),
                0x0011 => {
                    let n = if refch(0x0010) == Some(b'M') {
                        "magnetic"
                    } else {
                        "true"
                    };
                    (
                        "Direction the camera faced",
                        r(0).map(|d| format!("{}° ({n} north)", num1(d)))
                            .unwrap_or_default(),
                    )
                }
                0x0012 => ("Map datum", self.text(e)),
                0x0014 | 0x0016 => ("Destination position", self.text(e)),
                0x0018 => ("Destination bearing", self.text(e)),
                0x001A => ("Destination distance", self.text(e)),
                0x001B => ("GPS method", self.comment_text(&e.data)),
                0x001C => ("GPS area", self.comment_text(&e.data)),
                0x001D => ("GPS date", nice_date(&self.text(e))),
                0x001F => (
                    "GPS accuracy",
                    r(0).map(|m| format!("{} m", num1(m))).unwrap_or_default(),
                ),
                _ => ("GPS data", self.text(e)),
            };
            if !value.is_empty() {
                out.push(field(format!("exif:gps:0x{:04X}", e.tag), label, value));
            }
        }
    }

    /// The original byte ranges that a rewrite to `new` leaves unused
    /// (removed values and the old IFD tables), for zeroing in TIFF files.
    pub fn freed_ranges(&self, new: &Exif) -> Vec<(u32, u32)> {
        let kept: HashSet<(u32, u32)> = [&new.ifd0, &new.exif, &new.gps, &new.interop]
            .into_iter()
            .flatten()
            .filter_map(|e| e.src)
            .collect();
        let mut out: Vec<(u32, u32)> = [&self.ifd0, &self.exif, &self.gps, &self.interop]
            .into_iter()
            .flatten()
            .filter_map(|e| e.src)
            .filter(|r| !kept.contains(r))
            .collect();
        out.extend(&self.tables);
        out
    }

    /// Ranges new keeps in use (values of kept entries), never to be zeroed.
    pub fn used_ranges(&self) -> Vec<(u32, u32)> {
        [&self.ifd0, &self.exif, &self.gps, &self.interop]
            .into_iter()
            .flatten()
            .filter_map(|e| e.src)
            .collect()
    }
}

impl Entry {
    fn with_count(mut self, count: u32) -> Entry {
        self.count = count;
        self
    }
}

fn bad_date(v: &str) -> String {
    format!("\"{v}\" is not a date. Use YYYY-MM-DD HH:MM:SS.")
}

fn bad_gps(v: &str) -> String {
    format!("\"{v}\" is not a GPS position. Use latitude, longitude, e.g. 36.1901, 44.0091.")
}

/// Numbers without needless decimals.
fn num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn num1(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

pub fn orientation_text(v: u32) -> String {
    match v {
        1 => "Normal".into(),
        2 => "Mirrored horizontally".into(),
        3 => "Rotate 180°".into(),
        4 => "Mirrored vertically".into(),
        5 => "Mirrored horizontally, then rotated 270° clockwise".into(),
        6 => "Rotate 90° clockwise".into(),
        7 => "Mirrored horizontally, then rotated 90° clockwise".into(),
        8 => "Rotate 270° clockwise".into(),
        _ => format!("Unknown ({v})"),
    }
}

/// How a tag is shown and which cleaning choice removes it.
pub struct Spec {
    pub label: &'static str,
    pub cat: Category,
    pub flag: Option<Flag>,
    pub edit: Option<EditKey>,
}

type Row = (u16, &'static str, Category, Option<Flag>, Option<EditKey>);

const TAGS: &[Row] = {
    use Category::*;
    use EditKey as K;
    &[
        (0x000B, "Processing software", Software, None, None),
        (0x010D, "Document name", Description, None, None),
        (
            0x010E,
            "Description",
            Description,
            Some(Flag::Comments),
            Some(K::Description),
        ),
        (0x010F, "Camera make", Device, None, None),
        (0x0110, "Camera model", Device, None, None),
        (0x0131, "Program used", Software, None, None),
        (0x0132, "Date modified", Dates, None, None),
        (
            0x013B,
            "Photographer",
            People,
            Some(Flag::Names),
            Some(K::Author),
        ),
        (0x013C, "Computer name", Device, Some(Flag::Names), None),
        (0x4746, "Rating", Description, None, None),
        (
            0x8298,
            "Copyright",
            People,
            Some(Flag::Names),
            Some(K::Copyright),
        ),
        (0x829A, "Exposure time", Device, None, None),
        (0x829D, "Aperture", Device, None, None),
        (0x8827, "ISO speed", Device, None, None),
        (0x9003, "Date taken", Dates, None, Some(K::DateTaken)),
        (0x9004, "Date digitized", Dates, None, None),
        (0x9010, "Time zone (modified)", Dates, None, None),
        (0x9011, "Time zone (taken)", Dates, None, None),
        (0x9012, "Time zone (digitized)", Dates, None, None),
        (0x9209, "Flash", Device, None, None),
        (0x920A, "Focal length", Device, None, None),
        (0x927C, "Maker note", Hidden, Some(Flag::Serial), None),
        (
            0x9286,
            "User comment",
            Description,
            Some(Flag::Comments),
            Some(K::Description),
        ),
        (0x9290, "Fraction of second (modified)", Dates, None, None),
        (0x9291, "Fraction of second (taken)", Dates, None, None),
        (0x9292, "Fraction of second (digitized)", Dates, None, None),
        (0x9C9B, "Title (Windows)", Description, None, Some(K::Title)),
        (
            0x9C9C,
            "Comments (Windows)",
            Description,
            Some(Flag::Comments),
            Some(K::Description),
        ),
        (
            0x9C9D,
            "Authors (Windows)",
            People,
            Some(Flag::Names),
            Some(K::Author),
        ),
        (
            0x9C9E,
            "Tags (Windows)",
            Description,
            None,
            Some(K::Keywords),
        ),
        (
            0x9C9F,
            "Subject (Windows)",
            Description,
            Some(Flag::Comments),
            None,
        ),
        (0xA420, "Unique image ID", Hidden, Some(Flag::Serial), None),
        (0xA430, "Camera owner", People, Some(Flag::Names), None),
        (
            0xA431,
            "Camera serial number",
            Device,
            Some(Flag::Serial),
            None,
        ),
        (0xA432, "Lens specification", Device, None, None),
        (0xA433, "Lens make", Device, None, None),
        (0xA434, "Lens model", Device, None, None),
        (
            0xA435,
            "Lens serial number",
            Device,
            Some(Flag::Serial),
            None,
        ),
        (0xA436, "Title", Description, None, Some(K::Title)),
        (
            0xA437,
            "Photographer",
            People,
            Some(Flag::Names),
            Some(K::Author),
        ),
        (0xA438, "Image editor", People, Some(Flag::Names), None),
        (0xA439, "Camera firmware", Device, None, None),
        (0xA43A, "Raw developing program", Software, None, None),
        (0xA43B, "Editing program", Software, None, None),
        (0xA43C, "Metadata editing program", Software, None, None),
        (0xC4A5, "Print settings (PrintIM)", Hidden, None, None),
    ]
};

/// The label and category of an IFD0 / Exif IFD tag shown to the user.
pub fn spec(tag: u16) -> Option<Spec> {
    TAGS.iter().find(|r| r.0 == tag).map(|r| Spec {
        label: r.1,
        cat: r.2,
        flag: r.3,
        edit: r.4,
    })
}

/// Tags a TIFF picture needs to be shown (never removed from TIFF files).
pub fn structural(tag: u16) -> bool {
    matches!(
        tag,
        0x00FE..=0x0103
            | 0x0106..=0x0108
            | 0x010A
            | 0x0111
            | 0x0115..=0x011C
            | 0x0122..=0x0125
            | 0x0128
            | 0x0129
            | 0x012D
            | 0x013D..=0x0147
            | 0x014A
            | 0x014C
            | 0x0150..=0x0153
            | 0x015B
            | 0x0200..=0x0202
            | 0x0211..=0x0214
            | 0x828D
            | 0x828E
    )
}

/// The fields of a raw EXIF block (with or without the `Exif\0\0` prefix).
pub fn fields_of(data: &[u8]) -> Vec<Field> {
    Exif::parse(data).map(|x| x.fields()).unwrap_or_default()
}

/// The cleaned block, `None` when nothing is left.
pub fn strip_block(data: &[u8], opts: &CleanOptions) -> Option<Vec<u8>> {
    let mut x = Exif::parse(data)?;
    if opts.is_everything() {
        let o = x.orientation_only().filter(|_| !drops_orientation(opts))?;
        return Some(o.to_bytes());
    }
    x.strip(opts);
    (!x.is_empty()).then(|| x.to_bytes())
}

/// The invented EXIF of the sample files.
pub fn sample(thumbnail: Option<Vec<u8>>) -> Exif {
    use super::imgutil::sample_values as s;
    let mut x = Exif::new(true);
    let taken = DateTime::parse(s::TAKEN).unwrap().exif();
    x.ifd0 = vec![
        Exif::ascii(0x010E, s::DESCRIPTION),
        Exif::ascii(0x010F, s::MAKE),
        Exif::ascii(0x0110, s::MODEL),
        x.short(0x0112, 6),
        Exif::ascii(0x0131, s::SOFTWARE),
        Exif::ascii(0x0132, &taken),
        Exif::ascii(0x013B, s::ARTIST),
        Exif::ascii(0x8298, s::COPYRIGHT),
    ];
    x.exif = vec![
        x.rationals(0x829A, &[(1, 250)]),
        x.rationals(0x829D, &[(28, 10)]),
        x.short(0x8827, 100),
        Exif::ascii(0x9003, &taken),
        Exif::ascii(0x9004, &taken),
        x.rationals(0x920A, &[(42, 10)]),
        Exif::bytes(0x927C, 7, b"ExampleCam maker data SN-0042-TEST".to_vec()),
        x.user_comment("Test comment"),
        Exif::ascii(0xA420, "0123456789abcdef0123456789abcdef"),
        Exif::ascii(0xA431, s::SERIAL),
        Exif::ascii(0xA434, "EC Lens 4mm"),
    ];
    x.interop = vec![Exif::ascii(0x0001, "R98")];
    x.gps = vec![
        Exif::bytes(0x0000, 1, vec![2, 3, 0, 0]),
        Exif::ascii(0x0001, "N"),
        x.rationals(0x0002, &[(36, 1), (11, 1), (2436, 100)]),
        Exif::ascii(0x0003, "E"),
        x.rationals(0x0004, &[(44, 1), (0, 1), (3276, 100)]),
        Exif::bytes(0x0005, 1, vec![0]),
        x.rationals(0x0006, &[(412, 1)]),
        x.rationals(0x0007, &[(7, 1), (20, 1), (30, 1)]),
        Exif::ascii(0x001D, "2024:05:01"),
    ];
    x.thumbnail = thumbnail;
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;

    fn find<'a>(f: &'a [Field], id: &str) -> Option<&'a Field> {
        f.iter().find(|x| x.id == id)
    }

    #[test]
    fn round_trip_both_byte_orders() {
        let x = sample(Some(vec![0xFF, 0xD8, 0xFF, 0xD9]));
        for le in [true, false] {
            let mut y = x.clone();
            if !le {
                // Re-encode the sample in big-endian through its own API.
                y = Exif::new(false);
                y.ifd0 = vec![Exif::ascii(0x0110, s::MODEL), y.short(0x0112, 6)];
                y.gps = vec![
                    Exif::ascii(0x0001, "S"),
                    y.rationals(0x0002, &[(36, 1), (11, 1), (2436, 100)]),
                    Exif::ascii(0x0003, "W"),
                    y.rationals(0x0004, &[(44, 1), (0, 1), (3276, 100)]),
                ];
            }
            let bytes = y.to_bytes();
            let mut z = Exif::parse(&bytes).unwrap();
            z.tables.clear();
            for l in [
                &mut z.ifd0,
                &mut z.exif,
                &mut z.gps,
                &mut z.interop,
                &mut z.ifd1,
            ] {
                for e in l.iter_mut() {
                    e.src = None;
                }
            }
            assert_eq!(z.ifd0.len(), y.ifd0.len());
            assert_eq!(z.gps, y.gps);
            assert_eq!(z.thumbnail, y.thumbnail);
            assert_eq!(z.orientation(), Some(6));
            let g = z.gps_point().unwrap();
            let sign = if le { 1.0 } else { -1.0 };
            assert!((g.lat - sign * s::LAT).abs() < 1e-6 && (g.lon - sign * s::LON).abs() < 1e-6);
        }
    }

    #[test]
    fn fields_are_labelled() {
        let f = sample(Some(vec![1, 2, 3])).fields();
        assert_eq!(find(&f, "exif:0x0110").unwrap().value, "EC-100");
        assert_eq!(find(&f, "exif:0x0110").unwrap().category, Category::Device);
        assert_eq!(find(&f, "exif:0xA431").unwrap().flag, Some(Flag::Serial));
        assert_eq!(find(&f, "exif:0x013B").unwrap().flag, Some(Flag::Names));
        assert_eq!(find(&f, "exif:0x9003").unwrap().value, s::TAKEN);
        assert_eq!(
            find(&f, "exif:0x0112").unwrap().value,
            "Rotate 90° clockwise"
        );
        assert_eq!(find(&f, "exif:0x829A").unwrap().value, "1/250 s");
        assert_eq!(find(&f, "exif:0x829D").unwrap().value, "f/2.8");
        assert_eq!(find(&f, "exif:0x9286").unwrap().value, "Test comment");
        assert_eq!(find(&f, "exif:gps").unwrap().value, "36.190100, 44.009100");
        assert_eq!(
            find(&f, "exif:gps:0x0006").unwrap().value,
            "412 m above sea level"
        );
        assert_eq!(find(&f, "exif:thumbnail").unwrap().flag, Some(Flag::Hidden));
        assert!(find(&f, "exif:0x927C").unwrap().value.contains("serial"));
    }

    #[test]
    fn unicode_edits() {
        let mut x = sample(None);
        x.apply_edits(&[
            Edit::set(EditKey::Author, s::TITLE_KU),
            Edit::set(EditKey::Description, "تێبینی"),
            Edit::set(EditKey::Copyright, "© ستودیۆ"),
        ])
        .unwrap();
        let y = Exif::parse(&x.to_bytes()).unwrap();
        let f = y.fields();
        assert!(find(&f, "exif:0x013B").is_none(), "ASCII Artist must go");
        assert_eq!(find(&f, "exif:0x9C9D").unwrap().value, s::TITLE_KU);
        assert_eq!(find(&f, "exif:0x9286").unwrap().value, "تێبینی");
        assert!(find(&f, "exif:0x8298").is_none());
        assert!(x.apply_edits(&[Edit::set(EditKey::Gps, "north")]).is_err());
        assert!(x
            .apply_edits(&[Edit::set(EditKey::DateTaken, "yesterday")])
            .is_err());
    }

    #[test]
    fn strip_partial_and_everything() {
        let x = sample(Some(vec![1, 2, 3]));
        let mut y = x.clone();
        y.strip(&CleanOptions::only(&[Category::Location, Category::People]));
        let f = y.fields();
        assert!(y.gps.is_empty());
        assert!(find(&f, "exif:0x013B").is_none() && find(&f, "exif:0x8298").is_none());
        assert!(find(&f, "exif:0x0110").is_some() && find(&f, "exif:0x9003").is_some());
        assert!(y.thumbnail.is_some());

        let blob = x.to_bytes();
        let kept = strip_block(&blob, &CleanOptions::everything()).unwrap();
        let k = Exif::parse(&kept).unwrap();
        assert_eq!(k.fields().len(), 1);
        assert_eq!(k.orientation(), Some(6));
        let mut all = CleanOptions::everything();
        all.keep_orientation = false;
        assert!(strip_block(&blob, &all).is_none());
    }

    #[test]
    fn garbage_never_panics() {
        let blob = sample(Some(vec![9; 40])).to_bytes();
        for cut in 0..blob.len() {
            if let Some(x) = Exif::parse(&blob[..cut]) {
                let _ = x.fields();
            }
        }
        // A loop of IFDs pointing at each other.
        let mut b = b"II*\0\x08\0\0\0\x01\0\x69\x87\x04\0\x01\0\0\0\x08\0\0\0\x08\0\0\0".to_vec();
        b.extend([0; 8]);
        let x = Exif::parse(&b).unwrap();
        let _ = x.fields();
    }
}
