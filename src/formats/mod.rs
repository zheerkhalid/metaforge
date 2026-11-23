//! Format modules and the dispatch between them.
//!
//! Every format module has the same four functions:
//!
//! ```ignore
//! pub fn read(path: &Path) -> Result<Extract>;
//! pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()>;
//! pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()>;
//! pub fn sample() -> Vec<u8>; // a small file with invented metadata, for tests and --selftest
//! ```
//!
//! `src` and `dst` are always different paths; the engine writes to a
//! temporary file and moves it into place. Modules never touch `src`.

pub mod audio;
pub mod exif;
pub mod gif;
pub mod heif;
pub mod icc;
pub mod imgutil;
pub mod iptc;
pub mod jpeg;
pub mod mp4;
pub mod odf;
pub mod ooxml;
pub mod pdf;
pub mod png;
pub mod rtf;
pub mod tiff;
pub mod webp;
pub mod xmlutil;
pub mod xmp;
pub mod zipx;

use crate::detect::Format;
use crate::meta::{CleanOptions, Edit, Extract, Result};
use std::path::Path;

pub fn read(format: Format, path: &Path) -> Result<Extract> {
    match format {
        Format::Jpeg => jpeg::read(path),
        Format::Png => png::read(path),
        Format::Webp => webp::read(path),
        Format::Tiff => tiff::read(path),
        Format::Gif => gif::read(path),
        Format::Heif => heif::read(path),
        Format::Docx | Format::Xlsx | Format::Pptx => ooxml::read(path),
        Format::Odf => odf::read(path),
        Format::Pdf => pdf::read(path),
        Format::Rtf => rtf::read(path),
        Format::Mp3 | Format::Flac | Format::Ogg | Format::Wav | Format::M4a => audio::read(path),
        Format::Mp4 | Format::Mov => mp4::read(path),
        Format::Other => Ok(Extract::default()),
    }
}

pub fn clean(format: Format, src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    match format {
        Format::Jpeg => jpeg::clean(src, dst, opts),
        Format::Png => png::clean(src, dst, opts),
        Format::Webp => webp::clean(src, dst, opts),
        Format::Tiff => tiff::clean(src, dst, opts),
        Format::Gif => gif::clean(src, dst, opts),
        Format::Heif => heif::clean(src, dst, opts),
        Format::Docx | Format::Xlsx | Format::Pptx => ooxml::clean(src, dst, opts),
        Format::Odf => odf::clean(src, dst, opts),
        Format::Pdf => pdf::clean(src, dst, opts),
        Format::Rtf => rtf::clean(src, dst, opts),
        Format::Mp3 | Format::Flac | Format::Ogg | Format::Wav | Format::M4a => {
            audio::clean(src, dst, opts)
        }
        Format::Mp4 | Format::Mov => mp4::clean(src, dst, opts),
        Format::Other => std::fs::copy(src, dst)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

pub fn edit(format: Format, src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    match format {
        Format::Jpeg => jpeg::edit(src, dst, edits),
        Format::Png => png::edit(src, dst, edits),
        Format::Webp => webp::edit(src, dst, edits),
        Format::Tiff => tiff::edit(src, dst, edits),
        Format::Gif => gif::edit(src, dst, edits),
        Format::Heif => heif::edit(src, dst, edits),
        Format::Docx | Format::Xlsx | Format::Pptx => ooxml::edit(src, dst, edits),
        Format::Odf => odf::edit(src, dst, edits),
        Format::Pdf => pdf::edit(src, dst, edits),
        Format::Rtf => rtf::edit(src, dst, edits),
        Format::Mp3 | Format::Flac | Format::Ogg | Format::Wav | Format::M4a => {
            audio::edit(src, dst, edits)
        }
        Format::Mp4 | Format::Mov => mp4::edit(src, dst, edits),
        Format::Other => Err("This kind of file cannot be edited.".into()),
    }
}
