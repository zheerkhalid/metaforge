//! The edit and file-dates dialogs, and export.

use super::app::{App, Msg};
use super::theme::{pal, pal_ctx, semibold, Icon};
use super::widgets;
use crate::engine::{self, Output};
use crate::meta::{Edit, EditKey, GeoPoint, Kind, Meta};
use crate::settings::OutputMode;
use crate::util::DateTime;
use eframe::egui::{self, vec2, Align, Layout, RichText, Ui};
use std::path::PathBuf;

pub struct EditRow {
    pub key: EditKey,
    /// The current value; `None` when the files differ.
    pub original: Option<String>,
    pub value: String,
    pub remove: bool,
}

pub struct EditDlg {
    pub paths: Vec<PathBuf>,
    pub kind: Kind,
    pub rows: Vec<EditRow>,
    /// Change the originals instead of saving copies.
    pub replace: bool,
    pub error: Option<String>,
}

/// The value a file has for an editable key, as the editor shows it.
pub fn current_value(meta: &Meta, key: EditKey) -> String {
    if key == EditKey::Gps {
        return meta.gps.map(|g| g.display()).unwrap_or_default();
    }
    let v = meta
        .fields
        .iter()
        .find(|f| f.edit == Some(key) && !f.value.trim().is_empty())
        .map(|f| f.value.trim().to_string())
        .unwrap_or_default();
    if key == EditKey::DateTaken {
        if let Some(d) = DateTime::parse(&v) {
            return d.text();
        }
    }
    v
}

/// The keys every one of `metas` can edit, in the usual order.
pub fn common_keys(metas: &[&Meta]) -> Vec<EditKey> {
    EditKey::ALL
        .into_iter()
        .filter(|k| !metas.is_empty() && metas.iter().all(|m| m.editable.contains(k)))
        .collect()
}

/// Checks a value; `Err` holds the message for the user.
pub fn validate(key: EditKey, value: &str) -> Result<(), String> {
    let v = value.trim();
    match key {
        EditKey::DateTaken if DateTime::parse(v).is_none() => {
            Err(format!("\"{v}\" is not a date. Use YYYY-MM-DD HH:MM:SS."))
        }
        EditKey::Gps if GeoPoint::parse(v).is_none() => Err(format!(
            "\"{v}\" is not a position. Use latitude, longitude, e.g. 36.1901, 44.0091."
        )),
        EditKey::Year if !(v.len() == 4 && v.chars().all(|c| c.is_ascii_digit())) => {
            Err(format!("\"{v}\" is not a year."))
        }
        _ => Ok(()),
    }
}

/// The changes the dialog would make (only changed rows).
pub fn edits(rows: &[EditRow]) -> Result<Vec<Edit>, String> {
    let mut out = Vec::new();
    for r in rows {
        if r.remove {
            if r.original.as_deref() != Some("") {
                out.push(Edit::clear(r.key));
            }
            continue;
        }
        let v = r.value.trim();
        let changed = match &r.original {
            Some(o) => v != o.trim(),
            // The files differ: only a typed value changes them.
            None => !v.is_empty(),
        };
        if !changed {
            continue;
        }
        if v.is_empty() {
            out.push(Edit::clear(r.key));
        } else {
            validate(r.key, v)?;
            out.push(Edit::set(r.key, v));
        }
    }
    Ok(out)
}

pub struct DatesDlg {
    pub paths: Vec<PathBuf>,
    pub created: String,
    pub modified: String,
    pub accessed: String,
    /// The date in the metadata (single file).
    pub taken: Option<String>,
    pub error: Option<String>,
}

fn field_value(meta: &Meta, id: &str) -> String {
    meta.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.value.clone())
        .unwrap_or_default()
}

