//! Audio files: MP3, FLAC, Ogg, WAV (through lofty) and M4A (through the MP4
//! box reader in [`super::mp4`]).
//!
//! Every tag in the file is listed: ID3v2 frame by frame (so private frames,
//! unique file ids and embedded objects are visible too), ID3v1, APE, Vorbis
//! comments, RIFF INFO and, for WAV, the broadcast `bext` chunk and iXML /
//! XMP chunks. Cleaning only touches tags; the audio data stays byte for byte.

use crate::detect::Format;
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Kind, Result};
use crate::util::DateTime;
use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{FileType, TaggedFile};
use lofty::id3::v2::{Frame, FrameId, Id3v2Tag, TextInformationFrame};
use lofty::prelude::*;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};
use lofty::TextEncoding;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The keys audio files can edit.
pub(crate) const AUDIO_KEYS: [EditKey; 8] = [
    EditKey::Title,
    EditKey::Author,
    EditKey::Album,
    EditKey::Genre,
    EditKey::Track,
    EditKey::Year,
    EditKey::Description,
    EditKey::Copyright,
];

fn lofty_err(e: impl std::fmt::Display) -> String {
    format!("This audio file could not be read ({e}).")
}

fn write_err(e: impl std::fmt::Display) -> String {
    format!("The tags could not be written ({e}).")
}

fn io_err(e: std::io::Error) -> String {
    format!("The file could not be read: {e}")
}

fn is_m4a(path: &Path) -> bool {
    matches!(
        crate::detect::detect(path),
        Format::M4a | Format::Mp4 | Format::Mov
    )
}

fn open_tagged(path: &Path) -> Result<TaggedFile> {
    lofty::probe::Probe::open(path)
        .map_err(lofty_err)?
        .guess_file_type()
        .map_err(lofty_err)?
        .read()
        .map_err(lofty_err)
}

/// The ID3v2 tag with all its frames (the generic lofty tag drops frames it
/// cannot convert, such as PRIV and GEOB).
fn read_id3v2(path: &Path, ft: FileType) -> Option<Id3v2Tag> {
    let mut f = File::open(path).ok()?;
    let po = ParseOptions::new().read_properties(false);
    match ft {
        FileType::Mpeg => lofty::mpeg::MpegFile::read_from(&mut f, po)
            .ok()?
            .remove_id3v2(),
        FileType::Wav => lofty::iff::wav::WavFile::read_from(&mut f, po)
            .ok()?
            .remove_id3v2(),
        FileType::Flac => lofty::flac::FlacFile::read_from(&mut f, po)
            .ok()?
            .remove_id3v2(),
        FileType::Aiff => lofty::iff::aiff::AiffFile::read_from(&mut f, po)
            .ok()?
            .remove_id3v2(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Pictures.

/// A sample JPEG header: 600 x 600, no image data.
pub(crate) fn tiny_jpeg() -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
    v.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    v.extend_from_slice(&[
        0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x58, 0x02, 0x58, 0x03, 0x01, 0x22, 0x00, 0x02, 0x11,
        0x01, 0x03, 0x11, 0x01, 0xFF, 0xD9,
    ]);
    v
}

fn picture_dims(d: &[u8]) -> Option<(&'static str, Option<(u32, u32)>)> {
    let be16 = |i: usize| {
        d.get(i..i + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as u32)
    };
    let be32 = |i: usize| {
        d.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    if d.starts_with(b"\x89PNG") {
        return Some(("PNG", be32(16).zip(be32(20))));
    }
    if d.starts_with(b"GIF8") && d.len() >= 10 {
        let le = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]) as u32;
        return Some(("GIF", Some((le(6), le(8)))));
    }
    if d.starts_with(b"BM") {
        return Some(("BMP", None));
    }
    if d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WEBP" {
        return Some(("WebP", None));
    }
    if d.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= d.len() {
            if d[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = d[i + 1];
            if m == 0xFF {
                i += 1;
                continue;
            }
            if matches!(m, 0xD0..=0xD9 | 0x01) {
                i += 2;
                continue;
            }
            let len = be16(i + 2)? as usize;
            if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                let (h, w) = (be16(i + 5)?, be16(i + 7)?);
                return Some(("JPEG", Some((w, h))));
            }
            i += 2 + len;
        }
        return Some(("JPEG", None));
    }
    None
}

/// "Cover picture (JPEG, 600 x 600)".
pub(crate) fn picture_label(data: &[u8]) -> String {
    match picture_dims(data) {
        Some((kind, Some((w, h)))) => format!("Cover picture ({kind}, {w} x {h})"),
        Some((kind, None)) => format!("Cover picture ({kind}, {} bytes)", data.len()),
        None => format!("Cover picture ({} bytes)", data.len()),
    }
}

// ---------------------------------------------------------------------------
// Classification.

struct Class {
    label: String,
    category: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
}

fn class(label: &str, category: Category, flag: Option<Flag>, edit: Option<EditKey>) -> Class {
    Class {
        label: label.to_string(),
        category,
        flag,
        edit,
    }
}

/// An ID3v2 text frame: id, label, category, flag, edit key.
type TextFrameInfo = (
    &'static str,
    &'static str,
    Category,
    Option<Flag>,
    Option<EditKey>,
);

/// ID3v2 text frames.
#[rustfmt::skip]
const ID3_TEXT: &[TextFrameInfo] = {
    use Category::*;
    const N: Option<Flag> = Some(Flag::Names);
    &[
        ("TIT2", "Title", Description, None, Some(EditKey::Title)),
        ("TIT1", "Content group", Description, None, None),
        ("TIT3", "Subtitle", Description, None, None),
        ("TALB", "Album", Description, None, Some(EditKey::Album)),
        ("TPE1", "Artist", People, N, Some(EditKey::Author)),
        ("TPE2", "Album artist", People, N, None),
        ("TPE3", "Conductor", People, N, None),
        ("TPE4", "Remixed by", People, N, None),
        ("TCOM", "Composer", People, N, None),
        ("TEXT", "Lyricist", People, N, None),
        ("TOLY", "Original lyricist", People, N, None),
        ("TOPE", "Original artist", People, N, None),
        ("TENC", "Encoded by", People, N, None),
        ("TPUB", "Publisher", People, N, None),
        ("TOWN", "File owner", People, N, None),
        ("TRSO", "Radio station owner", People, N, None),
        ("TCOP", "Copyright", Description, None, Some(EditKey::Copyright)),
        ("TCON", "Genre", Description, None, Some(EditKey::Genre)),
        ("TRCK", "Track", Description, None, Some(EditKey::Track)),
        ("TPOS", "Disc", Description, None, None),
        ("TDRC", "Recording date", Dates, None, Some(EditKey::Year)),
        ("TYER", "Year", Dates, None, Some(EditKey::Year)),
        ("TDAT", "Recording day", Dates, None, None),
        ("TIME", "Recording time", Dates, None, None),
        ("TORY", "Original year", Dates, None, None),
        ("TDOR", "Original release date", Dates, None, None),
        ("TDRL", "Release date", Dates, None, None),
        ("TDEN", "Encoding date", Dates, None, None),
        ("TDTG", "Tagging date", Dates, None, None),
        ("TSSE", "Encoder settings", Software, None, None),
        ("TSRC", "ISRC code", Hidden, Some(Flag::Serial), None),
        ("TOFN", "Original file name", Hidden, Some(Flag::Hidden), None),
    ]
};

