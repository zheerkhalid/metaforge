//! Small helpers on top of quick-xml for the document formats: collecting
//! elements with their text, and a streaming transform that drops elements,
//! replaces their text or rewrites attributes while copying everything else
//! through unchanged (declarations, namespaces, whitespace, other markup).

use quick_xml::escape::escape;
use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};

/// One element: qualified name (with prefix), attributes (unescaped values),
/// the text of all its descendants, and the qualified names of its ancestors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub text: String,
    pub path: Vec<String>,
}

impl El {
    /// The value of an attribute by qualified name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The value of the first attribute with this local name (any prefix).
    pub fn attr_local(&self, local_name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| local(k) == local_name)
            .map(|(_, v)| v.as_str())
    }

    pub fn local(&self) -> &str {
        local(&self.name)
    }

    /// Whether an ancestor has this local name.
    pub fn inside(&self, local_name: &str) -> bool {
        self.path.iter().any(|p| local(p) == local_name)
    }
}

/// The part of a qualified name after the prefix.
pub fn local(qname: &str) -> &str {
    qname.rsplit(':').next().unwrap_or(qname)
}

fn err(e: impl std::fmt::Display) -> String {
    format!("The file contains damaged XML ({e}).")
}

fn element(start: &BytesStart, path: &[String]) -> Result<El, String> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for a in start.attributes().with_checks(false) {
        let a = a.map_err(err)?;
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let value = a
            .unescape_value()
            .map(|v| v.into_owned())
            .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
        attrs.push((key, value));
    }
    Ok(El {
        name,
        attrs,
        text: String::new(),
        path: path.to_vec(),
    })
}

fn reader(xml: &str) -> Reader<&[u8]> {
    let mut r = Reader::from_str(xml);
    let c = r.config_mut();
    c.trim_text(false);
    c.check_end_names = true;
    r
}

