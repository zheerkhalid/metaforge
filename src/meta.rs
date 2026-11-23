//! The metadata model shared by every format, the engine and the window.
//!
//! A format module turns a file into an [`Extract`]: a flat list of
//! [`Field`]s, each with a plain-language label, a [`Category`] (which group
//! it is shown in and which cleaning choice removes it) and optionally a
//! [`Flag`] (which privacy badge it raises).
//!
//! The contract every format module keeps: after `clean` with a set of
//! categories, `read` of the result returns no *removable* field in any of
//! those categories. The engine checks exactly this after every clean.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Groups of fields, in the order the details panel shows them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    /// GPS coordinates, place names, altitude, direction.
    Location,
    /// Author, artist, last modified by, owner, comment authors, company.
    People,
    /// Camera / phone make and model, lens, serial numbers, printer.
    Device,
    /// Date taken, created, modified, printed, digitized.
    Dates,
    /// Programs and versions that made or edited the file.
    Software,
    /// Title, subject, description, comments, keywords, ratings.
    Description,
    /// Things not visible when the file is opened normally: embedded
    /// thumbnails, custom properties, template paths, tracked changes,
    /// hidden sheets, speaker notes, editing history, embedded pictures.
    Hidden,
    /// Intrinsic details (size, dimensions, codec, colour profile,
    /// orientation). Never removed by a preset; colour profile and
    /// orientation follow the "keep" options instead.
    Technical,
    /// Windows-level traces: alternate data streams such as the
    /// "downloaded from" note, and file timestamps.
    Windows,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Location,
        Category::People,
        Category::Device,
        Category::Dates,
        Category::Software,
        Category::Description,
        Category::Hidden,
        Category::Technical,
        Category::Windows,
    ];

    /// The heading in the details panel.
    pub fn title(self) -> &'static str {
        match self {
            Category::Location => "Where it was taken",
            Category::People => "Who made it",
            Category::Device => "Device",
            Category::Dates => "Dates",
            Category::Software => "Software",
            Category::Description => "Titles, comments and tags",
            Category::Hidden => "Hidden content",
            Category::Technical => "File details",
            Category::Windows => "Windows traces",
        }
    }

    /// One line explaining the group, for the clean dialog.
    pub fn blurb(self) -> &'static str {
        match self {
            Category::Location => "GPS position, place names, altitude",
            Category::People => "Author, artist, last edited by, company, comment authors",
            Category::Device => "Camera or phone model, lens, serial numbers, printer",
            Category::Dates => "When it was taken, created, edited or printed",
            Category::Software => "Programs and versions used",
            Category::Description => "Title, subject, comments, keywords, ratings",
            Category::Hidden => {
                "Thumbnails, custom properties, template paths, tracked changes, notes, cover art"
            }
            Category::Technical => "Size, dimensions, codec (kept)",
            Category::Windows => "\"Downloaded from\" notes and other alternate streams",
        }
    }

    /// A short key for JSON, CSV and the command line.
    pub fn key(self) -> &'static str {
        match self {
            Category::Location => "location",
            Category::People => "people",
            Category::Device => "device",
            Category::Dates => "dates",
            Category::Software => "software",
            Category::Description => "description",
            Category::Hidden => "hidden",
            Category::Technical => "technical",
            Category::Windows => "windows",
        }
    }

    pub fn from_key(key: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.key() == key)
    }

    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// A set of categories (what a clean removes).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
pub struct CatSet(pub u16);

impl CatSet {
    pub const EMPTY: CatSet = CatSet(0);

    pub fn of(cats: &[Category]) -> CatSet {
        CatSet(cats.iter().fold(0, |acc, c| acc | c.bit()))
    }
    pub fn contains(self, c: Category) -> bool {
        self.0 & c.bit() != 0
    }
    pub fn insert(&mut self, c: Category) {
        self.0 |= c.bit();
    }
    pub fn remove(&mut self, c: Category) {
        self.0 &= !c.bit();
    }
    pub fn set(&mut self, c: Category, on: bool) {
        if on {
            self.insert(c)
        } else {
            self.remove(c)
        }
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn iter(self) -> impl Iterator<Item = Category> {
        Category::ALL.into_iter().filter(move |c| self.contains(*c))
    }
}

/// The privacy badges a file can carry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Flag {
    /// "Has location": GPS or a place.
    Location,
    /// "Has names": a person's or company's name, a user account in a path.
    Names,
    /// "Has device serial": a serial number or unique device / document id.
    Serial,
    /// "Has comments": comments, descriptions, notes, tracked changes.
    Comments,
    /// "Has hidden content": thumbnails, hidden sheets, custom properties.
    Hidden,
}

