//! XMP packets (RDF/XML), shared by the image formats, PDF, MP4 and others.
//!
//! The packet is parsed into a small element tree that keeps every element
//! and attribute as written, so a cleaned or edited packet keeps whatever
//! it did not need to touch. Properties are found in every
//! `rdf:Description`, in attribute form and element form (plain text,
//! `rdf:Seq` / `rdf:Bag` / `rdf:Alt` lists, and structures).

use super::imgutil::{self, nice_date};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Field, Flag, GeoPoint, Result};
use crate::util::DateTime;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// Well-known namespaces and the prefix used for ids and labels.
const NAMESPACES: &[(&str, &str)] = &[
    ("http://purl.org/dc/elements/1.1/", "dc"),
    ("http://ns.adobe.com/xap/1.0/", "xmp"),
    ("http://ns.adobe.com/xap/1.0/mm/", "xmpMM"),
    ("http://ns.adobe.com/xap/1.0/rights/", "xmpRights"),
    ("http://ns.adobe.com/photoshop/1.0/", "photoshop"),
    (
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/",
        "Iptc4xmpCore",
    ),
    ("http://iptc.org/std/Iptc4xmpExt/2008-02-29/", "Iptc4xmpExt"),
    ("http://ns.adobe.com/exif/1.0/", "exif"),
    ("http://cipa.jp/exif/1.0/", "exifEX"),
    ("http://ns.adobe.com/tiff/1.0/", "tiff"),
    ("http://ns.adobe.com/exif/1.0/aux/", "aux"),
    ("http://ns.adobe.com/pdf/1.3/", "pdf"),
    ("http://ns.adobe.com/camera-raw-settings/1.0/", "crs"),
    ("http://ns.adobe.com/lightroom/1.0/", "lr"),
    ("http://ns.microsoft.com/photo/1.0/", "MicrosoftPhoto"),
    ("http://ns.microsoft.com/photo/1.0", "MicrosoftPhoto"),
    ("http://ns.adobe.com/xmp/note/", "xmpNote"),
    ("http://ns.adobe.com/xmp/1.0/DynamicMedia/", "xmpDM"),
    ("http://ns.google.com/photos/1.0/camera/", "GCamera"),
    ("http://ns.google.com/photos/1.0/container/", "GContainer"),
    ("http://ns.google.com/photos/1.0/image/", "GImage"),
    ("http://ns.google.com/photos/1.0/depthmap/", "GDepth"),
    ("http://ns.useplus.org/ldf/xmp/1.0/", "plus"),
    ("http://ns.adobe.com/xap/1.0/g/img/", "xmpGImg"),
    (
        "http://ns.adobe.com/photoshop/1.0/camera-profile",
        "stCamera",
    ),
    ("http://ns.adobe.com/xap/1.0/bj/", "xmpBJ"),
    ("http://ns.adobe.com/xap/1.0/t/pg/", "xmpTPg"),
    ("http://ns.adobe.com/pdfx/1.3/", "pdfx"),
    ("http://ns.adobe.com/xmp/1.0/Script/", "xmpScript"),
    ("http://www.digikam.org/ns/1.0/", "digiKam"),
    ("http://purl.org/dc/terms/", "dcterms"),
];

fn ns_uri(prefix: &str) -> &'static str {
    NAMESPACES
        .iter()
        .find(|n| n.1 == prefix)
        .map(|n| n.0)
        .unwrap_or("")
}

// ----- a minimal element tree -----------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

fn node_from(e: &BytesStart) -> Node {
    let mut n = Node {
        name: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
        ..Node::default()
    };
    for a in e.attributes().with_checks(false).flatten() {
        let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let v = match a.unescape_value() {
            Ok(v) => v.into_owned(),
            Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
        };
        n.attrs.push((k, v));
    }
    n
}

/// The top-level elements of an XML text (malformed tails are cut off).
fn parse_dom(xml: &str) -> Vec<Node> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    let mut stack = vec![Node::default()];
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if stack.len() > 200 {
                    break;
                }
                stack.push(node_from(&e));
            }
            Ok(Event::Empty(e)) => {
                let n = node_from(&e);
                if let Some(top) = stack.last_mut() {
                    top.children.push(n);
                }
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let n = stack.pop().unwrap_or_default();
                    if let Some(top) = stack.last_mut() {
                        top.children.push(n);
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let s = match t.unescape() {
                    Ok(s) => s.into_owned(),
                    Err(_) => String::from_utf8_lossy(&t).into_owned(),
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Ok(Event::CData(c)) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    while stack.len() > 1 {
        let n = stack.pop().unwrap_or_default();
        if let Some(top) = stack.last_mut() {
            top.children.push(n);
        }
    }
    stack.pop().map(|r| r.children).unwrap_or_default()
}

fn esc(s: &str) -> std::borrow::Cow<'_, str> {
    quick_xml::escape::escape(s)
}

fn write_node(n: &Node, out: &mut String) {
    out.push('<');
    out.push_str(&n.name);
    for (k, v) in &n.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&esc(v));
        out.push('"');
    }
    let text = if n.children.is_empty() || !n.text.trim().is_empty() {
        n.text.as_str()
    } else {
        ""
    };
    if n.children.is_empty() && text.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    out.push_str(&esc(text));
    for c in &n.children {
        write_node(c, out);
    }
    out.push_str("</");
    out.push_str(&n.name);
    out.push('>');
}

