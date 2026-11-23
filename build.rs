//! Build script: renders the MetaForge icon (vector, via tiny-skia) into a
//! multi-size .ico and a PNG for the window, then embeds the icon, manifest
//! and version info.

#![allow(clippy::field_reassign_with_default)]

use std::{env, fs, path::PathBuf};
use tiny_skia::*;

fn rounded_rect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + r, y0);
    pb.line_to(x1 - r, y0);
    pb.quad_to(x1, y0, x1, y0 + r);
    pb.line_to(x1, y1 - r);
    pb.quad_to(x1, y1, x1 - r, y1);
    pb.line_to(x0 + r, y1);
    pb.quad_to(x0, y1, x0, y1 - r);
    pb.line_to(x0, y0 + r);
    pb.quad_to(x0, y0, x0 + r, y0);
    pb.close();
    pb.finish().expect("rounded rect")
}

/// The gradient of the background: cobalt blue to spring mint.
fn gradient(s: f32) -> Shader<'static> {
    LinearGradient::new(
        Point::from_xy(8.0 * s, 8.0 * s),
        Point::from_xy(248.0 * s, 248.0 * s),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x25, 0x49, 0xd6, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x2b, 0xd9, 0xa0, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("gradient")
}

/// A price-tag outline pointing left, centred on its middle; `k` scales it.
fn tag_path(k: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(-48.0 * k, 0.0);
    pb.line_to(-18.0 * k, -32.0 * k);
    pb.line_to(46.0 * k, -32.0 * k);
    pb.line_to(46.0 * k, 32.0 * k);
    pb.line_to(-18.0 * k, 32.0 * k);
    pb.close();
    pb.finish().expect("tag")
}

fn render_icon(size: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    let s = size as f32 / 256.0;
    let id = Transform::identity();

    let bg = rounded_rect(8.0 * s, 8.0 * s, 248.0 * s, 248.0 * s, 56.0 * s);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = gradient(s);
    pm.fill_path(&bg, &paint, FillRule::Winding, id, None);

    let mut white = Paint::default();
    white.anti_alias = true;
    white.set_color_rgba8(255, 255, 255, 255);
    let mut ink = Paint::default();
    ink.anti_alias = true;
    ink.set_color_rgba8(0x2a, 0x6e, 0xd0, 255);
    let round = |width: f32| Stroke {
        width: width * s,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };

    let small = size < 32;
    if !small {
        // A page with a folded corner, a small picture and two lines of text.
        let (x0, y0, x1, y1, fold) = (50.0, 36.0, 166.0, 204.0, 34.0);
        let mut pb = PathBuilder::new();
        pb.move_to((x0 + 12.0) * s, y0 * s);
        pb.line_to((x1 - fold) * s, y0 * s);
        pb.line_to(x1 * s, (y0 + fold) * s);
        pb.line_to(x1 * s, (y1 - 12.0) * s);
        pb.quad_to(x1 * s, y1 * s, (x1 - 12.0) * s, y1 * s);
        pb.line_to((x0 + 12.0) * s, y1 * s);
        pb.quad_to(x0 * s, y1 * s, x0 * s, (y1 - 12.0) * s);
        pb.line_to(x0 * s, (y0 + 12.0) * s);
        pb.quad_to(x0 * s, y0 * s, (x0 + 12.0) * s, y0 * s);
        pb.close();
        let page = pb.finish().expect("page");
        pm.fill_path(&page, &white, FillRule::Winding, id, None);
        // Mountains and a sun.
        let mut pb = PathBuilder::new();
        pb.move_to(68.0 * s, 124.0 * s);
        pb.line_to(96.0 * s, 88.0 * s);
        pb.line_to(114.0 * s, 108.0 * s);
        pb.line_to(124.0 * s, 98.0 * s);
        pb.line_to(146.0 * s, 124.0 * s);
        pb.close();
        let hills = pb.finish().expect("hills");
        pm.fill_path(&hills, &ink, FillRule::Winding, id, None);
        let mut pb = PathBuilder::new();
        pb.push_circle(130.0 * s, 74.0 * s, 9.0 * s);
        let sun = pb.finish().expect("sun");
        pm.fill_path(&sun, &ink, FillRule::Winding, id, None);
        for (y, x1) in [(150.0, 140.0), (172.0, 116.0)] {
            let mut pb = PathBuilder::new();
            pb.move_to(70.0 * s, y * s);
            pb.line_to(x1 * s, y * s);
            let line = pb.finish().expect("line");
            pm.stroke_path(&line, &ink, &round(11.0), id, None);
        }
    }

    // The tag: cut out of the page with a ring of background, then filled.
    let (k, cx, cy) = if small {
        (1.55, 128.0, 128.0)
    } else {
        (1.0, 178.0, 176.0)
    };
    let t = Transform::from_rotate(45.0).post_translate(cx * s, cy * s);
    let tag = tag_path(k * s);
    if !small {
        pm.stroke_path(&tag, &paint, &round(26.0), t, None);
    }
    pm.stroke_path(&tag, &white, &round(10.0 * k), t, None);
    pm.fill_path(&tag, &white, FillRule::Winding, t, None);
    let mut pb = PathBuilder::new();
    pb.push_circle(-14.0 * k * s, 0.0, 9.0 * k * s);
    let hole = pb.finish().expect("hole");
    pm.fill_path(&hole, &paint, FillRule::Winding, t, None);
    // An eraser stroke across the tag: the information goes away.
    let mut pb = PathBuilder::new();
    pb.move_to(8.0 * k * s, -14.0 * k * s);
    pb.line_to(30.0 * k * s, -14.0 * k * s);
    pb.move_to(8.0 * k * s, 6.0 * k * s);
    pb.line_to(24.0 * k * s, 6.0 * k * s);
    let strokes = pb.finish().expect("strokes");
    pm.stroke_path(&strokes, &paint, &round(9.0 * k), t, None);

    pm.encode_png().expect("png")
}

