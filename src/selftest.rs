//! `--selftest`: writes a sample file of every format into a temporary
//! folder, then reads, cleans (verifying the result) and edits each one.
//! Nothing outside that folder is touched, and it is deleted at the end.

use crate::detect::{self, Format};
use crate::engine::{self, CleanJob, Output};
use crate::formats;
use crate::meta::{Category, CleanOptions, Edit, EditKey};
use std::path::Path;

/// `(file name, bytes)` of every sample the format modules provide.
pub fn samples() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("photo.jpg", formats::jpeg::sample()),
        ("picture.png", formats::png::sample()),
        ("picture.webp", formats::webp::sample()),
        ("scan.tif", formats::tiff::sample()),
        ("animation.gif", formats::gif::sample()),
        ("phone.heic", formats::heif::sample()),
        ("report.docx", formats::ooxml::sample()),
        ("notes.odt", formats::odf::sample()),
        ("paper.pdf", formats::pdf::sample()),
        ("letter.rtf", formats::rtf::sample()),
        ("song.mp3", formats::audio::sample()),
        ("clip.mp4", formats::mp4::sample()),
    ]
}

fn check(dir: &Path, name: &str, bytes: &[u8], log: &mut dyn FnMut(String)) -> bool {
    if bytes.is_empty() {
        log(format!("SKIP {name}: no sample available"));
        return true;
    }
    let path = dir.join(name);
    if let Err(e) = std::fs::write(&path, bytes) {
        log(format!("FAIL {name}: could not write the sample: {e}"));
        return false;
    }
    let meta = engine::inspect(&path);
    if meta.format == Format::Other {
        log(format!("FAIL {name}: format not recognised"));
        return false;
    }
    if let Some(e) = &meta.error {
        log(format!("FAIL {name}: read failed: {e}"));
        return false;
    }
    let flags: Vec<&str> = meta.flags().iter().map(|f| f.label()).collect();
    let removable = meta.removable_count();
    if removable == 0 {
        log(format!(
            "FAIL {name}: the sample shows no removable metadata"
        ));
        return false;
    }
    let mut ok = true;
    // Location and names only, as a copy.
    let partial = CleanJob {
        opts: CleanOptions::only(&[Category::Location, Category::People]),
        output: Output::Suffix("_ln".into()),
        keep_file_dates: true,
    };
    let r = engine::clean_file(&path, &partial);
    if !r.ok() {
        log(format!(
            "FAIL {name}: location+names clean: {}",
            r.error
                .clone()
                .unwrap_or_else(|| format!("{} fields left", r.left.len()))
        ));
        ok = false;
    }
    // Everything, as a copy.
    let full = CleanJob {
        opts: CleanOptions::everything(),
        output: Output::Suffix("_clean".into()),
        keep_file_dates: true,
    };
    let r = engine::clean_file(&path, &full);
    if !r.ok() {
        log(format!(
            "FAIL {name}: full clean: {}",
            r.error.clone().unwrap_or_else(|| {
                let left: Vec<String> = r.left.iter().map(|f| f.label.clone()).collect();
                format!("left: {}", left.join(", "))
            })
        ));
        ok = false;
    } else if detect::detect(&r.dst) != meta.format {
        log(format!(
            "FAIL {name}: the cleaned file is no longer recognised"
        ));
        ok = false;
    }
    let removed = r.removed.len();
    // Edit the title (or the comment where that is all there is).
    let key = [EditKey::Title, EditKey::Description]
        .into_iter()
        .find(|k| meta.editable.contains(k));
    let mut edited = "not editable".to_string();
    if let Some(key) = key {
        let value = "MetaForge self-test ✓ تاقیکردنەوە";
        match engine::edit_file(
            &path,
            &[Edit::set(key, value)],
            &Output::Suffix("_edit".into()),
            true,
        ) {
            Ok(out) => {
                let back = engine::inspect(&out);
                if back
                    .fields
                    .iter()
                    .any(|f| f.edit == Some(key) && f.value == value)
                {
                    edited = format!("{} edited", key.key());
                } else {
                    log(format!("FAIL {name}: the edited value does not read back"));
                    ok = false;
                }
            }
            Err(e) => {
                log(format!("FAIL {name}: edit: {e}"));
                ok = false;
            }
        }
    }
    if ok {
        log(format!(
            "ok   {name}: {} fields ({}), {removed} removed and verified, {edited}",
            meta.fields.len(),
            if flags.is_empty() {
                "no badges".to_string()
            } else {
                flags.join(", ")
            }
        ));
    }
    ok
}

/// Runs the self-test; `log` receives one line per step. Returns success.
pub fn run(mut log: impl FnMut(String)) -> bool {
    let dir = std::env::temp_dir().join(format!("metaforge-selftest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log(format!("FAIL could not create {}: {e}", dir.display()));
        return false;
    }
    log(format!("Working in {}", dir.display()));
    let mut ok = true;
    for (name, bytes) in samples() {
        ok &= check(&dir, name, &bytes, &mut log);
    }
    // Windows traces: a "downloaded from" note is found and removed.
    let file = dir.join("download.txt");
    let traced = std::fs::write(&file, b"hello").is_ok()
        && crate::winmeta::write_stream(
            &file,
            "Zone.Identifier",
            b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/file.txt\r\n",
        )
        .is_ok();
    if traced {
        let r = engine::clean_file(
            &file,
            &CleanJob {
                opts: CleanOptions::everything(),
                output: Output::Replace,
                keep_file_dates: true,
            },
        );
        if r.ok() && crate::winmeta::streams(&file).is_empty() {
            log("ok   download.txt: \"downloaded from\" note removed".into());
        } else {
            log("FAIL download.txt: the download note was not removed".into());
            ok = false;
        }
    }
    // METAFORGE_SELFTEST_KEEP=1 keeps the folder, to check the files with other tools.
    if std::env::var_os("METAFORGE_SELFTEST_KEEP").is_some() {
        log(format!("Kept {}", dir.display()));
    } else {
        let _ = std::fs::remove_dir_all(&dir);
        log(format!("Removed {}", dir.display()));
    }
    ok
}
