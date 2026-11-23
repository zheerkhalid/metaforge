#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use metaforge::handoff::{self, Intent};
use metaforge::ui::{self, Options};
use metaforge::{cli, context_menu, settings, sys};
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };

    if has("--help") || has("-h") || has("/?") {
        sys::attach_parent_console();
        println!("{}", cli::USAGE);
        return Ok(());
    }
    if has("--selftest") {
        sys::attach_parent_console();
        let ok = metaforge::selftest::run(|line| println!("{line}"));
        println!(
            "{}",
            if ok {
                "SELFTEST PASSED"
            } else {
                "SELFTEST FAILED"
            }
        );
        std::process::exit(if ok { 0 } else { 1 });
    }
    if has("--show") {
        sys::attach_parent_console();
        std::process::exit(cli::show(&args));
    }
    if has("--clean") {
        sys::attach_parent_console();
        std::process::exit(cli::clean(&args));
    }
    if has("--export") {
        sys::attach_parent_console();
        std::process::exit(cli::export(&args));
    }
    if has("--register-menu") || has("--unregister-menu") {
        sys::attach_parent_console();
        let result = if has("--register-menu") {
            std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| context_menu::register(context_menu::CLASSES, &exe))
        } else {
            context_menu::unregister(context_menu::CLASSES)
        };
        if let Err(e) = &result {
            eprintln!("{e}");
        }
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    // The demo and screenshots: invented files, nothing on this PC is touched.
    if has("--demo") || has("--screenshot") {
        let options = Options {
            demo: true,
            screenshot: value("--screenshot").map(PathBuf::from),
            view: value("--view").or_else(|| std::env::var("METAFORGE_VIEW").ok()),
            light: has("--light"),
            ..Default::default()
        };
        return ui::run(options, window_icon());
    }

    // Paths from Explorer ("--menu-show <path>", "--menu-clean <path>") or
    // dropped on the EXE.
    let clean_now = has("--menu-clean");
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .collect();

    // One window: further copies hand their paths over and exit.
    let Some(_instance) = sys::single_instance("MetaForge.Window") else {
        if !files.is_empty() {
            let intent = if clean_now {
                Intent::Clean
            } else {
                Intent::Show
            };
            let _ = handoff::send(&settings::app_dir(), intent, &files);
        }
        return Ok(());
    };
    ui::run(
        Options {
            files,
            clean_now,
            ..Default::default()
        },
        window_icon(),
    )
}

fn window_icon() -> egui::IconData {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/icon-256.png"));
    match image::load_from_memory(bytes) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData {
                rgba: rgba.into_raw(),
                width,
                height,
            }
        }
        Err(_) => egui::IconData::default(),
    }
}
