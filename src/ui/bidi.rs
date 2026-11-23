//! (Shared with FindForge.) Display of right-to-left scripts (Kurdish Sorani, Arabic, Persian, Urdu,
//! Hebrew…).
//!
//! egui lays text out one glyph per character, left to right, and never joins
//! letters, so a Kurdish or Arabic name comes out backwards and in pieces.
//! Substituting "presentation form" characters is not enough either: several
//! Kurdish letters (ڕ ڵ ە ێ) have none, so words still break apart.
//!
//! Instead, text that contains such letters is drawn by Windows itself
//! (Uniscribe, which shapes every script and orders mixed text the same way
//! File Explorer does) into a small white-on-transparent bitmap, cached as an
//! egui texture and tinted with the wanted colour. Windows also reports where
//! every character ended up, which is what makes match highlighting and a
//! working caret in text boxes possible. Everything else still goes through
//! egui, so ordinary names cost one character scan.

use eframe::egui::{
    self, pos2, vec2, Color32, ColorImage, Pos2, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

fn is_rtl_char(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Whether `s` contains right-to-left letters that egui cannot draw properly.
pub fn needs_native(s: &str) -> bool {
    s.chars().any(is_rtl_char)
}

/// The widest bitmap ever made, in pixels.
const HARD_MAX: u32 = 8192;
/// Text boxes with more characters than this fall back to plain egui editing.
const EDIT_LIMIT: usize = 600;

/// A rendered line: white text with coverage in the alpha channel.
struct Rendered {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    cut: bool,
    /// For every UTF-16 unit that was drawn: the x of its leading and of its
    /// trailing edge (leading is on the right for right-to-left letters).
    edges: Vec<[i32; 2]>,
    /// How many UTF-16 units of the text were drawn (fewer when it was cut).
    units: usize,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct Key {
    text: String,
    size_px: u32,
    bold: bool,
    max_px: u32,
}

struct Entry {
    /// Keeps the texture alive while the line is cached.
    #[allow(dead_code)]
    tex: TextureHandle,
    line: Line,
}

/// A cached line, ready to paint. Sizes are in physical pixels.
#[derive(Clone)]
struct Line {
    tex: egui::TextureId,
    size: [usize; 2],
    cut: bool,
    edges: Rc<[[i32; 2]]>,
    units: usize,
}

/// Rendered lines are kept while they are in use; the cache is simply emptied
/// when it grows large (scrolling through thousands of names).
const CACHE_LIMIT: usize = 1500;

thread_local! {
    static CACHE: RefCell<HashMap<Key, Entry>> = RefCell::new(HashMap::new());
}

/// The few Uniscribe (usp10.dll) functions used here, declared directly so
/// the crate does not need the `Win32_Globalization` feature of `windows`.
#[cfg(windows)]
mod usp {
    use core::ffi::c_void;

    pub const SSA_FALLBACK: u32 = 0x20;
    pub const SSA_GLYPHS: u32 = 0x80;
    pub const SSA_LINK: u32 = 0x1000;

    #[repr(C)]
    pub struct Size {
        pub cx: i32,
        pub cy: i32,
    }

    #[link(name = "usp10", kind = "raw-dylib")]
    extern "system" {
        #[allow(clippy::too_many_arguments)]
        pub fn ScriptStringAnalyse(
            hdc: *mut c_void,
            string: *const c_void,
            len: i32,
            glyphs: i32,
            charset: i32,
            flags: u32,
            req_width: i32,
            control: *const c_void,
            state: *const c_void,
            dx: *const i32,
            tabdef: *const c_void,
            in_class: *const u8,
            ssa: *mut *mut c_void,
        ) -> i32;
        pub fn ScriptString_pSize(ssa: *mut c_void) -> *const Size;
        pub fn ScriptStringFree(ssa: *mut *mut c_void) -> i32;
        #[allow(clippy::too_many_arguments)]
        pub fn ScriptStringOut(
            ssa: *mut c_void,
            x: i32,
            y: i32,
            options: u32,
            rect: *const c_void,
            min_sel: i32,
            max_sel: i32,
            disabled: i32,
        ) -> i32;
        pub fn ScriptStringCPtoX(ssa: *mut c_void, cp: i32, trailing: i32, x: *mut i32) -> i32;
    }

    /// `ScriptStringCPtoX` as a `Result`-free helper.
    ///
    /// SAFETY: `ssa` must be a live analysis.
    pub unsafe fn cp_to_x(ssa: *mut c_void, cp: i32, trailing: bool) -> Option<i32> {
        let mut x = 0;
        (ScriptStringCPtoX(ssa, cp, trailing as i32, &mut x) >= 0).then_some(x)
    }
}

/// A Uniscribe analysis of one line: shaping, ordering and positions.
#[cfg(windows)]
struct Analysis(*mut core::ffi::c_void);

#[cfg(windows)]
impl Analysis {
    /// SAFETY: `dc` must be a valid device context with the font selected.
    unsafe fn new(dc: windows::Win32::Graphics::Gdi::HDC, text: &[u16]) -> Option<Analysis> {
        use usp::{ScriptStringAnalyse, SSA_FALLBACK, SSA_GLYPHS, SSA_LINK};
        if text.is_empty() {
            return None;
        }
        let mut ssa = std::ptr::null_mut();
        let hr = ScriptStringAnalyse(
            dc.0,
            text.as_ptr() as *const core::ffi::c_void,
            text.len() as i32,
            (text.len() * 3 / 2 + 16) as i32,
            -1,
            SSA_GLYPHS | SSA_FALLBACK | SSA_LINK,
            0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            &mut ssa,
        );
        if hr < 0 {
            return None;
        }
        (!ssa.is_null()).then_some(Analysis(ssa))
    }

    fn size(&self) -> (i32, i32) {
        // SAFETY: the analysis is alive; the returned pointer belongs to it.
        unsafe {
            let p = usp::ScriptString_pSize(self.0);
            if p.is_null() {
                (0, 0)
            } else {
                ((*p).cx, (*p).cy)
            }
        }
    }
}

#[cfg(windows)]
impl Drop for Analysis {
    fn drop(&mut self) {
        // SAFETY: frees the analysis made in `new`, exactly once.
        unsafe {
            let _ = usp::ScriptStringFree(&mut self.0);
        }
    }
}

#[cfg(windows)]
fn render(text: &str, size_px: u32, bold: bool, max_px: u32) -> Option<Rendered> {
    use windows::core::w;
    use windows::Win32::Graphics::Gdi::*;

    let full: Vec<u16> = text.encode_utf16().collect();
    if full.is_empty() || size_px == 0 {
        return None;
    }
    let max = max_px.clamp(1, HARD_MAX) as i32;
    // SAFETY: plain GDI calls on a memory DC and a font we create and free here.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return None;
        }
        let mut face = [0u16; 32];
        let name = w!("Segoe UI");
        let name = name.as_wide();
        face[..name.len()].copy_from_slice(name);
        let lf = LOGFONTW {
            lfHeight: -(size_px as i32),
            lfWeight: if bold { 600 } else { 400 },
            lfCharSet: DEFAULT_CHARSET,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: ANTIALIASED_QUALITY,
            lfFaceName: face,
            ..Default::default()
        };
        let font = CreateFontIndirectW(&lf);
        let old_font = SelectObject(dc, font);
        let result = draw(dc, &full, max);
        SelectObject(dc, old_font);
        let _ = DeleteObject(font);
        let _ = DeleteDC(dc);
        result
    }
}

/// Draws `full` (cut to `max` pixels with an ellipsis) on a new bitmap.
///
/// SAFETY: `dc` must be a valid memory device context with the font selected.
#[cfg(windows)]
unsafe fn draw(dc: windows::Win32::Graphics::Gdi::HDC, full: &[u16], max: i32) -> Option<Rendered> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::Graphics::Gdi::*;

    let mut drawn: Vec<u16> = full.to_vec();
    let mut analysis = Analysis::new(dc, &drawn)?;
    let cut = analysis.size().0 > max;
    if cut {
        // Keep the longest beginning that fits together with an ellipsis.
        let starts: Vec<usize> = (0..full.len())
            .filter(|i| !(0xDC00..0xE000).contains(&full[*i]))
            .collect();
        let with_dots = |units: usize| {
            let mut v = full[..units].to_vec();
            v.push(0x2026);
            v
        };
        drawn = with_dots(0);
        analysis = Analysis::new(dc, &drawn)?;
        let (mut lo, mut hi) = (1usize, starts.len());
        while lo < hi {
            let mid = (lo + hi) / 2;
            let candidate = with_dots(starts[mid]);
            match Analysis::new(dc, &candidate) {
                Some(a) if a.size().0 <= max => {
                    drawn = candidate;
                    analysis = a;
                    lo = mid + 1;
                }
                _ => hi = mid,
            }
        }
    }
    let (w, h) = analysis.size();
    let width = w.clamp(1, max) as usize;
    let height = h.max(1) as usize;

    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp);
        return None;
    }
    let old_bmp = SelectObject(dc, bmp);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, COLORREF(0x00FF_FFFF));
    let painted = usp::ScriptStringOut(analysis.0, 0, 0, 0, std::ptr::null(), 0, 0, 0) >= 0;
    let _ = GdiFlush();
    let mut pixels = Vec::with_capacity(width * height * 4);
    if painted {
        let src = std::slice::from_raw_parts(bits as *const u8, width * height * 4);
        for px in src.as_chunks::<4>().0 {
            // Grey-scale anti-aliasing: any channel is the coverage.
            // A mild curve thins the edges to match egui's own text weight.
            let c = px[0].max(px[1]).max(px[2]);
            let a = (255.0 * (c as f32 / 255.0).powf(1.45)).round() as u8;
            pixels.extend_from_slice(&[255, 255, 255, a]);
        }
    }
    SelectObject(dc, old_bmp);
    let _ = DeleteObject(bmp);
    if !painted {
        return None;
    }
    let mut edges = Vec::with_capacity(drawn.len());
    for i in 0..drawn.len() as i32 {
        let lead = usp::cp_to_x(analysis.0, i, false).unwrap_or(0);
        let trail = usp::cp_to_x(analysis.0, i, true).unwrap_or(lead);
        edges.push([lead, trail]);
    }
    Some(Rendered {
        pixels,
        width,
        height,
        cut,
        edges,
        units: if cut { drawn.len() - 1 } else { drawn.len() },
    })
}

