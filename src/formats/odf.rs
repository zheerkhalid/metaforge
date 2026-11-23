//! OpenDocument files (LibreOffice / OpenOffice text, spreadsheets,
//! presentations, drawings).
//!
//! Reads meta.xml (authors, dates, program, editing statistics, template,
//! user-defined fields), the printer in settings.xml, the thumbnail, saved
//! older versions, and the authors of comments and tracked changes in
//! content.xml / styles.xml. Cleaning keeps `mimetype` first and stored and
//! keeps META-INF/manifest.xml in step with dropped parts.

use super::ooxml::NEUTRAL;
use super::xmlutil::{anonymize_path, collect, esc, transform, user_in_path, Act, El};
use super::zipx::{self, Entry, Zip};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use crate::util::DateTime;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

const SRC_META: &str = "OpenDocument meta";
const SRC_DOC: &str = "Document content";
const SRC_SETTINGS: &str = "OpenDocument settings";

const NS_OFFICE: &str = "urn:oasis:names:tc:opendocument:xmlns:office:1.0";
const NS_META: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_XLINK: &str = "http://www.w3.org/1999/xlink";

/// The date cleaning writes where the format requires one (tracked changes).
const NEUTRAL_DATE: &str = "1970-01-01T00:00:00";

use Category::{Dates, Description, Hidden, People, Software};

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

const META: &[Prop] = &[
    prop(
        "initial-creator",
        "Author",
        People,
        Some(Flag::Names),
        Some(EditKey::Author),
    ),
    prop("creator", "Last saved by", People, Some(Flag::Names), None),
    prop("printed-by", "Printed by", People, Some(Flag::Names), None),
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
        "keyword",
        "Keywords",
        Description,
        None,
        Some(EditKey::Keywords),
    ),
    prop("language", "Language", Description, None, None),
    prop(
        "creation-date",
        "Created",
        Dates,
        None,
        Some(EditKey::DateTaken),
    ),
    prop("date", "Modified", Dates, None, None),
    prop("print-date", "Last printed", Dates, None, None),
    prop("generator", "Program used", Software, None, None),
    prop("editing-cycles", "Revision number", Hidden, None, None),
    prop("editing-duration", "Total editing time", Hidden, None, None),
    prop("template", "Template", Hidden, None, None),
    prop(
        "user-defined",
        "Custom property",
        Hidden,
        Some(Flag::Hidden),
        None,
    ),
];

fn find(local: &str) -> Option<&'static Prop> {
    META.iter().find(|p| p.local == local)
}

/// The meta.xml element for an edit key.
fn meta_element(key: EditKey) -> Option<(&'static str, &'static str)> {
    Some(match key {
        EditKey::Title => ("title", "dc:title"),
        EditKey::Author => ("initial-creator", "meta:initial-creator"),
        EditKey::Subject => ("subject", "dc:subject"),
        EditKey::Description => ("description", "dc:description"),
        EditKey::Keywords => ("keyword", "meta:keyword"),
        EditKey::DateTaken => ("creation-date", "meta:creation-date"),
        _ => return None,
    })
}

fn open(path: &Path) -> Result<(Zip, String)> {
    let mut z = Zip::open(path)?;
    let mime = z.text("mimetype")?.unwrap_or_default();
    let mime = mime.trim().to_string();
    if !mime.starts_with("application/vnd.oasis.opendocument") && !z.has("content.xml") {
        return Err("This is not an OpenDocument file.".into());
    }
    Ok((z, mime))
}

fn kind_name(mime: &str) -> &'static str {
    let sub = mime.rsplit('.').next().unwrap_or("");
    match sub {
        s if s.starts_with("text") => "OpenDocument text",
        s if s.starts_with("spreadsheet") => "OpenDocument spreadsheet",
        s if s.starts_with("presentation") => "OpenDocument presentation",
        s if s.starts_with("graphics") => "OpenDocument drawing",
        _ => "OpenDocument file",
    }
}

