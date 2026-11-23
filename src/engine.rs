//! What the window and the command line do with files: inspect, clean (with
//! verification), edit, set file dates, rename from metadata, export and
//! compare. Format details live in `formats`; this module adds the file
//! handling around them (temporary files, output choices, Windows traces).

use crate::detect::{self, Format};
use crate::formats;
use crate::meta::{Category, CleanOptions, Edit, Field, Flag, Meta, Result};
use crate::util::{self, DateTime};
use crate::winmeta::{self, Times};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Reads everything about one file. Never fails: problems end up in `error`.
pub fn inspect(path: &Path) -> Meta {
    let format = detect::detect(path);
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut meta = Meta {
        path: path.to_path_buf(),
        format,
        size,
        fields: Vec::new(),
        notes: Vec::new(),
        gps: None,
        taken: None,
        editable: Vec::new(),
        error: None,
    };
    if !path.is_file() {
        meta.error = Some("The file could not be found.".into());
        return meta;
    }
    match std::panic::catch_unwind(|| formats::read(format, path)) {
        Ok(Ok(x)) => {
            meta.fields = x.fields;
            meta.notes = x.notes;
            meta.gps = x.gps;
            meta.taken = x.taken;
            meta.editable = x.editable;
        }
        Ok(Err(e)) => meta.error = Some(e),
        Err(_) => meta.error = Some("This file could not be read (it may be damaged).".into()),
    }
    if format == Format::Other {
        meta.notes.push(
            "MetaForge does not know the inside of this kind of file; only Windows traces are shown."
                .into(),
        );
    }
    meta.fields.push(
        Field::new(
            "file:size",
            "File size",
            util::human_size(size),
            Category::Technical,
            "File",
        )
        .fixed(),
    );
    meta.fields.push(
        Field::new(
            "file:type",
            "Type",
            format.name(),
            Category::Technical,
            "File",
        )
        .fixed(),
    );
    meta.fields.extend(winmeta::fields(path));
    meta
}

/// Files of known types in the given files and folders (folders recursively).
/// Files named directly are always included, whatever their type.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            walk(p, &mut out, 0);
        } else if p.is_file() {
            out.push(p.clone());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.to_string_lossy().to_lowercase()));
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if detect::known_extension(&p) {
            out.push(p);
        }
    }
}

/// Where cleaned or edited files go.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Output {
    /// Overwrite the original (after the result was verified).
    Replace,
    /// A copy next to the original: `photo.jpg` -> `photo<suffix>.jpg`.
    Suffix(String),
    /// A copy with the same name in another folder.
    Folder(PathBuf),
}

impl Output {
    pub fn describe(&self) -> String {
        match self {
            Output::Replace => "Replace the original files".into(),
            Output::Suffix(s) => format!("Save a copy next to each file, ending in \"{s}\""),
            Output::Folder(f) => format!("Save copies in {}", f.display()),
        }
    }
}

/// `name.ext` -> `name (2).ext` until the path is free.
fn unique(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    for n in 2.. {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Where the result for `src` goes.
pub fn output_path(src: &Path, output: &Output) -> PathBuf {
    match output {
        Output::Replace => src.to_path_buf(),
        Output::Suffix(suffix) => {
            let stem = src
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = src
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            let suffix = if suffix.is_empty() { "_clean" } else { suffix };
            unique(src.with_file_name(format!("{stem}{suffix}{ext}")))
        }
        Output::Folder(dir) => {
            let name = src
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_default();
            unique(dir.join(name))
        }
    }
}

/// A temporary file next to `target`, removed again if dropped unused.
struct Temp(PathBuf);

impl Temp {
    fn near(target: &Path) -> Temp {
        let dir = target.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = target
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = target
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let pid = std::process::id();
        let mut n = 0;
        loop {
            let p = dir.join(format!("~{stem}.metaforge-{pid}-{n}{ext}"));
            if !p.exists() {
                return Temp(p);
            }
            n += 1;
        }
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// How a clean is done.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanJob {
    pub opts: CleanOptions,
    pub output: Output,
    /// Give the result the original's created and modified dates.
    pub keep_file_dates: bool,
}

/// The outcome for one file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanResult {
    pub src: PathBuf,
    /// Where the cleaned file is (same as `src` when replacing).
    pub dst: PathBuf,
    pub format: Format,
    /// Fields that were removed (as they were before).
    pub removed: Vec<Field>,
    /// Fields in the chosen categories that are still there afterwards
    /// (verification failures; empty when all went well).
    pub left: Vec<Field>,
    pub flags_before: Vec<Flag>,
    pub flags_after: Vec<Flag>,
    pub size_before: u64,
    pub size_after: u64,
    /// The result was read back and nothing chosen was left.
    pub verified: bool,
    /// Nothing to do: the file had nothing in the chosen categories.
    pub already_clean: bool,
    pub error: Option<String>,
}

impl CleanResult {
    fn failed(src: &Path, format: Format, error: String) -> CleanResult {
        CleanResult {
            src: src.to_path_buf(),
            dst: src.to_path_buf(),
            format,
            removed: Vec::new(),
            left: Vec::new(),
            flags_before: Vec::new(),
            flags_after: Vec::new(),
            size_before: 0,
            size_after: 0,
            verified: false,
            already_clean: false,
            error: Some(error),
        }
    }

    pub fn ok(&self) -> bool {
        self.error.is_none() && self.left.is_empty()
    }
}

/// Fields a clean with `opts` is expected to remove.
pub fn targeted<'a>(fields: &'a [Field], opts: &CleanOptions) -> Vec<&'a Field> {
    fields
        .iter()
        .filter(|f| f.removable && opts.remove.contains(f.category))
        .collect()
}

