//! Photoshop image resource blocks ("8BIM", JPEG APP13 and TIFF tag 34377)
//! and the IPTC-IIM records inside them (resource 0x0404, also TIFF tag
//! 33723).

use super::imgutil::{self, drops, drops_icc, text_of};
use super::{exif, icc, xmp};
use crate::meta::{Category, CleanOptions, Edit, EditKey, Field, Flag, Result};
use crate::util::DateTime;

const SOURCE: &str = "IPTC";

/// The prefix of a Photoshop block in a JPEG APP13 segment.
pub const APP13_HEADER: &[u8] = b"Photoshop 3.0\0";

/// One image resource.
#[derive(Clone, Debug, PartialEq)]
pub struct Resource {
    pub sig: [u8; 4],
    pub id: u16,
    /// The Pascal-string name as stored (length byte, text, padding).
    pub name: Vec<u8>,
    pub data: Vec<u8>,
}

/// Parses a resource block; stops quietly at the first malformed entry.
pub fn parse_irb(data: &[u8]) -> Vec<Resource> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 12 <= data.len() {
        let sig: [u8; 4] = data[p..p + 4].try_into().unwrap_or_default();
        if !matches!(&sig, b"8BIM" | b"PHUT" | b"AgHg" | b"DCSR" | b"MeSa") {
            break;
        }
        let Some(id) = imgutil::be16(data, p + 4) else {
            break;
        };
        let nlen = data[p + 6] as usize;
        let name_total = (1 + nlen + 1) & !1;
        let q = p + 6 + name_total;
        let Some(size) = imgutil::be32(data, q) else {
            break;
        };
        let start = q + 4;
        let Some(body) = data.get(start..start.saturating_add(size as usize)) else {
            break;
        };
        out.push(Resource {
            sig,
            id,
            name: data[p + 6..q].to_vec(),
            data: body.to_vec(),
        });
        p = start + ((size as usize + 1) & !1);
    }
    out
}

pub fn write_irb(res: &[Resource]) -> Vec<u8> {
    let mut out = Vec::new();
    for r in res {
        out.extend(r.sig);
        out.extend(r.id.to_be_bytes());
        if r.name.is_empty() {
            out.extend([0, 0]);
        } else {
            out.extend(&r.name);
        }
        out.extend((r.data.len() as u32).to_be_bytes());
        out.extend(&r.data);
        if r.data.len() % 2 == 1 {
            out.push(0);
        }
    }
    out
}

/// One IIM dataset: record, dataset number, value.
pub type Dataset = (u8, u8, Vec<u8>);

pub fn parse_iim(data: &[u8]) -> Vec<Dataset> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 5 <= data.len() && data[p] == 0x1C {
        let (rec, ds) = (data[p + 1], data[p + 2]);
        let Some(mut size) = imgutil::be16(data, p + 3).map(|s| s as usize) else {
            break;
        };
        let mut start = p + 5;
        if size & 0x8000 != 0 {
            let n = size & 0x7FFF;
            if n == 0 || n > 4 {
                break;
            }
            let Some(b) = data.get(start..start + n) else {
                break;
            };
            size = b.iter().fold(0usize, |a, &x| (a << 8) | x as usize);
            start += n;
        }
        let Some(v) = data.get(start..start.saturating_add(size)) else {
            break;
        };
        out.push((rec, ds, v.to_vec()));
        p = start + size;
    }
    out
}

pub fn write_iim(sets: &[Dataset]) -> Vec<u8> {
    let mut out = Vec::new();
    for (rec, ds, v) in sets {
        out.extend([0x1C, *rec, *ds]);
        if v.len() < 0x8000 {
            out.extend((v.len() as u16).to_be_bytes());
        } else {
            out.extend([0x80, 0x04]);
            out.extend((v.len() as u32).to_be_bytes());
        }
        out.extend(v);
    }
    out
}

struct Spec {
    label: &'static str,
    cat: Category,
    flag: Option<Flag>,
    edit: Option<EditKey>,
    show: bool,
}

