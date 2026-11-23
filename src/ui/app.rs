//! Application state, background work and the window frame (header,
//! navigation rail, about dialog, toasts, drag and drop, screenshots).

use super::clean::{CleanDlg, Phase};
use super::dialogs::{DatesDlg, EditDlg};
use super::files::{self, ListFilter};
use super::theme::{self, pal, pal_ctx, regular, semibold, Icon};
use super::{widgets, Options};
use crate::context_menu::{self, MenuStatus};
use crate::engine::{self, CleanResult};
use crate::handoff::{self, Intent};
use crate::meta::{Kind, Meta};
use crate::settings::{self, Settings, ThemeChoice};
use crate::{demo, sys};
use eframe::egui::{
    self, vec2, Align, Align2, Color32, CornerRadius, Frame, Key, Layout, Margin, Rect, RichText,
    Sense, Stroke, TextureHandle, Ui,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Files,
    Compare,
    Rename,
    Settings,
}

/// One file in the list.
pub struct Item {
    pub meta: Meta,
    /// Ticked for the actions.
    pub selected: bool,
}

/// Results of background work, delivered to the window thread.
pub enum Msg {
    /// One file was read. Replaces an entry with the same path.
    Inspected(Meta),
    /// A batch of added paths is done (`clean`: open the clean dialog).
    BatchDone {
        paths: Vec<PathBuf>,
        clean: bool,
    },
    /// A file changed on disk (`old` may differ from the new path).
    Replaced {
        old: PathBuf,
        meta: Meta,
    },
    CleanProgress(CleanResult),
    /// The batch is over; `after` holds the demo's cleaned files.
    CleanDone {
        after: Vec<Meta>,
    },
    Thumb(PathBuf, Option<egui::ColorImage>),
    Toast(String, bool),
}

struct Toast {
    text: String,
    error: bool,
    born: Instant,
}

struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

pub struct App {
    pub(super) demo: bool,
    pub(super) settings: Settings,
    saved_settings: Settings,
    pub(super) tab: Tab,
    pub(super) items: Vec<Item>,
    /// The file shown in the details panel.
    pub(super) current: Option<usize>,
    /// For shift-click ranges.
    pub(super) anchor: Option<usize>,
    pub(super) filter: ListFilter,
    pub(super) search: String,
    pub(super) focus_search: bool,
    pub(super) scroll_to_current: bool,
    /// Files being read in the background.
    pub(super) reading: usize,
    pub(super) clean: Option<Box<CleanDlg>>,
    pub(super) edit: Option<Box<EditDlg>>,
    pub(super) dates: Option<Box<DatesDlg>>,
    pub(super) about_open: bool,
    pub(super) compare: Option<(Meta, Meta)>,
    pub(super) compare_only_diff: bool,
    pub(super) rename_cache: Option<(String, Vec<PathBuf>, Vec<engine::Rename>)>,
    pub(super) menu_status: Option<MenuStatus>,
    pub(super) thumbs: HashMap<PathBuf, Option<TextureHandle>>,
    toasts: Vec<Toast>,
    pub(super) tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub(super) ctx: egui::Context,
    pub(super) logo: Option<TextureHandle>,
    shot: Option<Shot>,
    last_poll: Instant,
}

