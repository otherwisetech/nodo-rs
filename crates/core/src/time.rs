//! Unix-seconds helpers without a date library.

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `2026-11-20T10:00:00Z`, `2026-11-20T10:00:00.123456-05:00` → unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, u32, u32) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time_end = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, zone) = rest.split_at(time_end);
    let mut t = time.split(':');
    let (hh, mm): (i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    let ss: f64 = t.next().unwrap_or("0").parse().ok()?;
    let offset = match zone.chars().next() {
        Some('+') | Some('-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z = &zone[1..];
            let (zh, zm) = z.split_once(':').unwrap_or((z.get(..2)?, z.get(2..).unwrap_or("0")));
            sign * (zh.parse::<i64>().ok()? * 3600 + zm.parse::<i64>().unwrap_or(0) * 60)
        }
        _ => 0,
    };
    Some(days_from_civil(y, m, day) * 86_400 + hh * 3600 + mm * 60 + ss as i64 - offset)
}

/// Howard Hinnant's days_from_civil.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 → (year, month, day).
pub fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `2026-11-20 10:00` (UTC).
pub fn format_utc(secs: i64) -> String {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let r = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", r / 3600, r % 3600 / 60)
}

/// `2026-11-20T10:00:00Z`.
pub fn format_rfc3339(secs: i64) -> String {
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let r = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", r / 3600, r % 3600 / 60, r % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_rfc3339("2026-11-20T10:00:00.5-05:00"), parse_rfc3339("2026-11-20T15:00:00Z"));
        assert_eq!(parse_rfc3339("2026-11-20T10:00:00+0130"), parse_rfc3339("2026-11-20T08:30:00Z"));
        assert_eq!(parse_rfc3339("nope"), None);
        let t = parse_rfc3339("2026-11-20T10:11:12Z").unwrap();
        assert_eq!(format_rfc3339(t), "2026-11-20T10:11:12Z");
        assert_eq!(format_utc(t), "2026-11-20 10:11");
    }
}