fn id3_text_class(id: &str) -> Class {
    match ID3_TEXT.iter().find(|t| t.0 == id) {
        Some(&(_, label, cat, flag, edit)) => class(label, cat, flag, edit),
        None => class(&format!("Text ({id})"), Category::Description, None, None),
    }
}

/// One ID3v2 frame as (id suffix, class, value).
fn id3_frame(frame: &Frame<'_>) -> (String, Class, String) {
    use Category::*;
    let id = frame.id_str().to_string();
    match frame {
        Frame::Text(t) => (id.clone(), id3_text_class(&id), t.value.replace('\0', "; ")),
        Frame::Timestamp(t) => (id.clone(), id3_text_class(&id), t.timestamp.to_string()),
        Frame::UserText(t) => {
            let d = t.description.as_str();
            let low = d.to_ascii_lowercase();
            let c = if low.starts_with("musicbrainz") || low.starts_with("acoustid") {
                class(
                    &format!("Database id ({d})"),
                    Hidden,
                    Some(Flag::Serial),
                    None,
                )
            } else if low.contains("location") || low.contains("gps") {
                class(
                    &format!("Place ({d})"),
                    Location,
                    Some(Flag::Location),
                    None,
                )
            } else {
                class(&format!("Custom text ({d})"), Description, None, None)
            };
            (format!("TXXX:{d}"), c, t.content.clone())
        }
        Frame::Comment(c) if c.description.is_empty() => (
            "COMM".into(),
            class(
                "Comment",
                Description,
                Some(Flag::Comments),
                Some(EditKey::Description),
            ),
            c.content.clone(),
        ),
        Frame::Comment(c) => {
            let flag = (!c.description.starts_with("iTun")).then_some(Flag::Comments);
            let label = format!("Comment ({})", c.description);
            (
                format!("COMM:{}", c.description),
                class(&label, Description, flag, None),
                c.content.clone(),
            )
        }
        Frame::UnsynchronizedText(u) => (
            "USLT".into(),
            class("Lyrics", Description, None, None),
            u.content.clone(),
        ),
        Frame::Url(u) => (
            id.clone(),
            class(&format!("Web link ({id})"), Description, None, None),
            u.url().to_string(),
        ),
        Frame::UserUrl(u) => {
            let label = format!("Web link ({})", u.description);
            (
                format!("WXXX:{}", u.description),
                class(&label, Description, None, None),
                u.content.clone(),
            )
        }
        Frame::Picture(p) => (
            "APIC".into(),
            class("Cover picture", Hidden, Some(Flag::Hidden), None),
            picture_label(p.picture.data()),
        ),
        Frame::Popularimeter(p) if !p.email.is_empty() => (
            "POPM".into(),
            class("Rated by", People, Some(Flag::Names), None),
            format!("{} (rating {})", p.email, p.rating),
        ),
        Frame::Popularimeter(p) => (
            "POPM".into(),
            class("Rating", Description, None, None),
            p.rating.to_string(),
        ),
        Frame::KeyValue(k) => {
            let v = k
                .key_value_pairs
                .iter()
                .map(|(a, b)| format!("{a}: {b}"))
                .collect::<Vec<_>>();
            (
                id,
                class("Involved people", People, Some(Flag::Names), None),
                v.join("; "),
            )
        }
        Frame::UniqueFileIdentifier(u) => {
            let value = if u
                .identifier
                .iter()
                .all(|c| c.is_ascii_graphic() || *c == b' ')
            {
                String::from_utf8_lossy(&u.identifier).into_owned()
            } else {
                u.identifier.iter().map(|b| format!("{b:02x}")).collect()
            };
            let label = format!("Unique file id ({})", u.owner);
            (
                format!("UFID:{}", u.owner),
                class(&label, Hidden, Some(Flag::Serial), None),
                value,
            )
        }
        Frame::Private(p) => {
            let label = format!("Private data ({})", p.owner);
            let value = format!("{} bytes", p.private_data.len());
            (
                format!("PRIV:{}", p.owner),
                class(&label, Hidden, Some(Flag::Hidden), None),
                value,
            )
        }
        Frame::Binary(b) if id == "GEOB" => (
            id,
            class("Embedded object", Hidden, Some(Flag::Hidden), None),
            format!("{} bytes", b.data.len()),
        ),
        Frame::Binary(b) => {
            let label = format!("Other data ({id})");
            (
                id,
                class(&label, Hidden, None, None),
                format!("{} bytes", b.data.len()),
            )
        }
        _ => (
            id.clone(),
            class(&format!("Other data ({id})"), Hidden, None, None),
            "present".into(),
        ),
    }
}

/// What a generic lofty item means.
fn item_class(key: &ItemKey) -> Class {
    use Category::*;
    use ItemKey as K;
    let n = Some(Flag::Names);
    let human = || {
        let dbg = format!("{key:?}");
        let mut s = String::new();
        for (i, ch) in dbg.chars().enumerate() {
            if ch.is_ascii_uppercase() && i > 0 {
                s.push(' ');
                s.push(ch.to_ascii_lowercase());
            } else {
                s.push(ch);
            }
        }
        s
    };
    match key {
        K::TrackTitle => class("Title", Description, None, Some(EditKey::Title)),
        K::TrackArtist | K::TrackArtists => class("Artist", People, n, Some(EditKey::Author)),
        K::AlbumTitle => class("Album", Description, None, Some(EditKey::Album)),
        K::Genre => class("Genre", Description, None, Some(EditKey::Genre)),
        K::TrackNumber => class("Track", Description, None, Some(EditKey::Track)),
        K::RecordingDate => class("Recording date", Dates, None, Some(EditKey::Year)),
        K::Year => class("Year", Dates, None, Some(EditKey::Year)),
        K::Comment => class(
            "Comment",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        ),
        K::CopyrightMessage => class("Copyright", Description, None, Some(EditKey::Copyright)),
        K::Description | K::PodcastDescription => {
            class("Description", Description, Some(Flag::Comments), None)
        }
        K::AlbumArtist
        | K::Composer
        | K::Conductor
        | K::Lyricist
        | K::Writer
        | K::Arranger
        | K::Engineer
        | K::MixDj
        | K::MixEngineer
        | K::Producer
        | K::Performer
        | K::Remixer
        | K::Director
        | K::MusicianCredits
        | K::OriginalArtist
        | K::OriginalLyricist
        | K::Publisher
        | K::Label
        | K::EncodedBy
        | K::FileOwner
        | K::InternetRadioStationOwner
        | K::Popularimeter => class(&human(), People, n, None),
        K::ReleaseDate | K::OriginalReleaseDate | K::TaggingTime | K::EncodingTime => {
            class(&human(), Dates, None, None)
        }
        K::EncoderSoftware => class("Program used", Software, None, None),
        K::EncoderSettings => class("Encoder settings", Software, None, None),
        K::Isrc | K::Barcode | K::CatalogNumber | K::PodcastGlobalUniqueId | K::AppleXid => {
            class(&human(), Hidden, Some(Flag::Serial), None)
        }
        K::OriginalFileName => class("Original file name", Hidden, Some(Flag::Hidden), None),
        K::Unknown(u) => {
            let low = u.to_ascii_lowercase();
            if low.contains("location") || low.contains("gps") || low == "iarl" {
                class(
                    &format!("Place ({u})"),
                    Location,
                    Some(Flag::Location),
                    None,
                )
            } else if low.starts_with("musicbrainz") || low.starts_with("acoustid") {
                class(
                    &format!("Database id ({u})"),
                    Hidden,
                    Some(Flag::Serial),
                    None,
                )
            } else if low.contains("encoder") || low == "isft" {
                class("Program used", Software, None, None)
            } else if matches!(
                low.as_str(),
                "contact" | "iown" | "ieng" | "itch" | "author"
            ) {
                class(&format!("Person ({u})"), People, n, None)
            } else {
                class(u, Description, None, None)
            }
        }
        k if format!("{k:?}").starts_with("MusicBrainz") => {
            class(&human(), Hidden, Some(Flag::Serial), None)
        }
        _ => class(&human(), Description, None, None),
    }
}

