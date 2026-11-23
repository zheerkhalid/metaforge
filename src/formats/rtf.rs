//! Rich Text Format: the `{\info ...}` group, `{\*\generator}`,
//! `{\*\userprops}` (custom properties), `{\*\rsidtbl}` (editing-session
//! ids), `{\*\revtbl}` (tracked-change authors) and comment authors
//! (`{\*\atnauthor}`, `{\*\atnid}`).
//!
//! The file is tokenised (control words, symbols, groups, text) only to find
//! byte ranges; cleaning and editing splice those ranges and leave every
//! other byte as it was.

use crate::meta::{Category, CleanOptions, Edit, EditKey, Extract, Field, Flag, Result};
use crate::util::DateTime;
use std::collections::BTreeSet;
use std::path::Path;

const SRC: &str = "RTF info";

// ---------------------------------------------------------------------------
// Tokens and groups

#[derive(Clone, Debug)]
enum Tok {
    Open(usize),
    Close(usize),
    /// `\word123 `: byte range including the delimiting space.
    Word {
        start: usize,
        end: usize,
        name: String,
        param: Option<i32>,
    },
    /// `\'hh` (hex) or another control symbol such as `\{`, `\~`, `\*`.
    Sym {
        ch: u8,
        hex: Option<u8>,
    },
    Text(usize, usize),
}

fn tokenize(data: &[u8]) -> Result<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut i = 0;
    let n = data.len();
    while i < n {
        match data[i] {
            b'{' => {
                toks.push(Tok::Open(i));
                i += 1;
            }
            b'}' => {
                toks.push(Tok::Close(i));
                i += 1;
            }
            b'\\' => {
                let start = i;
                i += 1;
                if i >= n {
                    break;
                }
                if data[i].is_ascii_alphabetic() {
                    let ns = i;
                    while i < n && data[i].is_ascii_alphabetic() && i - ns < 32 {
                        i += 1;
                    }
                    let name = String::from_utf8_lossy(&data[ns..i]).into_owned();
                    let ps = i;
                    if i < n && data[i] == b'-' {
                        i += 1;
                    }
                    while i < n && data[i].is_ascii_digit() && i - ps < 11 {
                        i += 1;
                    }
                    let param = std::str::from_utf8(&data[ps..i])
                        .ok()
                        .and_then(|s| s.parse::<i64>().ok())
                        .map(|v| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
                    if i < n && data[i] == b' ' {
                        i += 1;
                    }
                    if name == "bin" {
                        // Raw binary data follows; skip it.
                        i = i.saturating_add(param.unwrap_or(0).max(0) as usize).min(n);
                    }
                    toks.push(Tok::Word {
                        start,
                        end: i,
                        name,
                        param,
                    });
                } else if data[i] == b'\'' {
                    let hex = data
                        .get(i + 1..i + 3)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    toks.push(Tok::Sym { ch: b'\'', hex });
                    i += if hex.is_some() { 3 } else { 1 };
                } else {
                    toks.push(Tok::Sym {
                        ch: data[i],
                        hex: None,
                    });
                    i += 1;
                }
            }
            _ => {
                let s = i;
                while i < n && !matches!(data[i], b'{' | b'}' | b'\\') {
                    i += 1;
                }
                toks.push(Tok::Text(s, i));
            }
        }
    }
    Ok(toks)
}

#[derive(Clone, Debug)]
struct Group {
    /// Token indexes of `{` and `}`.
    open: usize,
    close: usize,
    /// First token after the destination name.
    body: usize,
    /// Byte range of the whole group, `{` to `}` inclusive.
    start: usize,
    end: usize,
    name: String,
    parent: Option<usize>,
    depth: usize,
}

struct Doc {
    data: Vec<u8>,
    toks: Vec<Tok>,
    groups: Vec<Group>,
}

