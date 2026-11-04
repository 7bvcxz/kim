//! Small helpers shared by the modules.

/// `YYYY-MM-DDTHH:MM:SSZ` (UTC) for timestamps in `--json` output.
pub fn rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Renders rows as aligned columns; the first row is the header, the last column is not padded.
pub fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut s = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i + 1 == cols {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn timestamps_and_tables() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_793_836_800 + 3_723)),
            "2026-11-05T01:02:03Z"
        );
        let t = table(&[
            vec!["A".into(), "BB".into(), "C".into()],
            vec!["long".into(), "x".into(), "end".into()],
        ]);
        assert_eq!(t, "A     BB  C\nlong  x   end\n");
    }
}
