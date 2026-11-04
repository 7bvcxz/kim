//! Rule CRUD on a [`Document`] (docs/DDALGAK_RULE_MODEL.md §4, §6.3, §8, §13;
//! `DDALGAK-FR-007`, `DDALGAK-SEC-006`).
//!
//! Every operation is a pure change of an in-memory document. Callers (the CLI, later the GUI)
//! load the document under the store's lock, apply one operation, and then call [`review`],
//! which compares the validation reports before and after: **a change that introduces an error
//! is refused and nothing is written** (`rule_conflict` for a trigger another rule already has,
//! `hotkey_invalid`/`hotkey_reserved` for bad or reserved triggers, `rule_invalid` for anything
//! else). Warnings never block. Existing problems elsewhere do not block an unrelated change.
//!
//! Consent (§6.3): `--allow <permission>` records a grant (`via: "cli"`, with the launch
//! fingerprint for `launch-programs`). A new rule that needs consent it was not given is saved
//! **disabled** and the command reports `consent_required`; editing or enabling a rule into a
//! state that needs missing consent is refused. A revoked permission disables the rule. Copies
//! keep consent only because their targets are unchanged, and are created disabled.

use crate::launch;
use crate::model::{
    Action, ClickOp, Delay, Direction, Document, EngineOp, EngineStep, EnvVar, Grant, GridSize,
    IfRunning, ModeOp, ModeStep, MouseClick, MouseMove, MouseScroll, MoveMode, MoveUnit,
    OpenProgram, OpenUrl, Permission, Point, RelativeTo, Remap, Rule, RuleEntry, SendKeys,
    TextMethod, TypeText, WhileHeld, WindowState, is_rule_id, new_rule_id,
};
use crate::trigger::{self, TriggerError};
use crate::validate::{self, Problem, Report, Severity};
use kim_hotkeys::keys::Sequence;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

/// Why an operation was refused. Nothing was changed.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    Usage(String),
    RuleNotFound(String),
    ProfileNotFound(String),
    ModeNotFound(String),
    /// `hotkey_invalid`, `hotkey_reserved` or `rule_invalid`, with the problems.
    Invalid {
        code: &'static str,
        message: String,
        problems: Vec<Problem>,
    },
    /// `rule_conflict`: the change collides with another rule (or an ID is taken).
    Conflict {
        message: String,
        problems: Vec<Problem>,
    },
    /// `consent_required`: the change needs `--allow` for these permissions.
    ConsentRequired {
        rule: String,
        permissions: Vec<Permission>,
    },
}

impl EditError {
    /// The CLI error code (docs/CLI_CONTRACT.md §6).
    pub fn code(&self) -> &'static str {
        match self {
            EditError::Usage(_) => "usage",
            EditError::RuleNotFound(_) => "rule_not_found",
            EditError::ProfileNotFound(_) => "profile_not_found",
            EditError::ModeNotFound(_) => "mode_not_found",
            EditError::Invalid { code, .. } => code,
            EditError::Conflict { .. } => "rule_conflict",
            EditError::ConsentRequired { .. } => "consent_required",
        }
    }

    pub fn message(&self) -> String {
        match self {
            EditError::Usage(m) | EditError::Invalid { message: m, .. } => m.clone(),
            EditError::Conflict { message, .. } => message.clone(),
            EditError::RuleNotFound(id) => format!("no rule `{id}`"),
            EditError::ProfileNotFound(id) => format!("no profile `{id}`"),
            EditError::ModeNotFound(id) => format!("no mode `{id}` in the profile"),
            EditError::ConsentRequired { rule, permissions } => format!(
                "rule `{rule}` needs consent for {}; nothing was enabled",
                names(permissions)
            ),
        }
    }

    pub fn problems(&self) -> &[Problem] {
        match self {
            EditError::Invalid { problems, .. } | EditError::Conflict { problems, .. } => problems,
            _ => &[],
        }
    }
}

fn names(p: &[Permission]) -> String {
    p.iter()
        .map(|p| format!("`{}`", p.as_str()))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// What an operation changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Change {
    /// The rules it touched, in order.
    pub rules: Vec<String>,
    /// A rule it created.
    pub created: Option<String>,
    /// Whether the document differs from before.
    pub changed: bool,
    /// Permissions a new rule still needs: it was saved disabled (`consent_required`).
    pub consent_needed: Vec<Permission>,
    /// Plain notes for the user ("disabled because …").
    pub notes: Vec<String>,
}

/// Where a rule goes in its profile's list (the list order is the last precedence step, §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Position {
    #[default]
    End,
    Start,
    Before(String),
    After(String),
    /// A zero-based index (clamped to the list).
    Index(usize),
}

/// Canonical trigger text from user input: the hotkey contract's aliases (`ctrl+shift+j`,
/// `Esc`), Ddalgak's lock keys and bare side-specific modifiers (§3.1). Reserved or invalid
/// triggers are refused with their code.
pub fn canonical_trigger(input: &str) -> Result<String, TriggerError> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.is_empty() {
        return Err(TriggerError::Invalid("no keys given".into()));
    }
    let mut out = Vec::new();
    for p in &parts {
        out.push(canonical_step(p, false)?);
    }
    let text = out.join(" ");
    trigger::Trigger::parse(&text)?;
    Ok(text)
}

/// Canonical output text (`remap` target, `send_keys`): one to eight chords; bare modifiers
/// including `LWin`/`RWin` and `Win` chords are allowed (§5.1, §5.3). Mouse buttons are remap
/// outputs only (`max == 1`); `send_keys` presses keys.
pub fn canonical_output(input: &str, max: usize) -> Result<String, String> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.is_empty() || parts.len() > max {
        return Err(format!("`{input}`: give one to {max} keys or chords"));
    }
    let mut out = Vec::new();
    for p in parts {
        let step = canonical_step(p, true).map_err(|e| e.to_string())?;
        if max > 1 && MOUSE.iter().any(|(n, _)| step.ends_with(n)) {
            return Err(format!(
                "`{input}`: mouse buttons are remap targets; send_keys presses keys only"
            ));
        }
        if max == 1 {
            crate::remap::Output::parse(&step)?;
        }
        out.push(step);
    }
    Ok(out.join(" "))
}

const BARE: [&str; 8] = [
    "LCtrl", "RCtrl", "LAlt", "RAlt", "LShift", "RShift", "LWin", "RWin",
];
const LOCKS: [&str; 2] = ["CapsLock", "NumLock"];

/// Mouse button names with the AutoHotkey-style aliases the CLI also accepts (Step 68). Triggers
/// may use only the middle and X buttons; outputs may use all five.
const MOUSE: [(&str, &[&str]); 5] = [
    ("MouseLeft", &["lbutton"]),
    ("MouseRight", &["rbutton"]),
    ("MouseMiddle", &["mbutton"]),
    ("MouseX1", &["xbutton1"]),
    ("MouseX2", &["xbutton2"]),
];