impl Doc {
    fn parse(data: Vec<u8>) -> Result<Doc> {
        if !data.starts_with(b"{\\rtf") {
            return Err("This is not an RTF file (it does not start with {\\rtf).".into());
        }
        let toks = tokenize(&data)?;
        let mut groups: Vec<Group> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (ti, t) in toks.iter().enumerate() {
            match t {
                Tok::Open(pos) => {
                    let mut body = ti + 1;
                    if let Some(Tok::Sym { ch: b'*', .. }) = toks.get(body) {
                        body += 1;
                    }
                    let name = match toks.get(body) {
                        Some(Tok::Word { name, .. }) => {
                            body += 1;
                            name.clone()
                        }
                        _ => String::new(),
                    };
                    groups.push(Group {
                        open: ti,
                        close: ti,
                        body,
                        start: *pos,
                        end: *pos,
                        name,
                        parent: stack.last().copied(),
                        depth: stack.len(),
                    });
                    stack.push(groups.len() - 1);
                }
                Tok::Close(pos) => {
                    let g = stack
                        .pop()
                        .ok_or("This RTF file is damaged (a group closes that never opened).")?;
                    groups[g].close = ti;
                    groups[g].end = pos + 1;
                    groups[g].body = groups[g].body.min(ti);
                }
                _ => {}
            }
        }
        if !stack.is_empty() || groups.is_empty() {
            return Err("This RTF file is damaged or incomplete (a group is never closed).".into());
        }
        Ok(Doc { data, toks, groups })
    }

    fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (usize, &'a Group)> + 'a {
        self.groups
            .iter()
            .enumerate()
            .filter(move |(_, g)| g.name == name)
    }

    fn children(&self, parent: usize) -> impl Iterator<Item = (usize, &Group)> + '_ {
        self.groups
            .iter()
            .enumerate()
            .filter(move |(_, g)| g.parent == Some(parent))
    }

    fn info(&self) -> Option<usize> {
        self.named("info").map(|(i, _)| i).next()
    }

    /// The text of a group's body.
    fn text(&self, g: &Group) -> String {
        decode(&self.data, &self.toks[g.body..g.close])
    }

    /// Control words directly inside a group (not in a nested group).
    fn direct_words(&self, g: &Group) -> Vec<&Tok> {
        let mut depth = 0usize;
        let mut out = Vec::new();
        for t in &self.toks[g.body..g.close] {
            match t {
                Tok::Open(_) => depth += 1,
                Tok::Close(_) => depth = depth.saturating_sub(1),
                Tok::Word { .. } if depth == 0 => out.push(t),
                _ => {}
            }
        }
        out
    }

    fn date(&self, g: &Group) -> Option<DateTime> {
        let mut dt = DateTime {
            year: 0,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
        };
        for t in self.direct_words(g) {
            if let Tok::Word {
                name,
                param: Some(p),
                ..
            } = t
            {
                let v = (*p).max(0) as u32;
                match name.as_str() {
                    "yr" => dt.year = *p as i64,
                    "mo" => dt.month = v,
                    "dy" => dt.day = v,
                    "hr" => dt.hour = v,
                    "min" => dt.minute = v,
                    "sec" => dt.second = v,
                    _ => {}
                }
            }
        }
        let ok = (1000..=9999).contains(&dt.year)
            && (1..=12).contains(&dt.month)
            && (1..=31).contains(&dt.day)
            && dt.hour < 24
            && dt.minute < 60
            && dt.second < 61;
        ok.then_some(dt)
    }

    fn count_words(&self, names: &[&str]) -> usize {
        self.toks
            .iter()
            .filter(|t| matches!(t, Tok::Word { name, .. } if names.contains(&name.as_str())))
            .count()
    }
}

/// Windows-1252 for the bytes 0x80..0x9F; other bytes map to Latin-1.
fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&b) {
        HIGH[(b - 0x80) as usize]
    } else {
        b as char
    }
}

/// Text of a token run: `\'hh`, `\uN` (with its fallback characters
/// skipped), escaped braces and backslashes.
fn decode(data: &[u8], toks: &[Tok]) -> String {
    let mut out = String::new();
    let mut units: Vec<u16> = Vec::new();
    let mut uc = 1usize;
    let mut skip = 0usize;
    let flush = |units: &mut Vec<u16>, out: &mut String| {
        if !units.is_empty() {
            out.push_str(&String::from_utf16_lossy(units));
            units.clear();
        }
    };
    for t in toks {
        match t {
            Tok::Word { name, param, .. } if name == "u" => {
                let v = param.unwrap_or(0);
                units.push(if v < 0 { (v + 65536) as u16 } else { v as u16 });
                skip = uc;
                continue;
            }
            Tok::Word { name, param, .. } if name == "uc" => {
                uc = param.unwrap_or(1).max(0) as usize;
                continue;
            }
            _ => {}
        }
        match t {
            Tok::Text(a, b) => {
                for &byte in &data[*a..*b] {
                    if byte == b'\r' || byte == b'\n' {
                        continue;
                    }
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    flush(&mut units, &mut out);
                    out.push(cp1252(byte));
                }
            }
            Tok::Sym { ch, hex } => {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                flush(&mut units, &mut out);
                match (ch, hex) {
                    (_, Some(h)) => out.push(cp1252(*h)),
                    (b'\\' | b'{' | b'}', _) => out.push(*ch as char),
                    (b'~', _) => out.push(' '),
                    (b'_', _) => out.push('-'),
                    _ => {}
                }
            }
            Tok::Word { name, .. } => {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                flush(&mut units, &mut out);
                match name.as_str() {
                    "tab" => out.push('\t'),
                    "par" | "line" => out.push('\n'),
                    _ => {}
                }
            }
            Tok::Open(_) | Tok::Close(_) => skip = 0,
        }
    }
    flush(&mut units, &mut out);
    out.trim().to_string()
}