fn write_ico(path: &PathBuf, sizes: &[u32]) {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render_icon(s)).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (i, &s) in sizes.iter().enumerate() {
        let b = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[b, b, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(pngs[i].len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += pngs[i].len() as u32;
    }
    for p in &pngs {
        out.extend_from_slice(p);
    }
    fs::write(path, out).expect("write ico");
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out.join("metaforge.ico");
    write_ico(&ico, &[16, 20, 24, 32, 40, 48, 64, 256]);
    let png = render_icon(256);
    fs::write(out.join("icon-256.png"), &png).expect("write png");
    let docs_png = PathBuf::from("docs/assets/metaforge-icon.png");
    if fs::read(&docs_png).ok().as_deref() != Some(&png[..]) {
        let _ = fs::create_dir_all("docs/assets");
        let _ = fs::write(&docs_png, &png);
    }
    // The installer needs the .ico at a fixed place.
    let _ = fs::copy(&ico, "res/metaforge.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ver = env::var("CARGO_PKG_VERSION").expect("version");
    let parts: Vec<u16> = ver
        .split(['.', '-'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    let (ma, mi, pa) = (
        parts[0],
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    let ver4 = format!("{ma}.{mi}.{pa}.0");
    println!("cargo:rerun-if-changed=res/metaforge.manifest.in");
    let manifest = fs::read_to_string("res/metaforge.manifest.in")
        .expect("manifest")
        .replace("@VERSION4@", &ver4);
    let manifest_path = out.join("metaforge.manifest");
    fs::write(&manifest_path, manifest).expect("write manifest");
    let esc = |p: &PathBuf| p.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {ma},{mi},{pa},0
PRODUCTVERSION {ma},{mi},{pa},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Zheer Khalid (zheer.dev)"
      VALUE "FileDescription", "MetaForge metadata viewer and cleaner"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "MetaForge"
      VALUE "LegalCopyright", "Copyright (c) 2026 Zheer Khalid. MIT License."
      VALUE "OriginalFilename", "MetaForge.exe"
      VALUE "ProductName", "MetaForge"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = esc(&ico),
        manifest = esc(&manifest_path),
    );
    let rc_path = out.join("metaforge.rc");
    fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE);
    println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
}
