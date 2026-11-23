//! ICC colour profiles: the header (device, creator, date) and the
//! description and copyright tags. Profiles are shown as file details
//! (Technical); the "keep colour profile" option decides their removal.

use super::imgutil::be32;
use crate::meta::{Category, Field};

const SOURCE: &str = "ICC profile";

fn sig_text(b: &[u8]) -> Option<String> {
    let t: String = b
        .iter()
        .map(|&c| c as char)
        .collect::<String>()
        .trim()
        .to_string();
    (!t.is_empty()
        && b.iter().any(|&c| c != 0)
        && t.chars().all(|c| c.is_ascii_graphic() || c == ' '))
    .then_some(t)
}

/// Text of a 'desc', 'text' or 'mluc' tag.
fn tag_text(p: &[u8], off: usize, size: usize) -> Option<String> {
    let t = p.get(off..off.checked_add(size)?)?;
    let kind = t.get(0..4)?;
    let clean = |s: String| {
        let s = s
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        (!s.is_empty()).then_some(s)
    };
    match kind {
        b"desc" => {
            let n = be32(t, 8)? as usize;
            let a = t.get(12..12usize.checked_add(n)?)?;
            clean(String::from_utf8_lossy(a).into_owned())
        }
        b"text" => clean(String::from_utf8_lossy(t.get(8..)?).into_owned()),
        b"mluc" => {
            let n = be32(t, 8)? as usize;
            let rec = be32(t, 12)? as usize;
            if n == 0 || rec < 12 {
                return None;
            }
            // Prefer English, else the first record.
            let pick = (0..n.min(64))
                .find(|i| t.get(16 + i * rec..16 + i * rec + 2) == Some(b"en"))
                .unwrap_or(0);
            let r = 16 + pick * rec;
            let len = be32(t, r + 4)? as usize;
            let at = be32(t, r + 8)? as usize;
            let u = t.get(at..at.checked_add(len)?)?;
            let units: Vec<u16> = u
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            clean(String::from_utf16_lossy(&units))
        }
        _ => None,
    }
}

/// Finds a tag in the tag table: (offset, size).
fn find_tag(p: &[u8], sig: &[u8; 4]) -> Option<(usize, usize)> {
    let n = be32(p, 128)? as usize;
    (0..n.min(200)).find_map(|i| {
        let e = 132 + i * 12;
        (p.get(e..e + 4)? == sig).then_some(())?;
        Some((be32(p, e + 4)? as usize, be32(p, e + 8)? as usize))
    })
}

/// The profile's description ("sRGB IEC61966-2.1").
pub fn description(p: &[u8]) -> Option<String> {
    let (o, s) = find_tag(p, b"desc")?;
    tag_text(p, o, s)
}

/// The fields of a profile (all Technical).
pub fn fields(p: &[u8]) -> Vec<Field> {
    let mut out = Vec::new();
    if p.len() < 132 || p.get(36..40) != Some(b"acsp") {
        if !p.is_empty() {
            out.push(Field::new(
                "icc:desc",
                "Colour profile",
                format!("Embedded profile, {} bytes", p.len()),
                Category::Technical,
                SOURCE,
            ));
        }
        return out;
    }
    let f =
        |id: &str, label: &str, v: String| Field::new(id, label, v, Category::Technical, SOURCE);
    let desc = description(p).unwrap_or_else(|| format!("Embedded profile, {} bytes", p.len()));
    out.push(f("icc:desc", "Colour profile", desc));
    if let Some(c) = find_tag(p, b"cprt").and_then(|(o, s)| tag_text(p, o, s)) {
        out.push(f("icc:cprt", "Colour profile copyright", c));
    }
    let d: Vec<u16> = (0..6)
        .filter_map(|i| super::imgutil::be16(p, 24 + i * 2))
        .collect();
    if d.len() == 6 && d[0] >= 1900 && (1..=12).contains(&d[1]) {
        out.push(f(
            "icc:date",
            "Colour profile date",
            format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                d[0], d[1], d[2], d[3], d[4], d[5]
            ),
        ));
    }
    let device: Vec<String> = [&p[48..52], &p[52..56]]
        .iter()
        .filter_map(|b| sig_text(b))
        .collect();
    if !device.is_empty() {
        out.push(f("icc:device", "Colour profile device", device.join(" ")));
    }
    if let Some(c) = sig_text(&p[80..84]) {
        out.push(f("icc:creator", "Colour profile maker", c));
    }
    out
}

