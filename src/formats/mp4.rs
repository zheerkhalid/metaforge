//! MP4 and QuickTime video (also used for MP4 audio): GPS position, phone
//! make and model, software, dates and titles stored in the box tree.
//!
//! Reading walks the box headers and seeks over `mdat`, so large videos are
//! never loaded into memory. Cleaning copies the file and patches the copy in
//! place: removed boxes become `free` boxes of the same size with a zeroed
//! payload and timestamps are zeroed, so the file size and every chunk offset
//! (`stco` / `co64`) stay valid. Editing writes the iTunes-style `ilst` with
//! lofty (which fixes the chunk offsets) and then frees older copies of the
//! edited values stored elsewhere (QuickTime `udta` or `mdta` items).

use crate::meta::{
    Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, GeoPoint, Kind, Result,
};
use crate::util::DateTime;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The `uuid` box type that holds an XMP packet.
const XMP_UUID: [u8; 16] = [
    0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF, 0xAC,
];
/// Seconds from 1904-01-01 (the MP4 epoch) to 1970-01-01.
const MAC_EPOCH: i64 = 2_082_844_800;
/// The largest metadata payload read into memory (pictures included).
const MAX_PAYLOAD: u64 = 64 << 20;
const SRC_QT: &str = "QuickTime metadata";
const SRC_ILST: &str = "MP4 tags";

fn io_err(e: std::io::Error) -> String {
    format!("The file could not be read: {e}")
}

fn damaged() -> String {
    "The file is damaged or incomplete: its MP4 box structure is broken.".into()
}

/// One box: where it starts, its header length and its total size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bx {
    kind: [u8; 4],
    start: u64,
    header: u64,
    size: u64,
}

impl Bx {
    fn body(self) -> u64 {
        self.start + self.header
    }
    fn end(self) -> u64 {
        self.start + self.size
    }
    fn body_len(self) -> u64 {
        self.size - self.header
    }
}

/// The boxes between `from` and `to`.
fn children(f: &mut File, from: u64, to: u64) -> Result<Vec<Bx>> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos + 8 <= to {
        let mut h = [0u8; 8];
        f.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        f.read_exact(&mut h).map_err(|_| damaged())?;
        let kind = [h[4], h[5], h[6], h[7]];
        let mut size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let mut header = 8;
        if size == 1 {
            let mut l = [0u8; 8];
            f.read_exact(&mut l).map_err(|_| damaged())?;
            size = u64::from_be_bytes(l);
            header = 16;
        } else if size == 0 {
            size = to - pos;
        }
        // QuickTime allows a 32-bit zero terminator at the end of a list.
        if size < header || pos.checked_add(size).is_none_or(|e| e > to) {
            if u32::from_be_bytes([h[0], h[1], h[2], h[3]]) == 0 && to - pos < 8 {
                break;
            }
            return Err(damaged());
        }
        out.push(Bx {
            kind,
            start: pos,
            header,
            size,
        });
        pos += size;
        if out.len() > 200_000 {
            return Err(damaged());
        }
    }
    Ok(out)
}

fn read_at(f: &mut File, off: u64, len: u64) -> Result<Vec<u8>> {
    if len > MAX_PAYLOAD {
        return Err("A metadata block in this file is unreasonably large.".into());
    }
    let mut buf = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(off)).map_err(io_err)?;
    f.read_exact(&mut buf).map_err(|_| damaged())?;
    Ok(buf)
}

fn body(f: &mut File, b: Bx) -> Result<Vec<u8>> {
    read_at(f, b.body(), b.body_len())
}

fn be16(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .unwrap_or(0)
}

fn be32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .unwrap_or(0)
}

fn be64(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes(s.try_into().unwrap_or([0; 8])))
        .unwrap_or(0)
}

/// A box type as text, with `0xA9` shown as `©`.
fn fourcc(k: [u8; 4]) -> String {
    k.iter()
        .map(|&c| match c {
            0xA9 => '©',
            0x20..=0x7E => c as char,
            _ => '?',
        })
        .collect()
}

/// How a removed item is taken out of the file.
#[derive(Clone, Debug)]
enum Patch {
    /// The box becomes a `free` box of the same size.
    Free(Bx),
    /// These byte ranges are zeroed (timestamps).
    Zeros(Vec<(u64, u64)>),
    /// An XMP packet: stripped in place, or freed.
    Xmp { bx: Bx, payload: u64 },
}

struct Item {
    field: Field,
    patch: Patch,
}

/// Timestamp slots: (offset, width in bytes, value).
type Slots = Vec<(u64, u64, u64)>;

#[derive(Default)]
struct Track {
    handler: [u8; 4],
    dims: Option<(u32, u32)>,
    codec: Option<[u8; 4]>,
    channels: u16,
    sample_rate: u32,
}

#[derive(Default)]
struct Scan {
    items: Vec<Item>,
    /// udta / meta / XMP boxes that "remove everything" frees whole.
    blocks: Vec<Bx>,
    created: Slots,
    modified: Slots,
    gps: Option<GeoPoint>,
    brand: Option<[u8; 4]>,
    duration: Option<f64>,
    tracks: Vec<Track>,
    has_moov: bool,
}

impl Scan {
    fn push(&mut self, id: String, key: &str, value: String, source: &str, patch: Patch) {
        let value = value.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if value.is_empty() {
            return;
        }
        let c = classify(key);
        let mut value = value.to_string();
        if c.category == Category::Location && self.gps.is_none() {
            self.gps = parse_iso6709(&value);
        }
        if c.category == Category::Dates {
            if let Some(dt) = DateTime::parse(&value).filter(|_| value.len() > 10) {
                value = dt.text();
            }
        }
        let mut field = Field::new(id, c.label, value, c.category, source);
        field.flag = c.flag;
        field.edit = c.edit;
        self.items.push(Item { field, patch });
    }
}

struct Class {
    label: String,
    category: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
}

