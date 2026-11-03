//! Reference checker for the hotkey contract v1 scenarios (docs/HOTKEY_CONTRACT.md §15).
//!
//! Every file in `docs/contracts/examples/hotkey-scenarios-v1/` describes installed tools
//! (manifest + optional `hotkeys.json`), chords other programs already registered, an optional
//! `set`/`disable`/`reset` request, and the exact expected conflict report or outcome. This
//! module validates the embedded documents against their own schemas and then recomputes the
//! expectation from the contract's rules (§5, §7, §9), so the fixtures are proven consistent.
//!
//! Deliberately limited (ADR 0017): no OS calls, no profiles, no Ddalgak rule triggers, no
//! input aliases. It is a specification oracle, not the hotkey library; Step 46 builds that
//! library, must pass the same scenarios, and may then replace this file.

use crate::jsonschema::Validator;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::path::Path;

/// Schemas whose shared `$defs` must stay identical (they were generated from one source).
const SCHEMAS: [&str; 4] = [
    "kim-hotkeys-v1",
    "kim-hotkey-manifest-v1",
    "hotkeys-cli-v1",
    "hotkey-scenarios-v1",
];

#[derive(Debug, Default)]
pub struct Report {
    pub scenarios: usize,
    pub problems: Vec<String>,
}

// ------------------------------------------------------------------------------------------
// Key model (§5)

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Side {
    #[default]
    None,
    Any,
    Left,
    Right,
}