/// RTF text: 7-bit, escapes for `\ { }`, `\uN?` for everything else.
fn encode(s: &str) -> String {
    let mut out = String::new();
    if !s.is_ascii() {
        out.push_str("\\uc1 ");
    }
    for c in s.chars() {
        match c {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\tab "),
            '\n' => out.push_str("\\line "),
            '\r' => {}
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{}?", *unit as i16));
                }
            }
        }
    }
    out
}

/// A name that is already neutral (written by a clean, or Word's default).
fn neutral(name: &str) -> bool {
    let n = name.trim();
    n.is_empty() || n.eq_ignore_ascii_case("unknown") || n == NEUTRAL
}

const NEUTRAL: &str = "Author";

// ---------------------------------------------------------------------------
// What the info group holds

struct Spec {
    label: &'static str,
    category: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
}

fn group_spec(name: &str) -> Option<Spec> {
    let s = |label, category, flag, edit| {
        Some(Spec {
            label,
            category,
            flag,
            edit,
        })
    };
    use Category::*;
    match name {
        "title" => s("Title", Description, None, Some(EditKey::Title)),
        "subject" => s("Subject", Description, None, Some(EditKey::Subject)),
        "author" => s("Author", People, Some(Flag::Names), Some(EditKey::Author)),
        "manager" => s("Manager", People, Some(Flag::Names), None),
        "company" => s("Company", People, Some(Flag::Names), Some(EditKey::Company)),
        "operator" => s("Last saved by", People, Some(Flag::Names), None),
        "category" => s("Category", Description, None, None),
        "keywords" => s("Keywords", Description, None, Some(EditKey::Keywords)),
        "comment" => s("Comments", Description, Some(Flag::Comments), None),
        "doccomm" => s(
            "Description",
            Description,
            Some(Flag::Comments),
            Some(EditKey::Description),
        ),
        "hlinkbase" => s("Hyperlink base", Hidden, None, None),
        "creatim" => s("Created", Dates, None, Some(EditKey::DateTaken)),
        "revtim" => s("Modified", Dates, None, None),
        "printim" => s("Last printed", Dates, None, None),
        "buptim" => s("Last backed up", Dates, None, None),
        _ => None,
    }
}

/// Control words written straight into the info group.
fn word_spec(name: &str) -> Option<(&'static str, Category, bool)> {
    // (label, category, removable)
    Some(match name {
        "version" => ("Revision number", Category::Hidden, true),
        "edmins" => ("Total editing time", Category::Hidden, true),
        "vern" => ("Program version", Category::Software, true),
        "id" => ("Document id", Category::Hidden, true),
        "nofpages" => ("Pages", Category::Technical, false),
        "nofwords" => ("Words", Category::Technical, false),
        "nofchars" => ("Characters", Category::Technical, false),
        _ => return None,
    })
}

fn user_path(s: &str) -> bool {
    let l = s.to_ascii_lowercase().replace('/', "\\");
    l.contains("\\users\\") || l.contains("\\documents and settings\\")
}

const EDITABLE: [EditKey; 7] = [
    EditKey::Title,
    EditKey::Author,
    EditKey::Subject,
    EditKey::Keywords,
    EditKey::Company,
    EditKey::Description,
    EditKey::DateTaken,
];

// ---------------------------------------------------------------------------
// Reading

fn open(path: &Path) -> Result<Doc> {
    let data = std::fs::read(path).map_err(|e| format!("Could not open the file: {e}"))?;
    Doc::parse(data)
}