/// What a metadata key (a udta / ilst four-character code or an `mdta` key
/// name) means.
fn classify(key: &str) -> Class {
    use Category::*;
    let k = key.to_ascii_lowercase();
    let last = k.rsplit('.').next().unwrap_or(&k).to_string();
    let c = |label: &str, category, flag, edit| Class {
        label: label.to_string(),
        category,
        flag,
        edit,
    };
    let is = |names: &[&str]| names.contains(&key) || names.contains(&last.as_str());
    if is(&["©xyz", "loci", "iso6709"]) || k.contains("location") || k.contains("gps") {
        return c("Recorded at (GPS)", Location, Some(Flag::Location), None);
    }
    if k.contains("serial") || k.contains("identifier") || k.contains("uuid") {
        return c(
            &format!("Unique id ({key})"),
            Hidden,
            Some(Flag::Serial),
            None,
        );
    }
    if is(&["©mak", "make", "manufacturer"]) {
        return c("Phone make", Device, None, None);
    }
    if is(&["©mod", "model"]) || k.contains("lens") {
        let label = if k.contains("lens") {
            "Lens"
        } else {
            "Phone model"
        };
        return c(label, Device, None, None);
    }
    if is(&["©swr", "©too", "©enc", "software", "version", "encoder"]) {
        return c("Program used", Software, None, None);
    }
    if is(&["©day", "creationdate", "yrrc", "date"]) {
        return c("Recorded on", Dates, None, Some(EditKey::DateTaken));
    }
    if is(&["©nam", "titl", "title", "displayname"]) {
        return c("Title", Description, None, Some(EditKey::Title));
    }
    if is(&["©art", "©aut", "auth", "author", "artist"]) {
        return c("Author", People, Some(Flag::Names), Some(EditKey::Author));
    }
    if is(&[
        "aart", "©wrt", "perf", "©prd", "©dir", "producer", "director", "owner",
    ]) {
        return c("Credited person", People, Some(Flag::Names), None);
    }
    if is(&["©cmt", "comment", "cmt"]) {
        return c(
            "Comment",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        );
    }
    if is(&[
        "©des",
        "desc",
        "ldes",
        "dscp",
        "description",
        "information",
        "©inf",
    ]) {
        return c(
            "Description",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        );
    }
    if is(&["©alb", "albm", "album"]) {
        return c("Album", Description, None, Some(EditKey::Album));
    }
    if is(&["©gen", "gnre", "genre"]) {
        return c("Genre", Description, None, Some(EditKey::Genre));
    }
    if is(&["trkn"]) {
        return c("Track", Description, None, Some(EditKey::Track));
    }
    if is(&["cprt", "©cpy", "copyright"]) {
        return c("Copyright", Description, None, Some(EditKey::Copyright));
    }
    if is(&["kywd", "keyw", "keywords"]) {
        return c("Keywords", Description, None, None);
    }
    if is(&["©lyr"]) {
        return c("Lyrics", Description, None, None);
    }
    if is(&["covr"]) {
        return c("Cover picture", Hidden, Some(Flag::Hidden), None);
    }
    c(&format!("Other data ({key})"), Hidden, None, None)
}