/// How a record-2 dataset is shown; `None` for structural ones.
fn spec(ds: u8) -> Option<Spec> {
    use Category::*;
    use EditKey as K;
    let names = Some(Flag::Names);
    let place = Some(Flag::Location);
    let comments = Some(Flag::Comments);
    let (label, cat, flag, edit, show) = match ds {
        0 => return None,
        5 => ("Title", Description, None, Some(K::Title), true),
        25 => ("Keywords", Description, None, Some(K::Keywords), true),
        40 => ("Special instructions", Description, comments, None, true),
        55 => ("Date taken", Dates, None, Some(K::DateTaken), true),
        60 => ("Time taken", Dates, None, Some(K::DateTaken), false),
        62 => ("Date digitized", Dates, None, None, true),
        63 => ("Time digitized", Dates, None, None, false),
        65 => ("Program used", Software, None, None, true),
        70 => ("Program version", Software, None, None, true),
        80 => ("Photographer", People, names, Some(K::Author), true),
        85 => ("Photographer's job title", People, names, None, true),
        90 => ("City", Location, place, None, true),
        92 => ("Sub-location", Location, place, None, true),
        95 => ("State / province", Location, place, None, true),
        100 => ("Country code", Location, place, None, true),
        101 => ("Country", Location, place, None, true),
        103 => ("Job reference", Hidden, None, None, true),
        105 => ("Headline", Description, comments, None, true),
        110 => ("Credit", People, names, None, true),
        115 => ("Source", People, None, None, true),
        116 => ("Copyright", People, names, Some(K::Copyright), true),
        118 => ("Contact", People, names, None, true),
        120 => ("Caption", Description, comments, Some(K::Description), true),
        122 => ("Caption writer", People, names, None, true),
        _ => ("IPTC data", Description, None, None, true),
    };
    Some(Spec {
        label,
        cat,
        flag,
        edit,
        show,
    })
}

fn is_utf8(sets: &[Dataset]) -> bool {
    sets.iter()
        .any(|(r, d, v)| *r == 1 && *d == 90 && v.as_slice() == b"\x1b%G")
}

fn decode(v: &[u8], utf8: bool) -> String {
    if utf8 {
        String::from_utf8_lossy(v).trim().to_string()
    } else {
        text_of(v).trim().to_string()
    }
}

/// The fields of IPTC-IIM data.
pub fn iim_fields(data: &[u8]) -> Vec<Field> {
    let sets = parse_iim(data);
    let utf8 = is_utf8(&sets);
    let mut out: Vec<Field> = Vec::new();
    let text = |ds: u8| -> Option<String> {
        sets.iter()
            .find(|(r, d, _)| *r == 2 && *d == ds)
            .map(|(_, _, v)| decode(v, utf8))
    };
    for (rec, ds, v) in &sets {
        if *rec != 2 {
            continue;
        }
        let Some(s) = spec(*ds) else { continue };
        if !s.show {
            continue;
        }
        let mut value = decode(v, utf8);
        if *ds == 55 || *ds == 62 {
            value = iim_date(&value, text(*ds + 5).as_deref());
        }
        if value.is_empty() {
            continue;
        }
        let id = format!("iptc:2:{ds}");
        if let Some(f) = out.iter_mut().find(|f| f.id == id) {
            f.value = format!("{}; {value}", f.value);
            continue;
        }
        let label = if s.label == "IPTC data" {
            format!("IPTC data (2:{ds})")
        } else {
            s.label.to_string()
        };
        let mut f = Field::new(id, label, value, s.cat, SOURCE);
        f.flag = s.flag;
        f.edit = s.edit;
        out.push(f);
    }
    out
}

/// `20240501` + `102030+0300` -> `2024-05-01 10:20:30`.
fn iim_date(date: &str, time: Option<&str>) -> String {
    let d: String = date.chars().filter(char::is_ascii_digit).collect();
    if d.len() != 8 {
        return date.to_string();
    }
    let t: String = time
        .unwrap_or("")
        .chars()
        .take(6)
        .filter(char::is_ascii_digit)
        .collect();
    let t = if t.len() == 6 { t } else { "000000".into() };
    match DateTime::parse(&format!("{d}{t}")) {
        Some(dt) => dt.text(),
        None => date.to_string(),
    }
}

/// The IIM data without what `opts` removes; `None` when nothing is left.
pub fn strip_iim(data: &[u8], opts: &CleanOptions) -> Option<Vec<u8>> {
    let mut sets = parse_iim(data);
    if opts.is_everything() {
        sets.clear();
    }
    sets.retain(|(rec, ds, _)| *rec != 2 || spec(*ds).is_none_or(|s| !drops(opts, s.cat)));
    let useful = sets.iter().any(|(r, d, _)| *r == 2 && *d != 0);
    useful.then(|| write_iim(&sets))
}

