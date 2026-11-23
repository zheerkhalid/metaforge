//! Helpers shared by the image format modules: file access with plain error
//! messages, the "best date / GPS" summary of an extract, byte readers,
//! the sample picture, and (for tests) a temporary-folder kit.

use crate::meta::{Category, CleanOptions, Extract, Field, GeoPoint};
use crate::util::DateTime;
use std::path::Path;

/// Reads a whole file, with a plain-language error.
pub fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("Could not read the file: {e}"))
}

/// Writes a whole file, with a plain-language error.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("Could not write the file: {e}"))
}

/// The "Dimensions" field (intrinsic, never removed).
pub fn dims_field(width: u64, height: u64) -> Field {
    Field::new(
        "image:size",
        "Dimensions",
        format!("{width} x {height} pixels"),
        Category::Technical,
        "Image",
    )
    .fixed()
}

/// Does a clean with `opts` drop the orientation tag?
pub fn drops_orientation(opts: &CleanOptions) -> bool {
    opts.is_everything() && !opts.keep_orientation
}

/// Does a clean with `opts` drop the colour profile?
pub fn drops_icc(opts: &CleanOptions) -> bool {
    opts.is_everything() && !opts.keep_color_profile
}

/// Does a clean with `opts` remove a removable field of category `cat`?
pub fn drops(opts: &CleanOptions, cat: Category) -> bool {
    cat != Category::Technical && opts.removes(cat)
}

/// The order in which dates count as "the date the photo was taken".
const TAKEN_PRIORITY: &[&str] = &[
    "exif:0x9003",
    "xmp:exif:DateTimeOriginal",
    "xmp:photoshop:DateCreated",
    "xmp:xmp:CreateDate",
    "iptc:2:55",
    "exif:0x0132",
    "png:iTXt:Creation Time",
    "png:tEXt:Creation Time",
    "png:zTXt:Creation Time",
];

/// Fills `taken` and `gps` from the fields (EXIF first, then XMP), and
/// removes exact duplicate ids.
pub fn finish(ex: &mut Extract) {
    let mut seen = std::collections::HashSet::new();
    ex.fields.retain(|f| seen.insert(f.id.clone()));
    if ex.gps.is_none() {
        ex.gps = ["exif:gps", "xmp:exif:gps"].iter().find_map(|id| {
            ex.fields
                .iter()
                .find(|f| f.id == *id)
                .and_then(|f| GeoPoint::parse(&f.value))
        });
    }
    if ex.taken.is_none() {
        ex.taken = TAKEN_PRIORITY.iter().find_map(|id| {
            ex.fields
                .iter()
                .filter(|f| f.id == *id || f.id.starts_with(&format!("{id}#")))
                .find_map(|f| DateTime::parse(&f.value))
                .map(|d| d.text())
        });
    }
}

/// A date as `YYYY-MM-DD HH:MM:SS` when it parses, else the text as is.
pub fn nice_date(text: &str) -> String {
    let t = text.trim();
    match DateTime::parse(t) {
        Some(d) if t.len() >= 10 => d.text(),
        _ => t.to_string(),
    }
}

