//! JPEG: a segment walk that keeps the picture (tables, frame header and
//! entropy-coded scans) byte for byte, and reads, removes or rewrites the
//! metadata segments around it: APP1 EXIF and XMP (plus extended XMP),
//! APP2 ICC profile and MPF, APP13 Photoshop/IPTC, comments, vendor APPn
//! segments and data after the end-of-image marker.

use super::exif::{self, Exif};
use super::imgutil::{self, drops, drops_icc, finish, read_file, text_of, write_file};
use super::{icc, iptc, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use std::path::Path;

const XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const EXT_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const ICC_HEADER: &[u8] = b"ICC_PROFILE\0";
const MPF_HEADER: &[u8] = b"MPF\0";
const MAX_PAYLOAD: usize = 65533;

#[derive(Clone, Debug, PartialEq)]
enum Piece {
    /// A marker segment; `at` is the payload's offset in the original file.
    Seg {
        marker: u8,
        data: Vec<u8>,
        at: Option<usize>,
    },
    /// A marker without a length (RSTn, TEM).
    Marker(u8),
    /// Entropy-coded data following a start-of-scan segment.
    Scan(Vec<u8>),
}

#[derive(Clone, Debug)]
struct Jpeg {
    pieces: Vec<Piece>,
    trailer: Vec<u8>,
    trailer_at: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Exif,
    Xmp,
    ExtXmp,
    Icc,
    Mpf,
    Irb,
    Com,
    Jfxx,
    Vendor,
    Keep,
}

fn kind(marker: u8, d: &[u8]) -> Kind {
    match marker {
        0xE0 if d.starts_with(b"JFIF\0") => Kind::Keep,
        0xE0 if d.starts_with(b"JFXX\0") => Kind::Jfxx,
        0xE1 if d.starts_with(b"Exif\0") && d.len() >= 6 => Kind::Exif,
        0xE1 if d.starts_with(XMP_HEADER) => Kind::Xmp,
        0xE1 if d.starts_with(EXT_XMP_HEADER) => Kind::ExtXmp,
        0xE2 if d.starts_with(ICC_HEADER) => Kind::Icc,
        0xE2 if d.starts_with(MPF_HEADER) => Kind::Mpf,
        0xED if d.starts_with(iptc::APP13_HEADER) => Kind::Irb,
        0xEE if d.starts_with(b"Adobe") => Kind::Keep,
        0xE0..=0xEF => Kind::Vendor,
        0xFE => Kind::Com,
        _ => Kind::Keep,
    }
}

fn parse(b: &[u8]) -> Result<Jpeg> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return Err("This is not a JPEG picture.".into());
    }
    let mut j = Jpeg {
        pieces: Vec::new(),
        trailer: Vec::new(),
        trailer_at: b.len(),
    };
    let mut pos = 2;
    while pos + 1 < b.len() {
        if b[pos] != 0xFF {
            return Err("The JPEG structure is damaged.".into());
        }
        while pos + 1 < b.len() && b[pos + 1] == 0xFF {
            pos += 1;
        }
        let Some(&m) = b.get(pos + 1) else { break };
        pos += 2;
        match m {
            0xD9 => {
                j.trailer = b[pos..].to_vec();
                j.trailer_at = pos;
                return Ok(j);
            }
            0xD0..=0xD7 | 0x01 => j.pieces.push(Piece::Marker(m)),
            _ => {
                let len = imgutil::be16(b, pos).ok_or("The JPEG file is cut short.")? as usize;
                if len < 2 || pos + len > b.len() {
                    return Err("The JPEG file is cut short.".into());
                }
                j.pieces.push(Piece::Seg {
                    marker: m,
                    data: b[pos + 2..pos + len].to_vec(),
                    at: Some(pos + 2),
                });
                pos += len;
                if m == 0xDA {
                    let start = pos;
                    let mut i = pos;
                    while i + 1 < b.len() {
                        if b[i] == 0xFF {
                            match b[i + 1] {
                                0x00 | 0xD0..=0xD7 => i += 2,
                                0xFF => i += 1,
                                _ => break,
                            }
                        } else {
                            i += 1;
                        }
                    }
                    let end = if i + 1 < b.len() { i } else { b.len() };
                    j.pieces.push(Piece::Scan(b[start..end].to_vec()));
                    pos = end;
                }
            }
        }
    }
    // No end-of-image marker: a truncated file. Keep what was found.
    j.trailer_at = b.len();
    Ok(j)
}