fn same_field(a: &Field, b: &Field) -> bool {
    a.id == b.id && a.value == b.value
}

/// Cleans one file: the format module writes a temporary file, which is read
/// back and checked before it replaces anything.
pub fn clean_file(src: &Path, job: &CleanJob) -> CleanResult {
    let before = inspect(src);
    let format = before.format;
    if let Some(e) = &before.error {
        if format != Format::Other {
            return CleanResult::failed(src, format, e.clone());
        }
    }
    let wanted: Vec<Field> = targeted(&before.fields, &job.opts)
        .into_iter()
        .cloned()
        .collect();
    let dst = output_path(src, &job.output);
    if let Some(parent) = dst.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return CleanResult::failed(src, format, format!("{}: {e}", parent.display()));
        }
    }
    let temp = Temp::near(&dst);
    let formats_opts = job.opts;
    let done = std::panic::catch_unwind(|| formats::clean(format, src, &temp.0, &formats_opts));
    match done {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return CleanResult::failed(src, format, e),
        Err(_) => {
            return CleanResult::failed(
                src,
                format,
                "Cleaning failed unexpectedly; the original was not changed.".into(),
            )
        }
    }
    if !temp.0.is_file() {
        return CleanResult::failed(src, format, "No cleaned file was written.".into());
    }
    // Windows traces: keep the streams unless they are to be removed. A copy
    // made with CopyFile (which some formats use) already carries them along.
    let _ = winmeta::delete_all_streams(&temp.0);
    if !job.opts.removes(Category::Windows) {
        winmeta::copy_streams(src, &temp.0);
    }
    if job.keep_file_dates {
        if let Some(t) = winmeta::times(src) {
            let _ = winmeta::set_times(
                &temp.0,
                Times {
                    accessed: None,
                    ..t
                },
            );
        }
    }
    // Verify on the temporary file, before anything is replaced.
    let after = inspect(&temp.0);
    if let Some(e) = after.error.as_ref().filter(|_| format != Format::Other) {
        return CleanResult::failed(
            src,
            format,
            format!("The cleaned file could not be read back ({e}); the original was not changed."),
        );
    }
    let left: Vec<Field> = targeted(&after.fields, &job.opts)
        .into_iter()
        .filter(|f| f.category != Category::Windows || !f.id.starts_with("file:"))
        .cloned()
        .collect();
    let removed: Vec<Field> = wanted
        .iter()
        .filter(|w| !after.fields.iter().any(|a| same_field(a, w)))
        .cloned()
        .collect();
    let size_after = std::fs::metadata(&temp.0).map(|m| m.len()).unwrap_or(0);
    let flags_after = after.flags();
    // Move into place.
    let placed = if job.output == Output::Replace {
        std::fs::rename(&temp.0, src).map_err(|e| e.to_string())
    } else {
        std::fs::rename(&temp.0, &dst).map_err(|e| e.to_string())
    };
    if let Err(e) = placed {
        return CleanResult::failed(src, format, format!("Could not save the cleaned file: {e}"));
    }
    CleanResult {
        src: src.to_path_buf(),
        dst,
        format,
        already_clean: wanted.is_empty(),
        verified: left.is_empty(),
        removed,
        left,
        flags_before: before.flags(),
        flags_after,
        size_before: before.size,
        size_after,
        error: None,
    }
}

