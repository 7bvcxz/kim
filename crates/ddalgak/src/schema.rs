//! The `ddalgak-rules-v1` schema at run time (docs/DDALGAK_RULE_MODEL.md §8, ADR 0028).
//!
//! Ddalgak validates rules with **the same schema file and the same validator** that
//! `cargo xtask schema-check` uses on the contract examples: the schema is compiled in from
//! `docs/contracts/schemas/ddalgak-rules-v1.schema.json` and the validator is
//! `xtask/src/jsonschema.rs`, included as a module. There is no second copy that could drift.
//!
//! The schema is applied in two parts (ADR 0028):
//!
//! - **The document without its rules** ([`file_errors`]): a violation there (a missing profile
//!   list, a bad mode, more than 1000 rules) means the file is not loaded (`rules_invalid`).
//! - **Each rule on its own** ([`rule_problems`]): a violation makes only that rule inactive, with
//!   the code of §8 that names the part that is wrong (`trigger_reserved`, `trigger_invalid`,
//!   `output_reserved`, `step_invalid`, `recursion_risk`, `consent_missing`, `rule_invalid`).
//!   The other rules keep working (`DDALGAK-FR-006`).

use crate::jsonschema::Validator;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// The rules schema as published in the repository.
pub const RULES_SCHEMA: &str =
    include_str!("../../../docs/contracts/schemas/ddalgak-rules-v1.schema.json");

/// Codes of §8 that schema parts produce, in the order a rule reports them first.
pub const SCHEMA_CODES: [&str; 7] = [
    "trigger_reserved",
    "trigger_invalid",
    "output_reserved",
    "step_invalid",
    "recursion_risk",
    "consent_missing",
    "rule_invalid",
];

struct Roots {
    /// The document; rules are validated separately.
    doc: Value,
    rule: Value,
    /// The rule without `trigger`, `steps` and the cross-field constraints.
    rule_base: Value,
    trigger: Value,
    trigger_keys: Value,
    trigger_syntax: Value,
    step: Value,
    /// `$defs/step_<type>` for each known step type, and `$defs/step_future`.
    step_types: Vec<(String, Value)>,
    step_future: Value,
    output_reserved_free: Value,
    /// The rule's `allOf` constraints with the code each one maps to.
    constraints: Vec<(Value, &'static str, String)>,
}

fn with_defs(defs: &Value, body: Value) -> Value {
    let mut v = body;
    v["$defs"] = defs.clone();
    v
}

fn roots() -> &'static Roots {
    static R: OnceLock<Roots> = OnceLock::new();
    R.get_or_init(|| {
        let schema: Value = serde_json::from_str(RULES_SCHEMA).expect("the built-in schema");
        let defs = schema["$defs"].clone();
        let reference = |name: &str| with_defs(&defs, json!({ "$ref": format!("#/$defs/{name}") }));
        let mut base = defs["rule"].clone();
        if let Some(o) = base.as_object_mut() {
            o.remove("allOf");
            if let Some(p) = o.get_mut("properties").and_then(Value::as_object_mut) {
                p.remove("trigger");
                p.remove("steps");
            }
            if let Some(r) = o.get_mut("required").and_then(Value::as_array_mut) {
                r.retain(|n| n != "trigger" && n != "steps");
            }
        }
        let constraints = defs["rule"]["allOf"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let d = c["description"].as_str().unwrap_or_default().to_string();
                let lower = d.to_lowercase();
                let code = if lower.contains("recursion") {
                    "recursion_risk"
                } else if lower.contains("consent") {
                    "consent_missing"
                } else if lower.contains("bare modifier") {
                    "trigger_invalid"
                } else {
                    "step_invalid"
                };
                (with_defs(&defs, json!({ "allOf": [c] })), code, d)
            })
            .collect();
        Roots {
            doc: schema,
            rule: reference("rule"),
            rule_base: with_defs(&defs, base),
            trigger: reference("trigger"),
            trigger_keys: reference("trigger_keys"),
            trigger_syntax: reference("trigger_syntax"),
            step: reference("step"),
            step_types: crate::model::STEP_TYPES
                .iter()
                .map(|t| ((*t).to_string(), reference(&format!("step_{t}"))))
                .collect(),
            step_future: reference("step_future"),
            output_reserved_free: reference("output_reserved_free"),
            constraints,
        }
    })
}

