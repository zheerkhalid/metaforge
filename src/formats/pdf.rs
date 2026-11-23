//! PDF documents: the Info dictionary, the XMP packet, comment (annotation)
//! authors, attached files, the document id and older saved versions.
//!
//! Cleaning and editing always write the whole file anew (no incremental
//! update) and drop every object nothing refers to any more, so text from
//! earlier saved versions and orphaned metadata is really gone. The page
//! content streams are checked to be unchanged before anything is written.

use crate::formats::xmp;
use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use crate::util::DateTime;
use lopdf::xref::XrefType;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::BTreeSet;
use std::path::Path;

const SRC_INFO: &str = "PDF Info";
const SRC_XMP: &str = "PDF XMP";
const SRC_PDF: &str = "PDF";

const ENCRYPTED: &str =
    "This PDF is encrypted (password-protected). MetaForge shows what it can read, \
     but it cannot change an encrypted PDF without damaging it. Remove the password in a PDF \
     program first, then clean it here.";

/// Categories that something inside a PDF can belong to (all but Technical and Windows).
const PDF_CATEGORIES: [Category; 7] = [
    Category::Location,
    Category::People,
    Category::Device,
    Category::Dates,
    Category::Software,
    Category::Description,
    Category::Hidden,
];

// ---------------------------------------------------------------------------
// Loading and small helpers

fn load(bytes: &[u8]) -> Result<Document> {
    if !bytes.windows(5).take(1024).any(|w| w == b"%PDF-") {
        return Err("This is not a PDF file (the PDF header is missing).".into());
    }
    match std::panic::catch_unwind(|| Document::load_mem(bytes)) {
        Ok(Ok(doc)) => Ok(doc),
        Ok(Err(e)) => Err(format!(
            "This PDF could not be read; it may be damaged or incomplete ({e})."
        )),
        Err(_) => Err("This PDF could not be read; it may be damaged.".into()),
    }
}

/// Encrypted and not opened (the strings are still scrambled).
fn locked(doc: &Document) -> bool {
    doc.trailer.has(b"Encrypt")
}

/// Encrypted at all (also when it opened with the empty password).
fn encrypted(doc: &Document) -> bool {
    locked(doc) || doc.encryption_state.is_some()
}

fn deref<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    doc.dereference(obj).map(|(_, o)| o).unwrap_or(obj)
}

fn dict_of(obj: &Object) -> Option<&Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}

fn dict_of_mut(obj: &mut Object) -> Option<&mut Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&mut s.dict),
        _ => None,
    }
}

fn get_dict<'a>(doc: &'a Document, d: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    d.get(key).ok().and_then(|o| dict_of(deref(doc, o)))
}

fn name_of(d: &Dictionary, key: &[u8]) -> Option<String> {
    d.get(key)
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
}

/// A value as text: text strings (PDFDocEncoding, UTF-16, UTF-8), names, numbers.
fn text(obj: &Object) -> Option<String> {
    let s = match obj {
        Object::String(bytes, _) => lopdf::decode_text_string(obj)
            .unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned()),
        Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
        Object::Integer(i) => i.to_string(),
        Object::Real(r) => r.to_string(),
        Object::Boolean(b) => b.to_string(),
        _ => return None,
    };
    let s = s
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    (!s.is_empty()).then_some(s)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn pdf_date(dt: DateTime) -> String {
    format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}",
        dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
    )
}

fn root_id(doc: &Document) -> Option<ObjectId> {
    doc.trailer
        .get(b"Root")
        .ok()
        .and_then(|o| o.as_reference().ok())
}

fn info_dict(doc: &Document) -> Option<&Dictionary> {
    doc.trailer
        .get(b"Info")
        .ok()
        .and_then(|o| dict_of(deref(doc, o)))
}

/// How an Info dictionary key is shown and cleaned.
struct Spec {
    label: String,
    category: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
    removable: bool,
}

fn info_spec(key: &[u8]) -> Spec {
    let spec = |label: &str, category, flag, edit| Spec {
        label: label.to_string(),
        category,
        flag,
        edit,
        removable: true,
    };
    match key {
        b"Title" => spec("Title", Category::Description, None, Some(EditKey::Title)),
        b"Author" => spec(
            "Author",
            Category::People,
            Some(Flag::Names),
            Some(EditKey::Author),
        ),
        b"Subject" => spec(
            "Subject",
            Category::Description,
            None,
            Some(EditKey::Subject),
        ),
        b"Keywords" => spec(
            "Keywords",
            Category::Description,
            None,
            Some(EditKey::Keywords),
        ),
        b"Creator" => spec("Program used", Category::Software, None, None),
        b"Producer" => spec("PDF made by", Category::Software, None, None),
        b"CreationDate" => spec("Created", Category::Dates, None, Some(EditKey::DateTaken)),
        b"ModDate" => spec("Modified", Category::Dates, None, None),
        b"Trapped" => Spec {
            removable: false,
            ..spec("Trapped (print setting)", Category::Technical, None, None)
        },
        other => spec(
            &format!("Custom property: {}", String::from_utf8_lossy(other)),
            Category::Hidden,
            Some(Flag::Hidden),
            None,
        ),
    }
}

