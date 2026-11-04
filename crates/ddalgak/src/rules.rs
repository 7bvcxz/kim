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
//!   and `block` (§5.2; Step 68), and since Step 69 a single `type_text` (§5.4) or `send_keys`
//!   (§5.3) step, with a keys or a hotstring trigger (§3.4), and since Step 70 rules made only of
//!   `open_program` (§5.5) and `open_url` (§5.6) steps with a keys trigger, and since Step 71
//!   rules made only of `mouse_move`, `mouse_scroll` and `mouse_click` steps (§5.7–§5.9; a held
//!   button or a move while held as the only step), become bindings. Every other action type, a
//!   typing step among other steps, and launches or mouse steps mixed with other steps are
//!   inactive with `step_unsupported` until their Step (72, 78).
//! - Modes: there is no mode switching before Step 72, so the profile's `default_mode` is active;
//!   rules limited to other modes are counted, not run.
//! - Precedence (§7.1): mode-scoped rules first, then higher `priority`, then file order.

use crate::hotstring;
use crate::model::Rule;
use crate::remap::Output;
use crate::run::{KeysSpec, TextSpec};
use crate::store::{self, MIGRATIONS};
use crate::table::{Action, Binding, EngineOp, Injected, On, RemapSpec};
use crate::trigger::{TKey, Trigger};
use crate::validate::Context;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;

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

/// The step types this version runs (Step 66: `engine`; Step 68: `remap`, `block`; Step 69:
/// `send_keys`, `type_text`; Step 70: `open_program`, `open_url`; Step 71: `mouse_move`,
/// `mouse_scroll`, `mouse_click`). A rule made of `engine` and `block` steps is an engine-control
/// rule (a `block` step adds nothing to a rule that consumes its key anyway); a `remap`, a held
/// button and a move while held are always the rule's only step (schema `holds_trigger`); a
/// typing step runs alone until ordered multi-step runs (Step 78); launch steps run together, in
/// order, without other steps, and so do mouse steps.
pub const RUNNABLE_STEPS: [&str; 10] = [
    "engine",
    "remap",
    "block",
    "send_keys",
    "type_text",
    "open_program",
    "open_url",
    "mouse_move",
    "mouse_scroll",
    "mouse_click",
];

const TYPING_ALONE: &str = "a `type_text` or `send_keys` step runs alone in this version; ordered multi-step runs arrive in Step 78";
const HOTSTRING_STEPS: &str = "a hotstring runs one `type_text` or `send_keys` step in this version; other steps after a hotstring arrive with ordered multi-step runs (Step 78)";
const LAUNCHES_ALONE: &str = "`open_program` and `open_url` steps run together, in order, in this version; mixed with other steps (delays, keys, modes) they wait for ordered multi-step runs (Step 78)";

const MOUSE_ALONE: &str = "`mouse_move`, `mouse_scroll` and `mouse_click` steps run together, in order, in this version; mixed with other steps (delays, keys, launches, modes) they wait for ordered multi-step runs (Step 78)";

fn is_launch(kind: &str) -> bool {
    matches!(kind, "open_program" | "open_url")
}

fn is_mouse(kind: &str) -> bool {
    matches!(kind, "mouse_move" | "mouse_scroll" | "mouse_click")
}

/// A combination of runnable steps this version does not run yet (`step_unsupported`): the step
/// index and why.
pub fn later_step(rule: &Rule) -> Option<(usize, String)> {
    use crate::model::Action as A;
    let typing = |a: &A| matches!(a, A::TypeText(_) | A::SendKeys(_));
    if rule.trigger.hotstring.is_some() {
        if let Some(i) = rule.steps.iter().position(|s| !typing(s)) {
            return Some((i, HOTSTRING_STEPS.into()));
        }
        if rule.steps.len() > 1 {
            return Some((1, HOTSTRING_STEPS.into()));
        }
    }
    if rule.steps.len() > 1
        && let Some(i) = rule.steps.iter().position(typing)
    {
        return Some((i, TYPING_ALONE.into()));
    }
    if rule.steps.iter().any(|s| is_launch(s.kind()))
        && let Some(i) = rule.steps.iter().position(|s| !is_launch(s.kind()))
    {
        return Some((i, LAUNCHES_ALONE.into()));
    }
    if rule.steps.iter().any(|s| is_mouse(s.kind()))
        && let Some(i) = rule.steps.iter().position(|s| !is_mouse(s.kind()))
    {
        return Some((i, MOUSE_ALONE.into()));
    }
    None
}

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
    "hotstring",
    "on",
    "hold_ms",
    "repeat",
    "pass_through",
    "injected",
];