#[cfg(not(windows))]
fn render(_: &str, _: u32, _: bool, _: u32) -> Option<Rendered> {
    None
}

fn lookup(ctx: &egui::Context, text: &str, size_px: u32, bold: bool, max_px: u32) -> Option<Line> {
    let key = Key {
        text: text.to_string(),
        size_px,
        bold,
        max_px,
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(e) = cache.get(&key) {
            return Some(e.line.clone());
        }
        let r = render(text, size_px, bold, max_px)?;
        let image = ColorImage::from_rgba_unmultiplied([r.width, r.height], &r.pixels);
        let tex = ctx.load_texture("rtl-text", image, TextureOptions::NEAREST);
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let line = Line {
            tex: tex.id(),
            size: [r.width, r.height],
            cut: r.cut,
            edges: r.edges.into(),
            units: r.units,
        };
        cache.insert(
            key,
            Entry {
                tex,
                line: line.clone(),
            },
        );
        Some(line)
    })
}

/// Looks up or renders a line no wider than `max_px`. The uncut line is
/// tried first, so resizing a column does not redraw names that fit anyway.
fn line(ctx: &egui::Context, text: &str, size_px: u32, bold: bool, max_px: u32) -> Option<Line> {
    let whole = lookup(ctx, text, size_px, bold, HARD_MAX)?;
    if max_px >= HARD_MAX || (!whole.cut && whole.size[0] as u32 <= max_px) {
        return Some(whole);
    }
    lookup(ctx, text, size_px, bold, max_px)
}

fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

/// Paints the pixels `a..b` of a line whose top-left corner is at `min`.
fn paint_piece(painter: &egui::Painter, l: &Line, min: Pos2, ppp: f32, a: i32, b: i32, c: Color32) {
    if b <= a {
        return;
    }
    let (w, h) = (l.size[0] as f32, l.size[1] as f32);
    painter.image(
        l.tex,
        Rect::from_min_max(
            pos2(min.x + a as f32 / ppp, min.y),
            pos2(min.x + b as f32 / ppp, min.y + h / ppp),
        ),
        Rect::from_min_max(pos2(a as f32 / w, 0.0), pos2(b as f32 / w, 1.0)),
        c,
    );
}

/// Pixel spans (sorted, merged) covered by the characters of `text` that lie
/// in `ranges` (byte ranges). Characters that were cut off are left out.
fn mark_spans(
    text: &str,
    ranges: &[Range<usize>],
    edges: &[[i32; 2]],
    units: usize,
    width: i32,
) -> Vec<(i32, i32)> {
    let mut spans: Vec<(i32, i32)> = Vec::new();
    if ranges.is_empty() {
        return spans;
    }
    let drawn = units.min(edges.len());
    let mut unit = 0usize;
    for (byte, c) in text.char_indices() {
        let n = c.len_utf16();
        if unit + n > drawn {
            break;
        }
        if ranges.iter().any(|r| r.contains(&byte)) {
            for [a, b] in &edges[unit..unit + n] {
                let (lo, hi) = ((*a).min(*b).max(0), (*a).max(*b).min(width));
                if hi > lo {
                    spans.push((lo, hi));
                }
            }
        }
        unit += n;
    }
    spans.sort_unstable();
    let mut merged: Vec<(i32, i32)> = Vec::with_capacity(spans.len());
    for (lo, hi) in spans {
        match merged.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

/// Where the caret stands before each character of `text` (and, last, after
/// the final one), in pixels from the left of the line.
fn caret_stops(text: &str, edges: &[[i32; 2]], units: usize) -> Vec<i32> {
    let drawn = units.min(edges.len());
    let end = if drawn > 0 { edges[drawn - 1][1] } else { 0 };
    let mut stops = Vec::with_capacity(text.len() + 1);
    let mut unit = 0usize;
    for c in text.chars() {
        stops.push(if unit < drawn { edges[unit][0] } else { end });
        unit += c.len_utf16();
    }
    stops.push(end);
    stops
}

/// The caret stop closest to `x`.
fn nearest_stop(stops: &[i32], x: i32) -> usize {
    stops
        .iter()
        .enumerate()
        .min_by_key(|(_, s)| (**s - x).abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Paints `text` on one line inside `rect`, vertically centred, cut with an
/// ellipsis when too wide. Returns `None` when native drawing is unavailable,
/// otherwise whether the text was cut.
pub fn paint(
    ui: &Ui,
    rect: Rect,
    text: &str,
    size_pt: f32,
    bold: bool,
    color: Color32,
    right: bool,
) -> Option<bool> {
    paint_line(ui, rect, text, size_pt, bold, color, right, color, &[])
}

#[allow(clippy::too_many_arguments)]
fn paint_line(
    ui: &Ui,
    rect: Rect,
    text: &str,
    size_pt: f32,
    bold: bool,
    color: Color32,
    right: bool,
    mark: Color32,
    ranges: &[Range<usize>],
) -> Option<bool> {
    let ppp = ui.ctx().pixels_per_point();
    let size_px = (size_pt * ppp).round() as u32;
    let max_px = (rect.width() * ppp).floor().max(1.0) as u32;
    let l = line(ui.ctx(), text, size_px, bold, max_px)?;
    let size = vec2(l.size[0] as f32, l.size[1] as f32) / ppp;
    let x = if right {
        rect.right() - size.x
    } else {
        rect.left()
    };
    let y = rect.center().y - size.y / 2.0;
    // Snap to whole pixels so the bitmap is drawn 1:1, without blurring.
    let min = pos2(snap(x, ppp), snap(y, ppp));
    let width = l.size[0] as i32;
    let painter = ui.painter();
    let mut at = 0;
    for (lo, hi) in mark_spans(text, ranges, &l.edges, l.units, width) {
        paint_piece(painter, &l, min, ppp, at, lo, color);
        paint_piece(painter, &l, min, ppp, lo, hi, mark);
        at = hi;
    }
    paint_piece(painter, &l, min, ppp, at, width, color);
    Some(l.cut)
}

/// The size `text` would take on one line, in points.
pub fn measure(ui: &Ui, text: &str, size_pt: f32, bold: bool) -> Option<Vec2> {
    let ppp = ui.ctx().pixels_per_point();
    let size_px = (size_pt * ppp).round() as u32;
    let l = line(ui.ctx(), text, size_px, bold, HARD_MAX)?;
    Some(vec2(l.size[0] as f32, l.size[1] as f32) / ppp)
}

/// A one-line label that fills the available width and truncates, drawing
/// right-to-left text natively. Falls back to an egui label for other text.
pub fn label(ui: &mut Ui, text: &str, size_pt: f32, bold: bool, color: Color32) -> egui::Response {
    if needs_native(text) {
        if let Some(natural) = measure(ui, text, size_pt, bold) {
            let width = natural.x.min(ui.available_width()).max(1.0);
            let (rect, response) = ui.allocate_exact_size(vec2(width, natural.y), Sense::hover());
            paint(ui, rect, text, size_pt, bold, color, false);
            return response.on_hover_text(text);
        }
    }
    let font = if bold {
        super::theme::semibold(size_pt)
    } else {
        egui::FontId::proportional(size_pt)
    };
    ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).truncate())
}

/// A one-line text box that shows right-to-left text properly.
///
/// egui still does the editing (typing, the clipboard, arrow keys, undo); when
/// the text has right-to-left letters its own drawing is made invisible and
/// the text, the selection and the caret are painted here from what Windows
/// reports, and clicks are mapped back to characters the same way.
/// `build` adds the options (hint, width, font…) to the text box.
pub fn show_edit<'t>(
    ui: &mut Ui,
    text: &'t mut String,
    size_pt: f32,
    color: Color32,
    build: impl FnOnce(egui::TextEdit<'t>) -> egui::TextEdit<'t>,
) -> egui::text_edit::TextEditOutput {
    use egui::text::{CCursor, CCursorRange};

    let ppp = ui.ctx().pixels_per_point();
    let size_px = (size_pt * ppp).round() as u32;
    let native = needs_native(text)
        && text.chars().count() <= EDIT_LIMIT
        && line(ui.ctx(), text, size_px, false, HARD_MAX).is_some();
    let edit = build(egui::TextEdit::singleline(text));
    if !native {
        let out = edit.show(ui);
        if needs_native(out.galley.text()) {
            // The first right-to-left letter was just typed.
            ui.ctx().request_repaint();
        }
        return out;
    }

    let saved = ui.visuals().clone();
    let selection_fill = saved.selection.bg_fill;
    let caret_stroke = saved.text_cursor.stroke;
    {
        let v = ui.visuals_mut();
        v.selection.bg_fill = Color32::TRANSPARENT;
        v.text_cursor.stroke = Stroke::NONE;
    }
    let mut out = edit.text_color(Color32::TRANSPARENT).show(ui);
    *ui.visuals_mut() = saved;

    let shown = out.galley.text().to_owned();
    let l = if shown.is_empty() || shown.chars().count() > EDIT_LIMIT {
        None
    } else {
        line(ui.ctx(), &shown, size_px, false, HARD_MAX)
    };
    let Some(l) = l else {
        ui.ctx().request_repaint();
        return out;
    };
    let clip = out.text_clip_rect;
    let id = out.response.id;
    let stops = caret_stops(&shown, &l.edges, l.units);
    let chars = stops.len() - 1;
    let scroll_id = id.with("rtl-scroll");
    let anchor_id = id.with("rtl-anchor");
    let mut scroll: f32 = ui.data(|d| d.get_temp(scroll_id)).unwrap_or(0.0);

    // The mouse: egui placed the caret by its own left-to-right layout.
    let (pos, down, pressed, shift) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.primary_down(),
            i.pointer.primary_pressed(),
            i.modifiers.shift,
        )
    });
    let mut moved = false;
    if let (true, true, Some(pos)) = (down, out.response.is_pointer_button_down_on(), pos) {
        let x = ((pos.x - clip.left() + scroll) * ppp).round() as i32;
        let at = nearest_stop(&stops, x);
        let before = out.state.cursor.char_range().map(|r| r.secondary.index);
        let anchor = if pressed {
            let a = if shift { before.unwrap_or(at) } else { at };
            ui.data_mut(|d| d.insert_temp(anchor_id, a));
            a
        } else {
            ui.data(|d| d.get_temp(anchor_id)).unwrap_or(at)
        };
        out.state.cursor.set_char_range(Some(CCursorRange {
            primary: CCursor::new(at),
            secondary: CCursor::new(anchor.min(chars)),
        }));
        moved = true;
    }
    if out.response.double_clicked() {
        out.state.cursor.set_char_range(Some(CCursorRange::two(
            CCursor::new(0),
            CCursor::new(chars),
        )));
        moved = true;
    }
    if moved {
        out.state.clone().store(ui.ctx(), id);
    }

    let focused = out.response.has_focus();
    let range = out
        .state
        .cursor
        .char_range()
        .map(|r| (r.primary.index.min(chars), r.secondary.index.min(chars)));
    let width = l.size[0] as f32 / ppp;
    let height = l.size[1] as f32 / ppp;
    let view = clip.width();
    if width <= view {
        scroll = 0.0;
    } else {
        if let (true, Some((primary, _))) = (focused, range) {
            let caret = stops[primary] as f32 / ppp;
            if caret - scroll > view - 2.0 {
                scroll = caret - view + 2.0;
            }
            if caret - scroll < 0.0 {
                scroll = caret;
            }
        }
        scroll = scroll.clamp(0.0, width - view);
    }
    ui.data_mut(|d| d.insert_temp(scroll_id, scroll));

    let painter = ui.painter_at(clip.expand(1.0));
    let min = pos2(
        snap(clip.left() - scroll, ppp),
        snap(clip.center().y - height / 2.0, ppp),
    );
    if let (true, Some((primary, secondary))) = (focused, range) {
        if primary != secondary {
            let byte_of = |ch: usize| {
                shown
                    .char_indices()
                    .nth(ch)
                    .map(|(b, _)| b)
                    .unwrap_or(shown.len())
            };
            let selected = byte_of(primary.min(secondary))..byte_of(primary.max(secondary));
            for (lo, hi) in mark_spans(&shown, &[selected], &l.edges, l.units, l.size[0] as i32) {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(min.x + lo as f32 / ppp, min.y),
                        pos2(min.x + hi as f32 / ppp, min.y + height),
                    ),
                    0.0,
                    selection_fill,
                );
            }
        }
    }
    paint_piece(&painter, &l, min, ppp, 0, l.size[0] as i32, color);
    if let (true, Some((primary, _))) = (focused, range) {
        let x = snap(min.x + stops[primary] as f32 / ppp, ppp);
        let stroke = if caret_stroke.width > 0.0 {
            caret_stroke
        } else {
            Stroke::new(2.0_f32, color)
        };
        painter.vline(x, (min.y + 1.0)..=(min.y + height - 1.0), stroke);
    }
    out
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn only_rtl_text_takes_the_native_path() {
        assert!(!needs_native("report.pdf"));
        assert!(!needs_native("C:\\Users\\Alex\\Documents"));
        assert!(needs_native("گۆرانی کوردی.mp3"));
        assert!(needs_native("D:\\Music\\بەڵگەنامەکان\\track.mp3"));
        assert!(needs_native("תיקייה"));
    }

    #[test]
    fn marks_follow_the_characters() {
        // "ab" left to right, then two right-to-left letters (drawn 30..10).
        let edges = [[0, 5], [5, 10], [30, 20], [20, 10]];
        let text = "abگۆ";
        assert_eq!(mark_spans(text, &[], &edges, 4, 30), vec![]);
        assert_eq!(mark_spans(text, &[0..1], &edges, 4, 30), vec![(0, 5)]);
        // Both right-to-left letters: one merged span.
        assert_eq!(mark_spans(text, &[2..6], &edges, 4, 30), vec![(10, 30)]);
        // The first of them is the one on the right.
        assert_eq!(mark_spans(text, &[2..4], &edges, 4, 30), vec![(20, 30)]);
        // Letters that were cut off are not marked; spans stay inside the width.
        assert_eq!(mark_spans(text, &[0..6], &edges, 2, 30), vec![(0, 10)]);
        assert_eq!(mark_spans(text, &[0..6], &edges, 4, 25), vec![(0, 25)]);
        // A character outside the basic plane takes two units.
        let edges = [[0, 4], [4, 8], [8, 12]];
        assert_eq!(mark_spans("😀a", &[4..5], &edges, 3, 12), vec![(8, 12)]);
    }

    #[test]
    fn caret_positions() {
        let edges = [[0, 5], [5, 10], [30, 20], [20, 10]];
        let stops = caret_stops("abگۆ", &edges, 4);
        // After the last (right-to-left) letter the caret is at its left edge.
        assert_eq!(stops, vec![0, 5, 30, 20, 10]);
        assert_eq!(nearest_stop(&stops, -4), 0);
        assert_eq!(nearest_stop(&stops, 6), 1);
        assert_eq!(nearest_stop(&stops, 19), 3);
        assert_eq!(nearest_stop(&stops, 11), 4);
        assert_eq!(nearest_stop(&stops, 400), 2);
        assert_eq!(caret_stops("", &[], 0), vec![0]);
        // Cut text: stops past the cut all sit at the end.
        assert_eq!(caret_stops("abcd", &edges[..2], 2), vec![0, 5, 10, 10, 10]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_renders_kurdish_text_with_ink() {
        let r = render("بەڵگەنامەکان ڕاپۆرت", 20, false, 2000).expect("rendered");
        assert!(r.width > 40 && r.height >= 18, "{}x{}", r.width, r.height);
        assert!(!r.cut);
        let ink = r
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 128)
            .count();
        assert!(ink > 50, "expected visible letters, got {ink} dark pixels");
    }

    #[cfg(windows)]
    #[test]
    fn windows_reports_where_letters_are() {
        let text = "ab گۆڕ";
        let r = render(text, 20, false, 2000).expect("rendered");
        assert_eq!(r.edges.len(), text.encode_utf16().count());
        assert_eq!(r.units, r.edges.len());
        // Latin letters run left to right…
        assert!(r.edges[0][0] < r.edges[0][1] && r.edges[0][1] <= r.edges[1][0] + 1);
        // …Kurdish letters right to left: the first is the rightmost.
        assert!(r.edges[3][0] > r.edges[3][1], "{:?}", r.edges);
        assert!(r.edges[3][0] > r.edges[5][0], "{:?}", r.edges);
        assert!(r.edges.iter().all(|e| e[0] >= 0 && e[0] <= r.width as i32));
        // A match on the Kurdish word is marked at the right end of the line.
        let spans = mark_spans(text, &[3..text.len()], &r.edges, r.units, r.width as i32);
        assert_eq!(spans.len(), 1);
        assert!(spans[0].0 >= r.edges[1][1] && spans[0].1 == r.edges[3][0]);
    }

    #[cfg(windows)]
    #[test]
    fn long_text_is_cut_to_the_width() {
        let text = "ڕاپۆرتی ساڵانەی بەڵگەنامەکانی کوردستان";
        let r = render(text, 20, false, 60).expect("rendered");
        assert!(r.cut);
        assert!(r.width <= 60 && r.width > 20, "{}", r.width);
        // Fewer letters were drawn, plus the ellipsis.
        assert!(r.units < text.encode_utf16().count());
        assert_eq!(r.edges.len(), r.units + 1);
    }
}