/// Cleans several files, reporting each result as it is done.
/// `progress(done, total, result)`. Stops early when `cancel` is set.
pub fn clean_batch(
    paths: &[PathBuf],
    job: &CleanJob,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize, &CleanResult),
) -> Vec<CleanResult> {
    let mut out = Vec::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let r = clean_file(p, job);
        progress(i + 1, paths.len(), &r);
        out.push(r);
    }
    out
}

/// Totals for a batch report.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub files: usize,
    pub cleaned: usize,
    pub already_clean: usize,
    pub failed: usize,
    pub not_verified: usize,
    pub fields_removed: usize,
}

pub fn summarize(results: &[CleanResult]) -> Summary {
    let mut s = Summary {
        files: results.len(),
        ..Default::default()
    };
    for r in results {
        if r.error.is_some() {
            s.failed += 1;
        } else if !r.left.is_empty() {
            s.not_verified += 1;
        } else if r.already_clean {
            s.already_clean += 1;
        } else {
            s.cleaned += 1;
        }
        s.fields_removed += r.removed.len();
    }
    s
}

/// Applies edits; returns where the result is.
pub fn edit_file(
    src: &Path,
    edits: &[Edit],
    output: &Output,
    keep_file_dates: bool,
) -> Result<PathBuf> {
    let format = detect::detect(src);
    if edits.is_empty() {
        return Ok(src.to_path_buf());
    }
    let dst = output_path(src, output);
    let temp = Temp::near(&dst);
    match std::panic::catch_unwind(|| formats::edit(format, src, &temp.0, edits)) {
        Ok(r) => r?,
        Err(_) => return Err("Editing failed unexpectedly; the original was not changed.".into()),
    }
    // Check that the new values read back.
    let after = inspect(&temp.0);
    if let Some(e) = after.error {
        return Err(format!(
            "The edited file could not be read back ({e}); the original was not changed."
        ));
    }
    let _ = winmeta::delete_all_streams(&temp.0);
    winmeta::copy_streams(src, &temp.0);
    if keep_file_dates {
        if let Some(t) = winmeta::times(src) {
            let _ = winmeta::set_times(
                &temp.0,
                Times {
                    accessed: None,
                    ..t
                },
            );
        }
    }
    let target = if *output == Output::Replace {
        src
    } else {
        &dst
    };
    std::fs::rename(&temp.0, target).map_err(|e| format!("Could not save the edited file: {e}"))?;
    Ok(target.to_path_buf())
}

/// Sets file timestamps from `YYYY-MM-DD HH:MM:SS` local text; `None` leaves one as it is.
pub fn set_file_dates(
    path: &Path,
    created: Option<&str>,
    modified: Option<&str>,
    accessed: Option<&str>,
) -> Result<()> {
    let parse = |s: Option<&str>| -> Result<Option<std::time::SystemTime>> {
        match s.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(t) => DateTime::parse(t)
                .map(|d| Some(util::from_local(d)))
                .ok_or_else(|| format!("\"{t}\" is not a date (use YYYY-MM-DD HH:MM:SS).")),
        }
    };
    winmeta::set_times(
        path,
        Times {
            created: parse(created)?,
            modified: parse(modified)?,
            accessed: parse(accessed)?,
        },
    )
}

/// Sets the file's created and modified dates to the date in its metadata.
pub fn copy_date_to_file(meta: &Meta) -> Result<String> {
    let taken = meta
        .taken
        .as_deref()
        .ok_or("This file has no date in its metadata.")?;
    let dt = DateTime::parse(taken).ok_or("The date in the metadata could not be understood.")?;
    let text = dt.text();
    set_file_dates(&meta.path, Some(&text), Some(&text), None)?;
    Ok(text)
}

/// One planned rename.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Rename {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Why this one cannot be renamed (no date, name taken...).
    pub problem: Option<String>,
}

/// The placeholders a rename pattern understands.
pub const RENAME_TOKENS: &[(&str, &str)] = &[
    ("{date}", "date taken, 2024-05-01"),
    ("{time}", "time taken, 10-20-30"),
    ("{year}", "year"),
    ("{month}", "month"),
    ("{day}", "day"),
    ("{name}", "the current name"),
    ("{camera}", "camera or device model"),
    ("{title}", "title"),
    ("{n}", "a running number, 001"),
];

