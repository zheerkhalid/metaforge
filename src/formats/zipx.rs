//! ZIP containers (Office and OpenDocument files): reading parts, and
//! rewriting an archive entry by entry (keep the compressed bytes, replace
//! the content, or drop the entry) while keeping order and compression.
//!
//! The rewrite never copies "extra fields" (NTFS times, owner ids) or the
//! archive comment. With `reset_times`, every entry gets the neutral ZIP
//! time 1980-01-01 00:00.

use crate::meta::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipArchive, ZipWriter};

/// Largest part read into memory (protects against ZIP bombs).
const MAX_PART: u64 = 512 * 1024 * 1024;

/// An opened archive.
pub struct Zip {
    archive: ZipArchive<BufReader<File>>,
    names: Vec<String>,
}

fn zip_err(e: zip::result::ZipError) -> String {
    format!("The file is not a valid Office/OpenDocument package ({e}).")
}

impl Zip {
    pub fn open(path: &Path) -> Result<Zip> {
        let file = File::open(path).map_err(|e| format!("Could not open the file: {e}"))?;
        let archive = ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
        let names = (0..archive.len())
            .map(|i| archive.name_for_index(i).unwrap_or_default().to_string())
            .collect();
        Ok(Zip { archive, names })
    }

    /// Entry names in archive order.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.iter().any(|n| n == name)
    }

    /// The uncompressed size of an entry.
    pub fn size(&mut self, name: &str) -> Option<u64> {
        self.archive.by_name(name).ok().map(|f| f.size())
    }

    /// The bytes of an entry (`None` when missing).
    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>> {
        let mut file = match self.archive.by_name(name) {
            Ok(f) => f,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(zip_err(e)),
        };
        if file.encrypted() {
            return Err("The file is password-protected.".into());
        }
        if file.size() > MAX_PART {
            return Err(format!("The part {name} is too large to read."));
        }
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.by_ref()
            .take(MAX_PART)
            .read_to_end(&mut buf)
            .map_err(|e| format!("The part {name} is damaged ({e})."))?;
        Ok(Some(buf))
    }

    /// An entry as text (UTF-8, a BOM is skipped).
    pub fn text(&mut self, name: &str) -> Result<Option<String>> {
        Ok(self.read(name)?.map(|b| {
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            String::from_utf8_lossy(b).into_owned()
        }))
    }
}

/// What happens to one entry in [`rewrite`].
pub enum Entry {
    Keep,
    Replace(Vec<u8>),
    Drop,
}