const HOTSTRING_FIELDS: &[&str] = &[
    "abbreviation",
    "case",
    "inside_words",
    "immediate",
    "end_chars",
    "omit_end",
    "erase",
];

/// Compiles `trigger.hotstring` (§3.4).
fn compile_hotstring(h: &Value) -> Result<hotstring::Spec, (&'static str, String)> {
    let o = h.as_object().ok_or((
        "trigger_invalid",
        "`trigger.hotstring` must be an object".to_string(),
    ))?;
    if let Some(f) = unknown_field(o, HOTSTRING_FIELDS) {
        return Err(("rule_unsupported", format!("unknown hotstring field `{f}`")));
    }
    let bad = |what: &str| {
        (
            "trigger_invalid",
            format!("`trigger.hotstring.{what}` has a wrong type or value"),
        )
    };
    let abbreviation = o
        .get("abbreviation")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("abbreviation"))?;
    let len = abbreviation.chars().count();
    if !(1..=hotstring::MAX_ABBREVIATION).contains(&len)
        || abbreviation
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad("abbreviation"));
    }
    let flag = |k: &str, default: bool| -> Result<bool, (&'static str, String)> {
        match o.get(k) {
            None => Ok(default),
            Some(v) => v.as_bool().ok_or_else(|| bad(k)),
        }
    };
    let mut spec = hotstring::Spec::new(abbreviation);
    spec.case = match o
        .get("case")
        .map(|v| v.as_str().and_then(hotstring::Case::parse))
    {
        None => hotstring::Case::Conform,
        Some(Some(c)) => c,
        Some(None) => return Err(bad("case")),
    };
    spec.inside_words = flag("inside_words", false)?;
    spec.immediate = flag("immediate", false)?;
    spec.omit_end = flag("omit_end", false)?;
    spec.erase = flag("erase", true)?;
    if let Some(e) = o.get("end_chars") {
        let e = e
            .as_str()
            .filter(|e| !e.is_empty())
            .ok_or_else(|| bad("end_chars"))?;
        spec.end_chars = e.chars().collect();
    }
    Ok(spec)
}

