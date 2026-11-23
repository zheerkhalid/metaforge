//! Invented files for `--demo` and screenshots. Everything here is made up in
//! code: no file on this PC is read or changed, the people, companies and
//! serial numbers do not exist, and the coordinates are generic.

use crate::detect::Format;
use crate::engine::{self, CleanJob, CleanResult, Output};
use crate::meta::{Category, EditKey, Field, Flag, GeoPoint, Meta};
use crate::util;
use std::path::{Path, PathBuf};

const PICTURES: &str = "C:\\Users\\Alex\\Pictures\\Holiday 2025";
const DOCUMENTS: &str = "C:\\Users\\Alex\\Documents";
const MUSIC: &str = "C:\\Users\\Alex\\Music";
const DOWNLOADS: &str = "C:\\Users\\Alex\\Downloads";

/// A removable field.
fn f(id: &str, label: &str, value: &str, cat: Category, source: &str) -> Field {
    Field::new(id, label, value, cat, source)
}

/// A field with a privacy badge.
fn fl(id: &str, label: &str, value: &str, cat: Category, source: &str, flag: Flag) -> Field {
    Field::new(id, label, value, cat, source).flag(flag)
}

/// A fixed technical field.
fn tech(id: &str, label: &str, value: &str, source: &str) -> Field {
    Field::new(id, label, value, Category::Technical, source).fixed()
}