/// Wraps serialized RDF in an XMP packet.
fn wrap(nodes: &[Node]) -> String {
    let mut out = String::from("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    for n in nodes {
        write_node(n, &mut out);
    }
    out.push_str("\n<?xpacket end=\"w\"?>");
    out
}

// ----- namespaces -----------------------------------------------------------

type Scope = Vec<(String, String)>;

fn push_scope(scope: &Scope, n: &Node) -> Scope {
    let mut s = scope.clone();
    for (k, v) in &n.attrs {
        if k == "xmlns" {
            s.push((String::new(), v.clone()));
        } else if let Some(p) = k.strip_prefix("xmlns:") {
            s.push((p.to_string(), v.clone()));
        }
    }
    s
}

/// (namespace URI, local name) of a qualified name.
fn resolve(scope: &Scope, qname: &str) -> (String, String) {
    let (p, local) = match qname.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", qname),
    };
    let uri = scope
        .iter()
        .rev()
        .find(|(k, _)| k == p)
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    (uri, local.to_string())
}

fn is_rdf(scope: &Scope, qname: &str, local: &str) -> bool {
    let (u, l) = resolve(scope, qname);
    u == RDF && l == local
}

/// The prefix used for ids: the well-known one for the URI, else the
/// document's own prefix. The flag tells whether the namespace is known.
fn id_prefix(uri: &str, qname: &str) -> (String, bool) {
    match NAMESPACES.iter().find(|n| n.0 == uri) {
        Some(n) => (n.1.to_string(), true),
        None => (
            qname.split_once(':').map(|x| x.0).unwrap_or("").to_string(),
            false,
        ),
    }
}

/// Calls `f` for every `rdf:Description` directly under an `rdf:RDF`.
fn each_description(
    nodes: &mut [Node],
    scope: &Scope,
    in_rdf: bool,
    f: &mut dyn FnMut(&mut Node, &Scope),
) {
    for n in nodes {
        let s = push_scope(scope, n);
        if in_rdf && is_rdf(&s, &n.name, "Description") {
            f(n, &s);
        } else {
            let rdf = is_rdf(&s, &n.name, "RDF");
            each_description(&mut n.children, &s, rdf, f);
        }
    }
}

// ----- properties -----------------------------------------------------------

/// One property of a description: where it is and what it holds.
struct Prop {
    prefix: String,
    known: bool,
    local: String,
    value: String,
    items: usize,
}

fn struct_label(local: &str) -> &str {
    match local {
        "CiEmailWork" => "email",
        "CiTelWork" => "phone",
        "CiUrlWork" => "website",
        "CiAdrExtadr" => "address",
        "CiAdrCity" => "city",
        "CiAdrRegion" => "region",
        "CiAdrPcode" => "postcode",
        "CiAdrCtry" => "country",
        l => l,
    }
}

/// Text of a structure given by attributes and child elements.
fn struct_text(n: &Node, scope: &Scope) -> String {
    let s = push_scope(scope, n);
    let mut parts = Vec::new();
    for (k, v) in &n.attrs {
        if k.starts_with("xmlns") || resolve(&s, k).0 == RDF || k.starts_with("xml:") {
            continue;
        }
        if !v.trim().is_empty() {
            parts.push(format!("{}: {}", struct_label(&resolve(&s, k).1), v.trim()));
        }
    }
    for c in &n.children {
        let (_, local) = resolve(&push_scope(&s, c), &c.name);
        let (v, _) = node_value(c, &s);
        if !v.is_empty() {
            parts.push(format!("{}: {v}", struct_label(&local)));
        }
    }
    parts.join("; ")
}

