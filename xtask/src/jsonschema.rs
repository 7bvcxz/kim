//! Minimal JSON Schema (2020-12 subset) validator for the repository's contract schemas.
//!
//! Supported keywords: `$ref` (local `#/$defs/...`), `type`, `enum`, `const`, `properties`,
//! `required`, `additionalProperties`, `items`, `minItems`, `maxItems`, `minLength`,
//! `maxLength`, `pattern`, `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`,
//! `oneOf`, `anyOf`, `allOf`, `not`. Annotation keywords are ignored. Any other keyword is a hard
//! error, so a schema can never silently claim a constraint this validator does not enforce.
//!
//! This file is self-contained (std, `serde_json`, `regex`) on purpose: `cargo xtask
//! schema-check` uses it for the committed examples, and the `kim` and `subak` CLI regression
//! tests compile the very same file (`#[path]` module) to validate the real binaries' `--json`
//! output. One validator, no copy that could drift, and no dependency cycle (xtask depends on
//! `kim`). See ADR 0015.

use regex::Regex;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;

const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "$comment",
    "title",
    "description",
    "examples",
    "default",
    "format",
    "deprecated",
    "readOnly",
    "writeOnly",
];

pub struct Validator<'a> {
    root: &'a Value,
    regexes: RefCell<HashMap<String, Regex>>,
}

impl std::fmt::Debug for Validator<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Validator").finish_non_exhaustive()
    }
}

impl<'a> Validator<'a> {
    pub fn new(root: &'a Value) -> Self {
        Self {
            root,
            regexes: RefCell::new(HashMap::new()),
        }
    }

    /// Returns the list of violations (empty = valid), or `Err` if the schema itself uses an
    /// unsupported keyword or is malformed.
    pub fn validate(&self, instance: &Value) -> Result<Vec<String>, String> {
        let mut errors = Vec::new();
        self.check(self.root, instance, "$", &mut errors)?;
        Ok(errors)
    }

