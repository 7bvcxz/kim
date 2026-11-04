//! The engine's view of `rules.json` (docs/DDALGAK_RULE_MODEL.md §2, §7, §9;
//! `DDALGAK-FR-006`, `DDALGAK-FR-009`, `DDALGAK-FR-017`, `DDALGAK-FR-021`).
//!
//! The file is read and checked by the rule store ([`crate::store`], [`crate::validate`]); this
//! module turns the result into what the engine runs and what `status` shows:
//!
//! - A file that is not usable (not JSON, not `ddalgak.rules`, a schema violation outside the
//!   rules, an unknown active profile or default mode) is **not loaded** (`rules_invalid`); a
//!   newer `schema_version` is not run either (`rules_from_newer_version`). The engine cannot be
//!   turned on with it, and a running engine keeps its previous rules.
//! - Of the active profile's enabled rules, every rule with an error or an `*_unsupported` info
//!   is **inactive** with that code; the others keep working.
//! - Of the remaining rules, those whose steps are all `engine` steps (§5.11), a `remap` (§5.1)
//!   and `block` (§5.2; Step 68) become bindings. Every other action type is inactive with
//!   `step_unsupported` until its Step (69–72, 78).
//! - Modes: there is no mode switching before Step 72, so the profile's `default_mode` is active;
//!   rules limited to other modes are counted, not run.
//! - Precedence (§7.1): mode-scoped rules first, then higher `priority`, then file order.

use crate::remap::Output;
use crate::store::{self, MIGRATIONS};
use crate::table::{Action, Binding, EngineOp, Injected, On, RemapSpec};
use crate::trigger::{TKey, Trigger};
use crate::validate::Context;
use serde_json::{Map, Value, json};
use std::path::Path;

pub use crate::store::FileState;

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
    /// A hash of the file's bytes: a reload applies only real changes.
    pub hash: u64,
    /// Errors and warnings in the whole document (every profile).
    pub errors: usize,
    pub warnings: usize,
}

impl Loaded {
    fn empty(state: FileState, revision: Option<u64>, hash: u64) -> Loaded {
        Loaded {
            state,
            revision,
            profile: "default".into(),
            mode: "base".into(),
            engine_on_start: false,
            max_runs_per_second: 20,
            bindings: Vec::new(),
            inactive: Vec::new(),
            disabled: 0,
            other_modes: 0,
            total: 0,
            hash,
            errors: 0,
            warnings: 0,
        }
    }
}

/// The step types this version runs (Step 66: `engine`; Step 68: `remap`, `block`). A rule made
/// of `engine` and `block` steps is an engine-control rule (a `block` step adds nothing to a rule
/// that consumes its key anyway); a `remap` is always the rule's only step (schema
/// `holds_trigger`).
pub const RUNNABLE_STEPS: [&str; 3] = ["engine", "remap", "block"];

/// Reads and interprets the rules file.
pub fn load(path: &Path) -> Loaded {
    load_with(path, &Context::default())
}

/// Reads and interprets the rules file with what the checks know beyond it (Ddalgak's own
/// shortcuts).
pub fn load_with(path: &Path, ctx: &Context) -> Loaded {
    from_store(&store::read_path(path, ctx, MIGRATIONS))
}

