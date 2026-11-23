//! GIF: comment extensions, the XMP application extension (with its
//! "magic trailer"), the ICC profile extension and other application
//! extensions. Looping (NETSCAPE2.0 / ANIMEXTS1.0), graphic control and
//! image blocks are copied untouched.

use super::imgutil::{self, drops, drops_icc, finish, read_file, text_of, write_file};
use super::{icc, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use std::path::Path;

const XMP_APP: &[u8; 11] = b"XMP DataXMP";
const ICC_APP: &[u8; 11] = b"ICCRGBG1012";

#[derive(Clone, Debug, PartialEq)]
enum Block {
    /// An extension: label and its whole raw bytes (0x21 ... terminator).
    Ext(u8, Vec<u8>),
    /// An image: descriptor, colour table and LZW data, raw.
    Image(Vec<u8>),
}

#[derive(Clone, Debug)]
struct Gif {
    head: Vec<u8>,
    blocks: Vec<Block>,
    /// Bytes after the trailer.
    after: Vec<u8>,
    complete: bool,
}

/// The end of a sub-block chain starting at `p` (after its terminator).
fn skip_sub_blocks(b: &[u8], mut p: usize) -> Option<usize> {
    loop {
        let n = *b.get(p)? as usize;
        p += 1;
        if n == 0 {
            return Some(p);
        }
        p += n;
    }
}

fn sub_block_data(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut p = 0;
    while let Some(&n) = b.get(p) {
        if n == 0 {
            break;
        }
        out.extend(b.get(p + 1..p + 1 + n as usize).unwrap_or(&b[p + 1..]));
        p += 1 + n as usize;
    }
    out
}

fn sub_blocks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in data.chunks(255) {
        out.push(c.len() as u8);
        out.extend(c);
    }
    out.push(0);
    out
}

fn parse(b: &[u8]) -> Result<Gif> {
    if !(b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) || b.len() < 13 {
        return Err("This is not a GIF picture.".into());
    }
    let mut p = 13;
    if b[10] & 0x80 != 0 {
        p += 3 << ((b[10] & 7) + 1);
    }
    if p > b.len() {
        return Err("The GIF file is cut short.".into());
    }
    let mut g = Gif {
        head: b[..p].to_vec(),
        blocks: Vec::new(),
        after: Vec::new(),
        complete: false,
    };
    while let Some(&intro) = b.get(p) {
        match intro {
            0x3B => {
                g.after = b[p + 1..].to_vec();
                g.complete = true;
                break;
            }
            0x21 => {
                let Some(&label) = b.get(p + 1) else { break };
                let Some(end) = skip_sub_blocks(b, p + 2) else {
                    break;
                };
                g.blocks.push(Block::Ext(label, b[p..end].to_vec()));
                p = end;
            }
            0x2C => {
                let Some(&flags) = b.get(p + 9) else { break };
                let mut q = p + 10;
                if flags & 0x80 != 0 {
                    q += 3 << ((flags & 7) + 1);
                }
                let Some(end) = skip_sub_blocks(b, q + 1) else {
                    break;
                };
                g.blocks.push(Block::Image(b[p..end].to_vec()));
                p = end;
            }
            _ => break,
        }
    }
    Ok(g)
}

impl Gif {
    fn write(&self, after: bool) -> Vec<u8> {
        let mut out = self.head.clone();
        for b in &self.blocks {
            match b {
                Block::Ext(_, raw) | Block::Image(raw) => out.extend(raw),
            }
        }
        out.push(0x3B);
        if after {
            out.extend(&self.after);
        }
        out
    }
}

/// The application identifier of an application extension.
fn app_id(raw: &[u8]) -> Option<&[u8]> {
    (raw.get(2) == Some(&11)).then(|| raw.get(3..14)).flatten()
}

/// The XMP text of an "XMP DataXMP" extension.
fn xmp_text(raw: &[u8]) -> String {
    let body = raw.get(14..raw.len().saturating_sub(1)).unwrap_or_default();
    let end = body
        .windows(3)
        .position(|w| w == [0x01, 0xFF, 0xFE])
        .unwrap_or(body.len());
    text_of(&body[..end])
}

fn xmp_ext(packet: &str) -> Vec<u8> {
    let mut out = vec![0x21, 0xFF, 11];
    out.extend(XMP_APP);
    out.extend(packet.as_bytes());
    out.push(0x01);
    out.extend((0..=255u8).rev());
    out.push(0);
    out
}

