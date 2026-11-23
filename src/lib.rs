//! MetaForge: see and remove hidden information in files before sharing them.
//!
//! The logic lives in this library so that it can be unit-tested; `main.rs`
//! only parses the command line.
pub mod cli;
pub mod context_menu;
pub mod demo;
pub mod detect;
pub mod engine;
pub mod formats;
pub mod handoff;
pub mod meta;
pub mod selftest;
pub mod settings;
pub mod sys;
pub mod ui;
pub mod util;
pub mod winmeta;