impl Flag {
    pub const ALL: [Flag; 5] = [
        Flag::Location,
        Flag::Names,
        Flag::Serial,
        Flag::Comments,
        Flag::Hidden,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Flag::Location => "Location",
            Flag::Names => "Names",
            Flag::Serial => "Serial",
            Flag::Comments => "Comments",
            Flag::Hidden => "Hidden",
        }
    }

    pub fn long_label(self) -> &'static str {
        match self {
            Flag::Location => "Has location",
            Flag::Names => "Has names",
            Flag::Serial => "Has device serial",
            Flag::Comments => "Has comments",
            Flag::Hidden => "Has hidden content",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Flag::Location => "location",
            Flag::Names => "names",
            Flag::Serial => "serial",
            Flag::Comments => "comments",
            Flag::Hidden => "hidden",
        }
    }
}

/// The common fields that can be edited. Each format lists the ones it
/// supports in [`Extract::editable`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EditKey {
    Title,
    /// Author / creator; "Artist" for audio.
    Author,
    Copyright,
    /// Description / comments.
    Description,
    /// Comma-separated keywords.
    Keywords,
    Subject,
    Company,
    /// Date taken (images), created (documents), recorded (audio/video).
    /// Value format: `YYYY-MM-DD HH:MM:SS` (local time, as cameras store it).
    DateTaken,
    /// GPS position. Value format: `lat, lon` in decimal degrees.
    Gps,
    Album,
    Genre,
    /// Track number, e.g. `3` or `3/12`.
    Track,
    /// Year, e.g. `2024`.
    Year,
}

impl EditKey {
    pub const ALL: [EditKey; 13] = [
        EditKey::Title,
        EditKey::Author,
        EditKey::Copyright,
        EditKey::Description,
        EditKey::Keywords,
        EditKey::Subject,
        EditKey::Company,
        EditKey::DateTaken,
        EditKey::Gps,
        EditKey::Album,
        EditKey::Genre,
        EditKey::Track,
        EditKey::Year,
    ];

    /// The label in the editor; `audio` switches Author to "Artist".
    pub fn label(self, kind: Kind) -> &'static str {
        match self {
            EditKey::Title => "Title",
            EditKey::Author if kind == Kind::Audio => "Artist",
            EditKey::Author => "Author",
            EditKey::Copyright => "Copyright",
            EditKey::Description if kind == Kind::Audio => "Comment",
            EditKey::Description => "Description",
            EditKey::Keywords => "Keywords",
            EditKey::Subject => "Subject",
            EditKey::Company => "Company",
            EditKey::DateTaken => match kind {
                Kind::Image => "Date taken",
                Kind::Document => "Date created",
                _ => "Date recorded",
            },
            EditKey::Gps => "GPS position",
            EditKey::Album => "Album",
            EditKey::Genre => "Genre",
            EditKey::Track => "Track",
            EditKey::Year => "Year",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            EditKey::DateTaken => "YYYY-MM-DD HH:MM:SS",
            EditKey::Gps => "latitude, longitude  (e.g. 36.1901, 44.0091)",
            EditKey::Keywords => "comma-separated",
            EditKey::Track => "3 or 3/12",
            EditKey::Year => "2026",
            _ => "",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            EditKey::Title => "title",
            EditKey::Author => "author",
            EditKey::Copyright => "copyright",
            EditKey::Description => "description",
            EditKey::Keywords => "keywords",
            EditKey::Subject => "subject",
            EditKey::Company => "company",
            EditKey::DateTaken => "date",
            EditKey::Gps => "gps",
            EditKey::Album => "album",
            EditKey::Genre => "genre",
            EditKey::Track => "track",
            EditKey::Year => "year",
        }
    }

    pub fn from_key(key: &str) -> Option<EditKey> {
        EditKey::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// One change: `None` removes the field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Edit {
    pub key: EditKey,
    pub value: Option<String>,
}

impl Edit {
    pub fn set(key: EditKey, value: &str) -> Edit {
        Edit {
            key,
            value: Some(value.to_string()),
        }
    }
    pub fn clear(key: EditKey) -> Edit {
        Edit { key, value: None }
    }
}

