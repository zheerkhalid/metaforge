//! Word, Excel and PowerPoint files (Office Open XML packages).
//!
//! Reads the document properties (core, app, custom), comment and tracked
//! change authors, people lists, template paths, editing-session ids,
//! printer settings, user names in linked file paths, hidden sheets and
//! slides, speaker notes and embedded files. Cleaning rewrites only the
//! parts that hold metadata and keeps the package consistent: a dropped part
//! loses its relationships and content-type entry too.

use super::xmlutil::{self, anonymize_path, collect, esc, transform, user_in_path, Act, El};
use super::zipx::{self, Entry, Zip};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use crate::util::DateTime;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

/// The neutral name cleaning puts where a person's name was.
pub const NEUTRAL: &str = "Author";

const SRC_CORE: &str = "Office properties";
const SRC_CUSTOM: &str = "Custom properties";
const SRC_DOC: &str = "Document content";

const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_DCTERMS: &str = "http://purl.org/dc/terms/";
const NS_CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
const REL_CORE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_APP: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties";
const CT_CORE: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const CT_APP: &str = "application/vnd.openxmlformats-officedocument.extended-properties+xml";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Doc {
    Word,
    Excel,
    PowerPoint,
}

impl Doc {
    fn app(self) -> &'static str {
        match self {
            Doc::Word => "Word",
            Doc::Excel => "Excel",
            Doc::PowerPoint => "PowerPoint",
        }
    }
}

fn doc_kind(names: &[String]) -> Option<Doc> {
    let has = |p: &str| names.iter().any(|n| n.starts_with(p));
    if has("word/") {
        Some(Doc::Word)
    } else if has("xl/") {
        Some(Doc::Excel)
    } else if has("ppt/") {
        Some(Doc::PowerPoint)
    } else {
        None
    }
}

fn open(path: &Path) -> Result<(Zip, Doc)> {
    let z = Zip::open(path)?;
    let kind = doc_kind(z.names())
        .filter(|_| z.has("[Content_Types].xml"))
        .ok_or("This is not a Word, Excel or PowerPoint file.")?;
    Ok((z, kind))
}

// ---------------------------------------------------------------------------
// Package structure: relationships and part names

#[derive(Clone, Debug)]
struct Rel {
    id: String,
    typ: String,
    target: String,
    external: bool,
}

/// The part a relationships file belongs to (`""` for the package).
fn rels_source(rels: &str) -> Option<String> {
    let (dir, file) = rels
        .rsplit_once("_rels/")
        .filter(|(_, f)| f.ends_with(".rels"))?;
    Some(format!("{dir}{}", file.trim_end_matches(".rels")))
}