impl Side {
    fn on(self) -> bool {
        self != Side::None
    }
    fn overlaps(self, other: Side) -> bool {
        match (self, other) {
            (Side::None, Side::None) => true,
            (Side::None, _) | (_, Side::None) => false,
            (Side::Any, _) | (_, Side::Any) => true,
            (a, b) => a == b,
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Side::Left => "L",
            Side::Right => "R",
            _ => "",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Mods {
    ctrl: Side,
    alt: Side,
    altgr: bool,
    shift: Side,
    win: Side,
}

impl Mods {
    fn overlaps(&self, o: &Mods) -> bool {
        self.ctrl.overlaps(o.ctrl)
            && self.alt.overlaps(o.alt)
            && self.altgr == o.altgr
            && self.shift.overlaps(o.shift)
            && self.win.overlaps(o.win)
    }
    fn side_specific(&self) -> bool {
        [self.ctrl, self.alt, self.shift, self.win]
            .iter()
            .any(|s| matches!(s, Side::Left | Side::Right))
    }
    /// A "command" modifier makes a chord a shortcut rather than typing (§7.2).
    fn has_command(&self) -> bool {
        self.ctrl.on() || self.alt.on() || self.altgr || self.win.on()
    }
    /// Exactly these generic families are held (sides ignored).
    fn exactly(&self, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.ctrl.on() == ctrl
            && self.alt.on() == alt
            && self.shift.on() == shift
            && !self.altgr
            && !self.win.on()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Key {
    Named(String),
    Code(String),
    Char(char),
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Step {
    Chord(Mods, Key),
    Leader(String),
}

const IME_KEYS: [&str; 5] = ["HangulMode", "Hanja", "KanaMode", "Convert", "NonConvert"];
const NAV_NO_COMMAND: [&str; 16] = [
    "Tab",
    "Escape",
    "Enter",
    "Space",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Backspace",
    "Insert",
    "Delete",
    "ContextMenu",
];

fn parse_step(s: &str) -> Result<Step, String> {
    if let Some(l) = s.strip_prefix('@') {
        return Ok(Step::Leader(l.to_string()));
    }
    if let Some(c) = s.strip_prefix("char:") {
        let mut it = c.chars();
        return match (it.next(), it.next()) {
            (Some(ch), None) => Ok(Step::Chord(Mods::default(), Key::Char(ch))),
            _ => Err(format!("`{s}`: char: takes exactly one character")),
        };
    }
    let parts: Vec<&str> = s.split('+').collect();
    let (key, mods_text) = parts
        .split_last()
        .ok_or_else(|| format!("empty step `{s}`"))?;
    let mut m = Mods::default();
    let mut rank = 0;
    for token in mods_text {
        let (side, family) = match token.as_bytes().first() {
            Some(b'L') if token.len() > 1 => (Side::Left, &token[1..]),
            Some(b'R') if token.len() > 1 => (Side::Right, &token[1..]),
            _ => (Side::Any, *token),
        };
        let (slot, r) = match family {
            "Ctrl" => (&mut m.ctrl, 1),
            "Alt" => (&mut m.alt, 2),
            "AltGr" if side == Side::Any => {
                m.altgr = true;
                if rank >= 2 {
                    return Err(format!("`{s}`: AltGr out of order"));
                }
                rank = 2;
                continue;
            }
            "Shift" => (&mut m.shift, 3),
            "Win" => (&mut m.win, 4),
            _ => return Err(format!("`{s}`: unknown modifier `{token}`")),
        };
        if r <= rank {
            return Err(format!("`{s}`: modifiers out of canonical order"));
        }
        rank = r;
        *slot = side;
    }
    let key = match key.strip_prefix("code:") {
        Some(code) => Key::Code(code.to_string()),
        None => Key::Named((*key).to_string()),
    };
    Ok(Step::Chord(m, key))
}

fn parse_binding(s: &str) -> Result<Vec<Step>, String> {
    s.split(' ').map(parse_step).collect()
}

fn render_chord(m: &Mods, k: &Key) -> String {
    let mut out = String::new();
    let alt_gr = if m.altgr { Side::Any } else { Side::None };
    for (side, name) in [
        (m.ctrl, "Ctrl"),
        (m.alt, "Alt"),
        (alt_gr, "AltGr"),
        (m.shift, "Shift"),
        (m.win, "Win"),
    ] {
        if side.on() {
            out.push_str(side.prefix());
            out.push_str(name);
            out.push('+');
        }
    }
    match k {
        Key::Named(n) => out.push_str(n),
        Key::Code(c) => {
            out.push_str("code:");
            out.push_str(c);
        }
        Key::Char(c) => {
            out.push_str("char:");
            out.push(*c);
        }
    }
    out
}

fn key_overlaps(a: &Step, b: &Step) -> bool {
    let (Step::Chord(ma, ka), Step::Chord(mb, kb)) = (a, b) else {
        return false;
    };
    match (ka, kb) {
        (Key::Char(x), Key::Char(y)) => x == y,
        (Key::Char(c), Key::Named(_)) => char_matches(*c, mb, kb),
        (Key::Named(_), Key::Char(c)) => char_matches(*c, ma, ka),
        (Key::Named(x), Key::Named(y)) | (Key::Code(x), Key::Code(y)) => x == y && ma.overlaps(mb),
        // Logical vs physical keys depend on the layout; the checker does not compare them.
        _ => false,
    }
}

/// `char:` vs a named key: compared only for ASCII letters and digits (US-like layouts).
fn char_matches(c: char, m: &Mods, k: &Key) -> bool {
    let Key::Named(n) = k else { return false };
    if m.has_command() {
        return false;
    }
    if c.is_ascii_alphabetic() {
        n.len() == 1
            && n.starts_with(c.to_ascii_uppercase())
            && m.shift.on() == c.is_ascii_uppercase()
    } else if c.is_ascii_digit() {
        n.len() == 1 && n.starts_with(c) && !m.shift.on()
    } else {
        false
    }
}

// ------------------------------------------------------------------------------------------
// Tools, bindings, scopes

#[derive(Clone, PartialEq, Eq, Debug)]
enum Scope {
    Global,
    App,
    Context(String),
}

impl Scope {
    fn text(&self) -> String {
        match self {
            Scope::Global => "global".into(),
            Scope::App => "app".into(),
            Scope::Context(c) => format!("context:{c}"),
        }
    }
    /// Overlapping scopes within one tool (§9.3).
    fn overlaps(&self, o: &Scope) -> bool {
        match (self, o) {
            (Scope::Global, _) | (_, Scope::Global) | (Scope::App, Scope::App) => true,
            (Scope::Context(a), Scope::Context(b)) => a == b,
            _ => false,
        }
    }
    fn family(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::App => "app",
            Scope::Context(_) => "context",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Layer {
    Default,
    User,
}

impl Layer {
    fn text(self) -> &'static str {
        match self {
            Layer::Default => "default",
            Layer::User => "user",
        }
    }
}

#[derive(Clone, Debug)]
struct Binding {
    tool: String,
    action: String,
    keys: String,
    steps: Vec<Step>,
    scope: Scope,
    trigger: String,
    layer: Layer,
    is_leader: bool,
}

impl Binding {
    fn same(&self, o: &Binding) -> bool {
        self.tool == o.tool && self.action == o.action && self.keys == o.keys
    }
    /// The leader table this binding belongs to (`@prefix …`), if any.
    fn table(&self) -> Option<&str> {
        match (self.is_leader, self.steps.as_slice()) {
            (false, [Step::Leader(l), _, ..]) => Some(l),
            _ => None,
        }
    }
    fn single_chord(&self) -> Option<(&Mods, &Key)> {
        match self.steps.as_slice() {
            [Step::Chord(m, k)] if !matches!(k, Key::Char(_)) => Some((m, k)),
            _ => None,
        }
    }
    fn kim_ref(&self) -> Value {
        json!({ "tool": self.tool, "action": self.action, "keys": self.keys, "scope": self.scope.text() })
    }
}

#[derive(Clone, Debug)]
struct ActionDecl {
    id: String,
    scopes: Vec<Scope>,
    context: Option<String>,
    defaults: Vec<Value>,
    bindable: bool,
}

#[derive(Clone, Debug)]
struct Tool {
    id: String,
    version: String,
    backend: String,
    enabled: bool,
    disabled_scopes: Vec<String>,
    passthrough: bool,
    leaders: Vec<(String, String)>, // (id, default chord)
    actions: Vec<ActionDecl>,
    config: Option<Value>,
}

impl Tool {
    fn from_docs(manifest: &Value, config: Option<Value>) -> Result<Tool, String> {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let arr = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let mut actions = Vec::new();
        for a in arr(manifest, "actions") {
            let scopes = arr(&a, "scopes")
                .iter()
                .map(|x| match x.as_str() {
                    Some("global") => Scope::Global,
                    _ => Scope::App,
                })
                .collect();
            actions.push(ActionDecl {
                id: s(&a, "id"),
                scopes,
                context: a.get("context").and_then(Value::as_str).map(str::to_string),
                defaults: arr(&a, "defaults"),
                bindable: a.get("bindable").and_then(Value::as_bool).unwrap_or(true),
            });
        }
        let cfg = config.clone().unwrap_or(Value::Null);
        let mut seen = BTreeSet::new();
        for o in arr(&cfg, "overrides") {
            if !seen.insert(s(&o, "action")) {
                return Err(format!("duplicate override for `{}`", s(&o, "action")));
            }
        }
        Ok(Tool {
            id: s(manifest, "tool"),
            version: s(manifest, "version"),
            backend: s(manifest, "backend"),
            enabled: cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            disabled_scopes: arr(&cfg, "disabled_scopes")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            passthrough: arr(manifest, "contexts")
                .iter()
                .any(|c| c.get("passthrough").and_then(Value::as_bool) == Some(true)),
            leaders: arr(manifest, "leaders")
                .iter()
                .map(|l| (s(l, "id"), s(l, "default")))
                .collect(),
            actions,
            config,
        })
    }

    fn override_for(&self, action: &str) -> Option<Value> {
        self.config
            .as_ref()?
            .get("overrides")?
            .as_array()?
            .iter()
            .find(|o| o.get("action").and_then(Value::as_str) == Some(action))
            .cloned()
    }

    fn leader_chord(&self, id: &str) -> Option<(String, Layer)> {
        let user = self
            .config
            .as_ref()
            .and_then(|c| c.get("leaders"))
            .and_then(|l| l.get(id))
            .and_then(Value::as_str);
        match user {
            Some(k) => Some((k.to_string(), Layer::User)),
            None => self
                .leaders
                .iter()
                .find(|(l, _)| l == id)
                .map(|(_, k)| (k.clone(), Layer::Default)),
        }
    }

    fn scope_enabled(&self, scope: &Scope) -> bool {
        !self.disabled_scopes.iter().any(|d| d == scope.family())
    }

    fn action(&self, id: &str) -> Option<&ActionDecl> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// `{source, bindings}` of an action in the current layers (change records, §10.2).
    fn layer_state(&self, action: &str) -> Value {
        match self.override_for(action) {
            Some(o) if o.get("disabled").is_some() => json!({"source": "disabled", "bindings": []}),
            Some(o) => json!({"source": "user", "bindings": spec_keys(o.get("bindings"))}),
            None => json!({
                "source": "default",
                "bindings": spec_keys(self.action(action).map(|a| Value::Array(a.defaults.clone())).as_ref())
            }),
        }
    }
}

fn spec_keys(specs: Option<&Value>) -> Vec<String> {
    specs
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| spec_parts(s).map(|p| p.0))
                .collect()
        })
        .unwrap_or_default()
}

/// (keys, explicit scope, trigger) of a binding spec (text or object form).
fn spec_parts(spec: &Value) -> Option<(String, Option<String>, String)> {
    match spec {
        Value::String(k) => Some((k.clone(), None, "press".into())),
        Value::Object(o) => Some((
            o.get("keys")?.as_str()?.to_string(),
            o.get("scope").and_then(Value::as_str).map(str::to_string),
            o.get("trigger")
                .and_then(Value::as_str)
                .unwrap_or("press")
                .to_string(),
        )),
        _ => None,
    }
}

// ------------------------------------------------------------------------------------------
// Conflicts (§9.3–§9.5)

#[derive(Clone, Debug)]
struct Conflict {
    code: &'static str,
    reason: Option<&'static str>,
    tool: String,
    action: String,
    keys: Option<String>,
    scope: Option<String>,
    source: &'static str,
    with: Value,
    /// The subject binding, when there is one (offers are computed for it).
    subject: Option<Binding>,
    offers: Vec<Value>,
}

fn severity(code: &str) -> &'static str {
    match code {
        "os_reserved" | "kim_reserved" | "ime_reserved" | "invalid_binding"
        | "duplicate_binding" | "sequence_prefix" | "cross_tool_global" => "error",
        "context_override" | "default_suppressed" | "shadows_terminal_input" => "info",
        _ => "warning",
    }
}

fn severity_rank(code: &str) -> u8 {
    match severity(code) {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    }
}

impl Conflict {
    fn about(code: &'static str, b: &Binding, with: Value) -> Conflict {
        Conflict {
            code,
            reason: None,
            tool: b.tool.clone(),
            action: b.action.clone(),
            keys: Some(b.keys.clone()),
            scope: Some(b.scope.text()),
            source: b.layer.text(),
            with,
            subject: Some(b.clone()),
            offers: Vec::new(),
        }
    }
    fn involves(&self, tool: &str, action: &str) -> bool {
        (self.tool == tool && self.action == action)
            || (self.with.get("tool").and_then(Value::as_str) == Some(tool)
                && self.with.get("action").and_then(Value::as_str) == Some(action))
    }
    fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("code".into(), json!(self.code));
        m.insert("severity".into(), json!(severity(self.code)));
        if let Some(r) = self.reason {
            m.insert("reason".into(), json!(r));
        }
        m.insert("tool".into(), json!(self.tool));
        m.insert("action".into(), json!(self.action));
        m.insert("keys".into(), json!(self.keys));
        m.insert("scope".into(), json!(self.scope));
        m.insert("source".into(), json!(self.source));
        m.insert("with".into(), self.with.clone());
        m.insert("offers".into(), Value::Array(self.offers.clone()));
        Value::Object(m)
    }
}

fn reserved_ref(kind: &str, keys: &str, name: &str) -> Value {
    json!({ "reserved": kind, "keys": keys, "name": name })
}

/// Reserved chords (§7). `first` says whether this is the binding's first step.
fn reserved(
    step: &Step,
    scope: &Scope,
    first: bool,
) -> Option<(&'static str, &'static str, &'static str)> {
    let Step::Chord(m, k) = step else { return None };
    let named = |n: &str| matches!(k, Key::Named(x) if x == n);
    let app_or_global = matches!(scope, Scope::App | Scope::Global);
    let typing = match k {
        Key::Char(_) | Key::Code(_) => true,
        Key::Named(n) => {
            (n.len() == 1)
                || ["Plus", "Minus", "Comma", "Period"].contains(&n.as_str())
                || n.starts_with("Numpad")
        }
    };
    let ime = match k {
        Key::Named(n) => IME_KEYS.contains(&n.as_str()),
        Key::Code(c) => c.starts_with("Lang"),
        Key::Char(_) => false,
    };
    let plain_alt = m.alt.on() && !m.altgr && !m.win.on();
    if ime {
        Some(("ime_reserved", "ime", "Input method key"))
    } else if m.win.on() {
        Some(("os_reserved", "os", "Windows key shortcut"))
    } else if m.ctrl.on() && plain_alt && (named("Delete") || named("NumpadDecimal")) {
        Some(("os_reserved", "os", "Secure Attention Sequence"))
    } else if m.exactly(true, false, true) && named("Escape") {
        Some(("os_reserved", "os", "Task Manager"))
    } else if m.exactly(true, false, false) && named("Escape") {
        Some(("os_reserved", "os", "Start menu"))
    } else if plain_alt && named("Tab") {
        Some(("os_reserved", "os", "Task switching"))
    } else if plain_alt && !m.ctrl.on() && named("Escape") {
        Some(("os_reserved", "os", "Window cycling"))
    } else if m.exactly(false, true, true) && named("PrintScreen") {
        Some(("os_reserved", "os", "High Contrast toggle"))
    } else if m.exactly(false, true, false) && named("F4") {
        Some(("kim_reserved", "kim", "Closes the window"))
    } else if m.exactly(true, true, true) && named("Escape") {
        Some(("kim_reserved", "emergency", "Emergency chord"))
    } else if *scope == Scope::Global && named("F12") {
        Some(("os_reserved", "os", "Reserved for debuggers"))
    } else if !(first && app_or_global) {
        None
    } else if (m.exactly(false, false, false) && (named("F6") || named("ContextMenu")))
        || (m.exactly(false, false, true) && (named("F6") || named("F10") || named("Tab")))
        || (!m.has_command() && NAV_NO_COMMAND.iter().any(|n| named(n)))
    {
        Some(("kim_reserved", "kim", "Navigation key"))
    } else if !m.has_command() && typing {
        Some(("kim_reserved", "kim", "Typing key"))
    } else {
        None
    }
}

/// Per-binding validity (§7, §4.2, §5): the first problem, as a conflict.
fn validate(
    tool: &Tool,
    decl: &ActionDecl,
    b: &Binding,
    explicit_scope: Option<&str>,
) -> Option<Conflict> {
    let invalid = |reason: &'static str| {
        let mut c = Conflict::about("invalid_binding", b, Value::Null);
        c.reason = Some(reason);
        Some(c)
    };
    if b.layer == Layer::User && !decl.bindable {
        return invalid("not_bindable");
    }
    if decl.context.is_some() && explicit_scope.is_some() {
        return invalid("scope_not_allowed");
    }
    if decl.context.is_none() && !decl.scopes.contains(&b.scope) {
        return invalid("scope_not_allowed");
    }
    if decl.bindable {
        for (i, step) in b.steps.iter().enumerate() {
            let step = expand(tool, step).unwrap_or_else(|| step.clone());
            if let Some((code, kind, name)) = reserved(&step, &b.scope, i == 0) {
                let keys = match &step {
                    Step::Chord(m, k) => render_chord(m, k),
                    Step::Leader(l) => format!("@{l}"),
                };
                return Some(Conflict::about(code, b, reserved_ref(kind, &keys, name)));
            }
        }
    }
    if b.steps
        .iter()
        .any(|s| matches!(s, Step::Leader(l) if tool.leader_chord(l).is_none()))
    {
        return invalid("unknown_leader");
    }
    if b.scope == Scope::Global {
        let beyond_register = b.steps.len() > 1
            || b.trigger != "press"
            || b.steps.iter().any(|s| match s {
                Step::Chord(m, k) => m.side_specific() || !matches!(k, Key::Named(_)),
                Step::Leader(_) => true,
            });
        if tool.backend == "focus" || (tool.backend == "register" && beyond_register) {
            return invalid("backend_unsupported");
        }
    }
    if b.trigger != "press" && b.steps.len() > 1 {
        return invalid("trigger_unsupported");
    }
    None
}

fn expand(tool: &Tool, step: &Step) -> Option<Step> {
    match step {
        Step::Leader(l) => parse_step(&tool.leader_chord(l)?.0).ok(),
        Step::Chord(..) => None,
    }
}

/// Single-binding warnings (§9.3): os_shortcut, altgr_collision, ime_toggle.
fn binding_warning(b: &Binding) -> Option<Conflict> {
    let chords = b.steps.iter().filter_map(|s| match s {
        Step::Chord(m, k) => Some((m, k)),
        Step::Leader(_) => None,
    });
    for (i, (m, k)) in chords.enumerate() {
        let named = |n: &str| matches!(k, Key::Named(x) if x == n);
        let printable = match k {
            Key::Named(n) => {
                n.len() == 1 || ["Plus", "Minus", "Comma", "Period"].contains(&n.as_str())
            }
            Key::Code(_) => true,
            Key::Char(_) => false,
        };
        if i == 0
            && matches!(b.scope, Scope::Global | Scope::App)
            && b.steps.len() == 1
            && ((m.exactly(false, false, false) && named("PrintScreen"))
                || (m.exactly(false, true, false) && (named("PrintScreen") || named("Space"))))
        {
            return Some(Conflict::about("os_shortcut", b, Value::Null));
        }
        if m.ctrl.on() && m.alt.on() && !m.altgr && !m.win.on() && printable {
            return Some(Conflict::about("altgr_collision", b, Value::Null));
        }
        if (m.exactly(true, false, false) && named("Space"))
            || (m.exactly(false, false, true) && named("Space"))
            || (m.exactly(false, true, false) && matches!(k, Key::Code(c) if c == "Backquote"))
        {
            return Some(Conflict::about("ime_toggle", b, Value::Null));
        }
    }
    None
}

#[derive(PartialEq, Eq, Debug)]
enum Rel {
    Equal,
    Prefix,
    None,
}

/// How two bindings of one tool relate, honoring leader tables (§9.3).
fn relation(tool: &Tool, a: &Binding, b: &Binding) -> Rel {
    let seq = |x: &Binding, skip: usize| -> Vec<Step> {
        x.steps[skip..]
            .iter()
            .map(|s| expand(tool, s).unwrap_or_else(|| s.clone()))
            .collect()
    };
    let (sa, sb) = match (a.table(), b.table()) {
        (Some(x), Some(y)) if x == y => (seq(a, 1), seq(b, 1)),
        (None, None) => (seq(a, 0), seq(b, 0)),
        _ => return Rel::None,
    };
    let n = sa.len().min(sb.len());
    if n == 0 || !(0..n).all(|i| key_overlaps(&sa[i], &sb[i])) {
        Rel::None
    } else if sa.len() == sb.len() {
        Rel::Equal
    } else {
        Rel::Prefix
    }
}

/// The first step a key press must match, with leaders resolved (cross-tool checks).
fn first_step(tool: &Tool, b: &Binding) -> Option<Step> {
    if b.table().is_some() {
        return None; // the leader binding itself carries the first step
    }
    b.steps
        .first()
        .map(|s| expand(tool, s).unwrap_or_else(|| s.clone()))
}

struct World<'a> {
    tools: &'a [Tool],
    taken: Vec<(String, Step)>,
}