fn field_value<'a>(meta: &'a Meta, labels: &[&str]) -> Option<&'a str> {
    labels.iter().find_map(|l| {
        meta.fields
            .iter()
            .find(|f| f.label.eq_ignore_ascii_case(l) && !f.value.trim().is_empty())
            .map(|f| f.value.trim())
    })
}

/// Plans renames from a pattern such as `{date}_{time}` (the extension is kept).
pub fn rename_plan(metas: &[Meta], pattern: &str) -> Vec<Rename> {
    let mut taken_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, m) in metas.iter().enumerate() {
        let stem = m
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = m
            .path
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let dt = m.taken.as_deref().and_then(DateTime::parse);
        let needs_date = ["{date}", "{time}", "{year}", "{month}", "{day}"]
            .iter()
            .any(|t| pattern.contains(t));
        let mut problem = None;
        if needs_date && dt.is_none() {
            problem = Some("No date in the metadata".to_string());
        }
        let camera =
            field_value(m, &["Camera model", "Phone model", "Model", "Device model"]).unwrap_or("");
        let title = m
            .fields
            .iter()
            .find(|f| f.edit == Some(crate::meta::EditKey::Title) && !f.value.trim().is_empty())
            .map(|f| f.value.trim().to_string())
            .unwrap_or_default();
        let mut name = pattern.to_string();
        if let Some(d) = dt {
            name = name
                .replace(
                    "{date}",
                    &format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
                )
                .replace(
                    "{time}",
                    &format!("{:02}-{:02}-{:02}", d.hour, d.minute, d.second),
                )
                .replace("{year}", &format!("{:04}", d.year))
                .replace("{month}", &format!("{:02}", d.month))
                .replace("{day}", &format!("{:02}", d.day));
        }
        name = name
            .replace("{name}", &stem)
            .replace("{camera}", camera)
            .replace("{title}", &title)
            .replace("{n}", &format!("{:03}", i + 1));
        let name = util::safe_file_name(&name);
        let dir = m.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut to = dir.join(format!("{name}{ext}"));
        if problem.is_none() && name.is_empty() {
            problem = Some("The pattern gives an empty name".into());
        }
        if problem.is_none() && to != m.path {
            // Avoid clashes with existing files and with other planned names.
            let mut n = 2;
            while (to.exists() && to != m.path)
                || taken_names.contains(&to.to_string_lossy().to_lowercase())
            {
                to = dir.join(format!("{name} ({n}){ext}"));
                n += 1;
            }
        }
        if problem.is_none() {
            taken_names.insert(to.to_string_lossy().to_lowercase());
        }
        out.push(Rename {
            from: m.path.clone(),
            to: if problem.is_some() {
                m.path.clone()
            } else {
                to
            },
            problem,
        });
    }
    out
}

/// Carries out a plan; returns the renames that failed with the reason.
pub fn apply_renames(plan: &[Rename]) -> Vec<(PathBuf, String)> {
    let mut failed = Vec::new();
    for r in plan {
        if r.problem.is_some() || r.from == r.to {
            continue;
        }
        if r.to.exists() {
            failed.push((
                r.from.clone(),
                "A file with that name already exists".into(),
            ));
            continue;
        }
        if let Err(e) = std::fs::rename(&r.from, &r.to) {
            failed.push((r.from.clone(), e.to_string()));
        }
    }
    failed
}

