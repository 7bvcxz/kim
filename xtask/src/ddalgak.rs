//! Reference checks for Ddalgak's rule model (docs/DDALGAK_RULE_MODEL.md §8), run by
//! `cargo xtask schema-check` on the contract examples.
//!
//! The `ddalgak-rules-v1` schema catches everything that one rule can get wrong on its own
//! (reserved triggers, outputs Ddalgak never produces, missing consent, the injected-input
//! recursion guard). What needs the whole document is checked here:
//!
//! * unique profile, mode and rule IDs, and every mode or profile reference resolves;
//! * no rule re-sends its own trigger (`output_is_trigger`: the classic recursive remap);
//! * no two enabled rules of a profile claim the same trigger with the same modes, conditions
//!   and priority (`duplicate_trigger`);
//! * a `launch-programs` consent covers exactly the rule's current targets (`consent_stale`,
//!   recomputing the SHA-256 fingerprint of §6.2);
//! * a run's delays stay within 60 seconds (`run_too_long`).
//!
//! Valid examples must produce no error. Each file in `examples/ddalgak-rules-v1/semantic-invalid/`
//! is named `<expected_code>--<what>.json`, must pass the schema, and must produce exactly that
//! error code. The check also keeps `$defs.reserved_free` identical to the hotkey schemas', and
//! checks that every `ddalgak-simulation-v1` example (`<rules example>--<what>.json`) traces
//! rules that exist in that rules example. Like `hotkeys.rs`, this is a reference for the fixtures, not a library:
//! Ddalgak's Step 67 implements these rules itself and must agree with the same files.

use crate::jsonschema::Validator;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const RULES: &str = "ddalgak-rules-v1";
pub const SIMULATION: &str = "ddalgak-simulation-v1";
/// The longest total delay of one rule run (DDALGAK_RULE_MODEL §5.12).
const MAX_RUN_DELAY_MS: u64 = 60_000;

#[derive(Debug, Default)]
pub struct Report {
    /// Rules documents checked semantically (valid and semantic-invalid examples).
    pub documents: usize,
    pub problems: Vec<String>,
}

/// One semantic error: a stable code and where it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub at: String,
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

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

pub fn check(contracts: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let schemas = contracts.join("schemas");
    let rules_schema = read_json(&schemas.join(format!("{RULES}.schema.json")))?;
    let hotkeys_schema = read_json(&schemas.join("kim-hotkeys-v1.schema.json"))?;
    if rules_schema["$defs"]["reserved_free"]["allOf"]
        != hotkeys_schema["$defs"]["reserved_free"]["allOf"]
    {
        report.problems.push(format!(
            "{RULES}: `$defs.reserved_free` differs from kim-hotkeys-v1 (HOTKEY_CONTRACT §7)"
        ));
    }
    let validator = Validator::new(&rules_schema);
    let examples = contracts.join("examples").join(RULES);

    let mut rules_docs = BTreeMap::new();
    for path in json_files(&examples) {
        report.documents += 1;
        let doc = read_json(&path)?;
        for f in semantic_errors(&doc) {
            report.problems.push(format!(
                "examples/{RULES}/{}.json: {} at {}",
                stem(&path),
                f.code,
                f.at
            ));
        }
        rules_docs.insert(stem(&path), doc);
    }

    let semantic_invalid = json_files(&examples.join("semantic-invalid"));
    if semantic_invalid.is_empty() {
        report.problems.push(format!(
            "{RULES}: needs semantic-invalid examples in examples/{RULES}/semantic-invalid/"
        ));
    }
    for path in semantic_invalid {
        report.documents += 1;
        let rel = format!("examples/{RULES}/semantic-invalid/{}.json", stem(&path));
        let name = stem(&path);
        let Some((expected, _)) = name.split_once("--") else {
            report.problems.push(format!(
                "{rel}: name must be `<expected_code>--<what>.json`"
            ));
            continue;
        };
        let doc = read_json(&path)?;
        let schema_errors = validator.validate(&doc)?;
        if !schema_errors.is_empty() {
            report.problems.push(format!(
                "{rel}: must pass the schema (only its cross-references are wrong) but: {}",
                schema_errors.join("; ")
            ));
        }
        let codes: BTreeSet<&str> = semantic_errors(&doc).iter().map(|f| f.code).collect();
        if codes != BTreeSet::from([expected]) {
            report.problems.push(format!(
                "{rel}: expected exactly `{expected}`, found {codes:?}"
            ));
        }
    }

    for path in json_files(&contracts.join("examples").join(SIMULATION)) {
        check_simulation(&path, &rules_docs, &mut report.problems)?;
    }
    Ok(report)
}

