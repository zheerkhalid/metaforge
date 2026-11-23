//! Windows-level traces: NTFS alternate data streams (such as the
//! `Zone.Identifier` note that says where a file was downloaded from) and
//! the file's own timestamps.

use crate::meta::{Category, Field, Flag};
use crate::util;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// An alternate data stream: its name (without the `:$DATA` suffix) and size.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub name: String,
    pub size: u64,
}

fn stream_path(path: &Path, name: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(":");
    s.push(name);
    PathBuf::from(s)
}

/// The alternate data streams of a file (not the main contents).
#[cfg(windows)]
pub fn streams(path: &Path) -> Vec<Stream> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut out = Vec::new();
    let mut data = WIN32_FIND_STREAM_DATA::default();
    // SAFETY: the name is NUL-terminated and outlives the calls; the find
    // handle is closed before returning.
    unsafe {
        let Ok(handle) = FindFirstStreamW(
            PCWSTR(wide.as_ptr()),
            FindStreamInfoStandard,
            &mut data as *mut _ as *mut core::ffi::c_void,
            0,
        ) else {
            return out;
        };
        loop {
            let end = data
                .cStreamName
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(data.cStreamName.len());
            let raw = String::from_utf16_lossy(&data.cStreamName[..end]);
            // ":Zone.Identifier:$DATA" -> "Zone.Identifier"; "::$DATA" is the file itself.
            let name = raw
                .trim_start_matches(':')
                .trim_end_matches(":$DATA")
                .to_string();
            if !name.is_empty() && raw != "::$DATA" {
                out.push(Stream {
                    name,
                    size: data.StreamSize as u64,
                });
            }
            if FindNextStreamW(handle, &mut data as *mut _ as *mut core::ffi::c_void).is_err() {
                break;
            }
        }
        let _ = FindClose(handle);
    }
    out
}

#[cfg(not(windows))]
pub fn streams(_path: &Path) -> Vec<Stream> {
    Vec::new()
}

pub fn read_stream(path: &Path, name: &str) -> Option<Vec<u8>> {
    std::fs::read(stream_path(path, name)).ok()
}

pub fn write_stream(path: &Path, name: &str, data: &[u8]) -> Result<(), String> {
    std::fs::write(stream_path(path, name), data).map_err(|e| format!("{name}: {e}"))
}

pub fn delete_stream(path: &Path, name: &str) -> Result<(), String> {
    std::fs::remove_file(stream_path(path, name)).map_err(|e| format!("{name}: {e}"))
}

/// Removes every alternate data stream. Returns how many were removed.
pub fn delete_all_streams(path: &Path) -> Result<usize, String> {
    let list = streams(path);
    for s in &list {
        delete_stream(path, &s.name)?;
    }
    Ok(list.len())
}

/// Copies the alternate data streams of `from` onto `to`.
pub fn copy_streams(from: &Path, to: &Path) {
    for s in streams(from) {
        if let Some(data) = read_stream(from, &s.name) {
            let _ = write_stream(to, &s.name, &data);
        }
    }
}

/// The fields of a `Zone.Identifier` note.
pub fn zone_fields(text: &str) -> Vec<Field> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let (label, value, flag) = match key.trim() {
            "ZoneId" => (
                "Marked as",
                match value {
                    "0" => "Made on this PC".to_string(),
                    "1" => "From the local network".to_string(),
                    "2" => "From a trusted site".to_string(),
                    "3" => "Downloaded from the internet".to_string(),
                    "4" => "From a restricted site".to_string(),
                    other => format!("Zone {other}"),
                },
                None,
            ),
            "ReferrerUrl" => (
                "Downloaded from page",
                value.to_string(),
                Some(Flag::Hidden),
            ),
            "HostUrl" => (
                "Downloaded from address",
                value.to_string(),
                Some(Flag::Hidden),
            ),
            "HostIpAddress" => ("Server address", value.to_string(), Some(Flag::Hidden)),
            "LastWriterPackageFamilyName" => ("Saved by app", value.to_string(), None),
            "AppZoneId" => continue,
            other => (other, value.to_string(), None),
        };
        let mut f = Field::new(
            format!("ads:Zone.Identifier:{}", key.trim()),
            label,
            value,
            Category::Windows,
            "Zone.Identifier stream",
        );
        f.flag = flag;
        out.push(f);
    }
    out
}