/// Canonical text of a step that ends in a mouse button, if it does.
fn canonical_mouse(step: &str) -> Option<Result<String, TriggerError>> {
    let lower = step.to_ascii_lowercase();
    let (prefix, name) = MOUSE.iter().find_map(|(name, aliases)| {
        std::iter::once(name.to_ascii_lowercase())
            .chain(aliases.iter().map(|a| (*a).to_string()))
            .find(|n| lower == *n || lower.ends_with(&format!("+{n}")))
            .map(|n| (&step[..step.len() - n.len()], *name))
    })?;
    if prefix.is_empty() {
        return Some(Ok(name.to_string()));
    }
    Some(
        Sequence::parse_input(&format!("{prefix}F1"))
            .map_err(|e| TriggerError::Invalid(e.message))
            .map(|seq| {
                format!(
                    "{}{name}",
                    seq.to_string().strip_suffix("F1").unwrap_or_default()
                )
            }),
    )
}

fn canonical_step(step: &str, output: bool) -> Result<String, TriggerError> {
    if let Some(b) = BARE.iter().find(|b| b.eq_ignore_ascii_case(step)) {
        if !output && b.ends_with("Win") {
            return Err(TriggerError::Reserved("Windows key shortcut"));
        }
        return Ok((*b).to_string());
    }
    let lower = step.to_ascii_lowercase();
    if let Some(lock) = LOCKS.iter().find(|l| {
        lower == l.to_ascii_lowercase() || lower.ends_with(&format!("+{}", l.to_ascii_lowercase()))
    }) {
        let prefix = &step[..step.len() - lock.len()];
        if prefix.is_empty() {
            return Ok((*lock).to_string());
        }
        let shown = Sequence::parse_input(&format!("{prefix}F1"))
            .map_err(|e| TriggerError::Invalid(e.message))?
            .to_string();
        return Ok(format!(
            "{}{lock}",
            shown.strip_suffix("F1").unwrap_or_default()
        ));
    }
    if let Some(m) = canonical_mouse(step) {
        return m;
    }
    let seq = Sequence::parse_input(step).map_err(|e| TriggerError::Invalid(e.message))?;
    if seq.len() != 1 {
        return Err(TriggerError::Invalid(format!(
            "`{step}`: one chord expected"
        )));
    }
    Ok(seq.to_string())
}

/// The step inputs the CLI offers as flags (`--action <type>` with `--to`, `--keys`, …; since
/// Step 71 the mouse flags `--dx`, `--dy`, `--x`, `--y`, `--relative-to`, `--grid`, `--speed`,
/// `--acceleration`, `--repeat-ms`, `--delay-ms`, `--direction`, `--notches`, `--button`).
/// Anything richer is given as JSON (`--step`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepInputs {
    pub to: Option<String>,
    pub tap: Option<String>,
    pub tap_ms: Option<u32>,
    pub keys: Option<String>,
    pub step_delay_ms: Option<u32>,
    pub text: Option<String>,
    pub method: Option<String>,
    pub key_delay_ms: Option<u32>,
    /// `--variables`: the text is a template (date and time variables, `{cursor}`).
    pub variables: bool,
    pub path: Option<String>,
    pub app_id: Option<String>,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    /// `--env NAME=VALUE`: explicit environment overrides for the program (Step 70).
    pub env: Vec<(String, String)>,
    pub window: Option<String>,
    pub if_running: Option<String>,
    pub url: Option<String>,
    pub op: Option<String>,
    pub target_mode: Option<String>,
    pub target_modes: Vec<String>,
    pub ms: Option<u32>,
    /// `mouse_move` by a distance (logical pixels or grid cells).
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    /// `mouse_move` to a point, or `mouse_click` at one.
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub relative_to: Option<String>,
    /// `--grid COLSxROWS`: dx/dy count cells of this grid.
    pub grid: Option<String>,
    pub speed: Option<u32>,
    pub acceleration: Option<u32>,
    pub repeat_ms: Option<u32>,
    pub delay_ms: Option<u32>,
    pub direction: Option<String>,
    pub notches: Option<u32>,
    pub button: Option<String>,
}

/// The point of `--x`/`--y` (with `--relative-to`), if given.
fn mouse_point(i: &StepInputs, kind: &str) -> Result<Option<Point>, String> {
    match (i.x, i.y) {
        (None, None) => {
            if i.relative_to.is_some() {
                return Err(format!(
                    "`--action {kind}`: --relative-to needs --x and --y"
                ));
            }
            Ok(None)
        }
        (Some(x), Some(y)) => Ok(Some(Point {
            x,
            y,
            relative_to: parse("point anchor", &i.relative_to, RelativeTo::parse)?,
            extra: Map::new(),
        })),
        _ => Err(format!("`--action {kind}`: give both --x and --y")),
    }
}

/// `--grid 16x9`.
fn grid_size(g: &str) -> Result<GridSize, String> {
    let bad = || format!("--grid takes COLUMNSxROWS (2–64 each, like 16x9), not `{g}`");
    let (c, r) = g.split_once(['x', 'X', '×']).ok_or_else(bad)?;
    let n = |v: &str| {
        v.trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (2..=64).contains(n))
    };
    Ok(GridSize {
        columns: n(c).ok_or_else(bad)?,
        rows: n(r).ok_or_else(bad)?,
        extra: Map::new(),
    })
}

/// `--action mouse_move`: `--dx/--dy` (relative; `--grid`, and `--speed`/`--acceleration` or
/// `--repeat-ms`/`--delay-ms` while held) or `--x/--y` (absolute, `--relative-to`).
fn mouse_move(i: &StepInputs) -> Result<Action, String> {
    let relative = i.dx.is_some() || i.dy.is_some();
    let held = i.speed.is_some()
        || i.acceleration.is_some()
        || i.repeat_ms.is_some()
        || i.delay_ms.is_some();
    let to = mouse_point(i, "mouse_move")?;
    let extra = Map::new;
    match (relative, to) {
        (true, Some(_)) => {
            Err("`--action mouse_move` moves by --dx/--dy or to --x/--y, not both".into())
        }
        (false, None) => Err(
            "`--action mouse_move` needs --dx/--dy (by a distance) or --x/--y (to a point)".into(),
        ),
        (false, Some(p)) => {
            if held || i.grid.is_some() {
                return Err("--speed, --repeat-ms and --grid are for moves by --dx/--dy".into());
            }
            Ok(Action::MouseMove(MouseMove {
                mode: MoveMode::Absolute,
                dx: None,
                dy: None,
                to: Some(p),
                unit: None,
                grid: None,
                while_held: None,
                extra: extra(),
            }))
        }
        (true, None) => {
            if i.speed.is_some() && (i.repeat_ms.is_some() || i.delay_ms.is_some()) {
                return Err("glide with --speed or step with --repeat-ms, not both".into());
            }
            if i.acceleration.is_some() && i.speed.is_none() {
                return Err("--acceleration goes with --speed".into());
            }
            if i.delay_ms.is_some() && i.repeat_ms.is_none() {
                return Err("--delay-ms goes with --repeat-ms".into());
            }
            let grid = i.grid.as_deref().map(grid_size).transpose()?;
            Ok(Action::MouseMove(MouseMove {
                mode: MoveMode::Relative,
                dx: Some(i.dx.unwrap_or(0)),
                dy: Some(i.dy.unwrap_or(0)),
                to: None,
                unit: grid.is_some().then_some(MoveUnit::Grid),
                grid,
                while_held: held.then(|| WhileHeld {
                    speed: i.speed,
                    acceleration: i.acceleration,
                    repeat_ms: i.repeat_ms,
                    delay_ms: i.delay_ms,
                    extra: extra(),
                }),
                extra: extra(),
            }))
        }
    }
}

