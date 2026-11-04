//! Reading `rules.json` for the engine (DDALGAK_RULE_MODEL §2, §9).
//!
//! **Scope (Step 66).** This is the engine's *read-only runtime loader*, not the rule store:
//! Step 67 implements the full schema and semantic validation, CRUD, history and atomic writes,
//! and replaces the checks here with its own. Until then this loader is deliberately
//! conservative ("never broader", §9): it runs only what this version fully understands and keeps
//! everything else inactive with a stated reason.
//!
//! - A missing file is one empty profile `default` with mode `base`.
//! - A file that is not JSON, is not a `ddalgak.rules` document, or names a missing profile is
//!   **not loaded** (`rules_invalid`); a newer `schema_version` is not run either
//!   (`rules_from_newer_version`). The engine cannot be turned on with such a file, and a running
//!   engine keeps its previous rules (`DDALGAK-FR-006`).
//! - Of the active profile's enabled rules, those whose steps are all `engine` steps
//!   (DDALGAK_RULE_MODEL §5.11, `DDALGAK-FR-017`) become bindings. Every other action type is
//!   inactive with `step_unsupported` until the Step that implements it (68–72); rules with
//!   `when` are inactive with `condition_unsupported` (Step 77); unknown fields inside a rule make
//!   it `rule_unsupported`.
//! - Modes: Step 66 has no mode switching (Step 72), so the profile's `default_mode` is always
//!   active; rules limited to other modes are counted, not run.
//! - Precedence (§7.1): mode-scoped rules first, then higher `priority`, then file order. Two
//!   rules with the same trigger, modes and priority: the later one is `duplicate_trigger`.

use crate::table::{Action, Binding, EngineOp, Injected, On};
use crate::trigger::{TKey, Trigger};
use serde_json::{Map, Value, json};
use std::path::Path;

/// What the file looked like.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileState {
    Missing,
    Loaded,
    /// Not loaded: why.
    Invalid(String),
    /// Written by a newer Ddalgak; not loaded.
    NewerVersion(u64),
}

impl FileState {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileState::Missing => "missing",
            FileState::Loaded => "loaded",
            FileState::Invalid(_) => "invalid",
            FileState::NewerVersion(_) => "newer_version",
        }
    }

    /// Whether the engine may run with this file.
    pub fn usable(&self) -> bool {
        matches!(self, FileState::Missing | FileState::Loaded)
    }
}

/// An enabled rule that does not run, with the code of DDALGAK_RULE_MODEL §8.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Inactive {
    pub rule: String,
    pub code: &'static str,
    pub reason: String,
}

/// The engine's view of `rules.json`.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub state: FileState,
    pub revision: Option<u64>,
    pub profile: String,
    pub mode: String,
    pub engine_on_start: bool,
    pub max_runs_per_second: u32,
    /// Active bindings in precedence order.
    pub bindings: Vec<Binding>,
    pub inactive: Vec<Inactive>,
    pub disabled: usize,
    /// Enabled rules limited to modes other than the active one.
    pub other_modes: usize,
    /// Rules in the active profile.
    pub total: usize,
}

impl Loaded {
    fn empty(state: FileState) -> Loaded {
        Loaded {
            state,
            revision: None,
            profile: "default".into(),
            mode: "base".into(),
            engine_on_start: false,
            max_runs_per_second: 20,
            bindings: Vec::new(),
            inactive: Vec::new(),
            disabled: 0,
            other_modes: 0,
            total: 0,
        }
    }
}

