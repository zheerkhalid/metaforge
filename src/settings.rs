//! Settings, saved as JSON in `%LOCALAPPDATA%\MetaForge\settings.json`.

use crate::engine::{CleanJob, Output};
use crate::meta::{CatSet, Category, CleanOptions, Preset};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

/// Where cleaned files go, as chosen in the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum OutputMode {
    /// Overwrite the original.
    Replace,
    /// A copy next to it with a suffix.
    #[default]
    Suffix,
    /// A copy in a chosen folder.
    Folder,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub preset: Preset,
    /// The categories of the Custom preset.
    pub custom: CatSet,
    pub keep_orientation: bool,
    pub keep_color_profile: bool,
    pub output_mode: OutputMode,
    pub suffix: String,
    pub folder: Option<PathBuf>,
    /// Cleaned and edited files keep the original's created/modified dates.
    pub keep_file_dates: bool,
    /// The last rename pattern.
    pub rename_pattern: String,
    /// Show the technical name and storage of each field in the details.
    pub show_technical: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            theme: ThemeChoice::System,
            preset: Preset::Everything,
            custom: CatSet::of(&[Category::Location, Category::People, Category::Device]),
            keep_orientation: true,
            keep_color_profile: true,
            output_mode: OutputMode::Suffix,
            suffix: "_clean".into(),
            folder: None,
            keep_file_dates: true,
            rename_pattern: "{date}_{time}".into(),
            show_technical: false,
        }
    }
}

impl Settings {
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = dir.join("settings.json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("settings.json")).map_err(|e| e.to_string())
    }

    pub fn clean_options(&self) -> CleanOptions {
        CleanOptions {
            remove: self.preset.categories(self.custom),
            keep_orientation: self.keep_orientation,
            keep_color_profile: self.keep_color_profile,
        }
    }

    pub fn output(&self) -> Output {
        match self.output_mode {
            OutputMode::Replace => Output::Replace,
            OutputMode::Suffix => Output::Suffix(if self.suffix.trim().is_empty() {
                "_clean".into()
            } else {
                self.suffix.trim().to_string()
            }),
            OutputMode::Folder => match &self.folder {
                Some(f) => Output::Folder(f.clone()),
                None => Output::Suffix("_clean".into()),
            },
        }
    }

    pub fn clean_job(&self) -> CleanJob {
        CleanJob {
            opts: self.clean_options(),
            output: self.output(),
            keep_file_dates: self.keep_file_dates,
        }
    }
}

/// `%LOCALAPPDATA%\MetaForge` (or `METAFORGE_DATA`).
pub fn app_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("METAFORGE_DATA").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MetaForge")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("metaforge-settings-{}", std::process::id()));
        let mut s = Settings::default();
        assert_eq!(s.output(), Output::Suffix("_clean".into()));
        s.preset = Preset::Custom;
        s.custom = CatSet::of(&[Category::Dates]);
        s.output_mode = OutputMode::Folder;
        s.folder = Some(PathBuf::from("D:\\Share"));
        s.save(&dir).unwrap();
        let back = Settings::load(&dir);
        assert_eq!(back, s);
        assert!(back.clean_options().remove.contains(Category::Dates));
        assert!(!back.clean_options().remove.contains(Category::Location));
        assert_eq!(back.output(), Output::Folder(PathBuf::from("D:\\Share")));
        // Unknown or missing keys fall back to defaults.
        std::fs::write(dir.join("settings.json"), "{\"theme\":\"Dark\",\"zzz\":1}").unwrap();
        let partial = Settings::load(&dir);
        assert_eq!(partial.theme, ThemeChoice::Dark);
        assert_eq!(partial.suffix, "_clean");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
