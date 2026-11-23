//! WebP: the RIFF chunks EXIF, "XMP " and ICCP, and unknown chunks.
//! The VP8X header flags follow what is kept; a simple (VP8 / VP8L) file
//! that gains metadata gets a VP8X header sized from its bitstream. The
//! image chunks (VP8, VP8L, ALPH, ANIM, ANMF) are copied untouched.

use super::exif::{self, Exif};
use super::imgutil::{self, drops, drops_icc, finish, read_file, text_of, write_file};
use super::{icc, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use std::path::Path;

const F_ICC: u8 = 0x20;
const F_ALPHA: u8 = 0x10;
const F_EXIF: u8 = 0x08;
const F_XMP: u8 = 0x04;

#[derive(Clone, Debug, PartialEq)]
struct Chunk {
    id: [u8; 4],
    data: Vec<u8>,
}

fn parse(b: &[u8]) -> Result<(Vec<Chunk>, bool)> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return Err("This is not a WebP picture.".into());
    }
    let end = (imgutil::le32(b, 4).unwrap_or(0) as usize)
        .saturating_add(8)
        .min(b.len());
    let mut out = Vec::new();
    let mut p = 12;
    let mut complete = true;
    while p + 8 <= end {
        let id: [u8; 4] = b[p..p + 4].try_into().unwrap_or_default();
        let len = imgutil::le32(b, p + 4).unwrap_or(0) as usize;
        let Some(data) = b.get(p + 8..(p + 8).saturating_add(len)) else {
            complete = false;
            break;
        };
        out.push(Chunk {
            id,
            data: data.to_vec(),
        });
        p += 8 + len + (len & 1);
    }
    if out.is_empty() {
        return Err("The WebP file is cut short.".into());
    }
    Ok((out, complete))
}