fn comment_ext(text: &str) -> Vec<u8> {
    let mut out = vec![0x21, 0xFE];
    out.extend(sub_blocks(text.as_bytes()));
    out
}

fn app_ext(id: &[u8; 11], data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x21, 0xFF, 11];
    out.extend(id);
    out.extend(sub_blocks(data));
    out
}

#[derive(PartialEq)]
enum Kind {
    Comment,
    Xmp,
    Icc,
    Looping,
    OtherApp,
    Keep,
}

fn kind(b: &Block) -> Kind {
    match b {
        Block::Ext(0xFE, _) => Kind::Comment,
        Block::Ext(0xFF, raw) => match app_id(raw) {
            Some(a) if a == XMP_APP => Kind::Xmp,
            Some(a) if a == ICC_APP => Kind::Icc,
            Some(a) if a == b"NETSCAPE2.0" || a == b"ANIMEXTS1.0" => Kind::Looping,
            _ => Kind::OtherApp,
        },
        _ => Kind::Keep,
    }
}

fn extract(g: &Gif) -> Extract {
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
    if !g.complete {
        ex.notes
            .push("The file seems cut short; only its first part was read.".into());
    }
    if let (Some(w), Some(h)) = (imgutil::le16(&g.head, 6), imgutil::le16(&g.head, 8)) {
        ex.fields.push(imgutil::dims_field(w as u64, h as u64));
    }
    let mut comments = Vec::new();
    for b in &g.blocks {
        let Block::Ext(_, raw) = b else { continue };
        match kind(b) {
            Kind::Comment => {
                let t = text_of(&sub_block_data(&raw[2..])).trim().to_string();
                if !t.is_empty() {
                    comments.push(t);
                }
            }
            Kind::Xmp => ex.fields.extend(xmp::fields(&xmp_text(raw), "XMP")),
            Kind::Icc => ex.fields.extend(icc::fields(&sub_block_data(&raw[14..]))),
            Kind::OtherApp => {
                let name = app_id(raw)
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .unwrap_or_default();
                ex.fields.push(
                    Field::new(
                        format!("gif:app:{name}"),
                        "Application data",
                        format!("'{}' block, {} bytes", name.trim(), raw.len()),
                        Category::Hidden,
                        "GIF",
                    )
                    .flag(Flag::Hidden),
                );
            }
            Kind::Looping | Kind::Keep => {}
        }
    }
    if !comments.is_empty() {
        ex.fields.push(
            Field::new(
                "gif:comment",
                "Comment",
                comments.join("; "),
                Category::Description,
                "GIF comment",
            )
            .flag(Flag::Comments)
            .edit(EditKey::Description),
        );
    }
    if g.after.iter().any(|&b| b != 0) {
        ex.fields.push(
            Field::new(
                "gif:trailer",
                "Data after the picture",
                format!("{} bytes", g.after.len()),
                Category::Hidden,
                "GIF",
            )
            .flag(Flag::Hidden),
        );
    }
    finish(&mut ex);
    ex
}

pub fn read(path: &Path) -> Result<Extract> {
    let b = read_file(path)?;
    Ok(extract(&parse(&b)?))
}

fn clean_gif(g: &Gif, opts: &CleanOptions) -> Vec<u8> {
    let mut out = g.clone();
    out.blocks = g
        .blocks
        .iter()
        .filter_map(|b| match kind(b) {
            Kind::Comment if drops(opts, Category::Description) => None,
            Kind::Xmp => {
                let Block::Ext(_, raw) = b else { return None };
                xmp::strip(&xmp_text(raw), opts).map(|x| Block::Ext(0xFF, xmp_ext(&x)))
            }
            Kind::Icc if drops_icc(opts) => None,
            Kind::OtherApp if drops(opts, Category::Hidden) => None,
            _ => Some(b.clone()),
        })
        .collect();
    out.write(!drops(opts, Category::Hidden))
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let b = read_file(src)?;
    write_file(dst, &clean_gif(&parse(&b)?, opts))
}

