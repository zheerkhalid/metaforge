//! HEIF / HEIC / AVIF (ISOBMFF): the `meta` box lists items (`iinf`) and
//! where their bytes are (`iloc`, in the file or in `idat`). EXIF items
//! and XMP (`mime` application/rdf+xml) items are read with the EXIF and
//! XMP modules. Cleaning rewrites those items in place with the same
//! length (a smaller EXIF block or an empty TIFF, a stripped or empty XMP
//! packet, padded), so no box ever moves. Editing is not supported.

use super::exif::{self, Exif};
use super::imgutil::{self, finish, read_file, write_file};
use super::{icc, xmp};
use crate::meta::{CleanOptions, Edit, Extract, Result};
use std::path::Path;

#[derive(Clone, Debug, Default)]
struct Item {
    id: u32,
    typ: [u8; 4],
    content_type: String,
}

#[derive(Clone, Debug, Default)]
struct Layout {
    items: Vec<Item>,
    /// Item id -> absolute (offset, length) extents.
    locs: Vec<(u32, Vec<(usize, usize)>)>,
    dims: Option<(u32, u32)>,
    icc: Option<Vec<u8>>,
}

fn uint(b: &[u8], at: usize, n: usize) -> Option<u64> {
    let s = b.get(at..at.checked_add(n)?)?;
    Some(s.iter().fold(0u64, |a, &x| (a << 8) | x as u64))
}

/// The boxes in `b[start..end]`: (type, content start, box end).
fn boxes(b: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut p = start;
    let end = end.min(b.len());
    while p + 8 <= end {
        let Some(size) = uint(b, p, 4) else { break };
        let typ: [u8; 4] = b[p + 4..p + 8].try_into().unwrap_or_default();
        let (head, size) = match size {
            0 => (8, (end - p) as u64),
            1 => match uint(b, p + 8, 8) {
                Some(s) => (16, s),
                None => break,
            },
            s => (8, s),
        };
        if size < head as u64 || p as u64 + size > end as u64 {
            break;
        }
        let box_end = p + size as usize;
        out.push((typ, p + head, box_end));
        p = box_end;
    }
    out
}

fn cstr(b: &[u8], at: usize, end: usize) -> Option<(String, usize)> {
    let s = b.get(at..end)?;
    let n = s.iter().position(|&c| c == 0)?;
    Some((String::from_utf8_lossy(&s[..n]).into_owned(), at + n + 1))
}

fn parse_infe(b: &[u8], start: usize, end: usize) -> Option<Item> {
    let v = *b.get(start)?;
    if v < 2 {
        return None;
    }
    let mut p = start + 4;
    let id = if v == 2 {
        p += 2;
        uint(b, p - 2, 2)? as u32
    } else {
        p += 4;
        uint(b, p - 4, 4)? as u32
    };
    p += 2; // protection index
    let typ: [u8; 4] = b.get(p..p + 4)?.try_into().ok()?;
    p += 4;
    let (_, p) = cstr(b, p, end)?;
    let content_type = if &typ == b"mime" {
        cstr(b, p, end).map(|x| x.0).unwrap_or_default()
    } else {
        String::new()
    };
    Some(Item {
        id,
        typ,
        content_type,
    })
}

