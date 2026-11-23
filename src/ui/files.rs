//! The Files view: the drop zone, the list with its filters and actions, and
//! the details panel of the selected file.

use super::app::{App, Tab};
use super::bidi;
use super::theme::{pal, pal_ctx, regular, semibold, Icon, Palette};
use super::widgets;
use crate::meta::{Category, Field, Flag, Kind, Meta};
use crate::{sys, util};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, Frame, Key, Layout, Margin, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui,
};

/// The filter chips above the list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListFilter {
    All,
    Location,
    Names,
    NotClean,
}

impl ListFilter {
    pub const ALL: [ListFilter; 4] = [
        ListFilter::All,
        ListFilter::Location,
        ListFilter::Names,
        ListFilter::NotClean,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ListFilter::All => "All",
            ListFilter::Location => "With location",
            ListFilter::Names => "With names",
            ListFilter::NotClean => "Not clean",
        }
    }

    pub fn accepts(self, meta: &Meta) -> bool {
        match self {
            ListFilter::All => true,
            ListFilter::Location => meta.flags().contains(&Flag::Location),
            ListFilter::Names => meta.flags().contains(&Flag::Names),
            ListFilter::NotClean => !meta.is_clean(),
        }
    }
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.to_lowercase().contains(needle_lower)
}

fn field_matches(f: &Field, q: &str) -> bool {
    contains_ci(&f.label, q) || contains_ci(&f.value, q) || contains_ci(f.category.title(), q)
}

/// Whether a file is shown in the list for a filter chip and search text
/// (the search looks at the name and at every field).
pub fn matches(meta: &Meta, filter: ListFilter, query: &str) -> bool {
    if !filter.accepts(meta) {
        return false;
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    contains_ci(&meta.name(), &q) || meta.fields.iter().any(|f| field_matches(f, &q))
}

/// The fields shown in the details panel for a search: the matching ones,
/// or all of them when only the name matched (or there is no search).
pub fn shown_fields<'a>(meta: &'a Meta, query: &str) -> Vec<&'a Field> {
    let q = query.trim().to_lowercase();
    if !q.is_empty() {
        let hits: Vec<&Field> = meta
            .fields
            .iter()
            .filter(|f| field_matches(f, &q))
            .collect();
        if !hits.is_empty() {
            return hits;
        }
    }
    meta.fields.iter().collect()
}

fn noun(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "photo",
        Kind::Document => "document",
        Kind::Audio => "audio file",
        Kind::Video => "video",
        Kind::Other => "file",
    }
}