/// Reads and interprets the rules file.
pub fn load(path: &Path) -> Loaded {
    match std::fs::read(path) {
        Ok(bytes) => parse(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded::empty(FileState::Missing),
        Err(e) => Loaded::empty(FileState::Invalid(format!("cannot read the file: {e}"))),
    }
}

fn is_rule_id(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

const RULE_FIELDS: &[&str] = &[
    "id",
    "title",
    "description",
    "enabled",
    "trigger",
    "modes",
    "when",
    "priority",
    "steps",
    "on_error",
    "consent",
    "created_at",
    "updated_at",
];
const TRIGGER_FIELDS: &[&str] = &[
    "keys",
    "on",
    "hold_ms",
    "repeat",
    "pass_through",
    "injected",
];
/// Step types of rules document v1 that later Steps implement.
const LATER_STEPS: &[(&str, &str)] = &[
    ("remap", "Step 68"),
    ("block", "Step 68"),
    ("send_keys", "Step 68"),
    ("type_text", "Step 69"),
    ("open_program", "Step 70"),
    ("open_url", "Step 70"),
    ("mouse_move", "Step 71"),
    ("mouse_scroll", "Step 71"),
    ("mouse_click", "Step 71"),
    ("mode", "Step 72"),
    ("delay", "Step 68"),
];

struct Candidate {
    binding: Binding,
    scoped: bool,
    priority: i64,
    order: usize,
}

fn unknown_field(o: &Map<String, Value>, known: &[&str]) -> Option<String> {
    o.keys().find(|k| !known.contains(&k.as_str())).cloned()
}

/// Interprets one rule; `Err` makes it inactive.
fn rule(r: &Map<String, Value>) -> Result<(Binding, i64), (&'static str, String)> {
    if let Some(f) = unknown_field(r, RULE_FIELDS) {
        return Err((
            "rule_unsupported",
            format!("unknown field `{f}` (a newer Ddalgak may restrict the rule with it)"),
        ));
    }
    if r.contains_key("when") {
        return Err((
            "condition_unsupported",
            "app and window conditions are evaluated from Step 77".into(),
        ));
    }
    let priority = match r.get("priority") {
        None => 0,
        Some(p) => p
            .as_i64()
            .filter(|p| (-1000..=1000).contains(p))
            .ok_or(("rule_invalid", "`priority` must be -1000…1000".to_string()))?,
    };
    let t = r
        .get("trigger")
        .and_then(Value::as_object)
        .ok_or(("trigger_invalid", "`trigger` must be an object".to_string()))?;
    if let Some(f) = unknown_field(t, TRIGGER_FIELDS) {
        return Err(("rule_unsupported", format!("unknown trigger field `{f}`")));
    }
    let keys = t
        .get("keys")
        .and_then(Value::as_str)
        .ok_or(("trigger_invalid", "`trigger.keys` is required".to_string()))?;
    let trigger = Trigger::parse(keys).map_err(|e| (e.code(), e.to_string()))?;
    let text = |k: &'static str| t.get(k).map(move |v| v.as_str().ok_or(k));
    let flag = |k: &'static str| t.get(k).map(move |v| v.as_bool().ok_or(k));
    let bad = |k: &str| {
        (
            "trigger_invalid",
            format!("`trigger.{k}` has a wrong type or value"),
        )
    };
    let on = match text("on").transpose().map_err(bad)? {
        None | Some("press") => On::Press,
        Some("release") => On::Release,
        Some("hold") => {
            let ms = match t.get("hold_ms") {
                None => 500,
                Some(v) => v
                    .as_u64()
                    .filter(|ms| (200..=5000).contains(ms))
                    .ok_or_else(|| bad("hold_ms"))?,
            };
            On::Hold(u32::try_from(ms).unwrap_or(500))
        }
        Some(_) => return Err(bad("on")),
    };
    if t.contains_key("hold_ms") && !matches!(on, On::Hold(_)) {
        return Err(bad("hold_ms"));
    }
    let repeat = flag("repeat").transpose().map_err(bad)?.unwrap_or(false);
    let pass_through = flag("pass_through")
        .transpose()
        .map_err(bad)?
        .unwrap_or(false);
    let injected = match text("injected").transpose().map_err(bad)? {
        None | Some("ignore") => Injected::Ignore,
        Some("accept") => Injected::Accept,
        Some(_) => return Err(bad("injected")),
    };
    if repeat && on != On::Press {
        return Err(bad("repeat"));
    }
    if trigger.is_sequence() && on != On::Press {
        return Err(("trigger_invalid", "sequences fire on press only".into()));
    }
    if let Some(c) = trigger.single()
        && matches!(c.key, TKey::Modifier(_))
        && on != On::Release
    {
        return Err((
            "trigger_invalid",
            "a bare modifier fires a non-remap rule only with `on: release`".into(),
        ));
    }
    let steps = r
        .get("steps")
        .and_then(Value::as_array)
        .filter(|s| (1..=32).contains(&s.len()))
        .ok_or(("step_invalid", "`steps` must hold 1–32 steps".to_string()))?;
    let mut ops = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        let s = s
            .as_object()
            .ok_or(("step_invalid", format!("step {i} is not an object")))?;
        let ty = s
            .get("type")
            .and_then(Value::as_str)
            .ok_or(("step_invalid", format!("step {i} has no `type`")))?;
        if ty == "engine" {
            if let Some(f) = unknown_field(s, &["type", "op"]) {
                return Err((
                    "rule_unsupported",
                    format!("unknown field `{f}` in step {i}"),
                ));
            }
            let op = s
                .get("op")
                .and_then(Value::as_str)
                .and_then(EngineOp::parse)
                .ok_or(("step_invalid", format!("step {i}: unknown engine `op`")))?;
            ops.push(op);
        } else if let Some((_, step)) = LATER_STEPS.iter().find(|(t, _)| *t == ty) {
            return Err((
                "step_unsupported",
                format!("`{ty}` steps are not implemented in this version yet ({step})"),
            ));
        } else {
            return Err((
                "step_unsupported",
                format!("unknown step type `{ty}` (written by a newer Ddalgak?)"),
            ));
        }
    }
    Ok((
        Binding {
            rule: r["id"].as_str().unwrap_or_default().to_string(),
            trigger,
            on,
            repeat,
            pass_through,
            injected,
            widen: false,
            injects: false,
            action: Action::Engine(ops),
        },
        priority,
    ))
}

/// Interprets the bytes of a rules file.
pub fn parse(bytes: &[u8]) -> Loaded {
    let invalid = |why: String| Loaded::empty(FileState::Invalid(why));
    let doc: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return invalid(format!("not valid JSON: {e}")),
    };
    let Some(doc) = doc.as_object() else {
        return invalid("the document is not a JSON object".into());
    };
    if doc.get("schema").and_then(Value::as_str) != Some("ddalgak.rules") {
        return invalid("`schema` is not `ddalgak.rules`".into());
    }
    match doc.get("schema_version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(v) if v > 1 => return Loaded::empty(FileState::NewerVersion(v)),
        _ => return invalid("`schema_version` must be 1".into()),
    }
    let settings = doc.get("settings").and_then(Value::as_object);
    let engine_on_start = settings
        .and_then(|s| s.get("engine_on_start"))
        .and_then(Value::as_str)
        == Some("on");
    let max_runs = settings
        .and_then(|s| s.get("max_runs_per_second"))
        .and_then(Value::as_u64)
        .map_or(20, |n| n.clamp(1, 100));
    let Some(profiles) = doc.get("profiles").and_then(Value::as_array) else {
        return invalid("`profiles` is missing".into());
    };
    let active = doc
        .get("active_profile")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(profile) = profiles
        .iter()
        .filter_map(Value::as_object)
        .find(|p| p.get("id").and_then(Value::as_str) == Some(active))
    else {
        return invalid(format!(
            "`active_profile` `{active}` is not a profile of the file (unknown_profile)"
        ));
    };
    let mode = profile
        .get("default_mode")
        .and_then(Value::as_str)
        .unwrap_or("base")
        .to_string();
    let rules = profile
        .get("rules")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut out = Loaded {
        state: FileState::Loaded,
        revision: doc.get("revision").and_then(Value::as_u64),
        profile: active.to_string(),
        mode: mode.clone(),
        engine_on_start,
        max_runs_per_second: u32::try_from(max_runs).unwrap_or(20),
        total: rules.len(),
        ..Loaded::empty(FileState::Loaded)
    };
    let mut ids = std::collections::BTreeSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (order, r) in rules.iter().enumerate() {
        let Some(r) = r.as_object() else {
            return invalid(format!("rule {order} is not an object"));
        };
        let id = r.get("id").and_then(Value::as_str).unwrap_or_default();
        if !is_rule_id(id) {
            return invalid(format!("rule {order} has no valid `id`"));
        }
        let Some(enabled) = r.get("enabled").and_then(Value::as_bool) else {
            return invalid(format!("rule `{id}` has no `enabled`"));
        };
        if !ids.insert(id.to_string()) {
            out.inactive.push(Inactive {
                rule: id.into(),
                code: "duplicate_rule_id",
                reason: "the ID is used by an earlier rule".into(),
            });
            continue;
        }
        if !enabled {
            out.disabled += 1;
            continue;
        }
        let modes: Option<Vec<&str>> = r
            .get("modes")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter_map(Value::as_str).collect());
        if let Some(m) = &modes
            && !m.contains(&mode.as_str())
        {
            out.other_modes += 1;
            continue;
        }
        match rule(r) {
            Ok((binding, priority)) => candidates.push(Candidate {
                binding,
                scoped: modes.is_some(),
                priority,
                order,
            }),
            Err((code, reason)) => out.inactive.push(Inactive {
                rule: id.into(),
                code,
                reason,
            }),
        }
    }
    // Duplicate triggers (same keys, scope and priority): the later rule is inactive.
    let mut kept: Vec<Candidate> = Vec::new();
    for c in candidates {
        if kept.iter().any(|k| {
            k.binding.trigger == c.binding.trigger
                && k.scoped == c.scoped
                && k.priority == c.priority
        }) {
            out.inactive.push(Inactive {
                rule: c.binding.rule.clone(),
                code: "duplicate_trigger",
                reason: format!("an earlier rule has the trigger `{}`", c.binding.trigger),
            });
            continue;
        }
        kept.push(c);
    }
    kept.sort_by(|a, b| {
        b.scoped
            .cmp(&a.scoped)
            .then(b.priority.cmp(&a.priority))
            .then(a.order.cmp(&b.order))
    });
    out.bindings = kept.into_iter().map(|c| c.binding).collect();
    out
}