    fn resolve(&self, reference: &str) -> Result<&'a Value, String> {
        let name = reference
            .strip_prefix("#/$defs/")
            .ok_or_else(|| format!("unsupported $ref `{reference}` (only #/$defs/<name>)"))?;
        self.root
            .get("$defs")
            .and_then(|d| d.get(name))
            .ok_or_else(|| format!("unresolved $ref `{reference}`"))
    }

    fn regex(&self, pattern: &str) -> Result<Regex, String> {
        if let Some(re) = self.regexes.borrow().get(pattern) {
            return Ok(re.clone());
        }
        let re = Regex::new(pattern).map_err(|e| format!("invalid pattern `{pattern}`: {e}"))?;
        self.regexes
            .borrow_mut()
            .insert(pattern.to_string(), re.clone());
        Ok(re)
    }

    fn check(
        &self,
        schema: &'a Value,
        inst: &Value,
        path: &str,
        errs: &mut Vec<String>,
    ) -> Result<(), String> {
        let obj = match schema {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => {
                errs.push(format!("{path}: not allowed"));
                return Ok(());
            }
            Value::Object(o) => o,
            _ => return Err(format!("schema at {path} is not an object or boolean")),
        };
        for (key, value) in obj {
            match key.as_str() {
                k if ANNOTATIONS.contains(&k) => {}
                "$ref" => {
                    let target = self.resolve(value.as_str().ok_or("$ref must be a string")?)?;
                    self.check(target, inst, path, errs)?;
                }
                "type" => check_type(value, inst, path, errs)?,
                "enum" => {
                    let options = value.as_array().ok_or("enum must be an array")?;
                    if !options.contains(inst) {
                        errs.push(format!("{path}: {inst} is not one of {value}"));
                    }
                }
                "const" => {
                    if value != inst {
                        errs.push(format!("{path}: expected {value}, found {inst}"));
                    }
                }
                "properties" => {
                    let props = value.as_object().ok_or("properties must be an object")?;
                    if let Some(map) = inst.as_object() {
                        for (name, sub) in props {
                            if let Some(v) = map.get(name) {
                                self.check(sub, v, &format!("{path}.{name}"), errs)?;
                            }
                        }
                    }
                }
                "required" => {
                    let names = value.as_array().ok_or("required must be an array")?;
                    if let Some(map) = inst.as_object() {
                        for n in names {
                            let n = n.as_str().ok_or("required entries must be strings")?;
                            if !map.contains_key(n) {
                                errs.push(format!("{path}: missing required property `{n}`"));
                            }
                        }
                    }
                }
                "additionalProperties" => {
                    if let Some(map) = inst.as_object() {
                        let declared = obj.get("properties").and_then(Value::as_object);
                        for (name, v) in map {
                            if declared.is_some_and(|d| d.contains_key(name)) {
                                continue;
                            }
                            self.check(value, v, &format!("{path}.{name}"), errs)?;
                        }
                    }
                }
                "items" => {
                    if let Some(items) = inst.as_array() {
                        for (i, v) in items.iter().enumerate() {
                            self.check(value, v, &format!("{path}[{i}]"), errs)?;
                        }
                    }
                }
                "minItems" | "maxItems" => {
                    let bound = value.as_u64().ok_or("minItems/maxItems must be integers")?;
                    if let Some(items) = inst.as_array() {
                        let len = items.len() as u64;
                        if (key == "minItems" && len < bound) || (key == "maxItems" && len > bound)
                        {
                            errs.push(format!("{path}: {len} items violates {key} {bound}"));
                        }
                    }
                }
                "minLength" | "maxLength" => {
                    let bound = value
                        .as_u64()
                        .ok_or("minLength/maxLength must be integers")?;
                    if let Some(s) = inst.as_str() {
                        let len = s.chars().count() as u64;
                        if (key == "minLength" && len < bound)
                            || (key == "maxLength" && len > bound)
                        {
                            errs.push(format!("{path}: length {len} violates {key} {bound}"));
                        }
                    }
                }
                "pattern" => {
                    let pattern = value.as_str().ok_or("pattern must be a string")?;
                    if let Some(s) = inst.as_str()
                        && !self.regex(pattern)?.is_match(s)
                    {
                        errs.push(format!("{path}: {s:?} does not match /{pattern}/"));
                    }
                }
                "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                    let bound = value.as_f64().ok_or("numeric bounds must be numbers")?;
                    if let Some(n) = inst.as_f64() {
                        let ok = match key.as_str() {
                            "minimum" => n >= bound,
                            "maximum" => n <= bound,
                            "exclusiveMinimum" => n > bound,
                            _ => n < bound,
                        };
                        if !ok {
                            errs.push(format!("{path}: {n} violates {key} {bound}"));
                        }
                    }
                }
                "oneOf" | "anyOf" | "allOf" => {
                    let subs = value.as_array().ok_or("oneOf/anyOf/allOf must be arrays")?;
                    let mut passing = 0;
                    // Report the closest failing alternative (fewest violations) as the hint.
                    let mut closest: Option<Vec<String>> = None;
                    for sub in subs {
                        let mut sub_errs = Vec::new();
                        self.check(sub, inst, path, &mut sub_errs)?;
                        if sub_errs.is_empty() {
                            passing += 1;
                        } else if closest.as_ref().is_none_or(|c| sub_errs.len() < c.len()) {
                            closest = Some(sub_errs);
                        }
                    }
                    let first_errors = closest.unwrap_or_default();
                    let ok = match key.as_str() {
                        "oneOf" => passing == 1,
                        "anyOf" => passing >= 1,
                        _ => passing == subs.len(),
                    };
                    if !ok {
                        errs.push(format!(
                            "{path}: {passing} of {} `{key}` alternatives matched{}",
                            subs.len(),
                            first_errors
                                .first()
                                .map(|e| format!(" (e.g. {e})"))
                                .unwrap_or_default()
                        ));
                    }
                }
                "not" => {
                    // Used for reserved values (e.g. OS-reserved hotkey chords). The sibling
                    // `description`, when present, names what was matched.
                    let mut sub_errs = Vec::new();
                    self.check(value, inst, path, &mut sub_errs)?;
                    if sub_errs.is_empty() {
                        let why = obj
                            .get("description")
                            .and_then(Value::as_str)
                            .map_or_else(|| format!("matches {value}"), str::to_string);
                        errs.push(format!("{path}: {inst} is not allowed: {why}"));
                    }
                }
                other => return Err(format!("unsupported schema keyword `{other}` at {path}")),
            }
        }
        Ok(())
    }
}