/// "a, b and c"
fn join_and(parts: &[&str]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The plain-language summary of a file, e.g. "This photo shows where it was
/// taken and the camera's serial number."
pub fn summary(meta: &Meta) -> String {
    let kind = meta.kind();
    if let Some(e) = &meta.error {
        return format!("MetaForge could not read this {}: {e}", noun(kind));
    }
    let flags = meta.flags();
    if flags.is_empty() {
        return format!("Nothing personal was found in this {}.", noun(kind));
    }
    let downloaded = meta
        .fields
        .iter()
        .any(|f| f.id.starts_with("ads:Zone.Identifier") && f.flag.is_some());
    let parts: Vec<&str> = flags
        .iter()
        .map(|f| match (f, kind) {
            (Flag::Location, Kind::Image) => "where it was taken",
            (Flag::Location, Kind::Video) => "where it was recorded",
            (Flag::Location, _) => "a location",
            (Flag::Names, Kind::Image) => "who took it",
            (Flag::Names, Kind::Document) => "who wrote and edited it",
            (Flag::Names, Kind::Audio) => "people's names",
            (Flag::Names, _) => "people's names",
            (Flag::Serial, Kind::Image | Kind::Video) => "the camera's serial number",
            (Flag::Serial, _) => "ids that can link it to other files",
            (Flag::Comments, Kind::Document) => "comments or tracked changes",
            (Flag::Comments, _) => "a title, comments or a description",
            (Flag::Hidden, _) if downloaded => "where it was downloaded from",
            (Flag::Hidden, Kind::Document) => "hidden content",
            (Flag::Hidden, Kind::Audio) => "an embedded picture",
            (Flag::Hidden, _) => "hidden extras such as a thumbnail",
        })
        .collect();
    format!("This {} shows {}.", noun(kind), join_and(&parts))
}

fn category_icon(c: Category) -> Icon {
    match c {
        Category::Location => Icon::MAP_PIN,
        Category::People => Icon::CONTACT,
        Category::Device => Icon::CAMERA,
        Category::Dates => Icon::CALENDAR,
        Category::Software => Icon::CODE,
        Category::Description => Icon::TAG,
        Category::Hidden => Icon::HIDE,
        Category::Technical => Icon::PAGE,
        Category::Windows => Icon::MONITOR,
    }
}

/// Paints the badges of a file from `x` along the centre line of `rect`.
fn paint_flags(ui: &Ui, rect: Rect, meta: &Meta, p: &Palette) {
    let mut x = rect.left();
    let at = |x: f32| Rect::from_min_max(pos2(x, rect.top()), rect.max);
    if meta.error.is_some() {
        widgets::paint_badge(ui, at(x), "Not readable", p.warn);
        return;
    }
    let flags = meta.flags();
    if flags.is_empty() {
        widgets::paint_badge(ui, at(x), &Icon::CHECK.with("Clean"), p.ok);
        return;
    }
    for f in flags {
        x += widgets::paint_badge(ui, at(x), f.label(), p.flag(f)) + 6.0;
    }
}

impl App {
    pub(super) fn files_view(&mut self, ctx: &egui::Context) {
        let p = pal_ctx(ctx);
        if self.items.is_empty() {
            egui::CentralPanel::default()
                .frame(
                    Frame::new()
                        .fill(p.bg)
                        .inner_margin(Margin::symmetric(24, 18)),
                )
                .show(ctx, |ui| {
                    if self.reading > 0 {
                        widgets::busy_state(
                            ui,
                            "Reading files…",
                            "Only this PC reads them; nothing is uploaded.",
                        );
                    } else {
                        self.drop_zone(ui);
                    }
                });
            return;
        }
        egui::SidePanel::right("details")
            .exact_width(440.0)
            .resizable(false)
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin {
                        left: 18,
                        right: 18,
                        top: 18,
                        bottom: 12,
                    })
                    .stroke(Stroke::NONE),
            )
            .show(ctx, |ui| {
                let full = ui.max_rect();
                ui.painter().vline(
                    full.left() - 18.0,
                    full.y_range().expand(18.0),
                    Stroke::new(1.0_f32, p.border),
                );
                self.details(ui);
            });
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(p.bg)
                    .inner_margin(Margin::symmetric(24, 18)),
            )
            .show(ctx, |ui| self.list_area(ui));
    }

    // ----------------------------------------------------------------------------------
    // Empty state
    // ----------------------------------------------------------------------------------

    fn drop_zone(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        App::page_header(
            ui,
            "Files",
            "See what your files reveal before you share them.",
            |_| {},
        );
        let avail = ui.available_rect_before_wrap();
        let zone = Rect::from_min_max(avail.min, avail.max - vec2(0.0, 4.0));
        let resp = ui.allocate_rect(zone, Sense::click());
        let hovered = resp.hovered();
        ui.painter().rect(
            zone,
            CornerRadius::same(18),
            if hovered { p.hover } else { p.card },
            Stroke::NONE,
            StrokeKind::Inside,
        );
        // Dashed outline.
        let r = zone.shrink(10.0);
        let dash = Stroke::new(
            1.5_f32,
            if hovered {
                p.accent
            } else {
                p.faint.gamma_multiply(0.7)
            },
        );
        let painter = ui.painter();
        let mut segs = Vec::new();
        let step = 14.0;
        let mut x = r.left() + 16.0;
        while x < r.right() - 16.0 {
            segs.push([
                pos2(x, r.top()),
                pos2((x + 7.0).min(r.right() - 16.0), r.top()),
            ]);
            segs.push([
                pos2(x, r.bottom()),
                pos2((x + 7.0).min(r.right() - 16.0), r.bottom()),
            ]);
            x += step;
        }
        let mut y = r.top() + 16.0;
        while y < r.bottom() - 16.0 {
            segs.push([
                pos2(r.left(), y),
                pos2(r.left(), (y + 7.0).min(r.bottom() - 16.0)),
            ]);
            segs.push([
                pos2(r.right(), y),
                pos2(r.right(), (y + 7.0).min(r.bottom() - 16.0)),
            ]);
            y += step;
        }
        for s in segs {
            painter.line_segment(s, dash);
        }

        let mut content = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(zone.shrink(40.0))
                .layout(Layout::top_down(Align::Center)),
        );
        let ui = &mut content;
        ui.add_space((zone.height() * 0.5 - 220.0).max(10.0));
        let (icon_rect, _) = ui.allocate_exact_size(vec2(84.0, 84.0), Sense::hover());
        ui.painter()
            .circle_filled(icon_rect.center(), 42.0, p.accent.gamma_multiply(0.14));
        ui.painter().text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            Icon::UPLOAD.glyph(),
            regular(34.0),
            p.accent,
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new("Drop photos, documents, music or videos here").font(semibold(21.0)),
        );
        ui.add_space(2.0);
        ui.scope(|ui| {
            ui.set_max_width(560.0);
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "MetaForge shows the hidden information inside them, such as where a photo was taken or who wrote a document, and removes it with one click.",
                    )
                    .color(p.muted),
                )
                .wrap(),
            );
        });
        ui.add_space(14.0);
        let mut open_files = false;
        let mut open_folder = false;
        ui.allocate_ui_with_layout(
            vec2(320.0, 36.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                open_files =
                    widgets::primary_button(ui, &Icon::ADD.with("Open files"), true).clicked();
                open_folder =
                    widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Open folder")).clicked();
            },
        );
        ui.add_space(18.0);
        ui.allocate_ui_with_layout(
            vec2(480.0, 24.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("Finds").color(p.faint));
                for f in [
                    Flag::Location,
                    Flag::Names,
                    Flag::Serial,
                    Flag::Comments,
                    Flag::Hidden,
                ] {
                    widgets::badge(ui, f.long_label(), p.flag(f));
                }
            },
        );
        ui.add_space(18.0);
        ui.label(
            RichText::new(Icon::LOCK.with("Nothing is uploaded. Everything stays on this PC."))
                .color(p.ok),
        );
        if open_files || (resp.clicked() && !open_folder) {
            self.pick_files();
        } else if open_folder {
            self.pick_folder();
        }
    }

    // ----------------------------------------------------------------------------------
    // The list
    // ----------------------------------------------------------------------------------

    fn list_area(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let risky = self
            .items
            .iter()
            .filter(|i| !i.meta.flags().is_empty())
            .count();
        let n = self.items.len();
        let subtitle = format!(
            "{n} {} · {}",
            if n == 1 { "file" } else { "files" },
            match risky {
                0 => "nothing personal found".to_string(),
                1 => "1 reveals personal information".to_string(),
                r => format!("{r} reveal personal information"),
            }
        );
        let mut add_files = false;
        let mut add_folder = false;
        let mut clear = false;
        App::page_header(ui, "Files", &subtitle, |ui| {
            if widgets::icon_button(ui, Icon::CLOSE, "Clear the list (files are not touched)")
                .clicked()
            {
                clear = true;
            }
            add_folder =
                widgets::ghost_button(ui, &Icon::FOLDER_OPEN.with("Open folder")).clicked();
            add_files = widgets::ghost_button(ui, &Icon::ADD.with("Open files")).clicked();
        });
        if add_files {
            self.pick_files();
        }
        if add_folder {
            self.pick_folder();
        }
        if clear {
            self.items.clear();
            self.current = None;
            self.anchor = None;
            self.rename_cache = None;
            return;
        }

        // Filter chips and search.
        widgets::row(ui, 34.0, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for f in ListFilter::ALL {
                let count = self.items.iter().filter(|i| f.accepts(&i.meta)).count();
                if widgets::chip(ui, f.label(), Some(count), self.filter == f).clicked() {
                    self.filter = f;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let w = ui.available_width().clamp(120.0, 240.0);
                let resp = Frame::new()
                    .fill(p.card)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(15))
                    .inner_margin(Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(
                                RichText::new(Icon::SEARCH.glyph())
                                    .color(p.faint)
                                    .size(13.0),
                            );
                            let tw = ui.available_width();
                            bidi::show_edit(ui, &mut self.search, 13.5, p.text, |e| {
                                e.hint_text(RichText::new("Search files and fields").color(p.faint))
                                    .frame(false)
                                    .desired_width(tw)
                                    .font(regular(13.5))
                            })
                            .response
                        })
                        .inner
                    })
                    .inner;
                if self.focus_search {
                    self.focus_search = false;
                    resp.request_focus();
                }
            });
        });
        ui.add_space(8.0);

        // Select-all line.
        let visible = self.visible();
        let ticked = self.items.iter().filter(|i| i.selected).count();
        widgets::row(ui, 26.0, |ui| {
            let all = !visible.is_empty() && visible.iter().all(|i| self.items[*i].selected);
            let some = visible.iter().any(|i| self.items[*i].selected);
            let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 24.0), Sense::click());
            widgets::paint_checkbox(
                ui,
                Rect::from_min_size(rect.min + vec2(8.0, 0.0), vec2(20.0, 24.0)),
                if all {
                    Some(true)
                } else if some {
                    None
                } else {
                    Some(false)
                },
                resp.hovered(),
            );
            if resp.on_hover_text("Tick all (Ctrl+A)").clicked() {
                for i in &visible {
                    self.items[*i].selected = !all;
                }
            }
            let text = if ticked > 0 {
                format!("{ticked} of {} ticked", self.items.len())
            } else if visible.len() < self.items.len() {
                format!("{} of {} shown", visible.len(), self.items.len())
            } else {
                "Tick files to work on several at once".to_string()
            };
            ui.add_space(4.0);
            ui.label(RichText::new(text).size(12.5).color(p.muted));
        });
        ui.add_space(2.0);

        // Rows, then the action bar at the bottom.
        let bar_h = 52.0;
        let list_h = (ui.available_height() - bar_h).max(80.0);
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), list_h),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_min_height(list_h);
                if visible.is_empty() {
                    widgets::empty_state(
                        ui,
                        Icon::FILTER,
                        p.muted,
                        "No files match",
                        "Try another filter chip or clear the search box.",
                    );
                    return;
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(list_h)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for i in visible.iter().copied() {
                            self.list_row(ui, i);
                        }
                    });
            },
        );
        self.action_bar(ui);
    }

    fn list_row(&mut self, ui: &mut Ui, i: usize) {
        let p = pal(ui);
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 58.0), Sense::click());
        if self.scroll_to_current && self.current == Some(i) {
            self.scroll_to_current = false;
            ui.scroll_to_rect(rect, Some(Align::Center));
        }
        let current = self.current == Some(i);
        let item = &self.items[i];
        let fill = if current {
            p.selected
        } else if resp.hovered() {
            p.hover
        } else {
            p.card
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(10),
            fill,
            Stroke::new(
                1.0_f32,
                if current {
                    p.accent.gamma_multiply(0.55)
                } else {
                    p.border
                },
            ),
            StrokeKind::Inside,
        );
        let check_rect = Rect::from_min_size(rect.min + vec2(6.0, 0.0), vec2(30.0, rect.height()));
        let pointer = ui.ctx().pointer_interact_pos();
        let over_check = pointer.is_some_and(|pos| check_rect.contains(pos));
        widgets::paint_checkbox(
            ui,
            check_rect,
            Some(item.selected),
            resp.hovered() && over_check,
        );
        let meta = &item.meta;
        let tile = Rect::from_min_size(rect.min + vec2(42.0, 11.0), vec2(36.0, 36.0));
        let tint = if meta.is_clean() {
            p.ok
        } else {
            meta.flags().first().map(|f| p.flag(*f)).unwrap_or(p.slate)
        };
        widgets::paint_kind_tile(ui, tile, meta.kind(), tint);
        let x = tile.right() + 12.0;
        let name_rect = Rect::from_min_max(
            pos2(x, rect.top() + 8.0),
            pos2(rect.right() - 12.0, rect.top() + 28.0),
        );
        widgets::text_in(ui, name_rect, &meta.name(), semibold(14.0), p.text);
        let info = format!("{} · {}", meta.format.short(), util::human_size(meta.size));
        let info_galley = ui.painter().layout_no_wrap(info, regular(12.5), p.muted);
        let info_w = info_galley.size().x;
        let line2 = rect.top() + 41.0;
        ui.painter().galley(
            pos2(x, line2 - info_galley.size().y / 2.0),
            info_galley,
            p.muted,
        );
        let badges = Rect::from_min_max(
            pos2(x + info_w + 12.0, line2 - 10.0),
            pos2(rect.right() - 8.0, line2 + 10.0),
        );
        paint_flags(ui, badges, meta, &p);

        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        if resp.clicked() {
            if over_check || ctrl {
                self.items[i].selected = !self.items[i].selected;
                self.anchor = Some(i);
            } else if shift {
                let visible = self.visible();
                let a = self.anchor.unwrap_or(i);
                let pa = visible.iter().position(|x| *x == a).unwrap_or(0);
                let pb = visible.iter().position(|x| *x == i).unwrap_or(0);
                for v in &visible[pa.min(pb)..=pa.max(pb)] {
                    self.items[*v].selected = true;
                }
            } else {
                self.current = Some(i);
                self.anchor = Some(i);
            }
        }
        if resp.double_clicked() && !over_check {
            let path = self.items[i].meta.path.clone();
            if self.demo {
                self.toast("Demo: the invented files are not on this PC.", false);
            } else if let Err(e) = sys::open(&path.to_string_lossy()) {
                self.toast(e, true);
            }
        }
        resp.context_menu(|ui| {
            self.current = Some(i);
            ui.set_min_width(200.0);
            if ui.button(Icon::BROOM.with("Clean for sharing")).clicked() {
                self.open_clean();
                ui.close_menu();
            }
            if ui.button(Icon::EDIT.with("Edit details")).clicked() {
                self.open_edit();
                ui.close_menu();
            }
            if ui.button(Icon::CALENDAR.with("File dates")).clicked() {
                self.open_dates();
                ui.close_menu();
            }
            ui.separator();
            if ui.button(Icon::COPY.with("Copy path")).clicked() {
                ui.ctx()
                    .copy_text(self.items[i].meta.path.to_string_lossy().into_owned());
                ui.close_menu();
            }
            if ui
                .button(Icon::FOLDER_OPEN.with("Show in Explorer"))
                .clicked()
            {
                let path = self.items[i].meta.path.clone();
                self.show_in_explorer(&path);
                ui.close_menu();
            }
            if ui.button(Icon::DELETE.with("Remove from list")).clicked() {
                for (k, item) in self.items.iter_mut().enumerate() {
                    item.selected = k == i;
                }
                self.remove_selection();
                ui.close_menu();
            }
        });
    }

    /// Up / Down move through the list; Space ticks.
    pub(super) fn list_keys(&mut self, ctx: &egui::Context) {
        let (up, down, space) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Space),
            )
        });
        if !(up || down || space) {
            return;
        }
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let pos = self
            .current
            .and_then(|c| visible.iter().position(|v| *v == c));
        if space {
            if let Some(c) = self.current {
                self.items[c].selected = !self.items[c].selected;
            }
            return;
        }
        let next = match pos {
            None => 0,
            Some(p) if up => p.saturating_sub(1),
            Some(p) => (p + 1).min(visible.len() - 1),
        };
        self.current = Some(visible[next]);
        self.anchor = self.current;
        self.scroll_to_current = true;
    }

    fn action_bar(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let sel = self.selection();
        let n = sel.len();
        let any = n > 0;
        ui.add_space(8.0);
        let mut clean = false;
        let mut edit = false;
        let mut dates = false;
        let mut export: Option<bool> = None;
        let mut compare = false;
        let mut rename = false;
        let mut explorer = false;
        let mut remove = false;
        widgets::row(ui, 40.0, |ui| {
            let label = if n > 1 {
                format!("Clean {n} files")
            } else {
                "Clean for sharing".to_string()
            };
            clean = widgets::primary_button(ui, &Icon::BROOM.with(&label), any)
                .on_hover_text("Remove the hidden information (you choose what, and where the cleaned files go)")
                .clicked();
            edit = widgets::ghost_button_enabled(ui, &Icon::EDIT.with("Edit"), any)
                .on_hover_text("Change the title, author, date and more")
                .clicked();
            dates = widgets::ghost_button_enabled(ui, &Icon::CALENDAR.with("Dates"), any)
                .on_hover_text("The file's created and modified dates in Windows")
                .clicked();
            ui.add_enabled_ui(any, |ui| {
                let style = ui.style_mut();
                style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
                style.spacing.button_padding = vec2(12.0, 8.0);
                ui.menu_button(Icon::EXPORT.with("Export"), |ui| {
                    ui.set_min_width(220.0);
                    if ui.button("CSV table (opens in Excel)…").clicked() {
                        export = Some(false);
                        ui.close_menu();
                    }
                    if ui.button("JSON…").clicked() {
                        export = Some(true);
                        ui.close_menu();
                    }
                })
                .response
                .on_hover_text("Save the details of the selected files as a table");
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                remove = ui
                    .add_enabled_ui(any, |ui| {
                        widgets::icon_button(ui, Icon::DELETE, "Remove from the list (Delete)")
                    })
                    .inner
                    .clicked();
                explorer = ui
                    .add_enabled_ui(any, |ui| {
                        widgets::icon_button(ui, Icon::FOLDER_OPEN, "Show in Explorer")
                    })
                    .inner
                    .clicked();
                rename = ui
                    .add_enabled_ui(any, |ui| {
                        widgets::icon_button(ui, Icon::RENAME, "Rename from metadata")
                    })
                    .inner
                    .clicked();
                compare = ui
                    .add_enabled_ui(n == 2, |ui| {
                        widgets::icon_button(ui, Icon::COMPARE, "Compare (tick exactly two files)")
                    })
                    .inner
                    .clicked();
            });
        });
        if clean {
            self.open_clean();
        }
        if edit {
            self.open_edit();
        }
        if dates {
            self.open_dates();
        }
        if let Some(json) = export {
            self.export(json);
        }
        if compare {
            self.compare_from_selection(true);
        }
        if rename {
            self.tab = Tab::Rename;
        }
        if explorer {
            if let Some(i) = sel.first() {
                let path = self.items[*i].meta.path.clone();
                self.show_in_explorer(&path);
            }
        }
        if remove {
            self.remove_selection();
        }
    }

    // ----------------------------------------------------------------------------------
    // The details panel
    // ----------------------------------------------------------------------------------

    fn details(&mut self, ui: &mut Ui) {
        let p = pal(ui);
        let Some(i) = self.current.filter(|i| *i < self.items.len()) else {
            widgets::empty_state(
                ui,
                Icon::INFO,
                p.muted,
                "Select a file",
                "Click a file in the list to see what it reveals.",
            );
            return;
        };
        let meta = self.items[i].meta.clone();
        self.want_thumb(&meta);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt(("details", meta.path.clone()))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                self.details_header(ui, &meta);
                self.summary_card(ui, &meta);
                if let Some(g) = meta.gps {
                    gps_card(ui, g);
                }
                if let Some(e) = &meta.error {
                    widgets::note_card(ui, Icon::WARNING, p.warn, e);
                }
                for n in &meta.notes {
                    widgets::note_card(ui, Icon::INFO, p.info, n);
                }
                let fields = shown_fields(&meta, &self.search);
                let searching = !self.search.trim().is_empty() && fields.len() < meta.fields.len();
                if searching {
                    ui.label(
                        RichText::new(format!(
                            "{} fields match \"{}\"",
                            fields.len(),
                            self.search.trim()
                        ))
                        .size(12.5)
                        .color(p.muted),
                    );
                }
                for cat in Category::ALL {
                    let in_cat: Vec<&Field> = fields
                        .iter()
                        .copied()
                        .filter(|f| f.category == cat)
                        .collect();
                    if in_cat.is_empty() {
                        continue;
                    }
                    self.field_group(ui, cat, &in_cat);
                }
                ui.add_space(6.0);
            });
    }

    fn details_header(&mut self, ui: &mut Ui, meta: &Meta) {
        let p = pal(ui);
        let thumb = self.thumbs.get(&meta.path).cloned().flatten();
        if let Some(tex) = thumb {
            let size = tex.size_vec2();
            let w = ui.available_width();
            let h = (w * size.y / size.x).min(220.0);
            let scale = (w / size.x).min(h / size.y);
            let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            ui.painter()
                .rect_filled(rect, CornerRadius::same(12), p.card);
            let img = Rect::from_center_size(rect.center(), size * scale);
            egui::Image::new(&tex)
                .corner_radius(CornerRadius::same(10))
                .paint_at(ui, img);
        } else if self.demo && matches!(meta.kind(), Kind::Image | Kind::Video) {
            let w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(w, 140.0), Sense::hover());
            paint_placeholder(ui, rect, meta, &p);
        }
        ui.horizontal(|ui| {
            let (tile, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            widgets::paint_kind_tile(ui, tile, meta.kind(), p.accent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                bidi::label(ui, &meta.name(), 17.0, true, p.text);
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        meta.format.name(),
                        util::human_size(meta.size)
                    ))
                    .size(12.5)
                    .color(p.muted),
                );
            });
        });
        if let Some(dir) = meta.path.parent() {
            ui.add_space(-6.0);
            let text = dir.to_string_lossy();
            let resp = bidi::label(ui, &text, 12.0, false, p.faint);
            resp.on_hover_text("Folder");
        }
    }

    fn summary_card(&mut self, ui: &mut Ui, meta: &Meta) {
        let p = pal(ui);
        let clean = meta.is_clean();
        let color = if clean { p.ok } else { p.warn };
        Frame::new()
            .fill(p.tint(color, 0.08))
            .stroke(Stroke::new(1.0_f32, p.tint(color, 0.45)))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    widgets::flag_badges(ui, &meta.flags(), meta.error.is_some());
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(summary(meta)).font(semibold(15.0)))
                        .wrap(),
                );
                let removable = meta.removable_count();
                if !clean && removable > 0 {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(format!(
                            "{removable} {} can be removed. The picture, text or sound itself is not changed.",
                            if removable == 1 { "field" } else { "fields" }
                        ))
                        .size(12.5)
                        .color(p.muted),
                    );
                    ui.add_space(6.0);
                    let label = Icon::BROOM.with("Clean this file");
                    if widgets::primary_button(ui, &label, true).clicked() {
                        for item in &mut self.items {
                            item.selected = false;
                        }
                        self.open_clean();
                    }
                }
            });
    }

    fn field_group(&mut self, ui: &mut Ui, cat: Category, fields: &[&Field]) {
        let p = pal(ui);
        let show_tech = self.settings.show_technical;
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin {
                left: 14,
                right: 8,
                top: 10,
                bottom: 8,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    let color = fields
                        .iter()
                        .find_map(|f| f.flag)
                        .map(|f| p.flag(f))
                        .unwrap_or(p.muted);
                    ui.label(
                        RichText::new(category_icon(cat).glyph())
                            .color(color)
                            .size(14.0),
                    );
                    ui.label(RichText::new(cat.title()).font(semibold(14.5)));
                    ui.label(
                        RichText::new(fields.len().to_string())
                            .size(12.0)
                            .color(p.faint),
                    );
                    if cat == Category::Technical {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(
                                RichText::new("kept when cleaning")
                                    .size(11.5)
                                    .color(p.faint),
                            );
                        });
                    }
                });
                ui.add_space(2.0);
                for (k, f) in fields.iter().enumerate() {
                    if k > 0 {
                        let (r, _) =
                            ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                        ui.painter().rect_filled(r, 0, p.border.gamma_multiply(0.6));
                    }
                    field_row(ui, f, show_tech, &p);
                }
            });
    }
}