/// Created, modified and last opened.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

pub fn times(path: &Path) -> Option<Times> {
    let m = std::fs::metadata(path).ok()?;
    Some(Times {
        created: m.created().ok(),
        modified: m.modified().ok(),
        accessed: m.accessed().ok(),
    })
}

/// Sets the given timestamps (others are left as they are).
pub fn set_times(path: &Path, t: Times) -> Result<(), String> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut ft = std::fs::FileTimes::new();
    if let Some(m) = t.modified {
        ft = ft.set_modified(m);
    }
    if let Some(a) = t.accessed {
        ft = ft.set_accessed(a);
    }
    #[cfg(windows)]
    if let Some(c) = t.created {
        use std::os::windows::fs::FileTimesExt;
        ft = ft.set_created(c);
    }
    file.set_times(ft).map_err(|e| e.to_string())
}

/// The Windows-level fields of a file: streams, then timestamps.
pub fn fields(path: &Path) -> Vec<Field> {
    let mut out = Vec::new();
    for s in streams(path) {
        if s.name == "Zone.Identifier" {
            let text = read_stream(path, &s.name)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let zone = zone_fields(&text);
            if zone.is_empty() {
                out.push(
                    Field::new(
                        "ads:Zone.Identifier",
                        "Download note",
                        util::human_size(s.size),
                        Category::Windows,
                        "Alternate data stream",
                    )
                    .flag(Flag::Hidden),
                );
            }
            out.extend(zone);
        } else {
            out.push(
                Field::new(
                    format!("ads:{}", s.name),
                    format!("Hidden stream \"{}\"", s.name),
                    util::human_size(s.size),
                    Category::Windows,
                    "Alternate data stream",
                )
                .flag(Flag::Hidden),
            );
        }
    }
    if let Some(t) = times(path) {
        for (id, label, value) in [
            ("file:created", "File created", t.created),
            ("file:modified", "File modified", t.modified),
            ("file:accessed", "File last opened", t.accessed),
        ] {
            if let Some(v) = value {
                out.push(
                    Field::new(
                        id,
                        label,
                        util::local_text(v),
                        Category::Windows,
                        "Windows file system",
                    )
                    .fixed(),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("metaforge-test-{}-{name}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn zone_identifier_is_read_and_removed() {
        let dir = temp("ads");
        let file = dir.join("download.txt");
        std::fs::write(&file, b"hello").unwrap();
        let note = "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/page\r\nHostUrl=https://example.com/file.txt\r\n";
        write_stream(&file, "Zone.Identifier", note.as_bytes()).unwrap();
        write_stream(&file, "extra", b"secret").unwrap();
        let list = streams(&file);
        assert_eq!(list.len(), 2, "{list:?}");
        let f = fields(&file);
        assert!(f
            .iter()
            .any(|x| x.label == "Downloaded from address"
                && x.value == "https://example.com/file.txt"));
        assert!(f.iter().any(|x| x.value == "Downloaded from the internet"));
        assert!(f.iter().any(|x| x.id == "ads:extra"));
        assert!(f.iter().any(|x| x.id == "file:modified" && !x.removable));

        let copy = dir.join("copy.txt");
        std::fs::write(&copy, b"hello").unwrap();
        copy_streams(&file, &copy);
        assert_eq!(streams(&copy).len(), 2);

        assert_eq!(delete_all_streams(&file).unwrap(), 2);
        assert!(streams(&file).is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_and_read_times() {
        let dir = temp("times");
        let file = dir.join("t.txt");
        std::fs::write(&file, b"x").unwrap();
        let when = util::system_time(1_600_000_000);
        set_times(
            &file,
            Times {
                created: Some(when),
                modified: Some(when),
                accessed: None,
            },
        )
        .unwrap();
        let t = times(&file).unwrap();
        assert_eq!(util::unix_of(t.modified.unwrap()), 1_600_000_000);
        assert_eq!(util::unix_of(t.created.unwrap()), 1_600_000_000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