/// One piece of metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Stable technical id, unique within the file where possible, e.g.
    /// `exif:0x0110`, `xmp:dc:creator`, `ooxml:core:lastModifiedBy`,
    /// `pdf:info:Author`, `id3:TPE1`, `ads:Zone.Identifier`.
    pub id: String,
    /// Plain-language label: "Camera model", "Last saved by".
    pub label: String,
    /// The value as text (lists joined with "; ", binary summarised such as
    /// "JPEG picture, 160 x 120").
    pub value: String,
    pub category: Category,
    /// Where it is stored: "EXIF", "XMP", "IPTC", "ICC profile",
    /// "PNG text", "ID3v2", "Office properties", "PDF Info", ...
    pub source: String,
    pub flag: Option<Flag>,
    /// The common field that edits this one, if any.
    pub edit: Option<EditKey>,
    /// False for things a clean cannot or should not remove (file size,
    /// dimensions, Windows file timestamps). The verification ignores these.
    pub removable: bool,
}

impl Field {
    /// A removable field without flag or edit key.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
        category: Category,
        source: impl Into<String>,
    ) -> Field {
        Field {
            id: id.into(),
            label: label.into(),
            value: value.into(),
            category,
            source: source.into(),
            flag: None,
            edit: None,
            removable: true,
        }
    }
    pub fn flag(mut self, flag: Flag) -> Field {
        self.flag = Some(flag);
        self
    }
    pub fn edit(mut self, key: EditKey) -> Field {
        self.edit = Some(key);
        self
    }
    pub fn fixed(mut self) -> Field {
        self.removable = false;
        self
    }
}

/// A position in decimal degrees.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    /// Parses `lat, lon` (also `lat lon`, `lat;lon`).
    pub fn parse(text: &str) -> Option<GeoPoint> {
        let parts: Vec<f64> = text
            .split([',', ';', ' '])
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().parse::<f64>())
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        match parts[..] {
            [lat, lon] if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) => {
                Some(GeoPoint { lat, lon })
            }
            _ => None,
        }
    }

    pub fn display(&self) -> String {
        format!("{:.6}, {:.6}", self.lat, self.lon)
    }

    /// A map link, opened in the default browser only when the user clicks it.
    pub fn map_url(&self) -> String {
        format!(
            "https://www.openstreetmap.org/?mlat={:.6}&mlon={:.6}#map=15/{:.6}/{:.6}",
            self.lat, self.lon, self.lat, self.lon
        )
    }
}

/// What a format module reads from one file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Extract {
    pub fields: Vec<Field>,
    /// Plain-language notes, e.g. "2 hidden sheets", "The embedded thumbnail
    /// may show the picture before it was cropped".
    pub notes: Vec<String>,
    pub gps: Option<GeoPoint>,
    /// The best "when was this made" date, `YYYY-MM-DD HH:MM:SS` (or a
    /// shorter prefix such as `YYYY-MM-DD` / `YYYY`). Used for renaming and
    /// for "copy date to file".
    pub taken: Option<String>,
    /// Common fields this format can edit.
    pub editable: Vec<EditKey>,
}

/// Broad kinds of files.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Kind {
    Image,
    Document,
    Audio,
    Video,
    Other,
}

/// Everything known about one file: the format's extract plus Windows traces.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub path: PathBuf,
    pub format: crate::detect::Format,
    pub size: u64,
    pub fields: Vec<Field>,
    pub notes: Vec<String>,
    pub gps: Option<GeoPoint>,
    pub taken: Option<String>,
    pub editable: Vec<EditKey>,
    /// Set when the file could not be read (the fields may still hold the
    /// Windows traces).
    pub error: Option<String>,
}

impl Meta {
    pub fn kind(&self) -> Kind {
        self.format.kind()
    }

    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// The badges, in a fixed order.
    pub fn flags(&self) -> Vec<Flag> {
        let mut out: Vec<Flag> = Flag::ALL
            .into_iter()
            .filter(|f| {
                self.fields
                    .iter()
                    .any(|x| x.flag == Some(*f) && !x.value.trim().is_empty())
            })
            .collect();
        if self.gps.is_some() && !out.contains(&Flag::Location) {
            out.insert(0, Flag::Location);
        }
        out
    }

    /// No badge at all: nothing personal found.
    pub fn is_clean(&self) -> bool {
        self.error.is_none() && self.flags().is_empty()
    }