/// `+36.1901+044.0091/` or `+36.1901+044.0091+012.000/` (ISO 6709).
pub(crate) fn parse_iso6709(text: &str) -> Option<GeoPoint> {
    let t = text.trim().trim_end_matches('/');
    let mut nums = Vec::new();
    let mut cur = String::new();
    for ch in t.chars() {
        if (ch == '+' || ch == '-') && !cur.is_empty() {
            nums.push(cur.clone());
            cur.clear();
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        nums.push(cur);
    }
    let lat: f64 = nums.first()?.parse().ok()?;
    let lon: f64 = nums.get(1)?.parse().ok()?;
    GeoPoint::parse(&format!("{lat}, {lon}"))
}

/// Text from a QuickTime `©xxx` user-data item: one or more
/// `[u16 length][u16 language][text]` entries, or an iTunes `data` box.
fn qt_text(p: &[u8]) -> String {
    if p.len() >= 16 && &p[4..8] == b"data" {
        return decode_data(&p[8..], "");
    }
    let mut parts = Vec::new();
    let mut i = 0;
    while i + 4 <= p.len() {
        let n = be16(p, i) as usize;
        if n == 0 || i + 4 + n > p.len() {
            break;
        }
        parts.push(String::from_utf8_lossy(&p[i + 4..i + 4 + n]).into_owned());
        i += 4 + n;
    }
    if parts.is_empty() {
        String::from_utf8_lossy(p).trim_matches('\0').to_string()
    } else {
        parts.join("; ")
    }
}

/// The value of an iTunes / QuickTime `data` box payload
/// (`[u32 type][u32 locale][value]`).
fn decode_data(p: &[u8], key: &str) -> String {
    if p.len() < 8 {
        return String::new();
    }
    let ty = be32(p, 0) & 0x00FF_FFFF;
    let v = &p[8..];
    match ty {
        1 | 4 => String::from_utf8_lossy(v).into_owned(),
        2 | 5 => {
            let units: Vec<u16> = v
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        }
        13 | 14 | 27 => crate::formats::audio::picture_label(v),
        21 => match v.len() {
            1 => (v[0] as i8).to_string(),
            2 => (be16(v, 0) as i16).to_string(),
            4 => (be32(v, 0) as i32).to_string(),
            8 => (be64(v, 0) as i64).to_string(),
            _ => format!("{} bytes", v.len()),
        },
        22 => match v.len() {
            1 => v[0].to_string(),
            2 => be16(v, 0).to_string(),
            4 => be32(v, 0).to_string(),
            8 => be64(v, 0).to_string(),
            _ => format!("{} bytes", v.len()),
        },
        23 if v.len() == 4 => f32::from_be_bytes([v[0], v[1], v[2], v[3]]).to_string(),
        24 if v.len() == 8 => f64::from_be_bytes(v.try_into().unwrap_or([0; 8])).to_string(),
        _ => match key {
            "trkn" | "disk" if v.len() >= 6 => {
                let (n, t) = (be16(v, 2), be16(v, 4));
                if t > 0 {
                    format!("{n}/{t}")
                } else {
                    n.to_string()
                }
            }
            "gnre" if v.len() >= 2 => format!("Genre #{}", be16(v, 0).saturating_sub(1)),
            "covr" => crate::formats::audio::picture_label(v),
            _ if !v.is_empty() && v.iter().all(|&c| c >= 0x20 || c == b'\n') => {
                String::from_utf8_lossy(v).into_owned()
            }
            _ => format!("{} bytes of data", v.len()),
        },
    }
}

/// A 3GPP string box (`titl`, `auth`, `dscp`, ...): version/flags, language,
/// then a UTF-8 or UTF-16 (with BOM) string.
fn gpp_text(p: &[u8]) -> String {
    let s = p.get(6..).unwrap_or(&[]);
    if s.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = s[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        String::from_utf8_lossy(&s[..end]).into_owned()
    }
}

/// The 3GPP `loci` box: place name and coordinates.
fn loci(p: &[u8]) -> (String, Option<GeoPoint>) {
    let s = p.get(6..).unwrap_or(&[]);
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    let name = String::from_utf8_lossy(&s[..end]).into_owned();
    let at = 6 + end + 1 + 1; // name, NUL, role
    let fixed = |i: usize| be32(p, i) as i32 as f64 / 65536.0;
    let gps = (p.len() >= at + 8)
        .then(|| GeoPoint::parse(&format!("{}, {}", fixed(at + 4), fixed(at))))
        .flatten();
    let value = match gps {
        Some(g) if name.is_empty() => g.display(),
        Some(g) => format!("{name} ({})", g.display()),
        None => name,
    };
    (value, gps)
}

fn walk_top(f: &mut File, len: u64, sc: &mut Scan) -> Result<()> {
    let top = children(f, 0, len)?;
    let first = top.first().ok_or("This is not an MP4 or QuickTime file.")?;
    let known: [&[u8; 4]; 8] = [
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"pnot",
    ];
    if !known.contains(&&first.kind) {
        return Err("This is not an MP4 or QuickTime file.".into());
    }
    for b in top {
        match &b.kind {
            b"ftyp" => {
                let p = read_at(f, b.body(), b.body_len().min(4))?;
                if p.len() == 4 {
                    sc.brand = Some([p[0], p[1], p[2], p[3]]);
                }
            }
            b"moov" => {
                sc.has_moov = true;
                walk_moov(f, b, sc)?;
            }
            b"uuid" => uuid_box(f, b, sc)?,
            _ => {}
        }
    }
    if !sc.has_moov {
        return Err("The movie header (moov) is missing; the file may be incomplete.".into());
    }
    Ok(())
}

fn times(sc: &mut Scan, b: Bx, p: &[u8]) {
    let (w, v1) = if p.first() == Some(&1) {
        (8, true)
    } else {
        (4, false)
    };
    let get = |at: usize| if v1 { be64(p, at) } else { be32(p, at) as u64 };
    sc.created.push((b.body() + 4, w, get(4)));
    sc.modified.push((b.body() + 4 + w, w, get(4 + w as usize)));
}

fn walk_moov(f: &mut File, moov: Bx, sc: &mut Scan) -> Result<()> {
    for b in children(f, moov.body(), moov.end())? {
        match &b.kind {
            b"mvhd" => {
                let p = read_at(f, b.body(), b.body_len().min(32))?;
                times(sc, b, &p);
                let (scale, dur) = if p.first() == Some(&1) {
                    (be32(&p, 20), be64(&p, 24))
                } else {
                    (be32(&p, 12), be32(&p, 16) as u64)
                };
                if scale > 0 && dur > 0 && dur != u32::MAX as u64 {
                    sc.duration = Some(dur as f64 / scale as f64);
                }
            }
            b"trak" => {
                let mut t = Track::default();
                walk_trak(f, b, sc, &mut t)?;
                sc.tracks.push(t);
            }
            b"udta" => udta(f, b, sc)?,
            b"meta" => meta(f, b, sc)?,
            b"uuid" => uuid_box(f, b, sc)?,
            _ => {}
        }
    }
    Ok(())
}

fn walk_trak(f: &mut File, parent: Bx, sc: &mut Scan, t: &mut Track) -> Result<()> {
    for b in children(f, parent.body(), parent.end())? {
        match &b.kind {
            b"tkhd" => {
                let p = body(f, b)?;
                times(sc, b, &p);
                if p.len() >= 84 {
                    let (w, h) = (be32(&p, p.len() - 8) >> 16, be32(&p, p.len() - 4) >> 16);
                    if w > 0 && h > 0 {
                        t.dims = Some((w, h));
                    }
                }
            }
            b"mdhd" => {
                let p = read_at(f, b.body(), b.body_len().min(32))?;
                times(sc, b, &p);
            }
            b"hdlr" => {
                let p = read_at(f, b.body(), b.body_len().min(12))?;
                if p.len() >= 12 {
                    t.handler = [p[8], p[9], p[10], p[11]];
                }
            }
            b"mdia" | b"minf" | b"stbl" => walk_trak(f, b, sc, t)?,
            b"stsd" => {
                let p = read_at(f, b.body(), b.body_len().min(64))?;
                if p.len() >= 16 {
                    t.codec = Some([p[12], p[13], p[14], p[15]]);
                    let e = &p[8..];
                    match &t.handler {
                        b"vide" => {
                            let (w, h) = (be16(e, 32) as u32, be16(e, 34) as u32);
                            if w > 0 && h > 0 {
                                t.dims = Some((w, h));
                            }
                        }
                        b"soun" => {
                            t.channels = be16(e, 24);
                            t.sample_rate = be32(e, 32) >> 16;
                        }
                        _ => {}
                    }
                }
            }
            b"udta" => udta(f, b, sc)?,
            b"meta" => meta(f, b, sc)?,
            _ => {}
        }
    }
    Ok(())
}

fn udta(f: &mut File, u: Bx, sc: &mut Scan) -> Result<()> {
    sc.blocks.push(u);
    for b in children(f, u.body(), u.end())? {
        let key = fourcc(b.kind);
        let id = format!("mp4:udta:{key}");
        match &b.kind {
            b"meta" => meta(f, b, sc)?,
            b"free" | b"skip" => {}
            b"XMP_" => xmp(f, b, b.body(), sc)?,
            b"loci" => {
                let (value, gps) = loci(&body(f, b)?);
                if sc.gps.is_none() {
                    sc.gps = gps;
                }
                sc.push(id, "loci", value, SRC_QT, Patch::Free(b));
            }
            b"titl" | b"auth" | b"dscp" | b"perf" | b"gnre" | b"cprt" | b"albm" | b"kywd" => {
                let v = gpp_text(&body(f, b)?);
                sc.push(id, &key, v, SRC_QT, Patch::Free(b));
            }
            b"yrrc" => {
                let p = body(f, b)?;
                sc.push(id, "yrrc", be16(&p, 4).to_string(), SRC_QT, Patch::Free(b));
            }
            [0xA9, ..] => {
                let v = qt_text(&body(f, b)?);
                sc.push(id, &key, v, SRC_QT, Patch::Free(b));
            }
            _ => {
                let v = format!("{} bytes", b.body_len());
                sc.push(id, &key, v, SRC_QT, Patch::Free(b));
            }
        }
    }
    Ok(())
}

/// A `meta` box: QuickTime `mdta` keys + `ilst`, or an iTunes `ilst`.
fn meta(f: &mut File, m: Bx, sc: &mut Scan) -> Result<()> {
    sc.blocks.push(m);
    let peek = read_at(f, m.body(), m.body_len().min(8))?;
    // QuickTime `meta` has no version/flags; ISO `meta` is a full box.
    let start = if peek.len() >= 8 && &peek[4..8] == b"hdlr" {
        m.body()
    } else {
        m.body() + 4
    };
    if start > m.end() {
        return Err(damaged());
    }
    let kids = children(f, start, m.end())?;
    let mut handler = [0u8; 4];
    let mut keys: Vec<String> = Vec::new();
    for b in &kids {
        match &b.kind {
            b"hdlr" => {
                let p = read_at(f, b.body(), b.body_len().min(12))?;
                if p.len() >= 12 {
                    handler = [p[8], p[9], p[10], p[11]];
                }
            }
            b"keys" => {
                let p = body(f, *b)?;
                let n = be32(&p, 4) as usize;
                let mut i = 8;
                for _ in 0..n.min(10_000) {
                    let size = be32(&p, i) as usize;
                    if size < 8 || i + size > p.len() {
                        break;
                    }
                    keys.push(String::from_utf8_lossy(&p[i + 8..i + size]).into_owned());
                    i += size;
                }
            }
            _ => {}
        }
    }
    let mdta = &handler == b"mdta";
    for ilst in kids.iter().filter(|b| &b.kind == b"ilst") {
        for item in children(f, ilst.body(), ilst.end())? {
            if matches!(&item.kind, b"free" | b"skip") {
                continue;
            }
            let mut key = if mdta {
                let idx = u32::from_be_bytes(item.kind) as usize;
                match idx.checked_sub(1).and_then(|i| keys.get(i)) {
                    Some(k) => k.clone(),
                    None => format!("key #{idx}"),
                }
            } else {
                fourcc(item.kind)
            };
            let mut value = String::new();
            for sub in children(f, item.body(), item.end())? {
                match &sub.kind {
                    b"name" if &item.kind == b"----" => {
                        let p = body(f, sub)?;
                        key = String::from_utf8_lossy(p.get(4..).unwrap_or(&[])).into_owned();
                    }
                    b"data" if value.is_empty() => {
                        value = decode_data(&body(f, sub)?, &fourcc(item.kind));
                    }
                    _ => {}
                }
            }
            let (id, source) = if mdta {
                (format!("mp4:keys:{key}"), SRC_QT)
            } else {
                (format!("mp4:ilst:{key}"), SRC_ILST)
            };
            sc.push(id, &key, value, source, Patch::Free(item));
        }
    }
    Ok(())
}

fn uuid_box(f: &mut File, b: Bx, sc: &mut Scan) -> Result<()> {
    if b.body_len() >= 16 && read_at(f, b.body(), 16)? == XMP_UUID {
        xmp(f, b, b.body() + 16, sc)?;
    }
    Ok(())
}

fn xmp(f: &mut File, b: Bx, payload: u64, sc: &mut Scan) -> Result<()> {
    sc.blocks.push(b);
    let data = read_at(f, payload, b.end() - payload)?;
    let text = String::from_utf8_lossy(&data);
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if text.is_empty() {
        return Ok(());
    }
    let fields = crate::formats::xmp::fields(text, "XMP");
    if fields.is_empty() {
        let field = Field::new(
            "mp4:xmp",
            "XMP metadata packet",
            format!("{} bytes", text.len()),
            Category::Hidden,
            "XMP",
        )
        .flag(Flag::Hidden);
        sc.items.push(Item {
            field,
            patch: Patch::Xmp { bx: b, payload },
        });
    }
    for field in fields {
        sc.items.push(Item {
            field,
            patch: Patch::Xmp { bx: b, payload },
        });
    }
    Ok(())
}

fn scan(path: &Path) -> Result<Scan> {
    let mut f = File::open(path).map_err(io_err)?;
    let len = f.metadata().map_err(io_err)?.len();
    let mut sc = Scan::default();
    walk_top(&mut f, len, &mut sc)?;
    // Timestamps: one field each for "created" and "modified".
    for (slots, id, label) in [
        (sc.created.clone(), "mp4:mvhd:created", "Created"),
        (sc.modified.clone(), "mp4:mvhd:modified", "Modified"),
    ] {
        if let Some(&(_, _, v)) = slots.iter().find(|s| s.2 != 0) {
            let text = DateTime::from_unix(v as i64 - MAC_EPOCH).text();
            let field = Field::new(
                id,
                label,
                format!("{text} UTC"),
                Category::Dates,
                "MP4 header",
            );
            let ranges = slots.iter().map(|&(o, w, _)| (o, w)).collect();
            sc.items.push(Item {
                field,
                patch: Patch::Zeros(ranges),
            });
        }
    }
    Ok(sc)
}

/// `1:02` / `1:02:03` (seconds rounded).
pub(crate) fn duration_text(secs: f64) -> String {
    let s = secs.round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else if secs < 10.0 {
        format!("{m}:{s:02} ({secs:.2} s)")
    } else {
        format!("{m}:{s:02}")
    }
}

fn codec_name(c: [u8; 4]) -> String {
    match &c {
        b"avc1" | b"avc3" => "H.264 (AVC)".into(),
        b"hvc1" | b"hev1" => "H.265 (HEVC)".into(),
        b"av01" => "AV1".into(),
        b"vp09" => "VP9".into(),
        b"mp4v" => "MPEG-4 Visual".into(),
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" => "Apple ProRes".into(),
        b"mp4a" => "AAC".into(),
        b"alac" => "Apple Lossless".into(),
        b"Opus" => "Opus".into(),
        b"fLaC" => "FLAC".into(),
        b"ac-3" => "Dolby Digital (AC-3)".into(),
        b"ec-3" => "Dolby Digital Plus".into(),
        b"lpcm" | b"sowt" | b"twos" => "PCM".into(),
        _ => fourcc(c).trim().to_string(),
    }
}

fn technical(sc: &Scan) -> Vec<Field> {
    let tech = |id: &str, label: &str, v: String| {
        Field::new(id, label, v, Category::Technical, "MP4 header").fixed()
    };
    let mut out = Vec::new();
    if let Some(b) = sc.brand {
        out.push(tech("mp4:ftyp", "File type", fourcc(b).trim().to_string()));
    }
    if let Some(d) = sc.duration {
        out.push(tech("mp4:duration", "Duration", duration_text(d)));
    }
    if let Some(t) = sc.tracks.iter().find(|t| &t.handler == b"vide") {
        if let Some((w, h)) = t.dims {
            out.push(tech("mp4:video:size", "Video size", format!("{w} x {h}")));
        }
        if let Some(c) = t.codec {
            out.push(tech("mp4:video:codec", "Video codec", codec_name(c)));
        }
    }
    if let Some(t) = sc.tracks.iter().find(|t| &t.handler == b"soun") {
        if let Some(c) = t.codec {
            out.push(tech("mp4:audio:codec", "Audio codec", codec_name(c)));
        }
        if t.sample_rate > 0 {
            out.push(tech(
                "mp4:audio:rate",
                "Sample rate",
                format!("{} Hz", t.sample_rate),
            ));
        }
        if t.channels > 0 {
            let v = match t.channels {
                1 => "1 (mono)".to_string(),
                2 => "2 (stereo)".to_string(),
                n => n.to_string(),
            };
            out.push(tech("mp4:audio:channels", "Channels", v));
        }
    }
    out
}

pub fn read(path: &Path) -> Result<Extract> {
    read_as(path, Kind::Video)
}

/// The fields of an MP4 file; `Kind::Audio` maps the date to "Year" and
/// lists the audio edit keys (used for M4A).
pub(crate) fn read_as(path: &Path, kind: Kind) -> Result<Extract> {
    let sc = scan(path)?;
    let mut ex = Extract {
        gps: sc.gps,
        ..Extract::default()
    };
    let mut created = None;
    for it in &sc.items {
        let mut field = it.field.clone();
        if kind == Kind::Audio && field.edit == Some(EditKey::DateTaken) {
            field.edit = Some(EditKey::Year);
            field.label = "Year / date".into();
        }
        if ex.taken.is_none() && matches!(field.edit, Some(EditKey::DateTaken | EditKey::Year)) {
            ex.taken = taken_text(&field.value);
        }
        if field.id == "mp4:mvhd:created" {
            created = field.value.strip_suffix(" UTC").map(str::to_string);
        }
        ex.fields.push(field);
    }
    if ex.taken.is_none() {
        ex.taken = created;
    }
    ex.fields.extend(technical(&sc));
    if sc
        .items
        .iter()
        .any(|i| i.field.id == "mp4:udta:loci" || i.field.category == Category::Location)
        && ex.gps.is_none()
    {
        ex.notes
            .push("A recording place is stored, but without readable coordinates.".into());
    }
    ex.editable = if kind == Kind::Audio {
        crate::formats::audio::AUDIO_KEYS.to_vec()
    } else {
        vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Description,
            EditKey::DateTaken,
        ]
    };
    Ok(ex)
}