/// A simulation example is named `<rules example>--<what>.json`. It must test a profile and mode
/// of that rules example, and every rule it names (the decision and the candidates) must exist
/// there; a fired rule's effects follow its steps, in order.
fn check_simulation(
    path: &Path,
    rules_docs: &BTreeMap<String, Value>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let name = stem(path);
    let rel = format!("examples/{SIMULATION}/{name}.json");
    let sim = read_json(path)?;
    let Some((source, _)) = name.split_once("--") else {
        problems.push(format!(
            "{rel}: name must be `<rules example>--<what>.json`"
        ));
        return Ok(());
    };
    let Some(doc) = rules_docs.get(source) else {
        problems.push(format!("{rel}: rules example `{source}` does not exist"));
        return Ok(());
    };
    let profile_id = sim["profile"].as_str().unwrap_or_default();
    let Some(profile) = profiles(doc).find(|p| p["id"] == profile_id) else {
        problems.push(format!("{rel}: `{source}` has no profile `{profile_id}`"));
        return Ok(());
    };
    let mode = &sim["context"]["mode"];
    if !list(&profile["modes"]).any(|m| &m["id"] == mode) {
        problems.push(format!("{rel}: profile `{profile_id}` has no mode {mode}"));
    }
    let rule_of = |id: &str| list(&profile["rules"]).find(|r| r["id"] == id);
    for c in list(&sim["decision"]["candidates"]) {
        let id = c["rule"].as_str().unwrap_or_default();
        if rule_of(id).is_none() {
            problems.push(format!(
                "{rel}: candidate `{id}` is not a rule of `{profile_id}`"
            ));
        }
    }
    let Some(rule_id) = sim["decision"]["rule"].as_str() else {
        return Ok(()); // nothing matched
    };
    let Some(rule) = rule_of(rule_id) else {
        problems.push(format!(
            "{rel}: profile `{profile_id}` of `{source}` has no rule `{rule_id}`"
        ));
        return Ok(());
    };
    let declared: Vec<&Value> = list(&rule["steps"]).map(|s| &s["type"]).collect();
    let traced: Vec<&Value> = list(&sim["effects"]).map(|e| &e["type"]).collect();
    if !traced.is_empty() && traced != declared {
        problems.push(format!(
            "{rel}: effects {traced:?} do not follow the steps {declared:?} of rule `{rule_id}`"
        ));
    }
    Ok(())
}

fn list(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

fn profiles(doc: &Value) -> impl Iterator<Item = &Value> {
    list(&doc["profiles"])
}

/// The semantic errors of a rules document (DDALGAK_RULE_MODEL §8), in document order.
pub fn semantic_errors(doc: &Value) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |code, at: String| out.push(Finding { code, at });

    let mut profile_ids = BTreeSet::new();
    for (pi, p) in profiles(doc).enumerate() {
        if let Some(id) = p["id"].as_str()
            && !profile_ids.insert(id)
        {
            push("duplicate_profile_id", format!("profiles[{pi}]"));
        }
    }
    if let Some(active) = doc["active_profile"].as_str()
        && !profile_ids.contains(active)
    {
        push("unknown_profile", "active_profile".into());
    }

    let mut rule_ids = BTreeSet::new();
    for (pi, p) in profiles(doc).enumerate() {
        let at_p = format!("profiles[{pi}]");
        let mut modes = BTreeSet::new();
        for (mi, m) in list(&p["modes"]).enumerate() {
            if let Some(id) = m["id"].as_str()
                && !modes.insert(id)
            {
                push("duplicate_mode_id", format!("{at_p}.modes[{mi}]"));
            }
        }
        let known = |id: &Value| id.as_str().is_some_and(|id| modes.contains(id));
        if !known(&p["default_mode"]) {
            push("unknown_mode", format!("{at_p}.default_mode"));
        }

        let mut triggers: BTreeMap<String, usize> = BTreeMap::new();
        for (ri, r) in list(&p["rules"]).enumerate() {
            let at = format!("{at_p}.rules[{ri}]");
            if let Some(id) = r["id"].as_str()
                && !rule_ids.insert(id.to_string())
            {
                push("duplicate_rule_id", format!("{at}.id"));
            }
            for (i, m) in list(&r["modes"]).enumerate() {
                if !known(m) {
                    push("unknown_mode", format!("{at}.modes[{i}]"));
                }
            }
            let keys = r["trigger"]["keys"].as_str().unwrap_or_default();
            let mut delay_ms = 0;
            for (si, s) in list(&r["steps"]).enumerate() {
                let at_s = format!("{at}.steps[{si}]");
                if s["type"] == "mode" {
                    if !s["mode"].is_null() && !known(&s["mode"]) {
                        push("unknown_mode", format!("{at_s}.mode"));
                    }
                    for (i, m) in list(&s["modes"]).enumerate() {
                        if !known(m) {
                            push("unknown_mode", format!("{at_s}.modes[{i}]"));
                        }
                    }
                }
                let output = match s["type"].as_str() {
                    Some("remap") => s["to"].as_str(),
                    Some("send_keys") => s["keys"].as_str(),
                    _ => None,
                };
                if output == Some(keys) {
                    push("output_is_trigger", at_s.clone());
                }
                if s["type"] == "delay" {
                    delay_ms += s["ms"].as_u64().unwrap_or(0);
                }
            }
            if delay_ms > MAX_RUN_DELAY_MS {
                push("run_too_long", format!("{at}.steps"));
            }
            if let Some(granted) = r["consent"]["launch-programs"]["fingerprint"].as_str()
                && granted != launch_fingerprint(r)
            {
                push("consent_stale", format!("{at}.consent.launch-programs"));
            }
            if r["enabled"] == true {
                let key = trigger_key(r);
                if triggers.insert(key, ri).is_some() {
                    push("duplicate_trigger", format!("{at}.trigger"));
                }
            }
        }
    }
    out
}