/// One label / value line with a copy button. Laid out with exact sizes so
/// rows never widen the panel.
fn field_row(ui: &mut Ui, f: &Field, show_tech: bool, p: &Palette) {
    let total = ui.available_width();
    let label_x = if f.flag.is_some() { 12.0 } else { 0.0 };
    let label_w = (total * 0.36).clamp(110.0, 170.0);
    let copy_w = 28.0;
    let value_w = (total - label_w - copy_w - 8.0).max(60.0);
    let label_g = ui.painter().layout(
        f.label.clone(),
        regular(13.0),
        p.muted,
        label_w - label_x - 6.0,
    );
    let rtl = bidi::needs_native(&f.value);
    let value_g = (!rtl).then(|| {
        ui.painter()
            .layout(f.value.clone(), regular(13.5), p.text, value_w)
    });
    let value_h = value_g.as_ref().map(|g| g.size().y).unwrap_or(20.0);
    let tech_g = show_tech.then(|| {
        ui.painter().layout(
            format!("{}  ·  {}", f.id, f.source),
            regular(11.0),
            p.faint,
            value_w,
        )
    });
    let tech_h = tech_g.as_ref().map(|g| g.size().y + 1.0).unwrap_or(0.0);
    let h = label_g.size().y.max(value_h + tech_h).max(20.0) + 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(total, h), Sense::hover());
    let top = rect.top() + 5.0;
    if let Some(flag) = f.flag {
        ui.painter()
            .circle_filled(pos2(rect.left() + 3.5, top + 9.0), 3.5, p.flag(flag));
    }
    ui.painter()
        .galley(pos2(rect.left() + label_x, top + 1.0), label_g, p.muted);
    let vx = rect.left() + label_w;
    match value_g {
        Some(g) => ui.painter().galley(pos2(vx, top), g, p.text),
        None => {
            bidi::paint(
                ui,
                Rect::from_min_size(pos2(vx, top), vec2(value_w, 20.0)),
                &f.value,
                13.5,
                false,
                p.text,
                false,
            );
        }
    }
    if let Some(g) = tech_g {
        ui.painter()
            .galley(pos2(vx, top + value_h + 1.0), g, p.faint);
    }
    let copy_rect = Rect::from_min_size(pos2(rect.right() - copy_w, top - 2.0), vec2(copy_w, 22.0));
    let copy = ui.interact(
        copy_rect,
        ui.id().with(("copy", &f.id, rect.top() as i32)),
        Sense::click(),
    );
    if copy.hovered() {
        ui.painter()
            .rect_filled(copy_rect, CornerRadius::same(6), p.hover);
    }
    ui.painter().text(
        copy_rect.center(),
        Align2::CENTER_CENTER,
        Icon::COPY.glyph(),
        regular(12.0),
        if copy.hovered() { p.text } else { p.faint },
    );
    if copy.on_hover_text("Copy the value").clicked() {
        ui.ctx().copy_text(f.value.clone());
        ui.ctx().request_repaint();
    }
    resp.on_hover_text(match f.flag {
        Some(flag) => format!("{} · stored in {}", flag.long_label(), f.source),
        None => format!("Stored in {}", f.source),
    });
}