/// Metadata of several files as JSON.
pub fn to_json(metas: &[Meta]) -> String {
    #[derive(Serialize)]
    struct FileOut<'a> {
        path: String,
        format: &'a str,
        size: u64,
        flags: Vec<&'static str>,
        gps: Option<[f64; 2]>,
        taken: Option<&'a str>,
        notes: &'a [String],
        error: Option<&'a str>,
        fields: Vec<FieldOut<'a>>,
    }
    #[derive(Serialize)]
    struct FieldOut<'a> {
        group: &'static str,
        label: &'a str,
        value: &'a str,
        source: &'a str,
        id: &'a str,
        removable: bool,
    }
    let files: Vec<FileOut> = metas
        .iter()
        .map(|m| FileOut {
            path: m.path.to_string_lossy().into_owned(),
            format: m.format.short(),
            size: m.size,
            flags: m.flags().into_iter().map(|f| f.key()).collect(),
            gps: m.gps.map(|g| [g.lat, g.lon]),
            taken: m.taken.as_deref(),
            notes: &m.notes,
            error: m.error.as_deref(),
            fields: m
                .fields
                .iter()
                .map(|f| FieldOut {
                    group: f.category.key(),
                    label: &f.label,
                    value: &f.value,
                    source: &f.source,
                    id: &f.id,
                    removable: f.removable,
                })
                .collect(),
        })
        .collect();
    serde_json::to_string_pretty(&files).unwrap_or_else(|_| "[]".into())
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Metadata of several files as CSV: one row per field. Starts with a UTF-8
/// byte-order mark so Excel shows Kurdish and Arabic text properly.
pub fn to_csv(metas: &[Meta]) -> String {
    let mut out = String::from("\u{feff}file,group,field,value,stored in,removable\r\n");
    for m in metas {
        let path = m.path.to_string_lossy();
        for f in &m.fields {
            out.push_str(&format!(
                "{},{},{},{},{},{}\r\n",
                csv_cell(&path),
                csv_cell(f.category.title()),
                csv_cell(&f.label),
                csv_cell(&f.value),
                csv_cell(&f.source),
                if f.removable { "yes" } else { "no" }
            ));
        }
    }
    out
}

/// One row of a comparison.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CompareRow {
    pub category: Category,
    pub label: String,
    pub a: Option<String>,
    pub b: Option<String>,
}

impl CompareRow {
    pub fn same(&self) -> bool {
        self.a == self.b
    }
}

/// Lines up the fields of two files by group and label.
pub fn compare(a: &Meta, b: &Meta) -> Vec<CompareRow> {
    let key = |f: &Field| (f.category, f.label.clone(), f.source.clone());
    let mut rows: Vec<CompareRow> = Vec::new();
    let mut index: std::collections::HashMap<(Category, String, String), usize> =
        std::collections::HashMap::new();
    for f in &a.fields {
        let k = key(f);
        if let Some(&i) = index.get(&k) {
            if let Some(v) = rows[i].a.as_mut() {
                v.push_str("; ");
                v.push_str(&f.value);
            }
            continue;
        }
        index.insert(k, rows.len());
        rows.push(CompareRow {
            category: f.category,
            label: f.label.clone(),
            a: Some(f.value.clone()),
            b: None,
        });
    }
    for f in &b.fields {
        let k = key(f);
        match index.get(&k) {
            Some(&i) => match rows[i].b.as_mut() {
                Some(v) => {
                    v.push_str("; ");
                    v.push_str(&f.value);
                }
                None => rows[i].b = Some(f.value.clone()),
            },
            None => {
                index.insert(k, rows.len());
                rows.push(CompareRow {
                    category: f.category,
                    label: f.label.clone(),
                    a: None,
                    b: Some(f.value.clone()),
                });
            }
        }
    }
    rows.sort_by_key(|r| r.category);
    rows
}

