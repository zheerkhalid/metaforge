//! The command line: `--show` and `--clean`.

use crate::engine::{self, CleanJob, Output};
use crate::meta::{CatSet, Category, CleanOptions, Preset};
use std::path::PathBuf;

pub const USAGE: &str = "MetaForge: see and remove hidden information in files before sharing them

  MetaForge.exe [files or folders]   open the window with these files
  MetaForge.exe --show <file> [--json]
                                     print what is hidden in a file
  MetaForge.exe --clean <files or folders> [options]
        --preset all|location-names|custom   what to remove (default: all)
        --remove location,people,device,dates,software,description,hidden,windows
                                     the categories for --preset custom
        --out <folder>               save cleaned copies in this folder
        --suffix <text>              save copies next to the originals (default: _clean)
        --replace                    overwrite the originals (after verifying the result)
        --drop-orientation           also remove the photo orientation
        --drop-color-profile         also remove colour profiles
        --new-file-dates             do not copy the original file dates
        --json                       print the report as JSON
  MetaForge.exe --export <files or folders> --out <file.csv|file.json>
                                     save all metadata as CSV or JSON
  MetaForge.exe --register-menu      add the Explorer right-click entries (this user only)
  MetaForge.exe --unregister-menu    remove them
  MetaForge.exe --demo               show invented files; nothing is read or changed
  MetaForge.exe --screenshot <png> [--view <name>] [--light]
                                     render a view of the demo to a PNG and exit
  MetaForge.exe --selftest           create sample files in a temporary folder and
                                     check reading, cleaning and editing of every format

Exit codes: 0 done, 1 some files failed or were not fully cleaned, 2 usage error.
MetaForge makes no network requests.";

/// Arguments after `flag` up to the next `--option`.
fn values_after(args: &[String], flag: &str) -> Vec<String> {
    let Some(i) = args.iter().position(|a| a == flag) else {
        return Vec::new();
    };
    args[i + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .cloned()
        .collect()
}

fn value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// `--show`: prints the fields of one or more files.
pub fn show(args: &[String]) -> i32 {
    let paths: Vec<PathBuf> = values_after(args, "--show")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    if paths.is_empty() {
        eprintln!("--show needs a file.");
        return 2;
    }
    let metas: Vec<_> = engine::expand(&paths)
        .iter()
        .map(|p| engine::inspect(p))
        .collect();
    if args.iter().any(|a| a == "--json") {
        println!("{}", engine::to_json(&metas));
        return 0;
    }
    for m in &metas {
        println!("{}", m.path.display());
        println!("  {}", m.format.name());
        let flags = m.flags();
        if flags.is_empty() {
            println!("  Nothing personal found.");
        } else {
            let list: Vec<&str> = flags.iter().map(|f| f.long_label()).collect();
            println!("  {}", list.join(", "));
        }
        if let Some(e) = &m.error {
            println!("  Problem: {e}");
        }
        for cat in Category::ALL {
            let fields: Vec<_> = m.by_category(cat).collect();
            if fields.is_empty() {
                continue;
            }
            println!("  {}", cat.title());
            for f in fields {
                println!("    {}: {}  [{}]", f.label, f.value, f.source);
            }
        }
        for n in &m.notes {
            println!("  Note: {n}");
        }
        println!();
    }
    0
}

/// Parses the clean options from the command line.
pub fn parse_job(args: &[String]) -> Result<CleanJob, String> {
    let preset = match value(args, "--preset") {
        Some(p) => Preset::from_key(&p).ok_or_else(|| format!("Unknown preset \"{p}\"."))?,
        None if args.iter().any(|a| a == "--remove") => Preset::Custom,
        None => Preset::Everything,
    };
    let mut custom = CatSet::EMPTY;
    if let Some(list) = value(args, "--remove") {
        for key in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let cat =
                Category::from_key(key).ok_or_else(|| format!("Unknown category \"{key}\"."))?;
            custom.insert(cat);
        }
    }
    if preset == Preset::Custom && custom.is_empty() {
        return Err("--preset custom needs --remove with at least one category.".into());
    }
    let output = if args.iter().any(|a| a == "--replace") {
        Output::Replace
    } else if let Some(dir) = value(args, "--out") {
        Output::Folder(PathBuf::from(dir))
    } else {
        Output::Suffix(value(args, "--suffix").unwrap_or_else(|| "_clean".into()))
    };
    Ok(CleanJob {
        opts: CleanOptions {
            remove: preset.categories(custom),
            keep_orientation: !args.iter().any(|a| a == "--drop-orientation"),
            keep_color_profile: !args.iter().any(|a| a == "--drop-color-profile"),
        },
        output,
        keep_file_dates: !args.iter().any(|a| a == "--new-file-dates"),
    })
}