pub fn read(path: &Path) -> Result<Extract> {
    Ok(extract(&open(path)?))
}

fn extract(doc: &Doc) -> Extract {
    let mut ex = Extract {
        editable: EDITABLE.to_vec(),
        ..Extract::default()
    };
    if let Some(info) = doc.info() {
        for (_, g) in doc.children(info) {
            let Some(spec) = group_spec(&g.name) else {
                continue;
            };
            let value = if spec.category == Category::Dates {
                match doc.date(g) {
                    Some(dt) => {
                        if g.name == "creatim" {
                            ex.taken = Some(dt.text());
                        }
                        dt.text()
                    }
                    None => continue,
                }
            } else {
                doc.text(g)
            };
            if value.is_empty() {
                continue;
            }
            let mut f = Field::new(
                format!("rtf:info:{}", g.name),
                spec.label,
                value,
                spec.category,
                SRC,
            );
            f.flag = spec.flag;
            f.edit = spec.edit;
            if g.name == "hlinkbase" && user_path(&f.value) {
                f.flag = Some(Flag::Names);
            }
            ex.fields.push(f);
        }
        for t in doc.direct_words(&doc.groups[info]) {
            let Tok::Word {
                name,
                param: Some(p),
                ..
            } = t
            else {
                continue;
            };
            let Some((label, category, removable)) = word_spec(name) else {
                continue;
            };
            let value = if name == "edmins" {
                format!("{p} minutes")
            } else {
                p.to_string()
            };
            let mut f = Field::new(format!("rtf:info:{name}"), label, value, category, SRC);
            f.removable = removable;
            if name == "id" {
                f.flag = Some(Flag::Serial);
            }
            ex.fields.push(f);
        }
    }
    for (_, g) in doc.named("generator") {
        let v = doc.text(g).trim_end_matches(';').trim().to_string();
        if !v.is_empty() {
            ex.fields.push(Field::new(
                "rtf:generator",
                "Program used",
                v,
                Category::Software,
                SRC,
            ));
        }
    }
    for (gi, _) in doc.named("userprops") {
        let mut name = String::new();
        for (_, c) in doc.children(gi) {
            match c.name.as_str() {
                "propname" => name = doc.text(c),
                "staticval" => {
                    let value = doc.text(c);
                    if !value.is_empty() {
                        ex.fields.push(
                            Field::new(
                                format!("rtf:userprops:{name}"),
                                format!("Custom property: {name}"),
                                value,
                                Category::Hidden,
                                "Custom properties",
                            )
                            .flag(Flag::Hidden),
                        );
                    }
                }
                _ => {}
            }
        }
    }
    for (_, g) in doc.named("rsidtbl") {
        let n = doc
            .direct_words(g)
            .iter()
            .filter(|t| matches!(t, Tok::Word { name, .. } if name == "rsid"))
            .count();
        if n > 0 {
            ex.fields.push(
                Field::new(
                    "rtf:rsidtbl",
                    "Editing session ids",
                    format!("{n} ids"),
                    Category::Hidden,
                    SRC,
                )
                .flag(Flag::Hidden),
            );
        }
    }
    let revisers: BTreeSet<String> = doc
        .named("revtbl")
        .flat_map(|(gi, _)| doc.children(gi).map(|(_, c)| doc.text(c)))
        .map(|s| s.trim_end_matches(';').trim().to_string())
        .filter(|s| !neutral(s))
        .collect();
    if !revisers.is_empty() {
        ex.fields.push(
            Field::new(
                "rtf:revtbl",
                "Tracked change authors",
                revisers.into_iter().collect::<Vec<_>>().join("; "),
                Category::People,
                SRC,
            )
            .flag(Flag::Names),
        );
    }
    let changes = doc.count_words(&["revised", "deleted"]);
    if changes > 0 {
        ex.fields.push(
            Field::new(
                "rtf:changes",
                "Tracked changes",
                format!(
                    "{changes} tracked change{}: their author names are removed, the changes \
                     themselves stay; accept or reject them in Word if they should not be shared",
                    if changes == 1 { "" } else { "s" }
                ),
                Category::Hidden,
                SRC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
    }
    for (name, id, label) in [
        ("atnauthor", "rtf:atnauthor", "Comment authors"),
        ("atnid", "rtf:atnid", "Comment initials"),
    ] {
        let names: BTreeSet<String> = doc
            .named(name)
            .map(|(_, g)| doc.text(g))
            .filter(|s| !neutral(s))
            .collect();
        if !names.is_empty() {
            ex.fields.push(
                Field::new(
                    id,
                    label,
                    names.into_iter().collect::<Vec<_>>().join("; "),
                    Category::People,
                    SRC,
                )
                .flag(Flag::Names),
            );
        }
    }
    let comments = doc.named("annotation").count();
    if comments > 0 {
        ex.fields.push(
            Field::new(
                "rtf:annotations",
                "Comments",
                format!(
                    "{comments} comment{}: their author names are removed, the comments themselves \
                     stay; delete them in Word if they should not be shared",
                    if comments == 1 { "" } else { "s" }
                ),
                Category::Description,
                SRC,
            )
            .flag(Flag::Comments)
            .fixed(),
        );
    }
    ex
}

// ---------------------------------------------------------------------------
// Cleaning and editing

/// Replaces byte ranges; ranges inside an earlier (outer) range are ignored.
fn splice(data: &[u8], mut edits: Vec<(usize, usize, Vec<u8>)>) -> Vec<u8> {
    // At one position: insertions first, then the widest replacement.
    edits.sort_by_key(|e| (e.0, e.1 > e.0, std::cmp::Reverse(e.1)));
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0;
    for (start, end, with) in edits {
        if start < pos {
            continue;
        }
        out.extend_from_slice(&data[pos..start]);
        out.extend_from_slice(&with);
        pos = end;
    }
    out.extend_from_slice(&data[pos..]);
    out
}

fn clean_doc(doc: &Doc, opts: &CleanOptions) -> Vec<u8> {
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let drop_group = |g: &Group, edits: &mut Vec<_>| edits.push((g.start, g.end, Vec::new()));
    if let Some(info) = doc.info() {
        let ig = &doc.groups[info];
        if opts.is_everything() {
            drop_group(ig, &mut edits);
        } else {
            for (_, c) in doc.children(info) {
                if group_spec(&c.name).is_some_and(|s| opts.removes(s.category)) {
                    drop_group(c, &mut edits);
                }
            }
            for t in doc.direct_words(ig) {
                if let Tok::Word {
                    start, end, name, ..
                } = t
                {
                    if word_spec(name).is_some_and(|(_, c, r)| r && opts.removes(c)) {
                        edits.push((*start, *end, Vec::new()));
                    }
                }
            }
        }
    }
    let by_name = |name: &str, cat: Category, edits: &mut Vec<_>| {
        if opts.removes(cat) {
            for (_, g) in doc.named(name) {
                drop_group(g, edits);
            }
        }
    };
    by_name("generator", Category::Software, &mut edits);
    by_name("userprops", Category::Hidden, &mut edits);
    by_name("rsidtbl", Category::Hidden, &mut edits);
    if opts.removes(Category::People) {
        // Keep the number of entries: \revauthN points into this table.
        for (gi, _) in doc.named("revtbl") {
            for (_, c) in doc.children(gi) {
                let name = doc.text(c);
                if !neutral(name.trim_end_matches(';')) {
                    edits.push((c.start, c.end, format!("{{{NEUTRAL};}}").into_bytes()));
                }
            }
        }
        for (_, g) in doc.named("atnauthor") {
            edits.push((
                g.start,
                g.end,
                format!("{{\\*\\atnauthor {NEUTRAL}}}").into_bytes(),
            ));
        }
        for (_, g) in doc.named("atnid") {
            edits.push((g.start, g.end, b"{\\*\\atnid }".to_vec()));
        }
    }
    splice(&doc.data, edits)
}

fn write_checked(dst: &Path, bytes: Vec<u8>) -> Result<()> {
    Doc::parse(bytes.clone())
        .map_err(|_| "The changed RTF file did not read back; nothing was saved.".to_string())?;
    std::fs::write(dst, bytes).map_err(|e| format!("Could not write {}: {e}", dst.display()))
}

pub fn clean(src: &Path, dst: &Path, opts: &CleanOptions) -> Result<()> {
    let doc = open(src)?;
    write_checked(dst, clean_doc(&doc, opts))
}

fn info_name(key: EditKey) -> Option<&'static str> {
    Some(match key {
        EditKey::Title => "title",
        EditKey::Author => "author",
        EditKey::Subject => "subject",
        EditKey::Keywords => "keywords",
        EditKey::Company => "company",
        EditKey::Description => "doccomm",
        EditKey::DateTaken => "creatim",
        _ => return None,
    })
}

/// Where a new info group goes: after the header tables, else after the
/// opening control words.
fn info_insert_point(doc: &Doc) -> usize {
    const HEADER: [&str; 10] = [
        "fonttbl",
        "filetbl",
        "colortbl",
        "stylesheet",
        "listtable",
        "listoverridetable",
        "revtbl",
        "rsidtbl",
        "generator",
        "mmathPr",
    ];
    let after_header = doc
        .groups
        .iter()
        .filter(|g| g.depth == 1 && HEADER.contains(&g.name.as_str()))
        .map(|g| g.end)
        .max();
    after_header.unwrap_or_else(|| {
        let root = &doc.groups[0];
        let mut pos = root.start + 1;
        for t in &doc.toks[root.open + 1..] {
            match t {
                Tok::Word { end, .. } => pos = *end,
                _ => break,
            }
        }
        pos
    })
}

fn edit_doc(doc: &Doc, edits: &[Edit]) -> Result<Vec<u8>> {
    let mut new_groups = String::new();
    let mut replaced = BTreeSet::new();
    for e in edits {
        let name = info_name(e.key)
            .ok_or_else(|| format!("\"{}\" cannot be edited in an RTF file.", e.key.key()))?;
        replaced.insert(name);
        let Some(value) = e.value.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        if e.key == EditKey::DateTaken {
            let dt = DateTime::parse(value)
                .ok_or_else(|| format!("\"{value}\" is not a date; use YYYY-MM-DD HH:MM:SS."))?;
            new_groups.push_str(&format!(
                "{{\\creatim\\yr{}\\mo{}\\dy{}\\hr{}\\min{}\\sec{}}}",
                dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
            ));
        } else {
            let star = if name == "company" { "\\*" } else { "" };
            new_groups.push_str(&format!("{{{star}\\{name} {}}}", encode(value)));
        }
    }
    let mut splices = Vec::new();
    match doc.info() {
        Some(info) => {
            for (_, c) in doc.children(info) {
                if replaced.contains(c.name.as_str()) {
                    splices.push((c.start, c.end, Vec::new()));
                }
            }
            // Right after "{\info".
            let at = match doc.toks.get(doc.groups[info].body.saturating_sub(1)) {
                Some(Tok::Word { end, .. }) => *end,
                _ => doc.groups[info].start + 1,
            };
            splices.push((at, at, new_groups.into_bytes()));
        }
        None if !new_groups.is_empty() => {
            let at = info_insert_point(doc);
            splices.push((at, at, format!("{{\\info{new_groups}}}").into_bytes()));
        }
        None => {}
    }
    Ok(splice(&doc.data, splices))
}

pub fn edit(src: &Path, dst: &Path, edits: &[Edit]) -> Result<()> {
    let doc = open(src)?;
    write_checked(dst, edit_doc(&doc, edits)?)
}

/// An RTF file with an info group, a generator, custom properties, an rsid
/// table, a tracked change and a comment by "Test Reviewer".
pub fn sample() -> Vec<u8> {
    concat!(
        r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fswiss Calibri;}}",
        r"{\*\revtbl {Unknown;}{Test Reviewer;}}",
        r"{\*\rsidtbl \rsid1180462\rsid7546113}",
        r"{\*\generator ExampleWriter 2.1;}",
        r"{\info{\title Quarterly Plan}{\subject Budget}{\author Test Author}",
        r"{\operator Test Editor}{\manager Test Manager}{\*\company Example Co}",
        r"{\category Internal}{\keywords plan, budget}{\comment Prepared with care}",
        r"{\doccomm Draft for review}{\hlinkbase C:\\Users\\TestUser\\Documents}",
        r"{\creatim\yr2024\mo3\dy1\hr10\min15}{\revtim\yr2024\mo3\dy2\hr9\min5}",
        r"{\printim\yr2024\mo3\dy3\hr8\min0}",
        r"\version3\edmins42\nofpages1\nofwords12\nofchars60\vern57441}",
        r"{\*\userprops {\propname Client}\proptype30{\staticval Example Client Ltd}}",
        "\r\n",
        r"\pard\plain Hello {\deleted\revauthdel1 old }{\revised\revauth1 new} text.",
        r"{\*\atnid TR}{\*\atnauthor Test Reviewer}\chatn{\*\annotation{\*\atnref 1}",
        r"\pard\plain Check this.}\par",
        "\r\n}"
    )
    .as_bytes()
    .to_vec()
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
                "metaforge-rtf-{name}-{}-{nanos}",
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

    #[test]
    fn reads_sample() {
        let ex = extract(&Doc::parse(sample()).unwrap());
        let v = |id: &str| field(&ex, id).map(|f| f.value.clone()).unwrap_or_default();
        assert_eq!(v("rtf:info:title"), "Quarterly Plan");
        assert_eq!(v("rtf:info:author"), "Test Author");
        assert_eq!(
            field(&ex, "rtf:info:author").unwrap().flag,
            Some(Flag::Names)
        );
        assert_eq!(v("rtf:info:operator"), "Test Editor");
        assert_eq!(
            field(&ex, "rtf:info:operator").unwrap().label,
            "Last saved by"
        );
        assert_eq!(v("rtf:info:company"), "Example Co");
        assert_eq!(v("rtf:info:hlinkbase"), r"C:\Users\TestUser\Documents");
        assert_eq!(
            field(&ex, "rtf:info:hlinkbase").unwrap().flag,
            Some(Flag::Names)
        );
        assert_eq!(v("rtf:info:creatim"), "2024-03-01 10:15:00");
        assert_eq!(ex.taken.as_deref(), Some("2024-03-01 10:15:00"));
        assert_eq!(v("rtf:info:edmins"), "42 minutes");
        assert!(!field(&ex, "rtf:info:nofpages").unwrap().removable);
        assert_eq!(v("rtf:generator"), "ExampleWriter 2.1");
        assert_eq!(v("rtf:userprops:Client"), "Example Client Ltd");
        assert_eq!(v("rtf:rsidtbl"), "2 ids");
        assert_eq!(v("rtf:revtbl"), "Test Reviewer");
        assert_eq!(v("rtf:atnauthor"), "Test Reviewer");
        assert_eq!(v("rtf:atnid"), "TR");
        assert!(v("rtf:changes").starts_with("2 tracked changes"));
        assert!(v("rtf:annotations").starts_with("1 comment"));
    }

    #[test]
    fn decodes_escapes() {
        let doc = Doc::parse(
            br"{\rtf1{\info{\title Caf\'e9 \u1580?\u1575? {\b x}\{y\}}{\subject\uc2 \u1580\'3f\'3fz}}}".to_vec(),
        )
        .unwrap();
        let ex = extract(&doc);
        assert_eq!(field(&ex, "rtf:info:title").unwrap().value, "Café جا x{y}");
        assert_eq!(field(&ex, "rtf:info:subject").unwrap().value, "جz");
        let round = |s: &str| {
            let e = encode(s);
            decode(e.as_bytes(), &tokenize(e.as_bytes()).unwrap())
        };
        assert_eq!(round("ڕاپۆرت {1}\\"), "ڕاپۆرت {1}\\");
        // Characters outside the Basic Multilingual Plane use two \u units.
        assert_eq!(round("a\u{1F600}b"), "a\u{1F600}b");
    }

    #[test]
    fn clean_everything() {
        let t = TempDir::new("all");
        let src = t.file("in.rtf", &sample());
        let dst = t.0.join("out.rtf");
        let opts = CleanOptions::everything();
        clean(&src, &dst, &opts).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), sample());
        let out = std::fs::read(&dst).unwrap();
        let ex = read(&dst).unwrap();
        assert_invariant(&ex, &opts);
        for s in [
            "Test Reviewer",
            "Test Author",
            "TestUser",
            "Example Client",
            "ExampleWriter",
            "rsidtbl",
            "TR}",
        ] {
            assert!(!contains(&out, s), "{s} left");
        }
        // The text, the comment and the tracked change stay.
        for s in [
            "Hello",
            "Check this.",
            r"\deleted\revauthdel1 old",
            r"{Unknown;}{Author;}",
        ] {
            assert!(contains(&out, s), "{s} lost");
        }
        assert!(field(&ex, "rtf:annotations").is_some());
        assert!(field(&ex, "rtf:changes").is_some());
    }

    #[test]
    fn partial_clean_keeps_the_rest() {
        let t = TempDir::new("partial");
        let src = t.file("in.rtf", &sample());
        let dst = t.0.join("out.rtf");
        let opts = CleanOptions::only(&[Category::Location, Category::People]);
        clean(&src, &dst, &opts).unwrap();
        let ex = read(&dst).unwrap();
        assert_invariant(&ex, &opts);
        assert_eq!(
            field(&ex, "rtf:info:title").unwrap().value,
            "Quarterly Plan"
        );
        assert!(field(&ex, "rtf:info:creatim").is_some());
        assert!(field(&ex, "rtf:generator").is_some());
        assert!(field(&ex, "rtf:userprops:Client").is_some());
        assert!(field(&ex, "rtf:info:edmins").is_some());
        assert!(field(&ex, "rtf:info:author").is_none());
        assert!(field(&ex, "rtf:revtbl").is_none());

        let dst2 = t.0.join("out2.rtf");
        let opts = CleanOptions::only(&[Category::Hidden, Category::Software]);
        clean(&src, &dst2, &opts).unwrap();
        let ex = read(&dst2).unwrap();
        assert_invariant(&ex, &opts);
        assert!(field(&ex, "rtf:info:author").is_some());
        assert!(field(&ex, "rtf:info:vern").is_none());
        assert!(field(&ex, "rtf:info:nofpages").is_some());
    }

    #[test]
    fn edit_round_trip() {
        let t = TempDir::new("edit");
        let src = t.file("in.rtf", &sample());
        let dst = t.0.join("out.rtf");
        let title = "ڕاپۆرتی ساڵانە";
        edit(
            &src,
            &dst,
            &[
                Edit::set(EditKey::Title, title),
                Edit::set(EditKey::Author, "Example {Writer}"),
                Edit::set(EditKey::Company, "کۆمپانیا"),
                Edit::set(EditKey::Description, "New description"),
                Edit::set(EditKey::DateTaken, "2025-01-02 03:04:05"),
                Edit::clear(EditKey::Keywords),
            ],
        )
        .unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert!(out.is_ascii());
        let ex = read(&dst).unwrap();
        let v = |id: &str| field(&ex, id).map(|f| f.value.clone());
        assert_eq!(v("rtf:info:title").as_deref(), Some(title));
        assert_eq!(v("rtf:info:author").as_deref(), Some("Example {Writer}"));
        assert_eq!(v("rtf:info:company").as_deref(), Some("کۆمپانیا"));
        assert_eq!(v("rtf:info:doccomm").as_deref(), Some("New description"));
        assert_eq!(ex.taken.as_deref(), Some("2025-01-02 03:04:05"));
        assert_eq!(v("rtf:info:keywords"), None);
        assert_eq!(v("rtf:info:subject").as_deref(), Some("Budget"));
        assert_eq!(
            ex.fields
                .iter()
                .filter(|f| f.id == "rtf:info:title")
                .count(),
            1
        );
        assert!(!contains(&out, "Quarterly Plan") && !contains(&out, "Test Author"));
        assert!(edit(&src, &dst, &[Edit::set(EditKey::Album, "x")]).is_err());
        assert!(edit(&src, &dst, &[Edit::set(EditKey::DateTaken, "later")]).is_err());
    }

    #[test]
    fn edit_adds_info_group_when_missing() {
        let t = TempDir::new("noinfo");
        let src = t.file(
            "in.rtf",
            br"{\rtf1\ansi\deff0{\fonttbl{\f0 Arial;}}\pard Hi\par}",
        );
        let dst = t.0.join("out.rtf");
        assert!(read(&src).unwrap().fields.is_empty());
        edit(&src, &dst, &[Edit::set(EditKey::Title, "Notes")]).unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert!(contains(
            &out,
            r"{\f0 Arial;}}{\info{\title Notes}}\pard Hi"
        ));
        assert_eq!(
            field(&read(&dst).unwrap(), "rtf:info:title").unwrap().value,
            "Notes"
        );
    }

    #[test]
    fn garbage_and_truncated_fail_cleanly() {
        let t = TempDir::new("bad");
        let s = sample();
        for (i, bytes) in [
            b"hello world".to_vec(),
            s[..s.len() / 2].to_vec(),
            b"{\\rtf1}}".to_vec(),
            b"{\\rtf1\\bin99999 x".to_vec(),
            Vec::new(),
        ]
        .into_iter()
        .enumerate()
        {
            let p = t.file(&format!("bad{i}.rtf"), &bytes);
            assert!(read(&p).is_err(), "case {i}");
            assert!(clean(&p, &t.0.join("o.rtf"), &CleanOptions::everything()).is_err());
        }
    }
}