/// A date value as `YYYY-MM-DD HH:MM:SS`, or the bare year.
pub(crate) fn taken_text(v: &str) -> Option<String> {
    let v = v.trim();
    if v.len() == 4 && v.chars().all(|c| c.is_ascii_digit()) {
        return Some(v.to_string());
    }
    DateTime::parse(v).map(|d| d.text())
}

fn zero(f: &mut File, off: u64, len: u64) -> Result<()> {
    f.seek(SeekFrom::Start(off)).map_err(io_err)?;
    let chunk = vec![0u8; 64 * 1024];
    let mut left = len;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        f.write_all(&chunk[..n]).map_err(io_err)?;
        left -= n as u64;
    }
    Ok(())
}

fn free_box(f: &mut File, b: Bx) -> Result<()> {
    f.seek(SeekFrom::Start(b.start + 4)).map_err(io_err)?;
    f.write_all(b"free").map_err(io_err)?;
    zero(f, b.body(), b.body_len())
}

/// Takes the matching items out of `path` in place.
fn apply(path: &Path, items: &[&Item], blocks: &[Bx], opts: Option<&CleanOptions>) -> Result<()> {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io_err)?;
    let mut xmp_done: Vec<u64> = Vec::new();
    for it in items {
        match &it.patch {
            Patch::Free(b) => free_box(&mut f, *b)?,
            Patch::Zeros(ranges) => {
                for &(o, w) in ranges {
                    zero(&mut f, o, w)?;
                }
            }
            Patch::Xmp { bx, payload } => {
                if xmp_done.contains(&bx.start) {
                    continue;
                }
                xmp_done.push(bx.start);
                let cap = bx.end() - payload;
                let data = read_at(&mut f, *payload, cap)?;
                let text = String::from_utf8_lossy(&data).into_owned();
                let stripped = match opts {
                    Some(o) if !o.is_everything() => crate::formats::xmp::strip(&text, o),
                    _ => None,
                };
                match stripped {
                    Some(s) if (s.len() as u64) <= cap => {
                        let mut out = s.into_bytes();
                        out.resize(cap as usize, b' ');
                        f.seek(SeekFrom::Start(*payload)).map_err(io_err)?;
                        f.write_all(&out).map_err(io_err)?;
                    }
                    _ => free_box(&mut f, *bx)?,
                }
            }
        }
    }
    for b in blocks {
        free_box(&mut f, *b)?;
    }
    f.sync_all().map_err(io_err)
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let sc = scan(src)?;
    std::fs::copy(src, dst).map_err(|e| format!("Could not write the cleaned copy: {e}"))?;
    let items: Vec<&Item> = sc
        .items
        .iter()
        .filter(|i| i.field.removable && opts.removes(i.field.category))
        .collect();
    let blocks: &[Bx] = if opts.is_everything() {
        &sc.blocks
    } else {
        &[]
    };
    apply(dst, &items, blocks, Some(opts))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::mp4::{Atom, AtomData, AtomIdent, Ilst, Mp4File};
    use lofty::prelude::*;

    scan(src)?;
    for e in edits {
        check_edit(e)?;
    }
    std::fs::copy(src, dst).map_err(|e| format!("Could not write the edited copy: {e}"))?;
    let mut file = File::open(dst).map_err(io_err)?;
    let parsed = Mp4File::read_from(&mut file, ParseOptions::new().read_properties(false))
        .map_err(|e| format!("The MP4 tags could not be read ({e})."))?;
    drop(file);
    let mut ilst = parsed.ilst().cloned().unwrap_or_else(Ilst::new);
    let text_atom = |code: &[u8; 4], v: &str| {
        Atom::new(AtomIdent::Fourcc(*code), AtomData::UTF8(v.to_string()))
    };
    for e in edits {
        let v = e.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        match (e.key, v) {
            (EditKey::Title, Some(v)) => ilst.set_title(v.into()),
            (EditKey::Title, None) => ilst.remove_title(),
            (EditKey::Author, Some(v)) => ilst.set_artist(v.into()),
            (EditKey::Author, None) => ilst.remove_artist(),
            (EditKey::Description, Some(v)) => ilst.set_comment(v.into()),
            (EditKey::Description, None) => ilst.remove_comment(),
            (EditKey::Album, Some(v)) => ilst.set_album(v.into()),
            (EditKey::Album, None) => ilst.remove_album(),
            (EditKey::Genre, Some(v)) => ilst.set_genre(v.into()),
            (EditKey::Genre, None) => ilst.remove_genre(),
            (EditKey::Track, v) => {
                ilst.remove_track();
                ilst.remove_track_total();
                if let Some((n, t)) = v.and_then(split_track) {
                    ilst.set_track(n);
                    if let Some(t) = t {
                        ilst.set_track_total(t);
                    }
                }
            }
            (EditKey::DateTaken | EditKey::Year, v) => {
                let _ = ilst.remove(&AtomIdent::Fourcc(*b"\xa9day")).count();
                if let Some(v) = v {
                    let v = match DateTime::parse(v) {
                        Some(dt) if v.len() > 4 => dt.iso(),
                        _ => v.to_string(),
                    };
                    ilst.replace_atom(text_atom(b"\xa9day", &v));
                }
            }
            (EditKey::Copyright, v) => {
                let _ = ilst.remove(&AtomIdent::Fourcc(*b"cprt")).count();
                if let Some(v) = v {
                    ilst.replace_atom(text_atom(b"cprt", v));
                }
            }
            _ => {}
        }
    }
    ilst.save_to_path(dst, WriteOptions::default())
        .map_err(|e| format!("The MP4 tags could not be written ({e})."))?;
    // Older copies of the edited values elsewhere would be read back as stale.
    let edited: Vec<EditKey> = edits
        .iter()
        .flat_map(|e| match e.key {
            EditKey::DateTaken | EditKey::Year => vec![EditKey::DateTaken, EditKey::Year],
            k => vec![k],
        })
        .collect();
    let sc = scan(dst)?;
    let stale: Vec<&Item> = sc
        .items
        .iter()
        .filter(|i| !i.field.id.starts_with("mp4:ilst:"))
        .filter(|i| i.field.edit.is_some_and(|k| edited.contains(&k)))
        .collect();
    if !stale.is_empty() {
        apply(dst, &stale, &[], None)?;
    }
    Ok(())
}