/// Starts the window.
pub fn run(options: Options, icon: egui::IconData) -> eframe::Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title(if options.demo {
            "MetaForge (demo)"
        } else {
            "MetaForge"
        })
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 600.0])
        .with_drag_and_drop(true)
        .with_icon(icon.clone());
    let native = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "MetaForge",
        native,
        Box::new(move |cc| Ok(Box::new(App::new(cc, options, icon)))),
    )
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, options: Options, icon: egui::IconData) -> Self {
        let ctx = &cc.egui_ctx;
        theme::install(ctx);
        let demo = options.demo;
        let live = !demo && options.screenshot.is_none();
        let mut settings = if live {
            Settings::load(&settings::app_dir())
        } else {
            Settings::default()
        };
        if options.light {
            settings.theme = ThemeChoice::Light;
        } else if options.screenshot.is_some() {
            settings.theme = ThemeChoice::Dark;
        }
        theme::apply_choice(ctx, settings.theme);

        let logo = (icon.width > 0).then(|| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
        });
        let menu_status = if demo {
            Some(MenuStatus::Off)
        } else {
            std::env::current_exe()
                .ok()
                .map(|exe| context_menu::status(context_menu::CLASSES, &exe))
        };
        let (tx, rx) = channel();
        let mut app = App {
            demo,
            saved_settings: settings.clone(),
            settings,
            tab: Tab::Files,
            items: Vec::new(),
            current: None,
            anchor: None,
            filter: ListFilter::All,
            search: String::new(),
            focus_search: false,
            scroll_to_current: false,
            reading: 0,
            clean: None,
            edit: None,
            dates: None,
            about_open: false,
            compare: None,
            compare_only_diff: false,
            rename_cache: None,
            menu_status,
            thumbs: HashMap::new(),
            toasts: Vec::new(),
            tx,
            rx,
            ctx: ctx.clone(),
            logo,
            shot: options.screenshot.clone().map(|path| Shot {
                path,
                frames: 0,
                requested: false,
            }),
            last_poll: Instant::now(),
        };
        if demo {
            let view = options.view.as_deref().unwrap_or("files");
            if view != "empty" {
                app.items = demo::files()
                    .into_iter()
                    .map(|meta| Item {
                        meta,
                        selected: false,
                    })
                    .collect();
            }
            app.apply_view(view);
        } else if !options.files.is_empty() {
            app.add_paths(options.files.clone(), options.clean_now);
        }
        app
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.items.iter().position(|i| i.meta.name() == name)
    }

    /// Opens the view named on the command line (demo and screenshots).
    fn apply_view(&mut self, view: &str) {
        let pick = |app: &mut App, name: &str| {
            if let Some(i) = app.find(name) {
                app.current = Some(i);
                app.anchor = Some(i);
            }
        };
        let tick = |app: &mut App, names: &[&str]| {
            for n in names {
                if let Some(i) = app.find(n) {
                    app.items[i].selected = true;
                }
            }
        };
        let photos = [
            "IMG_2041.jpg",
            "IMG_2057.jpg",
            "PXL_20250803_114502.heic",
            "وێنەی گەشت.jpg",
            "VID_20250716_201533.mp4",
        ];
        match view {
            "empty" => {}
            "document" => pick(self, "Quarterly plan.docx"),
            "media" => pick(self, "VID_20250716_201533.mp4"),
            "kurdish" => pick(self, "وێنەی گەشت.jpg"),
            "clean" | "report" => {
                pick(self, "IMG_2041.jpg");
                tick(
                    self,
                    &[
                        "IMG_2041.jpg",
                        "IMG_2057.jpg",
                        "PXL_20250803_114502.heic",
                        "Quarterly plan.docx",
                        "ڕاپۆرتی ساڵانە.docx",
                        "Screenshot 2025-09-02.png",
                    ],
                );
                self.open_clean();
                if view == "report" {
                    if let Some(d) = &mut self.clean {
                        let job = d.job();
                        let results: Vec<CleanResult> = d
                            .paths
                            .iter()
                            .filter_map(|p| self.items.iter().find(|i| &i.meta.path == p))
                            .map(|i| demo::clean(&i.meta, &job).0)
                            .collect();
                        d.phase = Phase::Report {
                            results,
                            cancelled: false,
                            copies_added: false,
                            after: Vec::new(),
                        };
                    }
                }
            }
            "edit" => {
                pick(self, "IMG_2041.jpg");
                self.open_edit();
            }
            "dates" => {
                pick(self, "IMG_2041.jpg");
                self.open_dates();
            }
            "rename" => {
                pick(self, "IMG_2041.jpg");
                tick(self, &photos);
                self.settings.rename_pattern = "{date}_{time} {camera}".into();
                self.tab = Tab::Rename;
            }
            "compare" => {
                self.compare = Some(demo::compare_pair());
                pick(self, "IMG_2041.jpg");
                self.tab = Tab::Compare;
            }
            "settings" => {
                pick(self, "IMG_2041.jpg");
                self.tab = Tab::Settings;
            }
            "about" => {
                pick(self, "IMG_2041.jpg");
                self.about_open = true;
            }
            _ => pick(self, "IMG_2041.jpg"),
        }
        self.scroll_to_current = true;
    }

    // ----------------------------------------------------------------------------------
    // Messages and background work
    // ----------------------------------------------------------------------------------

    pub(super) fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast {
            text: text.into(),
            error,
            born: Instant::now(),
        });
    }

    pub(super) fn demo_toast(&mut self) {
        self.toast("Demo: nothing on this PC is changed.", false);
    }

    /// Runs `work` on a background thread; its messages wake the window.
    pub(super) fn spawn(&self, work: impl FnOnce(&dyn Fn(Msg)) + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            work(&send);
        });
    }

    /// Adds files and folders (folders are searched for known file types),
    /// reading them in the background.
    pub(super) fn add_paths(&mut self, paths: Vec<PathBuf>, clean: bool) {
        if paths.is_empty() {
            return;
        }
        if self.demo {
            self.toast("Demo: files from this PC are not read in the demo.", false);
            return;
        }
        self.tab = Tab::Files;
        self.reading += 1;
        self.spawn(move |send| {
            let files = engine::expand(&paths);
            for p in &files {
                send(Msg::Inspected(engine::inspect(p)));
            }
            send(Msg::BatchDone {
                paths: files,
                clean,
            });
        });
    }

    /// Reads files again after they were changed (`(old path, new path)`).
    pub(super) fn reinspect(&mut self, pairs: Vec<(PathBuf, PathBuf)>) {
        if pairs.is_empty() {
            return;
        }
        self.reading += 1;
        self.spawn(move |send| {
            for (old, new) in pairs {
                send(Msg::Replaced {
                    old,
                    meta: engine::inspect(&new),
                });
            }
            send(Msg::BatchDone {
                paths: Vec::new(),
                clean: false,
            });
        });
    }

    pub(super) fn index_of(&self, path: &Path) -> Option<usize> {
        self.items.iter().position(|i| i.meta.path == path)
    }

    /// Puts a file in the list, replacing the entry for `old` (or the same path).
    pub(super) fn upsert(&mut self, old: Option<&Path>, meta: Meta) {
        let at = old
            .and_then(|o| self.index_of(o))
            .or_else(|| self.index_of(&meta.path));
        self.thumbs.remove(&meta.path);
        if let Some(o) = old {
            self.thumbs.remove(o);
        }
        self.rename_cache = None;
        match at {
            Some(i) => self.items[i].meta = meta,
            None => {
                self.items.push(Item {
                    meta,
                    selected: false,
                });
                if self.current.is_none() {
                    self.current = Some(self.items.len() - 1);
                }
            }
        }
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Inspected(meta) => self.upsert(None, meta),
                Msg::Replaced { old, meta } => self.upsert(Some(&old), meta),
                Msg::BatchDone { paths, clean } => {
                    self.reading = self.reading.saturating_sub(1);
                    if clean && !paths.is_empty() {
                        for item in &mut self.items {
                            item.selected = paths.contains(&item.meta.path);
                        }
                        if let Some(i) = self.index_of(&paths[0]) {
                            self.current = Some(i);
                        }
                        self.open_clean();
                    }
                }
                Msg::CleanProgress(r) => {
                    if let Some(d) = &mut self.clean {
                        if let Phase::Running { results, .. } = &mut d.phase {
                            results.push(r);
                        }
                    }
                }
                Msg::CleanDone { after } => self.clean_finished(after),
                Msg::Thumb(path, image) => {
                    let tex = image.map(|img| {
                        self.ctx
                            .load_texture("thumb", img, egui::TextureOptions::LINEAR)
                    });
                    self.thumbs.insert(path, tex);
                }
                Msg::Toast(text, error) => self.toast(text, error),
            }
        }
    }

    /// Paths handed over by Explorer while the window is open.
    fn poll_handoff(&mut self) {
        if self.demo || self.shot.is_some() || self.last_poll.elapsed() < Duration::from_millis(500)
        {
            return;
        }
        self.last_poll = Instant::now();
        let notes = handoff::drain(&settings::app_dir());
        let mut show = Vec::new();
        let mut clean = Vec::new();
        for (intent, path) in notes {
            match intent {
                Intent::Show => show.push(path),
                Intent::Clean => clean.push(path),
            }
        }
        if !show.is_empty() || !clean.is_empty() {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.add_paths(show, false);
        self.add_paths(clean, true);
    }

    fn save_settings_if_changed(&mut self) {
        if self.settings == self.saved_settings {
            return;
        }
        self.saved_settings = self.settings.clone();
        if self.demo || self.shot.is_some() {
            return;
        }
        if let Err(e) = self.settings.save(&settings::app_dir()) {
            self.toast(format!("Settings could not be saved: {e}"), true);
        }
    }

    // ----------------------------------------------------------------------------------
    // Selection
    // ----------------------------------------------------------------------------------

    /// The files the actions work on: the ticked ones, or else the one shown.
    pub(super) fn selection(&self) -> Vec<usize> {
        let ticked: Vec<usize> = (0..self.items.len())
            .filter(|i| self.items[*i].selected)
            .collect();
        if !ticked.is_empty() {
            return ticked;
        }
        self.current
            .filter(|i| *i < self.items.len())
            .into_iter()
            .collect()
    }

    pub(super) fn selected_metas(&self) -> Vec<Meta> {
        self.selection()
            .into_iter()
            .map(|i| self.items[i].meta.clone())
            .collect()
    }

    pub(super) fn visible(&self) -> Vec<usize> {
        (0..self.items.len())
            .filter(|i| files::matches(&self.items[*i].meta, self.filter, &self.search))
            .collect()
    }

    pub(super) fn remove_selection(&mut self) {
        let gone: HashSet<usize> = self.selection().into_iter().collect();
        if gone.is_empty() {
            return;
        }
        let current_path = self
            .current
            .filter(|i| !gone.contains(i))
            .map(|i| self.items[i].meta.path.clone());
        let mut i = 0;
        self.items.retain(|_| {
            let keep = !gone.contains(&i);
            i += 1;
            keep
        });
        self.current = current_path
            .and_then(|p| self.index_of(&p))
            .or(if self.items.is_empty() { None } else { Some(0) });
        self.anchor = self.current;
        self.rename_cache = None;
        let n = gone.len();
        self.toast(
            if n == 1 {
                "Removed from the list (the file itself is untouched).".to_string()
            } else {
                format!("{n} files removed from the list (the files themselves are untouched).")
            },
            false,
        );
    }

    pub(super) fn pick_files(&mut self) {
        if self.demo {
            self.toast("Demo: files from this PC are not read in the demo.", false);
            return;
        }
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Choose files to inspect")
            .add_filter(
                "Photos, documents, music and videos",
                crate::detect::EXTENSIONS,
            )
            .add_filter("All files", &["*"])
            .pick_files()
        {
            self.add_paths(files, false);
        }
    }

    pub(super) fn pick_folder(&mut self) {
        if self.demo {
            self.toast("Demo: files from this PC are not read in the demo.", false);
            return;
        }
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Choose a folder to inspect")
            .pick_folder()
        {
            self.add_paths(vec![folder], false);
        }
    }

    pub(super) fn show_in_explorer(&mut self, path: &Path) {
        if self.demo {
            self.toast("Demo: the invented files are not on this PC.", false);
            return;
        }
        if let Err(e) = sys::show_in_explorer(path) {
            self.toast(e, true);
        }
    }

    /// Requests a preview of a real picture (decoded on a worker thread).
    pub(super) fn want_thumb(&mut self, meta: &Meta) {
        if self.demo || meta.kind() != Kind::Image || self.thumbs.contains_key(&meta.path) {
            return;
        }
        if meta.format == crate::detect::Format::Heif {
            self.thumbs.insert(meta.path.clone(), None);
            return;
        }
        self.thumbs.insert(meta.path.clone(), None);
        let path = meta.path.clone();
        self.spawn(move |send| {
            let image = image::open(&path).ok().map(|img| {
                let small = img.thumbnail(360, 360).to_rgba8();
                let (w, h) = small.dimensions();
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &small)
            });
            send(Msg::Thumb(path, image));
        });
    }

    pub(super) fn modal_open(&self) -> bool {
        self.clean.is_some() || self.edit.is_some() || self.dates.is_some() || self.about_open
    }

    // ----------------------------------------------------------------------------------
    // Screenshots
    // ----------------------------------------------------------------------------------

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        ctx.request_repaint();
        shot.frames += 1;
        if shot.frames == 1 {
            ctx.set_pixels_per_point(1.0);
        }
        if shot.frames == 3 {
            // Now that one point is one pixel, this gives an exact 1280 x 800 image.
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1280.0, 800.0)));
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            if let Some(parent) = shot.path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let result = image::save_buffer(
                &shot.path,
                image.as_raw(),
                w as u32,
                h as u32,
                image::ColorType::Rgba8,
            );
            if let Err(e) = result {
                eprintln!("screenshot failed: {e}");
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.shot = None;
            return;
        }
        if !shot.requested && shot.frames > 30 {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
    }

    // ----------------------------------------------------------------------------------
    // Frame: header and navigation
    // ----------------------------------------------------------------------------------

    fn header(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::TopBottomPanel::top("header")
            .exact_height(64.0)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(18, 0)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().hline(
                    full.x_range().expand(18.0),
                    full.bottom(),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.horizontal_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                    if let Some(logo) = &self.logo {
                        ui.painter().image(
                            logo.id(),
                            rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new("MetaForge").font(semibold(19.0)));
                    if self.demo {
                        widgets::badge(ui, "DEMO", p.info).on_hover_text(
                            "Invented files. Nothing on this PC is read or changed.",
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::icon_button(ui, Icon::INFO, "About MetaForge").clicked() {
                            self.about_open = true;
                        }
                        ui.add_space(6.0);
                        widgets::badge(ui, &Icon::LOCK.with("Offline: nothing is uploaded"), p.ok)
                            .on_hover_text(
                            "MetaForge makes no network requests. Your files never leave this PC.",
                        );
                        if self.reading > 0 {
                            ui.add_space(10.0);
                            ui.label(RichText::new("Reading files…").color(p.muted));
                            ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        }
                    });
                });
            });
    }

    fn rail(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        egui::SidePanel::left("rail")
            .exact_width(196.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.rail)
                    .inner_margin(Margin::symmetric(10, 14)),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(
                    full.right() + 10.0,
                    full.y_range().expand(14.0),
                    Stroke::new(1.0_f32, p.border),
                );
                ui.spacing_mut().item_spacing.y = 4.0;
                let items: [(Tab, Icon, &str, Option<usize>); 4] = [
                    (Tab::Files, Icon::LIST, "Files", Some(self.items.len())),
                    (Tab::Compare, Icon::COMPARE, "Compare", None),
                    (Tab::Rename, Icon::RENAME, "Rename", None),
                    (Tab::Settings, Icon::SETTINGS, "Settings", None),
                ];
                for (tab, icon, label, count) in items {
                    if tab == Tab::Settings {
                        ui.add_space(8.0);
                        widgets::divider(ui);
                        ui.add_space(8.0);
                    }
                    let active = self.tab == tab;
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.selected);
                        let bar = Rect::from_min_size(
                            rect.left_center() - vec2(0.0, 9.0),
                            vec2(3.0, 18.0),
                        );
                        ui.painter()
                            .rect_filled(bar, CornerRadius::same(2), p.accent);
                    } else if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(9), p.hover);
                    }
                    let fg = if active { p.text } else { p.muted };
                    ui.painter().text(
                        rect.left_center() + vec2(24.0, 0.0),
                        Align2::CENTER_CENTER,
                        icon.glyph(),
                        regular(16.0),
                        if active { p.accent } else { fg },
                    );
                    ui.painter().text(
                        rect.left_center() + vec2(46.0, -1.0),
                        Align2::LEFT_CENTER,
                        label,
                        if active {
                            semibold(14.5)
                        } else {
                            regular(14.5)
                        },
                        fg,
                    );
                    if let Some(n) = count.filter(|n| *n > 0) {
                        ui.painter().text(
                            rect.right_center() - vec2(12.0, 1.0),
                            Align2::RIGHT_CENTER,
                            n.to_string(),
                            regular(12.5),
                            p.faint,
                        );
                    }
                    if resp.clicked() {
                        self.tab = tab;
                        if tab == Tab::Compare && self.compare.is_none() {
                            self.compare_from_selection(false);
                        }
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                                .size(12.0)
                                .color(p.faint),
                        );
                    });
                    ui.add_space(4.0);
                    let risky = self
                        .items
                        .iter()
                        .filter(|i| !i.meta.flags().is_empty())
                        .count();
                    if !self.items.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
                            let (color, text) = if risky == 0 {
                                (p.ok, "All files are clean".to_string())
                            } else if risky == 1 {
                                (p.warn, "1 file reveals personal info".to_string())
                            } else {
                                (p.warn, format!("{risky} files reveal personal info"))
                            };
                            ui.painter().circle_filled(dot.center(), 4.0, color);
                            ui.label(RichText::new(text).size(12.0).color(p.muted));
                        });
                    }
                });
            });
    }

    // ----------------------------------------------------------------------------------
    // Keyboard and drag and drop
    // ----------------------------------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let (open, find, all, delete, escape) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::O),
                i.modifiers.command && i.key_pressed(Key::F),
                i.modifiers.command && i.key_pressed(Key::A),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::Escape),
            )
        });
        if escape {
            if self.edit.is_some() {
                self.edit = None;
            } else if self.dates.is_some() {
                self.dates = None;
            } else if let Some(d) = &self.clean {
                if !matches!(d.phase, Phase::Running { .. }) {
                    self.close_clean();
                }
            } else if self.about_open {
                self.about_open = false;
            } else if !self.search.is_empty() && self.tab == Tab::Files {
                self.search.clear();
            }
            return;
        }
        if self.modal_open() {
            return;
        }
        if open {
            self.pick_files();
        }
        if find {
            self.tab = Tab::Files;
            self.focus_search = true;
        }
        if self.tab == Tab::Files && !typing {
            if all {
                let visible = self.visible();
                let every = visible.iter().all(|i| self.items[*i].selected);
                for i in visible {
                    self.items[i].selected = !every;
                }
            }
            if delete {
                self.remove_selection();
            }
            self.list_keys(ctx);
        }
    }

    fn handle_drop(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_paths(dropped, false);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            let p = pal_ctx(ctx);
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-overlay"),
            ));
            painter.rect_filled(screen, 0, Color32::from_black_alpha(150));
            let inner = screen.shrink(28.0);
            painter.rect(
                inner,
                CornerRadius::same(20),
                p.accent.gamma_multiply(0.12),
                Stroke::new(2.0_f32, p.accent),
                egui::StrokeKind::Inside,
            );
            painter.text(
                inner.center() - vec2(0.0, 24.0),
                Align2::CENTER_CENTER,
                Icon::UPLOAD.glyph(),
                regular(44.0),
                p.accent,
            );
            painter.text(
                inner.center() + vec2(0.0, 26.0),
                Align2::CENTER_CENTER,
                "Drop to add",
                semibold(24.0),
                Color32::WHITE,
            );
            painter.text(
                inner.center() + vec2(0.0, 56.0),
                Align2::CENTER_CENTER,
                "Files are only read on this PC.",
                regular(14.0),
                Color32::from_gray(220),
            );
        }
    }

    // ----------------------------------------------------------------------------------
    // About and toasts
    // ----------------------------------------------------------------------------------

    fn about_dialog(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let p = pal_ctx(ctx);
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("about"))
            .frame(
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(28)),
            )
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
                    if let Some(logo) = &self.logo {
                        ui.painter().image(
                            logo.id(),
                            rect,
                            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new("MetaForge").font(semibold(26.0)));
                    ui.label(
                        RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                            .color(p.muted),
                    );
                    ui.add_space(10.0);
                    ui.add(
                        egui::Label::new(
                            "See and remove the hidden information in your files before you share them.",
                        )
                        .wrap(),
                    );
                    ui.add_space(10.0);
                    ui.label(RichText::new("Developed by Zheer Khalid").font(semibold(15.0)));
                    ui.add_space(2.0);
                    ui.hyperlink_to(
                        RichText::new("zheer.dev").color(p.accent),
                        "https://zheer.dev",
                    );
                    ui.hyperlink_to(
                        RichText::new("github.com/zheerkhalid/metaforge").color(p.accent),
                        "https://github.com/zheerkhalid/metaforge",
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Free and open source under the MIT License.")
                            .size(12.0)
                            .color(p.faint),
                    );
                    ui.add_space(14.0);
                    if widgets::primary_button(ui, "Close", true).clicked() {
                        close = true;
                    }
                });
            });
        if close || modal.should_close() {
            self.about_open = false;
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        self.toasts
            .retain(|t| t.born.elapsed().as_secs_f32() < if t.error { 8.0 } else { 4.0 });
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let p = pal_ctx(ctx);
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -18.0))
            .order(egui::Order::Tooltip)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for t in self.toasts.iter().rev().take(4) {
                        let color = if t.error { p.danger } else { p.accent };
                        Frame::new()
                            .fill(p.card)
                            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.7)))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(14, 10))
                            .show(ui, |ui| {
                                ui.set_max_width(420.0);
                                ui.horizontal(|ui| {
                                    let icon = if t.error { Icon::ERROR } else { Icon::DONE };
                                    ui.label(RichText::new(icon.glyph()).color(color));
                                    ui.add(egui::Label::new(t.text.as_str()).wrap());
                                });
                            });
                    }
                });
            });
    }

    /// The standard page header: title, subtitle and right-aligned actions.
    pub(super) fn page_header(
        ui: &mut Ui,
        title: &str,
        subtitle: &str,
        actions: impl FnOnce(&mut Ui),
    ) {
        let p = pal(ui);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 52.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).font(semibold(24.0)));
                    ui.label(RichText::new(subtitle).color(p.muted));
                });
                ui.with_layout(Layout::right_to_left(Align::Center), actions);
            },
        );
        ui.add_space(6.0);
    }

    /// Sets up the compare view from the selection (two files), or the first two.
    pub(super) fn compare_from_selection(&mut self, strict: bool) {
        let sel = self.selection();
        let pair = if sel.len() == 2 {
            Some((sel[0], sel[1]))
        } else if strict {
            self.toast("Tick exactly two files to compare them.", false);
            None
        } else if self.items.len() >= 2 {
            let a = self.current.unwrap_or(0);
            let b = if a == 0 { 1 } else { 0 };
            Some((a, b))
        } else {
            None
        };
        if let Some((a, b)) = pair {
            self.compare = Some((self.items[a].meta.clone(), self.items[b].meta.clone()));
            self.tab = Tab::Compare;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages();
        self.handle_screenshot(ctx);
        self.poll_handoff();
        self.handle_keys(ctx);

        self.header(ctx);
        self.rail(ctx);
        let p = pal_ctx(ctx);
        match self.tab {
            Tab::Files => self.files_view(ctx),
            tab => {
                egui::CentralPanel::default()
                    .frame(
                        Frame::new()
                            .fill(p.bg)
                            .inner_margin(Margin::symmetric(24, 18)),
                    )
                    .show(ctx, |ui| match tab {
                        Tab::Compare => self.compare_page(ui),
                        Tab::Rename => self.rename_page(ui),
                        _ => self.settings_page(ui),
                    });
            }
        }

        self.clean_dialog(ctx);
        self.edit_dialog(ctx);
        self.dates_dialog(ctx);
        self.about_dialog(ctx);
        self.handle_drop(ctx);
        self.toasts_ui(ctx);
        self.save_settings_if_changed();
        if !self.demo {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.reading > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Screenshots must not depend on where the mouse happens to be:
        // no hover highlights, no tooltips.
        if self.shot.is_some() {
            raw_input.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerMoved(_)
                        | egui::Event::MouseMoved(_)
                        | egui::Event::PointerButton { .. }
                        | egui::Event::MouseWheel { .. }
                )
            });
            raw_input.events.push(egui::Event::PointerGone);
            raw_input.hovered_files.clear();
            raw_input.dropped_files.clear();
        }
    }
}