/// Applies edits to IIM data (an empty input starts a new record).
pub fn edit_iim(data: &[u8], edits: &[Edit]) -> Result<Option<Vec<u8>>> {
    let mut sets = parse_iim(data);
    for ed in edits {
        let v = ed.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        sets.retain(|(r, d, _)| *r != 2 || spec(*d).is_none_or(|s| s.edit != Some(ed.key)));
        let Some(v) = v else { continue };
        let mut add = |ds: u8, text: &str| sets.push((2, ds, text.as_bytes().to_vec()));
        match ed.key {
            EditKey::Title => add(5, v),
            EditKey::Author => add(80, v),
            EditKey::Copyright => add(116, v),
            EditKey::Description => add(120, v),
            EditKey::Keywords => {
                for k in imgutil::keywords(v) {
                    add(25, &k);
                }
            }
            EditKey::DateTaken => {
                let d = DateTime::parse(v)
                    .ok_or_else(|| format!("\"{v}\" is not a date. Use YYYY-MM-DD HH:MM:SS."))?;
                add(55, &format!("{:04}{:02}{:02}", d.year, d.month, d.day));
                add(60, &format!("{:02}{:02}{:02}", d.hour, d.minute, d.second));
            }
            _ => {}
        }
    }
    if !sets.iter().any(|(r, d, _)| *r == 2 && *d != 0) {
        return Ok(None);
    }
    // Record 1 (with the UTF-8 marker) first, then record 2.
    sets.retain(|(r, d, _)| !(*r == 1 && *d == 90));
    sets.insert(0, (1, 90, b"\x1b%G".to_vec()));
    if !sets.iter().any(|(r, d, _)| *r == 2 && *d == 0) {
        let at = sets
            .iter()
            .position(|(r, _, _)| *r >= 2)
            .unwrap_or(sets.len());
        sets.insert(at, (2, 0, vec![0, 4]));
    }
    sets.sort_by_key(|s| s.0);
    Ok(Some(write_iim(&sets)))
}

/// The fields of a Photoshop resource block.
pub fn irb_fields(data: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    for r in parse_irb(data) {
        match r.id {
            0x0404 => out.extend(iim_fields(&r.data)),
            0x0409 | 0x040C => out.push(
                Field::new(
                    "iptc:irb:thumbnail",
                    "Embedded thumbnail (Photoshop)",
                    format!("{} bytes", r.data.len()),
                    Category::Hidden,
                    "Photoshop",
                )
                .flag(Flag::Hidden),
            ),
            0x040F => out.extend(icc::fields(&r.data)),
            0x0422 => out.extend(exif::fields_of(&r.data)),
            0x0424 => out.extend(xmp::fields(&text_of(&r.data), "XMP")),
            _ => {}
        }
    }
    out
}

/// The resource block without what `opts` removes; `None` when empty.
pub fn strip_irb(data: &[u8], opts: &CleanOptions) -> Option<Vec<u8>> {
    let res = parse_irb(data);
    let every = opts.is_everything();
    let mut out = Vec::new();
    let mut iptc_changed = false;
    for mut r in res {
        let keep = match r.id {
            0x0404 => match strip_iim(&r.data, opts) {
                Some(d) => {
                    iptc_changed |= d != r.data;
                    r.data = d;
                    true
                }
                None => {
                    iptc_changed = true;
                    false
                }
            },
            0x0425 => true,
            0x0409 | 0x040C => !drops(opts, Category::Hidden),
            0x040F => !drops_icc(opts),
            0x0422 => match exif::strip_block(&r.data, opts) {
                Some(d) => {
                    r.data = d;
                    true
                }
                None => false,
            },
            0x0424 => match xmp::strip(&text_of(&r.data), opts) {
                Some(x) => {
                    r.data = x.into_bytes();
                    true
                }
                None => false,
            },
            0x03ED => true,
            _ => !every,
        };
        if keep {
            out.push(r);
        }
    }
    if iptc_changed || !out.iter().any(|r| r.id == 0x0404) {
        // The digest of the old IPTC data no longer matches.
        out.retain(|r| r.id != 0x0425);
    }
    let useful = out.iter().any(|r| r.id != 0x03ED);
    useful.then(|| write_irb(&out))
}