fn need<T: Clone>(v: &Option<T>, flag: &str, action: &str) -> Result<T, String> {
    v.clone()
        .ok_or_else(|| format!("`--action {action}` needs {flag}"))
}

fn parse<T>(what: &str, v: &Option<String>, f: fn(&str) -> Option<T>) -> Result<Option<T>, String> {
    v.as_deref()
        .map(|s| f(s).ok_or_else(|| format!("unknown {what} `{s}`")))
        .transpose()
}

/// Builds one step from `--action <kind>` and its flags.
pub fn build_step(kind: &str, i: &StepInputs) -> Result<Action, String> {
    let extra = Map::new;
    Ok(match kind {
        "remap" => Action::Remap(Remap {
            to: canonical_output(&need(&i.to, "--to <keys>", kind)?, 1)?,
            tap: i
                .tap
                .as_deref()
                .map(|t| canonical_output(t, 1))
                .transpose()?,
            tap_ms: i.tap_ms,
            extra: extra(),
        }),
        "block" => Action::Block(crate::model::Block { extra: extra() }),
        "send_keys" => Action::SendKeys(SendKeys {
            keys: canonical_output(&need(&i.keys, "--keys <keys>", kind)?, 8)?,
            step_delay_ms: i.step_delay_ms,
            extra: extra(),
        }),
        "type_text" => Action::TypeText(TypeText {
            text: need(&i.text, "--text <text>", kind)?,
            method: parse("method", &i.method, TextMethod::parse)?,
            key_delay_ms: i.key_delay_ms,
            variables: i.variables.then_some(true),
            extra: extra(),
        }),
        "open_program" => {
            if i.path.is_none() == i.app_id.is_none() {
                return Err("`--action open_program` needs --path <exe> or --app-id <id>".into());
            }
            // A pasted path loses its quotes and `/` becomes `\` (Step 70); it is never resolved
            // or searched for, and a path in a known folder keeps the form it was given.
            let path = i.path.as_deref().map(launch::normalize_path);
            if let Some(p) = path.as_deref()
                && let Some(problem) = launch::path_problem(p, true)
            {
                return Err(format!("--path: {}", problem.message()));
            }
            let working_dir = i.working_dir.as_deref().map(launch::normalize_path);
            if let Some(w) = working_dir.as_deref()
                && let Some(problem) = launch::path_problem(w, false)
            {
                return Err(format!("--working-dir: {}", problem.message()));
            }
            if let Some(p) = launch::args_problem(&i.args) {
                return Err(format!("--arg: {p}"));
            }
            if let Some(p) = launch::env_problem(&i.env) {
                return Err(format!("--env: {p}"));
            }
            Action::OpenProgram(OpenProgram {
                path,
                app_id: i.app_id.clone(),
                args: (!i.args.is_empty()).then(|| i.args.clone()),
                working_dir,
                env: (!i.env.is_empty()).then(|| {
                    i.env
                        .iter()
                        .map(|(name, value)| EnvVar {
                            name: name.clone(),
                            value: value.clone(),
                        })
                        .collect()
                }),
                window: parse("window state", &i.window, WindowState::parse)?,
                if_running: parse("if-running policy", &i.if_running, IfRunning::parse)?,
                extra: extra(),
            })
        }
        // The address is normalized as in the editor (https:// added, scheme and host in lower
        // case, spaces encoded); every scheme but http and https is refused (Step 70).
        "open_url" => Action::OpenUrl(OpenUrl {
            url: launch::normalize_url(&need(&i.url, "--url <url>", kind)?)
                .map_err(|p| format!("--url: {}", p.message()))?,
            extra: extra(),
        }),
        "engine" => Action::Engine(EngineStep {
            op: parse("engine op", &i.op, EngineOp::parse)?
                .ok_or("`--action engine` needs --op pause|resume|toggle_pause|disable")?,
            extra: extra(),
        }),
        "mode" => Action::Mode(ModeStep {
            op: parse("mode op", &i.op, ModeOp::parse)?
                .ok_or("`--action mode` needs --op set|toggle|cycle|hold|previous|default")?,
            mode: i.target_mode.clone(),
            modes: (!i.target_modes.is_empty()).then(|| i.target_modes.clone()),
            extra: extra(),
        }),
        "delay" => Action::Delay(Delay {
            ms: need(&i.ms, "--ms <milliseconds>", kind)?,
            extra: extra(),
        }),
        "mouse_move" => mouse_move(i)?,
        "mouse_scroll" => Action::MouseScroll(MouseScroll {
            direction: parse("scroll direction", &i.direction, Direction::parse)?
                .ok_or("`--action mouse_scroll` needs --direction up|down|left|right")?,
            notches: i.notches,
            extra: extra(),
        }),
        "mouse_click" => Action::MouseClick(MouseClick {
            button: parse("mouse button", &i.button, crate::model::Button::parse)?
                .ok_or("`--action mouse_click` needs --button left|right|middle|x1|x2")?,
            op: parse("click op", &i.op, ClickOp::parse)?.filter(|o| *o != ClickOp::Click),
            at: mouse_point(i, kind)?,
            extra: extra(),
        }),
        other => return Err(format!("unknown action type `{other}`")),
    })
}

/// A step from JSON (`--step`).
pub fn parse_step(json: &str) -> Result<Action, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("--step is not JSON: {e}"))?;
    let mut a: Action =
        serde_json::from_value(v).map_err(|e| format!("--step does not describe a step: {e}"))?;
    // Keys are stored in canonical form.
    match &mut a {
        Action::Remap(r) => {
            r.to = canonical_output(&r.to, 1)?;
            if let Some(t) = &r.tap {
                r.tap = Some(canonical_output(t, 1)?);
            }
        }
        Action::SendKeys(s) => s.keys = canonical_output(&s.keys, 8)?,
        _ => {}
    }
    Ok(a)
}