/// `3` or `3/12`.
pub(crate) fn split_track(v: &str) -> Option<(u32, Option<u32>)> {
    let mut parts = v.split('/').map(str::trim);
    let n = parts.next()?.parse().ok()?;
    let t = match parts.next() {
        Some(t) => Some(t.parse().ok()?),
        None => None,
    };
    Some((n, t))
}

fn check_edit(e: &Edit) -> Result<()> {
    let v = e.value.as_deref().map(str::trim).unwrap_or("");
    match e.key {
        EditKey::Title
        | EditKey::Author
        | EditKey::Description
        | EditKey::Album
        | EditKey::Genre
        | EditKey::Copyright => Ok(()),
        EditKey::Track if v.is_empty() || split_track(v).is_some() => Ok(()),
        EditKey::Track => Err("The track must look like 3 or 3/12.".into()),
        EditKey::DateTaken | EditKey::Year if v.is_empty() || DateTime::parse(v).is_some() => {
            Ok(())
        }
        EditKey::DateTaken | EditKey::Year => {
            Err("The date must look like 2024-05-01 10:20:30 or 2024.".into())
        }
        k => Err(format!(
            "{} cannot be changed in this kind of file.",
            k.label(Kind::Video)
        )),
    }
}

// ---------------------------------------------------------------------------
// Samples (invented data).