/// `rules` in status output: counts, codes and rule IDs (never rule content).
pub fn to_json(l: &Loaded) -> Value {
    json!({
        "file": l.state.as_str(),
        "problem": match &l.state {
            FileState::Invalid(why) => Some(why.clone()),
            FileState::NewerVersion(v) => Some(format!("schema_version {v} is newer than this Ddalgak")),
            _ => None,
        },
        "revision": l.revision,
        "engine_on_start": if l.engine_on_start { "on" } else { "off" },
        "total": l.total,
        "active": l.bindings.len(),
        "active_rules": l.bindings.iter().map(|b| b.rule.clone()).collect::<Vec<_>>(),
        "disabled": l.disabled,
        "other_modes": l.other_modes,
        "inactive": l.inactive.iter().map(|i| json!({ "rule": i.rule, "code": i.code, "reason": i.reason })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(clippy::needless_pass_by_value)]
    fn doc(rules: Value) -> Vec<u8> {
        json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "revision": 3, "active_profile": "default",
            "settings": { "engine_on_start": "on", "max_runs_per_second": 5 },
            "profiles": [ { "id": "default", "title": "Everyday", "default_mode": "base",
                "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Nav" } ],
                "rules": rules } ]
        })
        .to_string()
        .into_bytes()
    }

    fn engine_rule(id: &str, keys: &str, op: &str) -> Value {
        json!({ "id": id, "title": id, "enabled": true, "trigger": { "keys": keys },
                "steps": [ { "type": "engine", "op": op } ] })
    }

    #[test]
    fn missing_invalid_and_newer_files() {
        let missing = load(Path::new("Z:/does/not/exist/rules.json"));
        assert_eq!(missing.state, FileState::Missing);
        assert!(missing.state.usable());
        assert_eq!(
            (missing.profile.as_str(), missing.mode.as_str()),
            ("default", "base")
        );
        assert_eq!(parse(b"{not json").state.as_str(), "invalid");
        assert!(!parse(b"[]").state.usable());
        let newer = json!({"schema": "ddalgak.rules", "schema_version": 2}).to_string();
        assert_eq!(parse(newer.as_bytes()).state, FileState::NewerVersion(2));
        let wrong = json!({"schema": "ddalgak.rules", "schema_version": 1, "active_profile": "x",
            "profiles": [{"id": "default", "rules": []}]})
        .to_string();
        let l = parse(wrong.as_bytes());
        assert!(matches!(&l.state, FileState::Invalid(w) if w.contains("unknown_profile")));
    }

    #[test]
    fn engine_control_rules_load_and_others_stay_inactive_with_reasons() {
        let l = parse(&doc(json!([
            engine_rule("pause-key", "Ctrl+Alt+Shift+F13", "toggle_pause"),
            { "id": "caps", "title": "c", "enabled": true, "trigger": { "keys": "CapsLock" },
              "steps": [ { "type": "remap", "to": "LCtrl" } ],
              "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
            { "id": "future", "title": "f", "enabled": true, "trigger": { "keys": "F14" },
              "steps": [ { "type": "teleport" } ] },
            { "id": "cond", "title": "c", "enabled": true, "trigger": { "keys": "F15" },
              "when": { "apps": [ { "exe": "Code.exe" } ] },
              "steps": [ { "type": "engine", "op": "pause" } ] },
            { "id": "newer-field", "title": "n", "enabled": true, "trigger": { "keys": "F16", "only_on_tuesdays": true },
              "steps": [ { "type": "engine", "op": "pause" } ] },
            { "id": "off", "title": "o", "enabled": false, "trigger": { "keys": "F17" },
              "steps": [ { "type": "engine", "op": "pause" } ] },
            { "id": "reserved", "title": "r", "enabled": true, "trigger": { "keys": "Win+L" },
              "steps": [ { "type": "engine", "op": "pause" } ] },
            { "id": "nav-only", "title": "n", "enabled": true, "modes": ["nav"], "trigger": { "keys": "F18" },
              "steps": [ { "type": "engine", "op": "pause" } ] },
            engine_rule("dup", "Ctrl+Alt+Shift+F13", "pause"),
            engine_rule("pause-key", "F19", "pause")
        ])));
        assert_eq!(l.state, FileState::Loaded);
        assert_eq!(l.revision, Some(3));
        assert!(l.engine_on_start);
        assert_eq!(l.max_runs_per_second, 5);
        assert_eq!(l.total, 10);
        assert_eq!(l.disabled, 1);
        assert_eq!(l.other_modes, 1);
        assert_eq!(l.bindings.len(), 1);
        assert_eq!(l.bindings[0].rule, "pause-key");
        assert_eq!(
            l.bindings[0].action,
            Action::Engine(vec![EngineOp::TogglePause])
        );
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("caps", "step_unsupported"),
                ("future", "step_unsupported"),
                ("cond", "condition_unsupported"),
                ("newer-field", "rule_unsupported"),
                ("reserved", "trigger_reserved"),
                ("pause-key", "duplicate_rule_id"),
                ("dup", "duplicate_trigger"),
            ]
        );
        assert!(l.inactive[0].reason.contains("Step 68"));
    }

    #[test]
    fn precedence_is_mode_scope_then_priority_then_order() {
        let mut low = engine_rule("low", "F13", "pause");
        low["priority"] = json!(-5);
        let mut high = engine_rule("high", "F14", "pause");
        high["priority"] = json!(10);
        let mut scoped = engine_rule("scoped", "F15", "pause");
        scoped["modes"] = json!(["base"]);
        let l = parse(&doc(json!([
            low,
            engine_rule("plain", "F16", "pause"),
            high,
            scoped
        ])));
        let order: Vec<&str> = l.bindings.iter().map(|b| b.rule.as_str()).collect();
        assert_eq!(order, ["scoped", "high", "plain", "low"]);
    }

    #[test]
    fn trigger_options_are_checked() {
        let mut r = engine_rule("tap", "RAlt", "toggle_pause");
        r["trigger"] = json!({ "keys": "RAlt", "on": "release" });
        let mut bad_mod = engine_rule("badmod", "RCtrl", "pause");
        bad_mod["trigger"] = json!({ "keys": "RCtrl" });
        let mut hold = engine_rule("hold", "F13", "pause");
        hold["trigger"] =
            json!({ "keys": "F13", "on": "hold", "hold_ms": 800, "injected": "accept" });
        let mut bad_hold = engine_rule("badhold", "F14", "pause");
        bad_hold["trigger"] = json!({ "keys": "F14", "on": "hold", "hold_ms": 50 });
        let mut seq_release = engine_rule("seqrel", "F15 F16", "pause");
        seq_release["trigger"] = json!({ "keys": "F15 F16", "on": "release" });
        let l = parse(&doc(json!([r, bad_mod, hold, bad_hold, seq_release])));
        let active: Vec<&str> = l.bindings.iter().map(|b| b.rule.as_str()).collect();
        assert_eq!(active, ["tap", "hold"]);
        assert_eq!(l.bindings[1].on, On::Hold(800));
        assert_eq!(l.bindings[1].injected, Injected::Accept);
        assert!(
            l.inactive.iter().all(|i| i.code == "trigger_invalid"),
            "{:?}",
            l.inactive
        );
    }

    #[test]
    fn the_repository_examples_load_without_running_anything_unsupported() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/ddalgak-rules-v1");
        let mut seen = 0;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            seen += 1;
            let l = load(&p);
            assert!(
                l.state.usable() || matches!(l.state, FileState::NewerVersion(_)),
                "{}: {:?}",
                p.display(),
                l.state
            );
            for b in &l.bindings {
                assert!(b.is_engine_control(), "{}: {}", p.display(), b.rule);
            }
        }
        assert!(seen >= 10);
    }
}