fn tag_source(tt: TagType) -> (&'static str, &'static str) {
    match tt {
        TagType::Id3v1 => ("id3v1", "ID3v1"),
        TagType::Id3v2 => ("id3", "ID3v2"),
        TagType::Ape => ("ape", "APE tag"),
        TagType::VorbisComments => ("vorbis", "Vorbis comment"),
        TagType::RiffInfo => ("riff", "RIFF INFO"),
        TagType::Mp4Ilst => ("mp4:ilst", "MP4 tags"),
        TagType::AiffText => ("aiff", "AIFF text"),
        _ => ("tag", "Tag"),
    }
}

fn item_id(tt: TagType, key: &ItemKey) -> String {
    let (prefix, _) = tag_source(tt);
    let native = key
        .map_key(tt, true)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{key:?}"));
    format!("{prefix}:{native}")
}

fn item_value(item: &TagItem) -> String {
    match item.value() {
        ItemValue::Text(s) | ItemValue::Locator(s) => s.clone(),
        ItemValue::Binary(b) => format!("{} bytes of data", b.len()),
    }
}

fn make_field(id: String, c: Class, value: String, source: &str) -> Field {
    let mut f = Field::new(id, c.label, value, c.category, source);
    f.flag = c.flag;
    f.edit = c.edit;
    f
}

/// Makes ids unique by appending `#2`, `#3`, ...
fn dedupe(fields: &mut [Field]) {
    let mut seen: Vec<String> = Vec::new();
    for f in fields.iter_mut() {
        let base = f.id.clone();
        let mut n = 1;
        while seen.contains(&f.id) {
            n += 1;
            f.id = format!("{base}#{n}");
        }
        seen.push(f.id.clone());
    }
}

// ---------------------------------------------------------------------------
// WAV chunks lofty does not handle: bext, iXML, XMP, aXML.

struct Chunk {
    id: [u8; 4],
    /// Offset of the chunk header.
    start: u64,
    size: u64,
}

fn wav_chunks(f: &mut File) -> Result<Vec<Chunk>> {
    let len = f.metadata().map_err(io_err)?.len();
    let mut head = [0u8; 12];
    f.seek(SeekFrom::Start(0)).map_err(io_err)?;
    f.read_exact(&mut head)
        .map_err(|_| "This is not a WAV file.".to_string())?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err("This is not a WAV file.".into());
    }
    let mut out = Vec::new();
    let mut pos = 12u64;
    while pos + 8 <= len {
        let mut h = [0u8; 8];
        f.seek(SeekFrom::Start(pos)).map_err(io_err)?;
        f.read_exact(&mut h).map_err(io_err)?;
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
        if pos + 8 + size > len {
            return Err("The WAV file is damaged or incomplete.".into());
        }
        out.push(Chunk {
            id: [h[0], h[1], h[2], h[3]],
            start: pos,
            size,
        });
        pos += 8 + size + (size & 1);
    }
    Ok(out)
}

/// A bext field: offset, length, id, label, category, flag.
type BextInfo = (
    usize,
    usize,
    &'static str,
    &'static str,
    Category,
    Option<Flag>,
);

/// bext fields.
#[rustfmt::skip]
const BEXT: &[BextInfo] = &[
    (0, 256, "Description", "Description", Category::Description, Some(Flag::Comments)),
    (256, 32, "Originator", "Recorder (originator)", Category::Device, None),
    (288, 32, "OriginatorReference", "Recorder reference", Category::Device, Some(Flag::Serial)),
    (320, 18, "OriginationDate", "Recorded on", Category::Dates, None),
    (348, 64, "UMID", "Unique material id", Category::Hidden, Some(Flag::Serial)),
];

fn bext_fields(body: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    let text = |b: &[u8]| {
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        String::from_utf8_lossy(&b[..end]).trim().to_string()
    };
    for &(off, len, id, label, cat, flag) in BEXT {
        let Some(raw) = body.get(off..off + len) else {
            continue;
        };
        let value = match id {
            "UMID" if raw.iter().any(|&c| c != 0) => {
                raw.iter().map(|b| format!("{b:02x}")).collect()
            }
            "UMID" => String::new(),
            "OriginationDate" => {
                let s = format!("{} {}", text(&raw[..10]), text(&raw[10..]));
                DateTime::parse(&s)
                    .map(|d| d.text())
                    .unwrap_or(s.trim().to_string())
            }
            _ => text(raw),
        };
        if !value.is_empty() {
            let mut f = Field::new(
                format!("bext:{id}"),
                label,
                value,
                cat,
                "Broadcast WAV (bext)",
            );
            f.flag = flag;
            out.push(f);
        }
    }
    if let Some(hist) = body.get(602..).map(text).filter(|s| !s.is_empty()) {
        out.push(Field::new(
            "bext:CodingHistory",
            "Coding history",
            hist,
            Category::Software,
            "Broadcast WAV (bext)",
        ));
    }
    out
}

fn xml_chunk_field(id: [u8; 4], size: u64) -> Option<Field> {
    let (key, label) = match &id {
        b"iXML" => ("iXML", "iXML production data"),
        b"_PMX" => ("XMP", "XMP metadata"),
        b"axml" => ("aXML", "aXML metadata"),
        _ => return None,
    };
    Some(
        Field::new(
            format!("wav:{key}"),
            label,
            format!("{size} bytes"),
            Category::Hidden,
            "WAV chunk",
        )
        .flag(Flag::Hidden),
    )
}

fn wav_extra(path: &Path) -> Result<Vec<Field>> {
    let mut f = File::open(path).map_err(io_err)?;
    let mut out = Vec::new();
    for c in wav_chunks(&mut f)? {
        if &c.id == b"bext" && c.size <= 1 << 20 {
            let mut body = vec![0u8; c.size as usize];
            f.seek(SeekFrom::Start(c.start + 8)).map_err(io_err)?;
            f.read_exact(&mut body).map_err(io_err)?;
            out.extend(bext_fields(&body));
        } else if let Some(field) = xml_chunk_field(c.id, c.size) {
            out.push(field);
        }
    }
    Ok(out)
}

