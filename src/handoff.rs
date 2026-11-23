//! Passing paths to the window that is already open.
//!
//! Explorer starts one copy of MetaForge per selected item. The first copy
//! opens the window; the others drop their path into the inbox folder and
//! exit, and the window picks the paths up. So a selection of forty photos
//! gives one window with forty files, not forty windows.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Notes older than this are from a copy that died; they are discarded.
const STALE: Duration = Duration::from_secs(120);

/// What the window should do with handed-over paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Show,
    Clean,
}

pub fn inbox_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("inbox")
}

/// Leaves paths for the window that is already open.
pub fn send(app_dir: &Path, intent: Intent, paths: &[PathBuf]) -> Result<(), String> {
    let dir = inbox_dir(app_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("{now:x}-{}", std::process::id());
    let mut body = match intent {
        Intent::Show => "show".to_string(),
        Intent::Clean => "clean".to_string(),
    };
    for p in paths {
        body.push('\n');
        body.push_str(&p.to_string_lossy());
    }
    // Write under a temporary name first so the window never reads half a note.
    let tmp = dir.join(format!("{name}.part"));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(format!("{name}.txt"))).map_err(|e| e.to_string())
}

/// Collects (and removes) the notes left by other copies.
pub fn drain(app_dir: &Path) -> Vec<(Intent, PathBuf)> {
    let dir = inbox_dir(app_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut notes: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    notes.sort();
    let now = SystemTime::now();
    let mut out = Vec::new();
    for (modified, path) in notes {
        let age = now.duration_since(modified).unwrap_or_default();
        let is_note = path.extension().map(|x| x == "txt").unwrap_or(false);
        if is_note && age <= STALE {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let mut lines = text.lines();
                let intent = match lines.next() {
                    Some("clean") => Intent::Clean,
                    _ => Intent::Show,
                };
                out.extend(
                    lines
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| (intent, PathBuf::from(l))),
                );
            }
            let _ = std::fs::remove_file(&path);
        } else if age > STALE {
            let _ = std::fs::remove_file(&path);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_arrive_once() {
        let dir = std::env::temp_dir().join(format!("metaforge-handoff-{}", std::process::id()));
        send(&dir, Intent::Clean, &[PathBuf::from("C:\\a\\وێنە.jpg")]).unwrap();
        send(
            &dir,
            Intent::Show,
            &[PathBuf::from("C:\\b.pdf"), PathBuf::from("C:\\c.mp3")],
        )
        .unwrap();
        let got = drain(&dir);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (Intent::Clean, PathBuf::from("C:\\a\\وێنە.jpg")));
        assert_eq!(got[2], (Intent::Show, PathBuf::from("C:\\c.mp3")));
        assert!(drain(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