pub(crate) fn mk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 8);
    v.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

fn full(kind: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = flags.to_be_bytes().to_vec();
    b.extend_from_slice(body);
    mk(kind, &b)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

/// Seconds since 1904 for 2024-05-01 10:20:30 UTC (sample timestamps).
const SAMPLE_TIME: u32 = (1_714_558_830 + MAC_EPOCH) as u32;

const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

fn mvhd(duration: u32) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [SAMPLE_TIME, SAMPLE_TIME, 1000, duration, 0x10000] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&0x0100u16.to_be_bytes());
    b.extend_from_slice(&[0; 10]);
    MATRIX
        .iter()
        .for_each(|m| b.extend_from_slice(&m.to_be_bytes()));
    b.extend_from_slice(&[0; 24]);
    b.extend_from_slice(&3u32.to_be_bytes());
    full(b"mvhd", 0, &b)
}

fn tkhd(id: u32, duration: u32, dims: (u32, u32), audio: bool) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [SAMPLE_TIME, SAMPLE_TIME, id, 0, duration, 0, 0] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&[0, 0, 0, 0]); // layer, alternate group
    b.extend_from_slice(&(if audio { 0x0100u16 } else { 0 }).to_be_bytes());
    b.extend_from_slice(&[0, 0]);
    MATRIX
        .iter()
        .for_each(|m| b.extend_from_slice(&m.to_be_bytes()));
    b.extend_from_slice(&(dims.0 << 16).to_be_bytes());
    b.extend_from_slice(&(dims.1 << 16).to_be_bytes());
    full(b"tkhd", 3, &b)
}

fn mdhd(scale: u32, duration: u32) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [SAMPLE_TIME, SAMPLE_TIME, scale, duration] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&[0x55, 0xC4, 0, 0]);
    full(b"mdhd", 0, &b)
}

fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
    let mut b = vec![0; 4];
    b.extend_from_slice(kind);
    b.extend_from_slice(&[0; 13]);
    full(b"hdlr", 0, &b)
}

fn trak(id: u32, video: bool, chunk_offset: u32, chunk_len: u32) -> Vec<u8> {
    let (scale, dur) = if video { (600, 1200) } else { (44100, 88200) };
    let entry = if video {
        let mut e = vec![0; 6];
        e.extend_from_slice(&1u16.to_be_bytes());
        e.extend_from_slice(&[0; 16]);
        e.extend_from_slice(&320u16.to_be_bytes());
        e.extend_from_slice(&240u16.to_be_bytes());
        e.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        e.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        e.extend_from_slice(&[0; 4]);
        e.extend_from_slice(&1u16.to_be_bytes());
        e.extend_from_slice(&[0; 32]);
        e.extend_from_slice(&0x18u16.to_be_bytes());
        e.extend_from_slice(&0xFFFFu16.to_be_bytes());
        mk(b"avc1", &e)
    } else {
        let mut e = vec![0; 6];
        e.extend_from_slice(&1u16.to_be_bytes());
        e.extend_from_slice(&[0; 8]);
        e.extend_from_slice(&2u16.to_be_bytes());
        e.extend_from_slice(&16u16.to_be_bytes());
        e.extend_from_slice(&[0; 4]);
        e.extend_from_slice(&(44100u32 << 16).to_be_bytes());
        mk(b"mp4a", &e)
    };
    let u32s = |v: &[u32]| v.iter().flat_map(|x| x.to_be_bytes()).collect::<Vec<u8>>();
    let stbl = mk(
        b"stbl",
        &cat(&[
            full(b"stsd", 0, &cat(&[1u32.to_be_bytes().to_vec(), entry])),
            full(b"stts", 0, &u32s(&[1, 1, dur])),
            full(b"stsc", 0, &u32s(&[1, 1, 1, 1])),
            full(b"stsz", 0, &u32s(&[chunk_len, 1])),
            full(b"stco", 0, &u32s(&[1, chunk_offset])),
        ]),
    );
    let dinf = mk(
        b"dinf",
        &full(
            b"dref",
            0,
            &cat(&[1u32.to_be_bytes().to_vec(), full(b"url ", 1, &[])]),
        ),
    );
    let xmhd = if video {
        full(b"vmhd", 1, &[0; 8])
    } else {
        full(b"smhd", 0, &[0; 4])
    };
    let minf = mk(b"minf", &cat(&[xmhd, dinf, stbl]));
    let mdia = mk(
        b"mdia",
        &cat(&[
            mdhd(scale, dur),
            hdlr(if video { b"vide" } else { b"soun" }),
            minf,
        ]),
    );
    let dims = if video { (320, 240) } else { (0, 0) };
    mk(b"trak", &cat(&[tkhd(id, 2000, dims, !video), mdia]))
}

fn qt_item(code: &[u8; 4], text: &str) -> Vec<u8> {
    let mut b = (text.len() as u16).to_be_bytes().to_vec();
    b.extend_from_slice(&0x15C7u16.to_be_bytes());
    b.extend_from_slice(text.as_bytes());
    mk(code, &b)
}