/// The text of a property element and the number of list items.
fn node_value(n: &Node, scope: &Scope) -> (String, usize) {
    let s = push_scope(scope, n);
    if let Some((_, v)) = n.attrs.iter().find(|(k, _)| is_rdf(&s, k, "resource")) {
        return (v.trim().to_string(), 1);
    }
    let parse_type = n.attrs.iter().find(|(k, _)| is_rdf(&s, k, "parseType"));
    if parse_type.is_some_and(|(_, v)| v == "Resource") {
        return (struct_text(n, scope), 1);
    }
    for c in &n.children {
        let cs = push_scope(&s, c);
        let (uri, local) = resolve(&cs, &c.name);
        if uri == RDF && matches!(local.as_str(), "Seq" | "Bag" | "Alt") {
            let items: Vec<(String, bool)> = c
                .children
                .iter()
                .filter(|li| is_rdf(&push_scope(&cs, li), &li.name, "li"))
                .map(|li| {
                    let default = li
                        .attrs
                        .iter()
                        .any(|(k, v)| k == "xml:lang" && v == "x-default");
                    let has_struct = !li.children.is_empty()
                        || li
                            .attrs
                            .iter()
                            .any(|(k, _)| !k.starts_with("xml") && !k.starts_with("rdf:"));
                    let text = if has_struct && li.text.trim().is_empty() {
                        struct_text(li, &cs)
                    } else {
                        li.text.trim().to_string()
                    };
                    (text, default)
                })
                .collect();
            let n_items = items.len();
            if local == "Alt" {
                let pick = items.iter().find(|i| i.1).or(items.first());
                return (pick.map(|i| i.0.clone()).unwrap_or_default(), n_items);
            }
            let texts: Vec<String> = items
                .into_iter()
                .map(|i| i.0)
                .filter(|t| !t.is_empty())
                .collect();
            return (texts.join("; "), n_items);
        }
        if uri == RDF && local == "Description" {
            return (struct_text(c, &s), 1);
        }
    }
    let has_fields = n
        .attrs
        .iter()
        .any(|(k, _)| !k.starts_with("xmlns") && !k.starts_with("xml:") && resolve(&s, k).0 != RDF);
    if has_fields || !n.children.is_empty() {
        return (struct_text(n, scope), 1);
    }
    (n.text.trim().to_string(), 1)
}

/// Is this attribute of a description a property?
fn attr_is_prop(scope: &Scope, k: &str) -> bool {
    !(k.starts_with("xmlns") || k.starts_with("xml:") || resolve(scope, k).0 == RDF)
}

fn props_of(desc: &Node, scope: &Scope) -> Vec<Prop> {
    let mut out = Vec::new();
    for (k, v) in &desc.attrs {
        if attr_is_prop(scope, k) {
            let (uri, local) = resolve(scope, k);
            let (prefix, known) = id_prefix(&uri, k);
            out.push(Prop {
                prefix,
                known,
                local,
                value: v.trim().to_string(),
                items: 1,
            });
        }
    }
    for c in &desc.children {
        let cs = push_scope(scope, c);
        let (uri, local) = resolve(&cs, &c.name);
        let (prefix, known) = id_prefix(&uri, &c.name);
        let (value, items) = node_value(c, scope);
        out.push(Prop {
            prefix,
            known,
            local,
            value,
            items,
        });
    }
    out
}

/// How a property is shown and which cleaning choice removes it.
struct Spec {
    label: String,
    cat: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
    /// Shown as a field (structural properties are not).
    show: bool,
    removable: bool,
    /// Shown folded into one "editing settings" field.
    grouped: bool,
}

fn gps_label(local: &str) -> &'static str {
    match local {
        "GPSLatitude" => "GPS latitude",
        "GPSLongitude" => "GPS longitude",
        "GPSAltitude" => "Altitude",
        "GPSAltitudeRef" => "Altitude reference",
        "GPSTimeStamp" => "GPS time",
        "GPSImgDirection" => "Direction the camera faced",
        "GPSSpeed" => "Speed",
        "GPSTrack" => "Direction of movement",
        "GPSMapDatum" => "Map datum",
        "GPSProcessingMethod" => "GPS method",
        "GPSAreaInformation" => "GPS area",
        "GPSHPositioningError" => "GPS accuracy",
        "GPSDestLatitude" | "GPSDestLongitude" => "Destination position",
        _ => "GPS data",
    }
}

