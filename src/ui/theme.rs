//! Colours, fonts and icon glyphs (the Forge family style, in MetaForge's
//! cobalt and mint).

use crate::meta::Flag;
use crate::settings::ThemeChoice;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Theme, ThemePreference, Visuals,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub rail: Color32,
    pub card: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    /// Cobalt, tuned for text and outlines on this background.
    pub accent: Color32,
    /// Cobalt for filled buttons (white text on it).
    pub accent_fill: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warn: Color32,
    pub info: Color32,
    /// Mint: "clean", "verified", "all good".
    pub ok: Color32,
    pub violet: Color32,
    pub slate: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            bg: Color32::from_rgb(0x0e, 0x13, 0x1c),
            rail: Color32::from_rgb(0x0a, 0x0e, 0x16),
            card: Color32::from_rgb(0x15, 0x1b, 0x27),
            hover: Color32::from_rgb(0x1b, 0x23, 0x32),
            selected: Color32::from_rgb(0x18, 0x28, 0x4d),
            border: Color32::from_rgb(0x25, 0x2f, 0x42),
            text: Color32::from_rgb(0xe8, 0xee, 0xf6),
            muted: Color32::from_rgb(0x9a, 0xa7, 0xba),
            faint: Color32::from_rgb(0x67, 0x74, 0x8b),
            accent: Color32::from_rgb(0x6f, 0x9a, 0xff),
            accent_fill: Color32::from_rgb(0x3a, 0x6a, 0xf0),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(0xff, 0x6b, 0x5e),
            warn: Color32::from_rgb(0xf5, 0xa5, 0x24),
            info: Color32::from_rgb(0x38, 0xbd, 0xf8),
            ok: Color32::from_rgb(0x2b, 0xd9, 0xa0),
            violet: Color32::from_rgb(0xa7, 0x8b, 0xfa),
            slate: Color32::from_rgb(0x9a, 0xa9, 0xbe),
        }
    } else {
        Palette {
            dark,
            bg: Color32::from_rgb(0xf3, 0xf6, 0xfa),
            rail: Color32::from_rgb(0xff, 0xff, 0xff),
            card: Color32::from_rgb(0xff, 0xff, 0xff),
            hover: Color32::from_rgb(0xec, 0xf0, 0xf6),
            selected: Color32::from_rgb(0xe2, 0xea, 0xff),
            border: Color32::from_rgb(0xd8, 0xdf, 0xe9),
            text: Color32::from_rgb(0x13, 0x1c, 0x2b),
            muted: Color32::from_rgb(0x52, 0x60, 0x73),
            faint: Color32::from_rgb(0x8a, 0x96, 0xa6),
            accent: Color32::from_rgb(0x25, 0x49, 0xd6),
            accent_fill: Color32::from_rgb(0x25, 0x49, 0xd6),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(0xd3, 0x3a, 0x2c),
            warn: Color32::from_rgb(0xa8, 0x62, 0x00),
            info: Color32::from_rgb(0x06, 0x6f, 0xa8),
            ok: Color32::from_rgb(0x0a, 0x8a, 0x62),
            violet: Color32::from_rgb(0x6d, 0x4e, 0xd8),
            slate: Color32::from_rgb(0x55, 0x65, 0x7a),
        }
    }
}

impl Palette {
    /// `color` mixed into the card colour (`t` = 0: card, 1: colour).
    pub fn tint(&self, color: Color32, t: f32) -> Color32 {
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color32::from_rgb(
            mix(self.card.r(), color.r()),
            mix(self.card.g(), color.g()),
            mix(self.card.b(), color.b()),
        )
    }

    /// The colour of a privacy badge.
    pub fn flag(&self, f: Flag) -> Color32 {
        match f {
            Flag::Location => self.danger,
            Flag::Names => self.warn,
            Flag::Serial => self.violet,
            Flag::Comments => self.info,
            Flag::Hidden => self.slate,
        }
    }
}

pub fn pal(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

pub fn pal_ctx(ctx: &egui::Context) -> Palette {
    palette(ctx.style().visuals.dark_mode)
}

fn visuals(dark: bool) -> Visuals {
    let p = palette(dark);
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = if dark {
        Color32::from_rgb(0x0a, 0x0f, 0x17)
    } else {
        Color32::WHITE
    };
    v.faint_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = if dark {
        Color32::from_rgb(0x24, 0x40, 0x8f)
    } else {
        Color32::from_rgb(0xc9, 0xd8, 0xff)
    };
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.muted);
    v.widgets.inactive.bg_fill = p.hover;
    v.widgets.inactive.weak_bg_fill = p.hover;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.hovered.bg_fill = p.border;
    v.widgets.hovered.weak_bg_fill = p.border;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.faint);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.active.bg_fill = p.border;
    v.widgets.active.weak_bg_fill = p.border;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.open.bg_fill = p.hover;
    v.widgets.open.weak_bg_fill = p.hover;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.faint);
    v
}

static ICON_FONT: AtomicBool = AtomicBool::new(false);

