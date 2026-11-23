//! The clean dialog: what to remove, where the cleaned files go, the
//! progress of the batch and the report afterwards.

use super::app::{App, Msg};
use super::theme::{pal, pal_ctx, semibold, Icon};
use super::{bidi, widgets};
use crate::engine::{self, CleanJob, CleanResult, Output};
use crate::meta::{Category, CleanOptions, Meta, Preset};
use crate::settings::{OutputMode, Settings};
use crate::{demo, util};
use eframe::egui::{self, vec2, Align, Layout, RichText, Ui};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct CleanDlg {
    pub paths: Vec<PathBuf>,
    /// The choices, starting from the saved defaults.
    pub s: Settings,
    pub phase: Phase,
}

pub enum Phase {
    Setup,
    Running {
        total: usize,
        results: Vec<CleanResult>,
        cancel: Arc<AtomicBool>,
    },
    Report {
        results: Vec<CleanResult>,
        cancelled: bool,
        copies_added: bool,
        /// The demo's cleaned copies, added to the list on request.
        after: Vec<Meta>,
    },
}

impl CleanDlg {
    pub fn job(&self) -> CleanJob {
        self.s.clean_job()
    }
}

/// `(fields, files)` a clean with `opts` would remove: the preview line.
pub fn preview(metas: &[&Meta], opts: &CleanOptions) -> (usize, usize) {
    let mut fields = 0;
    let mut files = 0;
    for m in metas {
        let n = engine::targeted(&m.fields, opts).len();
        fields += n;
        if n > 0 {
            files += 1;
        }
    }
    (fields, files)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The preview sentence, e.g. "Will remove 23 fields from 4 files".
pub fn preview_text(fields: usize, files: usize) -> String {
    if fields == 0 {
        "Nothing to remove with these choices.".to_string()
    } else {
        format!(
            "Will remove {} from {}",
            plural(fields, "field", "fields"),
            plural(files, "file", "files")
        )
    }
}

const DEMO_FOLDER: &str = "C:\\Users\\Alex\\Desktop\\Cleaned";

impl App {
    pub(super) fn open_clean(&mut self) {
        let paths: Vec<PathBuf> = self
            .selection()
            .into_iter()
            .map(|i| self.items[i].meta.path.clone())
            .collect();
        if paths.is_empty() {
            self.toast("Select a file first.", false);
            return;
        }
        self.clean = Some(Box::new(CleanDlg {
            paths,
            s: self.settings.clone(),
            phase: Phase::Setup,
        }));
    }

    pub(super) fn close_clean(&mut self) {
        if let Some(d) = &self.clean {
            if let Phase::Running { cancel, .. } = &d.phase {
                cancel.store(true, Ordering::Relaxed);
                return;
            }
        }
        self.clean = None;
    }

    fn start_clean(&mut self) {
        let Some(d) = &mut self.clean else { return };
        // The choices become the new defaults.
        let s = &d.s;
        self.settings.preset = s.preset;
        self.settings.custom = s.custom;
        self.settings.keep_orientation = s.keep_orientation;
        self.settings.keep_color_profile = s.keep_color_profile;
        self.settings.keep_file_dates = s.keep_file_dates;
        self.settings.output_mode = s.output_mode;
        self.settings.suffix = s.suffix.clone();
        self.settings.folder = s.folder.clone();

        let job = d.job();
        let cancel = Arc::new(AtomicBool::new(false));
        d.phase = Phase::Running {
            total: d.paths.len(),
            results: Vec::new(),
            cancel: cancel.clone(),
        };
        if self.demo {
            let metas: Vec<Meta> = d
                .paths
                .iter()
                .filter_map(|p| self.items.iter().find(|i| &i.meta.path == p))
                .map(|i| i.meta.clone())
                .collect();
            self.spawn(move |send| {
                let mut after = Vec::new();
                for m in &metas {
                    std::thread::sleep(Duration::from_millis(260));
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let (r, a) = demo::clean(m, &job);
                    send(Msg::CleanProgress(r));
                    after.push(a);
                }
                send(Msg::CleanDone { after });
            });
        } else {
            let paths = d.paths.clone();
            self.spawn(move |send| {
                engine::clean_batch(&paths, &job, &cancel, |_, _, r| {
                    send(Msg::CleanProgress(r.clone()))
                });
                send(Msg::CleanDone { after: Vec::new() });
            });
        }
    }

    pub(super) fn clean_finished(&mut self, after: Vec<Meta>) {
        let Some(d) = &mut self.clean else { return };
        let Phase::Running {
            results, cancel, ..
        } = &mut d.phase
        else {
            return;
        };
        let results = std::mem::take(results);
        let cancelled = cancel.load(Ordering::Relaxed);
        let replace = d.s.output() == Output::Replace;
        d.phase = Phase::Report {
            results: results.clone(),
            cancelled,
            copies_added: false,
            after: if replace { Vec::new() } else { after.clone() },
        };
        if replace {
            if self.demo {
                for a in after {
                    let src = a.path.clone();
                    self.upsert(Some(&src), a);
                }
            } else {
                let pairs = results
                    .iter()
                    .filter(|r| r.error.is_none())
                    .map(|r| (r.src.clone(), r.dst.clone()))
                    .collect();
                self.reinspect(pairs);
            }
        }
    }

    fn add_cleaned_copies(&mut self) {
        let Some(d) = &mut self.clean else { return };
        let Phase::Report {
            results,
            copies_added,
            after,
            ..
        } = &mut d.phase
        else {
            return;
        };
        *copies_added = true;
        if self.demo {
            let after = std::mem::take(after);
            for a in after {
                self.upsert(None, a);
            }
        } else {
            let paths: Vec<PathBuf> = results
                .iter()
                .filter(|r| r.error.is_none() && r.dst != r.src)
                .map(|r| r.dst.clone())
                .collect();
            self.reinspect(paths.iter().map(|p| (p.clone(), p.clone())).collect());
        }
        self.toast("The cleaned copies were added to the list.", false);
    }

    pub(super) fn clean_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.clean.take() else {
            return;
        };
        let p = pal_ctx(ctx);
        let max_h = (ctx.screen_rect().height() - 210.0).max(240.0);
        let mut action = Action::None;
        let modal = egui::Modal::new(egui::Id::new("clean-dialog"))
            .frame(widgets::modal_frame(&p))
            .show(ctx, |ui| {
                ui.set_width(660.0);
                let kind = match d.phase {
                    Phase::Setup => 0,
                    Phase::Running { .. } => 1,
                    Phase::Report { .. } => 2,
                };
                match kind {
                    0 => action = self.clean_setup(ui, &mut d, max_h),
                    1 => running_ui(ui, &mut d),
                    _ => action = self.clean_report(ui, &mut d, max_h),
                }
            });
        let running = matches!(d.phase, Phase::Running { .. });
        if modal.should_close() && !running {
            action = Action::Close;
        }
        self.clean = Some(d);
        match action {
            Action::None => {}
            Action::Close => self.clean = None,
            Action::Start => self.start_clean(),
            Action::AddCopies => self.add_cleaned_copies(),
        }
    }

    fn clean_setup(&mut self, ui: &mut Ui, d: &mut CleanDlg, max_h: f32) -> Action {
        let p = pal(ui);
        let n = d.paths.len();
        let subtitle = if n == 1 {
            d.paths[0]
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            format!("{n} files selected")
        };
        widgets::dialog_title(ui, "Clean for sharing", &subtitle);
        egui::ScrollArea::vertical()
            .max_height(max_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(RichText::new("What to remove").font(semibold(14.5)));
                for (preset, text) in [
                    (
                        Preset::Everything,
                        "Location, names, device and serial numbers, dates, software, comments and hidden content. The picture, text or sound itself stays exactly as it is.",
                    ),
                    (
                        Preset::LocationAndNames,
                        "Only the GPS position and place names, and people's and company names. Dates and camera details stay.",
                    ),
                    (Preset::Custom, "Choose the groups yourself."),
                ] {
                    if widgets::radio_card(ui, d.s.preset == preset, preset.label(), text) {
                        d.s.preset = preset;
                    }
                }
                if d.s.preset == Preset::Custom {
                    ui.add_space(2.0);
                    egui::Grid::new("custom-cats")
                        .num_columns(2)
                        .spacing(vec2(18.0, 4.0))
                        .show(ui, |ui| {
                            let cats: Vec<Category> = Category::ALL
                                .into_iter()
                                .filter(|c| *c != Category::Technical)
                                .collect();
                            for (k, c) in cats.iter().enumerate() {
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 0.0;
                                    let mut on = d.s.custom.contains(*c);
                                    if widgets::checkbox(ui, &mut on, c.title()).changed() {
                                        d.s.custom.set(*c, on);
                                    }
                                    ui.horizontal(|ui| {
                                        ui.add_space(26.0);
                                        ui.label(
                                            RichText::new(c.blurb()).size(11.5).color(p.faint),
                                        );
                                    });
                                });
                                if k % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        });
                }
                ui.add_space(8.0);
                ui.label(RichText::new("Keep").font(semibold(14.5)));
                widgets::checkbox(
                    ui,
                    &mut d.s.keep_orientation,
                    "Keep photo orientation (so pictures are not shown sideways)",
                );
                widgets::checkbox(
                    ui,
                    &mut d.s.keep_color_profile,
                    "Keep colour profile (so colours do not change)",
                );
                widgets::checkbox(
                    ui,
                    &mut d.s.keep_file_dates,
                    "Keep the original file dates (created and modified)",
                );
                ui.add_space(8.0);
                ui.label(RichText::new("Where to save").font(semibold(14.5)));
                self.output_choice(ui, &mut d.s, true);
            });
        ui.add_space(8.0);
        let job = d.job();
        let metas: Vec<&Meta> = d
            .paths
            .iter()
            .filter_map(|p| self.items.iter().find(|i| &i.meta.path == p))
            .map(|i| &i.meta)
            .collect();
        let (fields, files) = preview(&metas, &job.opts);
        let replace = d.s.output_mode == OutputMode::Replace;
        widgets::note_card(
            ui,
            if fields == 0 { Icon::INFO } else { Icon::BROOM },
            if fields == 0 { p.muted } else { p.accent },
            &preview_text(fields, files),
        );
        if replace && fields > 0 {
            widgets::note_card(
                ui,
                Icon::WARNING,
                p.warn,
                "The originals are overwritten, after each cleaned file has been checked. This cannot be undone.",
            );
        }
        ui.add_space(6.0);
        let mut action = Action::None;
        widgets::button_row(ui, |ui| {
            let label = if n == 1 {
                "Clean".to_string()
            } else {
                format!("Clean {n} files")
            };
            let ok = fields > 0 && !job.opts.remove.is_empty();
            if widgets::primary_button(ui, &Icon::BROOM.with(&label), ok).clicked() {
                action = Action::Start;
            }
            if widgets::ghost_button(ui, "Cancel").clicked() {
                action = Action::Close;
            }
        });
        action
    }

    /// The three output choices (also used on the Settings page).
    pub(super) fn output_choice(&mut self, ui: &mut Ui, s: &mut Settings, for_clean: bool) {
        let p = pal(ui);
        widgets::row(ui, 34.0, |ui| {
            if widgets::radio(
                ui,
                s.output_mode == OutputMode::Suffix,
                "Save a copy next to each file, ending in",
            ) {
                s.output_mode = OutputMode::Suffix;
            }
            let r = widgets::text_field(ui, &mut s.suffix, "_clean", 120.0);
            if r.gained_focus() {
                s.output_mode = OutputMode::Suffix;
            }
            let suffix = if s.suffix.trim().is_empty() {
                "_clean"
            } else {
                s.suffix.trim()
            };
            ui.label(
                RichText::new(format!("photo{suffix}.jpg"))
                    .size(12.0)
                    .color(p.faint),
            );
        });
        widgets::row(ui, 34.0, |ui| {
            if widgets::radio(
                ui,
                s.output_mode == OutputMode::Folder,
                "Save copies in a folder",
            ) {
                s.output_mode = OutputMode::Folder;
            }
            if widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Choose…")).clicked() {
                if self.demo {
                    s.folder = Some(PathBuf::from(DEMO_FOLDER));
                    s.output_mode = OutputMode::Folder;
                } else if let Some(f) = rfd::FileDialog::new()
                    .set_title("Folder for the cleaned copies")
                    .pick_folder()
                {
                    s.folder = Some(f);
                    s.output_mode = OutputMode::Folder;
                }
            }
            match &s.folder {
                Some(f) => {
                    bidi::label(ui, &f.to_string_lossy(), 13.0, false, p.muted);
                }
                None => {
                    ui.label(
                        RichText::new("No folder chosen yet")
                            .size(13.0)
                            .color(p.faint),
                    );
                }
            }
        });
        widgets::row(ui, 30.0, |ui| {
            if widgets::radio(
                ui,
                s.output_mode == OutputMode::Replace,
                "Replace the original files",
            ) {
                s.output_mode = OutputMode::Replace;
            }
        });
        if s.output_mode == OutputMode::Folder && s.folder.is_none() && for_clean {
            ui.label(
                RichText::new("Choose a folder, or the copies are saved next to the files.")
                    .size(12.0)
                    .color(p.warn),
            );
        }
    }

    fn clean_report(&mut self, ui: &mut Ui, d: &mut CleanDlg, max_h: f32) -> Action {
        let p = pal(ui);
        let Phase::Report {
            results,
            cancelled,
            copies_added,
            ..
        } = &d.phase
        else {
            return Action::None;
        };
        let s = engine::summarize(results);
        let all_ok = s.failed == 0 && s.not_verified == 0;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), egui::Sense::hover());
            let color = if all_ok { p.ok } else { p.warn };
            ui.painter()
                .circle_filled(r.center(), 22.0, color.gamma_multiply(0.15));
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                if all_ok {
                    Icon::CHECK.glyph()
                } else {
                    Icon::WARNING.glyph()
                },
                super::theme::regular(20.0),
                color,
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let title = if *cancelled {
                    "Cleaning stopped".to_string()
                } else if all_ok {
                    "Ready to share".to_string()
                } else {
                    "Cleaned, with problems".to_string()
                };
                ui.label(RichText::new(title).font(semibold(20.0)));
                ui.label(
                    RichText::new(format!(
                        "{} cleaned · {} had nothing to remove · {} removed{}",
                        plural(s.cleaned, "file", "files"),
                        s.already_clean,
                        plural(s.fields_removed, "field", "fields"),
                        if s.failed > 0 {
                            format!(" · {} failed", s.failed)
                        } else {
                            String::new()
                        }
                    ))
                    .color(p.muted),
                );
            });
        });
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height((max_h - 60.0).min(430.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for r in results {
                    report_row(ui, r);
                }
            });
        ui.add_space(8.0);
        let copies = results.iter().any(|r| r.error.is_none() && r.dst != r.src);
        let first_dst = results
            .iter()
            .find(|r| r.error.is_none())
            .map(|r| r.dst.clone());
        let mut action = Action::None;
        let mut save = false;
        let mut explorer = false;
        widgets::button_row(ui, |ui| {
            if widgets::primary_button(ui, "Done", true).clicked() {
                action = Action::Close;
            }
            if copies
                && !*copies_added
                && widgets::ghost_button(ui, &Icon::ADD.with("Add cleaned copies to the list"))
                    .clicked()
            {
                action = Action::AddCopies;
            }
            if widgets::ghost_button(ui, &Icon::SAVE.with("Save report…")).clicked() {
                save = true;
            }
            if first_dst.is_some()
                && widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Show in Explorer")).clicked()
            {
                explorer = true;
            }
        });
        if save {
            let text = engine::report_text(results);
            if self.demo {
                self.demo_toast();
            } else if let Some(path) = rfd::FileDialog::new()
                .set_title("Save the cleaning report")
                .set_file_name("MetaForge report.txt")
                .add_filter("Text", &["txt"])
                .save_file()
            {
                match std::fs::write(&path, text) {
                    Ok(()) => self.toast("Report saved.", false),
                    Err(e) => self.toast(format!("The report could not be saved: {e}"), true),
                }
            }
        }
        if explorer {
            if let Some(dst) = first_dst {
                self.show_in_explorer(&dst);
            }
        }
        action
    }
}