struct Validators {
    doc: Validator<'static>,
    rule: Validator<'static>,
    rule_base: Validator<'static>,
    trigger: Validator<'static>,
    trigger_keys: Validator<'static>,
    trigger_syntax: Validator<'static>,
    step: Validator<'static>,
    step_types: Vec<(String, Validator<'static>)>,
    step_future: Validator<'static>,
    output_reserved_free: Validator<'static>,
    constraints: Vec<(Validator<'static>, &'static str, &'static str)>,
}

thread_local! {
    // A validator caches its compiled patterns; one set per thread keeps that cache warm.
    static V: Validators = {
        let r = roots();
        Validators {
            doc: Validator::new(&r.doc),
            rule: Validator::new(&r.rule),
            rule_base: Validator::new(&r.rule_base),
            trigger: Validator::new(&r.trigger),
            trigger_keys: Validator::new(&r.trigger_keys),
            trigger_syntax: Validator::new(&r.trigger_syntax),
            step: Validator::new(&r.step),
            step_types: r
                .step_types
                .iter()
                .map(|(t, val)| (t.clone(), Validator::new(val)))
                .collect(),
            step_future: Validator::new(&r.step_future),
            output_reserved_free: Validator::new(&r.output_reserved_free),
            constraints: r
                .constraints
                .iter()
                .map(|(v, code, d)| (Validator::new(v), *code, d.as_str()))
                .collect(),
        }
    };
}

fn run(v: &Validator<'_>, inst: &Value) -> Vec<String> {
    v.validate(inst)
        .unwrap_or_else(|e| vec![format!("$: the built-in schema cannot be applied: {e}")])
}

/// A step against `$defs/step`. Its `oneOf` alternatives are told apart by `type` (each known
/// type has a `const`; `step_future` excludes every known type), so the alternative of the
/// step's own type decides alone; other steps get the whole `oneOf` for the message.
fn step_errors(v: &Validators, s: &Value) -> Vec<String> {
    let typed = s.get("type").and_then(Value::as_str).and_then(|t| {
        v.step_types
            .iter()
            .find(|(name, _)| name == t)
            .map(|(_, val)| val)
    });
    match typed {
        Some(val) => run(val, s),
        None if s.get("type").is_some_and(Value::is_string) => run(&v.step_future, s),
        None => run(&v.step, s),
    }
}

/// Rewrites a validator path (`$.trigger.keys: …`) below `prefix` (`profiles[0].rules[3]`).
fn rebase(err: &str, prefix: &str) -> String {
    match err.strip_prefix('$') {
        Some(rest) if prefix.is_empty() => rest.trim_start_matches('.').to_string(),
        Some(rest) => format!("{prefix}{rest}"),
        None => err.to_string(),
    }
}

/// Schema violations of the document outside its rules (each profile's `rules` is checked for
/// its size only). Empty: the document may be loaded.
pub fn file_errors(doc: &Value) -> Vec<String> {
    let mut skeleton = doc.clone();
    let mut errors = Vec::new();
    if let Some(profiles) = skeleton.get_mut("profiles").and_then(Value::as_array_mut) {
        for (i, p) in profiles.iter_mut().enumerate() {
            if let Some(rules) = p.get_mut("rules")
                && let Some(list) = rules.as_array()
            {
                if list.len() > 1000 {
                    errors.push(format!(
                        "profiles[{i}].rules: {} rules violates maxItems 1000",
                        list.len()
                    ));
                }
                *rules = json!([]);
            }
        }
    }
    V.with(|v| {
        errors.extend(run(&v.doc, &skeleton).iter().map(|e| rebase(e, "")));
    });
    errors
}

/// Whether `keys` is a valid, unreserved trigger by the schema (`trigger_keys`).
pub fn trigger_keys_ok(keys: &str) -> bool {
    V.with(|v| run(&v.trigger_keys, &json!(keys)).is_empty())
}

/// The schema problems of one rule as `(code, message)`, codes from [`SCHEMA_CODES`]; empty when
/// the rule is valid. `at` is the rule's path (`profiles[0].rules[3]`) for the messages.
pub fn rule_problems(rule: &Value, at: &str) -> Vec<(&'static str, String)> {
    V.with(|v| {
        // The rule schema is `rule_base ∧ trigger ∧ steps ∧ allOf constraints`; checking the parts
        // gives the same verdict (a test compares it with the whole rule schema) and names the
        // part that is wrong.
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let mut push = |code: &'static str, msg: String| {
            if !out.iter().any(|(c, _)| *c == code) {
                out.push((code, msg));
            }
        };
        let at_t = format!("{at}.trigger");
        match rule.get("trigger") {
            Some(t) if t.is_object() => {
                let errs = run(&v.trigger, t);
                if let Some(first) = errs.first() {
                    let code = match t.get("keys").and_then(Value::as_str) {
                        Some(k)
                            if run(&v.trigger_syntax, &json!(k)).is_empty()
                                && !run(&v.trigger_keys, &json!(k)).is_empty() =>
                        {
                            "trigger_reserved"
                        }
                        _ => "trigger_invalid",
                    };
                    push(code, rebase(first, &at_t));
                }
            }
            _ => push(
                "trigger_invalid",
                format!("{at_t}: an object with `keys` is required"),
            ),
        }
        match rule.get("steps").and_then(Value::as_array) {
            Some(steps) if (1..=32).contains(&steps.len()) => {
                for (k, s) in steps.iter().enumerate() {
                    let errs = step_errors(v, s);
                    let Some(first) = errs.first() else { continue };
                    let reserved_output = ["to", "tap", "keys"].iter().any(|f| {
                        s.get(*f)
                            .and_then(Value::as_str)
                            .is_some_and(|o| !run(&v.output_reserved_free, &json!(o)).is_empty())
                    });
                    let at_s = format!("{at}.steps[{k}]");
                    if reserved_output {
                        push(
                            "output_reserved",
                            format!(
                                "{at_s}: the output contains the Secure Attention Sequence or the emergency chord, which Ddalgak never produces"
                            ),
                        );
                    } else if s.get("type").and_then(Value::as_str) == Some("type_text") {
                        // The validator quotes failing strings; a snippet's text never appears in
                        // a message (DDALGAK-SEC-011), only what is wrong with it.
                        let msg = match s.get("text").and_then(Value::as_str) {
                            Some(t) => crate::text::text_problem(
                                t,
                                s.get("variables").and_then(Value::as_bool) == Some(true),
                            )
                            .map(|p| format!("{at_s}.text: {p}")),
                            None => None,
                        };
                        let msg = msg.unwrap_or_else(|| {
                            if errs.iter().any(|e| e.contains(".text")) {
                                format!("{at_s}.text: the text is not valid (the text is not shown)")
                            } else {
                                rebase(first, &at_s)
                            }
                        });
                        push("step_invalid", msg);
                    } else if matches!(
                        s.get("type").and_then(Value::as_str),
                        Some("open_program" | "open_url")
                    ) {
                        // The validator quotes failing strings; a path, argument, URL or
                        // environment value never appears in a message (DDALGAK-SEC-011, Step 70).
                        let msg = match crate::launch::compile_step(s) {
                            Err(e) => format!("{at_s}: {e}"),
                            Ok(_) => format!(
                                "{at_s}: the step's inputs are not valid (DDALGAK_RULE_MODEL §5.5, §5.6; the target is not shown)"
                            ),
                        };
                        push("step_invalid", msg);
                    } else {
                        push("step_invalid", rebase(first, &at_s));
                    }
                }
            }
            _ => push("step_invalid", format!("{at}.steps: 1–32 steps are required")),
        }
        for (c, code, description) in &v.constraints {
            if !run(c, rule).is_empty() {
                push(code, format!("{at}: {description}"));
            }
        }
        for e in run(&v.rule_base, rule) {
            // A grant that is not a valid consent record (a launch grant without fingerprint) is
            // missing consent; every other field problem is `rule_invalid`.
            let code = if e.starts_with("$.consent") {
                "consent_missing"
            } else {
                "rule_invalid"
            };
            push(code, rebase(&e, at));
        }
        out.sort_by_key(|(c, _)| SCHEMA_CODES.iter().position(|x| x == c));
        out
    })
}

/// Validates a whole document against the full schema (what `schema-check` does).
pub fn document_errors(doc: &Value) -> Vec<String> {
    V.with(|v| run(&v.doc, doc))
}

/// One rule against the whole `$defs/rule` (the reference for [`rule_problems`]).
pub fn rule_errors_whole(rule: &Value) -> Vec<String> {
    V.with(|v| run(&v.rule, rule))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contracts/examples/ddalgak-rules-v1")
    }

