//! The Rename, Compare and Settings pages.

use super::app::{App, Tab};
use super::theme::{self, pal, regular, semibold, Icon};
use super::widgets;
use crate::context_menu::{self, MenuStatus};
use crate::engine::{self, CompareRow, Rename, RENAME_TOKENS};
use crate::meta::{Category, Meta, Preset};
use crate::settings::ThemeChoice;
use eframe::egui::{
    self, pos2, vec2, Align, Align2, CornerRadius, Frame, Layout, Margin, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui,
};
use std::path::PathBuf;

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A card with a title and a muted explanation.
fn settings_card(ui: &mut Ui, title: &str, text: &str, add: impl FnOnce(&mut Ui)) {
    let p = pal(ui);
    widgets::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(title).font(semibold(16.0)));
        if !text.is_empty() {
            ui.add_space(-4.0);
            ui.add(egui::Label::new(RichText::new(text).size(13.0).color(p.muted)).wrap());
        }
        ui.add_space(2.0);
        add(ui);
    });
}

/// A switch with a title and an explanation. Returns true when clicked.
fn toggle_row(ui: &mut Ui, on: bool, title: &str, text: &str, enabled: bool) -> bool {
    let p = pal(ui);
    let mut clicked = false;
    ui.horizontal(|ui| {
        let w = ui.available_width() - 56.0;
        ui.allocate_ui_with_layout(vec2(w, 36.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(w);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(title).color(if enabled { p.text } else { p.faint }));
            if !text.is_empty() {
                ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.muted)).wrap());
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            clicked = widgets::toggle(ui, on, enabled).clicked() && enabled;
        });
    });
    clicked
}

impl App {
    // ----------------------------------------------------------------------------------
    // Rename
    // ----------------------------------------------------------------------------------

    fn rename_targets(&self) -> Vec<usize> {
        self.selection()
    }