fn parse_iloc(
    b: &[u8],
    start: usize,
    end: usize,
    idat: Option<usize>,
) -> Vec<(u32, Vec<(usize, usize)>)> {
    let mut out = Vec::new();
    let mut f = || -> Option<()> {
        let v = *b.get(start)?;
        let mut p = start + 4;
        let s1 = *b.get(p)?;
        let s2 = *b.get(p + 1)?;
        p += 2;
        let (off_size, len_size) = ((s1 >> 4) as usize, (s1 & 15) as usize);
        let base_size = (s2 >> 4) as usize;
        let index_size = if v == 1 || v == 2 {
            (s2 & 15) as usize
        } else {
            0
        };
        let count = if v < 2 {
            p += 2;
            uint(b, p - 2, 2)?
        } else {
            p += 4;
            uint(b, p - 4, 4)?
        };
        for _ in 0..count.min(10_000) {
            let id_size = if v < 2 { 2 } else { 4 };
            let id = uint(b, p, id_size)? as u32;
            p += id_size;
            let mut method = 0;
            if v == 1 || v == 2 {
                method = uint(b, p, 2)? & 15;
                p += 2;
            }
            p += 2; // data reference index
            let base = uint(b, p, base_size)?;
            p += base_size;
            let n = uint(b, p, 2)?;
            p += 2;
            let mut ext = Vec::new();
            for _ in 0..n {
                p += index_size;
                let o = uint(b, p, off_size)?;
                p += off_size;
                let l = uint(b, p, len_size)?;
                p += len_size;
                let at = match method {
                    0 => base.checked_add(o)?,
                    1 => (idat? as u64).checked_add(base)?.checked_add(o)?,
                    _ => continue,
                };
                ext.push((at as usize, l as usize));
            }
            if p > end {
                return None;
            }
            out.push((id, ext));
        }
        Some(())
    };
    let _ = f();
    out
}