fn data_box(ty: u32, value: &[u8]) -> Vec<u8> {
    let mut b = ty.to_be_bytes().to_vec();
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(value);
    mk(b"data", &b)
}

fn itunes_meta(items: &[Vec<u8>]) -> Vec<u8> {
    full(
        b"meta",
        0,
        &cat(&[hdlr(b"mdir"), mk(b"ilst", &items.concat())]),
    )
}

fn mdta_meta(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut keys = (entries.len() as u32).to_be_bytes().to_vec();
    let mut ilst = Vec::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        keys.extend_from_slice(&mk(b"mdta", k.as_bytes()));
        ilst.extend_from_slice(&mk(
            &(i as u32 + 1).to_be_bytes(),
            &data_box(1, v.as_bytes()),
        ));
    }
    // QuickTime style: no version/flags on `meta` itself.
    mk(
        b"meta",
        &cat(&[hdlr(b"mdta"), full(b"keys", 0, &keys), mk(b"ilst", &ilst)]),
    )
}

fn xmp_box() -> Vec<u8> {
    let xml =
        "<?xpacket begin='' id='W5M0MpCehiHzreSzNTczkc9d'?><x:xmpmeta xmlns:x='adobe:ns:meta/'>\
<rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'><rdf:Description rdf:about='' \
xmlns:xmp='http://ns.adobe.com/xap/1.0/' xmp:CreatorTool='ExampleCam 1.0'/></rdf:RDF></x:xmpmeta>\
<?xpacket end='w'?>";
    mk(b"uuid", &cat(&[XMP_UUID.to_vec(), xml.as_bytes().to_vec()]))
}

fn build(brand: &[u8; 4], video: bool, udta_items: Vec<u8>, extra_moov: Vec<u8>) -> Vec<u8> {
    let ftyp = mk(
        b"ftyp",
        &cat(&[
            brand.to_vec(),
            vec![0, 0, 2, 0],
            brand.to_vec(),
            b"isommp42".to_vec(),
        ]),
    );
    let payload: Vec<u8> = (0..4096u32).map(|i| (i * 31 % 251) as u8).collect();
    let xmp = if video { xmp_box() } else { Vec::new() };
    let moov_with = |off: u32| {
        let mut parts = vec![mvhd(2000)];
        if video {
            parts.push(trak(1, true, off, 2048));
            parts.push(trak(2, false, off + 2048, 2048));
        } else {
            parts.push(trak(1, false, off, 4096));
        }
        parts.push(mk(b"udta", &udta_items));
        parts.push(extra_moov.clone());
        mk(b"moov", &cat(&parts))
    };
    let moov_len = moov_with(0).len();
    let off = (ftyp.len() + moov_len + xmp.len() + 8) as u32;
    cat(&[ftyp, moov_with(off), xmp, mk(b"mdat", &payload)])
}

fn video_sample(brand: &[u8; 4]) -> Vec<u8> {
    let udta = cat(&[
        qt_item(b"\xa9xyz", "+36.1901+044.0091/"),
        qt_item(b"\xa9mak", "ExamplePhone"),
        qt_item(b"\xa9mod", "EP-7"),
        qt_item(b"\xa9swr", "ExampleCam 1.0"),
        qt_item(b"\xa9nam", "Invented Clip"),
        qt_item(b"\xa9day", "2024-05-01T10:20:30+0300"),
        itunes_meta(&[
            mk(b"\xa9ART", &data_box(1, b"Test Person")),
            mk(b"\xa9cmt", &data_box(1, b"Sample comment")),
        ]),
    ]);
    let mdta = mdta_meta(&[
        (
            "com.apple.quicktime.location.ISO6709",
            "+36.1901+044.0091+012.000/",
        ),
        ("com.apple.quicktime.make", "ExamplePhone"),
        ("com.apple.quicktime.model", "EP-7"),
        ("com.apple.quicktime.software", "ExampleOS 17.1"),
        (
            "com.apple.quicktime.creationdate",
            "2024-05-01T10:20:30+0300",
        ),
    ]);
    build(brand, true, udta, mdta)
}

/// An MP4 video (H.264 + AAC track headers) with phone GPS, make and model.
pub fn sample() -> Vec<u8> {
    video_sample(b"isom")
}

/// The same as [`sample`] as a QuickTime `.mov`.
pub fn sample_mov() -> Vec<u8> {
    video_sample(b"qt  ")
}

