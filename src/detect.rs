//! Which format a file is: by its first bytes, then by its extension.

use crate::meta::Kind;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Tiff,
    Gif,
    /// HEIC / HEIF / AVIF (ISO base media "ftyp" with an image brand).
    Heif,
    Docx,
    Xlsx,
    Pptx,
    /// OpenDocument text, spreadsheet, presentation, drawing.
    Odf,
    Pdf,
    Rtf,
    Mp3,
    Flac,
    /// Ogg Vorbis / Opus.
    Ogg,
    Wav,
    /// MP4 audio (.m4a, .m4b).
    M4a,
    /// MP4 / M4V / 3GP video.
    Mp4,
    /// QuickTime .mov.
    Mov,
    /// Anything else: only Windows traces are shown.
    Other,
}

impl Format {
    pub fn kind(self) -> Kind {
        match self {
            Format::Jpeg
            | Format::Png
            | Format::Webp
            | Format::Tiff
            | Format::Gif
            | Format::Heif => Kind::Image,
            Format::Docx
            | Format::Xlsx
            | Format::Pptx
            | Format::Odf
            | Format::Pdf
            | Format::Rtf => Kind::Document,
            Format::Mp3 | Format::Flac | Format::Ogg | Format::Wav | Format::M4a => Kind::Audio,
            Format::Mp4 | Format::Mov => Kind::Video,
            Format::Other => Kind::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG image",
            Format::Png => "PNG image",
            Format::Webp => "WebP image",
            Format::Tiff => "TIFF image",
            Format::Gif => "GIF image",
            Format::Heif => "HEIC / AVIF image",
            Format::Docx => "Word document",
            Format::Xlsx => "Excel workbook",
            Format::Pptx => "PowerPoint presentation",
            Format::Odf => "OpenDocument file",
            Format::Pdf => "PDF document",
            Format::Rtf => "Rich Text document",
            Format::Mp3 => "MP3 audio",
            Format::Flac => "FLAC audio",
            Format::Ogg => "Ogg audio",
            Format::Wav => "WAV audio",
            Format::M4a => "M4A audio",
            Format::Mp4 => "MP4 video",
            Format::Mov => "QuickTime video",
            Format::Other => "File",
        }
    }

    /// Short tag for lists and filters.
    pub fn short(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::Webp => "WebP",
            Format::Tiff => "TIFF",
            Format::Gif => "GIF",
            Format::Heif => "HEIF",
            Format::Docx => "DOCX",
            Format::Xlsx => "XLSX",
            Format::Pptx => "PPTX",
            Format::Odf => "ODF",
            Format::Pdf => "PDF",
            Format::Rtf => "RTF",
            Format::Mp3 => "MP3",
            Format::Flac => "FLAC",
            Format::Ogg => "OGG",
            Format::Wav => "WAV",
            Format::M4a => "M4A",
            Format::Mp4 => "MP4",
            Format::Mov => "MOV",
            Format::Other => "FILE",
        }
    }

    /// Whether MetaForge understands the inside of this format.
    pub fn supported(self) -> bool {
        self != Format::Other
    }
}

/// Extensions MetaForge looks at when a folder is added.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "webp", "tif", "tiff", "dng", "gif", "heic", "heif",
    "avif", "docx", "docm", "dotx", "xlsx", "xlsm", "xltx", "pptx", "pptm", "potx", "odt", "ods",
    "odp", "odg", "ott", "pdf", "rtf", "mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "m4b",
    "mp4", "m4v", "3gp", "mov",
];