/// Applies edits to the IPTC record of a resource block. A block without
/// an IPTC record is returned unchanged.
pub fn edit_irb(data: &[u8], edits: &[Edit]) -> Result<Option<Vec<u8>>> {
    let mut res = parse_irb(data);
    let Some(i) = res.iter().position(|r| r.id == 0x0404) else {
        return Ok(Some(data.to_vec()));
    };
    match edit_iim(&res[i].data, edits)? {
        Some(d) => res[i].data = d,
        None => {
            res.remove(i);
        }
    }
    res.retain(|r| r.id != 0x0425);
    Ok((!res.is_empty()).then(|| write_irb(&res)))
}

/// The invented IPTC record of the samples.
pub fn sample_iim() -> Vec<u8> {
    use super::imgutil::sample_values as s;
    let t = |ds: u8, v: &str| (2u8, ds, v.as_bytes().to_vec());
    write_iim(&[
        (1, 90, b"\x1b%G".to_vec()),
        (2, 0, vec![0, 4]),
        t(5, "Sample"),
        t(25, "sample"),
        t(25, "test"),
        t(55, "20240501"),
        t(60, "102030+0300"),
        t(80, s::ARTIST),
        t(90, s::CITY),
        t(101, "Iraq"),
        t(116, s::COPYRIGHT),
        t(120, s::DESCRIPTION),
    ])
}

/// A resource block with the sample IPTC record and a small thumbnail.
pub fn sample_irb() -> Vec<u8> {
    let res = |id: u16, data: Vec<u8>| Resource {
        sig: *b"8BIM",
        id,
        name: vec![0, 0],
        data,
    };
    write_irb(&[
        res(
            0x03ED,
            vec![0, 72, 0, 0, 0, 1, 0, 1, 0, 72, 0, 0, 0, 1, 0, 1],
        ),
        res(0x0404, sample_iim()),
        res(0x040C, vec![0x11; 41]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::imgutil::sample_values as s;

    fn get<'a>(f: &'a [Field], id: &str) -> Option<&'a Field> {
        f.iter().find(|x| x.id == id)
    }

    #[test]
    fn reads_datasets() {
        let f = irb_fields(&sample_irb());
        assert_eq!(get(&f, "iptc:2:80").unwrap().value, s::ARTIST);
        assert_eq!(get(&f, "iptc:2:80").unwrap().flag, Some(Flag::Names));
        assert_eq!(get(&f, "iptc:2:90").unwrap().flag, Some(Flag::Location));
        assert_eq!(get(&f, "iptc:2:25").unwrap().value, "sample; test");
        assert_eq!(get(&f, "iptc:2:55").unwrap().value, s::TAKEN);
        assert!(get(&f, "iptc:irb:thumbnail").is_some());
        assert!(get(&f, "iptc:2:60").is_none());
    }

    #[test]
    fn strips_and_edits() {
        let irb = sample_irb();
        let partial = strip_irb(
            &irb,
            &CleanOptions::only(&[Category::Location, Category::People]),
        )
        .unwrap();
        let f = irb_fields(&partial);
        assert!(get(&f, "iptc:2:80").is_none() && get(&f, "iptc:2:90").is_none());
        assert!(get(&f, "iptc:2:55").is_some() && get(&f, "iptc:irb:thumbnail").is_some());
        assert!(strip_irb(&irb, &CleanOptions::everything()).is_none());

        let e = edit_irb(
            &irb,
            &[
                Edit::set(EditKey::Author, s::TITLE_KU),
                Edit::clear(EditKey::Keywords),
            ],
        )
        .unwrap()
        .unwrap();
        let f = irb_fields(&e);
        assert_eq!(get(&f, "iptc:2:80").unwrap().value, s::TITLE_KU);
        assert!(get(&f, "iptc:2:25").is_none());
        let e = edit_irb(&e, &[Edit::set(EditKey::DateTaken, "2001-02-03 04:05:06")])
            .unwrap()
            .unwrap();
        assert_eq!(
            get(&irb_fields(&e), "iptc:2:55").unwrap().value,
            "2001-02-03 04:05:06"
        );
    }

    #[test]
    fn truncated_blocks() {
        let irb = sample_irb();
        for cut in 0..irb.len() {
            let _ = irb_fields(&irb[..cut]);
            let _ = strip_irb(&irb[..cut], &CleanOptions::everything());
        }
    }
}