/// Compiles a typing step (`type_text`, `send_keys`).
fn compile_run(s: &Value, ty: &str) -> Result<Action, (&'static str, String)> {
    if ty == "type_text" {
        let text = s.get("text").and_then(Value::as_str).ok_or((
            "step_invalid",
            "a `type_text` step needs `text`".to_string(),
        ))?;
        return Ok(Action::Type(Arc::new(TextSpec {
            text: text.to_string(),
            paste: s.get("method").and_then(Value::as_str) == Some("paste"),
            key_delay_ms: s
                .get("key_delay_ms")
                .and_then(Value::as_u64)
                .map_or(0, |d| u32::try_from(d.min(200)).unwrap_or(0)),
            variables: s.get("variables").and_then(Value::as_bool) == Some(true),
        })));
    }
    let keys = s.get("keys").and_then(Value::as_str).ok_or((
        "step_invalid",
        "a `send_keys` step needs `keys`".to_string(),
    ))?;
    let chords = keys
        .split(' ')
        .map(|k| match Output::parse(k) {
            Ok(o) if matches!(o.key, Some(crate::inject::OutCode::Button(_))) => Err((
                "step_invalid",
                format!("`{k}`: send_keys presses keys, not mouse buttons"),
            )),
            Ok(o) => Ok(o),
            Err(e) => Err(("step_invalid", e)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Action::Keys(Arc::new(KeysSpec {
        chords,
        step_delay_ms: s
            .get("step_delay_ms")
            .and_then(Value::as_u64)
            .map_or(0, |d| u32::try_from(d.min(1000)).unwrap_or(0)),
    })))
}

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
    let hotstring = t.get("hotstring").map(compile_hotstring).transpose()?;
    let trigger = match (t.get("keys"), &hotstring) {
        (Some(k), None) => {
            let k = k
                .as_str()
                .ok_or(("trigger_invalid", "`trigger.keys` must be text".to_string()))?;
            Trigger::parse(k).map_err(|e| (e.code(), e.to_string()))?
        }
        (None, Some(_)) => Trigger::none(),
        _ => {
            return Err((
                "trigger_invalid",
                "a trigger has `keys` or a `hotstring`, not both".into(),
            ));
        }
    };
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
    let kinds: Vec<&str> = steps
        .iter()
        .map(|s| s.get("type").and_then(Value::as_str).unwrap_or_default())
        .collect();
    let typing = |k: &&str| matches!(*k, "type_text" | "send_keys");
    if hotstring.is_some() && (kinds.len() != 1 || !kinds.iter().all(typing)) {
        return Err(("step_unsupported", HOTSTRING_STEPS.into()));
    }
    if kinds.len() > 1 && kinds.iter().any(typing) {
        return Err(("step_unsupported", TYPING_ALONE.into()));
    }
    if kinds.iter().any(|k| is_launch(k)) && !kinds.iter().all(|k| is_launch(k)) {
        return Err(("step_unsupported", LAUNCHES_ALONE.into()));
    }
    if kinds.iter().any(|k| is_mouse(k)) && !kinds.iter().all(|k| is_mouse(k)) {
        return Err(("step_unsupported", MOUSE_ALONE.into()));
    }
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
    let mut run = None;
    let mut targets = Vec::new();
    let mut mouse = Vec::new();
    let mut held = None;
    // Steps that last while the trigger is held: the rule's only step, on a single chord pressed
    // without repeat or pass-through (schema `holds_trigger`; checked again here).
    let holds_ok =
        steps.len() == 1 && !trigger.is_sequence() && on == On::Press && !repeat && !pass_through;
    for (i, s) in steps.iter().enumerate() {
        let ty = s.get("type").and_then(Value::as_str).unwrap_or_default();
        match ty {
            "mouse_move" | "mouse_scroll" | "mouse_click" => {
                match crate::mouse::compile_step(s)
                    .map_err(|e| ("step_invalid", format!("step {i}: {e}")))?
                {
                    crate::mouse::Compiled::Run(m) => mouse.push(m),
                    h if holds_ok => held = Some(h),
                    _ => {
                        return Err((
                            "step_invalid",
                            "a held button or a move while held is the rule's only step, on a single chord pressed without repeat or pass-through"
                                .into(),
                        ));
                    }
                }
            }
            "open_program" | "open_url" => targets.push(
                crate::launch::compile_step(s)
                    .map_err(|e| ("step_invalid", format!("step {i}: {e}")))?,
            ),
            "engine" => {
                let op = s
                    .get("op")
                    .and_then(Value::as_str)
                    .and_then(EngineOp::parse)
                    .ok_or(("step_invalid", format!("step {i}: unknown engine `op`")))?;
                ops.push(op);
            }
            "block" => {}
            "type_text" | "send_keys" => run = Some(compile_run(s, ty)?),
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
    let (action, widen, injects) = match (remap, run) {
        // Mouse steps inject; they match exactly (no widening, §3.2).
        _ if !mouse.is_empty() => (
            Action::Mouse(Arc::new(crate::mouse::MousePlan {
                steps: mouse,
                stop_on_error: r.get("on_error").and_then(Value::as_str) != Some("continue"),
            })),
            false,
            true,
        ),
        _ if held.is_some() => match held {
            Some(crate::mouse::Compiled::Hold { button, at }) => (
                Action::MouseHold {
                    spec: RemapSpec {
                        to: Output::button(button),
                        tap: None,
                        tap_ms: 200,
                    },
                    at,
                },
                false,
                true,
            ),
            Some(crate::mouse::Compiled::Motion(m)) => (Action::Motion(m), false, true),
            _ => return Err(("step_invalid", "not a held mouse step".into())),
        },
        // Launches inject nothing: they still run while an elevated window has focus (§3).
        _ if !targets.is_empty() => (
            Action::Launch(Arc::new(crate::launch::Plan {
                targets,
                stop_on_error: r.get("on_error").and_then(Value::as_str) != Some("continue"),
            })),
            false,
            false,
        ),
        (Some(spec), _) => (Action::Remap(spec), plain, true),
        (None, Some(a)) => (a, false, true),
        (None, None) if ops.is_empty() => (Action::Block, plain, false),
        (None, None) => (Action::Engine(ops), false, false),
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
            hotstring: hotstring.map(Arc::new),
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

    /// A launch rule with `launch-programs` consent for exactly its targets.
    #[allow(clippy::needless_pass_by_value)]
    fn launch_rule(id: &str, keys: &str, steps: Value) -> Value {
        let mut r = json!({ "id": id, "title": id, "enabled": true, "trigger": { "keys": keys },
                            "steps": steps });
        let fp = crate::validate::launch_fingerprint(&r);
        r["consent"] = json!({ "launch-programs": {
            "granted_at": "2026-11-04T09:00:00Z", "via": "cli", "fingerprint": fp } });
        r
    }

    #[test]
    fn launch_rules_compile_and_changed_or_unconsented_targets_stay_inactive() {
        use crate::launch::{Show, Target};
        // The target changed after consent (Step 65's fingerprint check): inactive.
        let mut stale = launch_rule(
            "stale",
            "F15",
            json!([{ "type": "open_url", "url": "https://example.com/" }]),
        );
        stale["steps"][0]["url"] = json!("https://other.example/");
        // An environment value changed after consent: inactive as well.
        let mut env_changed = launch_rule(
            "env-changed",
            "F20",
            json!([{ "type": "open_program", "path": r"C:\Tools\t.exe",
                     "env": [{ "name": "MODE", "value": "safe" }] }]),
        );
        env_changed["steps"][0]["env"][0]["value"] = json!("unsafe");
        // An imported rule: disabled and without consent, it never runs.
        let imported = json!({ "id": "imported", "title": "imported", "enabled": false,
            "trigger": { "keys": "F16" },
            "steps": [{ "type": "open_program", "path": r"D:\Tools\tool.exe" }] });
        let l = parse(&doc(json!([
            launch_rule(
                "notepad",
                "F13",
                json!([{ "type": "open_program", "path": r"%SystemRoot%\System32\notepad.exe",
                         "args": ["a b"], "window": "minimized", "if_running": "activate",
                         "env": [{ "name": "K", "value": "v" }] }])
            ),
            launch_rule(
                "two",
                "F14",
                json!([{ "type": "open_url", "url": "https://github.com/" },
                       { "type": "open_program", "app_id": "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App" }])
            ),
            stale,
            imported,
            launch_rule(
                "mixed",
                "F17",
                json!([{ "type": "open_url", "url": "https://github.com/" }, { "type": "delay", "ms": 100 }])
            ),
            launch_rule(
                "colon",
                "F18",
                json!([{ "type": "open_program", "path": r"C:\notes.txt:hidden.exe" }])
            ),
            launch_rule(
                "dup-env",
                "F19",
                json!([{ "type": "open_program", "path": r"C:\x.exe",
                         "env": [{ "name": "A", "value": "1" }, { "name": "a", "value": "2" }] }])
            ),
            env_changed,
        ])));
        let active: Vec<&str> = l.bindings.iter().map(|b| b.rule.as_str()).collect();
        assert_eq!(active, ["notepad", "two"]);
        let Action::Launch(p) = &l.bindings[0].action else {
            panic!("{:?}", l.bindings[0].action)
        };
        let Target::Program(s) = &p.targets[0] else {
            panic!("{p:?}")
        };
        assert_eq!((s.show, s.activate), (Show::Minimized, true));
        assert_eq!(s.env, [("K".to_string(), "v".to_string())]);
        assert!(
            !l.bindings[0].injects && !l.bindings[0].widen,
            "launches inject nothing"
        );
        let Action::Launch(p) = &l.bindings[1].action else {
            panic!()
        };
        assert_eq!(p.targets.len(), 2);
        assert!(p.stop_on_error);
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("stale", "consent_stale"),
                ("mixed", "step_unsupported"),
                ("colon", "step_invalid"),
                ("dup-env", "step_invalid"),
                ("env-changed", "consent_stale"),
            ]
        );
        assert_eq!(l.disabled, 1);
        for i in &l.inactive {
            assert!(
                !i.reason.contains("notes.txt") && !i.reason.contains("other.example"),
                "reasons never quote targets: {i:?}"
            );
        }
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
    fn mouse_rules_compile_and_mixed_or_misplaced_holds_stay_inactive() {
        use crate::inject::Button;
        use crate::mouse::{MotionKind, MouseStep, Unit};
        let r = |id: &str, keys: &str, steps: Value| {
            json!({ "id": id, "title": id, "enabled": true, "trigger": { "keys": keys },
                    "steps": steps,
                    "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" } } })
        };
        let mut repeat_hold = r(
            "repeat-hold",
            "F20",
            json!([{"type":"mouse_click","button":"left","op":"hold"}]),
        );
        repeat_hold["trigger"]["repeat"] = json!(true);
        let l = parse(&doc(json!([
            r(
                "plan",
                "F13",
                json!([{"type":"mouse_move","mode":"absolute","to":{"x":1,"y":2}},
                       {"type":"mouse_click","button":"left","op":"double_click"}])
            ),
            r(
                "drag",
                "Ctrl+F14",
                json!([{"type":"mouse_click","button":"x2","op":"hold"}])
            ),
            r(
                "glide",
                "F15",
                json!([{"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":600}}])
            ),
            r(
                "cells",
                "F16",
                json!([{"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid",
                        "grid":{"columns":8,"rows":8},"while_held":{"repeat_ms":100}}])
            ),
            r(
                "wheel",
                "F17",
                json!([{"type":"mouse_scroll","direction":"right","notches":3}])
            ),
            r(
                "mixed",
                "F18",
                json!([{"type":"mouse_scroll","direction":"down"},{"type":"engine","op":"pause"}])
            ),
            r(
                "grid-glide",
                "F19",
                json!([{"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid",
                        "grid":{"columns":8,"rows":8},"while_held":{"speed":600}}])
            ),
            repeat_hold,
        ])));
        let active: Vec<&str> = l.bindings.iter().map(|b| b.rule.as_str()).collect();
        assert_eq!(active, ["plan", "drag", "glide", "cells", "wheel"]);
        assert!(l.bindings.iter().all(|b| b.injects && !b.widen));
        let Action::Mouse(p) = &l.bindings[0].action else {
            panic!("{:?}", l.bindings[0].action)
        };
        assert_eq!(p.steps.len(), 2);
        assert!(p.stop_on_error);
        assert!(matches!(
            p.steps[1],
            MouseStep::Click {
                button: Button::Left,
                double: true,
                at: None
            }
        ));
        let Action::MouseHold { spec, at: None } = &l.bindings[1].action else {
            panic!("{:?}", l.bindings[1].action)
        };
        assert_eq!(spec.to, Output::button(Button::X2));
        assert!(matches!(
            l.bindings[2].action,
            Action::Motion(m) if matches!(m.kind, MotionKind::Continuous { speed: 600, acceleration: 0 })
        ));
        assert!(matches!(
            l.bindings[3].action,
            Action::Motion(m) if matches!(m.unit, Unit::Grid(_)) && matches!(m.kind, MotionKind::Stepped { repeat_ms: 100, .. })
        ));
        // The compiled table knows which bindings hold a button or a motion.
        let t = crate::table::Table::compile(l.bindings.clone(), 20);
        assert!(t.remap(1).is_some() && t.remap(2).is_none());
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("mixed", "step_unsupported"),
                ("grid-glide", "step_invalid"),
                ("repeat-hold", "step_invalid"),
            ]
        );
        assert!(
            l.inactive[0].reason.contains("Step 78"),
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
                        Action::Engine(_)
                            | Action::Remap(_)
                            | Action::Block
                            | Action::Type(_)
                            | Action::Keys(_)
                            | Action::Launch(_)
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

    #[allow(clippy::needless_pass_by_value)]
    fn typing_rule(id: &str, trigger: Value, steps: Value) -> Value {
        json!({ "id": id, "title": id, "enabled": true, "trigger": trigger, "steps": steps,
                "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" },
                             "clipboard": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" } } })
    }

    #[test]
    fn typing_rules_and_hotstrings_compile_and_combinations_wait_for_step_78() {
        let l = parse(&doc(json!([
            typing_rule(
                "sign",
                json!({ "keys": "Ctrl+Shift+F2" }),
                json!([{ "type": "type_text", "text": "감사합니다\n", "method": "paste", "key_delay_ms": 5 }])
            ),
            typing_rule(
                "btw",
                json!({ "hotstring": { "abbreviation": "btw", "case": "sensitive",
                    "inside_words": true, "end_chars": " .", "omit_end": true, "erase": false } }),
                json!([{ "type": "type_text", "text": "by the way" }])
            ),
            typing_rule(
                "ac",
                json!({ "hotstring": { "abbreviation": ";ac", "immediate": true } }),
                json!([{ "type": "send_keys", "keys": "Ctrl+A Ctrl+C", "step_delay_ms": 40 }])
            ),
            typing_rule(
                "two",
                json!({ "keys": "F14" }),
                json!([{ "type": "type_text", "text": "a" }, { "type": "send_keys", "keys": "Enter" }])
            ),
            typing_rule(
                "hs-engine",
                json!({ "hotstring": { "abbreviation": ";p" } }),
                json!([{ "type": "engine", "op": "pause" }])
            ),
            typing_rule(
                "future-hs",
                json!({ "hotstring": { "abbreviation": ";f", "fuzzy": true } }),
                json!([{ "type": "type_text", "text": "f" }])
            )
        ])));
        let active: Vec<&str> = l.bindings.iter().map(|b| b.rule.as_str()).collect();
        assert_eq!(active, ["sign", "btw", "ac"]);
        let Action::Type(t) = &l.bindings[0].action else {
            panic!("{:?}", l.bindings[0].action)
        };
        assert!(t.paste && t.key_delay_ms == 5 && !t.variables);
        assert!(l.bindings[0].injects && !l.bindings[0].widen && l.bindings[0].hotstring.is_none());
        let h = l.bindings[1].hotstring.as_ref().unwrap();
        assert_eq!(h.abbreviation, ['b', 't', 'w']);
        assert_eq!(h.case, hotstring::Case::Sensitive);
        assert!(h.inside_words && h.omit_end && !h.erase && !h.immediate);
        assert_eq!(h.end_chars, [' ', '.']);
        assert!(l.bindings[1].trigger.steps().is_empty());
        let Action::Keys(k) = &l.bindings[2].action else {
            panic!()
        };
        assert_eq!((k.chords.len(), k.step_delay_ms), (2, 40));
        assert!(l.bindings[2].hotstring.as_ref().unwrap().immediate);
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [
                ("two", "step_unsupported"),
                ("hs-engine", "step_unsupported"),
                ("future-hs", "rule_unsupported")
            ]
        );
        assert!(
            l.inactive[0].reason.contains("Step 78"),
            "{:?}",
            l.inactive[0]
        );
        // The compiled table knows its hotstrings and bounds the core's buffer by them.
        let table = crate::table::Table::compile(l.bindings.clone(), 20);
        assert!(table.has_hotstrings());
        assert_eq!(table.longest_hotstring, 3);
    }

    #[test]
    fn duplicate_hotstrings_and_slow_typing_are_semantic_errors() {
        let tt = |t: &str| json!([{ "type": "type_text", "text": t }]);
        let mut sensitive = typing_rule(
            "btw-exact",
            json!({ "hotstring": { "abbreviation": "BTW", "case": "sensitive" } }),
            tt("BY THE WAY"),
        );
        sensitive["priority"] = json!(0);
        let l = parse(&doc(json!([
            typing_rule(
                "btw",
                json!({ "hotstring": { "abbreviation": "btw" } }),
                tt("by the way")
            ),
            typing_rule(
                "btw2",
                json!({ "hotstring": { "abbreviation": "Btw" } }),
                tt("x")
            ),
            sensitive,
            typing_rule(
                "slow",
                json!({ "keys": "F13" }),
                json!([{ "type": "type_text", "text": "y".repeat(400), "key_delay_ms": 200 }])
            )
        ])));
        let codes: Vec<(&str, &str)> = l
            .inactive
            .iter()
            .map(|i| (i.rule.as_str(), i.code))
            .collect();
        assert_eq!(
            codes,
            [("btw2", "duplicate_trigger"), ("slow", "run_too_long")],
            "a case-sensitive abbreviation is its own trigger"
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