/// The relationships file of a part.
fn rels_of(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The part name a relative target points to from `source`.
fn resolve(source: &str, target: &str) -> String {
    let target = percent_decode(target);
    if let Some(abs) = target.strip_prefix('/') {
        return normalize(abs.split('/').collect());
    }
    let mut parts: Vec<&str> = source.split('/').collect();
    parts.pop(); // the source file name
    parts.extend(target.split('/'));
    normalize(parts)
}

fn normalize(parts: Vec<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in parts {
        match p {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

fn parse_rels(xml: &str) -> Result<Vec<Rel>> {
    Ok(collect(xml, |n| n == "Relationship")?
        .into_iter()
        .map(|el| Rel {
            id: el.attr("Id").unwrap_or_default().to_string(),
            typ: el.attr("Type").unwrap_or_default().to_string(),
            target: el.attr("Target").unwrap_or_default().to_string(),
            external: el
                .attr("TargetMode")
                .is_some_and(|m| m.eq_ignore_ascii_case("External")),
        })
        .collect())
}

fn rels(z: &mut Zip, part: &str) -> Result<Vec<Rel>> {
    match z.text(&rels_of(part))? {
        Some(xml) => parse_rels(&xml),
        None => Ok(Vec::new()),
    }
}

/// The package-level part with a relationship type ending in `suffix`.
fn package_part(z: &mut Zip, suffix: &str, default: &str) -> Result<String> {
    let found = rels(z, "")?
        .into_iter()
        .find(|r| !r.external && r.typ.ends_with(suffix))
        .map(|r| resolve("", &r.target));
    Ok(found.unwrap_or_else(|| default.to_string()))
}

fn is_thumbnail(name: &str) -> bool {
    name.starts_with("docProps/thumbnail.")
}

fn is_printer(name: &str) -> bool {
    name.contains("printerSettings/")
}

/// Word parts that hold text and therefore comments / tracked changes.
fn is_word_story(name: &str) -> bool {
    let Some(file) = name.strip_prefix("word/") else {
        return false;
    };
    let file = file.strip_prefix("glossary/").unwrap_or(file);
    !file.contains('/')
        && file.ends_with(".xml")
        && ([
            "document.xml",
            "footnotes.xml",
            "endnotes.xml",
            "comments.xml",
        ]
        .contains(&file)
            || file.starts_with("header")
            || file.starts_with("footer"))
}

fn is_xl_comments(name: &str) -> bool {
    name.starts_with("xl/comments") && name.ends_with(".xml") && !name.contains("/_rels/")
}

fn in_dir_xml(name: &str, dir: &str) -> bool {
    name.starts_with(dir) && name.ends_with(".xml") && !name[dir.len()..].contains('/')
}

fn picture_kind(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpeg" | "jpg") => "JPEG picture",
        Some("png") => "PNG picture",
        Some("wmf") => "WMF picture",
        Some("emf") => "EMF picture",
        _ => "Picture",
    }
}

// ---------------------------------------------------------------------------
// Property tables

struct Prop {
    local: &'static str,
    label: &'static str,
    cat: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
}

const fn prop(
    local: &'static str,
    label: &'static str,
    cat: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
) -> Prop {
    Prop {
        local,
        label,
        cat,
        flag,
        edit,
    }
}

use Category::{Dates, Description, Hidden, People, Software};

const CORE: &[Prop] = &[
    prop(
        "creator",
        "Author",
        People,
        Some(Flag::Names),
        Some(EditKey::Author),
    ),
    prop(
        "lastModifiedBy",
        "Last saved by",
        People,
        Some(Flag::Names),
        None,
    ),
    prop("title", "Title", Description, None, Some(EditKey::Title)),
    prop(
        "subject",
        "Subject",
        Description,
        None,
        Some(EditKey::Subject),
    ),
    prop(
        "description",
        "Comments",
        Description,
        Some(Flag::Comments),
        Some(EditKey::Description),
    ),
    prop(
        "keywords",
        "Keywords",
        Description,
        None,
        Some(EditKey::Keywords),
    ),
    prop("category", "Category", Description, None, None),
    prop("contentStatus", "Status", Description, None, None),
    prop("language", "Language", Description, None, None),
    prop("version", "Version number", Hidden, None, None),
    prop(
        "identifier",
        "Document ID",
        Hidden,
        Some(Flag::Serial),
        None,
    ),
    prop("revision", "Revision number", Hidden, None, None),
    prop("created", "Created", Dates, None, Some(EditKey::DateTaken)),
    prop("modified", "Modified", Dates, None, None),
    prop("lastPrinted", "Last printed", Dates, None, None),
];

const APP: &[Prop] = &[
    prop("Application", "Program used", Software, None, None),
    prop("AppVersion", "Program version", Software, None, None),
    prop(
        "Company",
        "Company",
        People,
        Some(Flag::Names),
        Some(EditKey::Company),
    ),
    prop("Manager", "Manager", People, Some(Flag::Names), None),
    prop("Template", "Template name", Hidden, None, None),
    prop("TotalTime", "Total editing time", Hidden, None, None),
    prop("HyperlinkBase", "Hyperlink base", Hidden, None, None),
];

/// Document statistics shown as fixed facts.
const STATS: &[(&str, &str)] = &[("Pages", "Pages"), ("Words", "Words"), ("Slides", "Slides")];

fn find(table: &'static [Prop], local: &str) -> Option<&'static Prop> {
    table.iter().find(|p| p.local == local)
}

/// The core.xml element name written for an edit key.
fn core_element(key: EditKey) -> Option<(&'static str, &'static str)> {
    Some(match key {
        EditKey::Title => ("title", "dc:title"),
        EditKey::Author => ("creator", "dc:creator"),
        EditKey::Subject => ("subject", "dc:subject"),
        EditKey::Description => ("description", "dc:description"),
        EditKey::Keywords => ("keywords", "cp:keywords"),
        EditKey::DateTaken => ("created", "dcterms:created"),
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Reading

struct Reader<'a> {
    x: &'a mut Extract,
}

impl Reader<'_> {
    fn push(&mut self, field: Field) {
        if !field.value.trim().is_empty() {
            self.x.fields.push(field);
        }
    }
}

fn field(
    id: impl Into<String>,
    label: &str,
    value: impl Into<String>,
    cat: Category,
    src: &str,
) -> Field {
    Field::new(id, label, value, cat, src)
}

fn with(mut f: Field, flag: Option<Flag>, edit: Option<EditKey>) -> Field {
    f.flag = flag;
    f.edit = edit;
    f
}

fn date_text(v: &str) -> String {
    DateTime::parse(v)
        .map(|d| d.text())
        .unwrap_or_else(|| v.trim().to_string())
}

/// Distinct non-empty names other than the neutral placeholder.
fn names<'a>(it: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for n in it.map(str::trim) {
        if !n.is_empty() && n != NEUTRAL && seen.insert(n.to_string()) {
            out.push(n.to_string());
        }
    }
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

pub fn read(path: &Path) -> Result<Extract> {
    let (mut z, doc) = open(path)?;
    let mut x = Extract {
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Subject,
            EditKey::Description,
            EditKey::Keywords,
            EditKey::Company,
            EditKey::DateTaken,
        ],
        ..Extract::default()
    };
    let names = z.names().to_vec();
    let mut r = Reader { x: &mut x };
    r.x.fields.push(
        field(
            "ooxml:type",
            "Document type",
            format!("{} document", doc.app()),
            Category::Technical,
            SRC_CORE,
        )
        .fixed(),
    );
    let mut user_paths: Vec<String> = Vec::new();

    // Core properties.
    let core = package_part(&mut z, "/metadata/core-properties", "docProps/core.xml")?;
    if let Some(xml) = z.text(&core)? {
        for el in collect(&xml, |n| find(CORE, n).is_some())? {
            if el.path.len() != 1 {
                continue;
            }
            let p = find(CORE, el.local()).expect("filtered");
            let value = if p.cat == Dates {
                date_text(&el.text)
            } else {
                el.text.trim().to_string()
            };
            if p.local == "created" && !value.is_empty() {
                r.x.taken = Some(value.clone());
            }
            r.push(with(
                field(
                    format!("ooxml:core:{}", p.local),
                    p.label,
                    value,
                    p.cat,
                    SRC_CORE,
                ),
                p.flag,
                p.edit,
            ));
        }
    }

    // Application properties.
    let app = package_part(&mut z, "/extended-properties", "docProps/app.xml")?;
    if let Some(xml) = z.text(&app)? {
        for el in collect(&xml, |n| {
            find(APP, n).is_some() || STATS.iter().any(|s| s.0 == n)
        })? {
            if el.path.len() != 1 {
                continue;
            }
            let value = el.text.trim().to_string();
            if let Some((_, label)) = STATS.iter().find(|s| s.0 == el.local()) {
                r.push(
                    field(
                        format!("ooxml:app:{}", el.local()),
                        label,
                        value,
                        Category::Technical,
                        SRC_CORE,
                    )
                    .fixed(),
                );
                continue;
            }
            let p = find(APP, el.local()).expect("filtered");
            let mut flag = p.flag;
            let shown = match p.local {
                "TotalTime" => match value.parse::<u64>() {
                    Ok(0) => String::new(),
                    Ok(m) => plural(m as usize, "minute", "minutes"),
                    Err(_) => value.clone(),
                },
                "Template" | "HyperlinkBase" => {
                    if user_in_path(&value).is_some() {
                        user_paths.push(value.clone());
                        flag = Some(Flag::Names);
                    }
                    value.clone()
                }
                _ => value.clone(),
            };
            r.push(with(
                field(
                    format!("ooxml:app:{}", p.local),
                    p.label,
                    shown,
                    p.cat,
                    SRC_CORE,
                ),
                flag,
                p.edit,
            ));
        }
    }

    // Custom properties.
    let custom = package_part(&mut z, "/custom-properties", "docProps/custom.xml")?;
    if let Some(xml) = z.text(&custom)? {
        for el in collect(&xml, |n| n == "property")? {
            let name = el.attr("name").unwrap_or("(unnamed)");
            r.push(
                field(
                    format!("ooxml:custom:{name}"),
                    &format!("Custom property \"{name}\""),
                    el.text.trim(),
                    Hidden,
                    SRC_CUSTOM,
                )
                .flag(Flag::Hidden),
            );
        }
    }

    // Thumbnail.
    for name in names.iter().filter(|n| is_thumbnail(n)) {
        let size = z.size(name).unwrap_or(0);
        r.push(
            field(
                format!("ooxml:thumbnail:{name}"),
                "Thumbnail",
                format!("{}, {}", picture_kind(name), crate::util::human_size(size)),
                Hidden,
                SRC_CORE,
            )
            .flag(Flag::Hidden),
        );
    }

    // Printer settings.
    let printers = names.iter().filter(|n| is_printer(n)).count();
    if printers > 0 {
        r.push(
            field(
                "ooxml:printer",
                "Printer settings saved",
                plural(printers, "part", "parts"),
                Category::Device,
                SRC_DOC,
            )
            .flag(Flag::Hidden),
        );
    }

    // Comments, people, tracked changes.
    let mut authors: Vec<String> = Vec::new();
    let mut initials: Vec<String> = Vec::new();
    let mut accounts: Vec<String> = Vec::new();
    let mut comments = 0usize;
    let mut changes = 0usize;
    let mut change_authors: Vec<String> = Vec::new();
    let mut dates: Vec<String> = Vec::new();
    for name in &names {
        let interesting = is_word_story(name)
            || name == "word/people.xml"
            || name == "word/commentsExtensible.xml"
            || is_xl_comments(name)
            || in_dir_xml(name, "xl/threadedComments/")
            || in_dir_xml(name, "xl/persons/")
            || name == "ppt/commentAuthors.xml"
            || name == "ppt/authors.xml"
            || in_dir_xml(name, "ppt/comments/");
        if !interesting {
            continue;
        }
        let Some(xml) = z.text(name)? else { continue };
        let els = collect(&xml, |_| true)?;
        for el in &els {
            let local = el.local();
            match name.as_str() {
                n if n.starts_with("word/") => {
                    if let Some(d) = el.attr("w:date").or_else(|| el.attr_local("dateUtc")) {
                        dates.push(date_text(d));
                    }
                    if local == "person" {
                        authors.extend(el.attr_local("author").map(String::from));
                    } else if local == "presenceInfo" {
                        accounts.extend(el.attr_local("userId").map(String::from));
                    } else if local == "comment" && n.ends_with("comments.xml") {
                        comments += 1;
                        authors.extend(el.attr("w:author").map(String::from));
                        initials.extend(el.attr("w:initials").map(String::from));
                    } else if let Some(a) = el.attr("w:author") {
                        change_authors.push(a.to_string());
                        if matches!(local, "ins" | "del" | "moveFrom" | "moveTo") {
                            changes += 1;
                        }
                    }
                }
                n if n.starts_with("xl/") => match local {
                    "author" if el.inside("authors") => authors.push(el.text.clone()),
                    "comment" => comments += 1,
                    "person" => {
                        authors.extend(el.attr("displayName").map(String::from));
                        accounts.extend(el.attr("userId").map(String::from));
                    }
                    _ => {}
                },
                _ => match local {
                    "cmAuthor" | "author" => {
                        authors.extend(el.attr("name").map(String::from));
                        initials.extend(el.attr("initials").map(String::from));
                        accounts.extend(el.attr("userId").map(String::from));
                    }
                    "cm" => comments += 1,
                    _ => {}
                },
            }
        }
    }
    let authors = names_of(&authors);
    if !authors.is_empty() {
        r.push(
            field(
                "ooxml:comments:authors",
                "Comment authors",
                authors.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    let initials = names_of(&initials);
    if !initials.is_empty() {
        r.push(
            field(
                "ooxml:comments:initials",
                "Comment author initials",
                initials.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    let accounts = names_of(&accounts);
    if !accounts.is_empty() {
        r.push(
            field(
                "ooxml:comments:accounts",
                "Comment author accounts",
                accounts.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    let change_authors = names_of(&change_authors);
    if !change_authors.is_empty() {
        r.push(
            field(
                "ooxml:changes:authors",
                "Tracked change authors",
                change_authors.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    if !dates.is_empty() {
        dates.sort();
        let span = if dates.len() == 1 || dates[0] == dates[dates.len() - 1] {
            dates[0].clone()
        } else {
            format!("{} to {}", dates[0], dates[dates.len() - 1])
        };
        r.push(field(
            "ooxml:changes:dates",
            "Comment and change dates",
            span,
            Dates,
            SRC_DOC,
        ));
    }
    if comments > 0 {
        r.push(
            field(
                "ooxml:comments:count",
                "Comments",
                plural(comments, "comment", "comments"),
                Description,
                SRC_DOC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
        r.x.notes.push(format!(
            "{}: their author names are removed by cleaning, the comments themselves stay; delete them in {} if they should not be shared.",
            plural(comments, "comment", "comments"),
            doc.app()
        ));
    }
    if changes > 0 {
        r.push(
            field(
                "ooxml:changes:count",
                "Tracked changes",
                plural(changes, "change", "changes"),
                Hidden,
                SRC_DOC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
        r.x.notes.push(format!(
            "{}: the names of who made them are removed by cleaning, the changes themselves stay; accept or reject them in Word before sharing.",
            plural(changes, "tracked change", "tracked changes")
        ));
    }

    // Word settings: template, editing-session ids, document variables.
    if doc == Doc::Word {
        if let Some(xml) = z.text("word/settings.xml")? {
            let settings_rels = rels(&mut z, "word/settings.xml")?;
            let els = collect(&xml, |n| {
                matches!(n, "attachedTemplate" | "rsid" | "rsidRoot" | "docVar")
            })?;
            for el in els.iter().filter(|e| e.local() == "attachedTemplate") {
                let id = el.attr_local("id").unwrap_or_default();
                if let Some(rel) = settings_rels.iter().find(|r| r.id == id) {
                    let flag = if user_in_path(&rel.target).is_some() {
                        Flag::Names
                    } else {
                        Flag::Hidden
                    };
                    r.push(
                        field(
                            "ooxml:settings:attachedTemplate",
                            "Template",
                            rel.target.clone(),
                            Hidden,
                            SRC_DOC,
                        )
                        .flag(flag),
                    );
                }
            }
            let rsids = els
                .iter()
                .filter(|e| e.local() != "attachedTemplate" && e.local() != "docVar")
                .count();
            if rsids > 0 {
                r.push(
                    field(
                        "ooxml:settings:rsids",
                        "Editing session ids",
                        plural(rsids, "id", "ids"),
                        Hidden,
                        SRC_DOC,
                    )
                    .flag(Flag::Hidden),
                );
            }
            let vars: Vec<String> = els
                .iter()
                .filter(|e| e.local() == "docVar")
                .map(|e| {
                    format!(
                        "{} = {}",
                        e.attr_local("name").unwrap_or(""),
                        e.attr_local("val").unwrap_or("")
                    )
                })
                .collect();
            if !vars.is_empty() {
                r.push(
                    field(
                        "ooxml:settings:docVars",
                        "Document variables",
                        vars.join("; "),
                        Hidden,
                        SRC_DOC,
                    )
                    .flag(Flag::Hidden),
                );
            }
        }
    }

    // Excel: saved folder path, hidden sheets.
    if doc == Doc::Excel {
        if let Some(xml) = z.text("xl/workbook.xml")? {
            let els = collect(&xml, |n| matches!(n, "absPath" | "sheet"))?;
            for el in &els {
                if el.local() == "absPath" {
                    let url = el.attr("url").unwrap_or_default();
                    if user_in_path(url).is_some() {
                        user_paths.push(url.to_string());
                    }
                    r.push(
                        field(
                            "ooxml:workbook:absPath",
                            "Folder the file was saved in",
                            url,
                            Hidden,
                            SRC_DOC,
                        )
                        .flag(Flag::Hidden),
                    );
                }
            }
            let hidden: Vec<&str> = els
                .iter()
                .filter(|e| {
                    e.local() == "sheet" && matches!(e.attr("state"), Some("hidden" | "veryHidden"))
                })
                .map(|e| e.attr("name").unwrap_or("?"))
                .collect();
            if !hidden.is_empty() {
                r.push(
                    field(
                        "ooxml:workbook:hiddenSheets",
                        "Hidden sheets",
                        hidden.join("; "),
                        Hidden,
                        SRC_DOC,
                    )
                    .flag(Flag::Hidden)
                    .fixed(),
                );
                r.x.notes.push(format!(
                    "{} ({}): their content is still in the file. Unhide and check them in Excel, or delete them, before sharing.",
                    plural(hidden.len(), "hidden sheet", "hidden sheets"),
                    hidden.join(", ")
                ));
            }
        }
    }

    // PowerPoint: hidden slides, speaker notes.
    if doc == Doc::PowerPoint {
        let mut hidden_slides = 0;
        for name in names.iter().filter(|n| in_dir_xml(n, "ppt/slides/")) {
            if let Some(xml) = z.text(name)? {
                let root = collect(&xml, |n| n == "sld")?;
                if root
                    .first()
                    .and_then(|e| e.attr("show"))
                    .is_some_and(|v| v == "0" || v == "false")
                {
                    hidden_slides += 1;
                }
            }
        }
        if hidden_slides > 0 {
            r.push(
                field(
                    "ooxml:ppt:hiddenSlides",
                    "Hidden slides",
                    plural(hidden_slides, "slide", "slides"),
                    Hidden,
                    SRC_DOC,
                )
                .flag(Flag::Hidden)
                .fixed(),
            );
            r.x.notes.push(format!(
                "{}: they are not shown in the slide show but are in the file. Check or delete them in PowerPoint before sharing.",
                plural(hidden_slides, "hidden slide", "hidden slides")
            ));
        }
        let notes = names
            .iter()
            .filter(|n| in_dir_xml(n, "ppt/notesSlides/"))
            .count();
        if notes > 0 {
            r.push(
                field(
                    "ooxml:ppt:notes",
                    "Speaker notes",
                    plural(notes, "slide has notes", "slides have notes"),
                    Hidden,
                    SRC_DOC,
                )
                .flag(Flag::Comments)
                .fixed(),
            );
            r.x.notes.push(format!(
                "Speaker notes on {}: they are part of the presentation and stay. Remove them in PowerPoint if they should not be shared.",
                plural(notes, "slide", "slides")
            ));
        }
    }

    // External links: user names in paths, links to local files.
    let mut local_links = 0;
    for name in names.iter().filter(|n| n.ends_with(".rels")) {
        let Some(xml) = z.text(name)? else { continue };
        for rel in parse_rels(&xml)?.into_iter().filter(|r| r.external) {
            if user_in_path(&rel.target).is_some() {
                user_paths.push(rel.target.clone());
            }
            let lower = rel.target.to_ascii_lowercase();
            let web = ["http:", "https:", "mailto:", "ftp:", "#"]
                .iter()
                .any(|p| lower.starts_with(p));
            if !web && !rel.typ.ends_with("/attachedTemplate") {
                local_links += 1;
            }
        }
    }
    let users = names_of(
        &user_paths
            .iter()
            .filter_map(|p| user_in_path(p))
            .collect::<Vec<_>>(),
    );
    if !users.is_empty() {
        r.push(
            field(
                "ooxml:paths:users",
                "User names in file paths",
                users.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    if local_links > 0 {
        r.push(
            field(
                "ooxml:links:local",
                "Links to files on a computer",
                plural(local_links, "link", "links"),
                Hidden,
                SRC_DOC,
            )
            .flag(Flag::Hidden)
            .fixed(),
        );
        r.x.notes.push(format!(
            "{} to files on a computer: cleaning hides the user name in them but keeps the links. Remove them in {} if the paths should not be shared.",
            plural(local_links, "link points", "links point"),
            doc.app()
        ));
    }

    // Embedded files and macros.
    let embedded = names
        .iter()
        .filter(|n| n.contains("/embeddings/") && !n.ends_with('/'))
        .count();
    if embedded > 0 {
        r.push(
            field(
                "ooxml:embeddings",
                "Embedded files",
                plural(embedded, "file", "files"),
                Hidden,
                SRC_DOC,
            )
            .flag(Flag::Hidden)
            .fixed(),
        );
        r.x.notes.push(format!(
            "{} inside: they can carry their own metadata. Check them separately before sharing.",
            plural(embedded, "embedded file is", "embedded files are")
        ));
    }
    if names.iter().any(|n| n.ends_with("vbaProject.bin")) {
        r.x.notes
            .push("The file contains macros (program code).".into());
    }
    Ok(x)
}

fn names_of(list: &[String]) -> Vec<String> {
    names(list.iter().map(String::as_str))
}

// ---------------------------------------------------------------------------
// Cleaning

fn core_act(el: &El, opts: &CleanOptions) -> Act {
    match find(CORE, el.local()) {
        Some(p) if el.path.len() == 1 && opts.removes(p.cat) => Act::Drop,
        _ => Act::Keep,
    }
}

fn app_act(el: &El, opts: &CleanOptions) -> Act {
    match find(APP, el.local()) {
        Some(p) if el.path.len() == 1 && opts.removes(p.cat) => Act::Drop,
        _ => Act::Keep,
    }
}

/// Attribute changes on a Word story / comments part.
fn word_act(el: &El, opts: &CleanOptions) -> Act {
    let mut changes: Vec<(String, Option<String>)> = Vec::new();
    for (k, _) in &el.attrs {
        match k.as_str() {
            "w:author" if opts.removes(People) => changes.push((k.clone(), Some(NEUTRAL.into()))),
            "w:initials" if opts.removes(People) => changes.push((k.clone(), None)),
            "w:date" if opts.removes(Dates) => changes.push((k.clone(), None)),
            k2 if xmlutil::local(k2) == "dateUtc" && opts.removes(Dates) => {
                changes.push((k.clone(), None))
            }
            k2 if k2.starts_with("w:rsid") && opts.removes(Hidden) => {
                changes.push((k.clone(), None))
            }
            _ => {}
        }
    }
    if changes.is_empty() {
        Act::Keep
    } else {
        Act::Attrs(changes)
    }
}

/// Fills in the text of the elements a transform is asked about. The
/// streaming transform does not see element text; for the few text-only
/// elements whose text it rewrites, read it first.
fn texts_by_position(xml: &str, local: &str) -> Result<Vec<String>> {
    Ok(collect(xml, |n| n == local)?
        .into_iter()
        .map(|e| e.text)
        .collect())
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let (mut z, doc) = open(src)?;
    let names = z.names().to_vec();
    let core = package_part(&mut z, "/metadata/core-properties", "docProps/core.xml")?;
    let app = package_part(&mut z, "/extended-properties", "docProps/app.xml")?;
    let custom = package_part(&mut z, "/custom-properties", "docProps/custom.xml")?;

    // Parts dropped whole.
    let mut dropped: HashSet<String> = HashSet::new();
    if opts.removes(Hidden) {
        if z.has(&custom) {
            dropped.insert(custom.clone());
        }
        dropped.extend(names.iter().filter(|n| is_thumbnail(n)).cloned());
    }
    if opts.removes(Category::Device) {
        dropped.extend(names.iter().filter(|n| is_printer(n)).cloned());
    }

    let mut plan: HashMap<String, Entry> = HashMap::new();
    for d in &dropped {
        plan.insert(d.clone(), Entry::Drop);
        plan.insert(rels_of(d), Entry::Drop);
    }

    // Relationships: drop those to dropped parts and the attached template;
    // hide user names in external targets.
    let mut removed_ids: HashMap<String, Vec<String>> = HashMap::new();
    let rels_parts: Vec<String> = names
        .iter()
        .filter(|n| n.ends_with(".rels") && !plan.contains_key(*n))
        .cloned()
        .collect();
    for name in &rels_parts {
        let Some(source) = rels_source(name) else {
            continue;
        };
        let Some(xml) = z.text(name)? else { continue };
        let mut removed = Vec::new();
        let out = transform(
            &xml,
            |el| {
                if el.local() != "Relationship" {
                    return Act::Keep;
                }
                let id = el.attr("Id").unwrap_or_default().to_string();
                let target = el.attr("Target").unwrap_or_default();
                let typ = el.attr("Type").unwrap_or_default();
                let external = el
                    .attr("TargetMode")
                    .is_some_and(|m| m.eq_ignore_ascii_case("External"));
                if (!external && dropped.contains(&resolve(&source, target)))
                    || (opts.removes(Hidden) && typ.ends_with("/attachedTemplate"))
                {
                    removed.push(id);
                    Act::Drop
                } else if external && opts.removes(People) && user_in_path(target).is_some() {
                    Act::Attrs(vec![("Target".into(), Some(anonymize_path(target)))])
                } else {
                    Act::Keep
                }
            },
            &[],
        )?;
        if !removed.is_empty() {
            removed_ids.insert(source.clone(), removed);
        }
        if out != xml {
            plan.insert(name.clone(), Entry::Replace(out.into_bytes()));
        }
    }

    // Content types: no overrides for dropped parts.
    if !dropped.is_empty() {
        let ct = "[Content_Types].xml";
        if let Some(xml) = z.text(ct)? {
            let out = transform(
                &xml,
                |el| match el.attr("PartName") {
                    Some(p)
                        if el.local() == "Override"
                            && dropped
                                .iter()
                                .any(|d| p.trim_start_matches('/').eq_ignore_ascii_case(d)) =>
                    {
                        Act::Drop
                    }
                    _ => Act::Keep,
                },
                &[],
            )?;
            plan.insert(ct.into(), Entry::Replace(out.into_bytes()));
        }
    }

    // Parts with metadata.
    for name in &names {
        if plan.contains_key(name) || name.ends_with(".rels") || !name.ends_with(".xml") {
            continue;
        }
        let ids = removed_ids.get(name).cloned().unwrap_or_default();
        let wanted = *name == core
            || *name == app
            || (doc == Doc::Word
                && (is_word_story(name)
                    || matches!(
                        name.as_str(),
                        "word/people.xml" | "word/settings.xml" | "word/commentsExtensible.xml"
                    )))
            || (doc == Doc::Excel
                && (is_xl_comments(name)
                    || in_dir_xml(name, "xl/persons/")
                    || name == "xl/workbook.xml"))
            || (doc == Doc::PowerPoint
                && matches!(name.as_str(), "ppt/commentAuthors.xml" | "ppt/authors.xml"))
            || !ids.is_empty();
        if !wanted {
            continue;
        }
        let Some(xml) = z.text(name)? else { continue };
        let out = clean_part(name, &xml, opts, &core, &app, &ids)?;
        if out != xml {
            plan.insert(name.clone(), Entry::Replace(out.into_bytes()));
        }
    }
    zipx::rewrite(src, dst, plan, Vec::new(), opts.removes(Dates))
}

fn clean_part(
    name: &str,
    xml: &str,
    opts: &CleanOptions,
    core: &str,
    app: &str,
    removed_ids: &[String],
) -> Result<String> {
    let people = opts.removes(People);
    // Text of elements rewritten from their text (anonymised paths).
    let app_texts: HashMap<String, String> = if name == app {
        ["Template", "HyperlinkBase"]
            .iter()
            .filter_map(|l| {
                texts_by_position(xml, l)
                    .ok()?
                    .into_iter()
                    .next()
                    .map(|t| (l.to_string(), t))
            })
            .collect()
    } else {
        HashMap::new()
    };
    transform(
        xml,
        |el| {
            // A relationship id that no longer exists.
            if !removed_ids.is_empty() {
                if el.local() == "attachedTemplate"
                    && el
                        .attr_local("id")
                        .is_some_and(|id| removed_ids.iter().any(|r| r == id))
                {
                    return Act::Drop;
                }
                let stale: Vec<(String, Option<String>)> = el
                    .attrs
                    .iter()
                    .filter(|(k, v)| k.starts_with("r:") && removed_ids.contains(v))
                    .map(|(k, _)| (k.clone(), None))
                    .collect();
                if !stale.is_empty() {
                    return Act::Attrs(stale);
                }
            }
            let local = el.local();
            if name == core {
                return core_act(el, opts);
            }
            if name == app {
                if el.path.len() == 1 && matches!(local, "Template" | "HyperlinkBase") {
                    if let Some(p) = find(APP, local) {
                        if opts.removes(p.cat) {
                            return Act::Drop;
                        }
                    }
                    if people {
                        let t = app_texts.get(local).cloned().unwrap_or_default();
                        if user_in_path(&t).is_some() {
                            return Act::Text(anonymize_path(&t));
                        }
                    }
                    return Act::Keep;
                }
                return app_act(el, opts);
            }
            match name {
                "word/settings.xml" => match local {
                    "rsids" | "attachedTemplate" | "docVars" if opts.removes(Hidden) => Act::Drop,
                    _ => word_act(el, opts),
                },
                "word/people.xml" => match local {
                    "person" if people => Act::Attrs(
                        el.attrs
                            .iter()
                            .filter(|(k, _)| xmlutil::local(k) == "author")
                            .map(|(k, _)| (k.clone(), Some(NEUTRAL.to_string())))
                            .collect(),
                    ),
                    "presenceInfo" if people => Act::Drop,
                    _ => Act::Keep,
                },
                n if n.starts_with("word/") => word_act(el, opts),
                n if is_xl_comments(n) => match local {
                    "author" if people && el.inside("authors") => Act::Text(NEUTRAL.into()),
                    _ => Act::Keep,
                },
                n if n.starts_with("xl/persons/") => match local {
                    "person" if people => Act::Attrs(vec![
                        ("displayName".into(), Some(NEUTRAL.into())),
                        ("userId".into(), None),
                        ("providerId".into(), None),
                    ]),
                    _ => Act::Keep,
                },
                "xl/workbook.xml" => match local {
                    "absPath" if opts.removes(Hidden) => Act::Drop,
                    "absPath" if people => match el.attr("url") {
                        Some(u) if user_in_path(u).is_some() => {
                            Act::Attrs(vec![("url".into(), Some(anonymize_path(u)))])
                        }
                        _ => Act::Keep,
                    },
                    _ => Act::Keep,
                },
                "ppt/commentAuthors.xml" | "ppt/authors.xml" => match local {
                    "cmAuthor" | "author" if people => {
                        let mut ch = vec![
                            ("name".to_string(), Some(NEUTRAL.to_string())),
                            ("initials".to_string(), Some(String::new())),
                        ];
                        for k in ["userId", "providerId"] {
                            if el.attr(k).is_some() {
                                ch.push((k.into(), Some(String::new())));
                            }
                        }
                        Act::Attrs(ch)
                    }
                    _ => Act::Keep,
                },
                _ => Act::Keep,
            }
        },
        &[],
    )
}

// ---------------------------------------------------------------------------
// Editing

fn date_value(v: &str) -> Result<String> {
    DateTime::parse(v)
        .map(|d| format!("{}Z", d.iso()))
        .ok_or_else(|| format!("\"{v}\" is not a date; use the form YYYY-MM-DD HH:MM:SS."))
}

fn new_core_element(qname: &str, value: &str) -> String {
    let (prefix, local) = qname.split_once(':').unwrap_or(("", qname));
    let ns = match prefix {
        "dc" => NS_DC,
        "dcterms" => NS_DCTERMS,
        _ => NS_CP,
    };
    let extra = if local == "created" {
        format!(" xmlns:xsi=\"{NS_XSI}\" xsi:type=\"dcterms:W3CDTF\"")
    } else {
        String::new()
    };
    format!(
        "<{qname} xmlns:{prefix}=\"{ns}\"{extra}>{}</{qname}>",
        esc(value)
    )
}

/// Sets / removes children of a properties root. `edits`: local name ->
/// (qualified name for a new element, new value or None, raw new element).
fn edit_props(xml: &str, root: &str, edits: &[(&str, Option<String>, String)]) -> Result<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let found: HashSet<String> = collect(xml, |n| edits.iter().any(|e| e.0 == n))?
        .into_iter()
        .filter(|e| e.path.len() == 1)
        .map(|e| e.local().to_string())
        .collect();
    let mut append = String::new();
    for (local, value, raw) in edits {
        if value.is_some() && !found.contains(*local) {
            append.push_str(raw);
        }
    }
    let root_name = collect(xml, |n| n == root)?
        .into_iter()
        .next()
        .map(|e| e.name)
        .ok_or("The document properties are damaged.")?;
    transform(
        xml,
        |el| {
            if el.path.len() != 1 {
                return Act::Keep;
            }
            match edits.iter().find(|e| e.0 == el.local()) {
                // Only the first of duplicates gets the value; the rest go.
                Some((local, Some(v), _)) if seen.insert(*local) => Act::Text(v.clone()),
                Some(_) => Act::Drop,
                None => Act::Keep,
            }
        },
        &[(root_name.as_str(), append)],
    )
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let (mut z, _doc) = open(src)?;
    let mut core_edits: Vec<(&str, Option<String>, String)> = Vec::new();
    let mut app_edits: Vec<(&str, Option<String>, String)> = Vec::new();
    for e in edits {
        let value = e.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        match e.key {
            EditKey::Company => {
                let v = value.map(String::from);
                let raw = format!("<Company>{}</Company>", esc(v.as_deref().unwrap_or("")));
                app_edits.push(("Company", v, raw));
            }
            key => {
                let (local, qname) = core_element(key).ok_or_else(|| {
                    format!(
                        "{} cannot be edited in Office files.",
                        key.label(crate::meta::Kind::Document)
                    )
                })?;
                let v = match (key, value) {
                    (EditKey::DateTaken, Some(v)) => Some(date_value(v)?),
                    (_, v) => v.map(String::from),
                };
                let raw = new_core_element(qname, v.as_deref().unwrap_or(""));
                core_edits.push((local, v, raw));
            }
        }
    }
    let mut plan: HashMap<String, Entry> = HashMap::new();
    let mut add: Vec<(String, Vec<u8>)> = Vec::new();
    let mut new_parts: Vec<(&str, &str, &str)> = Vec::new();
    for (edits, suffix, default, root, rel, ct) in [
        (
            &core_edits,
            "/metadata/core-properties",
            "docProps/core.xml",
            "coreProperties",
            REL_CORE,
            CT_CORE,
        ),
        (
            &app_edits,
            "/extended-properties",
            "docProps/app.xml",
            "Properties",
            REL_APP,
            CT_APP,
        ),
    ] {
        if edits.is_empty() {
            continue;
        }
        let part = package_part(&mut z, suffix, default)?;
        let existing = z.text(&part)?;
        let base = match &existing {
            Some(x) => x.clone(),
            None if root == "coreProperties" => format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"{NS_CP}\" xmlns:dc=\"{NS_DC}\" xmlns:dcterms=\"{NS_DCTERMS}\" xmlns:xsi=\"{NS_XSI}\"></cp:coreProperties>"
            ),
            None => "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"></Properties>".to_string(),
        };
        let out = edit_props(&base, root, edits)?;
        if existing.is_some() {
            plan.insert(part, Entry::Replace(out.into_bytes()));
        } else {
            add.push((default.to_string(), out.into_bytes()));
            new_parts.push((default, rel, ct));
        }
    }
    if !new_parts.is_empty() {
        // Register new parts in the package relationships and content types.
        let rels_xml = z
            .text("_rels/.rels")?
            .ok_or("The package has no relationships part.")?;
        let mut rel_add = String::new();
        let mut ct_add = String::new();
        for (i, (part, rel, ct)) in new_parts.iter().enumerate() {
            rel_add.push_str(&format!(
                "<Relationship Id=\"rIdMetaForge{i}\" Type=\"{rel}\" Target=\"{part}\"/>"
            ));
            ct_add.push_str(&format!(
                "<Override PartName=\"/{part}\" ContentType=\"{ct}\"/>"
            ));
        }
        let rels_root = root_name(&rels_xml)?;
        let out = transform(&rels_xml, |_| Act::Keep, &[(rels_root.as_str(), rel_add)])?;
        plan.insert("_rels/.rels".into(), Entry::Replace(out.into_bytes()));
        let ct_xml = z
            .text("[Content_Types].xml")?
            .ok_or("The package has no content types.")?;
        let ct_root = root_name(&ct_xml)?;
        let out = transform(&ct_xml, |_| Act::Keep, &[(ct_root.as_str(), ct_add)])?;
        plan.insert(
            "[Content_Types].xml".into(),
            Entry::Replace(out.into_bytes()),
        );
    }
    zipx::rewrite(src, dst, plan, add, false)
}

fn root_name(xml: &str) -> Result<String> {
    let mut first = None;
    collect(xml, |_| true)?.into_iter().for_each(|e| {
        if first.is_none() && e.path.is_empty() {
            first = Some(e.name);
        }
    });
    first.ok_or_else(|| "The file contains damaged XML.".to_string())
}

// ---------------------------------------------------------------------------
// Samples

const CT_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/><Default Extension=\"bin\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.printerSettings\"/>";
const RELS_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn ct(overrides: &[(&str, &str)]) -> String {
    let mut s = CT_HEAD.to_string();
    for (p, t) in overrides {
        s.push_str(&format!(
            "<Override PartName=\"/{p}\" ContentType=\"{t}\"/>"
        ));
    }
    s + "</Types>"
}

fn rels_xml(list: &[(&str, &str, &str)]) -> String {
    let mut s = RELS_HEAD.to_string();
    for (id, typ, target) in list {
        let ext = if target.contains(':') {
            " TargetMode=\"External\""
        } else {
            ""
        };
        s.push_str(&format!(
            "<Relationship Id=\"{id}\" Type=\"{typ}\" Target=\"{}\"{ext}/>",
            esc(target)
        ));
    }
    s + "</Relationships>"
}

fn core_xml() -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<cp:coreProperties xmlns:cp=\"{NS_CP}\" xmlns:dc=\"{NS_DC}\" xmlns:dcterms=\"{NS_DCTERMS}\" xmlns:dcmitype=\"http://purl.org/dc/dcmitype/\" xmlns:xsi=\"{NS_XSI}\"><dc:title>Quarterly Test Report</dc:title><dc:subject>Invented subject</dc:subject><dc:creator>Test Author</dc:creator><cp:keywords>sample, test</cp:keywords><dc:description>Draft for review</dc:description><cp:lastModifiedBy>Test Editor</cp:lastModifiedBy><cp:revision>7</cp:revision><cp:lastPrinted>2024-04-30T08:00:00Z</cp:lastPrinted><dcterms:created xsi:type=\"dcterms:W3CDTF\">2024-05-01T10:20:30Z</dcterms:created><dcterms:modified xsi:type=\"dcterms:W3CDTF\">2024-05-02T11:00:00Z</dcterms:modified><cp:category>Reports</cp:category></cp:coreProperties>")
}

fn app_xml(app: &str, stats: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/extended-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"><Template>Normal.dotm</Template><TotalTime>42</TotalTime>{stats}<Application>{app}</Application><DocSecurity>0</DocSecurity><Company>Example Test Company</Company><Manager>Test Manager</Manager><AppVersion>16.0000</AppVersion></Properties>")
}

fn custom_xml() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\" xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\"><property fmtid=\"{D5CDD505-2E9C-101B-9397-08002B2CF9AE}\" pid=\"2\" name=\"ProjectCode\"><vt:lpwstr>TEST-123</vt:lpwstr></property></Properties>"
}

/// A tiny JPEG-looking blob for the thumbnail (content is not decoded).
const THUMB: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0\xFF\xD9";

const PKG_CORE: (&str, &str) = ("docProps/core.xml", CT_CORE);
const PKG_APP: (&str, &str) = ("docProps/app.xml", CT_APP);
const PKG_CUSTOM: (&str, &str) = (
    "docProps/custom.xml",
    "application/vnd.openxmlformats-officedocument.custom-properties+xml",
);

fn package_rels(main_type: &str, main: &str) -> String {
    rels_xml(&[
        ("rId1", main_type, main),
        ("rId2", REL_CORE, "docProps/core.xml"),
        ("rId3", REL_APP, "docProps/app.xml"),
        (
            "rId4",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties",
            "docProps/custom.xml",
        ),
        (
            "rId5",
            "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail",
            "docProps/thumbnail.jpeg",
        ),
    ])
}

fn zip_bytes(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(
            zip::DateTime::from_date_and_time(2024, 5, 2, 11, 0, 0).unwrap_or_default(),
        );
    for (name, data) in entries {
        if w.start_file(name, opts).is_err() || w.write_all(&data).is_err() {
            return Vec::new();
        }
    }
    w.finish().map(|c| c.into_inner()).unwrap_or_default()
}

/// A Word document with properties, a comment by "Test Reviewer", a tracked
/// change by "Test Editor", a template under `C:\Users\TestUser`, editing
/// session ids and a thumbnail.
pub fn sample() -> Vec<u8> {
    let w = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    let doc = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"{w}\" xmlns:r=\"{R}\"><w:body><w:p w:rsidR=\"00A1B2C3\" w:rsidRDefault=\"00A1B2C3\"><w:commentRangeStart w:id=\"0\"/><w:r><w:t xml:space=\"preserve\">Hello </w:t></w:r><w:commentRangeEnd w:id=\"0\"/><w:r><w:commentReference w:id=\"0\"/></w:r><w:ins w:id=\"1\" w:author=\"Test Editor\" w:date=\"2024-05-02T09:00:00Z\"><w:r><w:t>world</w:t></w:r></w:ins></w:p><w:sectPr/></w:body></w:document>");
    let comments = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:comments xmlns:w=\"{w}\"><w:comment w:id=\"0\" w:author=\"Test Reviewer\" w:date=\"2024-05-02T10:00:00Z\" w:initials=\"TR\"><w:p><w:r><w:t>Please check</w:t></w:r></w:p></w:comment></w:comments>");
    let settings = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:settings xmlns:w=\"{w}\" xmlns:r=\"{R}\"><w:zoom w:percent=\"100\"/><w:attachedTemplate r:id=\"rId1\"/><w:defaultTabStop w:val=\"720\"/><w:docVars><w:docVar w:name=\"ClientRef\" w:val=\"TEST-9\"/></w:docVars><w:rsids><w:rsidRoot w:val=\"00A1B2C3\"/><w:rsid w:val=\"00A1B2C3\"/><w:rsid w:val=\"00D4E5F6\"/></w:rsids><w:compat/></w:settings>");
    let people = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w15:people xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\"><w15:person w15:author=\"Test Reviewer\"><w15:presenceInfo w15:providerId=\"None\" w15:userId=\"test.reviewer@example.com\"/></w15:person></w15:people>";
    let wt = "application/vnd.openxmlformats-officedocument.wordprocessingml";
    zip_bytes(vec![
        (
            "[Content_Types].xml",
            ct(&[
                ("word/document.xml", &format!("{wt}.document.main+xml")),
                ("word/comments.xml", &format!("{wt}.comments+xml")),
                ("word/settings.xml", &format!("{wt}.settings+xml")),
                ("word/people.xml", &format!("{wt}.people+xml")),
                PKG_CORE,
                PKG_APP,
                PKG_CUSTOM,
            ])
            .into_bytes(),
        ),
        (
            "_rels/.rels",
            package_rels(&format!("{R}/officeDocument"), "word/document.xml").into_bytes(),
        ),
        ("docProps/core.xml", core_xml().into_bytes()),
        (
            "docProps/app.xml",
            app_xml("Microsoft Office Word", "<Pages>1</Pages><Words>2</Words>").into_bytes(),
        ),
        ("docProps/custom.xml", custom_xml().as_bytes().to_vec()),
        ("docProps/thumbnail.jpeg", THUMB.to_vec()),
        ("word/document.xml", doc.into_bytes()),
        (
            "word/_rels/document.xml.rels",
            rels_xml(&[
                ("rId1", &format!("{R}/comments"), "comments.xml"),
                ("rId2", &format!("{R}/settings"), "settings.xml"),
                (
                    "rId3",
                    "http://schemas.microsoft.com/office/2011/relationships/people",
                    "people.xml",
                ),
            ])
            .into_bytes(),
        ),
        ("word/comments.xml", comments.into_bytes()),
        ("word/settings.xml", settings.into_bytes()),
        (
            "word/_rels/settings.xml.rels",
            rels_xml(&[(
                "rId1",
                &format!("{R}/attachedTemplate"),
                r"file:///C:\Users\TestUser\AppData\Roaming\Microsoft\Templates\Normal.dotm",
            )])
            .into_bytes(),
        ),
        ("word/people.xml", people.as_bytes().to_vec()),
    ])
}

/// An Excel workbook with a hidden sheet, a comment by "Test Reviewer",
/// printer settings and the folder it was saved in.
pub fn sample_xlsx() -> Vec<u8> {
    let s = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let st = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let workbook = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"{s}\" xmlns:r=\"{R}\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\" xmlns:x15ac=\"http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac\"><mc:AlternateContent><mc:Choice Requires=\"x15\"><x15ac:absPath url=\"C:\\Users\\TestUser\\Documents\\\"/></mc:Choice></mc:AlternateContent><sheets><sheet name=\"Summary\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"Salaries\" sheetId=\"2\" state=\"hidden\" r:id=\"rId2\"/></sheets></workbook>");
    let sheet = |extra: &str| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"{s}\" xmlns:r=\"{R}\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>1</t></is></c></row></sheetData>{extra}</worksheet>")
    };
    let comments = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<comments xmlns=\"{s}\"><authors><author>Test Reviewer</author></authors><commentList><comment ref=\"A1\" authorId=\"0\"><text><t>Check this</t></text></comment></commentList></comments>");
    zip_bytes(vec![
        (
            "[Content_Types].xml",
            ct(&[
                ("xl/workbook.xml", &format!("{st}.sheet.main+xml")),
                ("xl/worksheets/sheet1.xml", &format!("{st}.worksheet+xml")),
                ("xl/worksheets/sheet2.xml", &format!("{st}.worksheet+xml")),
                ("xl/comments1.xml", &format!("{st}.comments+xml")),
                (
                    "xl/printerSettings/printerSettings1.bin",
                    &format!("{st}.printerSettings"),
                ),
                PKG_CORE,
                PKG_APP,
                PKG_CUSTOM,
            ])
            .into_bytes(),
        ),
        (
            "_rels/.rels",
            package_rels(&format!("{R}/officeDocument"), "xl/workbook.xml").into_bytes(),
        ),
        ("docProps/core.xml", core_xml().into_bytes()),
        (
            "docProps/app.xml",
            app_xml("Microsoft Excel", "").into_bytes(),
        ),
        ("docProps/custom.xml", custom_xml().as_bytes().to_vec()),
        ("docProps/thumbnail.jpeg", THUMB.to_vec()),
        ("xl/workbook.xml", workbook.into_bytes()),
        (
            "xl/_rels/workbook.xml.rels",
            rels_xml(&[
                ("rId1", &format!("{R}/worksheet"), "worksheets/sheet1.xml"),
                ("rId2", &format!("{R}/worksheet"), "worksheets/sheet2.xml"),
            ])
            .into_bytes(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            sheet("<pageSetup orientation=\"portrait\" r:id=\"rId1\"/>").into_bytes(),
        ),
        (
            "xl/worksheets/_rels/sheet1.xml.rels",
            rels_xml(&[
                (
                    "rId1",
                    &format!("{R}/printerSettings"),
                    "../printerSettings/printerSettings1.bin",
                ),
                ("rId2", &format!("{R}/comments"), "../comments1.xml"),
            ])
            .into_bytes(),
        ),
        ("xl/worksheets/sheet2.xml", sheet("").into_bytes()),
        ("xl/comments1.xml", comments.into_bytes()),
        (
            "xl/printerSettings/printerSettings1.bin",
            b"Test Printer 3000 DEVMODE".to_vec(),
        ),
    ])
}

/// A PowerPoint file with a hidden slide, speaker notes and a comment.
pub fn sample_pptx() -> Vec<u8> {
    let p = "http://schemas.openxmlformats.org/presentationml/2006/main";
    let pt = "application/vnd.openxmlformats-officedocument.presentationml";
    let slide = |show: &str| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:sld xmlns:p=\"{p}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"{R}\"{show}><p:cSld><p:spTree/></p:cSld></p:sld>")
    };
    let pres = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:presentation xmlns:p=\"{p}\" xmlns:r=\"{R}\"><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"257\" r:id=\"rId2\"/></p:sldIdLst></p:presentation>");
    let authors = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:cmAuthorLst xmlns:p=\"{p}\"><p:cmAuthor id=\"1\" name=\"Test Reviewer\" initials=\"TR\" lastIdx=\"1\" clrIdx=\"0\"/></p:cmAuthorLst>");
    let comments = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:cmLst xmlns:p=\"{p}\"><p:cm authorId=\"1\" dt=\"2024-05-02T10:00:00.000\" idx=\"1\"><p:pos x=\"10\" y=\"10\"/><p:text>Shorter</p:text></p:cm></p:cmLst>");
    let notes = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<p:notes xmlns:p=\"{p}\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><p:cSld><p:spTree/></p:cSld></p:notes>");
    zip_bytes(vec![
        (
            "[Content_Types].xml",
            ct(&[
                (
                    "ppt/presentation.xml",
                    &format!("{pt}.presentation.main+xml"),
                ),
                ("ppt/slides/slide1.xml", &format!("{pt}.slide+xml")),
                ("ppt/slides/slide2.xml", &format!("{pt}.slide+xml")),
                (
                    "ppt/notesSlides/notesSlide1.xml",
                    &format!("{pt}.notesSlide+xml"),
                ),
                (
                    "ppt/commentAuthors.xml",
                    &format!("{pt}.commentAuthors+xml"),
                ),
                ("ppt/comments/comment1.xml", &format!("{pt}.comments+xml")),
                PKG_CORE,
                PKG_APP,
                PKG_CUSTOM,
            ])
            .into_bytes(),
        ),
        (
            "_rels/.rels",
            package_rels(&format!("{R}/officeDocument"), "ppt/presentation.xml").into_bytes(),
        ),
        ("docProps/core.xml", core_xml().into_bytes()),
        (
            "docProps/app.xml",
            app_xml("Microsoft Office PowerPoint", "<Slides>2</Slides>").into_bytes(),
        ),
        ("docProps/custom.xml", custom_xml().as_bytes().to_vec()),
        ("docProps/thumbnail.jpeg", THUMB.to_vec()),
        ("ppt/presentation.xml", pres.into_bytes()),
        (
            "ppt/_rels/presentation.xml.rels",
            rels_xml(&[
                ("rId1", &format!("{R}/slide"), "slides/slide1.xml"),
                ("rId2", &format!("{R}/slide"), "slides/slide2.xml"),
                ("rId3", &format!("{R}/commentAuthors"), "commentAuthors.xml"),
            ])
            .into_bytes(),
        ),
        ("ppt/slides/slide1.xml", slide("").into_bytes()),
        (
            "ppt/slides/_rels/slide1.xml.rels",
            rels_xml(&[
                (
                    "rId1",
                    &format!("{R}/notesSlide"),
                    "../notesSlides/notesSlide1.xml",
                ),
                ("rId2", &format!("{R}/comments"), "../comments/comment1.xml"),
            ])
            .into_bytes(),
        ),
        ("ppt/slides/slide2.xml", slide(" show=\"0\"").into_bytes()),
        ("ppt/notesSlides/notesSlide1.xml", notes.into_bytes()),
        ("ppt/commentAuthors.xml", authors.into_bytes()),
        ("ppt/comments/comment1.xml", comments.into_bytes()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::zipx::tests::temp_dir;
    use crate::meta::{CatSet, Kind};

    fn get<'a>(x: &'a Extract, id: &str) -> Option<&'a Field> {
        x.fields.iter().find(|f| f.id == id)
    }

    fn value(x: &Extract, id: &str) -> Option<String> {
        get(x, id).map(|f| f.value.clone())
    }

    /// Every internal relationship target and content-type override exists
    /// and every XML part parses.
    pub(crate) fn assert_valid_package(path: &Path) {
        let mut z = Zip::open(path).expect("zip opens");
        let names = z.names().to_vec();
        for name in &names {
            let data = z.read(name).unwrap().unwrap();
            if name.ends_with(".xml") || name.ends_with(".rels") {
                let text = String::from_utf8(data).expect("utf-8");
                assert!(xmlutil::well_formed(&text), "{name} is not well-formed");
            }
            if name.ends_with(".rels") {
                let source = rels_source(name).unwrap();
                assert!(
                    source.is_empty() || names.contains(&source),
                    "{name} belongs to a missing part"
                );
                for rel in parse_rels(&z.text(name).unwrap().unwrap()).unwrap() {
                    if !rel.external {
                        let t = resolve(&source, &rel.target);
                        assert!(names.contains(&t), "{name}: target {t} missing");
                    }
                }
            }
        }
        let ct = z.text("[Content_Types].xml").unwrap().unwrap();
        for el in collect(&ct, |n| n == "Override").unwrap() {
            let part = el.attr("PartName").unwrap().trim_start_matches('/');
            assert!(
                names.iter().any(|n| n == part),
                "override for missing {part}"
            );
        }
    }

    fn write(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    fn assert_invariant(x: &Extract, opts: &CleanOptions) {
        for f in &x.fields {
            assert!(
                !(f.removable && opts.remove.contains(f.category)),
                "left after clean: {} = {}",
                f.id,
                f.value
            );
        }
    }

    #[test]
    fn docx_read_finds_everything() {
        let dir = temp_dir("docx-read");
        let p = write(&dir, "a.docx", &sample());
        let x = read(&p).unwrap();
        assert_eq!(
            value(&x, "ooxml:core:creator").as_deref(),
            Some("Test Author")
        );
        assert_eq!(
            get(&x, "ooxml:core:creator").unwrap().flag,
            Some(Flag::Names)
        );
        assert_eq!(
            value(&x, "ooxml:core:lastModifiedBy").as_deref(),
            Some("Test Editor")
        );
        assert_eq!(
            value(&x, "ooxml:core:created").as_deref(),
            Some("2024-05-01 10:20:30")
        );
        assert_eq!(x.taken.as_deref(), Some("2024-05-01 10:20:30"));
        assert_eq!(
            value(&x, "ooxml:app:Company").as_deref(),
            Some("Example Test Company")
        );
        assert_eq!(
            value(&x, "ooxml:app:TotalTime").as_deref(),
            Some("42 minutes")
        );
        assert_eq!(
            value(&x, "ooxml:custom:ProjectCode").as_deref(),
            Some("TEST-123")
        );
        assert!(get(&x, "ooxml:thumbnail:docProps/thumbnail.jpeg").is_some());
        assert_eq!(
            value(&x, "ooxml:comments:authors").as_deref(),
            Some("Test Reviewer")
        );
        assert_eq!(value(&x, "ooxml:comments:initials").as_deref(), Some("TR"));
        assert_eq!(
            value(&x, "ooxml:comments:accounts").as_deref(),
            Some("test.reviewer@example.com")
        );
        assert_eq!(
            value(&x, "ooxml:changes:authors").as_deref(),
            Some("Test Editor")
        );
        assert!(get(&x, "ooxml:changes:dates").is_some());
        let count = get(&x, "ooxml:comments:count").unwrap();
        assert!(!count.removable && count.flag == Some(Flag::Comments));
        assert!(!get(&x, "ooxml:changes:count").unwrap().removable);
        let tpl = get(&x, "ooxml:settings:attachedTemplate").unwrap();
        assert!(tpl.value.contains(r"C:\Users\TestUser"));
        assert_eq!(tpl.flag, Some(Flag::Names));
        assert_eq!(value(&x, "ooxml:paths:users").as_deref(), Some("TestUser"));
        assert_eq!(value(&x, "ooxml:settings:rsids").as_deref(), Some("3 ids"));
        assert!(value(&x, "ooxml:settings:docVars")
            .unwrap()
            .contains("ClientRef"));
        assert!(x.notes.iter().any(|n| n.contains("1 comment")));
        assert!(x.editable.contains(&EditKey::Company));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn docx_clean_everything() {
        let dir = temp_dir("docx-clean");
        let p = write(&dir, "a.docx", &sample());
        let out = dir.join("b.docx");
        let opts = CleanOptions::everything();
        clean(&p, &out, &opts).unwrap();
        assert_valid_package(&out);
        let x = read(&out).unwrap();
        assert_invariant(&x, &opts);
        // Content counts stay, and the content itself.
        assert!(get(&x, "ooxml:comments:count").is_some());
        let mut z = Zip::open(&out).unwrap();
        let doc = z.text("word/document.xml").unwrap().unwrap();
        assert!(doc.contains("world") && doc.contains("Hello"));
        assert!(!doc.contains("rsidR") && !doc.contains("Test Editor"));
        assert!(!z.has("docProps/custom.xml") && !z.has("docProps/thumbnail.jpeg"));
        let settings = z.text("word/settings.xml").unwrap().unwrap();
        assert!(!settings.contains("attachedTemplate") && settings.contains("defaultTabStop"));
        let all: String = z
            .names()
            .to_vec()
            .iter()
            .map(|n| String::from_utf8_lossy(&z.read(n).unwrap().unwrap()).into_owned())
            .collect();
        for secret in [
            "TestUser",
            "Test Reviewer",
            "Test Author",
            "Example Test Company",
            "test.reviewer@",
            "TEST-123",
            "2024-05-01",
        ] {
            assert!(!all.contains(secret), "{secret} left");
        }
        // Zip times reset.
        let mut ar = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        for i in 0..ar.len() {
            assert_eq!(
                ar.by_index(i).unwrap().last_modified().unwrap(),
                zip::DateTime::default()
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn docx_partial_clean_keeps_the_rest() {
        let dir = temp_dir("docx-partial");
        let p = write(&dir, "a.docx", &sample());
        let out = dir.join("b.docx");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&p, &out, &opts).unwrap();
        assert_valid_package(&out);
        let x = read(&out).unwrap();
        assert_invariant(&x, &opts);
        assert_eq!(
            value(&x, "ooxml:core:title").as_deref(),
            Some("Quarterly Test Report")
        );
        assert_eq!(
            value(&x, "ooxml:core:created").as_deref(),
            Some("2024-05-01 10:20:30")
        );
        assert_eq!(
            value(&x, "ooxml:custom:ProjectCode").as_deref(),
            Some("TEST-123")
        );
        assert!(get(&x, "ooxml:changes:dates").is_some());
        // The template stays but without the user name.
        let tpl = value(&x, "ooxml:settings:attachedTemplate").unwrap();
        assert!(
            tpl.contains(r"C:\Users\user\") && !tpl.contains("TestUser"),
            "{tpl}"
        );
        assert!(get(&x, "ooxml:comments:authors").is_none());
        let mut z = Zip::open(&out).unwrap();
        let comments = z.text("word/comments.xml").unwrap().unwrap();
        assert!(comments.contains("w:author=\"Author\"") && comments.contains("Please check"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xlsx_and_pptx() {
        let dir = temp_dir("xlsx-pptx");
        let xl = write(&dir, "a.xlsx", &sample_xlsx());
        let x = read(&xl).unwrap();
        assert_eq!(value(&x, "ooxml:type").as_deref(), Some("Excel document"));
        assert_eq!(
            value(&x, "ooxml:workbook:hiddenSheets").as_deref(),
            Some("Salaries")
        );
        assert!(!get(&x, "ooxml:workbook:hiddenSheets").unwrap().removable);
        assert_eq!(value(&x, "ooxml:printer").as_deref(), Some("1 part"));
        assert_eq!(get(&x, "ooxml:printer").unwrap().category, Category::Device);
        assert_eq!(
            value(&x, "ooxml:comments:authors").as_deref(),
            Some("Test Reviewer")
        );
        assert_eq!(value(&x, "ooxml:paths:users").as_deref(), Some("TestUser"));
        let out = dir.join("b.xlsx");
        let opts = CleanOptions::everything();
        clean(&xl, &out, &opts).unwrap();
        assert_valid_package(&out);
        let y = read(&out).unwrap();
        assert_invariant(&y, &opts);
        assert!(get(&y, "ooxml:workbook:hiddenSheets").is_some());
        let mut z = Zip::open(&out).unwrap();
        assert!(!z.names().iter().any(|n| n.contains("printerSettings")));
        let sheet = z.text("xl/worksheets/sheet1.xml").unwrap().unwrap();
        assert!(
            sheet.contains("<pageSetup orientation=\"portrait\"/>"),
            "{sheet}"
        );
        assert!(!z
            .text("xl/workbook.xml")
            .unwrap()
            .unwrap()
            .contains("TestUser"));

        let pp = write(&dir, "a.pptx", &sample_pptx());
        let x = read(&pp).unwrap();
        assert_eq!(
            value(&x, "ooxml:ppt:hiddenSlides").as_deref(),
            Some("1 slide")
        );
        assert_eq!(
            value(&x, "ooxml:ppt:notes").as_deref(),
            Some("1 slide has notes")
        );
        assert_eq!(
            value(&x, "ooxml:comments:authors").as_deref(),
            Some("Test Reviewer")
        );
        assert_eq!(
            value(&x, "ooxml:comments:count").as_deref(),
            Some("1 comment")
        );
        let out = dir.join("b.pptx");
        clean(&pp, &out, &opts).unwrap();
        assert_valid_package(&out);
        let y = read(&out).unwrap();
        assert_invariant(&y, &opts);
        assert!(
            get(&y, "ooxml:ppt:notes").is_some() && get(&y, "ooxml:ppt:hiddenSlides").is_some()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_single_category_keeps_the_invariant() {
        let dir = temp_dir("ooxml-each");
        for (i, data) in [sample(), sample_xlsx(), sample_pptx()].iter().enumerate() {
            let p = write(&dir, &format!("s{i}.docx"), data);
            for cat in Category::ALL {
                let opts = CleanOptions {
                    remove: CatSet::of(&[cat]),
                    ..CleanOptions::everything()
                };
                let out = dir.join(format!("o{i}-{}.docx", cat.key()));
                clean(&p, &out, &opts).unwrap();
                assert_valid_package(&out);
                assert_invariant(&read(&out).unwrap(), &opts);
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_round_trip_with_kurdish() {
        let dir = temp_dir("docx-edit");
        let p = write(&dir, "a.docx", &sample());
        let out = dir.join("b.docx");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &p,
            &out,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "Kurdish <Test> & Co"),
                Edit::clear(EditKey::Subject),
                Edit::set(EditKey::Company, "نموونە"),
                Edit::set(EditKey::DateTaken, "2023-01-02 03:04:05"),
            ],
        )
        .unwrap();
        assert_valid_package(&out);
        let x = read(&out).unwrap();
        assert_eq!(value(&x, "ooxml:core:title").as_deref(), Some(title));
        assert_eq!(
            value(&x, "ooxml:core:creator").as_deref(),
            Some("Kurdish <Test> & Co")
        );
        assert!(get(&x, "ooxml:core:subject").is_none());
        assert_eq!(value(&x, "ooxml:app:Company").as_deref(), Some("نموونە"));
        assert_eq!(x.taken.as_deref(), Some("2023-01-02 03:04:05"));
        assert_eq!(
            x.fields
                .iter()
                .filter(|f| f.id == "ooxml:core:title")
                .count(),
            1
        );
        assert!(edit(&p, &out, &[Edit::set(EditKey::DateTaken, "soon")]).is_err());
        assert!(edit(&p, &out, &[Edit::set(EditKey::Gps, "1, 2")]).is_err());
        let _ = EditKey::Gps.label(Kind::Document);

        // A package without core/app properties gets them created.
        let bare = zip_bytes(vec![
            ("[Content_Types].xml", ct(&[("word/document.xml", "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")]).into_bytes()),
            ("_rels/.rels", rels_xml(&[("rId1", &format!("{R}/officeDocument"), "word/document.xml")]).into_bytes()),
            ("word/document.xml", b"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body/></w:document>".to_vec()),
        ]);
        let b = write(&dir, "bare.docx", &bare);
        let out2 = dir.join("bare2.docx");
        edit(
            &b,
            &out2,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Company, "X"),
            ],
        )
        .unwrap();
        assert_valid_package(&out2);
        let x = read(&out2).unwrap();
        assert_eq!(value(&x, "ooxml:core:title").as_deref(), Some(title));
        assert_eq!(value(&x, "ooxml:app:Company").as_deref(), Some("X"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn garbage_and_truncated_inputs_fail_cleanly() {
        let dir = temp_dir("docx-bad");
        let data = sample();
        let cut = write(&dir, "cut.docx", &data[..data.len() / 2]);
        assert!(read(&cut).is_err());
        assert!(clean(&cut, &dir.join("o.docx"), &CleanOptions::everything()).is_err());
        let junk = write(&dir, "junk.docx", b"PK\x03\x04garbage");
        assert!(read(&junk).is_err());
        let other = write(&dir, "o.zip", &zip_bytes(vec![("a.txt", b"x".to_vec())]));
        assert!(read(&other).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paths_resolve() {
        assert_eq!(
            resolve("xl/worksheets/sheet1.xml", "../printerSettings/p1.bin"),
            "xl/printerSettings/p1.bin"
        );
        assert_eq!(resolve("", "docProps/core.xml"), "docProps/core.xml");
        assert_eq!(
            resolve("word/document.xml", "/word/media/a%20b.png"),
            "word/media/a b.png"
        );
        assert_eq!(rels_of("word/document.xml"), "word/_rels/document.xml.rels");
        assert_eq!(rels_source("_rels/.rels").as_deref(), Some(""));
        assert_eq!(
            rels_source("word/_rels/settings.xml.rels").as_deref(),
            Some("word/settings.xml")
        );
    }
}