/// A small valid v2 RGB display profile with invented header values.
pub fn sample() -> Vec<u8> {
    fn pad4(v: &mut Vec<u8>) {
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
    }
    let name = b"Test RGB profile";
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend((name.len() as u32 + 1).to_be_bytes());
    desc.extend(name);
    desc.push(0);
    desc.extend([0; 8]); // Unicode language code and count
    desc.extend([0; 3]); // ScriptCode code and count
    desc.extend([0; 67]);
    pad4(&mut desc);
    let mut cprt = b"text\0\0\0\0No copyright, test data\0".to_vec();
    pad4(&mut cprt);
    let mut wtpt = b"XYZ \0\0\0\0".to_vec();
    for v in [0.9642f64, 1.0, 0.8249] {
        wtpt.extend(((v * 65536.0).round() as i32).to_be_bytes());
    }
    let tags: [(&[u8; 4], &Vec<u8>); 3] = [(b"desc", &desc), (b"cprt", &cprt), (b"wtpt", &wtpt)];
    let mut off = 128 + 4 + 12 * tags.len();
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut body: Vec<u8> = Vec::new();
    for (sig, data) in tags {
        table.extend(sig);
        table.extend((off as u32).to_be_bytes());
        table.extend((data.len() as u32).to_be_bytes());
        body.extend(data.iter());
        off += data.len();
    }
    let total = 128 + table.len() + body.len();
    let mut h = Vec::with_capacity(total);
    h.extend((total as u32).to_be_bytes());
    h.extend(b"MFTS");
    h.extend([2, 0x10, 0, 0]);
    h.extend(b"mntrRGB XYZ ");
    for v in [2024u16, 5, 1, 10, 20, 30] {
        h.extend(v.to_be_bytes());
    }
    h.extend(b"acspMSFT");
    h.extend([0; 4]); // flags
    h.extend(b"EXMPEC10");
    h.extend([0; 8]); // attributes
    h.extend([0; 4]); // intent
    for v in [0.9642f64, 1.0, 0.8249] {
        h.extend(((v * 65536.0).round() as i32).to_be_bytes());
    }
    h.extend(b"MFTS");
    h.resize(128, 0);
    h.extend(table);
    h.extend(body);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sample_profile() {
        let p = sample();
        let f = fields(&p);
        let get = |id: &str| f.iter().find(|x| x.id == id).map(|x| x.value.clone());
        assert_eq!(get("icc:desc").as_deref(), Some("Test RGB profile"));
        assert_eq!(get("icc:cprt").as_deref(), Some("No copyright, test data"));
        assert_eq!(get("icc:date").as_deref(), Some("2024-05-01 10:20:30"));
        assert_eq!(get("icc:device").as_deref(), Some("EXMP EC10"));
        assert!(f
            .iter()
            .all(|x| x.category == Category::Technical && x.removable));
        for cut in 0..p.len() {
            let _ = fields(&p[..cut]);
        }
    }

    #[test]
    fn reads_v4_mluc() {
        let mut p = sample();
        // Replace the desc tag with an mluc tag at the end.
        let text: Vec<u8> = "sRGB IEC61966-2.1"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        let mut m = b"mluc\0\0\0\0".to_vec();
        m.extend(1u32.to_be_bytes());
        m.extend(12u32.to_be_bytes());
        m.extend(b"enUS");
        m.extend((text.len() as u32).to_be_bytes());
        m.extend(28u32.to_be_bytes());
        m.extend(&text);
        let at = p.len() as u32;
        p.extend(&m);
        p[136..140].copy_from_slice(&at.to_be_bytes());
        p[140..144].copy_from_slice(&(m.len() as u32).to_be_bytes());
        assert_eq!(description(&p).as_deref(), Some("sRGB IEC61966-2.1"));
    }
}
