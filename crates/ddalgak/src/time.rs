//! UTC timestamps (RFC 3339) without a date crate, and the local wall-clock time the `type_text`
//! date and time variables use (Step 69, DDALGAK_RULE_MODEL §5.4).

use std::time::{SystemTime, UNIX_EPOCH};

/// A local wall-clock time (what `{date}` and `{time}` type). The platform reads it (`GetLocalTime`
/// on Windows); tests pass fixed values.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl LocalTime {
    /// The UTC time of a Unix time in seconds (the fallback where no local clock is read).
    pub fn utc(secs: u64) -> LocalTime {
        let (y, m, d) = civil(secs);
        let rem = secs % 86_400;
        LocalTime {
            year: i32::try_from(y).unwrap_or(1970),
            month: u8::try_from(m).unwrap_or(1),
            day: u8::try_from(d).unwrap_or(1),
            hour: u8::try_from(rem / 3600).unwrap_or(0),
            minute: u8::try_from(rem / 60 % 60).unwrap_or(0),
            second: u8::try_from(rem % 60).unwrap_or(0),
        }
    }
}

/// The civil date (year, month, day) of a Unix time in seconds (H. Hinnant's algorithm).
fn civil(secs: u64) -> (i64, i64, i64) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time in seconds.
pub fn rfc3339(secs: u64) -> String {
    let rem = secs % 86_400;
    let (y, m, d) = civil(secs);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    rfc3339(now_secs())
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_dates() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::rfc3339(1_793_789_045), "2026-11-04T10:44:05Z");
        let t = super::LocalTime::utc(1_793_789_045);
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (2026, 11, 4, 10, 44, 5)
        );
    }
}
