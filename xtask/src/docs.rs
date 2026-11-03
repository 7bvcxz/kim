//! Documentation consistency checks:
//! - requirement table rows in `docs/PRODUCT_REQUIREMENTS.md` are well-formed and unique;
//! - every requirement ID referenced anywhere in Markdown exists (range endpoints included);
//! - relative links and `<img src>` targets resolve;
//! - `docs/README.md` indexes every document in `docs/` and never marks an existing one planned.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub requirement_ids: usize,
    pub files: usize,
    pub links: usize,
    pub problems: Vec<String>,
}

const SKIP_DIRS: &[&str] = &["target", ".git", "node_modules"];

pub fn check(root: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let req_path = root.join("docs").join("PRODUCT_REQUIREMENTS.md");
    let req_text = read(&req_path)?;
    let defined = parse_requirement_table(&req_text, &mut report.problems);
    report.requirement_ids = defined.len();

    let mut files = Vec::new();
    collect_markdown(root, &mut files)?;
    files.sort();
    report.files = files.len();

    for file in &files {
        let text = read(file)?;
        let rel = file
            .strip_prefix(root)
            .unwrap_or(file)
            .display()
            .to_string();
        for id in referenced_ids(&text) {
            if !defined.contains_key(&id) {
                report
                    .problems
                    .push(format!("{rel}: references undefined requirement {id}"));
            }
        }
        for target in relative_links(&text) {
            report.links += 1;
            let dir = file.parent().unwrap_or(root);
            if !dir.join(&target).exists() {
                report
                    .problems
                    .push(format!("{rel}: broken link `{target}`"));
            }
        }
    }

    check_docs_index(root, &mut report.problems)?;
    Ok(report)
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect_markdown(&path, out)?;
            }
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
    Ok(())
}

/// Returns `id -> row` for every requirement row and records malformed or duplicate rows.
pub fn parse_requirement_table(text: &str, problems: &mut Vec<String>) -> BTreeMap<String, String> {
    let mut defined = BTreeMap::new();
    for line in text.lines() {
        let t = line.trim();
        if !(t.starts_with("| KIM-") || t.starts_with("| SUBAK-")) {
            continue;
        }
        let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
        let id = cells[0].to_string();
        if parse_id_at(&id, 0).map(|(found, len)| found == id && len == id.len()) != Some(true) {
            problems.push(format!(
                "PRODUCT_REQUIREMENTS: malformed requirement ID `{id}`"
            ));
            continue;
        }
        if cells.len() != 5 {
            problems.push(format!("{id}: expected 5 columns, found {}", cells.len()));
        } else {
            if !matches!(cells[2], "P0" | "P1" | "P2") {
                problems.push(format!("{id}: invalid priority `{}`", cells[2]));
            }
            if !matches!(cells[3], "M1" | "M2" | "M3" | "M4") {
                problems.push(format!("{id}: invalid milestone `{}`", cells[3]));
            }
            if cells[1].is_empty() || cells[4].is_empty() {
                problems.push(format!("{id}: empty requirement or verification cell"));
            }
        }
        if defined.insert(id.clone(), t.to_string()).is_some() {
            problems.push(format!("{id}: duplicate requirement ID"));
        }
    }
    defined
}

/// Parses `KIM|SUBAK - FR|NFR|SEC - ddd` starting at byte `i`; returns the ID and its length.
fn parse_id_at(text: &str, i: usize) -> Option<(String, usize)> {
    let rest = &text[i..];
    let product = ["KIM-", "SUBAK-"]
        .into_iter()
        .find(|p| rest.starts_with(p))?;
    let after_product = &rest[product.len()..];
    let kind = ["FR-", "NFR-", "SEC-"]
        .into_iter()
        .find(|k| after_product.starts_with(k))?;
    let digits = &after_product[kind.len()..];
    if digits.len() < 3 || !digits.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
        return None;
    }
    if digits.as_bytes().get(3).is_some_and(u8::is_ascii_digit) {
        return None; // more than three digits
    }
    let len = product.len() + kind.len() + 3;
    Some((rest[..len].to_string(), len))
}

/// All requirement IDs referenced in `text`, expanding `ID…ddd` / `ID...ddd` range endpoints.
pub fn referenced_ids(text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut i = 0;
    while i < text.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let preceded_by_word = text[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !preceded_by_word && let Some((id, len)) = parse_id_at(text, i) {
            let after = &text[i + len..];
            let range_end = ["…", "..."]
                .into_iter()
                .find_map(|sep| after.strip_prefix(sep))
                .map(str::trim_start)
                .and_then(|r| {
                    let d = r.get(..3)?;
                    d.bytes().all(|b| b.is_ascii_digit()).then(|| d.to_string())
                });
            if let Some(end) = range_end {
                ids.insert(format!("{}{end}", &id[..id.len() - 3]));
            }
            ids.insert(id);
            i += len;
            continue;
        }
        i += 1;
    }
    ids
}

