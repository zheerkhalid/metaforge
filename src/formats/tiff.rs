//! TIFF: the file is itself an EXIF-style structure. Cleaning and editing
//! never move the picture: the new first directory (and its Exif and GPS
//! directories) is appended to the file and the header points at it; the
//! old directory tables and the values that were removed are zeroed, except
//! where they share bytes with the strips or tiles. Later pages are kept.

use super::exif::{Exif, IfdKind};
use super::imgutil::{self, finish, read_file, text_of, write_file};
use super::{iptc, xmp};
use crate::meta::{CleanOptions, Edit, EditKey, Extract, Result};
use std::path::Path;

fn parse(b: &[u8]) -> Result<Exif> {
    if b.len() >= 4 && (b.starts_with(b"II+\0") || b.starts_with(b"MM\0+")) {
        return Err("BigTIFF files are not supported yet.".into());
    }
    if !(b.starts_with(b"II*\0") || b.starts_with(b"MM\0*")) {
        return Err("This is not a TIFF picture.".into());
    }
    Exif::parse_tiff(b).ok_or_else(|| "The TIFF file is damaged.".to_string())
}

pub fn read(path: &Path) -> Result<Extract> {
    let b = read_file(path)?;
    let x = parse(&b)?;
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
    if let Some((w, h)) = x.dimensions() {
        ex.fields.push(imgutil::dims_field(w as u64, h as u64));
    }
    ex.fields.extend(x.fields());
    if x.next_page != 0 {
        ex.notes.push(
            "Only the first page's details are shown; the other pages are kept as they are.".into(),
        );
    }
    finish(&mut ex);
    Ok(ex)
}

/// The strip or tile data ranges of the first page.
fn image_ranges(x: &Exif) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for (offs, counts) in [(0x0111, 0x0117), (0x0144, 0x0145)] {
        let (Some(o), Some(c)) = (x.find(IfdKind::Ifd0, offs), x.find(IfdKind::Ifd0, counts))
        else {
            continue;
        };
        for i in 0..o.count.min(c.count) as usize {
            if let (Some(a), Some(n)) = (x.uint(o, i), x.uint(c, i)) {
                out.push((a as u64, n as u64));
            }
        }
    }
    out
}

/// The original file with `new`'s directories appended and the header
/// pointing at them; freed bytes of `old` are zeroed.
fn write_appended(orig: &[u8], old: &Exif, new: &Exif) -> Vec<u8> {
    let mut out = orig.to_vec();
    let mut protected = image_ranges(new);
    protected.extend(
        new.used_ranges()
            .into_iter()
            .map(|(a, n)| (a as u64, n as u64)),
    );
    protected.push((0, 8));
    if new.next_page != 0 {
        // The next page's table is not ours to touch.
        protected.push((new.next_page as u64, 2));
    }
    let overlaps = |a: u64, n: u64| protected.iter().any(|&(b, m)| a < b + m && b < a + n);
    for (a, n) in old.freed_ranges(new) {
        let (a, n) = (a as u64, n as u64);
        if !overlaps(a, n) && a + n <= out.len() as u64 {
            out[a as usize..(a + n) as usize].fill(0);
        }
    }
    let at = new.emit(&mut out);
    let b = if new.le {
        at.to_le_bytes()
    } else {
        at.to_be_bytes()
    };
    out[4..8].copy_from_slice(&b);
    out
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let b = read_file(src)?;
    let x = parse(&b)?;
    let mut y = x.clone();
    y.strip(opts);
    write_file(dst, &write_appended(&b, &x, &y))
}

fn edit_model(x: &Exif, edits: &[Edit]) -> Result<Exif> {
    let mut y = x.clone();
    y.apply_edits(edits)?;
    let any_set = edits
        .iter()
        .any(|e| e.value.as_deref().is_some_and(|v| !v.trim().is_empty()));
    let old = y.find(IfdKind::Ifd0, 0x02BC).map(|e| text_of(&e.data));
    if old.is_some() || any_set {
        let packet = xmp::apply_edits(old.as_deref(), edits)?;
        y.set(IfdKind::Ifd0, Exif::bytes(0x02BC, 1, packet.into_bytes()));
    }
    if let Some(e) = y.find(IfdKind::Ifd0, 0x83BB) {
        match iptc::edit_iim(&e.data, edits)? {
            Some(d) => y.set(IfdKind::Ifd0, Exif::bytes(0x83BB, 7, d)),
            None => y.remove(IfdKind::Ifd0, 0x83BB),
        }
    }
    if let Some(e) = y.find(IfdKind::Ifd0, 0x8649) {
        match iptc::edit_irb(&e.data, edits)? {
            Some(d) => y.set(IfdKind::Ifd0, Exif::bytes(0x8649, 1, d)),
            None => y.remove(IfdKind::Ifd0, 0x8649),
        }
    }
    Ok(y)
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let b = read_file(src)?;
    let x = parse(&b)?;
    let y = edit_model(&x, edits)?;
    write_file(dst, &write_appended(&b, &x, &y))
}