struct Spec {
    dir: &'static str,
    name: &'static str,
    format: Format,
    size: u64,
    fields: Vec<Field>,
    notes: Vec<&'static str>,
    gps: Option<(f64, f64)>,
    taken: Option<&'static str>,
    editable: Vec<EditKey>,
    /// Windows file times: created, modified, last opened.
    times: [&'static str; 3],
}

fn build(s: Spec) -> Meta {
    let mut fields = s.fields;
    fields.push(tech(
        "file:size",
        "File size",
        &util::human_size(s.size),
        "File",
    ));
    fields.push(tech("file:type", "Type", s.format.name(), "File"));
    for (id, label, value) in [
        ("file:created", "File created", s.times[0]),
        ("file:modified", "File modified", s.times[1]),
        ("file:accessed", "File last opened", s.times[2]),
    ] {
        fields.push(Field::new(id, label, value, Category::Windows, "Windows file system").fixed());
    }
    Meta {
        path: Path::new(s.dir).join(s.name),
        format: s.format,
        size: s.size,
        fields,
        notes: s.notes.into_iter().map(String::from).collect(),
        gps: s.gps.map(|(lat, lon)| GeoPoint { lat, lon }),
        taken: s.taken.map(String::from),
        editable: s.editable,
        error: None,
    }
}

const PHOTO_EDITS: [EditKey; 7] = [
    EditKey::Title,
    EditKey::Author,
    EditKey::Copyright,
    EditKey::Description,
    EditKey::Keywords,
    EditKey::DateTaken,
    EditKey::Gps,
];

const DOC_EDITS: [EditKey; 7] = [
    EditKey::Title,
    EditKey::Subject,
    EditKey::Author,
    EditKey::Company,
    EditKey::Keywords,
    EditKey::Description,
    EditKey::DateTaken,
];

fn gps_fields(lat: &str, lon: &str, alt: &str) -> Vec<Field> {
    use Category::Location as L;
    vec![
        fl(
            "exif:gps:0x0002",
            "GPS latitude",
            lat,
            L,
            "EXIF",
            Flag::Location,
        )
        .edit(EditKey::Gps),
        fl(
            "exif:gps:0x0004",
            "GPS longitude",
            lon,
            L,
            "EXIF",
            Flag::Location,
        )
        .edit(EditKey::Gps),
        fl(
            "exif:gps:0x0006",
            "GPS altitude",
            alt,
            L,
            "EXIF",
            Flag::Location,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn camera_photo(
    name: &'static str,
    size: u64,
    gps: (f64, f64),
    lat: &str,
    lon: &str,
    taken: &'static str,
    serial: &str,
    extra: Vec<Field>,
) -> Meta {
    use Category::*;
    let mut fields = gps_fields(lat, lon, "41 m above sea level");
    fields.extend(extra);
    fields.extend([
        f("exif:0x010F", "Camera maker", "Lumora", Device, "EXIF"),
        f("exif:0x0110", "Camera model", "Lumora X-T7", Device, "EXIF"),
        f("exif:0xA434", "Lens", "LZ 18-55 mm f/2.8", Device, "EXIF"),
        fl(
            "exif:0xA431",
            "Camera serial number",
            serial,
            Device,
            "EXIF",
            Flag::Serial,
        ),
        fl(
            "exif:0xA435",
            "Lens serial number",
            "L55-210934",
            Device,
            "EXIF",
            Flag::Serial,
        ),
        f("exif:0x9003", "Date taken", taken, Dates, "EXIF").edit(EditKey::DateTaken),
        f("exif:0x9004", "Date digitized", taken, Dates, "EXIF"),
        f(
            "exif:0x0131",
            "Software",
            "Lumora firmware 2.10",
            Software,
            "EXIF",
        ),
        f(
            "exif:0x829A",
            "Exposure",
            "1/320 s, f/5.6, ISO 200",
            Device,
            "EXIF",
        ),
        fl(
            "exif:thumbnail",
            "Embedded thumbnail",
            "JPEG picture, 160 x 120",
            Hidden,
            "EXIF",
            Flag::Hidden,
        ),
        tech("img:size", "Dimensions", "6000 x 4000", "JPEG"),
        tech("exif:0x0112", "Orientation", "Normal", "EXIF"),
        tech(
            "icc:desc",
            "Colour profile",
            "sRGB IEC61966-2.1",
            "ICC profile",
        ),
    ]);
    build(Spec {
        dir: PICTURES,
        name,
        format: Format::Jpeg,
        size,
        fields,
        notes: vec![
            "The embedded thumbnail is a small copy of the picture; it may still show parts that were cropped later.",
        ],
        gps: Some(gps),
        taken: Some(taken),
        editable: PHOTO_EDITS.to_vec(),
        times: [taken, taken, "2025-11-20 09:14:02"],
    })
}

/// The invented files of the demo, in the order they are listed.
pub fn files() -> Vec<Meta> {
    use Category::*;
    let mut out = Vec::new();

    // 1. A holiday photo with GPS, the owner's name and the camera serial.
    out.push(camera_photo(
        "IMG_2041.jpg",
        4_412_806,
        (38.71204, -9.13661),
        "38° 42' 43.3\" N",
        "9° 8' 11.8\" W",
        "2025-07-14 18:42:07",
        "LX7-0042-8812",
        vec![
            fl(
                "exif:0x013B",
                "Artist",
                "Alex Rivera",
                People,
                "EXIF",
                Flag::Names,
            )
            .edit(EditKey::Author),
            fl(
                "exif:0x8298",
                "Copyright",
                "© Alex Rivera",
                People,
                "EXIF",
                Flag::Names,
            )
            .edit(EditKey::Copyright),
            fl(
                "xmp:photoshop:City",
                "City",
                "Harbourtown",
                Location,
                "XMP",
                Flag::Location,
            ),
        ],
    ));

    // 2. Another one from the same camera.
    out.push(camera_photo(
        "IMG_2057.jpg",
        3_980_114,
        (38.69161, -9.21598),
        "38° 41' 29.8\" N",
        "9° 12' 57.5\" W",
        "2025-07-15 11:03:51",
        "LX7-0042-8812",
        vec![],
    ));

    // 3. A phone picture (HEIC) with a depth map.
    out.push(build(Spec {
        dir: PICTURES,
        name: "PXL_20250803_114502.heic",
        format: Format::Heif,
        size: 2_871_330,
        fields: {
            let mut v = gps_fields("37° 1' 9.8\" N", "7° 55' 49.4\" W", "12 m above sea level");
            v.extend([
                f(
                    "exif:gps:0x0011",
                    "Direction the camera faced",
                    "214°",
                    Location,
                    "EXIF",
                )
                .flag(Flag::Location),
                f("exif:0x010F", "Phone maker", "Pixelon", Device, "EXIF"),
                f(
                    "exif:0x0110",
                    "Phone model",
                    "Pixelon 8 Pro",
                    Device,
                    "EXIF",
                ),
                f(
                    "exif:0xA434",
                    "Lens",
                    "Main camera 6.9 mm f/1.7",
                    Device,
                    "EXIF",
                ),
                f(
                    "exif:0x9003",
                    "Date taken",
                    "2025-08-03 11:45:02",
                    Dates,
                    "EXIF",
                )
                .edit(EditKey::DateTaken),
                f("exif:0x9011", "Time zone", "+01:00", Dates, "EXIF"),
                f("exif:0x0131", "Software", "HDR+ 1.0.6", Software, "EXIF"),
                fl(
                    "heif:depth",
                    "Depth map",
                    "Grey picture, 640 x 480",
                    Hidden,
                    "HEIF auxiliary image",
                    Flag::Hidden,
                ),
                tech("img:size", "Dimensions", "4080 x 3072", "HEIF"),
                tech("icc:desc", "Colour profile", "Display P3", "ICC profile"),
            ]);
            v
        },
        notes: vec!["This photo also holds a depth map, used for background blur."],
        gps: Some((37.01939, -7.93039)),
        taken: Some("2025-08-03 11:45:02"),
        editable: vec![
            EditKey::Title,
            EditKey::Description,
            EditKey::DateTaken,
            EditKey::Gps,
        ],
        times: [
            "2025-08-03 11:45:02",
            "2025-08-03 11:45:02",
            "2025-11-18 20:31:44",
        ],
    }));

    // 4. A photo with a Kurdish name, title and photographer.
    out.push(build(Spec {
        dir: PICTURES,
        name: "وێنەی گەشت.jpg",
        format: Format::Jpeg,
        size: 3_204_558,
        fields: {
            let mut v = gps_fields(
                "36° 24' 0.0\" N",
                "44° 19' 12.0\" E",
                "1,020 m above sea level",
            );
            v.extend([
                fl(
                    "xmp:dc:title",
                    "Title",
                    "گەشتی هاوین لە چیا",
                    Description,
                    "XMP",
                    Flag::Comments,
                )
                .edit(EditKey::Title),
                fl(
                    "xmp:dc:description",
                    "Description",
                    "یادگاری گەشتی خێزانی",
                    Description,
                    "XMP",
                    Flag::Comments,
                )
                .edit(EditKey::Description),
                fl(
                    "xmp:dc:creator",
                    "Photographer",
                    "هێمن عەلی",
                    People,
                    "XMP",
                    Flag::Names,
                )
                .edit(EditKey::Author),
                f(
                    "xmp:dc:subject",
                    "Keywords",
                    "گەشت، چیا، هاوین",
                    Description,
                    "XMP",
                )
                .edit(EditKey::Keywords),
                f("exif:0x010F", "Camera maker", "Lumora", Device, "EXIF"),
                f("exif:0x0110", "Camera model", "Lumora Z50", Device, "EXIF"),
                f(
                    "exif:0x9003",
                    "Date taken",
                    "2025-06-21 07:15:40",
                    Dates,
                    "EXIF",
                )
                .edit(EditKey::DateTaken),
                f(
                    "exif:0x0131",
                    "Software",
                    "Photo Studio 12.4",
                    Software,
                    "EXIF",
                ),
                tech("img:size", "Dimensions", "5568 x 3712", "JPEG"),
                tech(
                    "exif:0x0112",
                    "Orientation",
                    "Rotated 90° clockwise",
                    "EXIF",
                ),
            ]);
            v
        },
        notes: vec![],
        gps: Some((36.4, 44.32)),
        taken: Some("2025-06-21 07:15:40"),
        editable: PHOTO_EDITS.to_vec(),
        times: [
            "2025-06-21 07:15:40",
            "2025-06-22 21:04:12",
            "2025-11-21 08:02:55",
        ],
    }));

    // 5. A Word document full of names and history.
    out.push(build(Spec {
        dir: DOCUMENTS,
        name: "Quarterly plan.docx",
        format: Format::Docx,
        size: 184_320,
        fields: vec![
            f("ooxml:core:title", "Title", "Q3 plan (draft)", Description, "Office properties")
                .edit(EditKey::Title),
            fl("ooxml:core:creator", "Author", "Jordan Blake", People, "Office properties", Flag::Names)
                .edit(EditKey::Author),
            fl(
                "ooxml:core:lastModifiedBy",
                "Last saved by",
                "Sam Carter",
                People,
                "Office properties",
                Flag::Names,
            ),
            fl("ooxml:app:Company", "Company", "Bluefield Partners", People, "Office properties", Flag::Names)
                .edit(EditKey::Company),
            fl("ooxml:app:Manager", "Manager", "Dana Lee", People, "Office properties", Flag::Names),
            fl(
                "ooxml:comments:authors",
                "Comment authors",
                "Sam Carter; Dana Lee",
                People,
                "Comments",
                Flag::Names,
            ),
            fl(
                "ooxml:comments",
                "Comments",
                "3 comments, e.g. \"Check these numbers with finance first\"",
                Description,
                "Comments",
                Flag::Comments,
            ),
            fl(
                "ooxml:revisions",
                "Tracked changes",
                "14 insertions and 6 deletions by 2 people",
                Hidden,
                "Document body",
                Flag::Comments,
            ),
            fl(
                "ooxml:app:Template",
                "Template path",
                "C:\\Users\\Alex\\AppData\\Roaming\\Microsoft\\Templates\\Normal.dotm",
                Hidden,
                "Office properties",
                Flag::Names,
            ),
            f("ooxml:core:revision", "Revision number", "27", Hidden, "Office properties"),
            f("ooxml:app:TotalTime", "Total editing time", "4 h 12 min", Hidden, "Office properties"),
            fl(
                "ooxml:rsid",
                "Editing session ids",
                "41 ids (they link this file to other documents edited on the same PC)",
                Hidden,
                "Document body",
                Flag::Serial,
            ),
            f("ooxml:core:created", "Created", "2025-06-02 09:12:00", Dates, "Office properties")
                .edit(EditKey::DateTaken),
            f("ooxml:core:modified", "Last saved", "2025-09-29 17:48:31", Dates, "Office properties"),
            f("ooxml:core:lastPrinted", "Last printed", "2025-09-12 08:30:05", Dates, "Office properties"),
            f("ooxml:app:Application", "Program", "Microsoft Office Word", Software, "Office properties"),
            f("ooxml:app:AppVersion", "Program version", "16.0000", Software, "Office properties"),
            tech("ooxml:app:Pages", "Pages", "12", "Office properties"),
            tech("ooxml:app:Words", "Words", "3,418", "Office properties"),
        ],
        notes: vec![
            "There are 14 tracked changes; they are hidden in the \"Simple Markup\" view but anyone who opens the file can show them.",
            "The template path shows the Windows user name \"Alex\".",
        ],
        gps: None,
        taken: Some("2025-06-02 09:12:00"),
        editable: DOC_EDITS.to_vec(),
        times: ["2025-06-02 09:12:00", "2025-09-29 17:48:31", "2025-11-22 10:15:09"],
    }));

    // 6. A Word document in Kurdish.
    out.push(build(Spec {
        dir: DOCUMENTS,
        name: "ڕاپۆرتی ساڵانە.docx",
        format: Format::Docx,
        size: 96_540,
        fields: vec![
            f(
                "ooxml:core:title",
                "Title",
                "ڕاپۆرتی ساڵانەی ٢٠٢٥",
                Description,
                "Office properties",
            )
            .edit(EditKey::Title),
            f(
                "ooxml:core:subject",
                "Subject",
                "داهات و خەرجی",
                Description,
                "Office properties",
            )
            .edit(EditKey::Subject),
            f(
                "ooxml:core:keywords",
                "Keywords",
                "ڕاپۆرت، دارایی",
                Description,
                "Office properties",
            )
            .edit(EditKey::Keywords),
            fl(
                "ooxml:core:creator",
                "Author",
                "ئاسۆ ئەحمەد",
                People,
                "Office properties",
                Flag::Names,
            )
            .edit(EditKey::Author),
            fl(
                "ooxml:core:lastModifiedBy",
                "Last saved by",
                "ژیان کەریم",
                People,
                "Office properties",
                Flag::Names,
            ),
            fl(
                "ooxml:app:Company",
                "Company",
                "کۆمپانیای ڕووناکی",
                People,
                "Office properties",
                Flag::Names,
            )
            .edit(EditKey::Company),
            fl(
                "ooxml:comments",
                "Comments",
                "1 comment: «ئەم ژمارانە پشکنینیان بۆ بکە»",
                Description,
                "Comments",
                Flag::Comments,
            ),
            f(
                "ooxml:core:created",
                "Created",
                "2025-01-10 10:00:00",
                Dates,
                "Office properties",
            )
            .edit(EditKey::DateTaken),
            f(
                "ooxml:core:modified",
                "Last saved",
                "2025-03-04 14:22:10",
                Dates,
                "Office properties",
            ),
            f(
                "ooxml:app:Application",
                "Program",
                "Microsoft Office Word",
                Software,
                "Office properties",
            ),
            tech("ooxml:app:Pages", "Pages", "8", "Office properties"),
        ],
        notes: vec![],
        gps: None,
        taken: Some("2025-01-10 10:00:00"),
        editable: DOC_EDITS.to_vec(),
        times: [
            "2025-01-10 10:00:00",
            "2025-03-04 14:22:10",
            "2025-11-19 16:40:03",
        ],
    }));

    // 7. A workbook with a hidden sheet.
    out.push(build(Spec {
        dir: DOCUMENTS,
        name: "Budget 2026.xlsx",
        format: Format::Xlsx,
        size: 61_210,
        fields: vec![
            f("ooxml:core:title", "Title", "Budget 2026", Description, "Office properties")
                .edit(EditKey::Title),
            fl("ooxml:core:creator", "Author", "Sam Carter", People, "Office properties", Flag::Names)
                .edit(EditKey::Author),
            fl(
                "ooxml:core:lastModifiedBy",
                "Last saved by",
                "Jordan Blake",
                People,
                "Office properties",
                Flag::Names,
            ),
            fl("ooxml:app:Company", "Company", "Bluefield Partners", People, "Office properties", Flag::Names)
                .edit(EditKey::Company),
            fl(
                "xlsx:hiddenSheets",
                "Hidden sheets",
                "Salaries",
                Hidden,
                "Workbook",
                Flag::Hidden,
            ),
            fl(
                "ooxml:custom:Client",
                "Custom property \"Client\"",
                "Harbor Foods",
                Hidden,
                "Custom properties",
                Flag::Hidden,
            ),
            f("xlsx:externalLinks", "Links to other files", "D:\\Finance\\Payroll 2025.xlsx", Hidden, "Workbook")
                .flag(Flag::Hidden),
            f("ooxml:core:created", "Created", "2025-10-01 08:05:00", Dates, "Office properties")
                .edit(EditKey::DateTaken),
            f("ooxml:core:modified", "Last saved", "2025-11-14 16:12:45", Dates, "Office properties"),
            f("ooxml:app:Application", "Program", "Microsoft Excel", Software, "Office properties"),
            tech("xlsx:sheets", "Sheets", "4 (1 hidden)", "Workbook"),
        ],
        notes: vec![
            "1 hidden sheet (\"Salaries\") is inside this workbook. Cleaning removes the hidden sheet's name from the properties, not its cells: delete the sheet in Excel if it must not be shared.",
        ],
        gps: None,
        taken: Some("2025-10-01 08:05:00"),
        editable: DOC_EDITS.to_vec(),
        times: ["2025-10-01 08:05:00", "2025-11-14 16:12:45", "2025-11-23 09:40:18"],
    }));

    // 8. A PDF with Info, XMP and an earlier revision.
    out.push(build(Spec {
        dir: DOCUMENTS,
        name: "Contract draft.pdf",
        format: Format::Pdf,
        size: 412_877,
        fields: vec![
            f("pdf:info:Title", "Title", "Service agreement – draft 3", Description, "PDF Info")
                .edit(EditKey::Title),
            fl("pdf:info:Author", "Author", "Jordan Blake", People, "PDF Info", Flag::Names)
                .edit(EditKey::Author),
            fl("xmp:dc:creator", "Creator (XMP)", "Jordan Blake", People, "XMP", Flag::Names),
            f("pdf:info:Creator", "Made with", "Microsoft Word", Software, "PDF Info"),
            f("pdf:info:Producer", "PDF producer", "PageWriter 3.2", Software, "PDF Info"),
            f("pdf:info:CreationDate", "Created", "2025-09-30 11:20:00", Dates, "PDF Info")
                .edit(EditKey::DateTaken),
            f("pdf:info:ModDate", "Modified", "2025-10-02 15:41:09", Dates, "PDF Info"),
            fl(
                "xmp:xmpMM:DocumentID",
                "Document id",
                "uuid:7d4c1e2a-93b0-4f6e-a1c5-2b8e90f4d613",
                Hidden,
                "XMP",
                Flag::Serial,
            ),
            fl(
                "pdf:revisions",
                "Earlier versions inside the file",
                "2 earlier saves",
                Hidden,
                "PDF structure",
                Flag::Hidden,
            ),
            tech("pdf:pages", "Pages", "6", "PDF"),
            tech("pdf:version", "PDF version", "1.7", "PDF"),
        ],
        notes: vec![
            "This PDF was saved 3 times and the earlier versions are still inside it: an older text of the contract can be recovered. Cleaning rewrites the file without them.",
        ],
        gps: None,
        taken: Some("2025-09-30 11:20:00"),
        editable: vec![EditKey::Title, EditKey::Author, EditKey::Subject, EditKey::Keywords, EditKey::DateTaken],
        times: ["2025-09-30 11:20:00", "2025-10-02 15:41:09", "2025-11-21 13:02:47"],
    }));

    // 9. An MP3 with tags and cover art.
    out.push(build(Spec {
        dir: MUSIC,
        name: "Summer mix - 03 Open Road.mp3",
        format: Format::Mp3,
        size: 8_644_096,
        fields: vec![
            f("id3:TIT2", "Title", "Open Road", Description, "ID3v2").edit(EditKey::Title),
            fl(
                "id3:TPE1",
                "Artist",
                "The Harbour Lights",
                People,
                "ID3v2",
                Flag::Names,
            )
            .edit(EditKey::Author),
            f("id3:TALB", "Album", "Summer Mix 2025", Description, "ID3v2").edit(EditKey::Album),
            f("id3:TCON", "Genre", "Indie", Description, "ID3v2").edit(EditKey::Genre),
            f("id3:TRCK", "Track", "3/12", Description, "ID3v2").edit(EditKey::Track),
            f("id3:TDRC", "Year", "2025", Dates, "ID3v2").edit(EditKey::Year),
            fl(
                "id3:COMM",
                "Comment",
                "Ripped for Alex's car",
                Description,
                "ID3v2",
                Flag::Comments,
            )
            .edit(EditKey::Description),
            fl(
                "id3:APIC",
                "Cover picture",
                "JPEG picture, 600 x 600",
                Hidden,
                "ID3v2",
                Flag::Hidden,
            ),
            f("id3:TSSE", "Encoder", "LAME 3.100", Software, "ID3v2"),
            tech("audio:duration", "Duration", "3:41", "MPEG audio"),
            tech(
                "audio:bitrate",
                "Bit rate",
                "320 kbit/s, 44.1 kHz, stereo",
                "MPEG audio",
            ),
        ],
        notes: vec![],
        gps: None,
        taken: Some("2025"),
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Album,
            EditKey::Genre,
            EditKey::Track,
            EditKey::Year,
            EditKey::Description,
        ],
        times: [
            "2025-05-30 19:20:11",
            "2025-05-30 19:20:11",
            "2025-11-23 18:01:37",
        ],
    }));

    // 10. A phone video with location.
    out.push(build(Spec {
        dir: PICTURES,
        name: "VID_20250716_201533.mp4",
        format: Format::Mp4,
        size: 148_897_792,
        fields: vec![
            fl(
                "mp4:udta:xyz",
                "Recorded at",
                "38.713900, -9.139400",
                Location,
                "QuickTime user data",
                Flag::Location,
            )
            .edit(EditKey::Gps),
            f(
                "mp4:mdta:make",
                "Phone maker",
                "Pixelon",
                Device,
                "QuickTime metadata",
            ),
            f(
                "mp4:mdta:model",
                "Phone model",
                "Pixelon 8 Pro",
                Device,
                "QuickTime metadata",
            ),
            f(
                "mp4:mvhd:created",
                "Date recorded",
                "2025-07-16 20:15:33",
                Dates,
                "Movie header",
            )
            .edit(EditKey::DateTaken),
            f(
                "mp4:mdta:software",
                "Software",
                "Android 15",
                Software,
                "QuickTime metadata",
            ),
            tech("video:duration", "Duration", "0:47", "MP4"),
            tech(
                "video:size",
                "Picture size",
                "3840 x 2160, 30 frames/s",
                "MP4",
            ),
            tech("video:codec", "Codec", "HEVC video, AAC audio", "MP4"),
        ],
        notes: vec![],
        gps: Some((38.7139, -9.1394)),
        taken: Some("2025-07-16 20:15:33"),
        editable: vec![EditKey::Title, EditKey::DateTaken, EditKey::Gps],
        times: [
            "2025-07-16 20:15:33",
            "2025-07-16 20:16:20",
            "2025-11-17 21:55:09",
        ],
    }));

    // 11. A downloaded invoice with the "downloaded from" note.
    out.push(build(Spec {
        dir: DOWNLOADS,
        name: "invoice-0425.pdf",
        format: Format::Pdf,
        size: 88_412,
        fields: vec![
            f(
                "pdf:info:Producer",
                "PDF producer",
                "Invoice Maker 5",
                Software,
                "PDF Info",
            ),
            f(
                "pdf:info:CreationDate",
                "Created",
                "2025-11-03 09:00:00",
                Dates,
                "PDF Info",
            )
            .edit(EditKey::DateTaken),
            f(
                "ads:Zone.Identifier:ZoneId",
                "Marked as",
                "Downloaded from the internet",
                Windows,
                "Zone.Identifier stream",
            ),
            fl(
                "ads:Zone.Identifier:HostUrl",
                "Downloaded from address",
                "https://billing.example.com/files/invoice-0425.pdf",
                Windows,
                "Zone.Identifier stream",
                Flag::Hidden,
            ),
            tech("pdf:pages", "Pages", "1", "PDF"),
        ],
        notes: vec![],
        gps: None,
        taken: Some("2025-11-03 09:00:00"),
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Subject,
            EditKey::Keywords,
            EditKey::DateTaken,
        ],
        times: [
            "2025-11-03 09:02:41",
            "2025-11-03 09:02:41",
            "2025-11-03 09:03:10",
        ],
    }));

    // 12. A screenshot that is already clean.
    out.push(build(Spec {
        dir: PICTURES,
        name: "Screenshot 2025-09-02.png",
        format: Format::Png,
        size: 312_664,
        fields: vec![
            tech("img:size", "Dimensions", "1920 x 1080", "PNG"),
            tech("png:color", "Colours", "RGB, 8 bits per channel", "PNG"),
        ],
        notes: vec![],
        gps: None,
        taken: None,
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Description,
            EditKey::Copyright,
        ],
        times: [
            "2025-09-02 12:30:00",
            "2025-09-02 12:30:00",
            "2025-11-10 15:21:08",
        ],
    }));

    out
}

/// Where a cleaned copy would go, without looking at the disk.
fn demo_output(src: &Path, output: &Output) -> PathBuf {
    match output {
        Output::Replace => src.to_path_buf(),
        Output::Suffix(suffix) => {
            let stem = src
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = src
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            src.with_file_name(format!("{stem}{suffix}{ext}"))
        }
        Output::Folder(dir) => dir.join(src.file_name().unwrap_or_default()),
    }
}

/// What a clean of `meta` with `job` would give: the report entry and the
/// file as it would read afterwards. Nothing is written.
pub fn clean(meta: &Meta, job: &CleanJob) -> (CleanResult, Meta) {
    let removed: Vec<Field> = engine::targeted(&meta.fields, &job.opts)
        .into_iter()
        .cloned()
        .collect();
    let mut after = meta.clone();
    after
        .fields
        .retain(|f| !(f.removable && job.opts.remove.contains(f.category)));
    if job.opts.removes(Category::Location) {
        after.gps = None;
    }
    if job.opts.removes(Category::Dates) {
        after.taken = None;
    }
    if job.opts.is_everything() {
        after.notes.clear();
    }
    let removed_bytes: u64 = removed.iter().map(|f| f.value.len() as u64 + 24).sum();
    let thumb = removed
        .iter()
        .any(|f| f.id == "exif:thumbnail" || f.id == "id3:APIC" || f.id == "heif:depth");
    let size_after = meta
        .size
        .saturating_sub(removed_bytes + if thumb { 11_000 } else { 0 });
    after.size = size_after;
    after.path = demo_output(&meta.path, &job.output);
    if let Some(s) = after.fields.iter_mut().find(|f| f.id == "file:size") {
        s.value = util::human_size(size_after);
    }
    let result = CleanResult {
        src: meta.path.clone(),
        dst: after.path.clone(),
        format: meta.format,
        already_clean: removed.is_empty(),
        verified: true,
        removed,
        left: Vec::new(),
        flags_before: meta.flags(),
        flags_after: after.flags(),
        size_before: meta.size,
        size_after,
        error: None,
    };
    (result, after)
}

/// Two files for the compare view: the first photo and its cleaned copy.
pub fn compare_pair() -> (Meta, Meta) {
    let photo = files().remove(0);
    let job = CleanJob {
        opts: crate::meta::CleanOptions::everything(),
        output: Output::Suffix("_clean".into()),
        keep_file_dates: true,
    };
    let (_, cleaned) = clean(&photo, &job);
    (photo, cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::{CleanOptions, Kind};

    #[test]
    fn demo_flags_come_from_fields() {
        let files = files();
        assert!(files.len() >= 10);
        for m in &files {
            // Every badge is backed by a field that carries it (GPS included).
            let from_fields: Vec<Flag> = Flag::ALL
                .into_iter()
                .filter(|f| {
                    m.fields
                        .iter()
                        .any(|x| x.flag == Some(*f) && !x.value.trim().is_empty())
                })
                .collect();
            assert_eq!(m.flags(), from_fields, "{}", m.name());
            if m.gps.is_some() {
                assert!(m.flags().contains(&Flag::Location), "{}", m.name());
            }
            // Fixed fields never carry a badge, and badges are never fixed.
            for f in &m.fields {
                assert!(f.removable || f.flag.is_none(), "{} {}", m.name(), f.id);
                if f.category == Category::Technical {
                    assert!(!f.removable, "{} {}", m.name(), f.id);
                }
                if let Some(k) = f.edit {
                    assert!(m.editable.contains(&k), "{} {:?}", m.name(), k);
                }
            }
            assert!(m.error.is_none());
        }
        // The mix the screenshots rely on.
        assert!(files.iter().any(|m| m.is_clean()));
        assert!(files.iter().filter(|m| m.gps.is_some()).count() >= 4);
        assert!(files.iter().any(|m| m.kind() == Kind::Audio));
        assert!(files.iter().any(|m| m.kind() == Kind::Video));
        assert!(files.iter().any(|m| m.format == Format::Docx));
        assert!(
            files
                .iter()
                .filter(|m| crate::ui::has_rtl(&m.name()))
                .count()
                >= 2
        );
    }

    #[test]
    fn demo_clean_removes_what_it_targets() {
        let job = CleanJob {
            opts: CleanOptions::everything(),
            output: Output::Suffix("_clean".into()),
            keep_file_dates: true,
        };
        for m in files() {
            let (r, after) = clean(&m, &job);
            assert!(r.ok() && r.verified);
            assert!(after.flags().is_empty(), "{}", m.name());
            assert_eq!(r.removed.len(), m.removable_count());
            assert!(after.name().contains("_clean"));
            assert_eq!(r.already_clean, m.removable_count() == 0);
        }
        let (a, b) = compare_pair();
        let rows = engine::compare(&a, &b);
        assert!(rows.iter().any(|r| !r.same()));
        assert!(rows.iter().any(|r| r.same()));
    }
}
