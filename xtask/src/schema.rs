//! The `schema-check` task: validates `docs/contracts/examples/<name>/*.json` (must pass) and
//! `docs/contracts/examples/<name>/invalid/*.json` (must fail) against
//! `docs/contracts/schemas/<name>.schema.json`, using the subset validator in
//! [`crate::jsonschema`]. The hotkey scenarios are additionally recomputed by the reference
//! checker in [`crate::hotkeys`] (docs/HOTKEY_CONTRACT.md §15).

use crate::jsonschema::Validator;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub schemas: usize,
    pub valid_examples: usize,
    pub invalid_examples: usize,
    pub hotkey_scenarios: usize,
    /// For each invalid example: (path, first violation) — shown by `schema-check --verbose`.
    pub rejections: Vec<(String, String)>,
    pub problems: Vec<String>,
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: invalid JSON: {e}", path.display()))
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

pub fn check_contracts(root: &Path) -> Result<Report, String> {
    let contracts = root.join("docs").join("contracts");
    let mut report = Report::default();
    for schema_path in json_files(&contracts.join("schemas")) {
        let file = schema_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let Some(name) = file.strip_suffix(".schema.json") else {
            report
                .problems
                .push(format!("schemas/{file}: must be named <name>.schema.json"));
            continue;
        };
        report.schemas += 1;
        let schema = read_json(&schema_path)?;
        let validator = Validator::new(&schema);
        let examples = contracts.join("examples").join(name);
        let valid = json_files(&examples);
        let invalid = json_files(&examples.join("invalid"));
        if valid.is_empty() || invalid.is_empty() {
            report.problems.push(format!(
                "{name}: needs at least one valid and one invalid example in examples/{name}/"
            ));
        }
        for path in &valid {
            report.valid_examples += 1;
            let errors = validator.validate(&read_json(path)?)?;
            if !errors.is_empty() {
                report.problems.push(format!(
                    "{}: should be valid but: {}",
                    path.strip_prefix(&contracts).unwrap_or(path).display(),
                    errors.join("; ")
                ));
            }
        }
        for path in &invalid {
            report.invalid_examples += 1;
            let rel = path
                .strip_prefix(&contracts)
                .unwrap_or(path)
                .display()
                .to_string();
            match validator.validate(&read_json(path)?)?.into_iter().next() {
                Some(first) => report.rejections.push((rel, first)),
                None => report
                    .problems
                    .push(format!("{rel}: should be rejected by {file} but validates")),
            }
        }
    }
    let hotkeys = crate::hotkeys::check(&contracts)?;
    report.hotkey_scenarios = hotkeys.scenarios;
    report.problems.extend(hotkeys.problems);
    if report.schemas == 0 {
        report
            .problems
            .push("no schemas found in docs/contracts/schemas".into());
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_contract_examples_conform() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let report = check_contracts(root).expect("contracts readable");
        assert!(
            report.schemas >= 10,
            "expected the IPC, state, catalog, CLI and hotkey schemas"
        );
        assert!(
            report.problems.is_empty(),
            "schema-check problems:\n{}",
            report.problems.join("\n")
        );
    }
}