impl Jpeg {
    fn segs(&self) -> impl Iterator<Item = (usize, u8, &Vec<u8>)> {
        self.pieces.iter().enumerate().filter_map(|(i, p)| match p {
            Piece::Seg { marker, data, .. } => Some((i, *marker, data)),
            _ => None,
        })
    }

    fn of_kind(&self, k: Kind) -> Vec<usize> {
        self.segs()
            .filter(|(_, m, d)| kind(*m, d) == k)
            .map(|s| s.0)
            .collect()
    }

    fn data(&self, i: usize) -> &[u8] {
        match &self.pieces[i] {
            Piece::Seg { data, .. } => data,
            _ => &[],
        }
    }

    fn ext_xmp(&self) -> Option<String> {
        let mut parts: Vec<(u32, &[u8])> = self
            .of_kind(Kind::ExtXmp)
            .into_iter()
            .filter_map(|i| {
                let d = self.data(i).get(EXT_XMP_HEADER.len()..)?;
                Some((imgutil::be32(d, 36)?, d.get(40..)?))
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        parts.sort_by_key(|p| p.0);
        let all: Vec<u8> = parts
            .into_iter()
            .flat_map(|p| p.1.iter().copied())
            .collect();
        Some(String::from_utf8_lossy(&all).into_owned())
    }

    fn icc(&self) -> Option<Vec<u8>> {
        let mut parts: Vec<(u8, &[u8])> = self
            .of_kind(Kind::Icc)
            .into_iter()
            .filter_map(|i| {
                let d = self.data(i);
                Some((*d.get(12)?, d.get(14..)?))
            })
            .collect();
        if parts.is_empty() {
            return None;
        }
        parts.sort_by_key(|p| p.0);
        Some(
            parts
                .into_iter()
                .flat_map(|p| p.1.iter().copied())
                .collect(),
        )
    }

    /// The file bytes; also re-bases the MPF offsets to the extra pictures.
    fn write(&self, trailer: bool) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        let mut mpf = None;
        for p in &self.pieces {
            match p {
                Piece::Seg { marker, data, at } => {
                    out.extend([0xFF, *marker]);
                    out.extend(((data.len() + 2) as u16).to_be_bytes());
                    if *marker == 0xE2 && data.starts_with(MPF_HEADER) {
                        mpf = at.map(|a| (a, out.len()));
                    }
                    out.extend(data);
                }
                Piece::Marker(m) => out.extend([0xFF, *m]),
                Piece::Scan(s) => out.extend(s),
            }
        }
        out.extend([0xFF, 0xD9]);
        if trailer {
            if let Some((old, new)) = mpf {
                let delta = (out.len() as i64 - new as i64) - (self.trailer_at as i64 - old as i64);
                if delta != 0 {
                    let end = new + self.data_len_at(old);
                    shift_mpf(&mut out[new + 4..end], delta);
                }
            }
            out.extend(&self.trailer);
        }
        out
    }

    fn data_len_at(&self, at: usize) -> usize {
        self.pieces
            .iter()
            .find_map(|p| match p {
                Piece::Seg {
                    data, at: Some(a), ..
                } if *a == at => Some(data.len()),
                _ => None,
            })
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn scans(&self) -> Vec<u8> {
        self.pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Scan(s) => Some(s.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect()
    }
}

/// Adds `delta` to the image offsets of an MP index (a TIFF structure).
fn shift_mpf(tiff: &mut [u8], delta: i64) {
    let Some(x) = Exif::parse_tiff(tiff) else {
        return;
    };
    let Some(e) = x.find(exif::IfdKind::Ifd0, 0xB002) else {
        return;
    };
    let Some((at, len)) = e.src else { return };
    for k in 0..(len as usize / 16) {
        let p = at as usize + k * 16 + 8;
        let old = if x.le {
            imgutil::le32(tiff, p)
        } else {
            imgutil::be32(tiff, p)
        };
        if let Some(o) = old.filter(|&o| o != 0) {
            let n = (o as i64 + delta).clamp(0, u32::MAX as i64) as u32;
            let b = if x.le {
                n.to_le_bytes()
            } else {
                n.to_be_bytes()
            };
            tiff[p..p + 4].copy_from_slice(&b);
        }
    }
}

/// Width and height from the frame header.
pub fn dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let j = parse(b).ok()?;
    sof_dims(&j)
}

fn sof_dims(j: &Jpeg) -> Option<(u32, u32)> {
    j.segs().find_map(|(_, m, d)| {
        let sof = matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
        if !sof {
            return None;
        }
        let h = imgutil::be16(d, 1)? as u32;
        let w = imgutil::be16(d, 3)? as u32;
        Some((w, h))
    })
}

fn vendor_name(d: &[u8]) -> String {
    let n: String = d
        .iter()
        .take_while(|&&c| c != 0)
        .take(24)
        .map(|&c| c as char)
        .collect();
    if !n.is_empty() && n.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        format!(", {n}")
    } else {
        String::new()
    }
}

fn extract(j: &Jpeg) -> Extract {
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
    let f = &mut ex.fields;
    if let Some((w, h)) = sof_dims(j) {
        f.push(imgutil::dims_field(w as u64, h as u64));
    }
    let mut comments = Vec::new();
    let mut has_mpf = false;
    for (_, m, d) in j.segs() {
        match kind(m, d) {
            Kind::Exif => {
                if let Some(x) = Exif::parse(&d[6..]) {
                    f.extend(x.fields());
                    if x.thumbnail.is_some() {
                        ex.notes.push("The embedded thumbnail may show the picture before it was cropped or edited.".into());
                    }
                }
            }
            Kind::Xmp => f.extend(xmp::fields(&text_of(&d[XMP_HEADER.len()..]), "XMP")),
            Kind::Irb => f.extend(iptc::irb_fields(&d[iptc::APP13_HEADER.len()..])),
            Kind::Com => {
                let t = text_of(d).trim().to_string();
                if !t.is_empty() {
                    comments.push(t);
                }
            }
            Kind::Mpf => {
                has_mpf = true;
                let n = Exif::parse_tiff(&d[4..])
                    .and_then(|x| {
                        x.find(exif::IfdKind::Ifd0, 0xB001)
                            .and_then(|e| x.uint(e, 0))
                    })
                    .unwrap_or(2);
                f.push(
                    Field::new(
                        "jpeg:mpf",
                        "Extra pictures (MPF)",
                        format!(
                            "{} more picture(s) stored after the main one",
                            n.saturating_sub(1).max(1)
                        ),
                        Category::Hidden,
                        "JPEG",
                    )
                    .flag(Flag::Hidden),
                );
            }
            Kind::Jfxx => f.push(
                Field::new(
                    "jpeg:jfxx",
                    "Embedded thumbnail (JFIF)",
                    format!("{} bytes", d.len()),
                    Category::Hidden,
                    "JPEG",
                )
                .flag(Flag::Hidden),
            ),
            Kind::Vendor => f.push(
                Field::new(
                    format!("jpeg:app{}", m - 0xE0),
                    "Vendor data block",
                    format!(
                        "APP{} segment{}, {} bytes",
                        m - 0xE0,
                        vendor_name(d),
                        d.len()
                    ),
                    Category::Hidden,
                    "JPEG",
                )
                .flag(Flag::Hidden),
            ),
            Kind::ExtXmp | Kind::Icc | Kind::Keep => {}
        }
    }
    if let Some(x) = j.ext_xmp() {
        f.extend(xmp::fields(&x, "XMP (extended)").into_iter().map(|mut fl| {
            fl.id = fl.id.replacen("xmp:", "xmpext:", 1);
            fl
        }));
    }
    if let Some(p) = j.icc() {
        f.extend(icc::fields(&p));
    }
    if !comments.is_empty() {
        f.push(
            Field::new(
                "jpeg:com",
                "Comment",
                comments.join("; "),
                Category::Description,
                "JPEG comment",
            )
            .flag(Flag::Comments)
            .edit(EditKey::Description),
        );
    }
    if j.trailer.iter().any(|&b| b != 0) {
        let what = if has_mpf { " (extra pictures)" } else { "" };
        f.push(
            Field::new(
                "jpeg:trailer",
                "Data after the picture",
                format!("{} bytes{what}", j.trailer.len()),
                Category::Hidden,
                "JPEG",
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

/// Does `opts` remove anything among these fields?
fn touches(fields: &[Field], opts: &CleanOptions) -> bool {
    fields
        .iter()
        .any(|f| f.removable && drops(opts, f.category))
}

fn clean_jpeg(j: &Jpeg, opts: &CleanOptions) -> Vec<u8> {
    let mut out = j.clone();
    let ext_drop = j
        .ext_xmp()
        .is_some_and(|x| opts.is_everything() || touches(&xmp::fields(&x, "XMP"), opts));
    let mut main_xmp_kept = false;
    out.pieces.retain_mut(|p| {
        let Piece::Seg { marker, data, .. } = p else {
            return true;
        };
        let new: Option<Vec<u8>> = match kind(*marker, data) {
            Kind::Exif => {
                exif::strip_block(&data[6..], opts).map(|b| [exif::APP1_HEADER, &b[..]].concat())
            }
            Kind::Xmp => xmp::strip(&text_of(&data[XMP_HEADER.len()..]), opts)
                .map(|x| [XMP_HEADER, x.as_bytes()].concat())
                .filter(|d| d.len() <= MAX_PAYLOAD),
            Kind::Irb => iptc::strip_irb(&data[iptc::APP13_HEADER.len()..], opts)
                .map(|b| [iptc::APP13_HEADER, &b[..]].concat())
                .filter(|d| d.len() <= MAX_PAYLOAD),
            Kind::ExtXmp => (!ext_drop).then(|| data.clone()),
            Kind::Icc => (!drops_icc(opts)).then(|| data.clone()),
            Kind::Mpf | Kind::Jfxx | Kind::Vendor => {
                (!drops(opts, Category::Hidden)).then(|| data.clone())
            }
            Kind::Com => (!drops(opts, Category::Description)).then(|| data.clone()),
            Kind::Keep => return true,
        };
        match new {
            Some(d) => {
                if kind(*marker, &d) == Kind::Xmp {
                    main_xmp_kept = true;
                }
                *data = d;
                true
            }
            None => false,
        }
    });
    if !main_xmp_kept {
        out.pieces.retain(|p| !matches!(p, Piece::Seg { marker, data, .. } if kind(*marker, data) == Kind::ExtXmp));
    }
    out.write(!drops(opts, Category::Hidden))
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let b = read_file(src)?;
    let j = parse(&b)?;
    write_file(dst, &clean_jpeg(&j, opts))
}

fn seg(marker: u8, data: Vec<u8>) -> Result<Piece> {
    if data.len() > MAX_PAYLOAD {
        return Err("The new metadata is too large to fit in a JPEG segment.".into());
    }
    Ok(Piece::Seg {
        marker,
        data,
        at: None,
    })
}

fn edit_jpeg(mut j: Jpeg, edits: &[Edit]) -> Result<Vec<u8>> {
    let any_set = edits
        .iter()
        .any(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()));
    // EXIF
    let exif_at = j.of_kind(Kind::Exif).first().copied();
    let mut x = exif_at
        .and_then(|i| Exif::parse(&j.data(i)[6..]))
        .unwrap_or_else(|| Exif::new(true));
    x.apply_edits(edits)?;
    let exif_seg = (!x.is_empty())
        .then(|| seg(0xE1, x.to_app1()))
        .transpose()?;
    // XMP
    let xmp_at = j.of_kind(Kind::Xmp).first().copied();
    let old = xmp_at.map(|i| text_of(&j.data(i)[XMP_HEADER.len()..]));
    let xmp_seg = if old.is_some() || any_set {
        let x = xmp::apply_edits(old.as_deref(), edits)?;
        Some(seg(0xE1, [XMP_HEADER, x.as_bytes()].concat())?)
    } else {
        None
    };
    // Extended XMP holding an edited property would show a stale value.
    let keys: Vec<EditKey> = edits.iter().map(|e| e.key).collect();
    let ext_stale = j.ext_xmp().is_some_and(|x| {
        xmp::fields(&x, "XMP")
            .iter()
            .any(|f| f.edit.is_some_and(|k| keys.contains(&k)))
    });
    // IPTC
    for i in j.of_kind(Kind::Irb) {
        let d = &j.data(i)[iptc::APP13_HEADER.len()..];
        j.pieces[i] = match iptc::edit_irb(d, edits)? {
            Some(n) => seg(0xED, [iptc::APP13_HEADER, &n[..]].concat())?,
            None => Piece::Marker(0),
        };
    }
    let drop_com = keys.contains(&EditKey::Description);
    j.pieces.retain(|p| match p {
        Piece::Marker(0) => false,
        Piece::Seg { marker, data, .. } => match kind(*marker, data) {
            Kind::Com => !drop_com,
            Kind::ExtXmp => !ext_stale,
            _ => true,
        },
        _ => true,
    });
    // Put EXIF and XMP back in place (or after the JFIF header).
    let first = j.of_kind(Kind::Exif).first().copied();
    let lead = j
        .pieces
        .iter()
        .take_while(|p| matches!(p, Piece::Seg { marker: 0xE0, .. }))
        .count();
    let exif_pos = match first {
        Some(i) => {
            j.pieces.remove(i);
            i
        }
        None => lead,
    };
    if let Some(s) = exif_seg {
        j.pieces.insert(exif_pos, s);
    }
    let xi = j.of_kind(Kind::Xmp).first().copied();
    let after_exif = j.of_kind(Kind::Exif).first().map(|i| i + 1).unwrap_or(lead);
    let xmp_pos = match xi {
        Some(i) => {
            j.pieces.remove(i);
            i
        }
        None => after_exif,
    };
    if let Some(s) = xmp_seg {
        j.pieces.insert(xmp_pos.min(j.pieces.len()), s);
    }
    Ok(j.write(true))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let b = read_file(src)?;
    let j = parse(&b)?;
    write_file(dst, &edit_jpeg(j, edits)?)
}

/// A small JPEG with known invented metadata in every kind of block.
pub fn sample() -> Vec<u8> {
    let base = imgutil::encode_sample(image::ImageFormat::Jpeg);
    let mut j = parse(&base).expect("the encoder writes a valid JPEG");
    let thumb = {
        let small = image::RgbImage::from_fn(16, 12, |x, y| {
            image::Rgb([(x * 16) as u8, (y * 20) as u8, 90])
        });
        let mut c = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(small)
            .write_to(&mut c, image::ImageFormat::Jpeg)
            .expect("thumbnail");
        c.into_inner()
    };
    let x = exif::sample(Some(thumb));
    let mut icc_seg = ICC_HEADER.to_vec();
    icc_seg.extend([1, 1]);
    icc_seg.extend(icc::sample());
    let new = [
        (0xE1, x.to_app1()),
        (0xE1, [XMP_HEADER, xmp::sample().as_bytes()].concat()),
        (0xE2, icc_seg),
        (0xED, [iptc::APP13_HEADER, &iptc::sample_irb()[..]].concat()),
        (0xEA, b"VendorX\0secret maker data".to_vec()),
        (0xFE, b"Sample comment".to_vec()),
    ];
    let lead = j
        .pieces
        .iter()
        .take_while(|p| matches!(p, Piece::Seg { marker: 0xE0, .. }))
        .count();
    for (k, (m, d)) in new.into_iter().enumerate() {
        j.pieces.insert(
            lead + k,
            Piece::Seg {
                marker: m,
                data: d,
                at: None,
            },
        );
    }
    j.trailer = b"TRAILER: hidden bytes after the picture".to_vec();
    j.write(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;

    #[test]
    fn reads_everything() {
        let t = Tmp::new("jpeg-read");
        let ex = read(&t.file("a.jpg", &sample())).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "exif:0xA431"), Some(s::SERIAL));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "iptc:2:80"), Some(s::ARTIST));
        assert_eq!(value(&ex, "iptc:2:90"), Some(s::CITY));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        assert_eq!(value(&ex, "jpeg:com"), Some("Sample comment"));
        assert_eq!(value(&ex, "exif:thumbnail"), Some("JPEG picture, 16 x 12"));
        assert!(has(&ex, "jpeg:app10") && has(&ex, "jpeg:trailer"));
        let g = ex.gps.unwrap();
        assert!((g.lat - s::LAT).abs() < 1e-6 && (g.lon - s::LON).abs() < 1e-6);
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        for flag in [
            Flag::Location,
            Flag::Names,
            Flag::Serial,
            Flag::Comments,
            Flag::Hidden,
        ] {
            assert!(ex.fields.iter().any(|f| f.flag == Some(flag)), "{flag:?}");
        }
    }

    #[test]
    fn clean_is_lossless_and_complete() {
        clean_suite("jpeg-clean", &sample(), "jpg", read, clean);
        let t = Tmp::new("jpeg-lossless");
        let src = t.file("a.jpg", &sample());
        let dst = t.path("b.jpg");
        clean(&src, &dst, &CleanOptions::everything()).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(parse(&a).unwrap().scans(), parse(&b).unwrap().scans());
        assert!(!parse(&a).unwrap().scans().is_empty());
        same_pixels(&a, &b);
        let ex = read(&dst).unwrap();
        // Kept by default: orientation and colour profile.
        assert_eq!(value(&ex, "exif:0x0112"), Some("Rotate 90° clockwise"));
        assert!(has(&ex, "icc:desc"));
        assert!(b.len() < a.len() / 2 + 2000);
        assert!(!b.windows(7).any(|w| w == b"TRAILER"));
    }

    #[test]
    fn keep_options_are_honoured() {
        let t = Tmp::new("jpeg-keep");
        let src = t.file("a.jpg", &sample());
        let dst = t.path("b.jpg");
        let mut o = CleanOptions::everything();
        o.keep_orientation = false;
        clean(&src, &dst, &o).unwrap();
        let ex = read(&dst).unwrap();
        assert!(!has(&ex, "exif:0x0112") && has(&ex, "icc:desc"));
        o.keep_orientation = true;
        o.keep_color_profile = false;
        clean(&src, &dst, &o).unwrap();
        let ex = read(&dst).unwrap();
        assert!(has(&ex, "exif:0x0112") && !has(&ex, "icc:desc"));
    }

    #[test]
    fn partial_clean_keeps_camera_and_dates() {
        let t = Tmp::new("jpeg-partial");
        let src = t.file("a.jpg", &sample());
        let dst = t.path("b.jpg");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none());
        assert!(!ex.fields.iter().any(|f| f.flag == Some(Flag::Names)));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        assert!(has(&ex, "exif:thumbnail") && has(&ex, "jpeg:trailer"));
        same_pixels(&std::fs::read(&src).unwrap(), &std::fs::read(&dst).unwrap());
    }