fn write(chunks: &[Chunk]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    for c in chunks {
        body.extend(c.id);
        body.extend((c.data.len() as u32).to_le_bytes());
        body.extend(&c.data);
        if c.data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

/// Canvas size and alpha from the VP8 / VP8L bitstream.
fn bitstream_size(chunks: &[Chunk]) -> Option<(u32, u32, bool)> {
    let alph = chunks.iter().any(|c| &c.id == b"ALPH");
    chunks.iter().find_map(|c| match &c.id {
        b"VP8L" if c.data.first() == Some(&0x2F) => {
            let v = imgutil::le32(&c.data, 1)?;
            Some((
                (v & 0x3FFF) + 1,
                ((v >> 14) & 0x3FFF) + 1,
                (v >> 28) & 1 == 1,
            ))
        }
        b"VP8 " if c.data.get(3..6) == Some(&[0x9D, 0x01, 0x2A]) => {
            let w = imgutil::le16(&c.data, 6)? as u32 & 0x3FFF;
            let h = imgutil::le16(&c.data, 8)? as u32 & 0x3FFF;
            Some((w, h, alph))
        }
        _ => None,
    })
}

fn canvas(chunks: &[Chunk]) -> Option<(u32, u32)> {
    match chunks.iter().find(|c| &c.id == b"VP8X") {
        Some(x) if x.data.len() >= 10 => {
            let w = u32::from_le_bytes([x.data[4], x.data[5], x.data[6], 0]) + 1;
            let h = u32::from_le_bytes([x.data[7], x.data[8], x.data[9], 0]) + 1;
            Some((w, h))
        }
        _ => bitstream_size(chunks).map(|(w, h, _)| (w, h)),
    }
}

const IMAGE: &[&[u8; 4]] = &[b"VP8 ", b"VP8L", b"VP8X", b"ALPH", b"ANIM", b"ANMF"];

fn is_unknown(c: &Chunk) -> bool {
    !IMAGE.contains(&&c.id) && !matches!(&c.id, b"EXIF" | b"XMP " | b"ICCP")
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
    if let Some((w, h)) = canvas(chunks) {
        ex.fields.push(imgutil::dims_field(w as u64, h as u64));
    }
    for c in chunks {
        match &c.id {
            b"EXIF" => ex.fields.extend(exif::fields_of(&c.data)),
            b"XMP " => ex.fields.extend(xmp::fields(&text_of(&c.data), "XMP")),
            b"ICCP" => ex.fields.extend(icc::fields(&c.data)),
            _ if is_unknown(c) => {
                let name = String::from_utf8_lossy(&c.id).trim().to_string();
                ex.fields.push(
                    Field::new(
                        format!("webp:{name}"),
                        "Unknown data block",
                        format!("'{name}' chunk, {} bytes", c.data.len()),
                        Category::Hidden,
                        "WebP",
                    )
                    .flag(Flag::Hidden),
                );
            }
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

/// Orders the chunks (VP8X, ICCP, image, EXIF, XMP) and sets the VP8X
/// flags; adds a VP8X header when metadata needs one.
fn finalize(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let has = |id: &[u8; 4]| chunks.iter().any(|c| &c.id == id);
    let needs_x = has(b"VP8X") || has(b"EXIF") || has(b"XMP ") || has(b"ICCP");
    let mut out = Vec::new();
    if needs_x {
        let mut x = match chunks.iter().find(|c| &c.id == b"VP8X") {
            Some(c) if c.data.len() >= 10 => c.clone(),
            _ => {
                let (w, h, alpha) = bitstream_size(&chunks).unwrap_or((1, 1, false));
                let mut d = vec![if alpha { F_ALPHA } else { 0 }, 0, 0, 0];
                d.extend(&(w.max(1) - 1).to_le_bytes()[..3]);
                d.extend(&(h.max(1) - 1).to_le_bytes()[..3]);
                Chunk {
                    id: *b"VP8X",
                    data: d,
                }
            }
        };
        let mut f = x.data[0] & !(F_ICC | F_EXIF | F_XMP);
        for (id, bit) in [(b"ICCP", F_ICC), (b"EXIF", F_EXIF), (b"XMP ", F_XMP)] {
            if has(id) {
                f |= bit;
            }
        }
        x.data[0] = f;
        out.push(x);
    }
    let pick = |id: &'static [u8; 4]| chunks.iter().filter(move |c| &c.id == id).cloned();
    out.extend(pick(b"ICCP"));
    out.extend(
        chunks
            .iter()
            .filter(|c| !matches!(&c.id, b"VP8X" | b"ICCP" | b"EXIF" | b"XMP "))
            .cloned(),
    );
    out.extend(pick(b"EXIF"));
    out.extend(pick(b"XMP "));
    out
}

fn clean_chunks(chunks: &[Chunk], opts: &CleanOptions) -> Vec<Chunk> {
    let kept = chunks
        .iter()
        .filter_map(|c| {
            let data = match &c.id {
                b"EXIF" => exif::strip_block(&c.data, opts)?,
                b"XMP " => xmp::strip(&text_of(&c.data), opts)?.into_bytes(),
                b"ICCP" if drops_icc(opts) => return None,
                _ if is_unknown(c) && drops(opts, Category::Hidden) => return None,
                _ => c.data.clone(),
            };
            Some(Chunk { id: c.id, data })
        })
        .collect();
    finalize(kept)
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let b = read_file(src)?;
    let (chunks, _) = parse(&b)?;
    write_file(dst, &write(&clean_chunks(&chunks, opts)))
}

fn edit_chunks(mut chunks: Vec<Chunk>, edits: &[Edit]) -> Result<Vec<Chunk>> {
    let any_set = edits
        .iter()
        .any(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()));
    let ei = chunks.iter().position(|c| &c.id == b"EXIF");
    let mut x = ei
        .and_then(|i| Exif::parse(&chunks[i].data))
        .unwrap_or_else(|| Exif::new(true));
    x.apply_edits(edits)?;
    chunks.retain(|c| &c.id != b"EXIF");
    if !x.is_empty() {
        chunks.push(Chunk {
            id: *b"EXIF",
            data: x.to_bytes(),
        });
    }
    let xi = chunks.iter().position(|c| &c.id == b"XMP ");
    let old = xi.map(|i| text_of(&chunks[i].data));
    if old.is_some() || any_set {
        let packet = xmp::apply_edits(old.as_deref(), edits)?;
        chunks.retain(|c| &c.id != b"XMP ");
        chunks.push(Chunk {
            id: *b"XMP ",
            data: packet.into_bytes(),
        });
    }
    Ok(finalize(chunks))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let b = read_file(src)?;
    let (chunks, _) = parse(&b)?;
    write_file(dst, &write(&edit_chunks(chunks, edits)?))
}

/// A small lossless WebP with known invented metadata.
pub fn sample() -> Vec<u8> {
    let base = imgutil::encode_sample(image::ImageFormat::WebP);
    let (mut chunks, _) = parse(&base).expect("the encoder writes a valid WebP");
    chunks.push(Chunk {
        id: *b"ICCP",
        data: icc::sample(),
    });
    chunks.push(Chunk {
        id: *b"EXIF",
        data: exif::sample(None).to_bytes(),
    });
    chunks.push(Chunk {
        id: *b"XMP ",
        data: xmp::sample().into_bytes(),
    });
    chunks.push(Chunk {
        id: *b"PRIV",
        data: b"odd-size vendor data".to_vec(),
    });
    write(&finalize(chunks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;

    fn image_data(b: &[u8]) -> Vec<Chunk> {
        parse(b)
            .unwrap()
            .0
            .into_iter()
            .filter(|c| matches!(&c.id, b"VP8 " | b"VP8L" | b"ALPH" | b"ANMF"))
            .collect()
    }

    fn flags(b: &[u8]) -> Option<u8> {
        parse(b)
            .unwrap()
            .0
            .iter()
            .find(|c| &c.id == b"VP8X")
            .map(|c| c.data[0])
    }

    #[test]
    fn reads_everything() {
        let t = Tmp::new("webp-read");
        let b = sample();
        let ex = read(&t.file("a.webp", &b)).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        assert!(has(&ex, "webp:PRIV"));
        assert!(ex.gps.is_some());
        assert_eq!(flags(&b), Some(F_ICC | F_EXIF | F_XMP));
        image::load_from_memory(&b).unwrap();
    }

    #[test]
    fn clean_is_lossless_and_fixes_flags() {
        clean_suite("webp-clean", &sample(), "webp", read, clean);
        let t = Tmp::new("webp-lossless");
        let src = t.file("a.webp", &sample());
        let dst = t.path("b.webp");
        let mut o = CleanOptions::everything();
        o.keep_orientation = false;
        clean(&src, &dst, &o).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(image_data(&a), image_data(&b));
        same_pixels(&a, &b);
        assert_eq!(flags(&b), Some(F_ICC));
        o.keep_color_profile = false;
        clean(&src, &dst, &o).unwrap();
        let b = std::fs::read(&dst).unwrap();
        assert_eq!(flags(&b), Some(0));
        assert_eq!(read(&dst).unwrap().fields.len(), 1);
        same_pixels(&a, &b);
    }

    #[test]
    fn partial_clean_keeps_camera_and_dates() {
        let t = Tmp::new("webp-partial");
        let src = t.file("a.webp", &sample());
        let dst = t.path("b.webp");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none() && !has(&ex, "xmp:dc:creator"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
    }

    #[test]
    fn edit_round_trip_and_vp8x_creation() {
        edit_suite("webp-edit", &sample(), "webp", read, edit);
        let t = Tmp::new("webp-simple");
        let bare = imgutil::encode_sample(image::ImageFormat::WebP);
        assert!(flags(&bare).is_none());
        let src = t.file("a.webp", &bare);
        let dst = t.path("b.webp");
        edit(&src, &dst, &[Edit::set(EditKey::Author, "Test Author")]).unwrap();
        let b = std::fs::read(&dst).unwrap();
        assert_eq!(flags(&b), Some(F_EXIF | F_XMP));
        assert_eq!(canvas(&parse(&b).unwrap().0), Some((64, 48)));
        same_pixels(&bare, &b);
        assert_eq!(
            value(&read(&dst).unwrap(), "exif:0x013B"),
            Some("Test Author")
        );
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("webp-bad", &sample(), "webp", read, clean);
    }
}