/// Copies a WAV file chunk by chunk, dropping / blanking what `opts` removes
/// from the bext, iXML, XMP and aXML chunks.
fn wav_rewrite(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let mut f = File::open(src).map_err(io_err)?;
    let chunks = wav_chunks(&mut f)?;
    let mut out = std::io::BufWriter::new(File::create(dst).map_err(write_err)?);
    out.write_all(b"RIFF\0\0\0\0WAVE").map_err(write_err)?;
    let mut total = 4u64;
    for c in chunks {
        let padded = c.size + (c.size & 1);
        let drop = match &c.id {
            b"bext" => opts.is_everything(),
            b"iXML" | b"_PMX" | b"axml" => opts.removes(Category::Hidden),
            _ => false,
        };
        if drop {
            continue;
        }
        f.seek(SeekFrom::Start(c.start)).map_err(io_err)?;
        if &c.id == b"bext" && c.size <= 1 << 20 {
            let mut buf = vec![0u8; 8 + padded as usize];
            f.read_exact(&mut buf).map_err(io_err)?;
            let body = &mut buf[8..];
            for &(off, len, _, _, cat, _) in BEXT {
                if opts.removes(cat) {
                    if let Some(r) = body.get_mut(off..off + len) {
                        r.fill(0);
                    }
                }
            }
            if opts.removes(Category::Software) && body.len() > 602 {
                let end = c.size as usize;
                body[602..end].fill(0);
            }
            out.write_all(&buf).map_err(write_err)?;
        } else {
            let copied =
                std::io::copy(&mut (&mut f).take(8 + padded), &mut out).map_err(write_err)?;
            if copied != 8 + padded {
                return Err("The WAV file is damaged or incomplete.".into());
            }
        }
        total += 8 + padded;
    }
    let mut file = out.into_inner().map_err(|e| write_err(e.error()))?;
    let riff_size = u32::try_from(total).map_err(|_| "The WAV file is too large.".to_string())?;
    file.seek(SeekFrom::Start(4)).map_err(write_err)?;
    file.write_all(&riff_size.to_le_bytes())
        .map_err(write_err)?;
    file.sync_all().map_err(write_err)
}

// ---------------------------------------------------------------------------
// Read.

pub fn read(path: &Path) -> Result<Extract> {
    if is_m4a(path) {
        return crate::formats::mp4::read_as(path, Kind::Audio);
    }
    let tagged = open_tagged(path)?;
    let ft = tagged.file_type();
    let mut fields = Vec::new();
    if let Some(id3) = read_id3v2(path, ft) {
        for frame in &id3 {
            let (id, c, value) = id3_frame(frame);
            let value = value.trim().to_string();
            if !value.is_empty() {
                fields.push(make_field(format!("id3:{id}"), c, value, "ID3v2"));
            }
        }
    }
    for tag in tagged.tags() {
        let tt = tag.tag_type();
        if tt == TagType::Id3v2 {
            continue;
        }
        let (prefix, source) = tag_source(tt);
        for item in tag.items() {
            let value = item_value(item).trim().to_string();
            if value.is_empty() {
                continue;
            }
            fields.push(make_field(
                item_id(tt, item.key()),
                item_class(item.key()),
                value,
                source,
            ));
        }
        for p in tag.pictures() {
            let c = class("Cover picture", Category::Hidden, Some(Flag::Hidden), None);
            fields.push(make_field(
                format!("{prefix}:picture"),
                c,
                picture_label(p.data()),
                source,
            ));
        }
    }
    if ft == FileType::Wav {
        fields.extend(wav_extra(path)?);
    }
    dedupe(&mut fields);
    let taken = fields
        .iter()
        .find(|f| f.edit == Some(EditKey::Year))
        .or_else(|| fields.iter().find(|f| f.id == "bext:OriginationDate"))
        .and_then(|f| crate::formats::mp4::taken_text(&f.value));
    // Intrinsic facts.
    let p = tagged.properties();
    let tech = |id: &str, label: &str, v: String| {
        Field::new(id, label, v, Category::Technical, "Audio stream").fixed()
    };
    let secs = p.duration().as_secs_f64();
    if secs > 0.0 {
        fields.push(tech(
            "audio:duration",
            "Duration",
            crate::formats::mp4::duration_text(secs),
        ));
    }
    if let Some(b) = p.audio_bitrate().filter(|&b| b > 0) {
        fields.push(tech("audio:bitrate", "Bitrate", format!("{b} kbps")));
    }
    if let Some(r) = p.sample_rate().filter(|&r| r > 0) {
        fields.push(tech("audio:rate", "Sample rate", format!("{r} Hz")));
    }
    if let Some(c) = p.channels().filter(|&c| c > 0) {
        let v = match c {
            1 => "1 (mono)".to_string(),
            2 => "2 (stereo)".to_string(),
            n => n.to_string(),
        };
        fields.push(tech("audio:channels", "Channels", v));
    }
    if let Some(d) = p.bit_depth().filter(|&d| d > 0) {
        fields.push(tech("audio:depth", "Bit depth", format!("{d} bit")));
    }
    Ok(Extract {
        fields,
        notes: Vec::new(),
        gps: None,
        taken,
        editable: AUDIO_KEYS.to_vec(),
    })
}

// ---------------------------------------------------------------------------
// Clean.

fn save_or_remove(tag: &Tag, path: &Path) -> Result<()> {
    if tag.item_count() == 0 && tag.picture_count() == 0 {
        tag.tag_type().remove_from_path(path).map_err(write_err)
    } else {
        tag.save_to_path(path, WriteOptions::default())
            .map_err(write_err)
    }
}