/// Every element for which `want(local name)` is true, in document order,
/// with the text of its descendants. Fails on malformed XML.
pub fn collect(xml: &str, want: impl Fn(&str) -> bool) -> Result<Vec<El>, String> {
    let mut r = reader(xml);
    let mut out: Vec<El> = Vec::new();
    // Open elements: (qualified name, index in `out` when collected).
    let mut stack: Vec<(String, Option<usize>)> = Vec::new();
    loop {
        match r.read_event().map_err(err)? {
            Event::Eof => break,
            Event::Start(s) => {
                let path: Vec<String> = stack.iter().map(|(n, _)| n.clone()).collect();
                let el = element(&s, &path)?;
                let idx = if want(el.local()) {
                    out.push(el.clone());
                    Some(out.len() - 1)
                } else {
                    None
                };
                stack.push((el.name, idx));
            }
            Event::Empty(s) => {
                let path: Vec<String> = stack.iter().map(|(n, _)| n.clone()).collect();
                let el = element(&s, &path)?;
                if want(el.local()) {
                    out.push(el);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Text(t) => {
                let text = t
                    .unescape()
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                for (_, idx) in &stack {
                    if let Some(i) = idx {
                        out[*i].text.push_str(&text);
                    }
                }
            }
            Event::CData(t) => {
                let text = String::from_utf8_lossy(&t).into_owned();
                for (_, idx) in &stack {
                    if let Some(i) = idx {
                        out[*i].text.push_str(&text);
                    }
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(err("unexpected end"));
    }
    Ok(out)
}

/// Whether the text is well-formed XML (as far as quick-xml checks).
pub fn well_formed(xml: &str) -> bool {
    collect(xml, |_| false).is_ok()
}

/// What to do with one element in [`transform`].
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Keep,
    /// Remove the element with everything inside it.
    Drop,
    /// Keep the element, replace its content by this text.
    Text(String),
    /// Set (`Some`) or remove (`None`) attributes, keep the content.
    Attrs(Vec<(String, Option<String>)>),
}

/// Streams `xml`, asking `f` about every element (with its attributes and
/// ancestors; `text` is empty). `append` inserts raw XML at the end of the
/// first element with that qualified name. Everything else is copied as is.
pub fn transform(
    xml: &str,
    mut f: impl FnMut(&El) -> Act,
    append: &[(&str, String)],
) -> Result<String, String> {
    let mut r = reader(xml);
    let mut w = Writer::new(Vec::with_capacity(xml.len() + 256));
    let mut stack: Vec<String> = Vec::new();
    let mut appended = vec![false; append.len()];
    // Depth of an element whose content is being skipped (dropped/replaced).
    let mut skip: Option<usize> = None;
    let write = |w: &mut Writer<Vec<u8>>, e: Event| w.write_event(e).map_err(err);
    loop {
        let ev = r.read_event().map_err(err)?;
        if let Some(depth) = skip {
            match ev {
                Event::Start(_) => stack.push(String::new()),
                Event::End(_) => {
                    stack.pop();
                    if stack.len() == depth {
                        skip = None;
                    }
                }
                Event::Eof => return Err(err("unexpected end")),
                _ => {}
            }
            continue;
        }
        let (s, is_empty) = match ev {
            Event::Eof => break,
            Event::Start(s) => (s, false),
            Event::Empty(s) => (s, true),
            Event::End(e) => {
                let name = stack.pop().unwrap_or_default();
                for i in 0..append.len() {
                    if !appended[i] && append[i].0 == name {
                        appended[i] = true;
                        w.get_mut().extend_from_slice(append[i].1.as_bytes());
                    }
                }
                write(&mut w, Event::End(e))?;
                continue;
            }
            other => {
                write(&mut w, other)?;
                continue;
            }
        };
        let el = element(&s, &stack)?;
        let pending: Vec<usize> = (0..append.len())
            .filter(|i| !appended[*i] && append[*i].0 == el.name)
            .collect();
        match f(&el) {
            Act::Drop => {
                if !is_empty {
                    stack.push(el.name);
                    skip = Some(stack.len() - 1);
                }
            }
            Act::Text(text) => {
                write(&mut w, Event::Start(s.borrow()))?;
                write(&mut w, Event::Text(BytesText::from_escaped(escape(&text))))?;
                write(&mut w, Event::End(s.to_end()))?;
                if !is_empty {
                    stack.push(el.name);
                    skip = Some(stack.len() - 1);
                }
            }
            act => {
                let start = match act {
                    Act::Attrs(changes) => with_attrs(&el, &changes),
                    _ => s.clone().into_owned(),
                };
                if is_empty && !pending.is_empty() {
                    write(&mut w, Event::Start(start.borrow()))?;
                    for i in pending {
                        appended[i] = true;
                        w.get_mut().extend_from_slice(append[i].1.as_bytes());
                    }
                    write(&mut w, Event::End(BytesEnd::new(el.name.clone())))?;
                } else if is_empty {
                    write(&mut w, Event::Empty(start))?;
                } else {
                    write(&mut w, Event::Start(start))?;
                    stack.push(el.name);
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(err("unexpected end"));
    }
    String::from_utf8(w.into_inner()).map_err(err)
}

fn with_attrs(el: &El, changes: &[(String, Option<String>)]) -> BytesStart<'static> {
    let mut start = BytesStart::new(el.name.clone());
    for (k, v) in &el.attrs {
        match changes.iter().find(|(ck, _)| ck == k) {
            Some((_, Some(nv))) => start.push_attribute((k.as_str(), nv.as_str())),
            Some((_, None)) => {}
            None => start.push_attribute((k.as_str(), v.as_str())),
        }
    }
    for (k, v) in changes {
        if let Some(v) = v {
            if !el.attrs.iter().any(|(ek, _)| ek == k) {
                start.push_attribute((k.as_str(), v.as_str()));
            }
        }
    }
    start
}

/// Escapes text for element content or attribute values.
pub fn esc(text: &str) -> String {
    escape(text).into_owned()
}

/// Folder names after which a path holds an account name.
const HOME_MARKERS: [&str; 3] = ["users", "documents and settings", "home"];
/// Account folders that are not a person.
const NOT_PEOPLE: [&str; 5] = ["public", "default", "all users", "user", "default user"];

/// `(start, end)` byte ranges of account names in a local path such as
/// `C:\Users\<name>\...`, `file:///C:/Users/<name>/...` or `/home/<name>/`.
fn user_ranges(path: &str) -> Vec<(usize, usize)> {
    let lower = path.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut out = Vec::new();
    for marker in HOME_MARKERS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(marker) {
            let start = from + pos;
            from = start + marker.len();
            let before_ok = start > 0 && matches!(b[start - 1], b'\\' | b'/');
            let after = start + marker.len();
            if !before_ok || after >= b.len() || !matches!(b[after], b'\\' | b'/') {
                continue;
            }
            let name_start = after + 1;
            let name_end = lower[name_start..]
                .find(['\\', '/'])
                .map(|i| name_start + i)
                .unwrap_or(lower.len());
            let name = &lower[name_start..name_end];
            if !name.is_empty() && !NOT_PEOPLE.contains(&name) {
                out.push((name_start, name_end));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The account (user) name inside a local path, if there is one.
pub fn user_in_path(path: &str) -> Option<String> {
    user_ranges(path)
        .first()
        .map(|&(s, e)| path[s..e].replace("%20", " "))
}

/// The path with every account name replaced by `user`.
pub fn anonymize_path(path: &str) -> String {
    let mut out = path.to_string();
    for (s, e) in user_ranges(path).into_iter().rev() {
        out.replace_range(s..e, "user");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "<?xml version=\"1.0\"?>\n<r:root xmlns:r=\"urn:x\"><r:a k=\"1\">A &amp; B</r:a><r:b/><r:c x=\"&lt;\" y=\"2\"><r:d>keep</r:d></r:c><r:e>tail</r:e></r:root>";

    #[test]
    fn collect_finds_text_and_attrs() {
        let els = collect(DOC, |n| matches!(n, "a" | "c" | "d")).unwrap();
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].text, "A & B");
        assert_eq!(els[0].attr("k"), Some("1"));
        assert_eq!(els[1].attr_local("x"), Some("<"));
        assert_eq!(els[1].text, "keep");
        assert!(els[2].inside("c"));
        assert!(collect("<a><b></a>", |_| true).is_err());
        assert!(!well_formed("<a>"));
    }

    #[test]
    fn transform_drops_replaces_and_appends() {
        let out = transform(
            DOC,
            |el| match el.local() {
                "a" => Act::Text("x \"y\" <z>".into()),
                "b" => Act::Drop,
                "c" => Act::Attrs(vec![("x".into(), None), ("n".into(), Some("v".into()))]),
                "e" => Act::Drop,
                _ => Act::Keep,
            },
            &[("r:root", "<r:new/>".to_string())],
        )
        .unwrap();
        assert!(out.starts_with("<?xml version=\"1.0\"?>\n<r:root xmlns:r=\"urn:x\">"));
        assert!(
            out.contains("<r:a k=\"1\">x \"y\" &lt;z&gt;</r:a>")
                || out.contains("<r:a k=\"1\">x &quot;y&quot; &lt;z&gt;</r:a>")
        );
        assert!(!out.contains("r:b") && !out.contains("tail"));
        assert!(out.contains("<r:c y=\"2\" n=\"v\"><r:d>keep</r:d></r:c>"));
        assert!(out.ends_with("<r:new/></r:root>"));
        assert!(well_formed(&out));
        // Appending into an empty element expands it.
        let out = transform("<m/>", |_| Act::Keep, &[("m", "<k>1</k>".into())]).unwrap();
        assert_eq!(out, "<m><k>1</k></m>");
    }

    #[test]
    fn user_names_in_paths() {
        let p = r"C:\Users\TestUser\AppData\Roaming\Microsoft\Templates\Normal.dotm";
        assert_eq!(user_in_path(p).as_deref(), Some("TestUser"));
        assert_eq!(
            anonymize_path(p),
            r"C:\Users\user\AppData\Roaming\Microsoft\Templates\Normal.dotm"
        );
        assert_eq!(
            user_in_path("file:///C:/Users/Test%20User/Docs/a.xlsx").as_deref(),
            Some("Test User")
        );
        assert_eq!(user_in_path("/home/tester/x").as_deref(), Some("tester"));
        assert_eq!(user_in_path(r"C:\Users\Public\x"), None);
        assert_eq!(user_in_path("Normal.dotm"), None);
        assert_eq!(user_in_path("https://example.com/users"), None);
        assert_eq!(anonymize_path("Normal.dotm"), "Normal.dotm");
    }
}
