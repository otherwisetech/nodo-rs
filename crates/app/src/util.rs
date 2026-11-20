//! Formatting helpers with no UI.

/// `2026-11-19 16:20` at `offset` seconds from UTC.
pub fn datetime(secs: i64, offset: i32) -> String {
    let t = secs + offset as i64;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil(z: i64) -> (i64, u32, u32) {
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

/// How long ago `secs` (since the epoch) was, from `now`: "3 days ago".
pub fn ago(secs: i64, now: i64) -> String {
    let d = (now - secs).max(0);
    match d {
        0..60 => i18n::t!("app.ago.now"),
        60..3600 => i18n::tn!("app.ago.minutes", d / 60),
        3600..86_400 => i18n::tn!("app.ago.hours", d / 3600),
        86_400..2_592_000 => i18n::tn!("app.ago.days", d / 86_400),
        2_592_000..31_536_000 => i18n::tn!("app.ago.months", d / 2_592_000),
        _ => i18n::tn!("app.ago.years", d / 31_536_000),
    }
}

/// This computer's UTC offset in seconds (from `date +%z`, once), for showing times.
pub fn local_offset() -> i32 {
    static OFF: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        if cfg!(windows) {
            return 0;
        }
        let out = std::process::Command::new("date").arg("+%z").output().ok();
        let s = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        parse_offset(&s).unwrap_or(0)
    })
}

/// `-0500` → -18000.
pub fn parse_offset(s: &str) -> Option<i32> {
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let h: i32 = s.get(1..3)?.parse().ok()?;
    let m: i32 = s.get(3..5)?.parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// A time in this computer's zone.
pub fn local_time(secs: i64) -> String {
    datetime(secs, local_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_ago() {
        assert_eq!(datetime(0, 0), "1970-01-01 00:00");
        assert_eq!(datetime(1_795_000_000, -5 * 3600), "2026-11-18 06:06");
        assert_eq!(parse_offset("-0500"), Some(-18000));
        assert_eq!(parse_offset("+0530"), Some(19800));
        assert_eq!(parse_offset("Z"), None);
        i18n::set_locale("en");
        assert_eq!(ago(100, 100 + 3 * 86_400), "3 days ago");
        assert_eq!(ago(100, 130), "just now");
    }
}