struct Analysis {
    conflicts: Vec<Conflict>,
    active: Vec<Binding>,
}

impl World<'_> {
    fn tool(&self, id: &str) -> &Tool {
        self.tools
            .iter()
            .find(|t| t.id == id)
            .expect("binding of a known tool")
    }

    /// Bindings of one tool before cross-binding analysis, plus per-binding conflicts.
    fn candidates(&self, tool: &Tool, out: &mut Vec<Binding>, conflicts: &mut Vec<Conflict>) {
        for (id, _) in &tool.leaders {
            let (keys, layer) = tool.leader_chord(id).expect("declared leader");
            if !tool.scope_enabled(&Scope::App) {
                continue;
            }
            let b = Binding {
                tool: tool.id.clone(),
                action: format!("@{id}"),
                steps: parse_binding(&keys).unwrap_or_default(),
                keys,
                scope: Scope::App,
                trigger: "press".into(),
                layer,
                is_leader: true,
            };
            if let Some(w) = binding_warning(&b) {
                conflicts.push(w);
            }
            out.push(b);
        }
        for o in tool
            .config
            .as_ref()
            .and_then(|c| c.get("overrides"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let action = o.get("action").and_then(Value::as_str).unwrap_or("");
            if tool.action(action).is_none() {
                conflicts.push(Conflict {
                    code: "unknown_action",
                    reason: None,
                    tool: tool.id.clone(),
                    action: action.to_string(),
                    keys: spec_keys(o.get("bindings")).into_iter().next(),
                    scope: None,
                    source: "user",
                    with: Value::Null,
                    subject: None,
                    offers: Vec::new(),
                });
            }
        }
        for decl in &tool.actions {
            let (specs, layer) = match tool.override_for(&decl.id) {
                Some(o) if o.get("disabled").is_some() => continue,
                Some(o) => (
                    o.get("bindings")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default(),
                    Layer::User,
                ),
                None => (decl.defaults.clone(), Layer::Default),
            };
            for spec in &specs {
                let Some((keys, explicit, trigger)) = spec_parts(spec) else {
                    continue;
                };
                let scope = match (&decl.context, explicit.as_deref()) {
                    (Some(c), _) => Scope::Context(c.clone()),
                    (None, Some("global")) => Scope::Global,
                    (None, Some(_)) => Scope::App,
                    (None, None) => decl.scopes.first().cloned().unwrap_or(Scope::App),
                };
                if !tool.scope_enabled(&scope) {
                    continue;
                }
                let b = Binding {
                    tool: tool.id.clone(),
                    action: decl.id.clone(),
                    steps: parse_binding(&keys).unwrap_or_default(),
                    keys,
                    scope,
                    trigger,
                    layer,
                    is_leader: false,
                };
                if let Some(c) = validate(tool, decl, &b, explicit.as_deref()) {
                    conflicts.push(c);
                    continue;
                }
                if let Some(w) = binding_warning(&b) {
                    conflicts.push(w);
                }
                out.push(b);
            }
        }
    }

    fn analyze(&self) -> Analysis {
        let mut conflicts = Vec::new();
        let mut active: Vec<Binding> = Vec::new();
        for tool in self.tools.iter().filter(|t| t.enabled) {
            let mut cands = Vec::new();
            self.candidates(tool, &mut cands, &mut conflicts);
            // A user binding beats a default of another action (§9.1).
            let mut suppressed = vec![false; cands.len()];
            for (i, d) in cands.iter().enumerate() {
                if d.layer != Layer::Default || d.is_leader {
                    continue;
                }
                if let Some(u) = cands.iter().find(|u| {
                    u.layer == Layer::User
                        && u.action != d.action
                        && u.scope.overlaps(&d.scope)
                        && relation(tool, d, u) == Rel::Equal
                }) {
                    suppressed[i] = true;
                    conflicts.push(Conflict::about("default_suppressed", d, u.kim_ref()));
                }
            }
            let cands: Vec<Binding> = cands
                .into_iter()
                .zip(suppressed)
                .filter_map(|(b, s)| (!s).then_some(b))
                .collect();
            let mut inactive = vec![false; cands.len()];
            for i in 0..cands.len() {
                for j in i + 1..cands.len() {
                    let (a, b) = (&cands[i], &cands[j]);
                    if a.action == b.action {
                        continue;
                    }
                    let rel = relation(tool, a, b);
                    if rel == Rel::None {
                        continue;
                    }
                    if a.scope.overlaps(&b.scope) {
                        let code = if rel == Rel::Equal {
                            "duplicate_binding"
                        } else {
                            "sequence_prefix"
                        };
                        let a_first = if a.is_leader != b.is_leader {
                            b.is_leader
                        } else if a.layer != b.layer {
                            a.layer > b.layer
                        } else if code == "sequence_prefix" {
                            a.steps.len() < b.steps.len()
                        } else {
                            a.action > b.action
                        };
                        let (s, o, si) = if a_first { (a, b, i) } else { (b, a, j) };
                        inactive[si] = true;
                        conflicts.push(Conflict::about(code, s, o.kim_ref()));
                    } else if rel == Rel::Equal {
                        let (app, ctx) = match (&a.scope, &b.scope) {
                            (Scope::App, Scope::Context(_)) => (a, b),
                            (Scope::Context(_), Scope::App) => (b, a),
                            _ => continue,
                        };
                        conflicts.push(Conflict::about("context_override", app, ctx.kim_ref()));
                    }
                }
            }
            for (b, off) in cands.into_iter().zip(inactive) {
                if off {
                    continue;
                }
                if tool.passthrough
                    && b.scope == Scope::App
                    && b.table().is_none()
                    && let Some(Step::Chord(m, Key::Named(n))) = b.steps.first()
                    && n.len() == 1
                    && n.as_bytes()[0].is_ascii_uppercase()
                    && (m.exactly(true, false, false) || m.exactly(false, true, false))
                {
                    conflicts.push(Conflict::about("shadows_terminal_input", &b, Value::Null));
                }
                active.push(b);
            }
        }
        // Across tools (§9.2): only global bindings collide.
        for (i, g) in active.iter().enumerate() {
            if g.scope != Scope::Global {
                continue;
            }
            let gt = self.tool(&g.tool);
            let Some(gs) = first_step(gt, g) else {
                continue;
            };
            for (name, t) in &self.taken {
                if key_overlaps(&gs, t) {
                    conflicts.push(Conflict::about(
                        "taken",
                        g,
                        json!({"other_program": true, "keys": name}),
                    ));
                }
            }
            for (j, b) in active.iter().enumerate() {
                if b.tool == g.tool {
                    continue;
                }
                let Some(bs) = first_step(self.tool(&b.tool), b) else {
                    continue;
                };
                if !key_overlaps(&gs, &bs) {
                    continue;
                }
                if b.scope == Scope::Global {
                    if j < i {
                        continue; // each pair once
                    }
                    let g_first = if g.layer != b.layer {
                        g.layer > b.layer
                    } else {
                        g.tool > b.tool
                    };
                    let (s, o) = if g_first { (g, b) } else { (b, g) };
                    conflicts.push(Conflict::about("cross_tool_global", s, o.kim_ref()));
                } else {
                    conflicts.push(Conflict::about("global_shadows_app", b, g.kim_ref()));
                }
            }
        }
        for c in &mut conflicts {
            c.offers = self.offers(c, &active);
        }
        conflicts.sort_by(|a, b| {
            (severity_rank(a.code), &a.tool, &a.action, &a.keys, a.code).cmp(&(
                severity_rank(b.code),
                &b.tool,
                &b.action,
                &b.keys,
                b.code,
            ))
        });
        Analysis { conflicts, active }
    }

    /// Non-destructive offers (§9.5).
    fn offers(&self, c: &Conflict, active: &[Binding]) -> Vec<Value> {
        let sev = severity(c.code);
        if sev == "info" {
            return Vec::new();
        }
        if matches!(c.code, "unknown_action" | "invalid_binding") {
            return vec![json!({"kind": "reset"})];
        }
        let mut out = Vec::new();
        if let Some(s) = &c.subject {
            for keys in self.suggestions(s, active) {
                out.push(json!({"kind": "rebind", "keys": keys}));
            }
            if !s.is_leader {
                out.push(json!({"kind": "disable"}));
            }
        }
        if sev == "warning" {
            out.push(json!({"kind": "keep"}));
        }
        out
    }

    fn suggestions(&self, s: &Binding, active: &[Binding]) -> Vec<String> {
        let Some((m, k)) = s.single_chord() else {
            return Vec::new();
        };
        let tool = self.tool(&s.tool);
        let decl = tool.action(&s.action);
        let mut out: Vec<String> = Vec::new();
        for (ctrl, alt) in [(false, false), (true, false), (false, true)] {
            if m.altgr && (ctrl || alt) {
                continue;
            }
            let mut c = m.clone();
            let add = |side: &mut Side, on: bool| {
                if on && !side.on() {
                    *side = Side::Any;
                }
            };
            add(&mut c.ctrl, ctrl);
            add(&mut c.alt, alt);
            add(&mut c.shift, true);
            let keys = render_chord(&c, k);
            if keys == s.keys || out.contains(&keys) {
                continue;
            }
            let cand = Binding {
                keys: keys.clone(),
                steps: vec![Step::Chord(c, k.clone())],
                ..s.clone()
            };
            let valid = match decl {
                Some(d) => validate(tool, d, &cand, None).is_none(),
                None => reserved(&cand.steps[0], &cand.scope, true).is_none(),
            };
            if valid && binding_warning(&cand).is_none() && self.free(&cand, s, active) {
                out.push(keys);
                if out.len() == 2 {
                    break;
                }
            }
        }
        out
    }

    /// No error or warning against any known binding or taken chord.
    fn free(&self, cand: &Binding, subject: &Binding, active: &[Binding]) -> bool {
        let tool = self.tool(&cand.tool);
        let Some(cs) = first_step(tool, cand) else {
            return false;
        };
        if cand.scope == Scope::Global && self.taken.iter().any(|(_, t)| key_overlaps(&cs, t)) {
            return false;
        }
        let same_action = |b: &Binding| b.tool == subject.tool && b.action == subject.action;
        active
            .iter()
            .filter(|b| !b.same(subject) && !same_action(b))
            .all(|b| {
                if b.tool == cand.tool {
                    !(b.scope.overlaps(&cand.scope) && relation(tool, cand, b) != Rel::None)
                } else if cand.scope == Scope::Global || b.scope == Scope::Global {
                    first_step(self.tool(&b.tool), b).is_none_or(|bs| !key_overlaps(&cs, &bs))
                } else {
                    true
                }
            })
    }
}