/// Relative link targets (`[x](target)` and `<img src="target">`) outside fenced code blocks,
/// without `#fragment`; external URLs and pure anchors are skipped.
pub fn relative_links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let mut rest = line;
        while let Some(pos) = rest.find("](") {
            let after = &rest[pos + 2..];
            let Some(end) = after.find(')') else { break };
            push_target(&after[..end], &mut out);
            rest = &after[end..];
        }
        let mut rest = line;
        while let Some(pos) = rest.find("<img src=\"") {
            let after = &rest[pos + 10..];
            let Some(end) = after.find('"') else { break };
            push_target(&after[..end], &mut out);
            rest = &after[end..];
        }
    }
    out
}

fn push_target(raw: &str, out: &mut Vec<String>) {
    let target = raw.split_whitespace().next().unwrap_or("");
    if target.is_empty()
        || target.starts_with('#')
        || target.contains("://")
        || target.starts_with("mailto:")
    {
        return;
    }
    let path = target.split('#').next().unwrap_or("");
    if !path.is_empty() {
        out.push(path.to_string());
    }
}

fn check_docs_index(root: &Path, problems: &mut Vec<String>) -> Result<(), String> {
    let docs = root.join("docs");
    let index = read(&docs.join("README.md"))?;
    let entries = std::fs::read_dir(&docs).map_err(|e| e.to_string())?;
    for entry in entries {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".md") && name != "README.md" && !index.contains(&format!("]({name})")) {
            problems.push(format!("docs/README.md: does not index docs/{name}"));
        }
    }
    for line in index.lines() {
        if !line.contains("| planned |") {
            continue;
        }
        let Some(start) = line.find('`') else {
            continue;
        };
        let Some(len) = line[start + 1..].find('`') else {
            continue;
        };
        let name = &line[start + 1..start + 1 + len];
        let candidate = if line.contains("(repo root)") {
            root.join(name)
        } else {
            docs.join(name)
        };
        if candidate.exists() {
            problems.push(format!(
                "docs/README.md: `{name}` exists but is still marked planned"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_and_ranges() {
        let ids = referenced_ids("See KIM-FR-001, SUBAK-SEC-001…003 and KIM-NFR-010...012.");
        let ids: Vec<_> = ids.into_iter().collect();
        assert_eq!(
            ids,
            [
                "KIM-FR-001",
                "KIM-NFR-010",
                "KIM-NFR-012",
                "SUBAK-SEC-001",
                "SUBAK-SEC-003"
            ]
        );
    }

    #[test]
    fn ignores_lookalikes() {
        assert!(referenced_ids("XKIM-FR-001 KIM-FR-01 KIM-FR-0001 KIM-XX-001").is_empty());
    }

    #[test]
    fn handles_non_ascii_text() {
        let ids = referenced_ids("요구사항 KIM-SEC-001 (자동 설치 금지)…");
        assert!(ids.contains("KIM-SEC-001"));
    }

    #[test]
    fn requirement_table_detects_problems() {
        let mut problems = Vec::new();
        let table = "\
| KIM-FR-001 | ok | P0 | M1 | T |
| KIM-FR-001 | duplicate | P0 | M1 | T |
| KIM-FR-002 | bad priority | P9 | M1 | T |
| KIM-FR-003 | bad milestone | P1 | M7 | T |
| KIM-FR-004 | missing column | P1 | M1 |
| KIM-FR-05 | malformed | P1 | M1 | T |
";
        let defined = parse_requirement_table(table, &mut problems);
        assert_eq!(defined.len(), 4);
        let joined = problems.join("\n");
        assert!(joined.contains("KIM-FR-001: duplicate"));
        assert!(joined.contains("invalid priority `P9`"));
        assert!(joined.contains("invalid milestone `M7`"));
        assert!(joined.contains("expected 5 columns"));
        assert!(joined.contains("malformed requirement ID `KIM-FR-05`"));
    }

    #[test]
    fn link_extraction_skips_external_anchors_and_code() {
        let text = "\
[a](docs/A.md) [b](https://x.y) [c](#top) [d](B.md#part)
<img src=\"img/i.png\" alt=\"x\">
```
[ignored](nope.md)
```
";
        assert_eq!(relative_links(text), ["docs/A.md", "B.md", "img/i.png"]);
    }

    #[test]
    fn repository_docs_are_consistent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let report = check(root).expect("docs readable");
        assert!(report.requirement_ids > 0);
        assert!(
            report.problems.is_empty(),
            "docs-check problems:\n{}",
            report.problems.join("\n")
        );
    }
}
