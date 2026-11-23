//! PNG: text chunks (tEXt, zTXt, iTXt, including the XMP packet and the
//! ImageMagick "Raw profile" hex blocks), eXIf, iCCP, tIME and private
//! ancillary chunks. Image chunks (IHDR, PLTE, IDAT, APNG frames, ...) are
//! copied untouched; every written chunk gets a fresh CRC.

use super::exif::{self, Exif};
use super::imgutil::{self, drops, drops_icc, finish, read_file, text_of, write_file};
use super::{icc, iptc, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use crate::util::DateTime;
use std::io::{Read, Write};
use std::path::Path;

const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
const XMP_KEY: &str = "XML:com.adobe.xmp";
const SOURCE: &str = "PNG text";
/// Decompressed text larger than this is not read.
const MAX_INFLATE: u64 = 32 << 20;

#[derive(Clone, Debug, PartialEq)]
struct Chunk {
    typ: [u8; 4],
    data: Vec<u8>,
}

impl Chunk {
    fn new(typ: &[u8; 4], data: Vec<u8>) -> Chunk {
        Chunk { typ: *typ, data }
    }
    fn is(&self, t: &[u8; 4]) -> bool {
        &self.typ == t
    }
}

/// The chunks, and whether the file ended properly with IEND.
fn parse(b: &[u8]) -> Result<(Vec<Chunk>, bool)> {
    if !b.starts_with(SIG) {
        return Err("This is not a PNG picture.".into());
    }
    let mut out = Vec::new();
    let mut p = SIG.len();
    while p + 12 <= b.len() {
        let len = imgutil::be32(b, p).unwrap_or(u32::MAX) as usize;
        let typ: [u8; 4] = b[p + 4..p + 8].try_into().unwrap_or_default();
        let Some(data) = b.get(p + 8..(p + 8).saturating_add(len)) else {
            break;
        };
        if p + 12 + len > b.len() {
            break;
        }
        out.push(Chunk {
            typ,
            data: data.to_vec(),
        });
        p += 12 + len;
        if &typ == b"IEND" {
            return Ok((out, true));
        }
    }
    if out.is_empty() {
        return Err("The PNG file is cut short.".into());
    }
    Ok((out, false))
}

fn write(chunks: &[Chunk]) -> Vec<u8> {
    let mut out = SIG.to_vec();
    for c in chunks {
        out.extend((c.data.len() as u32).to_be_bytes());
        out.extend(c.typ);
        out.extend(&c.data);
        let mut h = crc32fast::Hasher::new();
        h.update(&c.typ);
        h.update(&c.data);
        out.extend(h.finalize().to_be_bytes());
    }
    out
}

fn inflate(d: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(d)
        .take(MAX_INFLATE)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn deflate(d: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = e.write_all(d);
    e.finish().unwrap_or_default()
}

fn latin1_text(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// (keyword, text) of a tEXt, zTXt or iTXt chunk.
fn text_chunk(c: &Chunk) -> Option<(String, String)> {
    let nul = c.data.iter().position(|&b| b == 0)?;
    let key = latin1_text(&c.data[..nul]);
    let rest = &c.data[nul + 1..];
    let text = match &c.typ {
        b"tEXt" => latin1_text(rest),
        b"zTXt" => latin1_text(&inflate(rest.get(1..)?)?),
        b"iTXt" => {
            let compressed = *rest.first()? == 1;
            let after = rest.get(2..)?;
            let l = after.iter().position(|&b| b == 0)?;
            let after = &after[l + 1..];
            let t = after.iter().position(|&b| b == 0)?;
            let body = &after[t + 1..];
            let body = if compressed {
                inflate(body)?
            } else {
                body.to_vec()
            };
            String::from_utf8_lossy(&body).into_owned()
        }
        _ => return None,
    };
    Some((key, text))
}

fn is_text(c: &Chunk) -> bool {
    c.is(b"tEXt") || c.is(b"zTXt") || c.is(b"iTXt")
}

/// An uncompressed iTXt chunk (UTF-8).
fn itxt(key: &str, text: &str) -> Chunk {
    let mut d = key.as_bytes().to_vec();
    d.extend([0, 0, 0, 0, 0]);
    d.extend(text.as_bytes());
    Chunk::new(b"iTXt", d)
}

fn text_spec(key: &str) -> (String, Category, Option<Flag>, Option<EditKey>) {
    use Category::*;
    let (label, cat, flag, edit): (&str, _, _, _) = match key {
        "Title" => ("Title", Description, None, Some(EditKey::Title)),
        "Author" => ("Author", People, Some(Flag::Names), Some(EditKey::Author)),
        "Description" => (
            "Description",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        ),
        "Comment" => (
            "Comment",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        ),
        "Copyright" => (
            "Copyright",
            People,
            Some(Flag::Names),
            Some(EditKey::Copyright),
        ),
        "Creation Time" => ("Date created", Dates, None, Some(EditKey::DateTaken)),
        "Software" => ("Program used", Software, None, None),
        "Disclaimer" => ("Disclaimer", Description, None, None),
        "Warning" => ("Warning", Description, None, None),
        "Source" => ("Device used", Device, None, None),
        "Keywords" => ("Keywords", Description, None, Some(EditKey::Keywords)),
        "date:create" => ("Date created (ImageMagick)", Dates, None, None),
        "date:modify" => ("Date modified (ImageMagick)", Dates, None, None),
        "date:timestamp" => ("Date saved (ImageMagick)", Dates, None, None),
        _ => {
            return (
                format!("Custom text ({key})"),
                Hidden,
                Some(Flag::Hidden),
                None,
            );
        }
    };
    (label.to_string(), cat, flag, edit)
}

/// Decodes an ImageMagick "Raw profile type ..." hex block.
fn raw_profile(text: &str) -> Option<Vec<u8>> {
    let mut it = text.split_whitespace();
    let _name = it.next()?;
    let len: usize = it.next()?.parse().ok()?;
    let hex: String = it.collect();
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    (bytes.len() >= len).then(|| bytes[..len].to_vec())
}

fn raw_profile_text(name: &str, bytes: &[u8]) -> String {
    let mut s = format!("\n{name}\n{:8}\n", bytes.len());
    for line in bytes.chunks(36) {
        for b in line {
            s.push_str(&format!("{b:02x}"));
        }
        s.push('\n');
    }
    s
}

const KNOWN: &[&[u8; 4]] = &[
    b"cHRM", b"gAMA", b"iCCP", b"sBIT", b"sRGB", b"cICP", b"mDCV", b"mDCv", b"cLLI", b"cLLi",
    b"bKGD", b"hIST", b"tRNS", b"pHYs", b"sPLT", b"acTL", b"fcTL", b"fdAT", b"tEXt", b"zTXt",
    b"iTXt", b"tIME", b"eXIf", b"oFFs", b"pCAL", b"sCAL", b"sTER", b"gIFg", b"gIFx",
];

/// An ancillary chunk that is not part of the PNG standard.
fn is_private(c: &Chunk) -> bool {
    c.typ[0].is_ascii_lowercase() && !KNOWN.contains(&&c.typ)
}

fn extract(chunks: &[Chunk], complete: bool) -> Extract {
    let mut ex = Extract {
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Copyright,
            EditKey::Description,
            EditKey::Keywords,
            EditKey::DateTaken,
            EditKey::Gps,
        ],
        ..Extract::default()
    };
    if !complete {
        ex.notes
            .push("The file seems cut short; only its first part was read.".into());
    }
    let f = &mut ex.fields;
    for c in chunks {
        match &c.typ {
            b"IHDR" => {
                if let (Some(w), Some(h)) = (imgutil::be32(&c.data, 0), imgutil::be32(&c.data, 4)) {
                    f.push(imgutil::dims_field(w as u64, h as u64));
                }
            }
            b"eXIf" => f.extend(exif::fields_of(&c.data)),
            b"iCCP" => {
                let p = c
                    .data
                    .iter()
                    .position(|&b| b == 0)
                    .and_then(|n| inflate(c.data.get(n + 2..)?));
                match p {
                    Some(p) => f.extend(icc::fields(&p)),
                    None => f.push(Field::new(
                        "icc:desc",
                        "Colour profile",
                        "Embedded profile",
                        Category::Technical,
                        "ICC profile",
                    )),
                }
            }
            b"tIME" if c.data.len() >= 7 => {
                let d = &c.data;
                let v = format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    u16::from_be_bytes([d[0], d[1]]),
                    d[2],
                    d[3],
                    d[4],
                    d[5],
                    d[6]
                );
                f.push(Field::new(
                    "png:tIME",
                    "Last modified",
                    v,
                    Category::Dates,
                    "PNG",
                ));
            }
            _ if is_text(c) => {
                let Some((key, text)) = text_chunk(c) else {
                    continue;
                };
                let typ = latin1_text(&c.typ);
                if key == XMP_KEY {
                    f.extend(xmp::fields(&text, "XMP"));
                } else if let Some(kind) = key.strip_prefix("Raw profile type ") {
                    let bytes = raw_profile(&text).unwrap_or_default();
                    match kind {
                        "exif" | "APP1" => f.extend(exif::fields_of(&bytes)),
                        "iptc" | "8bim" => f.extend(iptc::irb_fields(&bytes)),
                        "icc" | "icm" => f.extend(icc::fields(&bytes)),
                        "xmp" => f.extend(xmp::fields(&text_of(&bytes), "XMP")),
                        other => f.push(
                            Field::new(
                                format!("png:{typ}:{key}"),
                                format!("Raw {other} data"),
                                format!("{} bytes", bytes.len()),
                                Category::Hidden,
                                SOURCE,
                            )
                            .flag(Flag::Hidden),
                        ),
                    }
                } else if !text.trim().is_empty() {
                    let (label, cat, flag, edit) = text_spec(&key);
                    let value = if cat == Category::Dates {
                        imgutil::nice_date(&text)
                    } else {
                        text.trim().to_string()
                    };
                    let mut fl = Field::new(format!("png:{typ}:{key}"), label, value, cat, SOURCE);
                    fl.flag = flag;
                    fl.edit = edit;
                    f.push(fl);
                }
            }
            _ if is_private(c) => f.push(
                Field::new(
                    format!("png:{}", latin1_text(&c.typ)),
                    "Private data block",
                    format!("'{}' chunk, {} bytes", latin1_text(&c.typ), c.data.len()),
                    Category::Hidden,
                    "PNG",
                )
                .flag(Flag::Hidden),
            ),
            _ => {}
        }
    }
    finish(&mut ex);
    ex
}

pub fn read(path: &Path) -> Result<Extract> {
    let b = read_file(path)?;
    let (chunks, complete) = parse(&b)?;
    Ok(extract(&chunks, complete))
}

/// A chunk after cleaning (`None` drops it).
fn clean_chunk(c: &Chunk, opts: &CleanOptions) -> Option<Chunk> {
    if c.is(b"eXIf") {
        return exif::strip_block(&c.data, opts).map(|b| Chunk::new(b"eXIf", b));
    }
    if c.is(b"iCCP") {
        return (!drops_icc(opts)).then(|| c.clone());
    }
    if c.is(b"tIME") {
        return (!drops(opts, Category::Dates)).then(|| c.clone());
    }
    if is_private(c) && !is_text(c) {
        return (!drops(opts, Category::Hidden)).then(|| c.clone());
    }
    if !is_text(c) {
        return Some(c.clone());
    }
    let Some((key, text)) = text_chunk(c) else {
        return (!drops(opts, Category::Hidden)).then(|| c.clone());
    };
    if key == XMP_KEY {
        return xmp::strip(&text, opts).map(|x| itxt(XMP_KEY, &x));
    }
    if let Some(kind) = key.strip_prefix("Raw profile type ") {
        let bytes = raw_profile(&text)?;
        let new = match kind {
            "exif" | "APP1" => exif::strip_block(&bytes, opts),
            "iptc" | "8bim" => iptc::strip_irb(&bytes, opts),
            "icc" | "icm" => (!drops_icc(opts)).then_some(bytes),
            "xmp" => xmp::strip(&text_of(&bytes), opts).map(String::into_bytes),
            _ => (!drops(opts, Category::Hidden)).then_some(bytes),
        }?;
        let mut d = key.as_bytes().to_vec();
        d.push(0);
        d.extend(raw_profile_text(kind, &new).as_bytes());
        return Some(Chunk::new(b"tEXt", d));
    }
    let (_, cat, _, _) = text_spec(&key);
    (!drops(opts, cat)).then(|| c.clone())
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let b = read_file(src)?;
    let (chunks, _) = parse(&b)?;
    let out: Vec<Chunk> = chunks.iter().filter_map(|c| clean_chunk(c, opts)).collect();
    write_file(dst, &write(&out))
}

fn edit_chunks(mut chunks: Vec<Chunk>, edits: &[Edit]) -> Result<Vec<Chunk>> {
    let keys: Vec<EditKey> = edits.iter().map(|e| e.key).collect();
    let any_set = edits
        .iter()
        .any(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()));
    let mut new = Vec::new();
    // XMP: patch the packet, or start one when something is set.
    let xmp_i = chunks
        .iter()
        .position(|c| is_text(c) && text_chunk(c).is_some_and(|(k, _)| k == XMP_KEY));
    let old = xmp_i.and_then(|i| text_chunk(&chunks[i])).map(|(_, t)| t);
    if old.is_some() || any_set {
        let x = xmp::apply_edits(old.as_deref(), edits)?;
        match xmp_i {
            Some(i) => chunks[i] = itxt(XMP_KEY, &x),
            None => new.push(itxt(XMP_KEY, &x)),
        }
    }
    // EXIF: patch when present.
    if let Some(i) = chunks.iter().position(|c| c.is(b"eXIf")) {
        let mut x = Exif::parse(&chunks[i].data).unwrap_or_else(|| Exif::new(true));
        x.apply_edits(edits)?;
        if x.is_empty() {
            chunks.remove(i);
        } else {
            chunks[i].data = x.to_bytes();
        }
    }
    // Text chunks: drop every one the keys edit, then write iTXt.
    chunks.retain(|c| {
        if !is_text(c) {
            return true;
        }
        match text_chunk(c) {
            Some((k, _)) if k != XMP_KEY => text_spec(&k).3.is_none_or(|e| !keys.contains(&e)),
            _ => true,
        }
    });
    for e in edits {
        let Some(v) = e.value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        let key = match e.key {
            EditKey::Title => "Title",
            EditKey::Author => "Author",
            EditKey::Copyright => "Copyright",
            EditKey::Description => "Description",
            EditKey::DateTaken => {
                let d = DateTime::parse(v)
                    .ok_or_else(|| format!("\"{v}\" is not a date. Use YYYY-MM-DD HH:MM:SS."))?;
                new.push(itxt("Creation Time", &d.text()));
                continue;
            }
            _ => continue,
        };
        new.push(itxt(key, v));
    }
    let at = chunks
        .iter()
        .position(|c| c.is(b"IDAT") || c.is(b"IEND"))
        .unwrap_or(chunks.len());
    chunks.splice(at..at, new);
    Ok(chunks)
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let b = read_file(src)?;
    let (chunks, _) = parse(&b)?;
    write_file(dst, &write(&edit_chunks(chunks, edits)?))
}