/// Interprets the bytes of a rules file.
pub fn parse(bytes: &[u8]) -> Loaded {
    from_store(&store::interpret(bytes, &Context::default(), MIGRATIONS))
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

struct Candidate {
    binding: Binding,
    scoped: bool,
    priority: i64,
    order: usize,
}

/// The engine's interpretation of a checked document.
pub fn from_store(l: &store::Loaded) -> Loaded {
    let (Some(doc), true) = (&l.doc, l.state.usable()) else {
        return Loaded::empty(l.state.clone(), l.revision, l.hash);
    };
    let settings = doc.settings.as_ref();
    let pi = doc
        .profile_index(&doc.active_profile)
        .expect("a usable document has its active profile");
    let profile = &doc.profiles[pi];
    let mode = profile.default_mode.clone();
    let mut out = Loaded {
        state: l.state.clone(),
        revision: doc.revision.or(l.revision),
        profile: profile.id.clone(),
        mode: mode.clone(),
        engine_on_start: settings.and_then(|s| s.engine_on_start) == Some(crate::model::OnOff::On),
        max_runs_per_second: settings
            .and_then(|s| s.max_runs_per_second)
            .map_or(20, |n| n.clamp(1, 100)),
        total: profile.rules.len(),
        errors: l.report.count(crate::validate::Severity::Error),
        warnings: l.report.count(crate::validate::Severity::Warning),
        ..Loaded::empty(l.state.clone(), l.revision, l.hash)
    };
    let mut candidates: Vec<Candidate> = Vec::new();
    for (ri, entry) in profile.rules.iter().enumerate() {
        let id = entry.id().unwrap_or_default().to_string();
        if entry.enabled() != Some(true) {
            out.disabled += 1;
            continue;
        }
        if let Some(p) = l.report.inactive((pi, ri)) {
            out.inactive.push(Inactive {
                rule: id,
                code: p.code,
                reason: p.message.clone(),
            });
            continue;
        }
        let Some(rule) = entry.rule() else {
            continue; // a raw rule always has a schema problem (above)
        };
        if let Some(m) = &rule.modes
            && !m.contains(&mode)
        {
            out.other_modes += 1;
            continue;
        }
        let Some(map) = rule.to_value().as_object().cloned() else {
            continue;
        };
        match compile(&map) {
            Ok((binding, priority)) => candidates.push(Candidate {
                binding,
                scoped: rule.modes.is_some(),
                priority,
                order: ri,
            }),
            Err((code, reason)) => out.inactive.push(Inactive {
                rule: id,
                code,
                reason,
            }),
        }
    }
    candidates.sort_by(|a, b| {
        b.scoped
            .cmp(&a.scoped)
            .then(b.priority.cmp(&a.priority))
            .then(a.order.cmp(&b.order))
    });
    out.bindings = candidates.into_iter().map(|c| c.binding).collect();
    out
}

fn unknown_field(o: &Map<String, Value>, known: &[&str]) -> Option<String> {
    o.keys().find(|k| !known.contains(&k.as_str())).cloned()
}

/// Compiles one checked rule into a binding; `Err` keeps it inactive. The validation already
/// refused everything below; these checks are the last line of defense ("never broader").
fn compile(r: &Map<String, Value>) -> Result<(Binding, i64), (&'static str, String)> {
    if let Some(f) = unknown_field(r, RULE_FIELDS) {
        return Err(("rule_unsupported", format!("unknown field `{f}`")));
    }
    if r.contains_key("when") {
        return Err((
            "condition_unsupported",
            "app and window conditions are evaluated from Step 77".into(),
        ));
    }
    let priority = r.get("priority").and_then(Value::as_i64).unwrap_or(0);
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
    let bad = |k: &str| {
        (
            "trigger_invalid",
            format!("`trigger.{k}` has a wrong type or value"),
        )
    };
    let on = match t.get("on").and_then(Value::as_str) {
        None | Some("press") => On::Press,
        Some("release") => On::Release,
        Some("hold") => {
            let ms = t.get("hold_ms").and_then(Value::as_u64).unwrap_or(500);
            On::Hold(u32::try_from(ms).unwrap_or(500))
        }
        Some(_) => return Err(bad("on")),
    };
    let repeat = t.get("repeat").and_then(Value::as_bool).unwrap_or(false);
    let pass_through = t
        .get("pass_through")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let injected = match t.get("injected").and_then(Value::as_str) {
        None | Some("ignore") => Injected::Ignore,
        Some("accept") => Injected::Accept,
        Some(_) => return Err(bad("injected")),
    };
    let steps = r
        .get("steps")
        .and_then(Value::as_array)
        .ok_or(("step_invalid", "`steps` must hold 1–32 steps".to_string()))?;
    let is_remap = steps.first().and_then(|s| s.get("type")) == Some(&json!("remap"));
    if let Some(c) = trigger.single()
        && matches!(c.key, TKey::Modifier(_))
        && on != On::Release
        && !is_remap
    {
        return Err((
            "trigger_invalid",
            "a bare modifier fires a non-remap rule only with `on: release`".into(),
        ));
    }
    let mut ops = Vec::new();
    let mut remap = None;
    for (i, s) in steps.iter().enumerate() {
        let ty = s.get("type").and_then(Value::as_str).unwrap_or_default();
        match ty {
            "engine" => {
                let op = s
                    .get("op")
                    .and_then(Value::as_str)
                    .and_then(EngineOp::parse)
                    .ok_or(("step_invalid", format!("step {i}: unknown engine `op`")))?;
                ops.push(op);
            }
            "block" => {}
            "remap" => {
                if steps.len() != 1 || trigger.is_sequence() || on != On::Press || repeat {
                    return Err((
                        "step_invalid",
                        "a remap is the rule's only step, on a single chord pressed without repeat"
                            .into(),
                    ));
                }
                let out = |field: &str| -> Result<Option<Output>, (&'static str, String)> {
                    s.get(field)
                        .and_then(Value::as_str)
                        .map(|t| Output::parse(t).map_err(|e| ("step_invalid", e)))
                        .transpose()
                };
                let to = out("to")?.ok_or(("step_invalid", "a remap needs `to`".to_string()))?;
                let tap = out("tap")?;
                if tap.is_some() && pass_through {
                    return Err((
                        "step_invalid",
                        "a dual-role remap (`tap`) cannot pass the original key through".into(),
                    ));
                }
                let tap_ms = s.get("tap_ms").and_then(Value::as_u64).unwrap_or(200);
                remap = Some(RemapSpec {
                    to,
                    tap,
                    tap_ms: u32::try_from(tap_ms).unwrap_or(200).clamp(100, 1000),
                });
            }
            _ => {
                return Err((
                    "step_unsupported",
                    format!("`{ty}` steps are not implemented in this version yet"),
                ));
            }
        }
    }
    // Remaps and blocks are wildcards: a plain trigger matches with modifiers held (§3.2).
    let plain = trigger.single().is_some_and(|c| c.mods.is_empty());
    let (action, widen, injects) = match remap {
        Some(spec) => (Action::Remap(spec), plain, true),
        None if ops.is_empty() => (Action::Block, plain, false),
        None => (Action::Engine(ops), false, false),
    };
    Ok((
        Binding {
            rule: r["id"].as_str().unwrap_or_default().to_string(),
            trigger,
            on,
            repeat,
            pass_through,
            injected,
            widen,
            injects,
            action,
        },
        priority,
    ))
}

/// `rules` in status output: counts, codes and rule IDs (never rule content).
pub fn to_json(l: &Loaded) -> Value {
    json!({
        "file": l.state.as_str(),
        "problem": l.state.problem(),
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

/// How one rule stands with this file and this version (`rule list`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleState {
    pub profile: String,
    pub id: String,
    pub position: usize,
    /// `active`, `disabled`, `inactive`, `other_mode` or `other_profile`.
    pub state: &'static str,
    pub code: Option<&'static str>,
    pub reason: Option<String>,
}

/// The state of every rule of a usable document, in document order.
pub fn rule_states(l: &store::Loaded) -> Vec<RuleState> {
    let Some(doc) = &l.doc else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (pi, p) in doc.profiles.iter().enumerate() {
        let active_profile = p.id == doc.active_profile;
        for (ri, e) in p.rules.iter().enumerate() {
            let problem = l.report.inactive((pi, ri));
            let (state, code, reason) = if e.enabled() != Some(true) {
                ("disabled", None, None)
            } else if let Some(pr) = problem {
                ("inactive", Some(pr.code), Some(pr.message.clone()))
            } else if !active_profile {
                ("other_profile", None, None)
            } else if e
                .rule()
                .and_then(|r| r.modes.as_ref())
                .is_some_and(|m| !m.contains(&p.default_mode))
            {
                ("other_mode", None, None)
            } else {
                ("active", None, None)
            };
            out.push(RuleState {
                profile: p.id.clone(),
                id: e.id().unwrap_or_default().to_string(),
                position: ri,
                state,
                code,
                reason,
            });
        }
    }
    out
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
        let wrong = json!({"schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "x",
            "profiles": [{"id": "default", "title": "D", "default_mode": "base",
                          "modes": [{"id": "base", "title": "B"}], "rules": []}]})
        .to_string();
        let l = parse(wrong.as_bytes());
        assert!(
            matches!(&l.state, FileState::Invalid(w) if w.contains("active_profile")),
            "{:?}",
            l.state
        );
        // A schema violation outside the rules (no modes) is a whole-file problem.
        let no_modes = json!({"schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "default",
            "profiles": [{"id": "default", "title": "D", "default_mode": "base", "modes": [], "rules": []}]})
        .to_string();
        assert!(!parse(no_modes.as_bytes()).state.usable());
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
            engine_rule("dup", "Ctrl+Alt+Shift+F13", "toggle_pause"),
            engine_rule("pause-key", "F19", "pause")
        ])));
        assert_eq!(l.state, FileState::Loaded);
        assert_eq!(l.revision, Some(3));
        assert!(l.engine_on_start);
        assert_eq!(l.max_runs_per_second, 5);
        assert_eq!(l.total, 10);
        assert_eq!(l.disabled, 1);
        assert_eq!(l.other_modes, 1);
        assert_eq!(l.bindings.len(), 2);
        assert_eq!(l.bindings[0].rule, "pause-key");
        assert_eq!(
            l.bindings[0].action,
            Action::Engine(vec![EngineOp::TogglePause])
        );
        assert_eq!(l.bindings[1].rule, "caps");
        assert!(matches!(l.bindings[1].action, Action::Remap(_)));
        assert!(l.bindings[1].widen && l.bindings[1].injects);
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("future", "step_unsupported"),
                ("cond", "condition_unsupported"),
                ("newer-field", "rule_unsupported"),
                ("reserved", "trigger_reserved"),
                ("dup", "duplicate_trigger"),
                ("pause-key", "duplicate_rule_id"),
            ]
        );
        assert!(
            l.inactive[0].reason.contains("newer"),
            "{:?}",
            l.inactive[0]
        );
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
            assert_eq!(l.errors, 0, "{}", p.display());
            for b in &l.bindings {
                assert!(
                    matches!(
                        b.action,
                        Action::Engine(_) | Action::Remap(_) | Action::Block
                    ),
                    "{}: {}",
                    p.display(),
                    b.rule
                );
            }
        }
        assert!(seen >= 10);
    }

    #[test]
    fn rule_states_name_every_rule() {
        let mut nav = engine_rule("nav-only", "F18", "pause");
        nav["modes"] = json!(["nav"]);
        let mut off = engine_rule("off", "F17", "pause");
        off["enabled"] = json!(false);
        let bytes = doc(json!([
            engine_rule("ra", "F13", "pause"),
            nav,
            off,
            engine_rule("rb", "F13", "pause")
        ]));
        let l = store::interpret(&bytes, &Context::default(), MIGRATIONS);
        let states = rule_states(&l);
        let s: Vec<(&str, &str)> = states.iter().map(|r| (r.id.as_str(), r.state)).collect();
        assert_eq!(
            s,
            [
                ("ra", "active"),
                ("nav-only", "other_mode"),
                ("off", "disabled"),
                ("rb", "inactive")
            ]
        );
    }

    #[test]
    fn loading_and_validating_1000_rules_is_fast_enough() {
        // 1000 distinct triggers (two-chord sequences over F13–F24 with modifiers), as a large
        // but sensible file would have.
        let rules: Vec<Value> = (0..1000)
            .map(|i| {
                let keys = format!(
                    "{}F{} F{}",
                    [
                        "",
                        "Ctrl+",
                        "Alt+",
                        "Shift+",
                        "Ctrl+Shift+",
                        "Ctrl+Alt+",
                        "Alt+Shift+"
                    ][i % 7],
                    13 + (i / 7) % 12,
                    13 + (i / 84) % 12
                );
                engine_rule(&format!("r-{i}"), &keys, "pause")
            })
            .collect();
        let bytes = doc(Value::Array(rules));
        let t = std::time::Instant::now();
        let l = parse(&bytes);
        let took = t.elapsed();
        assert_eq!(l.total, 1000);
        eprintln!(
            "load + validate 1000 rules: {} ms ({} active)",
            took.as_millis(),
            l.bindings.len()
        );
        // DDALGAK-NFR-004 (< 100 ms with the hook) is measured on release builds; a debug build
        // is several times slower.
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    }
}