/// The Info key an edit changes.
fn info_key(key: EditKey) -> Option<&'static str> {
    Some(match key {
        EditKey::Title => "Title",
        EditKey::Author => "Author",
        EditKey::Subject => "Subject",
        EditKey::Keywords => "Keywords",
        EditKey::DateTaken => "CreationDate",
        _ => return None,
    })
}

const EDITABLE: [EditKey; 5] = [
    EditKey::Title,
    EditKey::Author,
    EditKey::Subject,
    EditKey::Keywords,
    EditKey::DateTaken,
];

fn catalog_xmp(doc: &Document) -> Option<String> {
    let root = doc.get_dictionary(root_id(doc)?).ok()?;
    let stream = deref(doc, root.get(b"Metadata").ok()?).as_stream().ok()?;
    let bytes = stream
        .decompressed_content()
        .unwrap_or_else(|_| stream.content.clone());
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Annotation dictionaries of all pages (referenced or written inline).
fn annotations(doc: &Document) -> Vec<&Dictionary> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for page in doc.page_iter() {
        let Ok(page) = doc.get_dictionary(page) else {
            continue;
        };
        let Some(arr) = page
            .get(b"Annots")
            .ok()
            .and_then(|o| deref(doc, o).as_array().ok())
        else {
            continue;
        };
        for item in arr {
            if let Object::Reference(id) = item {
                if !seen.insert(*id) {
                    continue;
                }
            }
            if let Some(d) = dict_of(deref(doc, item)) {
                out.push(d);
            }
        }
    }
    out
}

/// Calls `f` on every annotation dictionary of every page.
fn for_each_annotation(doc: &mut Document, f: &mut dyn FnMut(&mut Dictionary)) {
    let pages: Vec<ObjectId> = doc.page_iter().collect();
    let mut refs = BTreeSet::new();
    for page in pages {
        let array_ref = doc
            .get_dictionary(page)
            .ok()
            .and_then(|d| d.get(b"Annots").ok())
            .and_then(|o| o.as_reference().ok());
        let array = match array_ref {
            Some(id) => doc.objects.get_mut(&id).and_then(|o| o.as_array_mut().ok()),
            None => doc
                .objects
                .get_mut(&page)
                .and_then(dict_of_mut)
                .and_then(|d| d.get_mut(b"Annots").ok())
                .and_then(|o| o.as_array_mut().ok()),
        };
        for item in array.into_iter().flatten() {
            match item {
                Object::Reference(id) => {
                    refs.insert(*id);
                }
                Object::Dictionary(d) => f(d),
                _ => {}
            }
        }
    }
    for id in refs {
        if let Some(d) = doc.objects.get_mut(&id).and_then(dict_of_mut) {
            f(d);
        }
    }
}

fn is_widget(d: &Dictionary) -> bool {
    name_of(d, b"Subtype").as_deref() == Some("Widget")
}

/// A comment-like annotation (not a link, form field or popup window).
fn is_comment(d: &Dictionary) -> bool {
    !matches!(
        name_of(d, b"Subtype").as_deref(),
        None | Some(
            "Link"
                | "Widget"
                | "Popup"
                | "PrinterMark"
                | "TrapNet"
                | "Watermark"
                | "Screen"
                | "Movie"
                | "3D"
        )
    )
}