fn text(key: &str, value: &str) -> Chunk {
    let mut d = key.as_bytes().to_vec();
    d.push(0);
    d.extend(imgutil::latin1(value).unwrap_or_default());
    Chunk::new(b"tEXt", d)
}

/// A small PNG with known invented metadata.
pub fn sample() -> Vec<u8> {
    use imgutil::sample_values as s;
    let base = imgutil::encode_sample(image::ImageFormat::Png);
    let (mut chunks, _) = parse(&base).expect("the encoder writes a valid PNG");
    let mut iccp = b"Test RGB\0\0".to_vec();
    iccp.extend(deflate(&icc::sample()));
    let mut ztxt = b"Description\0\0".to_vec();
    ztxt.extend(deflate(s::DESCRIPTION.as_bytes()));
    let new = vec![
        Chunk::new(b"iCCP", iccp),
        Chunk::new(b"eXIf", exif::sample(None).to_bytes()),
        text("Author", s::ARTIST),
        text("Copyright", s::COPYRIGHT),
        text("Software", s::SOFTWARE),
        text("Creation Time", "2024-05-01 10:20:30"),
        Chunk::new(b"zTXt", ztxt),
        itxt("Title", s::TITLE_KU),
        itxt(XMP_KEY, &xmp::sample()),
        Chunk::new(b"tIME", vec![0x07, 0xE8, 5, 2, 8, 0, 0]),
        Chunk::new(b"prVt", b"private vendor data".to_vec()),
    ];
    let at = chunks.iter().position(|c| c.is(b"IDAT")).unwrap_or(1);
    chunks.splice(at..at, new);
    write(&chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;

    fn idat(b: &[u8]) -> Vec<u8> {
        parse(b)
            .unwrap()
            .0
            .into_iter()
            .filter(|c| c.is(b"IDAT"))
            .flat_map(|c| c.data)
            .collect()
    }

    #[test]
    fn reads_everything() {
        let t = Tmp::new("png-read");
        let ex = read(&t.file("a.png", &sample())).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "png:tEXt:Author"), Some(s::ARTIST));
        assert_eq!(value(&ex, "png:zTXt:Description"), Some(s::DESCRIPTION));
        assert_eq!(value(&ex, "png:iTXt:Title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "exif:0xA431"), Some(s::SERIAL));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        assert_eq!(value(&ex, "png:tIME"), Some("2024-05-02 08:00:00"));
        assert!(has(&ex, "png:prVt"));
        assert!(ex.gps.is_some());
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
    }

    #[test]
    fn clean_is_lossless_and_complete() {
        clean_suite("png-clean", &sample(), "png", read, clean);
        let t = Tmp::new("png-lossless");
        let src = t.file("a.png", &sample());
        let dst = t.path("b.png");
        clean(&src, &dst, &CleanOptions::everything()).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(idat(&a), idat(&b));
        same_pixels(&a, &b);
        let ex = read(&dst).unwrap();
        assert_eq!(value(&ex, "exif:0x0112"), Some("Rotate 90° clockwise"));
        assert!(has(&ex, "icc:desc"));
        assert!(!has(&ex, "png:prVt") && !has(&ex, "png:tEXt:Author"));
    }

    #[test]
    fn partial_clean_and_keep_options() {
        let t = Tmp::new("png-partial");
        let src = t.file("a.png", &sample());
        let dst = t.path("b.png");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none() && !has(&ex, "png:tEXt:Author") && !has(&ex, "exif:0x013B"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "png:tEXt:Software"), Some(s::SOFTWARE));
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));

        let mut o = CleanOptions::everything();
        o.keep_color_profile = false;
        clean(&src, &dst, &o).unwrap();
        let ex = read(&dst).unwrap();
        assert!(!has(&ex, "icc:desc") && has(&ex, "exif:0x0112"));
        same_pixels(&std::fs::read(&src).unwrap(), &std::fs::read(&dst).unwrap());
    }

    #[test]
    fn edit_round_trip() {
        edit_suite("png-edit", &sample(), "png", read, edit);
        let t = Tmp::new("png-edit-bare");
        let src = t.file("a.png", &imgutil::encode_sample(image::ImageFormat::Png));
        let dst = t.path("b.png");
        edit(&src, &dst, &[Edit::set(EditKey::Title, s::TITLE_KU)]).unwrap();
        assert_eq!(
            value(&read(&dst).unwrap(), "png:iTXt:Title"),
            Some(s::TITLE_KU)
        );
        same_pixels(&std::fs::read(&src).unwrap(), &std::fs::read(&dst).unwrap());
    }

    #[test]
    fn raw_profile_hex_round_trip() {
        let blob = exif::sample(None).to_bytes();
        let txt = raw_profile_text("exif", &blob);
        assert_eq!(raw_profile(&txt).unwrap(), blob);
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("png-bad", &sample(), "png", read, clean);
    }
}