    pub(super) fn rename_page(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let targets = self.rename_targets();
        let n = targets.len();
        App::page_header(
            ui,
            "Rename from metadata",
            &if n == 0 {
                "Give files names from the date they were taken, the camera and more.".to_string()
            } else {
                format!(
                    "{} · the ticked files, or the one shown in Files",
                    if n == 1 {
                        "1 file".to_string()
                    } else {
                        format!("{n} files")
                    }
                )
            },
            |_| {},
        );
        if n == 0 {
            widgets::empty_state(
                ui,
                Icon::RENAME,
                p.accent,
                "No files to rename",
                "Add files on the Files page and tick the ones to rename.",
            );
            return;
        }
        widgets::card(ui, |ui| {
            ui.label(RichText::new("Pattern").font(semibold(14.5)));
            let w = ui.available_width();
            widgets::text_field(ui, &mut self.settings.rename_pattern, "{date}_{time}", w);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(RichText::new("Insert").size(12.5).color(p.faint));
                for (token, what) in RENAME_TOKENS {
                    if widgets::chip(ui, token, None, false)
                        .on_hover_text(*what)
                        .clicked()
                    {
                        self.settings.rename_pattern.push_str(token);
                    }
                }
            });
            ui.label(
                RichText::new("The extension is kept. Names that would clash get (2), (3)…")
                    .size(12.0)
                    .color(p.faint),
            );
        });
        ui.add_space(10.0);

        let paths: Vec<PathBuf> = targets
            .iter()
            .map(|i| self.items[*i].meta.path.clone())
            .collect();
        let pattern = self.settings.rename_pattern.clone();
        let fresh =
            matches!(&self.rename_cache, Some((pat, ps, _)) if *pat == pattern && *ps == paths);
        if !fresh {
            let metas: Vec<Meta> = targets
                .iter()
                .map(|i| self.items[*i].meta.clone())
                .collect();
            let plan = engine::rename_plan(&metas, &pattern);
            self.rename_cache = Some((pattern, paths, plan));
        }
        let plan: Vec<Rename> = self
            .rename_cache
            .as_ref()
            .map(|c| c.2.clone())
            .unwrap_or_default();
        let ready = plan
            .iter()
            .filter(|r| r.problem.is_none() && r.from != r.to)
            .count();

        let list_h = (ui.available_height() - 56.0).max(100.0);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), list_h),
            Layout::top_down(Align::Min),
            |ui| {
                Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let w = ui.available_width();
                        let col = (w - 40.0) / 2.0;
                        widgets::row(ui, 22.0, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.allocate_ui(vec2(col, 22.0), |ui| {
                                ui.set_min_width(col);
                                ui.label(RichText::new("Now").size(12.5).color(p.faint));
                            });
                            ui.add_space(40.0);
                            ui.label(RichText::new("New name").size(12.5).color(p.faint));
                        });
                        egui::ScrollArea::vertical()
                            .max_height(list_h - 60.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for r in &plan {
                                    let (rect, _) =
                                        ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
                                    ui.painter().hline(
                                        rect.x_range(),
                                        rect.top(),
                                        Stroke::new(1.0_f32, p.border.gamma_multiply(0.6)),
                                    );
                                    let left = Rect::from_min_size(rect.min, vec2(col - 8.0, 34.0));
                                    widgets::text_in(
                                        ui,
                                        left,
                                        &file_name(&r.from),
                                        regular(13.5),
                                        p.text,
                                    );
                                    ui.painter().text(
                                        pos2(rect.left() + col + 20.0, rect.center().y),
                                        Align2::CENTER_CENTER,
                                        "→",
                                        regular(14.0),
                                        p.faint,
                                    );
                                    let right = Rect::from_min_max(
                                        pos2(rect.left() + col + 40.0, rect.top()),
                                        rect.max,
                                    );
                                    match &r.problem {
                                        Some(problem) => {
                                            widgets::text_in(
                                                ui,
                                                right,
                                                &format!("{problem}; stays as it is"),
                                                regular(13.0),
                                                p.warn,
                                            );
                                        }
                                        None if r.from == r.to => {
                                            widgets::text_in(
                                                ui,
                                                right,
                                                "No change",
                                                regular(13.0),
                                                p.faint,
                                            );
                                        }
                                        None => {
                                            widgets::text_in(
                                                ui,
                                                right,
                                                &file_name(&r.to),
                                                semibold(13.5),
                                                p.accent,
                                            );
                                        }
                                    }
                                }
                            });
                    });
            },
        );
        ui.add_space(8.0);
        let mut apply = false;
        widgets::row(ui, 40.0, |ui| {
            let label = match ready {
                0 => "Rename".to_string(),
                1 => "Rename 1 file".to_string(),
                n => format!("Rename {n} files"),
            };
            apply = widgets::primary_button(ui, &Icon::RENAME.with(&label), ready > 0).clicked();
            if widgets::ghost_button(ui, "Back to Files").clicked() {
                self.tab = Tab::Files;
            }
        });
        if apply {
            if self.demo {
                self.demo_toast();
                return;
            }
            let failed = engine::apply_renames(&plan);
            let pairs: Vec<(PathBuf, PathBuf)> = plan
                .iter()
                .filter(|r| r.problem.is_none() && r.from != r.to)
                .filter(|r| !failed.iter().any(|(f, _)| *f == r.from))
                .map(|r| (r.from.clone(), r.to.clone()))
                .collect();
            let done = pairs.len();
            self.reinspect(pairs);
            self.rename_cache = None;
            if failed.is_empty() {
                self.toast(format!("Renamed {done} file(s)."), false);
            } else {
                let (f, e) = &failed[0];
                self.toast(
                    format!(
                        "Renamed {done}; {} could not be renamed ({}: {e}).",
                        failed.len(),
                        file_name(f)
                    ),
                    true,
                );
            }
        }
    }

    // ----------------------------------------------------------------------------------
    // Compare
    // ----------------------------------------------------------------------------------

    pub(super) fn compare_page(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let mut only = self.compare_only_diff;
        App::page_header(
            ui,
            "Compare",
            "Two files side by side: see what differs, for example before and after cleaning.",
            |ui| {
                ui.add_space(4.0);
                if widgets::toggle(ui, only, true).clicked() {
                    only = !only;
                }
                ui.label("Only differences");
            },
        );
        self.compare_only_diff = only;
        let Some((a, b)) = self.compare.clone() else {
            widgets::empty_state(
                ui,
                Icon::COMPARE,
                p.accent,
                "Pick two files",
                "Tick two files on the Files page and press Compare.",
            );
            return;
        };
        // The two pickers.
        let w = ui.available_width();
        let label_w = (w * 0.26).clamp(150.0, 230.0);
        let col = (w - label_w - 12.0) / 2.0;
        let mut pick_a = None;
        let mut pick_b = None;
        widgets::row(ui, 40.0, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.allocate_ui(vec2(label_w, 36.0), |ui| {
                ui.set_width(label_w);
                ui.label(RichText::new("Files").color(p.muted));
            });
            pick_a = self.file_picker(ui, "cmp-a", &a, col - 6.0);
            pick_b = self.file_picker(ui, "cmp-b", &b, col - 6.0);
        });
        if let Some(i) = pick_a {
            self.compare = Some((self.items[i].meta.clone(), b.clone()));
        }
        if let Some(i) = pick_b {
            self.compare = Some((a.clone(), self.items[i].meta.clone()));
        }
        ui.add_space(6.0);
        let rows = engine::compare(&a, &b);
        let diff = rows.iter().filter(|r| !r.same()).count();
        ui.label(
            RichText::new(match diff {
                0 => "The two files hold the same information.".to_string(),
                1 => "1 difference".to_string(),
                n => format!("{n} differences"),
            })
            .size(13.0)
            .color(p.muted),
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for cat in Category::ALL {
                    let in_cat: Vec<&CompareRow> = rows
                        .iter()
                        .filter(|r| r.category == cat && (!only || !r.same()))
                        .collect();
                    if in_cat.is_empty() {
                        continue;
                    }
                    widgets::card(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(cat.title()).font(semibold(14.5)));
                        ui.add_space(6.0);
                        for r in in_cat {
                            compare_row(ui, r, label_w, &p);
                        }
                    });
                }
            });
    }

    /// A button showing a file name that opens a list of the files.
    fn file_picker(&mut self, ui: &mut Ui, id: &str, current: &Meta, width: f32) -> Option<usize> {
        let p = pal(ui);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
        ui.painter().rect(
            rect,
            CornerRadius::same(8),
            if resp.hovered() { p.hover } else { p.card },
            Stroke::new(1.0_f32, p.border),
            StrokeKind::Inside,
        );
        let tile = Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(24.0, 24.0));
        widgets::paint_kind_tile(ui, tile, current.kind(), p.accent);
        let text = Rect::from_min_max(
            pos2(tile.right() + 8.0, rect.top()),
            pos2(rect.right() - 28.0, rect.bottom()),
        );
        widgets::text_in(ui, text, &current.name(), semibold(13.5), p.text);
        ui.painter().text(
            pos2(rect.right() - 14.0, rect.center().y),
            Align2::CENTER_CENTER,
            Icon::CHEVRON_DOWN.glyph(),
            regular(11.0),
            p.muted,
        );
        let popup = ui.make_persistent_id(id);
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup));
        }
        let mut picked = None;
        egui::popup_below_widget(
            ui,
            popup,
            &resp,
            egui::PopupCloseBehavior::CloseOnClick,
            |ui| {
                ui.set_min_width(width);
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for (i, item) in self.items.iter().enumerate() {
                            let (r, row) =
                                ui.allocate_exact_size(vec2(width - 12.0, 28.0), Sense::click());
                            if row.hovered() {
                                ui.painter().rect_filled(r, CornerRadius::same(6), p.hover);
                            }
                            let t = Rect::from_min_max(r.min + vec2(8.0, 0.0), r.max);
                            widgets::text_in(ui, t, &item.meta.name(), regular(13.5), p.text);
                            if row.clicked() {
                                picked = Some(i);
                            }
                        }
                    });
            },
        );
        if self.items.is_empty() && resp.clicked() {
            self.toast("Add files on the Files page first.", false);
        }
        picked
    }

    // ----------------------------------------------------------------------------------
    // Settings
    // ----------------------------------------------------------------------------------

    pub(super) fn settings_page(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        App::page_header(
            ui,
            "Settings",
            "Your defaults for cleaning and saving. Changes are saved automatically.",
            |_| {},
        );
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(860.0);
                ui.spacing_mut().item_spacing.y = 12.0;
                settings_card(ui, "Appearance", "", |ui| {
                    let options = ["System", "Dark", "Light"];
                    let current = match self.settings.theme {
                        ThemeChoice::System => 0,
                        ThemeChoice::Dark => 1,
                        ThemeChoice::Light => 2,
                    };
                    if let Some(i) = widgets::segmented(ui, &options, current) {
                        self.settings.theme = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light][i];
                        theme::apply_choice(ui.ctx(), self.settings.theme);
                    }
                });

                settings_card(
                    ui,
                    "Cleaning",
                    "What \"Clean for sharing\" removes unless you choose otherwise.",
                    |ui| {
                        let presets = [Preset::Everything, Preset::LocationAndNames, Preset::Custom];
                        let labels = ["Remove everything", "Location and names only", "Custom"];
                        let current = presets
                            .iter()
                            .position(|x| *x == self.settings.preset)
                            .unwrap_or(0);
                        if let Some(i) = widgets::segmented(ui, &labels, current) {
                            self.settings.preset = presets[i];
                        }
                        if self.settings.preset == Preset::Custom {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(16.0, 6.0);
                                for c in Category::ALL {
                                    if c == Category::Technical {
                                        continue;
                                    }
                                    let mut on = self.settings.custom.contains(c);
                                    if widgets::checkbox(ui, &mut on, c.title())
                                        .on_hover_text(c.blurb())
                                        .changed()
                                    {
                                        self.settings.custom.set(c, on);
                                    }
                                }
                            });
                        }
                        widgets::checkbox(
                            ui,
                            &mut self.settings.keep_orientation,
                            "Keep photo orientation (so pictures are not shown sideways)",
                        );
                        widgets::checkbox(
                            ui,
                            &mut self.settings.keep_color_profile,
                            "Keep colour profile (so colours do not change)",
                        );
                    },
                );

                let mut s = self.settings.clone();
                settings_card(
                    ui,
                    "Saving",
                    "Where cleaned and edited files go. The originals are only changed if you choose \"Replace\".",
                    |ui| {
                        self.output_choice(ui, &mut s, false);
                        widgets::checkbox(
                            ui,
                            &mut s.keep_file_dates,
                            "Keep the original file dates (created and modified)",
                        );
                    },
                );
                self.settings = s;

                settings_card(ui, "Details panel", "", |ui| {
                    if toggle_row(
                        ui,
                        self.settings.show_technical,
                        "Show technical names",
                        "Show where each field is stored (EXIF, XMP, Office properties…) and its id.",
                        true,
                    ) {
                        self.settings.show_technical = !self.settings.show_technical;
                    }
                });

                self.explorer_card(ui);

                settings_card(ui, "Privacy", "", |ui| {
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new(Icon::LOCK.glyph()).color(p.ok).size(16.0));
                        ui.add(
                            egui::Label::new(
                                "MetaForge makes no network requests. The only link it opens is \"Open in maps\", when you click it.",
                            )
                            .wrap(),
                        );
                    });
                    ui.label(
                        RichText::new(
                            "Settings are kept in %LOCALAPPDATA%\\MetaForge on this PC.",
                        )
                        .size(12.5)
                        .color(p.faint),
                    );
                });
                ui.add_space(8.0);
            });
    }

    fn explorer_card(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        settings_card(
            ui,
            "Explorer right-click menu",
            "Adds \"Show metadata with MetaForge\" and \"Clean metadata with MetaForge\" when you right-click files and folders. For your account only; no administrator rights needed. Nothing is cleaned until you confirm in the window.",
            |ui| {
                let status = self.menu_status.unwrap_or(MenuStatus::Off);
                let on = status != MenuStatus::Off;
                let detail = match status {
                    MenuStatus::On => "On",
                    MenuStatus::Off => "Off",
                    MenuStatus::OtherCopy => "On, but for a MetaForge.exe in another folder; switch off and on to point it here",
                };
                if toggle_row(ui, on, "Show MetaForge in the right-click menu", detail, true) {
                    self.toggle_menu(!on || status == MenuStatus::OtherCopy);
                }
                ui.label(
                    RichText::new("On Windows 11 the entries are under \"Show more options\" (or Shift + right-click).")
                        .size(12.0)
                        .color(p.faint),
                );
            },
        );
    }

    fn toggle_menu(&mut self, enable: bool) {
        if self.demo {
            self.toast("Demo: nothing is changed.", false);
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                self.toast(e.to_string(), true);
                return;
            }
        };
        let result = if enable {
            context_menu::register(context_menu::CLASSES, &exe)
        } else {
            context_menu::unregister(context_menu::CLASSES)
        };
        match result {
            Ok(()) => self.toast(
                if enable {
                    "Added to the Explorer right-click menu."
                } else {
                    "Removed from the Explorer right-click menu."
                },
                false,
            ),
            Err(e) => self.toast(e, true),
        }
        self.menu_status = Some(context_menu::status(context_menu::CLASSES, &exe));
    }
}