fn classify(prefix: &str, local: &str, known: bool) -> Spec {
    use Category::*;
    use EditKey as K;
    type R = (&'static str, Category, Option<Flag>, Option<EditKey>);
    let names = Some(Flag::Names);
    let comments = Some(Flag::Comments);
    let serial = Some(Flag::Serial);
    let hidden = Some(Flag::Hidden);
    let place = Some(Flag::Location);
    let mut show = true;
    let mut removable = true;
    let mut grouped = false;
    let r: R = match (prefix, local) {
        ("dc", "creator") => ("Author", People, names, Some(K::Author)),
        ("dc", "rights") => ("Copyright", People, names, Some(K::Copyright)),
        ("dc", "title") => ("Title", Description, None, Some(K::Title)),
        ("dc", "description") => ("Description", Description, comments, Some(K::Description)),
        ("dc", "subject") => ("Keywords", Description, None, Some(K::Keywords)),
        ("dc", "contributor") => ("Contributors", People, names, None),
        ("dc", "publisher") => ("Publisher", People, names, None),
        ("dc", "date") => ("Date", Dates, None, None),
        ("dc", "format") | ("pdf", "PDFVersion") | ("pdf", "Trapped") => {
            show = false;
            removable = false;
            ("File format", Technical, None, None)
        }
        ("xmp", "CreatorTool") => ("Program used", Software, None, None),
        ("xmp", "CreateDate") => ("Date created", Dates, None, Some(K::DateTaken)),
        ("xmp", "ModifyDate") => ("Date modified", Dates, None, None),
        ("xmp", "MetadataDate") => ("Metadata date", Dates, None, None),
        ("xmp", "Rating") => ("Rating", Description, None, None),
        ("xmp", "Label") => ("Label", Description, None, None),
        ("xmp", "Thumbnails") => ("Embedded thumbnail", Hidden, hidden, None),
        ("xmp", "Identifier") => ("Identifier", Hidden, serial, None),
        ("photoshop", "City") => ("City", Location, place, None),
        ("photoshop", "State") => ("State / province", Location, place, None),
        ("photoshop", "Country") => ("Country", Location, place, None),
        ("photoshop", "Credit") => ("Credit", People, names, None),
        ("photoshop", "AuthorsPosition") => ("Author's job title", People, names, None),
        ("photoshop", "CaptionWriter") => ("Caption writer", People, names, None),
        ("photoshop", "DateCreated") => ("Date taken", Dates, None, Some(K::DateTaken)),
        ("photoshop", "Headline") => ("Headline", Description, comments, None),
        ("photoshop", "Instructions") => ("Instructions", Description, comments, None),
        ("photoshop", "TransmissionReference") => ("Job reference", Hidden, None, None),
        ("photoshop", "DocumentAncestors") => ("Source document IDs", Hidden, serial, None),
        ("photoshop", "History") => ("Editing history", Hidden, hidden, None),
        ("photoshop", "TextLayers") => ("Text layers", Hidden, hidden, None),
        ("photoshop", "ICCProfile") => ("Colour profile name", Technical, None, None),
        ("photoshop", "ColorMode") | ("photoshop", "LegacyIPTCDigest") => {
            show = false;
            removable = local == "LegacyIPTCDigest";
            ("Photoshop data", Hidden, None, None)
        }
        ("Iptc4xmpCore", "Location") => ("Sub-location", Location, place, None),
        ("Iptc4xmpCore", "CountryCode") => ("Country code", Location, place, None),
        ("Iptc4xmpCore", "CreatorContactInfo") => ("Creator contact details", People, names, None),
        ("Iptc4xmpExt", "LocationCreated") => ("Place taken", Location, place, None),
        ("Iptc4xmpExt", "LocationShown") => ("Place shown", Location, place, None),
        ("Iptc4xmpExt", "PersonInImage") => ("People in the picture", People, names, None),
        ("exif", "GPSLatitude") | ("exif", "GPSLongitude") => {
            (gps_label(local), Location, place, Some(K::Gps))
        }
        ("exif", l) if l.starts_with("GPS") => (gps_label(l), Location, place, None),
        ("exif", "DateTimeOriginal") => ("Date taken", Dates, None, Some(K::DateTaken)),
        ("exif", "DateTimeDigitized") => ("Date digitized", Dates, None, None),
        ("exif", "UserComment") => ("User comment", Description, comments, Some(K::Description)),
        ("exif", "ImageUniqueID") => ("Unique image ID", Hidden, serial, None),
        ("exif", "PixelXDimension") | ("exif", "PixelYDimension") | ("exif", "ColorSpace") => {
            show = false;
            removable = false;
            ("Image size", Technical, None, None)
        }
        ("exif", _) => ("Camera setting", Device, None, None),
        ("exifEX", "BodySerialNumber") | ("aux", "SerialNumber") => {
            ("Camera serial number", Device, serial, None)
        }
        ("exifEX", "LensSerialNumber") | ("aux", "LensSerialNumber") => {
            ("Lens serial number", Device, serial, None)
        }
        ("exifEX", "CameraOwnerName") | ("aux", "OwnerName") => {
            ("Camera owner", People, names, None)
        }
        ("exifEX", "LensModel") | ("aux", "Lens") => ("Lens model", Device, None, None),
        ("exifEX", "LensMake") => ("Lens make", Device, None, None),
        ("exifEX", "Photographer") => ("Photographer", People, names, Some(K::Author)),
        ("exifEX", _) | ("aux", _) => ("Camera detail", Device, None, None),
        ("tiff", "Make") => ("Camera make", Device, None, None),
        ("tiff", "Model") => ("Camera model", Device, None, None),
        ("tiff", "Artist") => ("Photographer", People, names, Some(K::Author)),
        ("tiff", "Copyright") => ("Copyright", People, names, Some(K::Copyright)),
        ("tiff", "ImageDescription") => {
            ("Description", Description, comments, Some(K::Description))
        }
        ("tiff", "Software") => ("Program used", Software, None, None),
        ("tiff", "DateTime") => ("Date modified", Dates, None, None),
        ("tiff", "Orientation") => ("Orientation", Technical, None, None),
        ("tiff", _) => {
            show = false;
            removable = false;
            ("Image structure", Technical, None, None)
        }
        ("xmpMM", "DocumentID") => ("Document ID", Hidden, serial, None),
        ("xmpMM", "InstanceID") => ("Instance ID", Hidden, serial, None),
        ("xmpMM", "OriginalDocumentID") => ("Original document ID", Hidden, serial, None),
        ("xmpMM", "History") => ("Editing history", Hidden, hidden, None),
        ("xmpMM", "DerivedFrom") => ("Derived from", Hidden, serial, None),
        ("xmpMM", "Ingredients") | ("xmpMM", "Pantry") | ("xmpMM", "Manifest") => {
            ("Embedded source files", Hidden, hidden, None)
        }
        ("xmpMM", _) => ("Media management data", Hidden, None, None),
        ("pdf", "Producer") => ("PDF producer", Software, None, None),
        ("pdf", "Keywords") => ("Keywords", Description, None, Some(K::Keywords)),
        ("pdf", "Author") => ("Author", People, names, Some(K::Author)),
        ("xmpRights", "Owner") => ("Rights owner", People, names, None),
        ("xmpRights", "Marked") => ("Copyright status", People, None, None),
        ("xmpRights", "WebStatement") => ("Copyright web page", People, None, None),
        ("xmpRights", "UsageTerms") => ("Usage terms", People, None, None),
        ("xmpRights", _) => ("Rights", People, None, None),
        ("lr", "hierarchicalSubject") => {
            ("Keyword hierarchy", Description, None, Some(K::Keywords))
        }
        ("crs", _) | ("lr", _) => {
            grouped = true;
            ("Editing settings", Hidden, hidden, None)
        }
        ("MicrosoftPhoto", "LastKeywordXMP") | ("MicrosoftPhoto", "LastKeywordIPTC") => {
            ("Keywords (Windows)", Description, None, Some(K::Keywords))
        }
        ("MicrosoftPhoto", "DateAcquired") => ("Date acquired", Dates, None, None),
        ("xmpNote", _) => {
            show = false;
            ("Extended XMP", Hidden, None, None)
        }
        ("xmpDM", "artist")
        | ("xmpDM", "composer")
        | ("xmpDM", "engineer")
        | ("xmpDM", "director") => ("Artist", People, names, None),
        ("plus", _) => ("Licensing", People, names, None),
        ("GCamera", _) | ("GContainer", _) | ("GImage", _) | ("GDepth", _) => {
            ("Phone camera data", Hidden, hidden, None)
        }
        ("digiKam", "TagsList") => ("Tags (digiKam)", Description, None, Some(K::Keywords)),
        _ if known => ("", Description, None, None),
        _ => ("", Hidden, None, None),
    };
    let label = if r.0.is_empty() || matches!(r.0, "Camera setting" | "Camera detail") {
        let base = split_camel(local);
        if r.0.is_empty() {
            base
        } else {
            format!("{} ({})", r.0, base.to_lowercase())
        }
    } else {
        r.0.to_string()
    };
    Spec {
        label,
        cat: r.1,
        flag: r.2,
        edit: r.3,
        show,
        removable,
        grouped,
    }
}

/// "LensSerialNumber" -> "Lens serial number".
fn split_camel(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        if i > 0 && c.is_uppercase() && (chars[i - 1].is_lowercase() || next_lower) {
            out.push(' ');
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Parses an XMP GPS coordinate: `36,11.406N`, `36,11,24.36N` or decimal.
pub fn parse_coord(text: &str) -> Option<f64> {
    let t = text.trim();
    let last = t.chars().last()?;
    let (body, neg) = match last.to_ascii_uppercase() {
        'N' | 'E' => (&t[..t.len() - 1], false),
        'S' | 'W' => (&t[..t.len() - 1], true),
        _ => (t, false),
    };
    let parts: Vec<f64> = body
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    let v = match parts[..] {
        [d] => d,
        [d, m] => d + m / 60.0,
        [d, m, s] => d + m / 60.0 + s / 3600.0,
        _ => return None,
    };
    let v = if neg { -v } else { v };
    v.is_finite().then_some(v)
}

fn coord_text(v: f64, pos: char, neg: char) -> String {
    let a = v.abs();
    let d = a.floor();
    let m = (a - d) * 60.0;
    format!("{},{:.6}{}", d as u32, m, if v < 0.0 { neg } else { pos })
}

/// The fields of an XMP packet. `source` is shown as where they are stored.
pub fn fields(xml: &str, source: &str) -> Vec<Field> {
    let mut dom = parse_dom(xml);
    let mut props = Vec::new();
    each_description(&mut dom, &Vec::new(), false, &mut |d, s| {
        props.extend(props_of(d, s))
    });

    let mut out = Vec::new();
    let mut lat = None;
    let mut lon = None;
    let mut grouped = 0usize;
    for p in &props {
        if p.prefix == "exif" && p.local == "GPSLatitude" {
            lat = parse_coord(&p.value);
        }
        if p.prefix == "exif" && p.local == "GPSLongitude" {
            lon = parse_coord(&p.value);
        }
    }
    let point = match (lat, lon) {
        (Some(lat), Some(lon)) => Some(GeoPoint { lat, lon }),
        _ => None,
    };
    if let Some(p) = point {
        out.push(
            Field::new(
                "xmp:exif:gps",
                "GPS position",
                p.display(),
                Category::Location,
                source,
            )
            .flag(Flag::Location)
            .edit(EditKey::Gps),
        );
    }
    for p in props {
        let spec = classify(&p.prefix, &p.local, p.known);
        if !spec.show || p.value.is_empty() {
            continue;
        }
        if spec.grouped {
            grouped += 1;
            continue;
        }
        if point.is_some()
            && p.prefix == "exif"
            && matches!(p.local.as_str(), "GPSLatitude" | "GPSLongitude")
        {
            continue;
        }
        let value = if spec.cat == Category::Dates {
            nice_date(&p.value)
        } else if p.prefix == "xmpMM" && p.local == "History" {
            format!("{} recorded editing steps", p.items)
        } else if p.prefix == "tiff" && p.local == "Orientation" {
            p.value
                .parse()
                .map(super::exif::orientation_text)
                .unwrap_or(p.value.clone())
        } else {
            p.value.clone()
        };
        let mut f = Field::new(
            format!("xmp:{}:{}", p.prefix, p.local),
            spec.label,
            value,
            spec.cat,
            source,
        );
        f.flag = spec.flag;
        f.edit = spec.edit;
        f.removable = spec.removable;
        out.push(f);
    }
    if grouped > 0 {
        out.push(
            Field::new(
                "xmp:crs",
                "Editing settings",
                format!("{grouped} settings from a photo editor"),
                Category::Hidden,
                source,
            )
            .flag(Flag::Hidden),
        );
    }
    out
}

fn removes(spec: &Spec, prefix: &str, local: &str, opts: &CleanOptions) -> bool {
    if !spec.removable {
        return false;
    }
    if spec.cat == Category::Technical {
        let orient = prefix == "tiff" && local == "Orientation";
        return if orient {
            imgutil::drops_orientation(opts)
        } else {
            imgutil::drops_icc(opts)
        };
    }
    opts.removes(spec.cat)
}

/// Keeps the properties of every description for which `keep` says yes;
/// returns how many shown properties are left.
fn retain_props(dom: &mut [Node], keep: &dyn Fn(&Spec, &str, &str) -> bool) -> usize {
    let mut left = 0;
    each_description(dom, &Vec::new(), false, &mut |d, s| {
        let s = s.clone();
        let check = |qname: &str, scope: &Scope| {
            let (uri, local) = resolve(scope, qname);
            let (prefix, known) = id_prefix(&uri, qname);
            let spec = classify(&prefix, &local, known);
            (keep(&spec, &prefix, &local), spec.show)
        };
        let mut count = 0;
        d.attrs.retain(|(k, _)| {
            if !attr_is_prop(&s, k) {
                return true;
            }
            let (k2, shown) = check(k, &s);
            if k2 && shown {
                count += 1;
            }
            k2
        });
        d.children.retain(|c| {
            let cs = push_scope(&s, c);
            let (k2, shown) = check(&c.name, &cs);
            if k2 && shown {
                count += 1;
            }
            k2
        });
        left += count;
    });
    left
}

/// The packet without what `opts` removes; `None` when nothing worth keeping is left.
pub fn strip(xml: &str, opts: &CleanOptions) -> Option<String> {
    let mut dom = parse_dom(xml);
    let left = retain_props(&mut dom, &|spec, p, l| !removes(spec, p, l, opts));
    (left > 0).then(|| wrap(&dom))
}

const SKELETON: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>";

/// A new property element `prefix:local` declaring its own namespace.
fn prop_node(prefix: &str, local: &str) -> Node {
    Node {
        name: format!("{prefix}:{local}"),
        attrs: vec![(format!("xmlns:{prefix}"), ns_uri(prefix).to_string())],
        ..Node::default()
    }
}

fn list_node(rdf: &str, prefix: &str, local: &str, kind: &str, items: &[String]) -> Node {
    let mut p = prop_node(prefix, local);
    let mut list = Node {
        name: format!("{rdf}:{kind}"),
        ..Node::default()
    };
    for it in items {
        let mut li = Node {
            name: format!("{rdf}:li"),
            text: it.clone(),
            ..Node::default()
        };
        if kind == "Alt" {
            li.attrs.push(("xml:lang".into(), "x-default".into()));
        }
        list.children.push(li);
    }
    p.children.push(list);
    p
}

fn text_node(prefix: &str, local: &str, text: String) -> Node {
    let mut p = prop_node(prefix, local);
    p.text = text;
    p
}

/// The packet `xml` (or a new one) with the edits applied: every property
/// the key edits is removed, then the new value is written to dc:, xmp: or
/// exif: properties. Values are UTF-8, so any script is kept as typed.
pub fn apply_edits(xml: Option<&str>, edits: &[Edit]) -> Result<String> {
    let mut dom = xml.map(parse_dom).unwrap_or_default();
    let mut has_desc = false;
    each_description(&mut dom, &Vec::new(), false, &mut |_, _| has_desc = true);
    if !has_desc {
        dom = parse_dom(SKELETON);
    }
    for ed in edits {
        let key = ed.key;
        let value = ed.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        retain_props(&mut dom, &|spec, p, l| {
            let gps = key == EditKey::Gps && p == "exif" && l.starts_with("GPS");
            !(spec.edit == Some(key) || gps)
        });
        let Some(v) = value else { continue };
        let mut new: Vec<Node> = Vec::new();
        let mut done = false;
        each_description(&mut dom, &Vec::new(), false, &mut |d, _| {
            if done {
                return;
            }
            done = true;
            let rdf = d
                .name
                .split_once(':')
                .map(|x| x.0)
                .unwrap_or("rdf")
                .to_string();
            let one = [v.to_string()];
            let node = match key {
                EditKey::Title => list_node(&rdf, "dc", "title", "Alt", &one),
                EditKey::Author => list_node(&rdf, "dc", "creator", "Seq", &one),
                EditKey::Copyright => list_node(&rdf, "dc", "rights", "Alt", &one),
                EditKey::Description => list_node(&rdf, "dc", "description", "Alt", &one),
                EditKey::Keywords => list_node(&rdf, "dc", "subject", "Bag", &imgutil::keywords(v)),
                EditKey::DateTaken => match DateTime::parse(v) {
                    Some(dt) => text_node("xmp", "CreateDate", dt.iso()),
                    None => return,
                },
                EditKey::Gps => match GeoPoint::parse(v) {
                    Some(g) => {
                        new.push(text_node(
                            "exif",
                            "GPSLongitude",
                            coord_text(g.lon, 'E', 'W'),
                        ));
                        text_node("exif", "GPSLatitude", coord_text(g.lat, 'N', 'S'))
                    }
                    None => return,
                },
                _ => return,
            };
            new.push(node);
            d.children.append(&mut new);
        });
        match key {
            EditKey::DateTaken if DateTime::parse(v).is_none() => {
                return Err(format!("\"{v}\" is not a date. Use YYYY-MM-DD HH:MM:SS."));
            }
            EditKey::Gps if GeoPoint::parse(v).is_none() => {
                return Err(format!(
                    "\"{v}\" is not a GPS position. Use latitude, longitude, e.g. 36.1901, 44.0091."
                ));
            }
            _ => {}
        }
    }
    Ok(wrap(&dom))
}

/// Pads a packet with whitespace to exactly `len` bytes (for in-place
/// rewrites); `None` if it does not fit.
pub fn padded(xml: &str, len: usize) -> Option<Vec<u8>> {
    let mut b = xml.as_bytes().to_vec();
    if b.len() > len {
        return None;
    }
    b.resize(len, b' ');
    Some(b)
}

/// The invented XMP packet of the sample files (title in Kurdish).
pub fn sample() -> String {
    use super::imgutil::sample_values as s;
    format!(
        r#"<?xpacket begin="{bom}" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:exif="http://ns.adobe.com/exif/1.0/"
    xmlns:aux="http://ns.adobe.com/exif/1.0/aux/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:CreatorTool="{soft}"
    xmp:CreateDate="2024-05-01T10:20:30"
    xmpMM:DocumentID="xmp.did:0042abcd"
    xmpMM:InstanceID="xmp.iid:0042abcd"
    photoshop:City="{city}"
    exif:GPSLatitude="36,11.406N"
    exif:GPSLongitude="44,0.546E"
    aux:SerialNumber="{serial}"
    crs:Exposure2012="+0.50"
    crs:Contrast2012="+10">
   <dc:title><rdf:Alt><rdf:li xml:lang="x-default">{title}</rdf:li></rdf:Alt></dc:title>
   <dc:creator><rdf:Seq><rdf:li>{artist}</rdf:li></rdf:Seq></dc:creator>
   <dc:rights><rdf:Alt><rdf:li xml:lang="x-default">{copy}</rdf:li></rdf:Alt></dc:rights>
   <dc:description><rdf:Alt><rdf:li xml:lang="x-default">{desc}</rdf:li></rdf:Alt></dc:description>
   <dc:subject><rdf:Bag><rdf:li>sample</rdf:li><rdf:li>test</rdf:li></rdf:Bag></dc:subject>
   <xmpMM:History><rdf:Seq><rdf:li rdf:parseType="Resource"><stEvt:action xmlns:stEvt="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#">saved</stEvt:action></rdf:li></rdf:Seq></xmpMM:History>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#,
        bom = '\u{feff}',
        soft = s::SOFTWARE,
        city = s::CITY,
        serial = s::SERIAL,
        title = s::TITLE_KU,
        artist = s::ARTIST,
        copy = s::COPYRIGHT,
        desc = s::DESCRIPTION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;

    fn get<'a>(f: &'a [Field], id: &str) -> Option<&'a Field> {
        f.iter().find(|x| x.id == id)
    }

    #[test]
    fn reads_attribute_and_element_forms() {
        let f = fields(&sample(), "XMP");
        assert_eq!(get(&f, "xmp:dc:title").unwrap().value, s::TITLE_KU);
        assert_eq!(get(&f, "xmp:dc:creator").unwrap().flag, Some(Flag::Names));
        assert_eq!(get(&f, "xmp:dc:subject").unwrap().value, "sample; test");
        assert_eq!(
            get(&f, "xmp:xmp:CreatorTool").unwrap().category,
            Category::Software
        );
        assert_eq!(get(&f, "xmp:xmp:CreateDate").unwrap().value, s::TAKEN);
        assert_eq!(
            get(&f, "xmp:photoshop:City").unwrap().flag,
            Some(Flag::Location)
        );
        assert_eq!(
            get(&f, "xmp:aux:SerialNumber").unwrap().flag,
            Some(Flag::Serial)
        );
        assert_eq!(
            get(&f, "xmp:xmpMM:DocumentID").unwrap().category,
            Category::Hidden
        );
        assert_eq!(
            get(&f, "xmp:xmpMM:History").unwrap().value,
            "1 recorded editing steps"
        );
        assert_eq!(
            get(&f, "xmp:crs").unwrap().value,
            "2 settings from a photo editor"
        );
        let g = GeoPoint::parse(&get(&f, "xmp:exif:gps").unwrap().value).unwrap();
        assert!((g.lat - s::LAT).abs() < 1e-6 && (g.lon - s::LON).abs() < 1e-6);
        assert!(get(&f, "xmp:exif:GPSLatitude").is_none());
    }

    #[test]
    fn other_prefixes_and_structures() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<r:Description xmlns:d="http://purl.org/dc/elements/1.1/" xmlns:I="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" xmlns:zz="http://example.com/private/">
<d:creator><r:Seq><r:li>A &amp; B</r:li><r:li>C</r:li></r:Seq></d:creator>
<I:CreatorContactInfo I:CiEmailWork="me@example.com" I:CiTelWork="+964 750"/>
<zz:Secret>value</zz:Secret>
</r:Description></r:RDF></x:xmpmeta>"#;
        let f = fields(xml, "XMP");
        assert_eq!(get(&f, "xmp:dc:creator").unwrap().value, "A & B; C");
        let c = get(&f, "xmp:Iptc4xmpCore:CreatorContactInfo").unwrap();
        assert!(c.value.contains("email: me@example.com") && c.flag == Some(Flag::Names));
        assert_eq!(get(&f, "xmp:zz:Secret").unwrap().category, Category::Hidden);
        let only_dc = strip(
            xml,
            &CleanOptions::only(&[Category::Hidden, Category::People]),
        );
        assert!(only_dc.is_none());
    }

    #[test]
    fn strip_partial_keeps_the_rest() {
        let x = strip(
            &sample(),
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let f = fields(&x, "XMP");
        assert!(get(&f, "xmp:exif:gps").is_none() && get(&f, "xmp:photoshop:City").is_none());
        assert!(get(&f, "xmp:dc:creator").is_none() && get(&f, "xmp:dc:rights").is_none());
        assert_eq!(get(&f, "xmp:dc:title").unwrap().value, s::TITLE_KU);
        assert!(get(&f, "xmp:aux:SerialNumber").is_some());
        assert!(strip(&sample(), &CleanOptions::everything()).is_none());
        assert!(strip("not xml at all <<<", &CleanOptions::everything()).is_none());
    }

    #[test]
    fn edits_replace_and_remove() {
        let x = apply_edits(
            Some(&sample()),
            &[
                Edit::set(EditKey::Author, "ئاراس"),
                Edit::set(EditKey::Gps, "-1.5, -70.25"),
                Edit::set(EditKey::Keywords, "a, b"),
                Edit::clear(EditKey::Title),
            ],
        )
        .unwrap();
        let f = fields(&x, "XMP");
        assert_eq!(get(&f, "xmp:dc:creator").unwrap().value, "ئاراس");
        assert_eq!(
            get(&f, "xmp:exif:gps").unwrap().value,
            "-1.500000, -70.250000"
        );
        assert_eq!(get(&f, "xmp:dc:subject").unwrap().value, "a; b");
        assert!(get(&f, "xmp:dc:title").is_none());
        let fresh = apply_edits(
            None,
            &[Edit::set(EditKey::DateTaken, "2020-01-02 03:04:05")],
        )
        .unwrap();
        assert_eq!(
            get(&fields(&fresh, "XMP"), "xmp:xmp:CreateDate")
                .unwrap()
                .value,
            "2020-01-02 03:04:05"
        );
        assert!(apply_edits(None, &[Edit::set(EditKey::Gps, "x")]).is_err());
    }

    #[test]
    fn coords() {
        assert!((parse_coord("36,11.406N").unwrap() - 36.1901).abs() < 1e-9);
        assert!((parse_coord("44,0,32.76W").unwrap() + 44.0091).abs() < 1e-9);
        assert_eq!(parse_coord("12.5"), Some(12.5));
        assert_eq!(parse_coord("abc"), None);
        assert_eq!(split_camel("LensSerialNumber"), "Lens serial number");
    }
}
