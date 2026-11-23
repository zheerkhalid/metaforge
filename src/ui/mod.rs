//! The egui user interface: the Files view (list, details, actions), the
//! clean / edit / dates dialogs, rename, compare, settings and about.

mod app;
mod bidi;
mod clean;
mod dialogs;
mod files;
mod pages;
mod theme;
mod widgets;

use eframe::egui;
use std::path::PathBuf;

/// How the window was started.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Show invented files; nothing on this PC is read or changed.
    pub demo: bool,
    /// Render one frame of the chosen view to this PNG (1280 x 800), then exit.
    pub screenshot: Option<PathBuf>,
    /// Which view to open first (for screenshots): `files`, `document`,
    /// `media`, `kurdish`, `empty`, `clean`, `report`, `edit`, `dates`,
    /// `rename`, `compare`, `settings`, `about`.
    pub view: Option<String>,
    /// Force the light theme (for screenshots).
    pub light: bool,
    /// Files and folders to open at start.
    pub files: Vec<PathBuf>,
    /// Open the clean dialog for `files` right away (Explorer "Clean metadata").
    pub clean_now: bool,
}

/// Starts the window.
pub fn run(options: Options, icon: egui::IconData) -> eframe::Result<()> {
    app::run(options, icon)
}

/// Whether text has right-to-left letters (drawn by Windows, not egui).
pub fn has_rtl(text: &str) -> bool {
    bidi::needs_native(text)
}
