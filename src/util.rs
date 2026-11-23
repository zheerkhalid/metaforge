//! Dates without a date library: civil calendar arithmetic, local time via
//! the Windows time-zone offset, and the `YYYY-MM-DD HH:MM:SS` text form.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The date of a day number from [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A calendar date and time without a zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTime {
    /// Seconds since 1970 when read as UTC.
    pub fn to_unix(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86400
            + self.hour as i64 * 3600
            + self.minute as i64 * 60
            + self.second as i64
    }

    pub fn from_unix(secs: i64) -> DateTime {
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let (year, month, day) = civil_from_days(days);
        DateTime {
            year,
            month,
            day,
            hour: (rem / 3600) as u32,
            minute: (rem % 3600 / 60) as u32,
            second: (rem % 60) as u32,
        }
    }

    /// `YYYY-MM-DD HH:MM:SS`
    pub fn text(self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Parses the common date spellings found in metadata:
    /// `2024:05:01 10:20:30` (EXIF), `2024-05-01T10:20:30Z` / `+03:00`
    /// (XMP, Office; the offset is ignored), `D:20240501102030+03'00'`
    /// (PDF), `2024-05-01`, `2024`.
    pub fn parse(text: &str) -> Option<DateTime> {
        let t = text.trim().trim_start_matches("D:");
        let digits: Vec<u32> = t
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .flat_map(|s| {
                // PDF and compact ISO forms: 20240501102030
                if s.len() > 4 {
                    let mut parts = vec![s.get(0..4).and_then(|x| x.parse().ok())];
                    let mut i = 4;
                    while i + 2 <= s.len().min(14) {
                        parts.push(s.get(i..i + 2).and_then(|x| x.parse().ok()));
                        i += 2;
                    }
                    parts.into_iter().flatten().collect::<Vec<u32>>()
                } else {
                    s.parse().ok().into_iter().collect()
                }
            })
            .collect();
        let year = *digits.first()? as i64;
        if !(1000..=9999).contains(&year) {
            return None;
        }
        let get = |i: usize, default: u32| digits.get(i).copied().unwrap_or(default);
        let dt = DateTime {
            year,
            month: get(1, 1),
            day: get(2, 1),
            hour: get(3, 0),
            minute: get(4, 0),
            second: get(5, 0),
        };
        let valid = (1..=12).contains(&dt.month)
            && (1..=31).contains(&dt.day)
            && dt.hour < 24
            && dt.minute < 60
            && dt.second < 61;
        valid.then_some(dt)
    }

    /// The EXIF spelling `YYYY:MM:DD HH:MM:SS`.
    pub fn exif(self) -> String {
        format!(
            "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// The ISO / XMP spelling `YYYY-MM-DDTHH:MM:SS`.
    pub fn iso(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Seconds to add to UTC to get local time, right now.
#[cfg(windows)]
pub fn local_offset() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: plain out-parameter call.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    let mut bias = tz.Bias;
    match id {
        1 => bias += tz.StandardBias,
        2 => bias += tz.DaylightBias,
        _ => {}
    }
    -(bias as i64) * 60
}

#[cfg(not(windows))]
pub fn local_offset() -> i64 {
    0
}

pub fn unix_of(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub fn system_time(unix: i64) -> SystemTime {
    if unix >= 0 {
        UNIX_EPOCH + Duration::from_secs(unix as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(unix.unsigned_abs())
    }
}

/// A file time as local `YYYY-MM-DD HH:MM:SS`.
pub fn local_text(t: SystemTime) -> String {
    DateTime::from_unix(unix_of(t) + local_offset()).text()
}

/// A local date and time (as metadata stores it) as a file time.
pub fn from_local(dt: DateTime) -> SystemTime {
    system_time(dt.to_unix() - local_offset())
}

/// Human-readable size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Characters Windows does not allow in file names become `_`.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ' {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned.trim().trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-1000, 0, 1, 365, 10957, 19000, 20000, 50000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        let dt = DateTime::from_unix(1_700_000_000);
        assert_eq!(dt.text(), "2023-11-14 22:13:20");
        assert_eq!(DateTime::from_unix(dt.to_unix()), dt);
    }

    #[test]
    fn parse_spellings() {
        let want = "2024-05-01 10:20:30";
        for s in [
            "2024:05:01 10:20:30",
            "2024-05-01T10:20:30Z",
            "2024-05-01T10:20:30+03:00",
            "D:20240501102030+03'00'",
            "20240501102030",
        ] {
            assert_eq!(
                DateTime::parse(s).map(|d| d.text()).as_deref(),
                Some(want),
                "{s}"
            );
        }
        assert_eq!(
            DateTime::parse("2024").unwrap().text(),
            "2024-01-01 00:00:00"
        );
        assert_eq!(
            DateTime::parse("2024-05-01").unwrap().text(),
            "2024-05-01 00:00:00"
        );
        assert!(DateTime::parse("hello").is_none());
        assert!(DateTime::parse("2024:13:01 00:00:00").is_none());
        assert_eq!(DateTime::parse(want).unwrap().exif(), "2024:05:01 10:20:30");
    }

    #[test]
    fn sizes_and_names() {
        assert_eq!(human_size(10), "10 bytes");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(safe_file_name("a:b/c?.jpg"), "a_b_c_.jpg");
    }
}