fn gps_card(ui: &mut Ui, g: crate::meta::GeoPoint) {
    let p = pal(ui);
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.danger.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
                ui.painter()
                    .circle_filled(r.center(), 20.0, p.danger.gamma_multiply(0.15));
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    Icon::MAP_PIN.glyph(),
                    regular(18.0),
                    p.danger,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(RichText::new("GPS position").size(12.5).color(p.muted));
                    ui.label(RichText::new(g.display()).font(semibold(16.0)));
                });
            });
            ui.add_space(4.0);
            widgets::row(ui, 30.0, |ui| {
                let link = egui::Button::new(
                    RichText::new(Icon::OPEN_EXTERNAL.with("Open in maps")).color(p.accent),
                )
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::new(1.0_f32, p.accent.gamma_multiply(0.5)))
                .corner_radius(CornerRadius::same(8));
                if ui
                    .add(link)
                    .on_hover_text("Opens OpenStreetMap in your browser")
                    .clicked()
                {
                    let _ = sys::open(&g.map_url());
                }
                let copy = egui::Button::new(RichText::new(Icon::COPY.with("Copy")).color(p.muted))
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::NONE);
                if ui.add(copy).clicked() {
                    ui.ctx().copy_text(g.display());
                }
            });
            ui.label(
                RichText::new("Nothing goes online unless you click Open in maps.")
                    .size(11.5)
                    .color(p.faint),
            );
        });
}

