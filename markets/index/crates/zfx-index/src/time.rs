//! UTC calendar math. No time zones, no DST: a reference day is
//! `[d 00:00:00.000Z, d+1 00:00:00.000Z)` and always 86,400,000 ms long.

pub const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A UTC calendar day, stored as days since the epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDate(pub i64);

impl UtcDate {
    pub fn parse(s: &str) -> Option<UtcDate> {
        let b = s.as_bytes();
        if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let y: i64 = s.get(0..4)?.parse().ok()?;
        let m: u32 = s.get(5..7)?.parse().ok()?;
        let d: u32 = s.get(8..10)?.parse().ok()?;
        if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
            return None;
        }
        Some(UtcDate(days_from_civil(y, m, d)))
    }
    /// Accepts the source's `"YYYY-MM-DD 00:00:00"` day labels. Anything other than
    /// midnight is rejected: a day label must name a whole UTC day.
    pub fn parse_day_label(s: &str) -> Option<UtcDate> {
        let d = UtcDate::parse(s)?;
        match s.len() {
            10 => Some(d),
            19 if &s[10..] == " 00:00:00" || &s[10..] == "T00:00:00" => Some(d),
            20 if &s[10..] == "T00:00:00Z" => Some(d),
            _ => None,
        }
    }
    pub fn start_ms(self) -> i64 {
        self.0 * DAY_MS
    }
    pub fn end_ms(self) -> i64 {
        (self.0 + 1) * DAY_MS
    }
    pub fn of_ms(ms: i64) -> UtcDate {
        UtcDate(ms.div_euclid(DAY_MS))
    }
    pub fn succ(self) -> UtcDate {
        UtcDate(self.0 + 1)
    }
    pub fn pred(self) -> UtcDate {
        UtcDate(self.0 - 1)
    }
}

impl std::fmt::Display for UtcDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (y, m, d) = civil_from_days(self.0);
        write!(f, "{y:04}-{m:02}-{d:02}")
    }
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// RFC 3339 UTC with millisecond precision, e.g. `2026-11-10T00:00:00.000Z`.
pub fn fmt_ms(ms: i64) -> String {
    let day = UtcDate::of_ms(ms);
    let rem = ms.rem_euclid(DAY_MS);
    let (h, mi, s, milli) = (
        rem / 3_600_000,
        (rem / 60_000) % 60,
        (rem / 1000) % 60,
        rem % 1000,
    );
    format!("{day}T{h:02}:{mi:02}:{s:02}.{milli:03}Z")
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff]Z` (UTC only).
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let day = UtcDate::parse(s.get(0..10)?)?;
    let b = s.as_bytes();
    if b.len() < 20 || (b[10] != b'T' && b[10] != b' ') || !s.ends_with('Z') {
        return None;
    }
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let frac = &s[19..s.len() - 1];
    let milli = if frac.is_empty() {
        0
    } else {
        let digits = frac.strip_prefix('.')?;
        if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut v: i64 = 0;
        for (i, c) in digits.bytes().take(3).enumerate() {
            v += (c - b'0') as i64 * [100, 10, 1][i];
        }
        v
    };
    Some(day.start_ms() + h * 3_600_000 + mi * 60_000 + sec * 1000 + milli)
}

/// Parses an HTTP-date (`Tue, 10 Nov 2026 22:40:05 GMT`).
pub fn parse_http_date_ms(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || parts[5] != "GMT" {
        return None;
    }
    let d: u32 = parts[1].parse().ok()?;
    let m = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|x| *x == parts[2])? as u32
        + 1;
    let y: i64 = parts[3].parse().ok()?;
    let t: Vec<i64> = parts[4]
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if t.len() != 3 || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * DAY_MS + t[0] * 3_600_000 + t[1] * 60_000 + t[2] * 1000)
}

/// Formats an HTTP-date (`Tue, 10 Nov 2026 22:40:05 GMT`), seconds precision.
pub fn fmt_http_date(ms: i64) -> String {
    let day = UtcDate::of_ms(ms);
    let (y, m, d) = civil_from_days(day.0);
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][day.0.rem_euclid(7) as usize];
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][m as usize - 1];
    let rem = ms.rem_euclid(DAY_MS) / 1000;
    format!("{wd}, {d:02} {mon} {y} {:02}:{:02}:{:02} GMT", rem / 3600, (rem / 60) % 60, rem % 60)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_roundtrip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        for z in -800_000..800_000i64 {
            if z % 997 != 0 {
                continue;
            }
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn utc_day_boundaries() {
        let d = UtcDate::parse("2026-11-09").unwrap();
        assert_eq!(fmt_ms(d.start_ms()), "2026-11-09T00:00:00.000Z");
        assert_eq!(fmt_ms(d.end_ms()), "2026-11-10T00:00:00.000Z");
        // last millisecond of d belongs to d, first of d+1 does not
        assert_eq!(UtcDate::of_ms(d.end_ms() - 1), d);
        assert_eq!(UtcDate::of_ms(d.end_ms()), d.succ());
        assert_eq!(UtcDate::of_ms(d.start_ms()), d);
        assert_eq!(UtcDate::of_ms(-1), UtcDate(-1));
    }

    #[test]
    fn dst_transition_days_are_exactly_24h() {
        // US and EU DST transitions in 2026; the UTC reference day ignores them.
        for s in ["2026-03-08", "2026-03-29", "2026-10-25", "2026-11-01"] {
            let d = UtcDate::parse(s).unwrap();
            assert_eq!(d.end_ms() - d.start_ms(), DAY_MS, "{s}");
            assert_eq!(d.to_string(), s);
        }
    }

    #[test]
    fn leap_days() {
        assert!(UtcDate::parse("2028-02-29").is_some());
        assert!(UtcDate::parse("2026-02-29").is_none());
        assert!(UtcDate::parse("2100-02-29").is_none());
        assert!(UtcDate::parse("2000-02-29").is_some());
        assert!(UtcDate::parse("2026-13-01").is_none());
        assert!(UtcDate::parse("2026-11-31").is_none());
    }

    #[test]
    fn day_labels() {
        assert!(UtcDate::parse_day_label("2026-11-10 00:00:00").is_some());
        assert!(UtcDate::parse_day_label("2026-11-10").is_some());
        assert!(UtcDate::parse_day_label("2026-11-10 05:00:00").is_none());
        assert!(UtcDate::parse_day_label("2026-11-10T00:00:00+05:00").is_none());
    }

    #[test]
    fn rfc3339_and_http_date() {
        let t = parse_rfc3339_ms("2026-09-28T18:44:25.718Z").unwrap();
        assert_eq!(fmt_ms(t), "2026-09-28T18:44:25.718Z");
        assert_eq!(parse_rfc3339_ms("2026-09-28T18:44:25Z").unwrap() % 1000, 0);
        assert!(parse_rfc3339_ms("2026-09-28T18:44:25+01:00").is_none());
        let h = parse_http_date_ms("Tue, 10 Nov 2026 22:40:05 GMT").unwrap();
        assert_eq!(fmt_ms(h), "2026-11-10T22:40:05.000Z");
        assert_eq!(fmt_http_date(h), "Tue, 10 Nov 2026 22:40:05 GMT");
    }
}