/// Text from bytes that are UTF-8 or else Latin-1; trailing NULs dropped.
pub fn text_of(bytes: &[u8]) -> String {
    let b = trim_nul(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// Bytes without trailing NULs.
pub fn trim_nul(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    &bytes[..end]
}

/// Latin-1 encoding of `s`, if every character fits.
pub fn latin1(s: &str) -> Option<Vec<u8>> {
    s.chars().map(|c| u8::try_from(c as u32).ok()).collect()
}

pub fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
pub fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
pub fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
pub fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Splits comma-separated keywords.
pub fn keywords(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A 64 x 48 RGB gradient, the picture inside every sample file.
pub fn sample_picture() -> image::RgbImage {
    image::RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([(x * 4) as u8, (y * 5) as u8, ((x + y) * 2) as u8])
    })
}

/// The sample picture encoded in `format` by the `image` crate.
pub fn encode_sample(format: image::ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(sample_picture())
        .write_to(&mut out, format)
        .expect("encoding the sample picture");
    out.into_inner()
}

/// The invented values every sample carries.
pub mod sample_values {
    pub const LAT: f64 = 36.1901;
    pub const LON: f64 = 44.0091;
    pub const MAKE: &str = "ExampleCam";
    pub const MODEL: &str = "EC-100";
    pub const SERIAL: &str = "SN-0042-TEST";
    pub const ARTIST: &str = "Test Author";
    pub const COPYRIGHT: &str = "Test Studio";
    pub const TAKEN: &str = "2024-05-01 10:20:30";
    pub const SOFTWARE: &str = "SampleSoft 1.0";
    pub const DESCRIPTION: &str = "A sample picture for tests";
    pub const TITLE_KU: &str = "وێنەی تاقیکردنەوە";
    pub const CITY: &str = "Erbil";
}

#[cfg(test)]
pub mod testkit {
    //! Shared checks for the format tests.

    use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Result};
    use std::path::{Path, PathBuf};

    /// A unique temporary folder, deleted on drop.
    pub struct Tmp(pub PathBuf);

    impl Tmp {
        pub fn new(tag: &str) -> Tmp {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "metaforge-img-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
        pub fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
        pub fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub type ReadFn = fn(&Path) -> Result<Extract>;
    pub type CleanFn = fn(&Path, &Path, &CleanOptions) -> Result<()>;
    pub type EditFn = fn(&Path, &Path, &[Edit]) -> Result<()>;

    pub fn has(ex: &Extract, id: &str) -> bool {
        ex.fields.iter().any(|f| f.id == id)
    }

    pub fn value<'a>(ex: &'a Extract, id: &str) -> Option<&'a str> {
        ex.fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.value.as_str())
    }

    /// The engine's check: nothing removable left in a removed category.
    pub fn assert_clean(ex: &Extract, opts: &CleanOptions) {
        for f in &ex.fields {
            assert!(
                !(f.removable && opts.removes(f.category)),
                "left after clean: {f:?}"
            );
        }
    }

    /// Every field edited by `key` shows `want` (lists compared loosely).
    pub fn assert_key(ex: &Extract, key: EditKey, want: Option<&str>) {
        let norm = |s: &str| -> Vec<String> {
            let mut v = super::keywords(s);
            v.sort();
            v
        };
        let with: Vec<_> = ex.fields.iter().filter(|f| f.edit == Some(key)).collect();
        match want {
            None => assert!(with.is_empty(), "{key:?} still present: {with:?}"),
            Some(w) => {
                assert!(!with.is_empty(), "{key:?} missing; fields: {:?}", ex.fields);
                for f in with {
                    match key {
                        EditKey::Gps => {
                            let a = crate::meta::GeoPoint::parse(&f.value).unwrap();
                            let b = crate::meta::GeoPoint::parse(w).unwrap();
                            assert!(
                                (a.lat - b.lat).abs() < 1e-5 && (a.lon - b.lon).abs() < 1e-5,
                                "{f:?}"
                            );
                        }
                        EditKey::Keywords => assert_eq!(norm(&f.value), norm(w), "{f:?}"),
                        EditKey::DateTaken => assert_eq!(super::nice_date(&f.value), w, "{f:?}"),
                        _ => assert_eq!(f.value, w, "{f:?}"),
                    }
                }
            }
        }
    }

    /// Clean with "everything" (dropping orientation and profile too) and
    /// with "location + people", and check the engine's invariant.
    pub fn clean_suite(tag: &str, sample: &[u8], ext: &str, read: ReadFn, clean: CleanFn) {
        let t = Tmp::new(tag);
        let src = t.file(&format!("in.{ext}"), sample);
        let before = std::fs::read(&src).unwrap();
        let mut all = CleanOptions::everything();
        all.keep_orientation = false;
        all.keep_color_profile = false;
        for (i, opts) in [
            all,
            CleanOptions::everything(),
            CleanOptions::only(&[Category::Location, Category::People]),
            CleanOptions::only(&[Category::Hidden]),
            CleanOptions::only(&[Category::Device, Category::Dates, Category::Software]),
            CleanOptions::only(&[Category::Description]),
        ]
        .iter()
        .enumerate()
        {
            let dst = t.path(&format!("out{i}.{ext}"));
            clean(&src, &dst, opts).unwrap();
            let ex = read(&dst).unwrap();
            assert_clean(&ex, opts);
            if opts.removes(Category::Location) {
                assert!(ex.gps.is_none(), "gps left: {opts:?}");
            }
            // Cleaning twice changes nothing more.
            let dst2 = t.path(&format!("out{i}b.{ext}"));
            clean(&dst, &dst2, opts).unwrap();
            assert_eq!(read(&dst2).unwrap().fields, ex.fields);
        }
        assert_eq!(std::fs::read(&src).unwrap(), before, "source changed");
    }

    /// Sets title, author, date, GPS (with a Kurdish title and author),
    /// checks the read, then removes them and checks again.
    pub fn edit_suite(tag: &str, sample: &[u8], ext: &str, read: ReadFn, edit: EditFn) {
        let t = Tmp::new(tag);
        let src = t.file(&format!("in.{ext}"), sample);
        let a = t.path(&format!("a.{ext}"));
        let ku = super::sample_values::TITLE_KU;
        let sets = [
            (EditKey::Title, ku),
            (EditKey::Author, "ئاراس محەمەد"),
            (EditKey::Copyright, "New Studio"),
            (EditKey::Description, "Plain description"),
            (EditKey::Keywords, "one, two, سێ"),
            (EditKey::DateTaken, "2023-02-03 04:05:06"),
            (EditKey::Gps, "-33.8688, 151.2093"),
        ];
        let edits: Vec<Edit> = sets.iter().map(|(k, v)| Edit::set(*k, v)).collect();
        edit(&src, &a, &edits).unwrap();
        let ex = read(&a).unwrap();
        for (k, v) in sets {
            assert_key(&ex, k, Some(v));
        }
        let g = ex.gps.expect("gps after edit");
        assert!((g.lat + 33.8688).abs() < 1e-5 && (g.lon - 151.2093).abs() < 1e-5);
        assert_eq!(ex.taken.as_deref(), Some("2023-02-03 04:05:06"));

        // ASCII values for the keys that only had Unicode so far.
        let b = t.path(&format!("b.{ext}"));
        edit(&a, &b, &[Edit::set(EditKey::Author, "Jane Roe")]).unwrap();
        assert_key(&read(&b).unwrap(), EditKey::Author, Some("Jane Roe"));

        let c = t.path(&format!("c.{ext}"));
        let clears: Vec<Edit> = sets.iter().map(|(k, _)| Edit::clear(*k)).collect();
        edit(&b, &c, &clears).unwrap();
        let ex = read(&c).unwrap();
        for (k, _) in sets {
            assert_key(&ex, k, None);
        }
        assert!(ex.gps.is_none());
    }

    /// Every truncated prefix (and some garbage) reads without panicking.
    pub fn robustness(tag: &str, sample: &[u8], ext: &str, read: ReadFn, clean: CleanFn) {
        let t = Tmp::new(tag);
        let step = (sample.len() / 150).max(1);
        let mut cut = 0;
        while cut < sample.len() {
            let p = t.file(&format!("cut.{ext}"), &sample[..cut]);
            let _ = read(&p);
            let _ = clean(
                &p,
                &t.path(&format!("cut-out.{ext}")),
                &CleanOptions::everything(),
            );
            cut += step;
        }
        // Flipped bytes across the file.
        for seed in 0..20usize {
            let mut b = sample.to_vec();
            for k in 0..8 {
                let i = (seed * 7919 + k * 104_729) % b.len();
                b[i] ^= 0x5A;
            }
            let p = t.file(&format!("flip.{ext}"), &b);
            let _ = read(&p);
            let _ = clean(
                &p,
                &t.path(&format!("flip-out.{ext}")),
                &CleanOptions::everything(),
            );
        }
        let p = t.file(&format!("junk.{ext}"), b"this is not a picture at all");
        assert!(read(&p).is_err() || read(&p).unwrap().fields.is_empty());
    }

    /// Decodes two files and checks the pixels are identical.
    pub fn same_pixels(a: &[u8], b: &[u8]) {
        let x = image::load_from_memory(a).unwrap().to_rgba8();
        let y = image::load_from_memory(b).unwrap().to_rgba8();
        assert_eq!(x.dimensions(), y.dimensions());
        assert!(x.as_raw() == y.as_raw(), "pixels differ");
    }
}