fn layout(b: &[u8]) -> Result<Layout> {
    if b.get(4..8) != Some(b"ftyp") {
        return Err("This is not a HEIF or AVIF picture.".into());
    }
    let mut l = Layout::default();
    let Some(&(_, ms, me)) = boxes(b, 0, b.len()).iter().find(|x| &x.0 == b"meta") else {
        return Ok(l);
    };
    let children = boxes(b, ms + 4, me);
    let idat = children.iter().find(|x| &x.0 == b"idat").map(|x| x.1);
    for &(typ, s, e) in &children {
        match &typ {
            b"iinf" => {
                let v = b.get(s).copied().unwrap_or(0);
                let first = s + if v == 0 { 6 } else { 8 };
                for (t, cs, ce) in boxes(b, first, e) {
                    if &t == b"infe" {
                        l.items.extend(parse_infe(b, cs, ce));
                    }
                }
            }
            b"iloc" => l.locs = parse_iloc(b, s, e, idat),
            b"iprp" => {
                for (t, cs, ce) in boxes(b, s, e) {
                    if &t != b"ipco" {
                        continue;
                    }
                    for (p, ps, pe) in boxes(b, cs, ce) {
                        match &p {
                            b"ispe" if l.dims.is_none() => {
                                if let (Some(w), Some(h)) = (uint(b, ps + 4, 4), uint(b, ps + 8, 4))
                                {
                                    l.dims = Some((w as u32, h as u32));
                                }
                            }
                            b"colr" if matches!(b.get(ps..ps + 4), Some(t) if t == b"prof" || t == b"rICC") =>
                            {
                                l.icc = b.get(ps + 4..pe).map(|x| x.to_vec());
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(l)
}

impl Layout {
    fn extents(&self, id: u32) -> &[(usize, usize)] {
        self.locs
            .iter()
            .find(|x| x.0 == id)
            .map(|x| x.1.as_slice())
            .unwrap_or(&[])
    }

    /// The bytes of an item, if all its extents are inside the file.
    fn bytes(&self, b: &[u8], id: u32) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for &(o, l) in self.extents(id) {
            out.extend(b.get(o..o.checked_add(l)?)?);
        }
        (!out.is_empty()).then_some(out)
    }

    /// The metadata items: (id, is EXIF).
    fn meta_items(&self) -> Vec<(u32, bool)> {
        self.items
            .iter()
            .filter_map(|i| match &i.typ {
                b"Exif" => Some((i.id, true)),
                b"mime" if i.content_type.contains("rdf+xml") || i.content_type.contains("xmp") => {
                    Some((i.id, false))
                }
                _ => None,
            })
            .collect()
    }
}

/// The TIFF part of an EXIF item and where it starts.
fn exif_tiff(payload: &[u8]) -> Option<(usize, &[u8])> {
    let off = uint(payload, 0, 4)? as usize;
    let at = 4usize.checked_add(off)?;
    Some((at, payload.get(at..)?))
}

pub fn read(path: &Path) -> Result<Extract> {
    let b = read_file(path)?;
    let l = layout(&b)?;
    let mut ex = Extract::default();
    if let Some((w, h)) = l.dims {
        ex.fields.push(imgutil::dims_field(w as u64, h as u64));
    }
    for (id, is_exif) in l.meta_items() {
        let Some(p) = l.bytes(&b, id) else { continue };
        if is_exif {
            if let Some((_, t)) = exif_tiff(&p) {
                ex.fields.extend(exif::fields_of(t));
            }
        } else {
            ex.fields
                .extend(xmp::fields(&String::from_utf8_lossy(&p), "XMP"));
        }
    }
    if let Some(p) = &l.icc {
        // Part of how the picture is decoded: shown, never removed here.
        ex.fields
            .extend(icc::fields(p).into_iter().map(|f| f.fixed()));
    }
    finish(&mut ex);
    Ok(ex)
}

const EMPTY_XMP: &str =
    "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/><?xpacket end=\"w\"?>";

/// The cleaned payload of an item, exactly `old.len()` bytes long.
fn clean_payload(old: &[u8], is_exif: bool, opts: &CleanOptions) -> Vec<u8> {
    let len = old.len();
    if !is_exif {
        let text = String::from_utf8_lossy(old);
        return xmp::strip(&text, opts)
            .and_then(|x| xmp::padded(&x, len))
            .or_else(|| xmp::padded(EMPTY_XMP, len))
            .unwrap_or_else(|| vec![b' '; len]);
    }
    let Some((at, tiff)) = exif_tiff(old).filter(|(at, _)| *at <= len) else {
        return vec![0; len];
    };
    let le = tiff.starts_with(b"II");
    let parsed = Exif::parse(tiff);
    let minimal = parsed
        .as_ref()
        .and_then(|x| x.orientation_only())
        .filter(|_| !imgutil::drops_orientation(opts))
        .unwrap_or_else(|| Exif::new(le));
    let empty_tiff = || {
        let mut t = minimal.to_bytes();
        if minimal.is_empty() {
            // Header plus an IFD with no entries.
            t.truncate(8);
            t.extend([0; 6]);
        }
        t
    };
    let new = exif::strip_block(tiff, opts).unwrap_or_else(empty_tiff);
    let new = if at + new.len() <= len {
        new
    } else {
        empty_tiff()
    };
    let mut out = old[..at].to_vec();
    if at + new.len() <= len {
        out.extend(new);
    }
    out.resize(len, 0);
    out
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let mut b = read_file(src)?;
    let l = layout(&b)?;
    for (id, is_exif) in l.meta_items() {
        let Some(old) = l.bytes(&b, id) else { continue };
        let new = clean_payload(&old, is_exif, opts);
        let mut k = 0;
        for &(o, n) in l.extents(id) {
            b[o..o + n].copy_from_slice(&new[k..k + n]);
            k += n;
        }
    }
    write_file(dst, &b)
}

pub fn edit(_src: &Path, _dst: &Path, _edits: &[Edit]) -> Result<()> {
    Err("Editing HEIC and AVIF photos is not supported yet. Removing their metadata works.".into())
}

fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(typ);
    out.extend(body);
    out
}

fn full_box(typ: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version, 0, 0, 0];
    b.extend(body);
    bx(typ, &b)
}

/// A minimal HEIC-branded ISOBMFF file holding an EXIF item, an XMP item,
/// a size property and a colour profile. It has no coded picture, so it
/// does not display as an image; it exercises the metadata paths only.
pub fn sample() -> Vec<u8> {
    let mut exif_item = vec![0, 0, 0, 6];
    exif_item.extend(exif::APP1_HEADER);
    exif_item.extend(exif::sample(None).to_bytes());
    let xmp_item = xmp::sample().into_bytes();

    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let meta = |exif_at: u32, xmp_at: u32| {
        let hdlr = full_box(b"hdlr", 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let mut iinf_body = 2u16.to_be_bytes().to_vec();
        iinf_body.extend(full_box(b"infe", 2, b"\0\x01\0\0Exif\0"));
        iinf_body.extend(full_box(
            b"infe",
            2,
            b"\0\x02\0\0mime\0application/rdf+xml\0",
        ));
        let iinf = full_box(b"iinf", 0, &iinf_body);
        let mut ispe = 64u32.to_be_bytes().to_vec();
        ispe.extend(48u32.to_be_bytes());
        let mut colr = b"prof".to_vec();
        colr.extend(icc::sample());
        let ipco = bx(
            b"ipco",
            &[full_box(b"ispe", 0, &ispe), bx(b"colr", &colr)].concat(),
        );
        let iprp = bx(b"iprp", &ipco);
        let mut iloc_body = vec![0x44, 0x00];
        iloc_body.extend(2u16.to_be_bytes());
        for (id, at, len) in [
            (1u16, exif_at, exif_item.len()),
            (2, xmp_at, xmp_item.len()),
        ] {
            iloc_body.extend(id.to_be_bytes());
            iloc_body.extend([0, 0, 0, 0, 0, 1]); // method 0, data ref 0, one extent
            iloc_body.extend(at.to_be_bytes());
            iloc_body.extend((len as u32).to_be_bytes());
        }
        let iloc = full_box(b"iloc", 1, &iloc_body);
        full_box(b"meta", 0, &[hdlr, iinf, iprp, iloc].concat())
    };
    let meta_len = meta(0, 0).len();
    let data_at = (ftyp.len() + meta_len + 8) as u32;
    let meta = meta(data_at, data_at + exif_item.len() as u32);
    let mdat = bx(b"mdat", &[exif_item.as_slice(), &xmp_item].concat());
    [ftyp, meta, mdat].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;
    use crate::meta::{Category, EditKey};

    #[test]
    fn reads_items() {
        let t = Tmp::new("heif-read");
        let b = sample();
        assert_eq!(
            crate::detect::detect_bytes(&b, "heic"),
            crate::detect::Format::Heif
        );
        let ex = read(&t.file("a.heic", &b)).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        let g = ex.gps.unwrap();
        assert!((g.lat - s::LAT).abs() < 1e-6);
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        assert!(ex.editable.is_empty());
    }

    #[test]
    fn clean_blanks_in_place() {
        clean_suite("heif-clean", &sample(), "heic", read, clean);
        let t = Tmp::new("heif-inplace");
        let src = t.file("a.heic", &sample());
        let dst = t.path("b.heic");
        clean(&src, &dst, &CleanOptions::everything()).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(a.len(), b.len());
        // Everything up to the metadata bytes in mdat is identical.
        let l = layout(&a).unwrap();
        let first = l
            .locs
            .iter()
            .flat_map(|x| x.1.iter().map(|e| e.0))
            .min()
            .unwrap();
        assert_eq!(a[..first], b[..first]);
        let ex = read(&dst).unwrap();
        assert_eq!(value(&ex, "exif:0x0112"), Some("Rotate 90° clockwise"));
        assert!(!has(&ex, "exif:0x0110") && !has(&ex, "xmp:dc:title"));
        assert!(!b
            .windows(s::SERIAL.len())
            .any(|w| w == s::SERIAL.as_bytes()));

        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none() && !has(&ex, "exif:0x013B"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(std::fs::read(&dst).unwrap().len(), a.len());
    }

    #[test]
    fn edit_is_refused_plainly() {
        let t = Tmp::new("heif-edit");
        let src = t.file("a.heic", &sample());
        let e = edit(&src, &t.path("b.heic"), &[Edit::set(EditKey::Title, "x")]).unwrap_err();
        assert!(e.contains("not supported"));
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("heif-bad", &sample(), "heic", read, clean);
    }
}