/// `PT1H2M3S` / `P1DT2H` as readable text.
fn duration_text(v: &str) -> String {
    let mut secs = 0u64;
    let mut num = String::new();
    let mut in_time = false;
    for c in v.trim().chars() {
        match c {
            '0'..='9' | '.' => num.push(c),
            'T' => in_time = true,
            'P' => {}
            unit => {
                let n = num
                    .split('.')
                    .next()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0);
                num.clear();
                secs += n * match (unit, in_time) {
                    ('D', false) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => 0,
                };
            }
        }
    }
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (h, m) {
        (0, 0) => format!("{} seconds", secs),
        (0, m) => format!("{m} minutes"),
        (h, m) => format!("{h} hours {m} minutes"),
    }
}

fn date_text(v: &str) -> String {
    DateTime::parse(v)
        .map(|d| d.text())
        .unwrap_or_else(|| v.trim().to_string())
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn distinct(list: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    list.iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && s != NEUTRAL && seen.insert(s.clone()))
        .collect()
}

fn is_versions(name: &str) -> bool {
    name == "VersionList.xml" || name.starts_with("Versions/")
}

fn is_thumbnail(name: &str) -> bool {
    name.starts_with("Thumbnails/")
}

/// Content parts that can hold comments and tracked changes.
fn is_content(name: &str) -> bool {
    matches!(name, "content.xml" | "styles.xml")
}

/// Whether a `dc:creator` / `dc:date` element belongs to a comment or a
/// tracked change (rather than to the document meta).
fn in_annotation(el: &El) -> bool {
    el.inside("annotation")
}

fn in_change(el: &El) -> bool {
    el.inside("change-info")
}