/// A text report of a batch clean, for saving or copying.
pub fn report_text(results: &[CleanResult]) -> String {
    let s = summarize(results);
    let mut out = format!(
        "MetaForge cleaning report\r\n{} files: {} cleaned, {} had nothing to remove, {} failed, {} not fully cleaned. {} fields removed.\r\n\r\n",
        s.files, s.cleaned, s.already_clean, s.failed, s.not_verified, s.fields_removed
    );
    for r in results {
        out.push_str(&format!("{}\r\n", r.src.display()));
        if let Some(e) = &r.error {
            out.push_str(&format!("  Failed: {e}\r\n"));
            continue;
        }
        if r.dst != r.src {
            out.push_str(&format!("  Saved as {}\r\n", r.dst.display()));
        }
        if r.already_clean {
            out.push_str("  Nothing to remove.\r\n");
        }
        for f in &r.removed {
            out.push_str(&format!("  - removed {} ({})\r\n", f.label, f.source));
        }
        for f in &r.left {
            out.push_str(&format!("  ! still there: {} ({})\r\n", f.label, f.source));
        }
        if r.verified && !r.already_clean {
            out.push_str("  Verified: read back, nothing chosen is left.\r\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{CleanOptions, GeoPoint};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("metaforge-engine-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn output_paths_never_overwrite() {
        let dir = temp_dir("out");
        let src = dir.join("photo.jpg");
        std::fs::write(&src, b"x").unwrap();
        assert_eq!(output_path(&src, &Output::Replace), src);
        let s = output_path(&src, &Output::Suffix("_clean".into()));
        assert_eq!(s.file_name().unwrap(), "photo_clean.jpg");
        std::fs::write(&s, b"x").unwrap();
        let s2 = output_path(&src, &Output::Suffix("_clean".into()));
        assert_eq!(s2.file_name().unwrap(), "photo_clean (2).jpg");
        let f = output_path(&src, &Output::Folder(dir.join("shared")));
        assert_eq!(f, dir.join("shared").join("photo.jpg"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_files_show_windows_traces_and_clean_streams() {
        let dir = temp_dir("other");
        let src = dir.join("notes.xyz");
        std::fs::write(&src, b"plain contents").unwrap();
        winmeta::write_stream(
            &src,
            "Zone.Identifier",
            b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://example.com/x\r\n",
        )
        .unwrap();
        let m = inspect(&src);
        assert_eq!(m.format, Format::Other);
        assert!(m.flags().contains(&Flag::Hidden));
        let job = CleanJob {
            opts: CleanOptions::everything(),
            output: Output::Suffix("_clean".into()),
            keep_file_dates: true,
        };
        let r = clean_file(&src, &job);
        assert!(r.ok(), "{r:?}");
        assert!(r.verified);
        assert_eq!(std::fs::read(&r.dst).unwrap(), b"plain contents");
        assert!(winmeta::streams(&r.dst).is_empty());
        // The original is untouched, stream included.
        assert_eq!(winmeta::streams(&src).len(), 1);
        let t0 = winmeta::times(&src).unwrap().modified.unwrap();
        let t1 = winmeta::times(&r.dst).unwrap().modified.unwrap();
        assert_eq!(util::unix_of(t0), util::unix_of(t1));
        // Replace keeps the name and drops the stream.
        let r = clean_file(
            &src,
            &CleanJob {
                output: Output::Replace,
                ..job
            },
        );
        assert!(r.ok());
        assert_eq!(r.dst, src);
        assert!(winmeta::streams(&src).is_empty());
        assert!(std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".metaforge-")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake(path: &str, taken: Option<&str>, model: &str) -> Meta {
        Meta {
            path: PathBuf::from(path),
            format: Format::Jpeg,
            size: 1,
            fields: vec![Field::new(
                "exif:0x0110",
                "Camera model",
                model,
                Category::Device,
                "EXIF",
            )],
            notes: vec![],
            gps: Some(GeoPoint { lat: 1.0, lon: 2.0 }),
            taken: taken.map(String::from),
            editable: vec![],
            error: None,
        }
    }

    #[test]
    fn rename_plans_from_dates() {
        let metas = vec![
            fake(
                "C:\\nowhere-mf\\a.jpg",
                Some("2024-05-01 10:20:30"),
                "EC-100",
            ),
            fake(
                "C:\\nowhere-mf\\b.JPG",
                Some("2024-05-01 10:20:30"),
                "EC-100",
            ),
            fake("C:\\nowhere-mf\\c.jpg", None, ""),
        ];
        let plan = rename_plan(&metas, "{date}_{time} {camera}");
        assert_eq!(
            plan[0].to.file_name().unwrap(),
            "2024-05-01_10-20-30 EC-100.jpg"
        );
        assert_eq!(
            plan[1].to.file_name().unwrap(),
            "2024-05-01_10-20-30 EC-100 (2).JPG"
        );
        assert!(plan[2].problem.is_some());
        let plan = rename_plan(&metas, "trip {n}");
        assert_eq!(plan[2].to.file_name().unwrap(), "trip 003.jpg");
    }

    #[test]
    fn exports_and_compare() {
        let a = fake(
            "C:\\x\\a.jpg",
            Some("2024-05-01 10:20:30"),
            "EC-100, \"pro\"",
        );
        let b = fake("C:\\x\\b.jpg", None, "EC-200");
        let csv = to_csv(std::slice::from_ref(&a));
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.contains("\"EC-100, \"\"pro\"\"\""));
        let json = to_json(&[a.clone(), b.clone()]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["flags"][0], "location");
        assert_eq!(v[0]["fields"][0]["label"], "Camera model");
        let rows = compare(&a, &b);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].same());
        assert_eq!(rows[0].b.as_deref(), Some("EC-200"));
    }
}