    /// Fields a clean could remove, in a category.
    pub fn removable_in(&self, cat: Category) -> usize {
        self.fields
            .iter()
            .filter(|f| f.category == cat && f.removable)
            .count()
    }

    /// All removable fields (what "Remove everything" would take).
    pub fn removable_count(&self) -> usize {
        self.fields.iter().filter(|f| f.removable).count()
    }

    pub fn by_category(&self, cat: Category) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(move |f| f.category == cat)
    }
}

/// The ready-made cleaning choices.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Preset {
    /// Every category except the intrinsic file details.
    Everything,
    /// GPS and place names, and people's names. Dates and devices stay.
    LocationAndNames,
    /// The categories picked by hand.
    Custom,
}

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::Everything => "Remove everything",
            Preset::LocationAndNames => "Remove location and names only",
            Preset::Custom => "Custom",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Preset::Everything => "all",
            Preset::LocationAndNames => "location-names",
            Preset::Custom => "custom",
        }
    }
    pub fn from_key(key: &str) -> Option<Preset> {
        match key {
            "all" | "everything" => Some(Preset::Everything),
            "location-names" | "location" | "names" => Some(Preset::LocationAndNames),
            "custom" => Some(Preset::Custom),
            _ => None,
        }
    }
    /// The categories a preset removes (`Custom` returns `custom`).
    pub fn categories(self, custom: CatSet) -> CatSet {
        match self {
            Preset::Everything => CatSet::of(&[
                Category::Location,
                Category::People,
                Category::Device,
                Category::Dates,
                Category::Software,
                Category::Description,
                Category::Hidden,
                Category::Windows,
            ]),
            Preset::LocationAndNames => CatSet::of(&[Category::Location, Category::People]),
            Preset::Custom => custom,
        }
    }
}

/// What a clean removes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CleanOptions {
    pub remove: CatSet,
    /// Keep the orientation tag of photos, so they are not shown sideways.
    pub keep_orientation: bool,
    /// Keep the colour profile, so colours do not shift.
    pub keep_color_profile: bool,
}

impl CleanOptions {
    pub fn everything() -> CleanOptions {
        CleanOptions {
            remove: Preset::Everything.categories(CatSet::EMPTY),
            keep_orientation: true,
            keep_color_profile: true,
        }
    }

    pub fn only(cats: &[Category]) -> CleanOptions {
        CleanOptions {
            remove: CatSet::of(cats),
            keep_orientation: true,
            keep_color_profile: true,
        }
    }

    /// Does this remove every category a format module handles
    /// (all but Technical and Windows)? Modules may then drop whole blocks.
    pub fn is_everything(&self) -> bool {
        [
            Category::Location,
            Category::People,
            Category::Device,
            Category::Dates,
            Category::Software,
            Category::Description,
            Category::Hidden,
        ]
        .into_iter()
        .all(|c| self.remove.contains(c))
    }

    pub fn removes(&self, cat: Category) -> bool {
        self.remove.contains(cat)
    }
}

/// Error text for the user.
pub type Result<T> = std::result::Result<T, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catset_and_presets() {
        let all = Preset::Everything.categories(CatSet::EMPTY);
        assert!(all.contains(Category::Location));
        assert!(!all.contains(Category::Technical));
        let ln = Preset::LocationAndNames.categories(CatSet::EMPTY);
        assert_eq!(
            ln.iter().collect::<Vec<_>>(),
            [Category::Location, Category::People]
        );
        assert!(CleanOptions::everything().is_everything());
        assert!(!CleanOptions::only(&[Category::Location]).is_everything());
        let mut s = CatSet::EMPTY;
        s.set(Category::Dates, true);
        assert!(s.contains(Category::Dates));
        s.set(Category::Dates, false);
        assert!(s.is_empty());
        for c in Category::ALL {
            assert_eq!(Category::from_key(c.key()), Some(c));
        }
        for k in EditKey::ALL {
            assert_eq!(EditKey::from_key(k.key()), Some(k));
        }
    }

    #[test]
    fn geo_parse() {
        let g = GeoPoint::parse("36.1901, 44.0091").unwrap();
        assert!((g.lat - 36.1901).abs() < 1e-9 && (g.lon - 44.0091).abs() < 1e-9);
        assert!(GeoPoint::parse("-33.9 151.2").is_some());
        assert!(GeoPoint::parse("95, 10").is_none());
        assert!(GeoPoint::parse("abc").is_none());
        assert!(g.map_url().starts_with("https://www.openstreetmap.org/"));
    }
}