/// Rewrites `src` into `dst`. `plan` maps entry names to replacements or
/// drops (absent = keep); `add` appends new entries. `mimetype` (ODF) is
/// always written first and stored.
pub fn rewrite(
    src: &Path,
    dst: &Path,
    mut plan: HashMap<String, Entry>,
    add: Vec<(String, Vec<u8>)>,
    reset_times: bool,
) -> Result<()> {
    let file = File::open(src).map_err(|e| format!("Could not open the file: {e}"))?;
    let mut archive = ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
    let out = File::create(dst).map_err(|e| format!("Could not write the new file: {e}"))?;
    let mut writer = ZipWriter::new(BufWriter::new(out));
    // `mimetype` first, then the rest in their original order.
    let mut order: Vec<usize> = (0..archive.len()).collect();
    if let Some(pos) = order
        .iter()
        .position(|&i| archive.name_for_index(i) == Some("mimetype"))
    {
        let i = order.remove(pos);
        order.insert(0, i);
    }
    for i in order {
        let name = archive.name_for_index(i).unwrap_or_default().to_string();
        let entry = plan.remove(&name).unwrap_or(Entry::Keep);
        copy_entry(&mut archive, &mut writer, i, &name, entry, reset_times)?;
    }
    for (name, data) in add {
        let method = if name == "mimetype" {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(DateTime::default());
        writer.start_file(name, opts).map_err(write_err)?;
        writer.write_all(&data).map_err(write_err)?;
    }
    let mut inner = writer.finish().map_err(write_err)?;
    inner.flush().map_err(write_err)?;
    Ok(())
}

fn write_err(e: impl std::fmt::Display) -> String {
    format!("Could not write the new file: {e}")
}

fn copy_entry<R: Read + Seek, W: Write + Seek>(
    archive: &mut ZipArchive<R>,
    writer: &mut ZipWriter<W>,
    index: usize,
    name: &str,
    entry: Entry,
    reset_times: bool,
) -> Result<()> {
    if matches!(entry, Entry::Drop) {
        return Ok(());
    }
    let file = archive.by_index_raw(index).map_err(zip_err)?;
    if file.encrypted() {
        return Err("The file is password-protected; it cannot be changed.".into());
    }
    let time = if reset_times {
        DateTime::default()
    } else {
        file.last_modified()
            .filter(|t| t.is_valid())
            .unwrap_or_default()
    };
    let stored = name == "mimetype" || file.compression() == CompressionMethod::Stored;
    let method = if stored {
        CompressionMethod::Stored
    } else {
        CompressionMethod::Deflated
    };
    let opts = SimpleFileOptions::default()
        .compression_method(method)
        .last_modified_time(time);
    if file.is_dir() {
        drop(file);
        return writer.add_directory(name, opts).map_err(write_err);
    }
    match entry {
        Entry::Replace(data) => {
            drop(file);
            writer.start_file(name, opts).map_err(write_err)?;
            writer.write_all(&data).map_err(write_err)
        }
        _ if name == "mimetype" && file.compression() != CompressionMethod::Stored => {
            // ODF requires `mimetype` stored: recompress.
            drop(file);
            let mut data = Vec::new();
            archive
                .by_index(index)
                .map_err(zip_err)?
                .take(4096)
                .read_to_end(&mut data)
                .map_err(write_err)?;
            writer.start_file(name, opts).map_err(write_err)?;
            writer.write_all(&data).map_err(write_err)
        }
        _ => writer
            .raw_copy_file_touch(file, time, None)
            .map_err(write_err),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds a ZIP in memory: `(name, data, stored)`.
    pub fn build(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut w = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, stored) in entries {
            let method = if *stored {
                CompressionMethod::Stored
            } else {
                CompressionMethod::Deflated
            };
            let opts = SimpleFileOptions::default()
                .compression_method(method)
                .last_modified_time(DateTime::from_date_and_time(2024, 5, 1, 10, 20, 30).unwrap());
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.set_comment("made by Test User");
        w.finish().unwrap().into_inner()
    }

    pub fn temp_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "metaforge-doc-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rewrite_keeps_replaces_drops_and_resets_times() {
        let dir = temp_dir("zipx");
        let src = dir.join("a.zip");
        let dst = dir.join("b.zip");
        std::fs::write(
            &src,
            build(&[
                ("a.txt", b"hello hello hello", false),
                ("mimetype", b"application/x-test", false),
                ("b.txt", b"drop me", false),
                ("c.bin", b"stored", true),
            ]),
        )
        .unwrap();
        let mut plan = HashMap::new();
        plan.insert("a.txt".to_string(), Entry::Replace(b"new".to_vec()));
        plan.insert("b.txt".to_string(), Entry::Drop);
        rewrite(
            &src,
            &dst,
            plan,
            vec![("d.txt".into(), b"added".to_vec())],
            true,
        )
        .unwrap();
        let mut z = Zip::open(&dst).unwrap();
        assert_eq!(z.names(), ["mimetype", "a.txt", "c.bin", "d.txt"]);
        assert_eq!(z.read("a.txt").unwrap().unwrap(), b"new");
        assert_eq!(z.read("c.bin").unwrap().unwrap(), b"stored");
        assert_eq!(z.read("d.txt").unwrap().unwrap(), b"added");
        assert_eq!(z.read("missing").unwrap(), None);
        let mut ar = ZipArchive::new(File::open(&dst).unwrap()).unwrap();
        assert!(ar.comment().is_empty());
        for i in 0..ar.len() {
            let f = ar.by_index(i).unwrap();
            assert_eq!(
                f.last_modified().unwrap(),
                DateTime::default(),
                "{}",
                f.name()
            );
            if f.name() == "mimetype" || f.name() == "c.bin" {
                assert_eq!(f.compression(), CompressionMethod::Stored);
            }
            assert!(f.extra_data().is_none_or(|d| d.is_empty()));
        }
        // Keeping times.
        rewrite(&src, &dst, HashMap::new(), vec![], false).unwrap();
        let mut ar = ZipArchive::new(File::open(&dst).unwrap()).unwrap();
        let t = ar.by_name("c.bin").unwrap().last_modified().unwrap();
        assert_eq!((t.year(), t.month(), t.day()), (2024, 5, 1));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn garbage_is_an_error() {
        let dir = temp_dir("zipx-bad");
        let p = dir.join("bad.zip");
        std::fs::write(&p, b"PK\x03\x04 not really a zip").unwrap();
        assert!(Zip::open(&p).is_err());
        assert!(rewrite(&p, &dir.join("o.zip"), HashMap::new(), vec![], true).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