// ------------------------------------------------------------------------------------------
// Scenario evaluation

fn report_json(tools: &[Tool], a: &Analysis) -> Value {
    let mut ts: Vec<&Tool> = tools.iter().collect();
    ts.sort_by(|x, y| x.id.cmp(&y.id));
    let count = |s: &str| a.conflicts.iter().filter(|c| severity(c.code) == s).count();
    json!({
        "tools": ts.iter().map(|t| json!({"tool": t.id, "version": t.version, "source": "manifest", "enabled": t.enabled})).collect::<Vec<_>>(),
        "conflicts": a.conflicts.iter().map(Conflict::to_json).collect::<Vec<_>>(),
        "summary": {"errors": count("error"), "warnings": count("warning"), "infos": count("info")}
    })
}

fn refused(code: &str, exit: u8, conflicts: &[&Conflict]) -> Value {
    json!({
        "outcome": "refused",
        "error": {"code": code, "exit_code": exit},
        "conflicts": conflicts.iter().map(|c| c.to_json()).collect::<Vec<_>>()
    })
}

fn with_override(tool: &Tool, action: &str, entry: Option<Value>) -> Value {
    let mut doc = tool
        .config
        .clone()
        .unwrap_or_else(|| json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": tool.id}));
    let list = doc
        .as_object_mut()
        .expect("config object")
        .entry("overrides")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("overrides array");
    let pos = list
        .iter()
        .position(|o| o.get("action").and_then(Value::as_str) == Some(action));
    match (pos, entry) {
        (Some(p), Some(e)) => list[p] = e,
        (Some(p), None) => {
            list.remove(p);
        }
        (None, Some(e)) => list.push(e),
        (None, None) => {}
    }
    doc
}

/// Evaluates a `set`/`disable`/`reset` request (§9.6).
fn run_request(
    tools: &[Tool],
    taken: &[(String, Step)],
    req: &Value,
    syntax: &Validator<'_>,
) -> Result<Value, String> {
    let get = |k: &str| req.get(k).and_then(Value::as_str).unwrap_or("");
    let flag = |k: &str| req.get(k).and_then(Value::as_bool).unwrap_or(false);
    let (tool_id, action, command) = (get("tool"), get("action"), get("command"));
    let idx = tools.iter().position(|t| t.id == tool_id).ok_or_else(|| {
        format!("request names tool `{tool_id}` that the scenario does not install")
    })?;
    let tool = &tools[idx];
    let Some(decl) = tool.action(action) else {
        return Ok(refused("action_not_found", 3, &[]));
    };
    if !decl.bindable && command != "reset" {
        return Ok(refused("hotkey_reserved", 2, &[]));
    }
    let entry = match command {
        "reset" => None,
        "disable" => Some(json!({"action": action, "disabled": true})),
        _ => {
            let specs = req
                .get("bindings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for spec in &specs {
                let keys = spec_parts(spec).map(|p| p.0).unwrap_or_default();
                let object_ok = spec.is_string() || spec.get("keys").is_some();
                if !object_ok || !syntax.validate(&json!(keys))?.is_empty() {
                    return Ok(refused("hotkey_invalid", 2, &[]));
                }
                let (keys, explicit, trigger) = spec_parts(spec).expect("checked");
                let scope = match (&decl.context, explicit.as_deref()) {
                    (Some(c), _) => Scope::Context(c.clone()),
                    (None, Some("global")) => Scope::Global,
                    (None, Some(_)) => Scope::App,
                    (None, None) => decl.scopes.first().cloned().unwrap_or(Scope::App),
                };
                let b = Binding {
                    tool: tool.id.clone(),
                    action: action.to_string(),
                    steps: parse_binding(&keys)?,
                    keys,
                    scope,
                    trigger,
                    layer: Layer::User,
                    is_leader: false,
                };
                if let Some(mut c) = validate(tool, decl, &b, explicit.as_deref()) {
                    c.offers = Vec::new();
                    let (code, exit) = match (c.code, c.reason) {
                        (
                            "invalid_binding",
                            Some("backend_unsupported" | "trigger_unsupported"),
                        ) => ("hotkey_unsupported", 5),
                        ("invalid_binding", _) => ("hotkey_invalid", 2),
                        _ => ("hotkey_reserved", 2),
                    };
                    let world = World {
                        tools,
                        taken: taken.to_vec(),
                    };
                    c.offers = world.offers(&c, &world.analyze().active);
                    return Ok(refused(code, exit, &[&c]));
                }
            }
            Some(json!({"action": action, "bindings": specs}))
        }
    };
    let mut next: Vec<Tool> = tools.to_vec();
    next[idx].config = Some(with_override(tool, action, entry));
    let mut changes = vec![(action.to_string(), tool.layer_state(action))];
    let mut analysis;
    loop {
        let world = World {
            tools: &next,
            taken: taken.to_vec(),
        };
        analysis = world.analyze();
        let blocking: Vec<&Conflict> = analysis
            .conflicts
            .iter()
            .filter(|c| {
                command == "set" && severity(c.code) == "error" && c.involves(tool_id, action)
            })
            .filter(|c| !(flag("allow_conflict") && c.code == "cross_tool_global"))
            .collect();
        let Some(c) = blocking.first() else { break };
        // --replace takes the binding away from the other action of the same tool.
        let other = if c.tool == tool_id && c.action == action {
            &c.with
        } else {
            &json!({"tool": c.tool, "action": c.action, "keys": c.keys})
        };
        let other_action = other.get("action").and_then(Value::as_str).unwrap_or("");
        let replaceable = flag("replace")
            && matches!(c.code, "duplicate_binding" | "sequence_prefix")
            && other.get("tool").and_then(Value::as_str) == Some(tool_id)
            && !other_action.starts_with('@')
            && !changes.iter().any(|(a, _)| a == other_action);
        if !replaceable {
            return Ok(refused("hotkey_conflict", 7, &blocking));
        }
        let other_keys = other.get("keys").and_then(Value::as_str).unwrap_or("");
        let before = next[idx].layer_state(other_action);
        let rest: Vec<Value> = match next[idx].override_for(other_action) {
            Some(o) => o
                .get("bindings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            None => next[idx]
                .action(other_action)
                .map(|d| d.defaults.clone())
                .unwrap_or_default(),
        }
        .into_iter()
        .filter(|s| spec_parts(s).map(|p| p.0).as_deref() != Some(other_keys))
        .collect();
        let entry = if rest.is_empty() {
            json!({"action": other_action, "disabled": true})
        } else {
            json!({"action": other_action, "bindings": rest})
        };
        let cfg = with_override(&next[idx], other_action, Some(entry));
        next[idx].config = Some(cfg);
        changes.push((other_action.to_string(), before));
    }
    let dry_run = flag("dry_run");
    let config_after = if dry_run {
        tool.config.clone()
    } else {
        next[idx].config.clone()
    };
    Ok(json!({
        "outcome": "applied",
        "written": !dry_run,
        "config_after": config_after,
        "changes": changes.iter().map(|(a, before)| json!({"action": a, "before": before, "after": next[idx].layer_state(a)})).collect::<Vec<_>>(),
        "conflicts": analysis.conflicts.iter().filter(|c| c.involves(tool_id, action)).map(Conflict::to_json).collect::<Vec<_>>()
    }))
}

// ------------------------------------------------------------------------------------------
// Fixture loading and comparison

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: invalid JSON: {e}", path.display()))
}

/// A validator for one `$defs` entry of a schema document.
fn def_schema(schema: &Value, name: &str) -> Value {
    json!({ "$defs": schema["$defs"].clone(), "$ref": format!("#/$defs/{name}") })
}

fn strip_messages(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("message");
            m.values_mut().for_each(strip_messages);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_messages),
        _ => {}
    }
}