/// A new rule as the CLI describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct NewRule {
    pub id: Option<String>,
    pub profile: Option<String>,
    pub position: Position,
    pub rule: Rule,
    pub allow: Vec<Permission>,
}

fn profile_of(doc: &Document, id: Option<&str>) -> Result<usize, EditError> {
    let id = id.unwrap_or(&doc.active_profile);
    doc.profile_index(id)
        .ok_or_else(|| EditError::ProfileNotFound(id.to_string()))
}

fn locate(doc: &Document, id: &str) -> Result<(usize, usize), EditError> {
    doc.find_rule(id)
        .ok_or_else(|| EditError::RuleNotFound(id.to_string()))
}

fn insert_at(list: &mut Vec<RuleEntry>, pos: &Position, entry: RuleEntry) -> Result<(), EditError> {
    let idx = |id: &str| {
        list.iter()
            .position(|r| r.id() == Some(id))
            .ok_or_else(|| EditError::RuleNotFound(id.to_string()))
    };
    let at = match pos {
        Position::End => list.len(),
        Position::Start => 0,
        Position::Before(id) => idx(id)?,
        Position::After(id) => idx(id)? + 1,
        Position::Index(n) => (*n).min(list.len()),
    };
    list.insert(at, entry);
    Ok(())
}

/// Records grants for the permissions in `allow` that the rule needs.
fn grant(rule: &mut Rule, allow: &[Permission]) {
    let needed = rule.permissions_needed();
    for p in allow {
        if needed.contains(p) {
            let fp = (*p == Permission::LaunchPrograms)
                .then(|| validate::launch_fingerprint(&rule.to_value()));
            rule.set_grant(*p, Some(Grant::cli_now(fp)));
        }
    }
}

fn check_modes(doc: &Document, pi: usize, rule: &Rule) -> Result<(), EditError> {
    let known: BTreeSet<&str> = doc.profiles[pi]
        .modes
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    for m in rule.modes.iter().flatten() {
        if !known.contains(m.as_str()) {
            return Err(EditError::ModeNotFound(m.clone()));
        }
    }
    Ok(())
}

/// `rule add`: inserts a new rule. Without the consent it needs it is added disabled
/// (`consent_needed`).
pub fn add(doc: &mut Document, mut n: NewRule) -> Result<Change, EditError> {
    let pi = profile_of(doc, n.profile.as_deref())?;
    let id = match n.id.take() {
        Some(id) if !is_rule_id(&id) => {
            return Err(EditError::Usage(format!(
                "`{id}` is not a rule ID: 2–48 lowercase letters, digits and `-`, starting with a letter"
            )));
        }
        Some(id) if doc.has_rule_id(&id) => {
            return Err(EditError::Conflict {
                message: format!("a rule with the ID `{id}` already exists; IDs are never reused"),
                problems: Vec::new(),
            });
        }
        Some(id) => id,
        None => new_rule_id(|c| doc.has_rule_id(c)),
    };
    let mut rule = n.rule;
    rule.id = id.clone();
    check_modes(doc, pi, &rule)?;
    let now = crate::time::now_rfc3339();
    rule.created_at = Some(now.clone());
    rule.updated_at = Some(now);
    grant(&mut rule, &n.allow);
    let mut change = Change {
        rules: vec![id.clone()],
        created: Some(id),
        changed: true,
        ..Change::default()
    };
    let missing = validate::missing_consent(&rule);
    if rule.enabled && !missing.is_empty() {
        rule.enabled = false;
        change.notes.push(format!(
            "saved disabled: it needs --allow {}",
            missing
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(" --allow ")
        ));
        change.consent_needed = missing;
    }
    insert_at(
        &mut doc.profiles[pi].rules,
        &n.position,
        RuleEntry::Typed(Box::new(rule)),
    )?;
    Ok(change)
}

/// RFC 7396 JSON Merge Patch.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let t = target.as_object_mut().expect("object");
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// `rule edit`: applies a merge patch (built from flags and `--patch`) to one rule. The ID never
/// changes. Grants in `allow` are (re)recorded for the edited targets.
pub fn edit(
    doc: &mut Document,
    id: &str,
    patch: &Value,
    allow: &[Permission],
) -> Result<Change, EditError> {
    let (pi, ri) = locate(doc, id)?;
    if let Some(new) = patch.get("id")
        && new.as_str() != Some(id)
    {
        return Err(EditError::Usage(
            "rule IDs never change; duplicate the rule to get a new ID".into(),
        ));
    }
    let before = doc.profiles[pi].rules[ri].to_value();
    let mut v = before.clone();
    merge_patch(&mut v, patch);
    let entry = match serde_json::from_value::<Rule>(v.clone()) {
        Ok(mut rule) => {
            check_modes(doc, pi, &rule)?;
            grant(&mut rule, allow);
            if rule.to_value() != before {
                rule.updated_at = Some(crate::time::now_rfc3339());
            }
            let missing = validate::missing_consent(&rule);
            if rule.enabled && !missing.is_empty() {
                return Err(EditError::ConsentRequired {
                    rule: id.to_string(),
                    permissions: missing,
                });
            }
            RuleEntry::Typed(Box::new(rule))
        }
        // A rule that still does not fit the types stays as written (and inactive).
        Err(_) => RuleEntry::Raw(v),
    };
    let changed = entry.to_value() != before;
    doc.profiles[pi].rules[ri] = entry;
    Ok(Change {
        rules: vec![id.to_string()],
        changed,
        ..Change::default()
    })
}

/// `rule delete`: removes rules (all must exist).
pub fn delete(doc: &mut Document, ids: &[String]) -> Result<Change, EditError> {
    for id in ids {
        locate(doc, id)?;
    }
    for id in ids {
        if let Ok((pi, ri)) = locate(doc, id) {
            doc.profiles[pi].rules.remove(ri);
        }
    }
    Ok(Change {
        rules: ids.to_vec(),
        changed: !ids.is_empty(),
        ..Change::default()
    })
}