fn check_type(
    expected: &Value,
    inst: &Value,
    path: &str,
    errs: &mut Vec<String>,
) -> Result<(), String> {
    let names: Vec<&str> = match expected {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a
            .iter()
            .map(|v| v.as_str().ok_or("type entries must be strings"))
            .collect::<Result<_, _>>()?,
        _ => return Err("type must be a string or array".into()),
    };
    let matches = |name: &str| match name {
        "null" => inst.is_null(),
        "boolean" => inst.is_boolean(),
        "object" => inst.is_object(),
        "array" => inst.is_array(),
        "string" => inst.is_string(),
        "number" => inst.is_number(),
        "integer" => {
            inst.is_i64() || inst.is_u64() || inst.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    };
    for name in &names {
        if !matches!(
            *name,
            "null" | "boolean" | "object" | "array" | "string" | "number" | "integer"
        ) {
            return Err(format!("unknown type `{name}`"));
        }
    }
    if !names.iter().any(|n| matches(n)) {
        errs.push(format!("{path}: expected type {expected}, found {inst}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)] // test helper: owned `json!` literals read better
    fn errors(schema: Value, inst: Value) -> Vec<String> {
        Validator::new(&schema)
            .validate(&inst)
            .expect("schema supported")
    }

    #[test]
    fn types_and_required() {
        let s =
            json!({"type": "object", "required": ["a"], "properties": {"a": {"type": "integer"}}});
        assert!(errors(s.clone(), json!({"a": 3})).is_empty());
        assert_eq!(errors(s.clone(), json!({})).len(), 1);
        assert_eq!(errors(s, json!({"a": "x"})).len(), 1);
    }

    #[test]
    fn nullable_type_arrays() {
        let s = json!({"type": ["string", "null"]});
        assert!(errors(s.clone(), json!(null)).is_empty());
        assert!(errors(s.clone(), json!("x")).is_empty());
        assert!(!errors(s, json!(1)).is_empty());
    }

    #[test]
    fn additional_properties_false_and_schema() {
        let closed = json!({"properties": {"a": {}}, "additionalProperties": false});
        assert!(errors(closed.clone(), json!({"a": 1})).is_empty());
        assert!(!errors(closed, json!({"a": 1, "b": 2})).is_empty());
        let typed = json!({"additionalProperties": {"type": "string"}});
        assert!(!errors(typed, json!({"k": 1})).is_empty());
    }

    #[test]
    fn one_of_requires_exactly_one_match() {
        let s = json!({"oneOf": [{"type": "integer"}, {"type": "number"}]});
        // 1 matches both integer and number → oneOf fails.
        assert!(!errors(s.clone(), json!(1)).is_empty());
        assert!(errors(s, json!(1.5)).is_empty());
    }

    #[test]
    fn refs_patterns_and_bounds() {
        let s = json!({
            "$defs": {"id": {"type": "string", "pattern": "^t-[0-9a-f]{12}$"}},
            "type": "object",
            "properties": {
                "id": {"$ref": "#/$defs/id"},
                "size": {"type": "number", "exclusiveMinimum": 0, "maximum": 1},
                "name": {"type": "string", "minLength": 1, "maxLength": 3}
            }
        });
        assert!(
            errors(
                s.clone(),
                json!({"id": "t-3fa9c0d21b7e", "size": 1, "name": "수박"})
            )
            .is_empty()
        );
        assert_eq!(errors(s.clone(), json!({"id": "t-XYZ"})).len(), 1);
        assert_eq!(errors(s.clone(), json!({"size": 0})).len(), 1);
        assert_eq!(errors(s, json!({"name": "abcd"})).len(), 1);
    }

    #[test]
    fn not_rejects_matching_instances_and_names_the_reason() {
        let s = json!({
            "type": "string",
            "allOf": [{
                "description": "reserved by the OS",
                "not": {"type": "string", "pattern": "(^| )Win\\+"}
            }]
        });
        assert!(errors(s.clone(), json!("Ctrl+T")).is_empty());
        let e = errors(s, json!("Win+L"));
        assert_eq!(e.len(), 1);
        assert!(e[0].contains("reserved by the OS"), "{e:?}");
        // The `type` inside `not` keeps non-strings from being rejected by accident.
        assert!(errors(json!({"not": {"type": "string", "pattern": "x"}}), json!(1)).is_empty());
    }

    #[test]
    fn unsupported_keywords_are_errors_not_silently_ignored() {
        let s = json!({"type": "string", "contentEncoding": "base64"});
        assert!(Validator::new(&s).validate(&json!("x")).is_err());
        let r = json!({"$ref": "other.json#/x"});
        assert!(Validator::new(&r).validate(&json!(1)).is_err());
    }
}