pub fn check(contracts: &Path) -> Result<Report, String> {
    let mut report = Report::default();
    let schemas: Vec<Value> = SCHEMAS
        .iter()
        .map(|n| read_json(&contracts.join("schemas").join(format!("{n}.schema.json"))))
        .collect::<Result<_, _>>()?;
    // Shared definitions must be identical wherever they appear.
    for (i, a) in schemas.iter().enumerate() {
        for b in &schemas[i + 1..] {
            for (name, def) in a["$defs"].as_object().into_iter().flatten() {
                if let Some(other) = b["$defs"].get(name)
                    && other != def
                {
                    report.problems.push(format!(
                        "hotkey schemas: `$defs.{name}` differs between {} and {}",
                        a["$id"], b["$id"]
                    ));
                }
            }
        }
    }
    let [config_schema, manifest_schema, cli_schema, _] = &schemas[..] else {
        unreachable!("four schemas")
    };
    let config_v = Validator::new(config_schema);
    let manifest_v = Validator::new(manifest_schema);
    let report_def = def_schema(cli_schema, "check_data");
    let report_v = Validator::new(&report_def);
    let conflict_def = def_schema(cli_schema, "conflict");
    let conflict_v = Validator::new(&conflict_def);
    let change_def = def_schema(cli_schema, "change");
    let change_v = Validator::new(&change_def);
    let syntax_def = def_schema(config_schema, "binding_syntax");
    let syntax_v = Validator::new(&syntax_def);

    let dir = contracts.join("examples").join("hotkey-scenarios-v1");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    for path in files {
        report.scenarios += 1;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        match check_scenario(
            contracts,
            &path,
            &config_v,
            &manifest_v,
            &report_v,
            &conflict_v,
            &change_v,
            &syntax_v,
        ) {
            Ok(problems) => report.problems.extend(
                problems
                    .into_iter()
                    .map(|p| format!("hotkey-scenarios-v1/{name}: {p}")),
            ),
            Err(e) => report
                .problems
                .push(format!("hotkey-scenarios-v1/{name}: {e}")),
        }
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn check_scenario(
    contracts: &Path,
    path: &Path,
    config_v: &Validator<'_>,
    manifest_v: &Validator<'_>,
    report_v: &Validator<'_>,
    conflict_v: &Validator<'_>,
    change_v: &Validator<'_>,
    syntax_v: &Validator<'_>,
) -> Result<Vec<String>, String> {
    let s = read_json(path)?;
    let mut problems = Vec::new();
    let mut tools = Vec::new();
    for t in s["tools"].as_array().into_iter().flatten() {
        let manifest = match &t["manifest"] {
            Value::String(n) => read_json(
                &contracts
                    .join("examples")
                    .join("kim-hotkey-manifest-v1")
                    .join(format!("{n}.json")),
            )?,
            m => m.clone(),
        };
        for e in manifest_v.validate(&manifest)? {
            problems.push(format!("manifest {e}"));
        }
        let config = (!t["config"].is_null()).then(|| t["config"].clone());
        if let Some(c) = &config {
            for e in config_v.validate(c)? {
                problems.push(format!("config {e}"));
            }
            if c["tool"] != manifest["tool"] {
                problems.push("config names another tool than its manifest".into());
            }
        }
        tools.push(Tool::from_docs(&manifest, config)?);
    }
    let taken: Vec<(String, Step)> = s["environment"]["taken"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|k| parse_step(k).map(|st| (k.to_string(), st)))
        .collect::<Result<_, _>>()?;
    let mut expected = s["expected"].clone();
    let validate_list = |v: &Validator<'_>,
                         list: &Value,
                         what: &str,
                         problems: &mut Vec<String>|
     -> Result<(), String> {
        for item in list.as_array().into_iter().flatten() {
            for e in v.validate(item)? {
                problems.push(format!("expected {what} {e}"));
            }
        }
        Ok(())
    };
    let computed = if s.get("request").is_some() {
        validate_list(
            conflict_v,
            &expected["conflicts"],
            "conflict",
            &mut problems,
        )?;
        validate_list(change_v, &expected["changes"], "change", &mut problems)?;
        if let Some(c) = expected.get("config_after").filter(|c| !c.is_null()) {
            for e in config_v.validate(c)? {
                problems.push(format!("expected config_after {e}"));
            }
        }
        run_request(&tools, &taken, &s["request"], syntax_v)?
    } else {
        for e in report_v.validate(&expected["report"])? {
            problems.push(format!("expected report {e}"));
        }
        let world = World {
            tools: &tools,
            taken,
        };
        json!({ "report": report_json(&tools, &world.analyze()) })
    };
    strip_messages(&mut expected);
    if computed != expected {
        problems.push(format!(
            "expected result differs from the reference checker; computed:\n{}",
            serde_json::to_string_pretty(&computed).unwrap_or_default()
        ));
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("docs")
            .join("contracts")
    }

    #[test]
    fn parse_and_render_round_trip() {
        for s in [
            "Ctrl+Shift+T",
            "LCtrl+RAlt+F5",
            "AltGr+Shift+Q",
            "Ctrl+code:KeyZ",
            "char:?",
            "F1",
        ] {
            let steps = parse_binding(s).unwrap();
            let [Step::Chord(m, k)] = steps.as_slice() else {
                panic!("{s}")
            };
            assert_eq!(render_chord(m, k), s);
        }
        assert!(parse_step("Shift+Ctrl+T").is_err(), "non-canonical order");
        assert!(parse_step("Hyper+K").is_err());
        assert_eq!(
            parse_binding("@prefix char:c").unwrap()[0],
            Step::Leader("prefix".into())
        );
    }

    #[test]
    fn generic_and_side_specific_modifiers_overlap() {
        let a = parse_step("Ctrl+T").unwrap();
        assert!(key_overlaps(&a, &parse_step("LCtrl+T").unwrap()));
        assert!(!key_overlaps(
            &parse_step("LCtrl+T").unwrap(),
            &parse_step("RCtrl+T").unwrap()
        ));
        assert!(!key_overlaps(&a, &parse_step("Ctrl+Shift+T").unwrap()));
        assert!(key_overlaps(
            &parse_step("char:c").unwrap(),
            &parse_step("C").unwrap()
        ));
        assert!(key_overlaps(
            &parse_step("char:C").unwrap(),
            &parse_step("Shift+C").unwrap()
        ));
        assert!(!key_overlaps(
            &parse_step("char:c").unwrap(),
            &parse_step("Shift+C").unwrap()
        ));
    }

    /// The scope-independent reserved rules exist twice: as `reserved_free` in the schemas and
    /// natively here. They must agree.
    #[test]
    fn native_reserved_rules_agree_with_the_schema() {
        let schema = read_json(&root().join("schemas").join("kim-hotkeys-v1.schema.json")).unwrap();
        let def = def_schema(&schema, "reserved_free");
        let v = Validator::new(&def);
        let chords = [
            "Ctrl+Alt+Delete",
            "LCtrl+RAlt+Shift+NumpadDecimal",
            "Win+L",
            "Ctrl+LWin+D",
            "Ctrl+Shift+Escape",
            "Ctrl+Escape",
            "Alt+Tab",
            "Ctrl+Alt+Shift+Tab",
            "Alt+Escape",
            "Alt+Shift+Escape",
            "Alt+Shift+PrintScreen",
            "Alt+F4",
            "Ctrl+Alt+Shift+Escape",
            "HangulMode",
            "Ctrl+Hanja",
            "Ctrl+code:Lang1",
            "Ctrl+Alt+Escape",
            "Ctrl+Alt+F4",
            "AltGr+Tab",
            "Ctrl+T",
            "Shift+F10",
            "F12",
            "Ctrl+Shift+Delete",
            "Alt+Shift+F4",
        ];
        for c in chords {
            let schema_says = !v.validate(&json!(c)).unwrap().is_empty();
            // Context scope and a later step: only the scope-independent rules apply.
            let native =
                reserved(&parse_step(c).unwrap(), &Scope::Context("x".into()), false).is_some();
            assert_eq!(schema_says, native, "{c}");
        }
    }

    #[test]
    fn scope_dependent_reservations() {
        let r = |c: &str, s: Scope| reserved(&parse_step(c).unwrap(), &s, true).map(|x| x.0);
        assert_eq!(r("F12", Scope::Global), Some("os_reserved"));
        assert_eq!(r("F12", Scope::App), None);
        assert_eq!(r("Tab", Scope::App), Some("kim_reserved"));
        assert_eq!(r("Tab", Scope::Context("overlay".into())), None);
        assert_eq!(
            r("Shift+A", Scope::Global),
            Some("kim_reserved"),
            "typing key"
        );
        assert_eq!(r("char:?", Scope::App), Some("kim_reserved"), "typing key");
        assert_eq!(r("F1", Scope::Global), None);
        assert_eq!(r("Ctrl+Up", Scope::App), None);
    }

    #[test]
    fn repository_scenarios_match_the_reference_checker() {
        let report = check(&root()).expect("scenarios readable");
        assert!(report.scenarios >= 16, "expected the documented scenarios");
        assert!(report.problems.is_empty(), "{}", report.problems.join("\n"));
    }
}