/// An M4A song with iTunes tags and a cover picture.
pub fn sample_m4a() -> Vec<u8> {
    let jpeg = crate::formats::audio::tiny_jpeg();
    let udta = itunes_meta(&[
        mk(b"\xa9nam", &data_box(1, b"Invented Song")),
        mk(b"\xa9ART", &data_box(1, b"Test Artist")),
        mk(b"\xa9alb", &data_box(1, b"Sample Album")),
        mk(b"\xa9day", &data_box(1, b"2024")),
        mk(b"\xa9too", &data_box(1, b"ExampleEncoder 1.0")),
        mk(b"\xa9cmt", &data_box(1, b"Recorded at home")),
        mk(b"covr", &data_box(13, &jpeg)),
    ]);
    build(b"M4A ", false, udta, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "metaforge-mp4-{}-{name}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn get<'a>(ex: &'a Extract, id: &str) -> Option<&'a Field> {
        ex.fields.iter().find(|f| f.id == id)
    }

    fn leftovers(ex: &Extract, opts: &CleanOptions) -> Vec<Field> {
        ex.fields
            .iter()
            .filter(|f| f.removable && opts.removes(f.category))
            .cloned()
            .collect()
    }

    fn mdat(bytes: &[u8]) -> Vec<u8> {
        let i = bytes.windows(4).position(|w| w == b"mdat").unwrap();
        bytes[i - 4..].to_vec()
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn reads_phone_video() {
        let t = Tmp::new("read");
        let p = t.file("a.mp4", &sample());
        let ex = read(&p).unwrap();
        let g = ex.gps.unwrap();
        assert!((g.lat - 36.1901).abs() < 1e-6 && (g.lon - 44.0091).abs() < 1e-6);
        let xyz = get(&ex, "mp4:udta:©xyz").unwrap();
        assert_eq!(xyz.category, Category::Location);
        assert_eq!(xyz.flag, Some(Flag::Location));
        assert_eq!(xyz.label, "Recorded at (GPS)");
        assert_eq!(get(&ex, "mp4:udta:©mak").unwrap().value, "ExamplePhone");
        let model = get(&ex, "mp4:keys:com.apple.quicktime.model").unwrap();
        assert_eq!(
            (model.value.as_str(), model.category),
            ("EP-7", Category::Device)
        );
        assert_eq!(model.label, "Phone model");
        assert_eq!(
            get(&ex, "mp4:keys:com.apple.quicktime.software")
                .unwrap()
                .category,
            Category::Software
        );
        let art = get(&ex, "mp4:ilst:©ART").unwrap();
        assert_eq!(
            (art.value.as_str(), art.flag),
            ("Test Person", Some(Flag::Names))
        );
        assert_eq!(
            get(&ex, "mp4:mvhd:created").unwrap().value,
            "2024-05-01 10:20:30 UTC"
        );
        assert_eq!(ex.taken.as_deref(), Some("2024-05-01 10:20:30"));
        assert!(ex.fields.iter().any(|f| f.source == "XMP"));
        let size = get(&ex, "mp4:video:size").unwrap();
        assert_eq!((size.value.as_str(), size.removable), ("320 x 240", false));
        assert_eq!(get(&ex, "mp4:video:codec").unwrap().value, "H.264 (AVC)");
        assert_eq!(get(&ex, "mp4:duration").unwrap().value, "0:02 (2.00 s)");
        assert!(ex.editable.contains(&EditKey::Title));
        // QuickTime flavour reads the same.
        let mov = t.file("b.mov", &sample_mov());
        let ex = read(&mov).unwrap();
        assert!(ex.gps.is_some());
        assert_eq!(get(&ex, "mp4:ftyp").unwrap().value, "qt");
    }

    #[test]
    fn clean_everything_in_place() {
        let t = Tmp::new("all");
        let src_bytes = sample();
        let src = t.file("a.mp4", &src_bytes);
        let dst = t.0.join("b.mp4");
        let opts = CleanOptions::everything();
        clean(&src, &dst, &opts).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), src_bytes, "source untouched");
        let out = std::fs::read(&dst).unwrap();
        assert_eq!(out.len(), src_bytes.len());
        assert_eq!(mdat(&out), mdat(&src_bytes));
        for needle in [
            &b"+36.1901"[..],
            b"ExamplePhone",
            b"EP-7",
            b"Test Person",
            b"ExampleCam",
        ] {
            assert!(
                !contains(&out, needle),
                "{}",
                String::from_utf8_lossy(needle)
            );
        }
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &opts).is_empty(),
            "{:?}",
            leftovers(&ex, &opts)
        );
        assert!(ex.gps.is_none());
        assert_eq!(get(&ex, "mp4:video:size").unwrap().value, "320 x 240");
        assert!(get(&ex, "mp4:duration").is_some());
        // lofty still understands the file (chunk offsets intact).
        let f = lofty::read_from_path(&dst).unwrap();
        use lofty::prelude::AudioFile;
        assert_eq!(f.properties().sample_rate(), Some(44100));
    }

    #[test]
    fn partial_clean_keeps_the_rest() {
        let t = Tmp::new("part");
        let src = t.file("a.mp4", &sample());
        let dst = t.0.join("b.mp4");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&src, &dst, &opts).unwrap();
        let ex = read(&dst).unwrap();
        assert!(leftovers(&ex, &opts).is_empty());
        assert!(ex.gps.is_none());
        assert!(get(&ex, "mp4:ilst:©ART").is_none());
        assert_eq!(get(&ex, "mp4:udta:©mod").unwrap().value, "EP-7");
        assert_eq!(get(&ex, "mp4:udta:©nam").unwrap().value, "Invented Clip");
        assert_eq!(get(&ex, "mp4:ilst:©cmt").unwrap().value, "Sample comment");
        assert!(get(&ex, "mp4:mvhd:created").is_some());
        let dates = CleanOptions::only(&[Category::Dates]);
        let dst2 = t.0.join("c.mp4");
        clean(&src, &dst2, &dates).unwrap();
        let ex = read(&dst2).unwrap();
        assert!(get(&ex, "mp4:mvhd:created").is_none() && get(&ex, "mp4:mvhd:modified").is_none());
        assert!(leftovers(&ex, &dates).is_empty());
    }

    #[test]
    fn edit_round_trip() {
        let t = Tmp::new("edit");
        let src = t.file("a.mp4", &sample());
        let dst = t.0.join("b.mp4");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "New Author"),
                Edit::set(EditKey::DateTaken, "2025-02-03 04:05:06"),
            ],
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        let of = |k: EditKey| -> Vec<String> {
            ex.fields
                .iter()
                .filter(|f| f.edit == Some(k))
                .map(|f| f.value.clone())
                .collect()
        };
        assert_eq!(of(EditKey::Title), [title]);
        assert_eq!(of(EditKey::Author), ["New Author"]);
        assert_eq!(of(EditKey::DateTaken), ["2025-02-03 04:05:06"]);
        assert_eq!(ex.taken.as_deref(), Some("2025-02-03 04:05:06"));
        // Untouched values survive, GPS included.
        assert!(ex.gps.is_some());
        assert_eq!(get(&ex, "mp4:ilst:©cmt").unwrap().value, "Sample comment");
        // The media data moved but is intact.
        assert_eq!(mdat(&std::fs::read(&dst).unwrap()), mdat(&sample()));
        // Removing a value.
        let dst2 = t.0.join("c.mp4");
        edit(&dst, &dst2, &[Edit::clear(EditKey::Title)]).unwrap();
        let ex = read(&dst2).unwrap();
        assert!(ex.fields.iter().all(|f| f.edit != Some(EditKey::Title)));
        assert!(edit(&src, &t.0.join("d.mp4"), &[Edit::set(EditKey::Gps, "1, 2")]).is_err());
    }

    #[test]
    fn m4a_sample_reads() {
        let t = Tmp::new("m4a");
        let p = t.file("a.m4a", &sample_m4a());
        let ex = read_as(&p, Kind::Audio).unwrap();
        assert_eq!(get(&ex, "mp4:ilst:©nam").unwrap().value, "Invented Song");
        assert_eq!(
            get(&ex, "mp4:ilst:covr").unwrap().value,
            "Cover picture (JPEG, 600 x 600)"
        );
        assert_eq!(get(&ex, "mp4:ilst:©day").unwrap().edit, Some(EditKey::Year));
        assert_eq!(ex.taken.as_deref(), Some("2024"));
    }

    #[test]
    fn broken_input_is_an_error() {
        let t = Tmp::new("bad");
        let good = sample();
        for (name, bytes) in [
            ("garbage.mp4", b"hello, this is not a video at all".to_vec()),
            ("empty.mp4", Vec::new()),
            ("trunc.mp4", good[..good.len() / 3].to_vec()),
            ("head.mp4", good[..40].to_vec()),
        ] {
            let p = t.file(name, &bytes);
            assert!(read(&p).is_err(), "{name}");
            assert!(
                clean(&p, &t.0.join("out.mp4"), &CleanOptions::everything()).is_err(),
                "{name}"
            );
        }
        assert_eq!(
            parse_iso6709("-33.8688+151.2093+020.000/").map(|g| g.lon),
            Some(151.2093)
        );
        assert!(parse_iso6709("nonsense").is_none());
    }
}