fn compare_row(ui: &mut Ui, r: &CompareRow, label_w: f32, p: &theme::Palette) {
    let w = ui.available_width();
    let col = (w - label_w - 12.0) / 2.0;
    let same = r.same();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
    if !same {
        ui.painter().rect_filled(
            rect.expand2(vec2(6.0, 0.0)),
            CornerRadius::same(6),
            p.warn.gamma_multiply(0.09),
        );
    }
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        Stroke::new(1.0_f32, p.border.gamma_multiply(0.5)),
    );
    let label = Rect::from_min_size(rect.min, vec2(label_w - 8.0, 30.0));
    widgets::text_in(ui, label, &r.label, regular(13.0), p.muted);
    let value = |x: f32, v: &Option<String>| {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(col - 12.0, 30.0));
        match v {
            Some(text) => widgets::text_in(ui, cell, text, regular(13.5), p.text),
            None => widgets::text_in(ui, cell, "Not present", regular(13.0), p.faint),
        }
    };
    let cut_a = value(rect.left() + label_w, &r.a);
    let cut_b = value(rect.left() + label_w + col + 12.0, &r.b);
    if !same {
        let dot = pos2(rect.left() + label_w - 14.0, rect.center().y);
        ui.painter().circle_filled(dot, 3.0, p.warn);
    }
    if cut_a || cut_b {
        let id = ui.id().with(("cmp", &r.label, rect.top() as i32));
        let resp = ui.interact(rect, id, Sense::hover());
        let both = format!(
            "{}\n\nA: {}\nB: {}",
            r.label,
            r.a.as_deref().unwrap_or("not present"),
            r.b.as_deref().unwrap_or("not present")
        );
        resp.on_hover_text(both);
    }
}