impl App {
    pub(super) fn open_edit(&mut self) {
        let metas = self.selected_metas();
        if metas.is_empty() {
            self.toast("Select a file first.", false);
            return;
        }
        let refs: Vec<&Meta> = metas.iter().collect();
        let keys = common_keys(&refs);
        if keys.is_empty() {
            self.toast(
                if metas.len() == 1 {
                    "MetaForge cannot edit this kind of file.".to_string()
                } else {
                    "These files have no editable field in common. Try fewer or similar files."
                        .to_string()
                },
                false,
            );
            return;
        }
        let rows = keys
            .into_iter()
            .map(|key| {
                let first = current_value(&metas[0], key);
                let same = metas.iter().all(|m| current_value(m, key) == first);
                EditRow {
                    key,
                    original: same.then(|| first.clone()),
                    value: if same { first } else { String::new() },
                    remove: false,
                }
            })
            .collect();
        let kind = if metas.iter().all(|m| m.kind() == metas[0].kind()) {
            metas[0].kind()
        } else {
            Kind::Other
        };
        self.edit = Some(Box::new(EditDlg {
            paths: metas.iter().map(|m| m.path.clone()).collect(),
            kind,
            rows,
            replace: self.settings.output_mode == OutputMode::Replace,
            error: None,
        }));
    }

    fn edit_output(&self, replace: bool) -> Output {
        if replace {
            Output::Replace
        } else {
            match self.settings.output() {
                Output::Replace => Output::Suffix(if self.settings.suffix.trim().is_empty() {
                    "_clean".into()
                } else {
                    self.settings.suffix.trim().to_string()
                }),
                other => other,
            }
        }
    }