/// A small TIFF with known invented metadata (EXIF, GPS, XMP, IPTC, ICC).
pub fn sample() -> Vec<u8> {
    let base = imgutil::encode_sample(image::ImageFormat::Tiff);
    let x = parse(&base).expect("the encoder writes a valid TIFF");
    let s = super::exif::sample(None);
    let mut y = x.clone();
    for e in s.ifd0.iter().chain([
        &Exif::bytes(0x02BC, 1, xmp::sample().into_bytes()),
        &Exif::bytes(0x8773, 7, super::icc::sample()),
        &Exif::bytes(0x83BB, 7, iptc::sample_iim()),
    ]) {
        y.set(IfdKind::Ifd0, e.clone());
    }
    y.exif = s.exif;
    y.gps = s.gps;
    y.interop = s.interop;
    write_appended(&base, &x, &y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;
    use crate::formats::imgutil::testkit::*;
    use crate::meta::Category;

    fn strips(b: &[u8]) -> Vec<u8> {
        let x = parse(b).unwrap();
        image_ranges(&x)
            .into_iter()
            .flat_map(|(a, n)| b[a as usize..(a + n) as usize].to_vec())
            .collect()
    }

    #[test]
    fn reads_everything() {
        let t = Tmp::new("tiff-read");
        let b = sample();
        let ex = read(&t.file("a.tif", &b)).unwrap();
        assert_eq!(value(&ex, "image:size"), Some("64 x 48 pixels"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(value(&ex, "exif:0xA431"), Some(s::SERIAL));
        assert_eq!(value(&ex, "xmp:dc:title"), Some(s::TITLE_KU));
        assert_eq!(value(&ex, "iptc:2:90"), Some(s::CITY));
        assert_eq!(value(&ex, "icc:desc"), Some("Test RGB profile"));
        assert!(ex.gps.is_some());
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        same_pixels(&imgutil::encode_sample(image::ImageFormat::Tiff), &b);
    }

    #[test]
    fn clean_is_lossless_and_complete() {
        clean_suite("tiff-clean", &sample(), "tif", read, clean);
        let t = Tmp::new("tiff-lossless");
        let src = t.file("a.tif", &sample());
        let dst = t.path("b.tif");
        let mut o = CleanOptions::everything();
        o.keep_orientation = false;
        o.keep_color_profile = false;
        clean(&src, &dst, &o).unwrap();
        let (a, b) = (std::fs::read(&src).unwrap(), std::fs::read(&dst).unwrap());
        assert_eq!(strips(&a), strips(&b));
        same_pixels(&a, &b);
        let ex = read(&dst).unwrap();
        assert_eq!(ex.fields.len(), 1, "{:?}", ex.fields);
        // The removed values are gone from the bytes too, not just unlinked.
        for needle in [
            s::SERIAL.as_bytes(),
            s::ARTIST.as_bytes(),
            s::CITY.as_bytes(),
        ] {
            assert!(
                !b.windows(needle.len()).any(|w| w == needle),
                "{:?}",
                std::str::from_utf8(needle)
            );
        }
    }

    #[test]
    fn partial_clean_and_keep_options() {
        let t = Tmp::new("tiff-partial");
        let src = t.file("a.tif", &sample());
        let dst = t.path("b.tif");
        clean(
            &src,
            &dst,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert!(ex.gps.is_none() && !has(&ex, "exif:0x013B") && !has(&ex, "iptc:2:90"));
        assert_eq!(value(&ex, "exif:0x0110"), Some(s::MODEL));
        assert_eq!(ex.taken.as_deref(), Some(s::TAKEN));
        same_pixels(&std::fs::read(&src).unwrap(), &std::fs::read(&dst).unwrap());

        clean(&src, &dst, &CleanOptions::everything()).unwrap();
        let ex = read(&dst).unwrap();
        assert!(has(&ex, "exif:0x0112") && has(&ex, "icc:desc"));
    }

    #[test]
    fn edit_round_trip() {
        edit_suite("tiff-edit", &sample(), "tif", read, edit);
        let t = Tmp::new("tiff-edit-bare");
        let bare = imgutil::encode_sample(image::ImageFormat::Tiff);
        let src = t.file("a.tif", &bare);
        let dst = t.path("b.tif");
        edit(&src, &dst, &[Edit::set(EditKey::Title, s::TITLE_KU)]).unwrap();
        let b = std::fs::read(&dst).unwrap();
        assert_eq!(
            value(&read(&dst).unwrap(), "xmp:dc:title"),
            Some(s::TITLE_KU)
        );
        assert_eq!(strips(&bare), strips(&b));
        same_pixels(&bare, &b);
    }

    #[test]
    fn damaged_input_does_not_panic() {
        robustness("tiff-bad", &sample(), "tif", read, clean);
    }
}