fn by_extension(ext: &str) -> Format {
    match ext {
        "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
        "png" => Format::Png,
        "webp" => Format::Webp,
        "tif" | "tiff" | "dng" => Format::Tiff,
        "gif" => Format::Gif,
        "heic" | "heif" | "avif" => Format::Heif,
        "docx" | "docm" | "dotx" => Format::Docx,
        "xlsx" | "xlsm" | "xltx" => Format::Xlsx,
        "pptx" | "pptm" | "potx" => Format::Pptx,
        "odt" | "ods" | "odp" | "odg" | "ott" => Format::Odf,
        "pdf" => Format::Pdf,
        "rtf" => Format::Rtf,
        "mp3" => Format::Mp3,
        "flac" => Format::Flac,
        "ogg" | "oga" | "opus" => Format::Ogg,
        "wav" => Format::Wav,
        "m4a" | "m4b" => Format::M4a,
        "mp4" | "m4v" | "3gp" => Format::Mp4,
        "mov" => Format::Mov,
        _ => Format::Other,
    }
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Detects from the first bytes, using the extension to tell apart formats
/// that share a container (ZIP-based documents, MP4 audio and video).
pub fn detect_bytes(head: &[u8], ext: &str) -> Format {
    let by_ext = by_extension(ext);
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Format::Jpeg;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Format::Png;
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Format::Webp;
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return Format::Wav;
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return Format::Tiff;
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Format::Gif;
    }
    if starts(b"%PDF-")
        || (head.len() > 5
            && head[..head.len().min(1024)]
                .windows(5)
                .any(|w| w == b"%PDF-"))
    {
        return Format::Pdf;
    }
    if starts(b"{\\rtf") {
        return Format::Rtf;
    }
    if starts(b"fLaC") {
        return Format::Flac;
    }
    if starts(b"OggS") {
        return Format::Ogg;
    }
    if starts(b"ID3") {
        // ID3 tags also front FLAC files now and then; trust the extension then.
        return if by_ext == Format::Flac {
            Format::Flac
        } else {
            Format::Mp3
        };
    }
    if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 && by_ext == Format::Mp3 {
        return Format::Mp3;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        let brands: Vec<&[u8]> = std::iter::once(brand)
            .chain(
                head.get(16..head.len().min(64))
                    .unwrap_or(&[])
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| &c[..]),
            )
            .collect();
        let has = |b: &[u8]| brands.contains(&b);
        let image_brand = [b"heic", b"heix", b"mif1", b"avif", b"heim", b"msf1"]
            .iter()
            .any(|b| has(&b[..]));
        if image_brand
            && (!matches!(by_ext, Format::Mp4 | Format::Mov | Format::M4a) || brand != b"mp42")
        {
            return Format::Heif;
        }
        if brand == b"qt  " {
            return Format::Mov;
        }
        if brand == b"M4A " || brand == b"M4B " || by_ext == Format::M4a {
            return Format::M4a;
        }
        return if by_ext == Format::Mov {
            Format::Mov
        } else {
            Format::Mp4
        };
    }
    if head.len() >= 8
        && matches!(&head[4..8], b"moov" | b"wide" | b"mdat" | b"free" | b"skip")
        && by_ext == Format::Mov
    {
        return Format::Mov;
    }
    if starts(b"PK\x03\x04") {
        return match by_ext {
            Format::Docx | Format::Xlsx | Format::Pptx | Format::Odf => by_ext,
            _ => Format::Other,
        };
    }
    Format::Other
}

/// Detects a file on disk (reads the first 4 KB).
pub fn detect(path: &Path) -> Format {
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    head.truncate(n);
    detect_bytes(&head, &extension(path))
}

/// Whether a path's extension is one MetaForge reads (for folder walks).
pub fn known_extension(path: &Path) -> bool {
    EXTENSIONS.contains(&extension(path).as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_numbers() {
        assert_eq!(detect_bytes(&[0xFF, 0xD8, 0xFF, 0xE1], "bin"), Format::Jpeg);
        assert_eq!(detect_bytes(b"\x89PNG\r\n\x1a\n....", ""), Format::Png);
        assert_eq!(detect_bytes(b"RIFF\0\0\0\0WEBPVP8 ", ""), Format::Webp);
        assert_eq!(detect_bytes(b"RIFF\0\0\0\0WAVEfmt ", ""), Format::Wav);
        assert_eq!(detect_bytes(b"II*\0\x08\0\0\0", "tif"), Format::Tiff);
        assert_eq!(detect_bytes(b"GIF89a", ""), Format::Gif);
        assert_eq!(detect_bytes(b"%PDF-1.7\n", ""), Format::Pdf);
        assert_eq!(detect_bytes(b"{\\rtf1\\ansi", ""), Format::Rtf);
        assert_eq!(detect_bytes(b"fLaC\0\0\0\x22", ""), Format::Flac);
        assert_eq!(detect_bytes(b"ID3\x04\0\0", "mp3"), Format::Mp3);
        assert_eq!(detect_bytes(b"PK\x03\x04", "docx"), Format::Docx);
        assert_eq!(detect_bytes(b"PK\x03\x04", "zip"), Format::Other);
        assert_eq!(
            detect_bytes(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic", "heic"),
            Format::Heif
        );
        assert_eq!(
            detect_bytes(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1", "avif"),
            Format::Heif
        );
        assert_eq!(
            detect_bytes(b"\0\0\0\x14ftypqt  \0\0\0\0qt  ", "mov"),
            Format::Mov
        );
        assert_eq!(
            detect_bytes(b"\0\0\0\x18ftypisom\0\0\0\0isomiso2", "mp4"),
            Format::Mp4
        );
        assert_eq!(
            detect_bytes(b"\0\0\0\x18ftypM4A \0\0\0\0M4A mp42", "m4a"),
            Format::M4a
        );
        assert_eq!(detect_bytes(b"hello", "txt"), Format::Other);
        // Short headers must not panic.
        assert_eq!(detect_bytes(b"\0\0\0\x0cftypqt  ", "mov"), Format::Mov);
        for n in 0..20 {
            let _ = detect_bytes(&b"\0\0\0\x18ftypheic\0\0\0\0mif1"[..n.min(20)], "heic");
        }
    }
}