fn running_ui(ui: &mut Ui, d: &mut CleanDlg) {
    let p = pal(ui);
    let Phase::Running {
        total,
        results,
        cancel,
    } = &mut d.phase
    else {
        return;
    };
    widgets::dialog_title(ui, "Cleaning…", "");
    let done = results.len();
    ui.add(
        egui::ProgressBar::new(done as f32 / (*total).max(1) as f32)
            .desired_height(10.0)
            .fill(p.accent_fill)
            .animate(true),
    );
    let name = d
        .paths
        .get(done)
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} of {total}", done.min(*total))).color(p.muted));
        if !name.is_empty() {
            bidi::label(ui, &name, 14.0, false, p.muted);
        }
    });
    widgets::para(
        ui,
        "Each cleaned file is read back and checked before anything is saved.",
    );
    ui.add_space(8.0);
    let stopping = cancel.load(Ordering::Relaxed);
    widgets::button_row(ui, |ui| {
        if widgets::ghost_button_enabled(
            ui,
            if stopping { "Stopping…" } else { "Cancel" },
            !stopping,
        )
        .clicked()
        {
            cancel.store(true, Ordering::Relaxed);
        }
    });
}

enum Action {
    None,
    Close,
    Start,
    AddCopies,
}

fn report_row(ui: &mut Ui, r: &CleanResult) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.bg)
        .stroke(egui::Stroke::new(1.0_f32, p.border))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            let name = r
                .src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            widgets::row(ui, 22.0, |ui| {
                let w = (ui.available_width() - 190.0).max(120.0);
                ui.allocate_ui_with_layout(
                    vec2(w, 22.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        bidi::label(ui, &name, 14.0, true, p.text);
                    },
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(_e) = &r.error {
                        ui.label(RichText::new(Icon::ERROR.with("Failed")).color(p.danger));
                    } else if !r.left.is_empty() {
                        ui.label(
                            RichText::new(Icon::WARNING.with("Not fully cleaned")).color(p.warn),
                        );
                    } else if r.already_clean {
                        ui.label(RichText::new(Icon::CHECK.with("Nothing to remove")).color(p.ok));
                    } else if r.verified {
                        ui.label(RichText::new(Icon::CHECK.with("Verified")).color(p.ok))
                            .on_hover_text(
                                "The cleaned file was read back: nothing you chose is left.",
                            );
                    }
                    if r.error.is_none() && !r.removed.is_empty() {
                        ui.label(
                            RichText::new(plural(
                                r.removed.len(),
                                "field removed",
                                "fields removed",
                            ))
                            .size(12.5)
                            .color(p.muted),
                        );
                    }
                });
            });
            if let Some(e) = &r.error {
                ui.add(egui::Label::new(RichText::new(e).size(12.5).color(p.danger)).wrap());
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(5.0, 4.0);
                widgets::flag_badges(ui, &r.flags_before, false);
                ui.label(RichText::new("→").color(p.faint));
                widgets::flag_badges(ui, &r.flags_after, false);
                if util::human_size(r.size_before) != util::human_size(r.size_after) {
                    ui.label(
                        RichText::new(format!(
                            "{} → {}",
                            util::human_size(r.size_before),
                            util::human_size(r.size_after)
                        ))
                        .size(12.0)
                        .color(p.faint),
                    );
                }
            });
            if !r.left.is_empty() {
                let names: Vec<&str> = r.left.iter().map(|f| f.label.as_str()).collect();
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Still there: {}", names.join(", ")))
                            .size(12.5)
                            .color(p.warn),
                    )
                    .wrap(),
                );
            }
            if r.dst != r.src {
                let saved = r
                    .dst
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(RichText::new("Saved as").size(12.0).color(p.faint));
                    bidi::label(ui, &saved, 12.0, false, p.faint);
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::CatSet;

    #[test]
    fn preview_counts_targeted_fields() {
        let files = demo::files();
        let refs: Vec<&Meta> = files.iter().collect();
        let all = CleanOptions::everything();
        let (fields, n) = preview(&refs, &all);
        assert_eq!(
            fields,
            files.iter().map(|m| m.removable_count()).sum::<usize>()
        );
        // The screenshot is already clean.
        assert_eq!(n, files.len() - 1);
        let loc = CleanOptions::only(&[Category::Location]);
        let (lf, ln) = preview(&refs, &loc);
        assert!(lf > 0 && lf < fields);
        assert_eq!(
            ln,
            files
                .iter()
                .filter(|m| m.removable_in(Category::Location) > 0)
                .count()
        );
        let none = CleanOptions {
            remove: CatSet::EMPTY,
            ..all
        };
        assert_eq!(preview(&refs, &none), (0, 0));
        assert_eq!(preview_text(23, 4), "Will remove 23 fields from 4 files");
        assert_eq!(preview_text(1, 1), "Will remove 1 field from 1 file");
        assert_eq!(preview_text(0, 0), "Nothing to remove with these choices.");
    }
}