    #[test]
    fn edit_round_trip() {
        edit_suite("jpeg-edit", &sample(), "jpg", read, edit);
        // Editing a JPEG without any metadata creates EXIF and XMP.
        let t = Tmp::new("jpeg-edit-bare");
        let src = t.file("a.jpg", &imgutil::encode_sample(image::ImageFormat::Jpeg));
        let dst = t.path("b.jpg");
        edit(&src, &dst, &[Edit::set(EditKey::Gps, "36.1901, 44.0091")]).unwrap();
        let ex = read(&dst).unwrap();
        assert!(has(&ex, "exif:gps") && has(&ex, "xmp:exif:gps"));
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(parse(&a).unwrap().scans(), parse(&b).unwrap().scans());
    }

    #[test]
    fn mpf_offsets_follow_the_trailer() {
        // An MP index whose second picture starts right after EOI.
        let mut mp =
            b"MPF\0II*\0\x08\0\0\0\x01\0\x02\xB0\x07\0\x20\0\0\0\x1A\0\0\0\0\0\0\0".to_vec();
        let entries = {
            let mut e = vec![0u8; 32];
            e[24..28].copy_from_slice(&0u32.to_le_bytes());
            e
        };
        mp.extend(&entries);
        let base = sample();
        let mut j = parse(&base).unwrap();
        j.pieces.insert(
            1,
            Piece::Seg {
                marker: 0xE2,
                data: mp,
                at: None,
            },
        );
        let mut bytes = j.write(true);
        // Point the second entry at the trailer, relative to the MP header.
        let j2 = parse(&bytes).unwrap();
        let (i, _, _) = j2
            .segs()
            .find(|(_, m, d)| kind(*m, d) == Kind::Mpf)
            .unwrap();
        let Piece::Seg {
            at: Some(mp_at), ..
        } = j2.pieces[i]
        else {
            panic!()
        };
        let rel = (j2.trailer_at - (mp_at + 4)) as u32;
        let p = mp_at + 4 + 0x1A + 16 + 8;
        bytes[p..p + 4].copy_from_slice(&rel.to_le_bytes());
        let t = Tmp::new("jpeg-mpf");
        let src = t.file("a.jpg", &bytes);
        let dst = t.path("b.jpg");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let out = std::fs::read(&dst).unwrap();
        let j3 = parse(&out).unwrap();
        let (i, _, d) = j3
            .segs()
            .find(|(_, m, d)| kind(*m, d) == Kind::Mpf)
            .unwrap();
        let Piece::Seg { .. } = j3.pieces[i] else {
            panic!()
        };
        let new_rel = imgutil::le32(d, 4 + 0x1A + 16 + 8).unwrap() as usize;
        let mp_new = out.windows(8).position(|w| w == b"MPF\0II*\0").unwrap() + 4;
        assert_eq!(&out[mp_new + new_rel..mp_new + new_rel + 7], b"TRAILER");
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("jpeg-bad", &sample(), "jpg", read, clean);
    }
}