fn edit_gif(mut g: Gif, edits: &[Edit]) -> Result<Vec<u8>> {
    let any_set = edits
        .iter()
        .any(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()));
    let old = g.blocks.iter().find_map(|b| match b {
        Block::Ext(_, raw) if kind(b) == Kind::Xmp => Some(xmp_text(raw)),
        _ => None,
    });
    let packet = if old.is_some() || any_set {
        Some(xmp::apply_edits(old.as_deref(), edits)?)
    } else {
        None
    };
    let desc = edits.iter().rev().find(|e| e.key == EditKey::Description);
    g.blocks.retain(|b| {
        let k = kind(b);
        !(k == Kind::Xmp || (k == Kind::Comment && desc.is_some()))
    });
    // New blocks go before the first frame (after the looping block).
    let at = g
        .blocks
        .iter()
        .position(|b| {
            matches!(
                b,
                Block::Image(_) | Block::Ext(0xF9, _) | Block::Ext(0x01, _)
            )
        })
        .unwrap_or(g.blocks.len());
    let mut new = Vec::new();
    if let Some(v) = desc
        .and_then(|d| d.value.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        new.push(Block::Ext(0xFE, comment_ext(v)));
    }
    if let Some(p) = packet {
        new.push(Block::Ext(0xFF, xmp_ext(&p)));
    }
    if !new.is_empty() && g.head.starts_with(b"GIF87a") {
        g.head[3..6].copy_from_slice(b"89a");
    }
    g.blocks.splice(at..at, new);
    Ok(g.write(true))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let b = read_file(src)?;
    write_file(dst, &edit_gif(parse(&b)?, edits)?)
}

/// A small GIF with a comment, an XMP packet, an ICC profile and a
/// private application block.
pub fn sample() -> Vec<u8> {
    let base = imgutil::encode_sample(image::ImageFormat::Gif);
    let mut g = parse(&base).expect("the encoder writes a valid GIF");
    let new = vec![
        Block::Ext(0xFE, comment_ext("Sample comment")),
        Block::Ext(0xFF, xmp_ext(&xmp::sample())),
        Block::Ext(0xFF, app_ext(ICC_APP, &icc::sample())),
        Block::Ext(0xFF, app_ext(b"PRIVAPP1.00", b"vendor data")),
    ];
    let at = g
        .blocks
        .iter()
        .position(|b| matches!(b, Block::Image(_) | Block::Ext(0xF9, _)))
        .unwrap_or(0);
    g.blocks.splice(at..at, new);
    g.write(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;

    fn images(b: &[u8]) -> Vec<Block> {
        parse(b)
            .unwrap()
            .blocks
            .into_iter()
            .filter(|b| matches!(b, Block::Image(_)))
            .collect()
    }

    #[test]
    fn reads_everything() {
        let t = Tmp::new("gif-read");
        let b = sample();
        let ex = read(&t.file("a.gif", &b)).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "gif:comment"), Some("Sample comment"));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        assert!(has(&ex, "gif:app:PRIVAPP1.00"));
        assert!(ex.gps.is_some());
        same_pixels(&imgutil::encode_sample(image::ImageFormat::Gif), &b);
    }

    #[test]
    fn xmp_trailer_keeps_the_block_chain_valid() {
        let raw = xmp_ext(&xmp::sample());
        assert_eq!(skip_sub_blocks(&raw, 2), Some(raw.len()));
        assert_eq!(xmp_text(&raw), xmp::sample());
    }

    #[test]
    fn clean_is_lossless_and_complete() {
        clean_suite("gif-clean", &sample(), "gif", read, clean);
        let t = Tmp::new("gif-lossless");
        let src = t.file("a.gif", &sample());
        let dst = t.path("b.gif");
        let mut o = CleanOptions::everything();
        o.keep_color_profile = false;
        clean(&src, &dst, &o).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(images(&a), images(&b));
        same_pixels(&a, &b);
        assert_eq!(read(&dst).unwrap().fields.len(), 1);
    }

    #[test]
    fn partial_clean_keeps_the_rest() {
        let t = Tmp::new("gif-partial");
        let src = t.file("a.gif", &sample());
        let dst = t.path("b.gif");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none() && !has(&ex, "xmp:dc:creator"));
        assert_eq!(value(&ex, "gif:comment"), Some("Sample comment"));
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        clean(&src, &dst, &CleanOptions::everything()).unwrap();
        assert!(has(&read(&dst).unwrap(), "icc:desc"));
    }

    #[test]
    fn edit_round_trip() {
        edit_suite("gif-edit", &sample(), "gif", read, edit);
        let t = Tmp::new("gif-edit-bare");
        let bare = imgutil::encode_sample(image::ImageFormat::Gif);
        let src = t.file("a.gif", &bare);
        let dst = t.path("b.gif");
        edit(&src, &dst, &[Edit::set(EditKey::Description, "تێبینی")]).unwrap();
        let b = std::fs::read(&dst).unwrap();
        assert_eq!(value(&read(&dst).unwrap(), "gif:comment"), Some("تێبینی"));
        same_pixels(&bare, &b);
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("gif-bad", &sample(), "gif", read, clean);
    }
}