    fn read(p: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    /// The code each invalid example must get from the per-rule split, by its file name.
    fn expected(name: &str) -> &'static str {
        match name {
            n if n.starts_with("reserved-") => "trigger_reserved",
            "remap-emergency-chord" | "remap-emergency-chord-side-specific" => "trigger_reserved",
            n if n.starts_with("missing-consent-") || n == "launch-consent-without-fingerprint" => {
                "consent_missing"
            }
            n if n.starts_with("recursive-") => "recursion_risk",
            "remap-to-emergency-chord" | "send-secure-attention-sequence" => "output_reserved",
            "char-trigger"
            | "leader-trigger"
            | "non-canonical-modifier-order"
            | "sequence-release-trigger"
            | "hold-ms-without-hold"
            | "bare-modifier-press-action"
            | "mouse-primary-button-trigger"
            | "mouse-button-sequence-trigger"
            | "hotstring-with-keys"
            | "hotstring-abbreviation-with-space"
            | "hotstring-immediate-with-end-chars" => "trigger_invalid",
            // whole-file problems
            "newer-schema-version" | "no-modes" | "rule-id-with-dot" => "file",
            _ => "step_invalid",
        }
    }

    #[test]
    fn the_repository_schema_is_the_compiled_in_one() {
        let on_disk = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/contracts/schemas/ddalgak-rules-v1.schema.json"),
        )
        .unwrap();
        assert_eq!(on_disk, RULES_SCHEMA);
    }

    #[test]
    fn valid_examples_have_no_schema_problem_anywhere() {
        for e in std::fs::read_dir(dir()).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let doc = read(&p);
            assert!(document_errors(&doc).is_empty(), "{}", p.display());
            assert!(file_errors(&doc).is_empty(), "{}", p.display());
            for (pi, prof) in doc["profiles"].as_array().unwrap().iter().enumerate() {
                for (ri, r) in prof["rules"].as_array().unwrap().iter().enumerate() {
                    let at = format!("profiles[{pi}].rules[{ri}]");
                    assert!(rule_problems(r, &at).is_empty(), "{}: {at}", p.display());
                }
            }
        }
    }

    #[test]
    fn every_invalid_example_is_refused_by_exactly_the_part_that_is_wrong() {
        let mut seen = 0;
        for e in std::fs::read_dir(dir().join("invalid")).unwrap().flatten() {
            let p = e.path();
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let doc = read(&p);
            seen += 1;
            assert!(
                !document_errors(&doc).is_empty(),
                "{name}: the full schema refuses it"
            );
            let file = file_errors(&doc);
            let mut codes = Vec::new();
            for (pi, prof) in doc["profiles"].as_array().into_iter().flatten().enumerate() {
                for (ri, r) in prof["rules"].as_array().into_iter().flatten().enumerate() {
                    let at = format!("profiles[{pi}].rules[{ri}]");
                    // A rule ID the schema refuses cannot be named: that is a file problem.
                    if !crate::model::is_rule_id(r["id"].as_str().unwrap_or_default()) {
                        continue;
                    }
                    codes.extend(rule_problems(r, &at).into_iter().map(|(c, _)| c));
                }
            }
            match expected(&name) {
                "file" => assert!(
                    !file.is_empty()
                        || doc["profiles"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .any(|p| p["rules"].as_array().into_iter().flatten().any(|r| {
                                !crate::model::is_rule_id(r["id"].as_str().unwrap_or_default())
                            })),
                    "{name}: a whole-file problem, got {codes:?}"
                ),
                code => {
                    assert!(file.is_empty(), "{name}: {file:?}");
                    assert_eq!(codes.first().copied(), Some(code), "{name}: {codes:?}");
                }
            }
        }
        assert!(seen >= 39, "{seen}");
    }

    #[test]
    fn the_split_check_gives_the_whole_rule_schema_verdict() {
        // Every rule of every fixture, plus mutations of each, through both paths.
        let mut rules = Vec::new();
        for sub in ["", "invalid", "semantic-invalid"] {
            for e in std::fs::read_dir(dir().join(sub)).unwrap().flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "json") {
                    continue;
                }
                for prof in read(&p)["profiles"].as_array().into_iter().flatten() {
                    rules.extend(prof["rules"].as_array().into_iter().flatten().cloned());
                }
            }
        }
        type Mutation = Box<dyn Fn(&mut Value)>;
        let mutations: Vec<Mutation> = vec![
            Box::new(|_| {}),
            Box::new(|r| r["steps"][0]["type"] = json!("teleport")),
            Box::new(|r| r["steps"][0]["type"] = json!("Bad-Type")),
            Box::new(|r| r["steps"][0]["type"] = json!(7)),
            Box::new(|r| r["steps"][0] = json!("not a step")),
            Box::new(|r| r["steps"][0]["extra_field"] = json!(1)),
            Box::new(|r| r["enabled"] = json!(true)),
            Box::new(|r| r["trigger"]["injected"] = json!("accept")),
            Box::new(|r| r["trigger"]["on"] = json!("release")),
            Box::new(|r| r["steps"] = json!([])),
            Box::new(|r| r["priority"] = json!(5000)),
            Box::new(|r| {
                if let Some(o) = r.as_object_mut() {
                    o.remove("consent");
                }
            }),
            Box::new(|r| {
                let s = r["steps"][0].clone();
                if let Some(a) = r["steps"].as_array_mut() {
                    a.push(s);
                }
            }),
        ];
        let mut n = 0;
        for r in &rules {
            for m in &mutations {
                let mut r = r.clone();
                m(&mut r);
                let whole = rule_errors_whole(&r).is_empty();
                let split = rule_problems(&r, "x").is_empty();
                assert_eq!(whole, split, "{r}");
                n += 1;
            }
        }
        assert!(n > 500, "{n}");
    }

    #[test]
    fn messages_name_the_rule_path() {
        let r = json!({ "id": "a", "title": "A", "enabled": true, "trigger": { "keys": "Win+L" },
                        "steps": [ { "type": "block" } ] });
        let p = rule_problems(&r, "profiles[0].rules[2]");
        assert_eq!(p[0].0, "trigger_reserved");
        assert!(p[0].1.starts_with("profiles[0].rules[2].trigger"), "{p:?}");
        let r = json!({ "id": "a", "title": "", "enabled": true, "trigger": { "keys": "F13" },
                        "steps": [ { "type": "block" } ] });
        assert_eq!(rule_problems(&r, "x")[0].0, "rule_invalid");
        assert!(trigger_keys_ok("Ctrl+Shift+F13"));
        assert!(!trigger_keys_ok("Ctrl+Alt+Delete"));
    }
}