fn embedded_file_names(doc: &Document, node: &Dictionary, depth: usize, out: &mut Vec<String>) {
    if depth > 16 || out.len() > 1000 {
        return;
    }
    if let Some(arr) = node
        .get(b"Names")
        .ok()
        .and_then(|o| deref(doc, o).as_array().ok())
    {
        for pair in arr.chunks(2) {
            let key = text(deref(doc, &pair[0])).unwrap_or_default();
            let name = pair
                .get(1)
                .and_then(|o| dict_of(deref(doc, o)))
                .and_then(|fs| {
                    fs.get(b"UF")
                        .or_else(|_| fs.get(b"F"))
                        .ok()
                        .and_then(|o| text(deref(doc, o)))
                })
                .unwrap_or(key);
            out.push(name);
        }
    }
    if let Some(kids) = node
        .get(b"Kids")
        .ok()
        .and_then(|o| deref(doc, o).as_array().ok())
    {
        for kid in kids {
            if let Some(k) = dict_of(deref(doc, kid)) {
                embedded_file_names(doc, k, depth + 1, out);
            }
        }
    }
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// How many times the file was saved (incremental updates add a section each).
fn saved_versions(bytes: &[u8]) -> usize {
    let n = count(bytes, b"startxref");
    let head = &bytes[..bytes.len().min(1024)];
    // A linearized ("fast web view") file has two sections from one save.
    if n > 1 && head.windows(11).any(|w| w == b"/Linearized") {
        n - 1
    } else {
        n
    }
}

// ---------------------------------------------------------------------------
// Reading

pub fn read(path: &Path) -> Result<Extract> {
    let bytes = std::fs::read(path).map_err(|e| format!("Could not open the file: {e}"))?;
    read_bytes(&bytes)
}

fn read_bytes(bytes: &[u8]) -> Result<Extract> {
    let doc = load(bytes)?;
    let mut ex = Extract::default();
    let fixed = |id: &str, label: &str, value: String| {
        Field::new(id, label, value, Category::Technical, SRC_PDF).fixed()
    };
    ex.fields
        .push(fixed("pdf:version", "PDF version", doc.version.clone()));
    ex.fields.push(fixed(
        "pdf:pages",
        "Pages",
        doc.get_pages().len().to_string(),
    ));
    if encrypted(&doc) {
        ex.fields.push(fixed(
            "pdf:encrypted",
            "Encryption",
            "Encrypted (password-protected)".into(),
        ));
        ex.notes.push(ENCRYPTED.into());
    }
    if !locked(&doc) {
        read_info(&doc, &mut ex);
        read_xmp(&doc, &mut ex);
        read_annotations(&doc, &mut ex);
        read_hidden(&doc, &mut ex);
    }
    if let Some(id) = doc
        .trailer
        .get(b"ID")
        .ok()
        .and_then(|o| o.as_array().ok())
        .and_then(|a| a.first())
        .and_then(|o| o.as_str().ok())
        .filter(|s| !s.is_empty())
    {
        ex.fields.push(
            Field::new(
                "pdf:trailer:ID",
                "Document ID",
                hex(id),
                Category::Hidden,
                SRC_PDF,
            )
            .flag(Flag::Serial),
        );
    }
    let versions = saved_versions(bytes);
    if versions > 1 {
        let older = versions - 1;
        ex.fields.push(
            Field::new(
                "pdf:revisions",
                "Older saved versions",
                format!(
                    "{older} older version{} kept inside the file",
                    if older == 1 { "" } else { "s" }
                ),
                Category::Hidden,
                SRC_PDF,
            )
            .flag(Flag::Hidden),
        );
        ex.notes.push(format!(
            "This PDF was saved {versions} times by adding changes at the end. The older versions \
             (with their old text and properties) stay inside the file unless it is cleaned."
        ));
    }
    if !encrypted(&doc) {
        ex.editable = EDITABLE.to_vec();
    }
    Ok(ex)
}

fn read_info(doc: &Document, ex: &mut Extract) {
    let Some(info) = info_dict(doc) else {
        return;
    };
    for (key, value) in info.iter() {
        let Some(mut value) = text(deref(doc, value)) else {
            continue;
        };
        let spec = info_spec(key);
        if spec.category == Category::Dates {
            if let Some(dt) = DateTime::parse(&value) {
                value = dt.text();
                if key == b"CreationDate" {
                    ex.taken = Some(dt.text());
                }
            }
        }
        let mut f = Field::new(
            format!("pdf:info:{}", String::from_utf8_lossy(key)),
            spec.label,
            value,
            spec.category,
            SRC_INFO,
        );
        f.flag = spec.flag;
        f.edit = spec.edit;
        f.removable = spec.removable;
        ex.fields.push(f);
    }
}

fn read_xmp(doc: &Document, ex: &mut Extract) {
    let Some(xml) = catalog_xmp(doc) else {
        return;
    };
    let fields = xmp::fields(&xml, SRC_XMP);
    if fields.is_empty() {
        ex.fields.push(
            Field::new(
                "pdf:xmp",
                "XMP metadata packet",
                format!("{} bytes", xml.len()),
                Category::Hidden,
                SRC_XMP,
            )
            .flag(Flag::Hidden),
        );
    } else {
        ex.fields.extend(fields);
    }
}

fn read_annotations(doc: &Document, ex: &mut Extract) {
    let mut authors = BTreeSet::new();
    let mut dates = BTreeSet::new();
    let mut comments = 0;
    for a in annotations(doc) {
        if !is_widget(a) {
            if let Some(t) = a.get(b"T").ok().and_then(|o| text(deref(doc, o))) {
                authors.insert(t);
            }
        }
        for key in [&b"M"[..], b"CreationDate"] {
            if let Some(d) = a.get(key).ok().and_then(|o| text(deref(doc, o))) {
                dates.insert(DateTime::parse(&d).map(|d| d.text()).unwrap_or(d));
            }
        }
        if is_comment(a) {
            comments += 1;
        }
    }
    if !authors.is_empty() {
        ex.fields.push(
            Field::new(
                "pdf:annot:T",
                "Comment authors",
                authors.into_iter().collect::<Vec<_>>().join("; "),
                Category::People,
                SRC_PDF,
            )
            .flag(Flag::Names),
        );
    }
    if !dates.is_empty() {
        let newest = dates.iter().next_back().cloned().unwrap_or_default();
        let value = if dates.len() == 1 {
            newest
        } else {
            format!("{} dates, newest {newest}", dates.len())
        };
        ex.fields.push(Field::new(
            "pdf:annot:dates",
            "Comment dates",
            value,
            Category::Dates,
            SRC_PDF,
        ));
    }
    if comments > 0 {
        let what = if comments == 1 {
            "1 comment".to_string()
        } else {
            format!("{comments} comments")
        };
        ex.fields.push(
            Field::new(
                "pdf:annot:count",
                "Comments",
                format!(
                    "{what}: their author names are removed, the comments themselves stay; \
                     delete them in a PDF program if they should not be shared"
                ),
                Category::Description,
                SRC_PDF,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
    }
}

fn read_hidden(doc: &Document, ex: &mut Extract) {
    let root = root_id(doc);
    let mut metadata = 0;
    let mut piece_info = 0;
    let mut javascript = false;
    for (id, obj) in &doc.objects {
        let Some(d) = dict_of(obj) else {
            continue;
        };
        if d.has(b"Metadata") && Some(*id) != root {
            metadata += 1;
        }
        if d.has(b"PieceInfo") {
            piece_info += 1;
        }
        if d.has(b"JS") || name_of(d, b"S").as_deref() == Some("JavaScript") {
            javascript = true;
        }
    }
    if metadata > 0 {
        ex.fields.push(
            Field::new(
                "pdf:metadata:other",
                "Other metadata packets",
                format!("{metadata} (in pages, pictures or fonts)"),
                Category::Hidden,
                SRC_PDF,
            )
            .flag(Flag::Hidden),
        );
    }
    if piece_info > 0 {
        ex.fields.push(
            Field::new(
                "pdf:PieceInfo",
                "Private program data",
                format!(
                    "{piece_info} block{}",
                    if piece_info == 1 { "" } else { "s" }
                ),
                Category::Hidden,
                SRC_PDF,
            )
            .flag(Flag::Hidden),
        );
    }
    let catalog = root.and_then(|r| doc.get_dictionary(r).ok());
    let names = catalog.and_then(|c| get_dict(doc, c, b"Names"));
    if let Some(tree) = names.and_then(|n| get_dict(doc, n, b"EmbeddedFiles")) {
        let mut files = Vec::new();
        embedded_file_names(doc, tree, 0, &mut files);
        if !files.is_empty() {
            ex.fields.push(
                Field::new(
                    "pdf:EmbeddedFiles",
                    "Attached files",
                    format!("{} ({})", files.len(), files.join("; ")),
                    Category::Hidden,
                    SRC_PDF,
                )
                .flag(Flag::Hidden)
                .fixed(),
            );
            ex.notes.push(format!(
                "{} file{} attached inside this PDF. Attachments are content and stay; remove \
                 them in a PDF program if they should not be shared.",
                files.len(),
                if files.len() == 1 { " is" } else { "s are" }
            ));
        }
    }
    if javascript || names.is_some_and(|n| n.has(b"JavaScript")) {
        ex.notes.push(
            "This PDF contains JavaScript (scripts a PDF viewer may run when it is opened).".into(),
        );
    }
}

// ---------------------------------------------------------------------------
// Cleaning and editing

fn page_contents(doc: &Document) -> Vec<Vec<u8>> {
    doc.page_iter()
        .map(|p| doc.get_page_content(p).unwrap_or_default())
        .collect()
}

/// Replaces (or with `None` removes) the catalog's XMP packet.
fn set_catalog_xmp(doc: &mut Document, xml: Option<String>) {
    let Some(root) = root_id(doc) else {
        return;
    };
    let existing = doc
        .get_dictionary(root)
        .ok()
        .and_then(|c| c.get(b"Metadata").ok())
        .and_then(|o| o.as_reference().ok());
    match xml {
        None => {
            if let Ok(c) = doc.get_dictionary_mut(root) {
                c.remove(b"Metadata");
            }
        }
        Some(xml) => {
            let mut dict = Dictionary::new();
            dict.set("Type", Object::Name(b"Metadata".to_vec()));
            dict.set("Subtype", Object::Name(b"XML".to_vec()));
            let stream =
                Object::Stream(Stream::new(dict, xml.into_bytes()).with_compression(false));
            match existing {
                Some(id) => {
                    doc.objects.insert(id, stream);
                }
                None => {
                    let id = doc.add_object(stream);
                    if let Ok(c) = doc.get_dictionary_mut(root) {
                        c.set("Metadata", Object::Reference(id));
                    }
                }
            }
        }
    }
}

/// Strips the catalog XMP for `opts`; a packet with nothing left to show is dropped.
fn strip_xmp(doc: &mut Document, opts: &CleanOptions) {
    if !PDF_CATEGORIES.iter().any(|c| opts.removes(*c)) {
        return;
    }
    let Some(xml) = catalog_xmp(doc) else {
        return;
    };
    let kept = if opts.is_everything() {
        None
    } else {
        xmp::strip(&xml, opts).filter(|s| !xmp::fields(s, SRC_XMP).is_empty())
    };
    set_catalog_xmp(doc, kept);
}

/// The Info dictionary as an indirect object, created when missing.
fn info_object(doc: &mut Document) -> ObjectId {
    match doc.trailer.get(b"Info").ok().cloned() {
        Some(Object::Reference(id)) if doc.get_dictionary(id).is_ok() => id,
        Some(Object::Dictionary(d)) => {
            let id = doc.add_object(d);
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
        _ => {
            let id = doc.add_object(Dictionary::new());
            doc.trailer.set("Info", Object::Reference(id));
            id
        }
    }
}

/// Drops the Info dictionary from the trailer when it is empty.
fn drop_empty_info(doc: &mut Document) {
    if info_dict(doc).is_some_and(|d| d.is_empty()) {
        doc.trailer.remove(b"Info");
    }
}

fn clean_doc(doc: &mut Document, opts: &CleanOptions) {
    // Info dictionary.
    if opts.is_everything() {
        doc.trailer.remove(b"Info");
    } else if doc.trailer.has(b"Info") {
        let id = info_object(doc);
        if let Ok(info) = doc.get_dictionary_mut(id) {
            let doomed: Vec<Vec<u8>> = info
                .iter()
                .filter(|(k, _)| {
                    let s = info_spec(k);
                    s.removable && opts.removes(s.category)
                })
                .map(|(k, _)| k.clone())
                .collect();
            for k in doomed {
                info.remove(&k);
            }
        }
        drop_empty_info(doc);
    }
    strip_xmp(doc, opts);
    // Annotation authors and dates.
    let people = opts.removes(Category::People);
    let dates = opts.removes(Category::Dates);
    if people || dates {
        for_each_annotation(doc, &mut |a| {
            if people && !is_widget(a) {
                a.remove(b"T");
            }
            if dates {
                a.remove(b"M");
                a.remove(b"CreationDate");
            }
        });
    }
    if opts.removes(Category::Hidden) {
        doc.trailer.remove(b"ID");
        let root = root_id(doc);
        for (id, obj) in doc.objects.iter_mut() {
            if let Some(d) = dict_of_mut(obj) {
                d.remove(b"PieceInfo");
                if Some(*id) != root {
                    d.remove(b"Metadata");
                }
            }
        }
    }
}

/// Prepares a full rewrite: a fresh trailer, no unreachable objects, a classic
/// cross-reference table.
fn finish(doc: &mut Document) {
    let mut trailer = Dictionary::new();
    for key in [&b"Root"[..], b"Info", b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            trailer.set(key.to_vec(), v.clone());
        }
    }
    doc.trailer = trailer;
    doc.prune_objects();
    doc.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
}

/// Writes the document and checks the pages survived unchanged.
fn save_checked(doc: &mut Document, pages_before: &[Vec<u8>]) -> Result<Vec<u8>> {
    finish(doc);
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("The PDF could not be written: {e}"))?;
    let check =
        load(&out).map_err(|_| "The rewritten PDF did not read back; nothing was saved.")?;
    if page_contents(&check) != pages_before {
        return Err(
            "The rewritten PDF did not keep its pages unchanged; nothing was saved.".into(),
        );
    }
    Ok(out)
}

fn open_for_change(src: &Path) -> Result<(Document, Vec<Vec<u8>>)> {
    let bytes = std::fs::read(src).map_err(|e| format!("Could not open the file: {e}"))?;
    let doc = load(&bytes)?;
    if encrypted(&doc) {
        return Err(ENCRYPTED.into());
    }
    let pages = page_contents(&doc);
    Ok((doc, pages))
}

fn write(dst: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(dst, bytes).map_err(|e| format!("Could not write {}: {e}", dst.display()))
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let (mut doc, pages) = open_for_change(src)?;
    clean_doc(&mut doc, opts);
    let out = save_checked(&mut doc, &pages)?;
    write(dst, &out)
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let (mut doc, pages) = open_for_change(src)?;
    let id = info_object(&mut doc);
    let mut touched = Vec::new();
    for e in edits {
        let key = info_key(e.key)
            .ok_or_else(|| format!("\"{}\" cannot be edited in a PDF.", e.key.key()))?;
        let value = match e.value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            None => None,
            Some(v) if e.key == EditKey::DateTaken => {
                let dt = DateTime::parse(v)
                    .ok_or_else(|| format!("\"{v}\" is not a date; use YYYY-MM-DD HH:MM:SS."))?;
                Some(Object::string_literal(pdf_date(dt)))
            }
            Some(v) => Some(lopdf::text_string(v)),
        };
        let info = doc
            .get_dictionary_mut(id)
            .map_err(|e| format!("The PDF properties could not be changed: {e}"))?;
        match value {
            Some(v) => info.set(key, v),
            None => {
                info.remove(key.as_bytes());
            }
        }
        touched.push(match e.key {
            EditKey::Author => Category::People,
            EditKey::DateTaken => Category::Dates,
            _ => Category::Description,
        });
    }
    drop_empty_info(&mut doc);
    // The XMP copy of the changed properties would contradict the new values.
    if !touched.is_empty() {
        strip_xmp(&mut doc, &CleanOptions::only(&touched));
    }
    let out = save_checked(&mut doc, &pages)?;
    write(dst, &out)
}

// ---------------------------------------------------------------------------
// Sample

/// Builds a PDF from numbered object bodies, with an optional incremental
/// update section (`update`: objects replaced in a second save).
fn build_pdf(objects: &[(u32, Vec<u8>)], trailer: &str, update: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let size = objects.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
    let mut offsets = vec![None; size as usize];
    for (n, body) in objects {
        offsets[*n as usize] = Some(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref1 = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for off in &offsets[1..] {
        match off {
            Some(o) => out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} {trailer} >>\nstartxref\n{xref1}\n%%EOF\n").as_bytes(),
    );
    if !update.is_empty() {
        let mut entries = String::new();
        for (n, body) in update {
            entries.push_str(&format!("{n} 1\n{:010} 00000 n \n", out.len()));
            out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref2 = out.len();
        out.extend_from_slice(
            format!(
                "xref\n0 1\n0000000000 65535 f \n{entries}trailer\n<< /Size {size} {trailer} /Prev {xref1} >>\nstartxref\n{xref2}\n%%EOF\n"
            )
            .as_bytes(),
        );
    }
    out
}

fn stream_obj(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

const SAMPLE_XMP: &str = "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\
<dc:creator><rdf:Seq><rdf:li>Test Author</rdf:li></rdf:Seq></dc:creator>\
<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Annual Report</rdf:li></rdf:Alt></dc:title>\
<xmp:CreatorTool>ExampleWriter 3</xmp:CreatorTool><xmp:CreateDate>2024-03-01T10:15:00+03:00</xmp:CreateDate>\
</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>";

/// The page content of [`sample`].
const SAMPLE_CONTENT: &[u8] = b"0 0 1 rg 20 20 100 100 re f\n";

/// A one-page PDF with an Info dictionary, XMP, a comment by "Test Reviewer",
/// an attached file, private program data, a document id and an incremental
/// update (the first save had other title and author).
pub fn sample() -> Vec<u8> {
    let objects: Vec<(u32, Vec<u8>)> = vec![
        (
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /Names << /EmbeddedFiles 8 0 R >> >>"
                .to_vec(),
        ),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Annots [7 0 R] \
              /PieceInfo << /ExampleApp << /LastModified (D:20240302090000) /Private (TestUser workspace) >> >> >>"
                .to_vec(),
        ),
        (4, stream_obj("", SAMPLE_CONTENT)),
        (
            5,
            stream_obj("/Type /Metadata /Subtype /XML", SAMPLE_XMP.as_bytes()),
        ),
        (
            6,
            b"<< /Title (Old Draft Title) /Author (Old Author Name) /Producer (ExamplePDF Library 0.9) >>"
                .to_vec(),
        ),
        (
            7,
            b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /T (Test Reviewer) \
              /Contents (Please check the totals) /M (D:20240302091500+03'00') >>"
                .to_vec(),
        ),
        (8, b"<< /Names [(notes.txt) 9 0 R] >>".to_vec()),
        (
            9,
            b"<< /Type /Filespec /F (notes.txt) /UF (notes.txt) /EF << /F 10 0 R >> >>".to_vec(),
        ),
        (10, stream_obj("/Type /EmbeddedFile", b"invented notes")),
    ];
    let info = b"<< /Title (Annual Report) /Author (Test Author) /Subject (Finance) \
        /Keywords (report, 2024) /Creator (ExampleWriter 3) /Producer (ExamplePDF Library 1.0) \
        /CreationDate (D:20240301101500+03'00') /ModDate (D:20240305120000+03'00') \
        /Company (Example Co) /Trapped /False >>"
        .to_vec();
    build_pdf(
        &objects,
        "/Root 1 0 R /Info 6 0 R /ID [<0123456789ABCDEF0123456789ABCDEF> <0123456789ABCDEF0123456789ABCDEF>]",
        &[(6, info)],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> TempDir {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "metaforge-pdf-{name}-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn field<'a>(ex: &'a Extract, id: &str) -> Option<&'a Field> {
        ex.fields.iter().find(|f| f.id == id)
    }

    fn contains(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    fn assert_invariant(ex: &Extract, opts: &CleanOptions) {
        let left: Vec<_> = ex
            .fields
            .iter()
            .filter(|f| f.removable && opts.removes(f.category))
            .collect();
        assert!(left.is_empty(), "left after clean: {left:?}");
    }

    fn assert_valid(out: &[u8]) {
        let doc = Document::load_mem(out).unwrap();
        assert_eq!(doc.get_pages().len(), 1);
        assert_eq!(page_contents(&doc), vec![SAMPLE_CONTENT.to_vec()]);
        assert_eq!(saved_versions(out), 1);
    }

    #[test]
    fn reads_sample() {
        let ex = read_bytes(&sample()).unwrap();
        assert_eq!(field(&ex, "pdf:info:Title").unwrap().value, "Annual Report");
        let author = field(&ex, "pdf:info:Author").unwrap();
        assert_eq!(author.value, "Test Author");
        assert_eq!(author.flag, Some(Flag::Names));
        assert_eq!(author.edit, Some(EditKey::Author));
        assert_eq!(
            field(&ex, "pdf:info:Creator").unwrap().category,
            Category::Software
        );
        assert_eq!(
            field(&ex, "pdf:info:CreationDate").unwrap().value,
            "2024-03-01 10:15:00"
        );
        assert_eq!(ex.taken.as_deref(), Some("2024-03-01 10:15:00"));
        let company = field(&ex, "pdf:info:Company").unwrap();
        assert_eq!(company.category, Category::Hidden);
        assert!(!field(&ex, "pdf:info:Trapped").unwrap().removable);
        assert_eq!(field(&ex, "pdf:annot:T").unwrap().value, "Test Reviewer");
        let comments = field(&ex, "pdf:annot:count").unwrap();
        assert!(!comments.removable && comments.value.starts_with("1 comment"));
        assert!(field(&ex, "pdf:annot:dates").is_some());
        assert_eq!(
            field(&ex, "pdf:trailer:ID").unwrap().flag,
            Some(Flag::Serial)
        );
        assert!(field(&ex, "pdf:revisions").is_some());
        assert!(field(&ex, "pdf:PieceInfo").is_some());
        assert!(field(&ex, "pdf:EmbeddedFiles")
            .unwrap()
            .value
            .contains("notes.txt"));
        assert!(ex.fields.iter().any(|f| f.source == SRC_XMP));
        assert_eq!(field(&ex, "pdf:pages").unwrap().value, "1");
        assert!(ex.editable.contains(&EditKey::Title));
    }

    #[test]
    fn clean_everything_rewrites_without_old_versions() {
        let t = TempDir::new("all");
        let src = t.file("in.pdf", &sample());
        let dst = t.0.join("out.pdf");
        let opts = CleanOptions::everything();
        clean(&src, &dst, &opts).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), sample(), "source untouched");
        let out = std::fs::read(&dst).unwrap();
        assert_valid(&out);
        let ex = read(&dst).unwrap();
        assert_invariant(&ex, &opts);
        for s in [
            "Old Draft Title",
            "Old Author Name",
            "Test Author",
            "Test Reviewer",
            "Annual Report",
            "xmpmeta",
            "PieceInfo",
            "TestUser",
            "ExamplePDF",
        ] {
            assert!(!contains(&out, s), "{s} still in the file");
        }
        // Content and attachments stay.
        assert!(contains(&out, "Please check the totals"));
        assert!(field(&ex, "pdf:EmbeddedFiles").is_some());
        assert!(field(&ex, "pdf:annot:count").is_some());
    }

    #[test]
    fn partial_clean_keeps_the_rest() {
        let t = TempDir::new("partial");
        let src = t.file("in.pdf", &sample());
        let dst = t.0.join("out.pdf");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&src, &dst, &opts).unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert_valid(&out);
        let ex = read(&dst).unwrap();
        assert_invariant(&ex, &opts);
        assert!(field(&ex, "pdf:info:Author").is_none());
        assert!(field(&ex, "pdf:annot:T").is_none());
        assert_eq!(field(&ex, "pdf:info:Title").unwrap().value, "Annual Report");
        assert!(field(&ex, "pdf:info:CreationDate").is_some());
        assert!(field(&ex, "pdf:info:Company").is_some());
        assert!(field(&ex, "pdf:trailer:ID").is_some());
        assert!(field(&ex, "pdf:annot:dates").is_some());
        assert!(!contains(&out, "Old Author Name"));
    }

    #[test]
    fn hidden_only_removes_hidden() {
        let t = TempDir::new("hidden");
        let src = t.file("in.pdf", &sample());
        let dst = t.0.join("out.pdf");
        let opts = CleanOptions::only(&[Category::Hidden]);
        clean(&src, &dst, &opts).unwrap();
        let ex = read(&dst).unwrap();
        assert_invariant(&ex, &opts);
        assert!(field(&ex, "pdf:trailer:ID").is_none());
        assert!(field(&ex, "pdf:info:Company").is_none());
        assert!(field(&ex, "pdf:info:Author").is_some());
    }

    #[test]
    fn edit_round_trip() {
        let t = TempDir::new("edit");
        let src = t.file("in.pdf", &sample());
        let dst = t.0.join("out.pdf");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "Example Writer"),
                Edit::set(EditKey::DateTaken, "2025-01-02 03:04:05"),
                Edit::clear(EditKey::Keywords),
            ],
        )
        .unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert_valid(&out);
        let ex = read(&dst).unwrap();
        assert_eq!(field(&ex, "pdf:info:Title").unwrap().value, title);
        assert_eq!(
            field(&ex, "pdf:info:Author").unwrap().value,
            "Example Writer"
        );
        assert_eq!(ex.taken.as_deref(), Some("2025-01-02 03:04:05"));
        assert!(field(&ex, "pdf:info:Keywords").is_none());
        assert_eq!(field(&ex, "pdf:info:Subject").unwrap().value, "Finance");
        // No stale copy of the old values anywhere (old revision, XMP).
        assert!(!ex.fields.iter().any(|f| f.value.contains("Test Author")));
        assert!(!contains(&out, "Test Author") && !contains(&out, "Old Draft Title"));
        assert!(edit(&src, &dst, &[Edit::set(EditKey::DateTaken, "soon")]).is_err());
        assert!(edit(&src, &dst, &[Edit::set(EditKey::Album, "x")]).is_err());
    }

    #[test]
    fn encrypted_is_read_but_not_changed() {
        let pdf = build_pdf(
            &[
                (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
                (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
                (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_vec()),
                (
                    4,
                    format!(
                        "<< /Filter /Standard /V 1 /R 2 /O <{}> /U <{}> /P -4 >>",
                        "AB".repeat(32),
                        "CD".repeat(32)
                    )
                    .into_bytes(),
                ),
            ],
            "/Root 1 0 R /Encrypt 4 0 R /ID [<00112233445566778899AABBCCDDEEFF> <00112233445566778899AABBCCDDEEFF>]",
            &[],
        );
        let t = TempDir::new("enc");
        let src = t.file("in.pdf", &pdf);
        let ex = read(&src).unwrap();
        assert!(field(&ex, "pdf:encrypted").is_some());
        assert!(ex.editable.is_empty());
        let err = clean(&src, &t.0.join("out.pdf"), &CleanOptions::everything()).unwrap_err();
        assert!(err.contains("encrypted"));
        assert!(!t.0.join("out.pdf").exists());
    }

    #[test]
    fn garbage_and_truncated_fail_cleanly() {
        let t = TempDir::new("bad");
        let s = sample();
        for (i, bytes) in [
            b"hello world".to_vec(),
            s[..s.len() / 2].to_vec(),
            s[..40].to_vec(),
            Vec::new(),
        ]
        .into_iter()
        .enumerate()
        {
            let p = t.file(&format!("bad{i}.pdf"), &bytes);
            assert!(read(&p).is_err(), "case {i}");
            assert!(clean(&p, &t.0.join("o.pdf"), &CleanOptions::everything()).is_err());
        }
    }
}