fn save_id3v2(tag: &Id3v2Tag, path: &Path) -> Result<()> {
    if tag.is_empty() {
        TagType::Id3v2.remove_from_path(path).map_err(write_err)
    } else {
        tag.save_to_path(path, WriteOptions::default())
            .map_err(write_err)
    }
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    if is_m4a(src) {
        return crate::formats::mp4::clean(src, dst, opts);
    }
    let tagged = open_tagged(src)?;
    let ft = tagged.file_type();
    let id3 = read_id3v2(src, ft);
    if ft == FileType::Wav {
        wav_rewrite(src, dst, opts)?;
    } else {
        std::fs::copy(src, dst).map_err(|e| format!("Could not write the cleaned copy: {e}"))?;
    }
    if let Some(mut id3) = id3 {
        if opts.is_everything() || ft == FileType::Flac {
            // ID3v2 in FLAC is read-only in lofty: drop it whole when anything is removed.
            let any = (&id3)
                .into_iter()
                .any(|fr| opts.removes(id3_frame(fr).1.category));
            if opts.is_everything() || any {
                TagType::Id3v2.remove_from_path(dst).map_err(write_err)?;
            }
        } else {
            id3.retain(|fr| !opts.removes(id3_frame(fr).1.category));
            save_id3v2(&id3, dst)?;
        }
    }
    for tag in tagged.tags() {
        let tt = tag.tag_type();
        if tt == TagType::Id3v2 {
            continue;
        }
        if opts.is_everything() {
            tt.remove_from_path(dst).map_err(write_err)?;
            continue;
        }
        let mut tag = tag.clone();
        tag.retain(|item| !opts.removes(item_class(item.key()).category));
        if opts.removes(Category::Hidden) {
            while tag.picture_count() > 0 {
                tag.remove_picture(0);
            }
        }
        save_or_remove(&tag, dst)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edit.

fn check_edit(e: &Edit) -> Result<()> {
    let v = e.value.as_deref().map(str::trim).unwrap_or("");
    match e.key {
        k if !AUDIO_KEYS.contains(&k) => Err(format!(
            "{} cannot be changed in audio files.",
            k.label(Kind::Audio)
        )),
        EditKey::Track if !v.is_empty() && crate::formats::mp4::split_track(v).is_none() => {
            Err("The track must look like 3 or 3/12.".into())
        }
        EditKey::Year if !v.is_empty() && DateTime::parse(v).is_none() => {
            Err("The year must look like 2026 (or a date such as 2026-05-01).".into())
        }
        _ => Ok(()),
    }
}

fn value_of(e: &Edit) -> Option<&str> {
    e.value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

fn set_text_frame(tag: &mut Id3v2Tag, id: &'static str, value: Option<&str>) {
    tag.retain(|f| f.id_str() != id);
    if let (Some(v), Ok(fid)) = (value, FrameId::new(id)) {
        tag.insert(Frame::Text(TextInformationFrame::new(
            fid,
            TextEncoding::UTF8,
            v.to_string(),
        )));
    }
}

fn edit_id3v2(tag: &mut Id3v2Tag, edits: &[Edit]) {
    for e in edits {
        let v = value_of(e);
        match e.key {
            EditKey::Title => set_text_frame(tag, "TIT2", v),
            EditKey::Author => set_text_frame(tag, "TPE1", v),
            EditKey::Album => set_text_frame(tag, "TALB", v),
            EditKey::Genre => set_text_frame(tag, "TCON", v),
            EditKey::Track => set_text_frame(tag, "TRCK", v),
            EditKey::Copyright => set_text_frame(tag, "TCOP", v),
            EditKey::Year => {
                for old in ["TYER", "TDAT", "TIME"] {
                    tag.retain(|f| f.id_str() != old);
                }
                set_text_frame(tag, "TDRC", v);
            }
            EditKey::Description => match v {
                Some(v) => tag.set_comment(v.to_string()),
                None => tag.remove_comment(),
            },
            _ => {}
        }
    }
}

fn edit_generic(tag: &mut Tag, edits: &[Edit]) {
    use ItemKey as K;
    let id3v1 = tag.tag_type() == TagType::Id3v1;
    for e in edits {
        // ID3v1 holds 30 Latin-1 characters at most: drop what it cannot hold
        // rather than keep an old or mangled value.
        let v = value_of(e).filter(|v| !id3v1 || (v.len() <= 30 && v.is_ascii()));
        let keys: &[K] = match e.key {
            EditKey::Title => &[K::TrackTitle],
            EditKey::Author => &[K::TrackArtist, K::TrackArtists],
            EditKey::Album => &[K::AlbumTitle],
            EditKey::Genre => &[K::Genre],
            EditKey::Description => &[K::Comment],
            EditKey::Copyright => &[K::CopyrightMessage],
            EditKey::Year => &[K::RecordingDate, K::Year],
            EditKey::Track => &[K::TrackNumber, K::TrackTotal],
            _ => &[],
        };
        for k in keys {
            tag.remove_key(k);
        }
        let Some(v) = v else { continue };
        match e.key {
            EditKey::Track => {
                if let Some((n, t)) = crate::formats::mp4::split_track(v) {
                    tag.insert_text(K::TrackNumber, n.to_string());
                    if let Some(t) = t {
                        tag.insert_text(K::TrackTotal, t.to_string());
                    }
                }
            }
            EditKey::Year => {
                if !tag.insert_text(K::RecordingDate, v.to_string()) {
                    tag.insert_text(K::Year, v.to_string());
                }
            }
            _ => {
                if let Some(k) = keys.first() {
                    tag.insert_text(k.clone(), v.to_string());
                }
            }
        }
    }
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    if is_m4a(src) {
        return crate::formats::mp4::edit(src, dst, edits);
    }
    for e in edits {
        check_edit(e)?;
    }
    let tagged = open_tagged(src)?;
    let ft = tagged.file_type();
    let id3 = read_id3v2(src, ft);
    std::fs::copy(src, dst).map_err(|e| format!("Could not write the edited copy: {e}"))?;
    let mut wrote = false;
    if let Some(mut id3) = id3 {
        if ft == FileType::Flac {
            TagType::Id3v2.remove_from_path(dst).map_err(write_err)?;
        } else {
            edit_id3v2(&mut id3, edits);
            save_id3v2(&id3, dst)?;
            wrote = true;
        }
    }
    for tag in tagged.tags() {
        if tag.tag_type() == TagType::Id3v2 {
            continue;
        }
        let mut tag = tag.clone();
        edit_generic(&mut tag, edits);
        save_or_remove(&tag, dst)?;
        wrote |= tag.tag_type() != TagType::Id3v1;
    }
    if !wrote {
        match ft.primary_tag_type() {
            TagType::Id3v2 => {
                let mut id3 = Id3v2Tag::new();
                edit_id3v2(&mut id3, edits);
                save_id3v2(&id3, dst)?;
            }
            tt => {
                let mut tag = Tag::new(tt);
                edit_generic(&mut tag, edits);
                save_or_remove(&tag, dst)?;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Samples (invented data).

fn syncsafe(n: u32) -> [u8; 4] {
    [
        (n >> 21 & 0x7F) as u8,
        (n >> 14 & 0x7F) as u8,
        (n >> 7 & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn id3_frame_bytes(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&syncsafe(body.len() as u32));
    v.extend_from_slice(&[0, 0]);
    v.extend_from_slice(body);
    v
}

fn id3_text(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut b = vec![3u8]; // UTF-8
    b.extend_from_slice(text.as_bytes());
    id3_frame_bytes(id, &b)
}

/// The ID3v2.4 tag of the MP3 sample.
fn sample_id3v2() -> Vec<u8> {
    let mut frames = Vec::new();
    for (id, text) in [
        (b"TIT2", "Invented Song"),
        (b"TPE1", "Test Artist"),
        (b"TALB", "Sample Album"),
        (b"TDRC", "2024-05-01"),
        (b"TCON", "Ambient"),
        (b"TRCK", "3/12"),
        (b"TSSE", "ExampleEncoder 1.0"),
        (b"TCOP", "2024 Test Label"),
    ] {
        frames.extend(id3_text(id, text));
    }
    let mut comm = vec![3u8];
    comm.extend_from_slice(b"eng\0Recorded at home");
    frames.extend(id3_frame_bytes(b"COMM", &comm));
    let mut txxx = vec![3u8];
    txxx.extend_from_slice(b"MusicBrainz Album Id\x0012345678-0000-4000-8000-000000000001");
    frames.extend(id3_frame_bytes(b"TXXX", &txxx));
    frames.extend(id3_frame_bytes(
        b"UFID",
        b"http://example.org/ids\0EXAMPLE-ID-0042",
    ));
    frames.extend(id3_frame_bytes(
        b"PRIV",
        b"ExamplePlayer/Session\0\x01\x02\x03\x04",
    ));
    let mut apic = vec![0u8];
    apic.extend_from_slice(b"image/jpeg\0\x03\0");
    apic.extend_from_slice(&tiny_jpeg());
    frames.extend(id3_frame_bytes(b"APIC", &apic));
    let mut v = b"ID3\x04\x00\x00".to_vec();
    v.extend_from_slice(&syncsafe(frames.len() as u32));
    v.extend(frames);
    v
}

fn sample_id3v1() -> Vec<u8> {
    let field = |s: &str, n: usize| {
        let mut b = s.as_bytes().to_vec();
        b.resize(n, 0);
        b
    };
    let mut v = b"TAG".to_vec();
    v.extend(field("Invented Song", 30));
    v.extend(field("Test Artist", 30));
    v.extend(field("Sample Album", 30));
    v.extend(field("2024", 4));
    v.extend(field("Recorded at home", 28));
    v.extend_from_slice(&[0, 3, 26]); // track 3, genre "Ambient"
    v
}

/// Silent MPEG-1 Layer III frames: 128 kbps, 44.1 kHz, mono, 417 bytes each.
pub(crate) fn mp3_frames(count: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(count * 417);
    for _ in 0..count {
        let mut frame = vec![0u8; 417];
        frame[..4].copy_from_slice(&[0xFF, 0xFB, 0x90, 0xC4]);
        v.extend(frame);
    }
    v
}

/// An MP3 with an ID3v2.4 tag (artist, cover, private frame, file id) and an ID3v1 tag.
pub fn sample() -> Vec<u8> {
    let mut v = sample_id3v2();
    v.extend(mp3_frames(40));
    v.extend(sample_id3v1());
    v
}

fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Valid FLAC frames of silence: 192 samples, 44.1 kHz, mono, 16 bit.
pub(crate) fn flac_frames(count: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for n in 0..count.min(127) {
        let mut fr = vec![0xFF, 0xF8, 0x19, 0x08, n as u8];
        fr.push(crc8(&fr));
        fr.extend_from_slice(&[0x00, 0x00, 0x00]); // CONSTANT subframe, value 0
        let c = crc16(&fr);
        fr.extend_from_slice(&c.to_be_bytes());
        v.extend(fr);
    }
    v
}

fn flac_block(ty: u8, last: bool, body: &[u8]) -> Vec<u8> {
    let mut v = vec![ty | if last { 0x80 } else { 0 }];
    v.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    v.extend_from_slice(body);
    v
}

/// A FLAC file with Vorbis comments (incl. a place name) and a cover picture.
pub fn sample_flac() -> Vec<u8> {
    const FRAMES: usize = 100;
    let mut si = Vec::new();
    si.extend_from_slice(&192u16.to_be_bytes());
    si.extend_from_slice(&192u16.to_be_bytes());
    si.extend_from_slice(&11u32.to_be_bytes()[1..]);
    si.extend_from_slice(&11u32.to_be_bytes()[1..]);
    let packed: u64 = (44100u64 << 44) | (15u64 << 36) | (192 * FRAMES) as u64;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    let vendor = b"ExampleEncoder 1.0";
    vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vc.extend_from_slice(vendor);
    let comments = [
        "TITLE=Invented Song",
        "ARTIST=Test Artist",
        "ALBUM=Sample Album",
        "DATE=2024-05-01",
        "GENRE=Ambient",
        "COMMENT=Recorded at home",
        "LOCATION=Erbil Citadel",
        "ENCODER=ExampleEncoder 1.0",
        "COPYRIGHT=2024 Test Label",
        "MUSICBRAINZ_TRACKID=12345678-0000-4000-8000-000000000002",
    ];
    vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
        vc.extend_from_slice(c.as_bytes());
    }
    let jpeg = tiny_jpeg();
    let mut pic = Vec::new();
    pic.extend_from_slice(&3u32.to_be_bytes());
    pic.extend_from_slice(&10u32.to_be_bytes());
    pic.extend_from_slice(b"image/jpeg");
    pic.extend_from_slice(&0u32.to_be_bytes());
    for x in [600u32, 600, 24, 0, jpeg.len() as u32] {
        pic.extend_from_slice(&x.to_be_bytes());
    }
    pic.extend_from_slice(&jpeg);
    let mut v = b"fLaC".to_vec();
    v.extend(flac_block(0, false, &si));
    v.extend(flac_block(4, false, &vc));
    v.extend(flac_block(6, true, &pic));
    v.extend(flac_frames(FRAMES));
    v
}

fn riff_chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

/// A one-second WAV with RIFF INFO, a broadcast `bext` chunk and iXML.
pub fn sample_wav() -> Vec<u8> {
    let mut fmt = Vec::new();
    for x in [1u16, 1] {
        fmt.extend_from_slice(&x.to_le_bytes());
    }
    fmt.extend_from_slice(&8000u32.to_le_bytes());
    fmt.extend_from_slice(&16000u32.to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let data: Vec<u8> = (0..16000u32).map(|i| (i * 7 % 256) as u8).collect();
    let mut info = b"INFO".to_vec();
    for (id, text) in [
        (b"INAM", "Invented Take"),
        (b"IART", "Test Speaker"),
        (b"ICMT", "Interview draft"),
        (b"ICRD", "2024-05-01"),
        (b"ISFT", "ExampleRecorder 2.1"),
    ] {
        let mut t = text.as_bytes().to_vec();
        t.push(0);
        info.extend(riff_chunk(id, &t));
    }
    let mut bext = vec![0u8; 602];
    let put =
        |b: &mut Vec<u8>, off: usize, s: &str| b[off..off + s.len()].copy_from_slice(s.as_bytes());
    put(&mut bext, 0, "Field recording");
    put(&mut bext, 256, "ExampleRecorder");
    put(&mut bext, 288, "EXR-SN-0042");
    put(&mut bext, 320, "2024-05-01");
    put(&mut bext, 330, "10:20:30");
    bext[346] = 1; // version
    bext.extend_from_slice(b"A=PCM,F=8000,W=16,M=mono,T=ExampleRecorder\r\n");
    let ixml = b"<?xml version=\"1.0\"?><BWFXML><PROJECT>Invented Project</PROJECT></BWFXML>";
    let body = [
        b"WAVE".to_vec(),
        riff_chunk(b"fmt ", &fmt),
        riff_chunk(b"bext", &bext),
        riff_chunk(b"data", &data),
        riff_chunk(b"LIST", &info),
        riff_chunk(b"iXML", ixml),
    ]
    .concat();
    riff_chunk(b"RIFF", &body)
}

/// An M4A song with iTunes tags and a cover picture.
pub fn sample_m4a() -> Vec<u8> {
    crate::formats::mp4::sample_m4a()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let p =
                std::env::temp_dir().join(format!("metaforge-audio-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn get<'a>(ex: &'a Extract, id: &str) -> Option<&'a Field> {
        ex.fields.iter().find(|f| f.id == id)
    }

    fn leftovers(ex: &Extract, opts: &CleanOptions) -> Vec<Field> {
        ex.fields
            .iter()
            .filter(|f| f.removable && opts.removes(f.category))
            .cloned()
            .collect()
    }

    fn values(ex: &Extract, key: EditKey) -> Vec<String> {
        let mut v: Vec<String> = ex
            .fields
            .iter()
            .filter(|f| f.edit == Some(key))
            .map(|f| f.value.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn duration(p: &Path) -> std::time::Duration {
        lofty::read_from_path(p).unwrap().properties().duration()
    }

    /// The MPEG frames between the ID3v2 tag and the ID3v1 tag.
    fn mp3_audio(b: &[u8]) -> &[u8] {
        let start = if b.starts_with(b"ID3") {
            let s = &b[6..10];
            10 + ((s[0] as usize) << 21
                | (s[1] as usize) << 14
                | (s[2] as usize) << 7
                | s[3] as usize)
        } else {
            0
        };
        let end = if b.len() >= 128 && &b[b.len() - 128..b.len() - 125] == b"TAG" {
            b.len() - 128
        } else {
            b.len()
        };
        // Skip padding lofty may add after the tag.
        let start = start + b[start..].iter().position(|&c| c == 0xFF).unwrap();
        &b[start..end]
    }

    /// The FLAC frames after the last metadata block.
    fn flac_audio(b: &[u8]) -> &[u8] {
        let mut i = 4;
        loop {
            let last = b[i] & 0x80 != 0;
            let len = u32::from_be_bytes([0, b[i + 1], b[i + 2], b[i + 3]]) as usize;
            i += 4 + len;
            if last {
                return &b[i..];
            }
        }
    }

    fn wav_data(b: &[u8]) -> Vec<u8> {
        let i = b.windows(4).position(|w| w == b"data").unwrap();
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into().unwrap()) as usize;
        b[i + 8..i + 8 + n].to_vec()
    }

    #[test]
    fn mp3_read() {
        let t = Tmp::new("mp3read");
        let p = t.file("a.mp3", &sample());
        let ex = read(&p).unwrap();
        let artist = get(&ex, "id3:TPE1").unwrap();
        assert_eq!(
            (
                artist.value.as_str(),
                artist.category,
                artist.flag,
                artist.edit
            ),
            (
                "Test Artist",
                Category::People,
                Some(Flag::Names),
                Some(EditKey::Author)
            )
        );
        assert_eq!(
            get(&ex, "id3:APIC").unwrap().value,
            "Cover picture (JPEG, 600 x 600)"
        );
        let priv_ = get(&ex, "id3:PRIV:ExamplePlayer/Session").unwrap();
        assert_eq!(
            (priv_.category, priv_.flag),
            (Category::Hidden, Some(Flag::Hidden))
        );
        assert_eq!(
            get(&ex, "id3:UFID:http://example.org/ids").unwrap().flag,
            Some(Flag::Serial)
        );
        assert_eq!(
            get(&ex, "id3:TXXX:MusicBrainz Album Id").unwrap().flag,
            Some(Flag::Serial)
        );
        assert_eq!(get(&ex, "id3:COMM").unwrap().flag, Some(Flag::Comments));
        assert_eq!(get(&ex, "id3:TSSE").unwrap().category, Category::Software);
        assert!(ex
            .fields
            .iter()
            .any(|f| f.source == "ID3v1" && f.value == "Test Artist"));
        assert_eq!(ex.taken.as_deref(), Some("2024-05-01 00:00:00"));
        let d = get(&ex, "audio:duration").unwrap();
        assert!(!d.removable && d.category == Category::Technical);
        assert_eq!(get(&ex, "audio:rate").unwrap().value, "44100 Hz");
        assert_eq!(ex.editable, AUDIO_KEYS);
    }

    #[test]
    fn mp3_clean_everything_keeps_audio() {
        let t = Tmp::new("mp3all");
        let bytes = sample();
        let src = t.file("a.mp3", &bytes);
        let dst = t.path("b.mp3");
        let opts = CleanOptions::everything();
        clean(&src, &dst, &opts).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), bytes);
        let out = std::fs::read(&dst).unwrap();
        assert_eq!(mp3_audio(&out), mp3_audio(&bytes));
        assert_eq!(duration(&dst), duration(&src));
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &opts).is_empty(),
            "{:?}",
            leftovers(&ex, &opts)
        );
        assert!(!out.windows(11).any(|w| w == b"Test Artist"));
    }

    #[test]
    fn mp3_partial_clean() {
        let t = Tmp::new("mp3part");
        let src = t.file("a.mp3", &sample());
        let dst = t.path("b.mp3");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&src, &dst, &opts).unwrap();
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &opts).is_empty(),
            "{:?}",
            leftovers(&ex, &opts)
        );
        assert_eq!(values(&ex, EditKey::Title), ["Invented Song"]);
        assert!(get(&ex, "id3:APIC").is_some());
        assert!(get(&ex, "id3:PRIV:ExamplePlayer/Session").is_some());
        assert!(get(&ex, "id3:UFID:http://example.org/ids").is_some());
        assert!(values(&ex, EditKey::Author).is_empty());
        // Hidden alone takes the cover and private frames.
        let dst2 = t.path("c.mp3");
        let hidden = CleanOptions::only(&[Category::Hidden]);
        clean(&src, &dst2, &hidden).unwrap();
        let ex = read(&dst2).unwrap();
        assert!(leftovers(&ex, &hidden).is_empty());
        assert!(get(&ex, "id3:TPE1").is_some());
    }

    #[test]
    fn mp3_edit_round_trip() {
        let t = Tmp::new("mp3edit");
        let src = t.file("a.mp3", &sample());
        let dst = t.path("b.mp3");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "New Artist"),
                Edit::set(EditKey::Year, "2025"),
                Edit::set(EditKey::Track, "4/9"),
                Edit::set(EditKey::Description, "یادداشت"),
                Edit::clear(EditKey::Copyright),
            ],
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert_eq!(values(&ex, EditKey::Title), [title]);
        assert_eq!(values(&ex, EditKey::Author), ["New Artist"]);
        assert_eq!(values(&ex, EditKey::Year), ["2025"]);
        assert_eq!(values(&ex, EditKey::Description), ["یادداشت"]);
        assert!(values(&ex, EditKey::Copyright).is_empty());
        assert!(values(&ex, EditKey::Track).contains(&"4/9".to_string()));
        assert_eq!(ex.taken.as_deref(), Some("2025"));
        assert_eq!(
            mp3_audio(&std::fs::read(&dst).unwrap()),
            mp3_audio(&sample())
        );
        assert!(edit(&src, &t.path("c.mp3"), &[Edit::set(EditKey::Gps, "1, 2")]).is_err());
        assert!(edit(
            &src,
            &t.path("c.mp3"),
            &[Edit::set(EditKey::Year, "someday")]
        )
        .is_err());
    }

    #[test]
    fn flac_read_clean_edit() {
        let t = Tmp::new("flac");
        let bytes = sample_flac();
        let src = t.file("a.flac", &bytes);
        let ex = read(&src).unwrap();
        assert_eq!(
            get(&ex, "vorbis:ARTIST").unwrap().category,
            Category::People
        );
        let place = ex
            .fields
            .iter()
            .find(|f| f.value == "Erbil Citadel")
            .unwrap();
        assert_eq!(
            (place.category, place.flag),
            (Category::Location, Some(Flag::Location))
        );
        assert_eq!(
            get(&ex, "vorbis:picture").unwrap().value,
            "Cover picture (JPEG, 600 x 600)"
        );
        assert!(ex.fields.iter().any(|f| f.flag == Some(Flag::Serial)));
        assert_eq!(get(&ex, "audio:duration").unwrap().value, "0:00 (0.43 s)");

        let dst = t.path("b.flac");
        let all = CleanOptions::everything();
        clean(&src, &dst, &all).unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert_eq!(flac_audio(&out), flac_audio(&bytes));
        assert_eq!(duration(&dst), duration(&src));
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &all).is_empty(),
            "{:?}",
            leftovers(&ex, &all)
        );

        let dst = t.path("c.flac");
        let part = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&src, &dst, &part).unwrap();
        let ex = read(&dst).unwrap();
        assert!(leftovers(&ex, &part).is_empty());
        assert!(get(&ex, "vorbis:picture").is_some());
        assert_eq!(values(&ex, EditKey::Title), ["Invented Song"]);
        assert_eq!(
            flac_audio(&std::fs::read(&dst).unwrap()),
            flac_audio(&bytes)
        );

        let dst = t.path("d.flac");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Year, "2023"),
            ],
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert_eq!(values(&ex, EditKey::Title), [title]);
        assert_eq!(values(&ex, EditKey::Year), ["2023"]);
        assert!(get(&ex, "vorbis:picture").is_some());
        assert_eq!(
            flac_audio(&std::fs::read(&dst).unwrap()),
            flac_audio(&bytes)
        );
    }

    #[test]
    fn wav_read_clean_edit() {
        let t = Tmp::new("wav");
        let bytes = sample_wav();
        let src = t.file("a.wav", &bytes);
        let ex = read(&src).unwrap();
        assert_eq!(values(&ex, EditKey::Author), ["Test Speaker"]);
        assert_eq!(
            get(&ex, "bext:Originator").unwrap().category,
            Category::Device
        );
        assert_eq!(
            get(&ex, "bext:OriginatorReference").unwrap().flag,
            Some(Flag::Serial)
        );
        assert_eq!(
            get(&ex, "bext:OriginationDate").unwrap().value,
            "2024-05-01 10:20:30"
        );
        assert_eq!(get(&ex, "wav:iXML").unwrap().category, Category::Hidden);
        assert_eq!(get(&ex, "audio:duration").unwrap().value, "0:01 (1.00 s)");

        let dst = t.path("b.wav");
        let all = CleanOptions::everything();
        clean(&src, &dst, &all).unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert_eq!(wav_data(&out), wav_data(&bytes));
        assert_eq!(duration(&dst), duration(&src));
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &all).is_empty(),
            "{:?}",
            leftovers(&ex, &all)
        );
        assert!(!out.windows(12).any(|w| w == b"Test Speaker"));
        assert!(!out.windows(11).any(|w| w == b"EXR-SN-0042"));

        let dst = t.path("c.wav");
        let part = CleanOptions::only(&[Category::People, Category::Dates]);
        clean(&src, &dst, &part).unwrap();
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &part).is_empty(),
            "{:?}",
            leftovers(&ex, &part)
        );
        assert_eq!(
            get(&ex, "bext:Originator").unwrap().value,
            "ExampleRecorder"
        );
        assert!(get(&ex, "wav:iXML").is_some());
        assert_eq!(values(&ex, EditKey::Title), ["Invented Take"]);

        let dst = t.path("d.wav");
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, "New Take"),
                Edit::set(EditKey::Author, "Someone Else"),
            ],
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert_eq!(values(&ex, EditKey::Title), ["New Take"]);
        assert_eq!(values(&ex, EditKey::Author), ["Someone Else"]);
        assert_eq!(wav_data(&std::fs::read(&dst).unwrap()), wav_data(&bytes));
    }

    #[test]
    fn m4a_read_clean_edit() {
        let t = Tmp::new("m4a");
        let bytes = sample_m4a();
        let src = t.file("a.m4a", &bytes);
        let ex = read(&src).unwrap();
        assert_eq!(values(&ex, EditKey::Author), ["Test Artist"]);
        assert_eq!(ex.editable, AUDIO_KEYS);
        let dst = t.path("b.m4a");
        let all = CleanOptions::everything();
        clean(&src, &dst, &all).unwrap();
        let ex = read(&dst).unwrap();
        assert!(
            leftovers(&ex, &all).is_empty(),
            "{:?}",
            leftovers(&ex, &all)
        );
        assert_eq!(std::fs::metadata(&dst).unwrap().len(), bytes.len() as u64);
        assert_eq!(duration(&dst), duration(&src));

        let dst = t.path("c.m4a");
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Album, "ئەلبوم"),
                Edit::set(EditKey::Year, "2021"),
                Edit::set(EditKey::Track, "2/5"),
            ],
        )
        .unwrap();
        let ex = read(&dst).unwrap();
        assert_eq!(values(&ex, EditKey::Album), ["ئەلبوم"]);
        assert_eq!(values(&ex, EditKey::Year), ["2021"]);
        assert_eq!(values(&ex, EditKey::Track), ["2/5"]);
        assert_eq!(values(&ex, EditKey::Title), ["Invented Song"]);
        assert_eq!(duration(&dst), duration(&src));
    }

    #[test]
    fn broken_input_is_an_error() {
        let t = Tmp::new("bad");
        let mp3 = sample();
        let flac = sample_flac();
        let wav = sample_wav();
        for (name, bytes) in [
            ("garbage.mp3", b"definitely not audio".to_vec()),
            ("empty.flac", Vec::new()),
            ("trunc.flac", flac[..30].to_vec()),
            ("trunc.wav", wav[..60].to_vec()),
            ("trunc.mp3", mp3[..20].to_vec()),
        ] {
            let p = t.file(name, &bytes);
            assert!(read(&p).is_err(), "{name}");
            assert!(
                clean(&p, &t.path("out.bin"), &CleanOptions::everything()).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn picture_labels() {
        assert_eq!(
            picture_label(&tiny_jpeg()),
            "Cover picture (JPEG, 600 x 600)"
        );
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&[0, 0, 0, 16, 0, 0, 0, 8]);
        assert_eq!(picture_label(&png), "Cover picture (PNG, 16 x 8)");
        assert_eq!(picture_label(b"xyz"), "Cover picture (3 bytes)");
    }
}
