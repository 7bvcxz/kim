//! `design-check`: validates `design/tokens.json` — every color is `#RRGGBB`, every contrast
//! requirement references existing tokens, and every declared pair meets its minimum WCAG 2.x
//! contrast ratio.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct Report {
    pub themes: usize,
    pub colors: usize,
    /// (theme, fg, bg, ratio, min)
    pub pairs: Vec<(String, String, String, f64, f64)>,
    pub problems: Vec<String>,
}

pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// WCAG 2.x relative luminance.
pub fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG 2.x contrast ratio, 1.0 ..= 21.0.
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

pub fn check(root: &Path) -> Result<Report, String> {
    let path = root.join("design").join("tokens.json");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("tokens.json: {e}"))?;
    check_doc(&doc)
}

pub fn check_doc(doc: &Value) -> Result<Report, String> {
    let mut report = Report::default();
    if doc.get("tokens_version").and_then(Value::as_u64) != Some(1) {
        report.problems.push("tokens_version must be 1".into());
    }
    let themes = doc
        .get("themes")
        .and_then(Value::as_object)
        .ok_or("tokens.json: missing `themes` object")?;
    for (theme, body) in themes {
        report.themes += 1;
        let mut colors = BTreeMap::new();
        for (name, value) in body
            .get("color")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{theme}: missing `color` object"))?
        {
            match value.as_str().and_then(parse_hex) {
                Some(rgb) => {
                    colors.insert(name.clone(), rgb);
                }
                None => report
                    .problems
                    .push(format!("{theme}: color `{name}` is not #RRGGBB: {value}")),
            }
        }
        report.colors += colors.len();
        let reqs = body
            .get("contrast")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{theme}: missing `contrast` array"))?;
        for req in reqs {
            let fg = req.get("fg").and_then(Value::as_str).unwrap_or_default();
            let min = req.get("min").and_then(Value::as_f64).unwrap_or(0.0);
            if min < 3.0 {
                report.problems.push(format!(
                    "{theme}: requirement for `{fg}` has min {min} < 3.0"
                ));
            }
            let Some(&fg_rgb) = colors.get(fg) else {
                report
                    .problems
                    .push(format!("{theme}: contrast fg `{fg}` is not a color token"));
                continue;
            };
            for bg in req
                .get("bg")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let bg = bg.as_str().unwrap_or_default();
                let Some(&bg_rgb) = colors.get(bg) else {
                    report
                        .problems
                        .push(format!("{theme}: contrast bg `{bg}` is not a color token"));
                    continue;
                };
                let ratio = contrast(fg_rgb, bg_rgb);
                if ratio + 1e-9 < min {
                    report.problems.push(format!(
                        "{theme}: `{fg}` on `{bg}` is {ratio:.2}:1, needs {min}:1"
                    ));
                }
                report
                    .pairs
                    .push((theme.clone(), fg.to_string(), bg.to_string(), ratio, min));
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn contrast_matches_wcag_reference_values() {
        let white = (255, 255, 255);
        let black = (0, 0, 0);
        assert!((contrast(white, black) - 21.0).abs() < 1e-9);
        assert!((contrast(black, black) - 1.0).abs() < 1e-9);
        // #777777 on white is the classic ~4.48:1 "just fails AA" example.
        let ratio = contrast((0x77, 0x77, 0x77), white);
        assert!((ratio - 4.48).abs() < 0.01, "{ratio}");
    }

    #[test]
    fn hex_parsing_is_strict() {
        assert_eq!(parse_hex("#0E0e12"), Some((0x0e, 0x0e, 0x12)));
        assert_eq!(parse_hex("0E0E12"), None);
        assert_eq!(parse_hex("#0E0E1"), None);
        assert_eq!(parse_hex("#GG0000"), None);
    }

    #[test]
    fn failing_pair_and_unknown_token_are_reported() {
        let doc = json!({
            "tokens_version": 1,
            "themes": {"t": {
                "color": {"bg": "#000000", "dim": "#222222", "bad": "red"},
                "contrast": [
                    {"fg": "dim", "bg": ["bg"], "min": 4.5},
                    {"fg": "missing", "bg": ["bg"], "min": 4.5}
                ]
            }}
        });
        let report = check_doc(&doc).unwrap();
        let all = report.problems.join("\n");
        assert!(all.contains("`dim` on `bg`"), "{all}");
        assert!(all.contains("`missing` is not a color token"), "{all}");
        assert!(all.contains("`bad` is not #RRGGBB"), "{all}");
    }

    #[test]
    fn repository_tokens_meet_declared_contrast() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let report = check(root).expect("tokens readable");
        assert!(report.pairs.len() > 50, "expected the full pair list");
        assert!(
            report.problems.is_empty(),
            "design-check problems:\n{}",
            report.problems.join("\n")
        );
    }
}