/// What makes two enabled rules of one profile claim the same input: the trigger (keys, `on`,
/// `hold_ms`), the modes (unordered; none = every mode), the conditions and the priority.
fn trigger_key(rule: &Value) -> String {
    let t = &rule["trigger"];
    let modes: BTreeSet<&str> = list(&rule["modes"]).filter_map(Value::as_str).collect();
    let mut when = String::new();
    canonical_json(&rule["when"], &mut when);
    format!(
        "{}|{}|{}|{:?}|{}|{}",
        t["keys"].as_str().unwrap_or_default(),
        t["on"].as_str().unwrap_or("press"),
        t["hold_ms"].as_u64().unwrap_or(500),
        modes,
        when,
        rule["priority"].as_i64().unwrap_or(0)
    )
}

/// `sha256:` + hex SHA-256 of the canonical JSON array of the rule's `open_program` and
/// `open_url` steps, in order (DDALGAK_RULE_MODEL §6.2).
pub fn launch_fingerprint(rule: &Value) -> String {
    let targets: Vec<Value> = list(&rule["steps"])
        .filter(|s| s["type"] == "open_program" || s["type"] == "open_url")
        .cloned()
        .collect();
    let mut text = String::new();
    canonical_json(&Value::Array(targets), &mut text);
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// RFC 8785 (JSON Canonicalization Scheme) for the values rules contain: object keys sorted by
/// UTF-16 code units, no whitespace, strings escaped as serde_json does (which matches RFC 8785),
/// integers as written. Rules hold no floating-point numbers.
pub fn canonical_json(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push('{');
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical_json(val, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)] // test helper: owned `json!` literals read better
    fn doc(rules: Value) -> Value {
        json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "p",
            "profiles": [{"id": "p", "title": "P", "default_mode": "base",
                          "modes": [{"id": "base", "title": "Base"}], "rules": rules}]
        })
    }

    fn codes(d: &Value) -> Vec<&'static str> {
        semantic_errors(d).into_iter().map(|f| f.code).collect()
    }

    #[test]
    fn canonical_json_sorts_keys_and_drops_whitespace() {
        let mut s = String::new();
        canonical_json(
            &json!({"b": [1, {"z": "é\n", "a": true}], "a": null}),
            &mut s,
        );
        assert_eq!(s, r#"{"a":null,"b":[1,{"a":true,"z":"é\n"}]}"#);
    }

    #[test]
    fn fingerprint_covers_only_launch_targets_and_changes_with_them() {
        let mut rule = json!({"steps": [
            {"type": "delay", "ms": 10},
            {"type": "open_url", "url": "https://example.com/"}
        ]});
        let before = launch_fingerprint(&rule);
        assert!(before.starts_with("sha256:") && before.len() == 71);
        rule["steps"][0]["ms"] = json!(20);
        assert_eq!(launch_fingerprint(&rule), before, "a delay is not a target");
        rule["steps"][1]["url"] = json!("https://example.org/");
        assert_ne!(
            launch_fingerprint(&rule),
            before,
            "a new URL needs new consent"
        );
    }

    #[test]
    fn self_triggering_outputs_are_recursive() {
        let d = doc(json!([
            {"id": "a", "title": "A", "enabled": true, "trigger": {"keys": "F13"},
             "steps": [{"type": "remap", "to": "F13"}]},
            {"id": "b", "title": "B", "enabled": true, "trigger": {"keys": "F14"},
             "steps": [{"type": "send_keys", "keys": "F14"}]},
            {"id": "c", "title": "C (a swap is fine)", "enabled": true, "trigger": {"keys": "F15"},
             "steps": [{"type": "remap", "to": "F16"}]},
            {"id": "d", "title": "D", "enabled": true, "trigger": {"keys": "F16"},
             "steps": [{"type": "remap", "to": "F15"}]}
        ]));
        assert_eq!(codes(&d), ["output_is_trigger", "output_is_trigger"]);
    }

    #[test]
    fn duplicate_triggers_depend_on_modes_conditions_priority_and_enabled() {
        let rule = |id: &str, extra: Value| {
            let mut r = json!({"id": id, "title": id, "enabled": true,
                               "trigger": {"keys": "F13"}, "steps": [{"type": "block"}]});
            for (k, v) in extra.as_object().unwrap() {
                r[k] = v.clone();
            }
            r
        };
        let d = doc(json!([
            rule("a", json!({})),
            rule("b", json!({"priority": 1})),
            rule("c", json!({"when": {"apps": [{"exe": "code.exe"}]}})),
            rule("d", json!({"modes": ["base"]})),
            rule("e", json!({"enabled": false})),
            rule("f", json!({"trigger": {"keys": "F13", "on": "release"}})),
            rule("g", json!({"trigger": {"keys": "F13", "on": "press"}}))
        ]));
        let found = semantic_errors(&d);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].code, "duplicate_trigger");
        assert_eq!(found[0].at, "profiles[0].rules[6].trigger");
    }

    #[test]
    fn references_and_ids_resolve() {
        let mut d = doc(json!([
            {"id": "a", "title": "A", "enabled": true, "trigger": {"keys": "F13"},
             "modes": ["nav"], "steps": [{"type": "mode", "op": "cycle", "modes": ["base", "x"]}]}
        ]));
        d["active_profile"] = json!("missing");
        let mut second = d["profiles"][0].clone();
        second["modes"] = json!([{"id": "base", "title": "B"}, {"id": "base", "title": "B"}]);
        d["profiles"].as_array_mut().unwrap().push(second);
        assert_eq!(
            codes(&d),
            [
                "duplicate_profile_id",
                "unknown_profile",
                "unknown_mode",
                "unknown_mode",
                "duplicate_mode_id",
                "duplicate_rule_id",
                "unknown_mode",
                "unknown_mode"
            ]
        );
    }

    #[test]
    fn long_runs_and_stale_consent_are_reported() {
        let delays = vec![json!({"type": "delay", "ms": 10000}); 7];
        let mut r = json!({"id": "a", "title": "A", "enabled": true, "trigger": {"keys": "F13"},
                           "steps": delays});
        r["steps"][6] = json!({"type": "open_url", "url": "https://example.com/"});
        r["consent"] = json!({"launch-programs": {"granted_at": "2026-11-04T00:00:00Z",
                                                  "via": "gui", "fingerprint": launch_fingerprint(&r)}});
        assert!(codes(&doc(json!([r.clone()]))).is_empty());
        r["steps"][5] = json!({"type": "delay", "ms": 10001});
        r["steps"][6]["url"] = json!("https://example.org/");
        assert_eq!(codes(&doc(json!([r]))), ["run_too_long", "consent_stale"]);
    }

    #[test]
    fn repository_examples_conform() {
        // Also compiled into Ddalgak's tests (crates/ddalgak/src/validate.rs): find the root.
        let contracts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .map(|p| p.join("docs").join("contracts"))
            .find(|p| p.is_dir())
            .unwrap();
        let report = check(&contracts).expect("examples readable");
        assert!(report.documents >= 10, "{report:?}");
        assert!(report.problems.is_empty(), "{}", report.problems.join("\n"));
    }
}