/// A drawn stand-in for the demo's invented pictures and videos.
fn paint_placeholder(ui: &Ui, rect: Rect, meta: &Meta, p: &Palette) {
    let painter = ui.painter_at(rect);
    let seed = meta.name().len() as f32;
    let (sky_top, sky_bottom) = if meta.kind() == Kind::Video {
        (
            Color32::from_rgb(0x1d, 0x2b, 0x5e),
            Color32::from_rgb(0xe0, 0x7a, 0x5f),
        )
    } else if (seed as usize).is_multiple_of(2) {
        (
            Color32::from_rgb(0x2a, 0x6f, 0xdb),
            Color32::from_rgb(0x9f, 0xd8, 0xf5),
        )
    } else {
        (
            Color32::from_rgb(0x25, 0x49, 0xd6),
            Color32::from_rgb(0x2b, 0xd9, 0xa0),
        )
    };
    // Vertical gradient as a mesh.
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), sky_top);
    mesh.colored_vertex(rect.right_top(), sky_top);
    mesh.colored_vertex(rect.left_bottom(), sky_bottom);
    mesh.colored_vertex(rect.right_bottom(), sky_bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    // Sun and hills.
    painter.circle_filled(
        pos2(
            rect.left() + rect.width() * 0.72,
            rect.top() + rect.height() * 0.34,
        ),
        rect.height() * 0.13,
        Color32::from_rgba_unmultiplied(255, 236, 179, 230),
    );
    let base = rect.bottom();
    let hill = |x0: f32, x1: f32, peak: f32, c: Color32| {
        let pts = vec![
            pos2(rect.left() + rect.width() * x0, base),
            pos2(
                rect.left() + rect.width() * (x0 + x1) / 2.0,
                rect.top() + rect.height() * peak,
            ),
            pos2(rect.left() + rect.width() * x1, base),
        ];
        painter.add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
    };
    hill(
        -0.2,
        0.6,
        0.42,
        Color32::from_rgba_unmultiplied(16, 40, 80, 200),
    );
    hill(
        0.3,
        1.2,
        0.5,
        Color32::from_rgba_unmultiplied(10, 30, 60, 220),
    );
    hill(
        0.05,
        0.75,
        0.68,
        Color32::from_rgba_unmultiplied(6, 20, 40, 235),
    );
    if meta.kind() == Kind::Video {
        painter.circle_filled(rect.center(), 24.0, Color32::from_black_alpha(140));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "\u{E768}",
            regular(20.0),
            Color32::WHITE,
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(6.0_f32, p.bg),
        StrokeKind::Outside,
    );
    painter.text(
        rect.left_bottom() + vec2(10.0, -10.0),
        Align2::LEFT_BOTTOM,
        "Demo picture",
        regular(11.0),
        Color32::from_white_alpha(200),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    fn by_name(name: &str) -> Meta {
        demo::files()
            .into_iter()
            .find(|m| m.name() == name)
            .unwrap()
    }

    #[test]
    fn filters_and_search() {
        let files = demo::files();
        let count = |f: ListFilter, q: &str| files.iter().filter(|m| matches(m, f, q)).count();
        assert_eq!(count(ListFilter::All, ""), files.len());
        let loc = count(ListFilter::Location, "");
        assert!(loc >= 4 && loc < files.len());
        assert!(count(ListFilter::Names, "") >= 4);
        assert_eq!(count(ListFilter::NotClean, ""), files.len() - 1);
        // Search looks at names and at field values, ignoring case.
        assert_eq!(count(ListFilter::All, "quarterly"), 1);
        assert!(count(ListFilter::All, "jordan blake") >= 2);
        assert_eq!(count(ListFilter::All, "ئاسۆ"), 1);
        assert_eq!(count(ListFilter::Location, "docx"), 0);
        assert_eq!(count(ListFilter::All, "no-such-thing-zz"), 0);
    }

    #[test]
    fn details_search_narrows_fields() {
        let m = by_name("IMG_2041.jpg");
        assert_eq!(shown_fields(&m, "").len(), m.fields.len());
        let serial = shown_fields(&m, "serial");
        assert!(!serial.is_empty() && serial.iter().all(|f| f.label.contains("serial")));
        // Matching only the name shows everything.
        assert_eq!(shown_fields(&m, "IMG_2041").len(), m.fields.len());
    }

    #[test]
    fn plain_language_summary() {
        let s = summary(&by_name("IMG_2041.jpg"));
        assert!(s.starts_with("This photo shows where it was taken"), "{s}");
        assert!(s.contains("the camera's serial number"), "{s}");
        assert!(s.contains(" and "), "{s}");
        let s = summary(&by_name("Screenshot 2025-09-02.png"));
        assert_eq!(s, "Nothing personal was found in this photo.");
        let s = summary(&by_name("invoice-0425.pdf"));
        assert!(s.contains("where it was downloaded from"), "{s}");
        assert_eq!(join_and(&["a", "b", "c"]), "a, b and c");
    }
}