/// `rule duplicate`: a disabled copy right after the original, with a new ID, optionally a new
/// title and trigger. Consent is kept because the copy's targets are the same (§6.3).
pub fn duplicate(
    doc: &mut Document,
    id: &str,
    new_id: Option<String>,
    title: Option<String>,
    trigger: Option<String>,
) -> Result<Change, EditError> {
    let (pi, ri) = locate(doc, id)?;
    let Some(src) = doc.profiles[pi].rules[ri].rule().cloned() else {
        return Err(EditError::Invalid {
            code: "rule_invalid",
            message: format!(
                "rule `{id}` does not fit this version's rule format; fix it before copying it"
            ),
            problems: Vec::new(),
        });
    };
    let new_id = match new_id {
        Some(n) if !is_rule_id(&n) => {
            return Err(EditError::Usage(format!("`{n}` is not a rule ID")));
        }
        Some(n) if doc.has_rule_id(&n) => {
            return Err(EditError::Conflict {
                message: format!("a rule with the ID `{n}` already exists; IDs are never reused"),
                problems: Vec::new(),
            });
        }
        Some(n) => n,
        None => new_rule_id(|c| doc.has_rule_id(c)),
    };
    let mut copy = src.clone();
    copy.id = new_id.clone();
    copy.enabled = false;
    copy.title = title.unwrap_or_else(|| {
        let t = format!("{} (copy)", src.title);
        t.chars().take(80).collect()
    });
    if let Some(t) = trigger {
        copy.trigger.keys = Some(t);
        copy.trigger.hotstring = None;
    }
    let now = crate::time::now_rfc3339();
    copy.created_at = Some(now.clone());
    copy.updated_at = Some(now);
    doc.profiles[pi]
        .rules
        .insert(ri + 1, RuleEntry::Typed(Box::new(copy)));
    Ok(Change {
        rules: vec![new_id.clone()],
        created: Some(new_id),
        changed: true,
        notes: vec!["the copy is disabled; change its trigger, then enable it".into()],
        ..Change::default()
    })
}

/// `rule enable` / `rule disable`.
pub fn set_enabled(
    doc: &mut Document,
    ids: &[String],
    on: bool,
    allow: &[Permission],
) -> Result<Change, EditError> {
    for id in ids {
        locate(doc, id)?;
    }
    let mut change = Change {
        rules: ids.to_vec(),
        ..Change::default()
    };
    for id in ids {
        let (pi, ri) = locate(doc, id)?;
        let entry = &mut doc.profiles[pi].rules[ri];
        if on && let Some(rule) = entry.rule_mut() {
            grant(rule, allow);
            let missing = validate::missing_consent(rule);
            if !missing.is_empty() {
                return Err(EditError::ConsentRequired {
                    rule: id.clone(),
                    permissions: missing,
                });
            }
        }
        if entry.enabled() != Some(on) {
            entry.set_enabled(on);
            if let Some(r) = entry.rule_mut() {
                r.updated_at = Some(crate::time::now_rfc3339());
            }
            change.changed = true;
        }
    }
    Ok(change)
}

/// `rule move`: changes a rule's place in its profile's list.
pub fn move_rule(doc: &mut Document, id: &str, pos: &Position) -> Result<Change, EditError> {
    let (pi, ri) = locate(doc, id)?;
    if let Position::Before(o) | Position::After(o) = pos {
        if o == id {
            return Err(EditError::Usage(
                "a rule cannot move relative to itself".into(),
            ));
        }
        let (opi, _) = locate(doc, o)?;
        if opi != pi {
            return Err(EditError::Usage(format!(
                "`{o}` is in another profile; rules move within their profile"
            )));
        }
    }
    let list = &mut doc.profiles[pi].rules;
    let before: Vec<Option<String>> = list.iter().map(|r| r.id().map(str::to_string)).collect();
    let entry = list.remove(ri);
    insert_at(list, pos, entry)?;
    let after: Vec<Option<String>> = list.iter().map(|r| r.id().map(str::to_string)).collect();
    Ok(Change {
        rules: vec![id.to_string()],
        changed: before != after,
        ..Change::default()
    })
}

/// `rule revoke`: withdraws one consent; a rule that needs it is disabled (it stays, inactive).
pub fn revoke(doc: &mut Document, id: &str, p: Permission) -> Result<Change, EditError> {
    let (pi, ri) = locate(doc, id)?;
    let Some(rule) = doc.profiles[pi].rules[ri].rule_mut() else {
        return Err(EditError::Invalid {
            code: "rule_invalid",
            message: format!("rule `{id}` does not fit this version's rule format"),
            problems: Vec::new(),
        });
    };
    let mut change = Change {
        rules: vec![id.to_string()],
        ..Change::default()
    };
    if rule.grant(p).is_some() {
        rule.set_grant(p, None);
        change.changed = true;
        if rule.enabled && rule.permissions_needed().contains(&p) {
            rule.enabled = false;
            change
                .notes
                .push(format!("disabled: it needs `{}` to run", p.as_str()));
        }
        rule.updated_at = Some(crate::time::now_rfc3339());
    }
    Ok(change)
}

/// The errors `after` has that `before` did not: what a change would introduce.
pub fn introduced(before: &Report, after: &Report) -> Vec<Problem> {
    let key = |p: &Problem| {
        (
            p.code,
            p.profile.clone(),
            p.rule.clone(),
            if p.rule.is_some() {
                String::new()
            } else {
                p.at.clone()
            },
        )
    };
    let old: BTreeSet<_> = before
        .all()
        .filter(|p| p.severity == Severity::Error)
        .map(key)
        .collect();
    after
        .all()
        .filter(|p| p.severity == Severity::Error && !old.contains(&key(p)))
        .cloned()
        .collect()
}

