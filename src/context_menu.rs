//! The optional Explorer right-click entries "Show metadata with MetaForge"
//! and "Clean metadata with MetaForge".
//!
//! Registered per user under `HKCU\Software\Classes`, so no administrator
//! rights are needed and other accounts are not affected. The entries only
//! ever start MetaForge with the clicked paths; cleaning still waits for the
//! user to confirm in the window. On Windows 11 classic entries like these
//! appear under "Show more options" (or directly with Shift + right-click).

use std::path::Path;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
use winreg::RegKey;

/// The real location. Tests pass a throwaway key instead.
pub const CLASSES: &str = "Software\\Classes";
pub const SHOW_VERB: &str = "MetaForge.Show";
pub const CLEAN_VERB: &str = "MetaForge.Clean";

/// `(class, verb, label, arguments)`
fn entries() -> [(&'static str, &'static str, &'static str, &'static str); 3] {
    [
        (
            "*",
            SHOW_VERB,
            "Show metadata with MetaForge",
            "--menu-show \"%1\"",
        ),
        (
            "*",
            CLEAN_VERB,
            "Clean metadata with MetaForge",
            "--menu-clean \"%1\"",
        ),
        (
            "Directory",
            CLEAN_VERB,
            "Clean metadata with MetaForge",
            "--menu-clean \"%1\"",
        ),
    ]
}

/// The command line Explorer runs for one entry.
pub fn command_line(exe: &Path, arguments: &str) -> String {
    format!("\"{}\" {arguments}", exe.display())
}

fn werr(e: std::io::Error) -> String {
    format!("Could not write the Explorer menu entry: {e}")
}

/// Adds (or refreshes) the entries.
pub fn register(classes: &str, exe: &Path) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (class, verb, label, arguments) in entries() {
        let path = format!("{classes}\\{class}\\shell\\{verb}");
        let (key, _) = hkcu.create_subkey(&path).map_err(werr)?;
        key.set_value("", &label.to_string()).map_err(werr)?;
        key.set_value("Icon", &format!("\"{}\",0", exe.display()))
            .map_err(werr)?;
        // One copy per selected item, without the 15-item limit; the copies
        // hand their paths to the first window.
        key.set_value("MultiSelectModel", &"Player".to_string())
            .map_err(werr)?;
        let (cmd, _) = hkcu
            .create_subkey(format!("{path}\\command"))
            .map_err(werr)?;
        cmd.set_value("", &command_line(exe, arguments))
            .map_err(werr)?;
    }
    Ok(())
}

fn is_empty_key(hkcu: &RegKey, path: &str) -> bool {
    hkcu.open_subkey_with_flags(path, KEY_READ)
        .map(|k| k.enum_keys().next().is_none() && k.enum_values().next().is_none())
        .unwrap_or(false)
}

/// Removes the entries, and the keys that were only created to hold them.
pub fn unregister(classes: &str) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for (class, verb, _, _) in entries() {
        let shell = format!("{classes}\\{class}\\shell");
        if let Ok(key) = hkcu.open_subkey_with_flags(&shell, KEY_READ | KEY_WRITE) {
            match key.delete_subkey_all(verb) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("Could not remove the Explorer menu entry: {e}")),
            }
        }
        // Tidy up empty parents, never anything that holds other data.
        for parent in [shell, format!("{classes}\\{class}")] {
            if is_empty_key(&hkcu, &parent) {
                let _ = hkcu.delete_subkey(&parent);
            } else {
                break;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuStatus {
    Off,
    On,
    /// Registered, but for a MetaForge.exe somewhere else (a moved portable copy).
    OtherCopy,
}

/// Is the entry present, and does it point at this program?
pub fn status(classes: &str, exe: &Path) -> MenuStatus {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(cmd) = hkcu.open_subkey(format!("{classes}\\*\\shell\\{CLEAN_VERB}\\command")) else {
        return MenuStatus::Off;
    };
    let line: String = cmd.get_value("").unwrap_or_default();
    if line.to_lowercase() == command_line(exe, "--menu-clean \"%1\"").to_lowercase() {
        MenuStatus::On
    } else {
        MenuStatus::OtherCopy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs against a throwaway key, which is removed again at the end.
    #[test]
    fn register_and_unregister_leave_nothing_behind() {
        let base = format!("Software\\MetaForgeTest-{}", std::process::id());
        let classes = format!("{base}\\Classes");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let exe = Path::new("C:\\Apps\\Meta Forge\\MetaForge.exe");
        assert_eq!(status(&classes, exe), MenuStatus::Off);

        // Something that is not ours lives next to the entries and must survive.
        let (other, _) = hkcu
            .create_subkey(format!("{classes}\\*\\shell\\OtherTool"))
            .unwrap();
        other.set_value("", &"Other tool").unwrap();

        register(&classes, exe).unwrap();
        register(&classes, exe).unwrap();
        assert_eq!(status(&classes, exe), MenuStatus::On);
        assert_eq!(
            status(&classes, Path::new("D:\\Elsewhere\\MetaForge.exe")),
            MenuStatus::OtherCopy
        );
        let cmd: String = hkcu
            .open_subkey(format!("{classes}\\*\\shell\\{SHOW_VERB}\\command"))
            .unwrap()
            .get_value("")
            .unwrap();
        assert_eq!(
            cmd,
            "\"C:\\Apps\\Meta Forge\\MetaForge.exe\" --menu-show \"%1\""
        );
        let label: String = hkcu
            .open_subkey(format!("{classes}\\Directory\\shell\\{CLEAN_VERB}"))
            .unwrap()
            .get_value("")
            .unwrap();
        assert_eq!(label, "Clean metadata with MetaForge");

        unregister(&classes).unwrap();
        unregister(&classes).unwrap();
        assert_eq!(status(&classes, exe), MenuStatus::Off);
        assert!(
            hkcu.open_subkey(format!("{classes}\\Directory")).is_err(),
            "empty parents are tidied"
        );
        assert!(
            hkcu.open_subkey(format!("{classes}\\*\\shell\\OtherTool"))
                .is_ok(),
            "other tools' entries are untouched"
        );

        hkcu.delete_subkey_all(&base).unwrap();
        assert!(hkcu.open_subkey(&base).is_err());
    }
}