    pub(super) fn edit_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.edit.take() else {
            return;
        };
        let p = pal_ctx(ctx);
        let max_h = (ctx.screen_rect().height() - 260.0).max(200.0);
        let copy_text = self.edit_output(false).describe();
        let mut close = false;
        let mut save = false;
        let modal = egui::Modal::new(egui::Id::new("edit-dialog"))
            .frame(widgets::modal_frame(&p))
            .show(ctx, |ui| {
                ui.set_width(600.0);
                let subtitle = if d.paths.len() == 1 {
                    d.paths[0]
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                } else {
                    format!(
                        "{} files: only the fields you change are written to all of them",
                        d.paths.len()
                    )
                };
                widgets::dialog_title(ui, "Edit details", &subtitle);
                egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        for row in &mut d.rows {
                            edit_row(ui, row, d.kind);
                        }
                    });
                ui.add_space(6.0);
                ui.label(RichText::new("Save").font(semibold(14.0)));
                widgets::row(ui, 28.0, |ui| {
                    if widgets::radio(ui, !d.replace, &copy_text) {
                        d.replace = false;
                    }
                });
                widgets::row(ui, 28.0, |ui| {
                    if widgets::radio(ui, d.replace, "Change the original file") {
                        d.replace = true;
                    }
                });
                if let Some(e) = &d.error {
                    ui.label(RichText::new(e).size(12.5).color(p.danger));
                }
                ui.add_space(6.0);
                let changes = edits(&d.rows);
                let n = changes.as_ref().map(|c| c.len()).unwrap_or(0);
                widgets::button_row(ui, |ui| {
                    let label = match n {
                        0 => "Save".to_string(),
                        1 => "Save 1 change".to_string(),
                        n => format!("Save {n} changes"),
                    };
                    if widgets::primary_button(ui, &Icon::SAVE.with(&label), n > 0).clicked() {
                        save = true;
                    }
                    if widgets::ghost_button(ui, "Cancel").clicked() {
                        close = true;
                    }
                    if let Err(e) = &changes {
                        ui.label(RichText::new(e).size(12.5).color(p.danger));
                    }
                });
            });
        if modal.should_close() {
            close = true;
        }
        if save {
            match edits(&d.rows) {
                Err(e) => d.error = Some(e),
                Ok(changes) => {
                    close = true;
                    if self.demo {
                        self.demo_toast();
                    } else {
                        let output = self.edit_output(d.replace);
                        let keep = self.settings.keep_file_dates;
                        let paths = d.paths.clone();
                        let replace = d.replace;
                        self.reading += 1;
                        self.spawn(move |send| {
                            let mut ok = 0;
                            for p in &paths {
                                match engine::edit_file(p, &changes, &output, keep) {
                                    Ok(dst) => {
                                        ok += 1;
                                        let meta = engine::inspect(&dst);
                                        if replace {
                                            send(Msg::Replaced {
                                                old: p.clone(),
                                                meta,
                                            });
                                        } else {
                                            send(Msg::Inspected(meta));
                                        }
                                    }
                                    Err(e) => send(Msg::Toast(
                                        format!(
                                            "{}: {e}",
                                            p.file_name().unwrap_or_default().to_string_lossy()
                                        ),
                                        true,
                                    )),
                                }
                            }
                            if ok > 0 {
                                send(Msg::Toast(
                                    if replace {
                                        format!("Saved the changes in {ok} file(s).")
                                    } else {
                                        format!("Saved {ok} edited cop(ies); they are in the list.")
                                            .replace(
                                                "cop(ies)",
                                                if ok == 1 { "copy" } else { "copies" },
                                            )
                                    },
                                    false,
                                ));
                            }
                            send(Msg::BatchDone {
                                paths: Vec::new(),
                                clean: false,
                            });
                        });
                    }
                }
            }
        }
        if !close {
            self.edit = Some(d);
        }
    }

    pub(super) fn open_dates(&mut self) {
        let metas = self.selected_metas();
        if metas.is_empty() {
            self.toast("Select a file first.", false);
            return;
        }
        let single = metas.len() == 1;
        let m = &metas[0];
        self.dates = Some(Box::new(DatesDlg {
            paths: metas.iter().map(|m| m.path.clone()).collect(),
            created: if single {
                field_value(m, "file:created")
            } else {
                String::new()
            },
            modified: if single {
                field_value(m, "file:modified")
            } else {
                String::new()
            },
            accessed: if single {
                field_value(m, "file:accessed")
            } else {
                String::new()
            },
            taken: if single {
                m.taken
                    .as_deref()
                    .and_then(DateTime::parse)
                    .map(|d| d.text())
            } else {
                None
            },
            error: None,
        }));
    }

    pub(super) fn dates_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.dates.take() else {
            return;
        };
        let p = pal_ctx(ctx);
        let mut close = false;
        let mut save = false;
        let mut each_taken = false;
        let modal = egui::Modal::new(egui::Id::new("dates-dialog"))
            .frame(widgets::modal_frame(&p))
            .show(ctx, |ui| {
                ui.set_width(540.0);
                let single = d.paths.len() == 1;
                let subtitle = if single {
                    d.paths[0]
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                } else {
                    format!("{} files", d.paths.len())
                };
                widgets::dialog_title(ui, "File dates", &subtitle);
                widgets::para(
                    ui,
                    "These are the dates Windows keeps for the file itself (shown in Explorer), not the dates inside it. Format: YYYY-MM-DD HH:MM:SS.",
                );
                ui.add_space(6.0);
                let hint = if single { "YYYY-MM-DD HH:MM:SS" } else { "Leave empty to keep each file's date" };
                for (label, value) in [
                    ("Created", &mut d.created),
                    ("Modified", &mut d.modified),
                    ("Last opened", &mut d.accessed),
                ] {
                    widgets::row(ui, 36.0, |ui| {
                        ui.allocate_ui_with_layout(
                            vec2(110.0, 30.0),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.set_min_width(110.0);
                                ui.label(RichText::new(label).color(p.muted));
                            },
                        );
                        let w = ui.available_width();
                        widgets::text_field(ui, value, hint, w);
                    });
                }
                ui.add_space(4.0);
                if let Some(t) = d.taken.clone() {
                    let label = Icon::CLOCK.with(&format!("Use the date taken ({t})"));
                    if widgets::ghost_button(ui, &label)
                        .on_hover_text("Sets Created and Modified to the date stored in the file")
                        .clicked()
                    {
                        d.created = t.clone();
                        d.modified = t;
                    }
                } else if !single
                    && widgets::ghost_button(ui, &Icon::CLOCK.with("Set each file to its own date taken"))
                        .on_hover_text("Created and Modified become the date stored inside each file")
                        .clicked()
                    {
                        each_taken = true;
                    }
                if let Some(e) = &d.error {
                    ui.label(RichText::new(e).size(12.5).color(p.danger));
                }
                ui.add_space(8.0);
                widgets::button_row(ui, |ui| {
                    if widgets::primary_button(ui, &Icon::SAVE.with("Save dates"), true).clicked() {
                        save = true;
                    }
                    if widgets::ghost_button(ui, "Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if modal.should_close() {
            close = true;
        }
        if each_taken {
            close = true;
            if self.demo {
                self.demo_toast();
            } else {
                let metas: Vec<Meta> = d
                    .paths
                    .iter()
                    .filter_map(|p| self.index_of(p))
                    .map(|i| self.items[i].meta.clone())
                    .collect();
                self.reading += 1;
                self.spawn(move |send| {
                    let mut ok = 0;
                    for m in &metas {
                        match engine::copy_date_to_file(m) {
                            Ok(_) => ok += 1,
                            Err(e) => send(Msg::Toast(format!("{}: {e}", m.name()), true)),
                        }
                        send(Msg::Replaced {
                            old: m.path.clone(),
                            meta: engine::inspect(&m.path),
                        });
                    }
                    send(Msg::Toast(format!("Dates set for {ok} file(s)."), false));
                    send(Msg::BatchDone {
                        paths: Vec::new(),
                        clean: false,
                    });
                });
            }
        }
        if save {
            let parse_ok = [&d.created, &d.modified, &d.accessed]
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .find(|s| DateTime::parse(s).is_none())
                .map(|bad| format!("\"{bad}\" is not a date (use YYYY-MM-DD HH:MM:SS)."));
            if let Some(e) = parse_ok {
                d.error = Some(e);
            } else {
                close = true;
                if self.demo {
                    self.demo_toast();
                } else {
                    let (c, m, a) = (d.created.clone(), d.modified.clone(), d.accessed.clone());
                    let paths = d.paths.clone();
                    self.reading += 1;
                    self.spawn(move |send| {
                        let opt = |s: &str| {
                            let t = s.trim().to_string();
                            (!t.is_empty()).then_some(t)
                        };
                        let (c, m, a) = (opt(&c), opt(&m), opt(&a));
                        for p in &paths {
                            if let Err(e) =
                                engine::set_file_dates(p, c.as_deref(), m.as_deref(), a.as_deref())
                            {
                                send(Msg::Toast(e, true));
                            }
                            send(Msg::Replaced {
                                old: p.clone(),
                                meta: engine::inspect(p),
                            });
                        }
                        send(Msg::Toast("File dates saved.".into(), false));
                        send(Msg::BatchDone {
                            paths: Vec::new(),
                            clean: false,
                        });
                    });
                }
            }
        }
        if !close {
            self.dates = Some(d);
        }
    }

    /// Saves the details of the selected files as CSV or JSON.
    pub(super) fn export(&mut self, json: bool) {
        let metas = self.selected_metas();
        if metas.is_empty() {
            self.toast("Select a file first.", false);
            return;
        }
        if self.demo {
            self.demo_toast();
            return;
        }
        let (name, ext, label) = if json {
            ("MetaForge export.json", "json", "JSON")
        } else {
            ("MetaForge export.csv", "csv", "CSV (Excel)")
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Export the details")
            .set_file_name(name)
            .add_filter(label, &[ext])
            .save_file()
        else {
            return;
        };
        let text = if json {
            engine::to_json(&metas)
        } else {
            engine::to_csv(&metas)
        };
        match std::fs::write(&path, text) {
            Ok(()) => self.toast(
                format!(
                    "Exported {} to {}.",
                    if metas.len() == 1 {
                        "1 file".to_string()
                    } else {
                        format!("{} files", metas.len())
                    },
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                false,
            ),
            Err(e) => self.toast(format!("The export could not be saved: {e}"), true),
        }
    }
}

fn edit_row(ui: &mut Ui, row: &mut EditRow, kind: Kind) {
    let p = pal(ui);
    let label = row.key.label(kind);
    let mixed = row.original.is_none();
    let hint = if mixed {
        "Different in each file; leave empty to keep".to_string()
    } else if !row.key.hint().is_empty() {
        row.key.hint().to_string()
    } else {
        "Empty".to_string()
    };
    widgets::row(ui, 36.0, |ui| {
        ui.allocate_ui_with_layout(
            vec2(118.0, 30.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(118.0);
                ui.label(RichText::new(label).color(if row.remove { p.faint } else { p.muted }));
            },
        );
        let toggle_w = if row.key == EditKey::Gps { 150.0 } else { 96.0 };
        let w = (ui.available_width() - toggle_w - 8.0).max(120.0);
        if row.remove {
            ui.allocate_ui_with_layout(vec2(w, 30.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_width(w);
                ui.label(RichText::new("Will be removed").italics().color(p.danger));
            });
        } else {
            widgets::text_field(ui, &mut row.value, &hint, w);
        }
        if row.key == EditKey::Gps {
            let text = if row.remove {
                "Keep location"
            } else {
                "Remove location"
            };
            if widgets::ghost_button(ui, &Icon::MAP_PIN.with(text)).clicked() {
                row.remove = !row.remove;
            }
        } else {
            widgets::checkbox(ui, &mut row.remove, "Remove");
        }
    });
    if !row.remove && !row.value.trim().is_empty() {
        if let Err(e) = validate(row.key, &row.value) {
            ui.horizontal(|ui| {
                ui.add_space(124.0);
                ui.label(RichText::new(e).size(12.0).color(p.danger));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    #[test]
    fn edits_only_what_changed() {
        let files = demo::files();
        let photo = files.iter().find(|m| m.name() == "IMG_2041.jpg").unwrap();
        assert_eq!(current_value(photo, EditKey::Author), "Alex Rivera");
        assert_eq!(
            current_value(photo, EditKey::DateTaken),
            "2025-07-14 18:42:07"
        );
        assert!(current_value(photo, EditKey::Gps).starts_with("38.712040"));
        let mut rows: Vec<EditRow> = photo
            .editable
            .iter()
            .map(|k| EditRow {
                key: *k,
                original: Some(current_value(photo, *k)),
                value: current_value(photo, *k),
                remove: false,
            })
            .collect();
        assert!(edits(&rows).unwrap().is_empty());
        rows[0].value = "Sunset".into();
        rows.iter_mut()
            .find(|r| r.key == EditKey::Gps)
            .unwrap()
            .remove = true;
        let e = edits(&rows).unwrap();
        assert_eq!(
            e,
            vec![
                Edit::set(EditKey::Title, "Sunset"),
                Edit::clear(EditKey::Gps)
            ]
        );
        rows.iter_mut()
            .find(|r| r.key == EditKey::DateTaken)
            .unwrap()
            .value = "yesterday".into();
        assert!(edits(&rows).is_err());
        // Several files: only keys all of them have; differing values stay unless typed.
        let refs: Vec<&Meta> = files.iter().filter(|m| m.kind() == Kind::Image).collect();
        let keys = common_keys(&refs);
        assert!(keys.contains(&EditKey::Title) && !keys.contains(&EditKey::Album));
        let mixed = EditRow {
            key: EditKey::Title,
            original: None,
            value: String::new(),
            remove: false,
        };
        assert!(edits(&[mixed]).unwrap().is_empty());
    }
}