/// Refuses a change that introduces errors, with the CLI code that fits the first one.
pub fn review(before: &Report, after: &Report) -> Result<(), EditError> {
    let new = introduced(before, after);
    let Some(first) = new.first() else {
        return Ok(());
    };
    let conflict = new.iter().find(|p| {
        matches!(
            p.code,
            "duplicate_trigger" | "duplicate_binding" | "duplicate_rule_id"
        )
    });
    if let Some(c) = conflict {
        let subject = c.rule.as_deref().unwrap_or("?");
        let with = |k: &str| {
            c.with
                .as_ref()
                .and_then(|w| w.get(k))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        let message = match c.code {
            "duplicate_trigger" => format!(
                "rule `{subject}` would have the same trigger as rule `{}` (same modes, conditions and priority), so one of them could never fire; nothing was written",
                with("rule")
            ),
            "duplicate_binding" => format!(
                "rule `{subject}` would use `{}`, which is Ddalgak's own shortcut `{}` in hotkeys.json; nothing was written",
                with("keys"),
                with("action")
            ),
            _ => format!("{}; nothing was written", c.message),
        };
        return Err(EditError::Conflict {
            message,
            problems: new,
        });
    }
    let code = match first.code {
        "trigger_invalid" => "hotkey_invalid",
        "trigger_reserved" => "hotkey_reserved",
        _ => "rule_invalid",
    };
    Err(EditError::Invalid {
        code,
        message: format!("{}; nothing was written", first.message),
        problems: new,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Trigger;
    use crate::validate::{Context, check};
    use serde_json::json;

    fn engine(op: EngineOp) -> Action {
        Action::Engine(EngineStep {
            op,
            extra: Map::new(),
        })
    }

    fn new_rule(keys: &str, steps: Vec<Action>) -> NewRule {
        NewRule {
            id: None,
            profile: None,
            position: Position::End,
            rule: Rule::new(String::new(), "t".into(), Trigger::keys(keys), steps),
            allow: Vec::new(),
        }
    }

    fn report(doc: &Document) -> Report {
        check(&doc.to_value(), &Context::default())
    }

    #[test]
    fn canonical_input_accepts_aliases_lock_keys_and_bare_modifiers() {
        assert_eq!(canonical_trigger("ctrl+shift+j").unwrap(), "Ctrl+Shift+J");
        assert_eq!(
            canonical_trigger("Shift+Ctrl+F13").unwrap(),
            "Ctrl+Shift+F13"
        );
        assert_eq!(canonical_trigger("capslock").unwrap(), "CapsLock");
        assert_eq!(
            canonical_trigger("shift+capslock").unwrap(),
            "Shift+CapsLock"
        );
        assert_eq!(canonical_trigger("ralt").unwrap(), "RAlt");
        assert_eq!(
            canonical_trigger("Ctrl+K  Ctrl+U").unwrap(),
            "Ctrl+K Ctrl+U"
        );
        assert_eq!(
            canonical_trigger("Win+L").unwrap_err().code(),
            "trigger_reserved"
        );
        assert_eq!(
            canonical_trigger("lwin").unwrap_err().code(),
            "trigger_reserved"
        );
        assert_eq!(
            canonical_trigger("Ctrl+Alt+Delete").unwrap_err().code(),
            "trigger_reserved"
        );
        assert_eq!(
            canonical_trigger("Hyper+Q").unwrap_err().code(),
            "trigger_invalid"
        );
        assert_eq!(canonical_output("lwin", 1).unwrap(), "LWin");
        assert_eq!(canonical_output("win+d", 1).unwrap(), "Win+D");
        assert_eq!(
            canonical_output("ctrl+k ctrl+v", 8).unwrap(),
            "Ctrl+K Ctrl+V"
        );
        assert!(canonical_output("a b", 1).is_err());
    }

    #[test]
    fn mouse_buttons_are_triggers_and_outputs_with_their_limits() {
        // Step 68: the middle and X buttons as triggers (with modifiers), all five as remap
        // outputs; AutoHotkey-style names are accepted and stored canonically.
        assert_eq!(canonical_trigger("mousex1").unwrap(), "MouseX1");
        assert_eq!(canonical_trigger("XButton2").unwrap(), "MouseX2");
        assert_eq!(
            canonical_trigger("shift+ctrl+mbutton").unwrap(),
            "Ctrl+Shift+MouseMiddle"
        );
        for refused in ["MouseLeft", "ctrl+lbutton", "RButton", "MouseX1 F13"] {
            assert_eq!(
                canonical_trigger(refused).unwrap_err().code(),
                "trigger_invalid",
                "{refused}"
            );
        }
        assert_eq!(
            canonical_trigger("Win+MouseX1").unwrap_err().code(),
            "trigger_reserved"
        );
        assert_eq!(canonical_output("lbutton", 1).unwrap(), "MouseLeft");
        assert_eq!(
            canonical_output("ctrl+mouseright", 1).unwrap(),
            "Ctrl+MouseRight"
        );
        assert!(
            canonical_output("Ctrl+C MouseLeft", 8).is_err(),
            "send_keys presses keys only"
        );
        assert_eq!(canonical_output("code:KeyH", 1).unwrap(), "code:KeyH");
    }

    #[test]
    fn remap_rules_with_pass_through_and_dual_roles_are_checked_on_add() {
        let mut doc = Document::empty();
        let step = |to: &str, tap: Option<&str>| {
            build_step(
                "remap",
                &StepInputs {
                    to: Some(to.into()),
                    tap: tap.map(str::to_string),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut n = new_rule("F14", vec![step("F16", None)]);
        n.rule.trigger.pass_through = Some(true);
        n.allow = vec![Permission::InputInjection];
        assert!(add(&mut doc, n).unwrap().consent_needed.is_empty());
        // A dual role cannot also pass the original key through (schema, ADR 0031): the CLI's
        // review of the change refuses it.
        let report = |d: &Document| check(&d.to_value(), &Context::default());
        let before = report(&doc);
        let mut tried = doc.clone();
        let mut n = new_rule("F15", vec![step("LCtrl", Some("Escape"))]);
        n.rule.trigger.pass_through = Some(true);
        n.allow = vec![Permission::InputInjection];
        add(&mut tried, n).unwrap();
        let refused = review(&before, &report(&tried)).unwrap_err();
        assert_eq!(refused.code(), "rule_invalid", "{refused:?}");
        // A mouse-button trigger remapped to a key.
        let mut n = new_rule("MouseX1", vec![step("Alt+Left", None)]);
        n.allow = vec![Permission::InputInjection];
        assert!(add(&mut doc, n).is_ok());
        // `block` needs no consent.
        let block = build_step("block", &StepInputs::default()).unwrap();
        assert!(
            add(&mut doc, new_rule("Insert", vec![block]))
                .unwrap()
                .consent_needed
                .is_empty()
        );
        let l = crate::rules::from_store(&crate::store::interpret(
            doc.to_text().as_bytes(),
            &Context::default(),
            crate::store::MIGRATIONS,
        ));
        let active: Vec<String> = l.bindings.iter().map(|b| b.trigger.to_string()).collect();
        assert_eq!(active, ["F14", "MouseX1", "Insert"], "{:?}", l.inactive);
    }

    #[test]
    fn add_generates_ids_records_consent_or_saves_disabled() {
        let mut doc = Document::empty();
        let c = add(&mut doc, new_rule("F13", vec![engine(EngineOp::Pause)])).unwrap();
        let id = c.created.unwrap();
        assert!(id.starts_with("r-") && is_rule_id(&id));
        let r = doc.profiles[0].rules[0].rule().unwrap();
        assert!(r.enabled && r.created_at.is_some());
        // A remap needs input-injection: without --allow it is saved disabled.
        let remap = build_step(
            "remap",
            &StepInputs {
                to: Some("lctrl".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let c = add(&mut doc, new_rule("CapsLock", vec![remap.clone()])).unwrap();
        assert_eq!(c.consent_needed, [Permission::InputInjection]);
        assert_eq!(doc.profiles[0].rules[1].enabled(), Some(false));
        let mut n = new_rule("Shift+CapsLock", vec![remap]);
        n.allow = vec![Permission::InputInjection];
        n.id = Some("caps".into());
        let c = add(&mut doc, n).unwrap();
        assert!(c.consent_needed.is_empty());
        let caps = doc.profiles[0].rules[2].rule().unwrap();
        assert!(caps.enabled);
        assert_eq!(caps.grant(Permission::InputInjection).unwrap().via, "cli");
        // IDs are never reused.
        let mut n = new_rule("F14", vec![engine(EngineOp::Pause)]);
        n.id = Some("caps".into());
        assert_eq!(add(&mut doc, n).unwrap_err().code(), "rule_conflict");
        let mut n = new_rule("F14", vec![engine(EngineOp::Pause)]);
        n.profile = Some("nope".into());
        assert_eq!(add(&mut doc, n).unwrap_err().code(), "profile_not_found");
        let mut n = new_rule("F14", vec![engine(EngineOp::Pause)]);
        n.rule.modes = Some(vec!["nav".into()]);
        assert_eq!(add(&mut doc, n).unwrap_err().code(), "mode_not_found");
    }

    #[test]
    fn a_duplicate_trigger_is_refused_with_offers_and_nothing_changes() {
        let mut doc = Document::empty();
        add(&mut doc, new_rule("F13", vec![engine(EngineOp::Pause)])).unwrap();
        let before = report(&doc);
        let mut next = doc.clone();
        add(&mut next, new_rule("F13", vec![engine(EngineOp::Resume)])).unwrap();
        let err = review(&before, &report(&next)).unwrap_err();
        assert_eq!(err.code(), "rule_conflict");
        let p = &err.problems()[0];
        assert_eq!(p.code, "duplicate_trigger");
        assert!(matches!(p.offers[0], validate::Offer::Rebind(ref k) if k == "Shift+F13"));
        // Disabled, the same rule is fine.
        let mut ok = doc.clone();
        let mut n = new_rule("F13", vec![engine(EngineOp::Resume)]);
        n.rule.enabled = false;
        add(&mut ok, n).unwrap();
        assert!(review(&before, &report(&ok)).is_ok());
    }

    #[test]
    fn edit_keeps_ids_and_unknown_fields_and_checks_consent() {
        let mut doc = Document::empty();
        let id = add(&mut doc, new_rule("F13", vec![engine(EngineOp::Pause)]))
            .unwrap()
            .created
            .unwrap();
        doc.profiles[0].rules[0]
            .rule_mut()
            .unwrap()
            .extra
            .insert("from_v2".into(), json!(1));
        let c = edit(&mut doc, &id, &json!({ "title": "Renamed" }), &[]).unwrap();
        assert!(c.changed);
        let r = doc.profiles[0].rules[0].rule().unwrap();
        assert_eq!((r.id.as_str(), r.title.as_str()), (id.as_str(), "Renamed"));
        assert_eq!(r.extra["from_v2"], 1, "unknown fields survive an edit");
        assert_eq!(
            edit(&mut doc, &id, &json!({ "id": "other" }), &[])
                .unwrap_err()
                .code(),
            "usage"
        );
        // Changing the step to a launch needs launch-programs consent.
        let url = json!({ "steps": [ { "type": "open_url", "url": "https://example.com/" } ] });
        assert_eq!(
            edit(&mut doc, &id, &url, &[]).unwrap_err().code(),
            "consent_required"
        );
        edit(&mut doc, &id, &url, &[Permission::LaunchPrograms]).unwrap();
        assert!(report(&doc).all().all(|p| p.code != "consent_stale"));
        // A new URL makes the old consent stale: refused without a fresh --allow.
        let url2 = json!({ "steps": [ { "type": "open_url", "url": "https://example.org/" } ] });
        assert_eq!(
            edit(&mut doc, &id, &url2, &[]).unwrap_err().code(),
            "consent_required"
        );
        assert_eq!(
            edit(&mut doc, "nope", &url2, &[]).unwrap_err().code(),
            "rule_not_found"
        );
    }

    #[test]
    fn delete_duplicate_enable_disable_move_and_revoke() {
        let mut doc = Document::empty();
        let mut ids = Vec::new();
        for k in ["F13", "F14", "F15"] {
            ids.push(
                add(&mut doc, new_rule(k, vec![engine(EngineOp::Pause)]))
                    .unwrap()
                    .created
                    .unwrap(),
            );
        }
        let order = |d: &Document| {
            d.profiles[0]
                .rules
                .iter()
                .map(|r| r.id().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        move_rule(&mut doc, &ids[2], &Position::Start).unwrap();
        assert_eq!(
            order(&doc),
            [ids[2].clone(), ids[0].clone(), ids[1].clone()]
        );
        move_rule(&mut doc, &ids[2], &Position::After(ids[1].clone())).unwrap();
        assert_eq!(order(&doc), ids);
        assert!(
            !move_rule(&mut doc, &ids[2], &Position::End)
                .unwrap()
                .changed
        );
        let c = duplicate(&mut doc, &ids[0], Some("copy-a".into()), None, None).unwrap();
        assert_eq!(c.created.as_deref(), Some("copy-a"));
        let copy = doc.profiles[0].rules[1].rule().unwrap();
        assert!(!copy.enabled);
        assert!(copy.title.ends_with("(copy)"));
        assert!(
            report(&doc).all().all(|p| p.severity != Severity::Error),
            "a disabled copy is no duplicate"
        );
        set_enabled(&mut doc, &["copy-a".into()], false, &[]).unwrap();
        assert!(
            !set_enabled(&mut doc, &["copy-a".into()], false, &[])
                .unwrap()
                .changed
        );
        let before = report(&doc);
        let mut on = doc.clone();
        set_enabled(&mut on, &["copy-a".into()], true, &[]).unwrap();
        assert_eq!(
            review(&before, &report(&on)).unwrap_err().code(),
            "rule_conflict"
        );
        delete(&mut doc, &["copy-a".into(), ids[1].clone()]).unwrap();
        assert_eq!(order(&doc), [ids[0].clone(), ids[2].clone()]);
        assert_eq!(
            delete(&mut doc, &["gone".into()]).unwrap_err().code(),
            "rule_not_found"
        );
        // Revoke disables a rule that needs the permission.
        let mut n = new_rule(
            "F16",
            vec![
                build_step(
                    "send_keys",
                    &StepInputs {
                        keys: Some("F17".into()),
                        ..Default::default()
                    },
                )
                .unwrap(),
            ],
        );
        n.allow = vec![Permission::InputInjection];
        n.id = Some("send".into());
        add(&mut doc, n).unwrap();
        let c = revoke(&mut doc, "send", Permission::InputInjection).unwrap();
        assert!(c.changed && !c.notes.is_empty());
        let r = doc.profiles[0].rules.last().unwrap().rule().unwrap();
        assert!(!r.enabled && r.consent.is_none());
        assert_eq!(
            set_enabled(&mut doc, &["send".into()], true, &[])
                .unwrap_err()
                .code(),
            "consent_required"
        );
        set_enabled(
            &mut doc,
            &["send".into()],
            true,
            &[Permission::InputInjection],
        )
        .unwrap();
    }

    #[test]
    fn reserved_and_invalid_changes_map_to_hotkey_codes() {
        let mut doc = Document::empty();
        let id = add(&mut doc, new_rule("F13", vec![engine(EngineOp::Pause)]))
            .unwrap()
            .created
            .unwrap();
        let before = report(&doc);
        let mut bad = doc.clone();
        edit(
            &mut bad,
            &id,
            &json!({ "trigger": { "keys": "Alt+Tab" } }),
            &[],
        )
        .unwrap();
        assert_eq!(
            review(&before, &report(&bad)).unwrap_err().code(),
            "hotkey_reserved"
        );
        let mut bad = doc.clone();
        edit(&mut bad, &id, &json!({ "title": "" }), &[]).unwrap();
        assert_eq!(
            review(&before, &report(&bad)).unwrap_err().code(),
            "rule_invalid"
        );
        let mut bad = doc.clone();
        edit(
            &mut bad,
            &id,
            &json!({ "steps": [ { "type": "remap", "to": "F13" } ] }),
            &[Permission::InputInjection],
        )
        .unwrap();
        assert_eq!(
            review(&before, &report(&bad)).unwrap_err().code(),
            "rule_invalid",
            "output_is_trigger"
        );
    }

    #[test]
    fn steps_from_flags_and_json() {
        assert!(matches!(
            build_step("block", &StepInputs::default()).unwrap(),
            Action::Block(_)
        ));
        assert!(build_step("remap", &StepInputs::default()).is_err());
        assert!(
            build_step(
                "engine",
                &StepInputs {
                    op: Some("enable".into()),
                    ..Default::default()
                }
            )
            .is_err()
        );
        let p = build_step(
            "open_program",
            &StepInputs {
                path: Some("%SystemRoot%\\notepad.exe".into()),
                args: vec!["a b".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            matches!(p, Action::OpenProgram(ref o) if o.args.as_deref() == Some(&["a b".to_string()][..]))
        );
        assert!(
            build_step("mouse_click", &StepInputs::default())
                .unwrap_err()
                .contains("--button")
        );
        let s = parse_step(r#"{"type":"send_keys","keys":"ctrl+k ctrl+v"}"#).unwrap();
        assert!(matches!(s, Action::SendKeys(ref k) if k.keys == "Ctrl+K Ctrl+V"));
        let m = parse_step(r#"{"type":"mouse_click","button":"left"}"#).unwrap();
        assert_eq!(m.kind(), "mouse_click");
        assert!(parse_step("{").is_err());
    }

    #[test]
    fn mouse_flags_build_canonical_mouse_steps_and_refuse_mixtures() {
        // Step 71: `--action mouse_move|mouse_scroll|mouse_click` with flags.
        let step = |kind: &str, i: StepInputs| build_step(kind, &i).map(|a| a.to_value());
        assert_eq!(
            step(
                "mouse_move",
                StepInputs {
                    dx: Some(1),
                    speed: Some(600),
                    acceleration: Some(3),
                    ..Default::default()
                }
            ),
            Ok(
                serde_json::json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,
                                  "while_held":{"speed":600,"acceleration":3}})
            )
        );
        assert_eq!(
            step(
                "mouse_move",
                StepInputs {
                    dx: Some(-1),
                    grid: Some("16x9".into()),
                    repeat_ms: Some(150),
                    delay_ms: Some(400),
                    ..Default::default()
                }
            ),
            Ok(
                serde_json::json!({"type":"mouse_move","mode":"relative","dx":-1,"dy":0,
                                  "unit":"grid","grid":{"columns":16,"rows":9},
                                  "while_held":{"repeat_ms":150,"delay_ms":400}})
            )
        );
        assert_eq!(
            step(
                "mouse_move",
                StepInputs {
                    x: Some(-200),
                    y: Some(300),
                    relative_to: Some("primary_monitor".into()),
                    ..Default::default()
                }
            ),
            Ok(serde_json::json!({"type":"mouse_move","mode":"absolute",
                                  "to":{"x":-200,"y":300,"relative_to":"primary_monitor"}}))
        );
        assert_eq!(
            step(
                "mouse_scroll",
                StepInputs {
                    direction: Some("left".into()),
                    notches: Some(2),
                    ..Default::default()
                }
            ),
            Ok(serde_json::json!({"type":"mouse_scroll","direction":"left","notches":2}))
        );
        assert_eq!(
            step(
                "mouse_click",
                StepInputs {
                    button: Some("x2".into()),
                    op: Some("click".into()),
                    x: Some(5),
                    y: Some(6),
                    ..Default::default()
                }
            ),
            Ok(serde_json::json!({"type":"mouse_click","button":"x2","at":{"x":5,"y":6}})),
            "the default op is left out"
        );
        assert_eq!(
            step(
                "mouse_click",
                StepInputs {
                    button: Some("left".into()),
                    op: Some("hold".into()),
                    ..Default::default()
                }
            ),
            Ok(serde_json::json!({"type":"mouse_click","button":"left","op":"hold"}))
        );
        for (kind, bad, says) in [
            ("mouse_move", StepInputs::default(), "--dx/--dy"),
            (
                "mouse_move",
                StepInputs {
                    dx: Some(1),
                    x: Some(1),
                    y: Some(1),
                    ..Default::default()
                },
                "not both",
            ),
            (
                "mouse_move",
                StepInputs {
                    x: Some(1),
                    ..Default::default()
                },
                "both --x and --y",
            ),
            (
                "mouse_move",
                StepInputs {
                    dx: Some(1),
                    speed: Some(100),
                    repeat_ms: Some(100),
                    ..Default::default()
                },
                "not both",
            ),
            (
                "mouse_move",
                StepInputs {
                    dx: Some(1),
                    acceleration: Some(2),
                    ..Default::default()
                },
                "--speed",
            ),
            (
                "mouse_move",
                StepInputs {
                    x: Some(1),
                    y: Some(1),
                    grid: Some("4x4".into()),
                    ..Default::default()
                },
                "--dx/--dy",
            ),
            (
                "mouse_move",
                StepInputs {
                    dx: Some(1),
                    grid: Some("1x99".into()),
                    ..Default::default()
                },
                "COLUMNSxROWS",
            ),
            ("mouse_scroll", StepInputs::default(), "--direction"),
            (
                "mouse_click",
                StepInputs {
                    button: Some("x3".into()),
                    ..Default::default()
                },
                "unknown mouse button",
            ),
            (
                "mouse_click",
                StepInputs {
                    button: Some("left".into()),
                    op: Some("down".into()),
                    ..Default::default()
                },
                "unknown click op",
            ),
        ] {
            let e = build_step(kind, &bad).unwrap_err();
            assert!(e.contains(says), "{kind} {bad:?}: {e}");
        }
    }

    #[test]
    fn merge_patch_follows_rfc_7396() {
        let mut t = json!({ "a": "b", "c": { "d": "e", "f": "g" } });
        merge_patch(&mut t, &json!({ "a": "z", "c": { "f": null } }));
        assert_eq!(t, json!({ "a": "z", "c": { "d": "e" } }));
    }
}