fn load_fonts(ctx: &egui::Context) {
    let dir = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "C:\\Windows".into())
        .join("Fonts");
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert(name.to_string(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    // System fonts are loaded at run time so the program stays small.
    let regular = add("ui", "segoeui.ttf");
    let semibold = add("ui-semibold", "seguisb.ttf");
    let icons = add("ui-icons", "segmdl2.ttf") || add("ui-icons", "SegoeIcons.ttf");
    // Fallback glyphs for right-to-left letters (real shaping is in `bidi`).
    let rtl = add("ui-rtl", "tahoma.ttf");
    ICON_FONT.store(icons, Ordering::Relaxed);

    let defaults = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut body = Vec::new();
    if regular {
        body.push("ui".to_string());
    }
    if icons {
        body.push("ui-icons".to_string());
    }
    if rtl {
        body.push("ui-rtl".to_string());
    }
    body.extend(defaults.iter().cloned());
    let mut strong = Vec::new();
    if semibold {
        strong.push("ui-semibold".to_string());
    }
    strong.extend(body.iter().cloned());
    fonts.families.insert(FontFamily::Proportional, body);
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), strong);
    ctx.set_fonts(fonts);
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// One-time setup: fonts, spacing and both colour schemes.
pub fn install(ctx: &egui::Context) {
    load_fonts(ctx);
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, semibold(22.0)),
            (TextStyle::Body, regular(14.0)),
            (TextStyle::Button, regular(14.0)),
            (TextStyle::Small, regular(12.0)),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.scroll.bar_width = 10.0;
        style.spacing.scroll.floating = true;
        style.interaction.selectable_labels = false;
        style.visuals.indent_has_left_vline = false;
    });
}

pub fn apply_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(match choice {
        ThemeChoice::Dark => ThemePreference::Dark,
        ThemeChoice::Light => ThemePreference::Light,
        ThemeChoice::System => ThemePreference::System,
    });
}

/// Icon glyphs from the system icon font (Segoe MDL2 Assets). If the font is
/// missing the icons are simply left out.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Icon(pub &'static str);

// A palette of glyphs; not every one is used on every screen.
#[allow(dead_code)]
impl Icon {
    pub const SEARCH: Icon = Icon("\u{E721}");
    pub const SETTINGS: Icon = Icon("\u{E713}");
    pub const DELETE: Icon = Icon("\u{E74D}");
    pub const FOLDER: Icon = Icon("\u{E8B7}");
    pub const FOLDER_OPEN: Icon = Icon("\u{E838}");
    pub const WARNING: Icon = Icon("\u{E7BA}");
    pub const INFO: Icon = Icon("\u{E946}");
    pub const SHIELD: Icon = Icon("\u{EA18}");
    pub const CHECK: Icon = Icon("\u{E73E}");
    pub const CLOSE: Icon = Icon("\u{E711}");
    pub const EXPORT: Icon = Icon("\u{EDE1}");
    pub const CHEVRON_RIGHT: Icon = Icon("\u{E76C}");
    pub const CHEVRON_DOWN: Icon = Icon("\u{E70D}");
    pub const DOCUMENT: Icon = Icon("\u{E8A5}");
    pub const LINK: Icon = Icon("\u{E71B}");
    pub const GLOBE: Icon = Icon("\u{E774}");
    pub const CLOCK: Icon = Icon("\u{E823}");
    pub const BROOM: Icon = Icon("\u{EA99}");
    pub const DONE: Icon = Icon("\u{E930}");
    pub const ERROR: Icon = Icon("\u{EA39}");
    pub const FILTER: Icon = Icon("\u{E71C}");
    pub const COPY: Icon = Icon("\u{E8C8}");
    pub const RENAME: Icon = Icon("\u{E8AC}");
    pub const PICTURE: Icon = Icon("\u{EB9F}");
    pub const MUSIC: Icon = Icon("\u{E8D6}");
    pub const VIDEO: Icon = Icon("\u{E714}");
    pub const HELP: Icon = Icon("\u{E897}");
    pub const ADD: Icon = Icon("\u{E710}");
    pub const OPEN: Icon = Icon("\u{E8E5}");
    pub const SAVE: Icon = Icon("\u{E74E}");
    pub const PAGE: Icon = Icon("\u{E7C3}");
    pub const LIST: Icon = Icon("\u{EA37}");
    pub const MAP_PIN: Icon = Icon("\u{E81D}");
    pub const CONTACT: Icon = Icon("\u{E77B}");
    pub const CAMERA: Icon = Icon("\u{E722}");
    pub const CALENDAR: Icon = Icon("\u{E787}");
    pub const CODE: Icon = Icon("\u{E943}");
    pub const TAG: Icon = Icon("\u{E8EC}");
    pub const HIDE: Icon = Icon("\u{ED1A}");
    pub const MONITOR: Icon = Icon("\u{E7F4}");
    pub const EDIT: Icon = Icon("\u{E70F}");
    pub const COMPARE: Icon = Icon("\u{E8AB}");
    pub const LOCK: Icon = Icon("\u{E72E}");
    pub const OPEN_EXTERNAL: Icon = Icon("\u{E8A7}");
    pub const UPLOAD: Icon = Icon("\u{E898}");
    pub const FINGERPRINT: Icon = Icon("\u{E928}");
    pub const COMMENT: Icon = Icon("\u{E90A}");
    pub const SELECT_ALL: Icon = Icon("\u{E8B3}");

    /// The glyph, or an empty string when the icon font is unavailable.
    pub fn glyph(self) -> &'static str {
        if ICON_FONT.load(Ordering::Relaxed) {
            self.0
        } else {
            ""
        }
    }

    /// `"<glyph>  text"` for buttons.
    pub fn with(self, text: &str) -> String {
        if ICON_FONT.load(Ordering::Relaxed) {
            format!("{}  {text}", self.0)
        } else {
            text.to_string()
        }
    }
}