pub fn read(path: &Path) -> Result<Extract> {
    let (mut z, mime) = open(path)?;
    let names = z.names().to_vec();
    let mut x = Extract {
        editable: vec![
            EditKey::Title,
            EditKey::Author,
            EditKey::Subject,
            EditKey::Description,
            EditKey::Keywords,
            EditKey::DateTaken,
        ],
        ..Extract::default()
    };
    let push = |x: &mut Extract, f: Field| {
        if !f.value.trim().is_empty() {
            x.fields.push(f);
        }
    };
    push(
        &mut x,
        Field::new(
            "odf:type",
            "Document type",
            kind_name(&mime),
            Category::Technical,
            SRC_META,
        )
        .fixed(),
    );
    let mut user_paths = Vec::new();

    if let Some(xml) = z.text("meta.xml")? {
        let els = collect(&xml, |n| find(n).is_some() || n == "document-statistic")?;
        let mut keywords = Vec::new();
        for el in els.iter().filter(|e| e.path.len() == 2) {
            if el.local() == "document-statistic" {
                for (attr, label) in [("meta:page-count", "Pages"), ("meta:word-count", "Words")] {
                    if let Some(v) = el.attr(attr) {
                        push(
                            &mut x,
                            Field::new(
                                format!("odf:stat:{label}"),
                                label,
                                v,
                                Category::Technical,
                                SRC_META,
                            )
                            .fixed(),
                        );
                    }
                }
                continue;
            }
            let p = find(el.local()).expect("filtered");
            let mut f = Field::new(
                format!("odf:meta:{}", p.local),
                p.label,
                el.text.trim(),
                p.cat,
                SRC_META,
            );
            f.flag = p.flag;
            f.edit = p.edit;
            match p.local {
                "keyword" => {
                    keywords.push(el.text.trim().to_string());
                    continue;
                }
                "creation-date" | "date" | "print-date" => {
                    f.value = date_text(&el.text);
                    if p.local == "creation-date" && !f.value.is_empty() {
                        x.taken = Some(f.value.clone());
                    }
                }
                "editing-duration" => f.value = duration_text(&el.text),
                "template" => {
                    let href = el.attr("xlink:href").unwrap_or_default();
                    f.value = href.to_string();
                    if user_in_path(href).is_some() {
                        user_paths.push(href.to_string());
                        f.flag = Some(Flag::Names);
                    }
                }
                "user-defined" => {
                    let name = el.attr("meta:name").unwrap_or("(unnamed)");
                    f.id = format!("odf:user:{name}");
                    f.label = format!("Custom property \"{name}\"");
                }
                _ => {}
            }
            push(&mut x, f);
        }
        keywords.retain(|k| !k.is_empty());
        if !keywords.is_empty() {
            push(
                &mut x,
                Field::new(
                    "odf:meta:keyword",
                    "Keywords",
                    keywords.join(", "),
                    Description,
                    SRC_META,
                )
                .edit(EditKey::Keywords),
            );
        }
    }

    if let Some(xml) = z.text("settings.xml")? {
        for el in collect(&xml, |n| n == "config-item")? {
            match el.attr("config:name") {
                Some("PrinterName") => push(
                    &mut x,
                    Field::new(
                        "odf:settings:PrinterName",
                        "Printer",
                        el.text.trim(),
                        Category::Device,
                        SRC_SETTINGS,
                    ),
                ),
                Some("PrinterSetup") if !el.text.trim().is_empty() => push(
                    &mut x,
                    Field::new(
                        "odf:settings:PrinterSetup",
                        "Printer settings saved",
                        "Yes",
                        Category::Device,
                        SRC_SETTINGS,
                    )
                    .flag(Flag::Hidden),
                ),
                _ => {}
            }
        }
    }

    for name in names
        .iter()
        .filter(|n| is_thumbnail(n) && !n.ends_with('/'))
    {
        let size = z.size(name).unwrap_or(0);
        push(
            &mut x,
            Field::new(
                format!("odf:thumbnail:{name}"),
                "Thumbnail",
                format!("PNG picture, {}", crate::util::human_size(size)),
                Hidden,
                SRC_META,
            )
            .flag(Flag::Hidden),
        );
    }
    let versions = names
        .iter()
        .filter(|n| n.starts_with("Versions/") && !n.ends_with('/'))
        .count();
    if versions > 0 || names.iter().any(|n| n == "VersionList.xml") {
        push(
            &mut x,
            Field::new(
                "odf:versions",
                "Saved older versions",
                plural(versions.max(1), "version", "versions"),
                Hidden,
                SRC_DOC,
            )
            .flag(Flag::Hidden),
        );
        x.notes.push(
            "Older versions of the document are saved inside the file; cleaning removes them."
                .into(),
        );
    }

    // Comments and tracked changes.
    let mut authors = Vec::new();
    let mut initials = Vec::new();
    let mut change_authors = Vec::new();
    let mut dates = Vec::new();
    let mut comments = 0;
    let mut changes = 0;
    for name in names.iter().filter(|n| is_content(n)) {
        let Some(xml) = z.text(name)? else { continue };
        for el in collect(&xml, |n| {
            matches!(
                n,
                "annotation" | "creator" | "date" | "creator-initials" | "changed-region"
            )
        })? {
            match el.local() {
                "annotation" => comments += 1,
                "changed-region" => changes += 1,
                "creator" if in_annotation(&el) => authors.push(el.text.clone()),
                "creator" if in_change(&el) => change_authors.push(el.text.clone()),
                "creator-initials" if in_annotation(&el) => initials.push(el.text.clone()),
                "date"
                    if (in_annotation(&el) || in_change(&el))
                        && !el.text.trim().starts_with(NEUTRAL_DATE) =>
                {
                    dates.push(date_text(&el.text))
                }
                _ => {}
            }
        }
    }
    for (id, label, list) in [
        ("odf:comments:authors", "Comment authors", &authors),
        (
            "odf:comments:initials",
            "Comment author initials",
            &initials,
        ),
        (
            "odf:changes:authors",
            "Tracked change authors",
            &change_authors,
        ),
    ] {
        let list = distinct(list);
        if !list.is_empty() {
            push(
                &mut x,
                Field::new(id, label, list.join("; "), People, SRC_DOC).flag(Flag::Names),
            );
        }
    }
    dates.retain(|d| !d.is_empty());
    if !dates.is_empty() {
        dates.sort();
        let span = if dates[0] == dates[dates.len() - 1] {
            dates[0].clone()
        } else {
            format!("{} to {}", dates[0], dates[dates.len() - 1])
        };
        push(
            &mut x,
            Field::new(
                "odf:changes:dates",
                "Comment and change dates",
                span,
                Dates,
                SRC_DOC,
            ),
        );
    }
    if comments > 0 {
        push(
            &mut x,
            Field::new(
                "odf:comments:count",
                "Comments",
                plural(comments, "comment", "comments"),
                Description,
                SRC_DOC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
        x.notes.push(format!(
            "{}: their author names are removed by cleaning, the comments themselves stay; delete them in LibreOffice if they should not be shared.",
            plural(comments, "comment", "comments")
        ));
    }
    if changes > 0 {
        push(
            &mut x,
            Field::new(
                "odf:changes:count",
                "Tracked changes",
                plural(changes, "change", "changes"),
                Hidden,
                SRC_DOC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
        x.notes.push(format!(
            "{}: the names of who made them are removed by cleaning, the changes themselves stay; accept or reject them before sharing.",
            plural(changes, "tracked change", "tracked changes")
        ));
    }
    let users = distinct(
        &user_paths
            .iter()
            .filter_map(|p| user_in_path(p))
            .collect::<Vec<_>>(),
    );
    if !users.is_empty() {
        push(
            &mut x,
            Field::new(
                "odf:paths:users",
                "User names in file paths",
                users.join("; "),
                People,
                SRC_DOC,
            )
            .flag(Flag::Names),
        );
    }
    if names.iter().any(|n| n == "META-INF/documentsignatures.xml") {
        x.notes.push(
            "The document is digitally signed; changing it removes the signature's validity."
                .into(),
        );
    }
    Ok(x)
}

// ---------------------------------------------------------------------------
// Cleaning

fn meta_act(el: &El, opts: &CleanOptions) -> Act {
    if el.path.len() != 2 {
        return Act::Keep;
    }
    match find(el.local()) {
        Some(p) if opts.removes(p.cat) => Act::Drop,
        Some(p) if p.local == "template" && opts.removes(People) => match el.attr("xlink:href") {
            Some(h) if user_in_path(h).is_some() => {
                Act::Attrs(vec![("xlink:href".into(), Some(anonymize_path(h)))])
            }
            _ => Act::Keep,
        },
        _ => Act::Keep,
    }
}

fn content_act(el: &El, opts: &CleanOptions) -> Act {
    let note = in_annotation(el);
    let change = in_change(el);
    match el.local() {
        "creator" if (note || change) && opts.removes(People) => Act::Text(NEUTRAL.into()),
        "creator-initials" if note && opts.removes(People) => Act::Text(String::new()),
        "date" if note && opts.removes(Dates) => Act::Drop,
        "date" if change && opts.removes(Dates) => Act::Text(NEUTRAL_DATE.into()),
        _ => Act::Keep,
    }
}

fn settings_act(el: &El, opts: &CleanOptions) -> Act {
    match el.attr("config:name") {
        Some("PrinterName" | "PrinterSetup")
            if el.local() == "config-item" && opts.removes(Category::Device) =>
        {
            Act::Text(String::new())
        }
        _ => Act::Keep,
    }
}

/// The manifest without entries for dropped parts.
fn manifest_without(xml: &str, dropped: &dyn Fn(&str) -> bool) -> Result<String> {
    transform(
        xml,
        |el| match el.attr("manifest:full-path") {
            Some(p) if el.local() == "file-entry" && dropped(p) => Act::Drop,
            _ => Act::Keep,
        },
        &[],
    )
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let (mut z, _) = open(src)?;
    let names = z.names().to_vec();
    let hidden = opts.removes(Hidden);
    let dropped = |n: &str| hidden && (is_thumbnail(n) || is_versions(n));
    let mut plan: HashMap<String, Entry> = HashMap::new();
    for n in names.iter().filter(|n| dropped(n)) {
        plan.insert(n.clone(), Entry::Drop);
    }
    for name in &names {
        if plan.contains_key(name) {
            continue;
        }
        let act: Option<fn(&El, &CleanOptions) -> Act> = match name.as_str() {
            "meta.xml" => Some(meta_act),
            "settings.xml" => Some(settings_act),
            n if is_content(n) => Some(content_act),
            _ => None,
        };
        let replaced = match (name.as_str(), act) {
            ("META-INF/manifest.xml", _) if hidden => {
                let xml = z.text(name)?.unwrap_or_default();
                Some(manifest_without(&xml, &|p| {
                    is_thumbnail(p) || is_versions(p)
                })?)
            }
            (_, Some(act)) => {
                let xml = z.text(name)?.unwrap_or_default();
                let out = transform(&xml, |el| act(el, opts), &[])?;
                (out != xml).then_some(out)
            }
            _ => None,
        };
        if let Some(out) = replaced {
            plan.insert(name.clone(), Entry::Replace(out.into_bytes()));
        }
    }
    zipx::rewrite(src, dst, plan, Vec::new(), opts.removes(Dates))
}

// ---------------------------------------------------------------------------
// Editing

fn new_element(qname: &str, value: &str) -> String {
    let prefix = qname.split(':').next().unwrap_or("");
    let ns = if prefix == "dc" { NS_DC } else { NS_META };
    format!("<{qname} xmlns:{prefix}=\"{ns}\">{}</{qname}>", esc(value))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let (mut z, _) = open(src)?;
    let mut locals: Vec<&str> = Vec::new();
    let mut append = String::new();
    for e in edits {
        let (local, qname) = meta_element(e.key).ok_or_else(|| {
            format!(
                "{} cannot be edited in OpenDocument files.",
                e.key.label(crate::meta::Kind::Document)
            )
        })?;
        locals.push(local);
        let Some(v) = e.value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        match e.key {
            EditKey::DateTaken => {
                let d = DateTime::parse(v).ok_or_else(|| {
                    format!("\"{v}\" is not a date; use the form YYYY-MM-DD HH:MM:SS.")
                })?;
                append.push_str(&new_element(qname, &d.iso()));
            }
            EditKey::Keywords => {
                for k in v.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    append.push_str(&new_element(qname, k));
                }
            }
            _ => append.push_str(&new_element(qname, v)),
        }
    }
    let mut plan = HashMap::new();
    let mut add = Vec::new();
    match z.text("meta.xml")? {
        Some(xml) => {
            let meta_name = collect(&xml, |n| n == "meta")?
                .into_iter()
                .find(|e| e.path.len() == 1)
                .map(|e| e.name)
                .ok_or("The document information (meta.xml) is damaged.")?;
            let out = transform(
                &xml,
                |el| {
                    if el.path.len() == 2 && locals.contains(&el.local()) {
                        Act::Drop
                    } else {
                        Act::Keep
                    }
                },
                &[(meta_name.as_str(), append)],
            )?;
            plan.insert("meta.xml".to_string(), Entry::Replace(out.into_bytes()));
        }
        None => {
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-meta xmlns:office=\"{NS_OFFICE}\" xmlns:meta=\"{NS_META}\" xmlns:dc=\"{NS_DC}\" xmlns:xlink=\"{NS_XLINK}\" office:version=\"1.2\"><office:meta>{append}</office:meta></office:document-meta>"
            );
            add.push(("meta.xml".to_string(), xml.into_bytes()));
            let manifest = z
                .text("META-INF/manifest.xml")?
                .ok_or("The file has no manifest.")?;
            let root = collect(&manifest, |n| n == "manifest")?
                .into_iter()
                .next()
                .map(|e| e.name)
                .ok_or("The manifest is damaged.")?;
            let entry = "<manifest:file-entry manifest:full-path=\"meta.xml\" manifest:media-type=\"text/xml\"/>".to_string();
            let out = transform(&manifest, |_| Act::Keep, &[(root.as_str(), entry)])?;
            plan.insert(
                "META-INF/manifest.xml".to_string(),
                Entry::Replace(out.into_bytes()),
            );
        }
    }
    zipx::rewrite(src, dst, plan, add, false)
}

// ---------------------------------------------------------------------------
// Sample

/// An OpenDocument text with full meta, a comment by "Test Reviewer", a
/// tracked change by "Test Editor", a printer and a thumbnail.
pub fn sample() -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let text = "urn:oasis:names:tc:opendocument:xmlns:text:1.0";
    let meta = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-meta xmlns:office=\"{NS_OFFICE}\" xmlns:meta=\"{NS_META}\" xmlns:dc=\"{NS_DC}\" xmlns:xlink=\"{NS_XLINK}\" office:version=\"1.3\"><office:meta><meta:initial-creator>Test Author</meta:initial-creator><dc:creator>Test Editor</dc:creator><meta:printed-by>Test Printer User</meta:printed-by><dc:title>Quarterly Test Report</dc:title><dc:subject>Invented subject</dc:subject><dc:description>Draft for review</dc:description><meta:keyword>sample</meta:keyword><meta:keyword>test</meta:keyword><meta:creation-date>2024-05-01T10:20:30.123</meta:creation-date><dc:date>2024-05-02T11:00:00</dc:date><meta:print-date>2024-05-02T12:00:00</meta:print-date><meta:generator>TestOffice/1.0</meta:generator><meta:editing-cycles>7</meta:editing-cycles><meta:editing-duration>PT1H2M3S</meta:editing-duration><meta:template xlink:type=\"simple\" xlink:actuate=\"onRequest\" xlink:title=\"Letter\" xlink:href=\"file:///C:/Users/TestUser/Templates/Letter.ott\"/><meta:user-defined meta:name=\"ProjectCode\">TEST-123</meta:user-defined><meta:document-statistic meta:page-count=\"1\" meta:word-count=\"2\"/></office:meta></office:document-meta>");
    let content = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-content xmlns:office=\"{NS_OFFICE}\" xmlns:text=\"{text}\" xmlns:dc=\"{NS_DC}\" xmlns:meta=\"{NS_META}\" office:version=\"1.3\"><office:body><office:text><text:tracked-changes><text:changed-region text:id=\"ct1\"><text:insertion><office:change-info><dc:creator>Test Editor</dc:creator><dc:date>2024-05-02T09:00:00</dc:date></office:change-info></text:insertion></text:changed-region></text:tracked-changes><text:p>Hello <office:annotation><dc:creator>Test Reviewer</dc:creator><dc:date>2024-05-02T10:00:00</dc:date><meta:creator-initials>TR</meta:creator-initials><text:p>Please check</text:p></office:annotation><text:change-start text:change-id=\"ct1\"/>world<text:change-end text:change-id=\"ct1\"/></text:p></office:text></office:body></office:document-content>");
    let settings = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-settings xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:config=\"urn:oasis:names:tc:opendocument:xmlns:config:1.0\"><office:settings><config:config-item-set config:name=\"ooo:configuration-settings\"><config:config-item config:name=\"PrinterName\" config:type=\"string\">Test Printer 3000</config:config-item><config:config-item config:name=\"PrinterSetup\" config:type=\"base64Binary\">VGVzdCBQcmludGVy</config:config-item></config:config-item-set></office:settings></office:document-settings>";
    let styles = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-styles xmlns:office=\"{NS_OFFICE}\" office:version=\"1.3\"/>");
    let manifest = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.3\"><manifest:file-entry manifest:full-path=\"/\" manifest:version=\"1.3\" manifest:media-type=\"application/vnd.oasis.opendocument.text\"/><manifest:file-entry manifest:full-path=\"Thumbnails/thumbnail.png\" manifest:media-type=\"image/png\"/><manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"styles.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"meta.xml\" manifest:media-type=\"text/xml\"/><manifest:file-entry manifest:full-path=\"settings.xml\" manifest:media-type=\"text/xml\"/></manifest:manifest>";
    let entries: Vec<(&str, Vec<u8>, bool)> = vec![
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text".to_vec(),
            true,
        ),
        (
            "Thumbnails/thumbnail.png",
            b"\x89PNG\r\n\x1a\nnot a real picture".to_vec(),
            false,
        ),
        ("content.xml", content.into_bytes(), false),
        ("styles.xml", styles.into_bytes(), false),
        ("meta.xml", meta.into_bytes(), false),
        ("settings.xml", settings.as_bytes().to_vec(), false),
        ("META-INF/manifest.xml", manifest.as_bytes().to_vec(), false),
    ];
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let time = zip::DateTime::from_date_and_time(2024, 5, 2, 11, 0, 0).unwrap_or_default();
    for (name, data, stored) in entries {
        let method = if stored {
            zip::CompressionMethod::Stored
        } else {
            zip::CompressionMethod::Deflated
        };
        let opts = SimpleFileOptions::default()
            .compression_method(method)
            .last_modified_time(time);
        if w.start_file(name, opts).is_err() || w.write_all(&data).is_err() {
            return Vec::new();
        }
    }
    w.finish().map(|c| c.into_inner()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::xmlutil::well_formed;
    use crate::formats::zipx::tests::temp_dir;
    use crate::meta::CatSet;

    fn value(x: &Extract, id: &str) -> Option<String> {
        x.fields
            .iter()
            .find(|f| f.id == id)
            .map(|f| f.value.clone())
    }

    fn assert_invariant(x: &Extract, opts: &CleanOptions) {
        for f in &x.fields {
            assert!(
                !(f.removable && opts.remove.contains(f.category)),
                "left: {} = {}",
                f.id,
                f.value
            );
        }
    }

    /// mimetype first and stored, every XML part parses, every manifest
    /// entry exists.
    fn assert_valid(path: &Path) {
        let mut ar = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        {
            let first = ar.by_index(0).unwrap();
            assert_eq!(first.name(), "mimetype");
            assert_eq!(first.compression(), zip::CompressionMethod::Stored);
        }
        let mut z = Zip::open(path).unwrap();
        let names = z.names().to_vec();
        for n in names.iter().filter(|n| n.ends_with(".xml")) {
            assert!(well_formed(&z.text(n).unwrap().unwrap()), "{n}");
        }
        let manifest = z.text("META-INF/manifest.xml").unwrap().unwrap();
        for el in collect(&manifest, |n| n == "file-entry").unwrap() {
            let p = el.attr("manifest:full-path").unwrap();
            assert!(
                p == "/" || names.iter().any(|n| n == p),
                "manifest lists missing {p}"
            );
        }
    }

    #[test]
    fn read_finds_everything() {
        let dir = temp_dir("odf-read");
        let p = dir.join("a.odt");
        std::fs::write(&p, sample()).unwrap();
        let x = read(&p).unwrap();
        assert_eq!(value(&x, "odf:type").as_deref(), Some("OpenDocument text"));
        assert_eq!(
            value(&x, "odf:meta:initial-creator").as_deref(),
            Some("Test Author")
        );
        assert_eq!(
            value(&x, "odf:meta:creator").as_deref(),
            Some("Test Editor")
        );
        assert_eq!(
            value(&x, "odf:meta:printed-by").as_deref(),
            Some("Test Printer User")
        );
        assert_eq!(
            value(&x, "odf:meta:keyword").as_deref(),
            Some("sample, test")
        );
        assert_eq!(x.taken.as_deref(), Some("2024-05-01 10:20:30"));
        assert_eq!(
            value(&x, "odf:meta:editing-duration").as_deref(),
            Some("1 hours 2 minutes")
        );
        assert_eq!(
            value(&x, "odf:user:ProjectCode").as_deref(),
            Some("TEST-123")
        );
        assert_eq!(
            value(&x, "odf:settings:PrinterName").as_deref(),
            Some("Test Printer 3000")
        );
        assert!(value(&x, "odf:settings:PrinterSetup").is_some());
        assert!(value(&x, "odf:thumbnail:Thumbnails/thumbnail.png").is_some());
        assert_eq!(
            value(&x, "odf:comments:authors").as_deref(),
            Some("Test Reviewer")
        );
        assert_eq!(value(&x, "odf:comments:initials").as_deref(), Some("TR"));
        assert_eq!(
            value(&x, "odf:changes:authors").as_deref(),
            Some("Test Editor")
        );
        assert_eq!(
            value(&x, "odf:comments:count").as_deref(),
            Some("1 comment")
        );
        assert_eq!(value(&x, "odf:changes:count").as_deref(), Some("1 change"));
        assert_eq!(value(&x, "odf:paths:users").as_deref(), Some("TestUser"));
        assert_eq!(value(&x, "odf:stat:Pages").as_deref(), Some("1"));
        let tpl = x
            .fields
            .iter()
            .find(|f| f.id == "odf:meta:template")
            .unwrap();
        assert_eq!(tpl.flag, Some(Flag::Names));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clean_everything_and_each_category() {
        let dir = temp_dir("odf-clean");
        let p = dir.join("a.odt");
        std::fs::write(&p, sample()).unwrap();
        let out = dir.join("b.odt");
        let opts = CleanOptions::everything();
        clean(&p, &out, &opts).unwrap();
        assert_valid(&out);
        let x = read(&out).unwrap();
        assert_invariant(&x, &opts);
        assert!(value(&x, "odf:comments:count").is_some());
        let mut z = Zip::open(&out).unwrap();
        assert!(!z.has("Thumbnails/thumbnail.png"));
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
            "Test Editor",
            "Test Printer",
            "TEST-123",
            "2024-05",
        ] {
            assert!(!all.contains(secret), "{secret} left");
        }
        assert!(all.contains("Please check") && all.contains("world"));
        for cat in Category::ALL {
            let opts = CleanOptions {
                remove: CatSet::of(&[cat]),
                ..CleanOptions::everything()
            };
            let out = dir.join(format!("c-{}.odt", cat.key()));
            clean(&p, &out, &opts).unwrap();
            assert_valid(&out);
            assert_invariant(&read(&out).unwrap(), &opts);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn partial_clean_keeps_the_rest() {
        let dir = temp_dir("odf-partial");
        let p = dir.join("a.odt");
        std::fs::write(&p, sample()).unwrap();
        let out = dir.join("b.odt");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&p, &out, &opts).unwrap();
        assert_valid(&out);
        let x = read(&out).unwrap();
        assert_invariant(&x, &opts);
        assert_eq!(
            value(&x, "odf:meta:title").as_deref(),
            Some("Quarterly Test Report")
        );
        assert_eq!(x.taken.as_deref(), Some("2024-05-01 10:20:30"));
        assert!(value(&x, "odf:settings:PrinterName").is_some());
        let tpl = value(&x, "odf:meta:template").unwrap();
        assert!(
            tpl.contains("/Users/user/") && !tpl.contains("TestUser"),
            "{tpl}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edit_round_trip_with_kurdish() {
        let dir = temp_dir("odf-edit");
        let p = dir.join("a.odt");
        std::fs::write(&p, sample()).unwrap();
        let out = dir.join("b.odt");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &p,
            &out,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "نووسەر & <Test>"),
                Edit::set(EditKey::Keywords, "a, b ,c"),
                Edit::clear(EditKey::Subject),
                Edit::set(EditKey::DateTaken, "2023-01-02 03:04:05"),
            ],
        )
        .unwrap();
        assert_valid(&out);
        let x = read(&out).unwrap();
        assert_eq!(value(&x, "odf:meta:title").as_deref(), Some(title));
        assert_eq!(
            value(&x, "odf:meta:initial-creator").as_deref(),
            Some("نووسەر & <Test>")
        );
        assert_eq!(value(&x, "odf:meta:keyword").as_deref(), Some("a, b, c"));
        assert!(value(&x, "odf:meta:subject").is_none());
        assert_eq!(x.taken.as_deref(), Some("2023-01-02 03:04:05"));
        assert_eq!(
            x.fields.iter().filter(|f| f.id == "odf:meta:title").count(),
            1
        );
        assert!(edit(&p, &out, &[Edit::set(EditKey::Company, "x")]).is_err());
        assert!(edit(&p, &out, &[Edit::set(EditKey::DateTaken, "later")]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_input_is_an_error() {
        let dir = temp_dir("odf-bad");
        let data = sample();
        let p = dir.join("cut.odt");
        std::fs::write(&p, &data[..data.len() / 3]).unwrap();
        assert!(read(&p).is_err());
        assert!(clean(&p, &dir.join("o.odt"), &CleanOptions::everything()).is_err());
        std::fs::write(&p, b"garbage").unwrap();
        assert!(read(&p).is_err());
        assert_eq!(duration_text("P1DT0H0M5S"), "24 hours 0 minutes");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