/// `--clean`: cleans files and prints a report.
pub fn clean(args: &[String]) -> i32 {
    let job = match parse_job(args) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let paths: Vec<PathBuf> = values_after(args, "--clean")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let files = engine::expand(&paths);
    if files.is_empty() {
        eprintln!("No files to clean.");
        return 2;
    }
    let json = args.iter().any(|a| a == "--json");
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let results = engine::clean_batch(&files, &job, &cancel, |done, total, r| {
        if !json {
            let state = match (&r.error, r.left.is_empty(), r.already_clean) {
                (Some(e), _, _) => format!("FAILED: {e}"),
                (None, false, _) => format!("NOT FULLY CLEANED: {} left", r.left.len()),
                (None, true, true) => "nothing to remove".into(),
                (None, true, false) => format!("removed {} fields, verified", r.removed.len()),
            };
            println!("[{done}/{total}] {}: {state}", r.src.display());
        }
    });
    let summary = engine::summarize(&results);
    if json {
        #[derive(serde::Serialize)]
        struct Out<'a> {
            summary: &'a engine::Summary,
            files: &'a [engine::CleanResult],
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Out {
                summary: &summary,
                files: &results
            })
            .unwrap_or_default()
        );
    } else {
        println!(
            "{} files: {} cleaned, {} had nothing to remove, {} failed, {} not fully cleaned.",
            summary.files,
            summary.cleaned,
            summary.already_clean,
            summary.failed,
            summary.not_verified
        );
    }
    if summary.failed + summary.not_verified > 0 {
        1
    } else {
        0
    }
}

/// `--export`: all metadata of files to CSV or JSON.
pub fn export(args: &[String]) -> i32 {
    let paths: Vec<PathBuf> = values_after(args, "--export")
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let Some(out) = value(args, "--out").map(PathBuf::from) else {
        eprintln!("--export needs --out <file.csv|file.json>.");
        return 2;
    };
    let metas: Vec<_> = engine::expand(&paths)
        .iter()
        .map(|p| engine::inspect(p))
        .collect();
    let text = if out
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        engine::to_json(&metas)
    } else {
        engine::to_csv(&metas)
    };
    match std::fs::write(&out, text) {
        Ok(()) => {
            println!("{} files written to {}", metas.len(), out.display());
            0
        }
        Err(e) => {
            eprintln!("{}: {e}", out.display());
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn job_parsing() {
        let j = parse_job(&args("--clean a.jpg")).unwrap();
        assert!(j.opts.is_everything());
        assert_eq!(j.output, Output::Suffix("_clean".into()));
        let j = parse_job(&args("--clean a.jpg --preset location-names --out D:\\x")).unwrap();
        assert!(j.opts.removes(Category::Location) && !j.opts.removes(Category::Dates));
        assert_eq!(j.output, Output::Folder(PathBuf::from("D:\\x")));
        let j = parse_job(&args("--clean a --remove dates,device --replace")).unwrap();
        assert!(j.opts.removes(Category::Dates) && j.opts.removes(Category::Device));
        assert!(!j.opts.removes(Category::People));
        assert_eq!(j.output, Output::Replace);
        assert!(parse_job(&args("--clean a --preset custom")).is_err());
        assert!(parse_job(&args("--clean a --remove nonsense")).is_err());
        assert_eq!(
            values_after(&args("--clean a b --out c"), "--clean"),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
