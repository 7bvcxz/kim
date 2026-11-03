//! Layering, validation and conflict detection (docs/HOTKEY_CONTRACT.md §8.2, §9).
//!
//! [`analyze`] takes every tool it knows (its manifest, user layer, active profile and whether it
//! runs in safe mode) and the chords other programs are known to own, and returns the active
//! bindings plus a deterministic conflict report: stable codes and severities, one subject per
//! conflict, the contract's order, and non-destructive remap offers that are never applied by
//! themselves (`KIM-FR-046`). Nothing here touches the OS.

use crate::keys::{Chord, Key, Sequence, Side, Step, steps_overlap};
use crate::layer::{Layer, Override, UserLayer};
use crate::manifest::{ActionDecl, Backend, BindingSpec, Manifest, ScopeKind, Trigger};
use crate::reserved::{self, Owner};
use crate::scope::Scope;
use serde_json::{Map, Value, json};

/// Conflict codes (§9.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Code {
    OsReserved,
    KimReserved,
    ImeReserved,
    InvalidBinding,
    DuplicateBinding,
    SequencePrefix,
    CrossToolGlobal,
    GlobalShadowsApp,
    Taken,
    HookOverridesGlobal,
    OsShortcut,
    AltgrCollision,
    ImeToggle,
    UnknownAction,
    ContextOverride,
    DefaultSuppressed,
    ShadowsTerminalInput,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::OsReserved => "os_reserved",
            Code::KimReserved => "kim_reserved",
            Code::ImeReserved => "ime_reserved",
            Code::InvalidBinding => "invalid_binding",
            Code::DuplicateBinding => "duplicate_binding",
            Code::SequencePrefix => "sequence_prefix",
            Code::CrossToolGlobal => "cross_tool_global",
            Code::GlobalShadowsApp => "global_shadows_app",
            Code::Taken => "taken",
            Code::HookOverridesGlobal => "hook_overrides_global",
            Code::OsShortcut => "os_shortcut",
            Code::AltgrCollision => "altgr_collision",
            Code::ImeToggle => "ime_toggle",
            Code::UnknownAction => "unknown_action",
            Code::ContextOverride => "context_override",
            Code::DefaultSuppressed => "default_suppressed",
            Code::ShadowsTerminalInput => "shadows_terminal_input",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Code::OsReserved
            | Code::KimReserved
            | Code::ImeReserved
            | Code::InvalidBinding
            | Code::DuplicateBinding
            | Code::SequencePrefix
            | Code::CrossToolGlobal => Severity::Error,
            Code::ContextOverride | Code::DefaultSuppressed | Code::ShadowsTerminalInput => {
                Severity::Info
            }
            _ => Severity::Warning,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s {
            "error" => Some(Severity::Error),
            "warning" => Some(Severity::Warning),
            "info" => Some(Severity::Info),
            _ => None,
        }
    }
}

/// Why a binding with valid syntax cannot be used (`invalid_binding`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    ScopeNotAllowed,
    BackendUnsupported,
    TriggerUnsupported,
    UnknownLeader,
    NotBindable,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::ScopeNotAllowed => "scope_not_allowed",
            Reason::BackendUnsupported => "backend_unsupported",
            Reason::TriggerUnsupported => "trigger_unsupported",
            Reason::UnknownLeader => "unknown_leader",
            Reason::NotBindable => "not_bindable",
        }
    }

    fn sentence(self) -> &'static str {
        match self {
            Reason::ScopeNotAllowed => "the action is not available in that scope",
            Reason::BackendUnsupported => "the tool's input backend cannot receive it",
            Reason::TriggerUnsupported => "release and hold work only for single chords",
            Reason::UnknownLeader => "it names a leader the tool does not have",
            Reason::NotBindable => "the action's keys are fixed",
        }
    }
}

/// The other side of a conflict (§9.4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum With {
    None,
    Kim {
        tool: String,
        action: String,
        keys: String,
        scope: String,
    },
    Reserved {
        owner: Owner,
        keys: String,
        name: &'static str,
    },
    OtherProgram {
        keys: String,
    },
}

impl With {
    pub fn to_json(&self) -> Value {
        match self {
            With::None => Value::Null,
            With::Kim {
                tool,
                action,
                keys,
                scope,
            } => json!({"tool": tool, "action": action, "keys": keys, "scope": scope}),
            With::Reserved { owner, keys, name } => {
                json!({"reserved": owner.as_str(), "keys": keys, "name": name})
            }
            With::OtherProgram { keys } => json!({"other_program": true, "keys": keys}),
        }
    }

    fn kim(&self) -> Option<(&str, &str, &str)> {
        match self {
            With::Kim {
                tool, action, keys, ..
            } => Some((tool, action, keys)),
            _ => None,
        }
    }
}

/// A non-destructive remap offer (§9.5). Never applied by itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Offer {
    Rebind(String),
    Disable,
    Keep,
    Reset,
}

impl Offer {
    pub fn to_json(&self) -> Value {
        match self {
            Offer::Rebind(k) => json!({"kind": "rebind", "keys": k}),
            Offer::Disable => json!({"kind": "disable"}),
            Offer::Keep => json!({"kind": "keep"}),
            Offer::Reset => json!({"kind": "reset"}),
        }
    }
}

/// One effective (candidate) binding of a tool.
#[derive(Clone, PartialEq, Debug)]
pub struct Binding {
    pub tool: String,
    /// The action ID, or `@<leader>` for a leader's own chord.
    pub action: String,
    pub keys: String,
    pub seq: Sequence,
    pub scope: Scope,
    pub trigger: Trigger,
    pub repeat: bool,
    pub layer: Layer,
    pub is_leader: bool,
}

impl Binding {
    fn same(&self, o: &Binding) -> bool {
        self.tool == o.tool && self.action == o.action && self.keys == o.keys
    }

    fn table(&self) -> Option<&str> {
        if self.is_leader {
            None
        } else {
            self.seq.table()
        }
    }

    fn kim_ref(&self) -> With {
        With::Kim {
            tool: self.tool.clone(),
            action: self.action.clone(),
            keys: self.keys.clone(),
            scope: self.scope.to_string(),
        }
    }
}

/// One conflict (§9.3–§9.5).
#[derive(Clone, PartialEq, Debug)]
pub struct Conflict {
    pub code: Code,
    pub reason: Option<Reason>,
    pub tool: String,
    pub action: String,
    pub keys: Option<String>,
    pub scope: Option<String>,
    pub source: Layer,
    pub with: With,
    pub offers: Vec<Offer>,
    /// A human sentence (not part of the stable contract).
    pub message: String,
    subject: Option<Binding>,
}

impl Conflict {
    fn about(code: Code, b: &Binding, with: With) -> Conflict {
        Conflict {
            code,
            reason: None,
            tool: b.tool.clone(),
            action: b.action.clone(),
            keys: Some(b.keys.clone()),
            scope: Some(b.scope.to_string()),
            source: b.layer,
            with,
            offers: Vec::new(),
            message: String::new(),
            subject: Some(b.clone()),
        }
    }

    pub fn severity(&self) -> Severity {
        self.code.severity()
    }

    /// Whether the conflict is about this action or names it as the other side.
    pub fn involves(&self, tool: &str, action: &str) -> bool {
        (self.tool == tool && self.action == action)
            || self
                .with
                .kim()
                .is_some_and(|(t, a, _)| t == tool && a == action)
    }

    pub fn with(&self) -> &With {
        &self.with
    }

    /// The `hotkeys-cli-v1` conflict object.
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("code".into(), json!(self.code.as_str()));
        m.insert("severity".into(), json!(self.severity().as_str()));
        if let Some(r) = self.reason {
            m.insert("reason".into(), json!(r.as_str()));
        }
        m.insert("tool".into(), json!(self.tool));
        m.insert("action".into(), json!(self.action));
        m.insert("keys".into(), json!(self.keys));
        m.insert("scope".into(), json!(self.scope));
        m.insert("source".into(), json!(self.source.as_str()));
        m.insert("with".into(), self.with.to_json());
        m.insert(
            "offers".into(),
            Value::Array(self.offers.iter().map(Offer::to_json).collect()),
        );
        if !self.message.is_empty() {
            m.insert("message".into(), json!(self.message));
        }
        Value::Object(m)
    }

    /// One line for human output and error details: `code: …`.
    pub fn detail(&self) -> String {
        format!("{}: {}", self.code.as_str(), self.message)
    }
}

/// One tool as the analysis sees it.
#[derive(Clone, PartialEq, Debug)]
pub struct ToolState {
    pub manifest: Manifest,
    /// The user layer (`None`: no file, or safe mode).
    pub layer: Option<UserLayer>,
    /// The active profile, if the tool says one is active.
    pub profile: Option<String>,
    /// Safe mode (§8.3): the file is invalid or newer; defaults apply and nothing is global.
    pub safe_mode: bool,
}

/// `{source, bindings}` of an action in a layer (change records, §10.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayerState {
    /// `default`, `user` or `disabled`.
    pub source: &'static str,
    pub bindings: Vec<String>,
}

impl LayerState {
    pub fn to_json(&self) -> Value {
        json!({"source": self.source, "bindings": self.bindings})
    }
}

fn spec_keys(specs: &[BindingSpec]) -> Vec<String> {
    specs.iter().map(|s| s.keys().to_string()).collect()
}

impl ToolState {
    pub fn new(manifest: Manifest, layer: Option<UserLayer>) -> ToolState {
        ToolState {
            manifest,
            layer,
            profile: None,
            safe_mode: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.manifest.tool
    }

    /// The tool-wide switch (`enabled`).
    pub fn enabled(&self) -> bool {
        self.layer.as_ref().is_none_or(UserLayer::is_enabled)
    }

    pub fn scope_enabled(&self, scope: &Scope) -> bool {
        if self.safe_mode && *scope == Scope::Global {
            return false;
        }
        !self
            .layer
            .as_ref()
            .is_some_and(|l| l.scope_disabled(scope.family()))
    }

    pub fn effective_override(&self, action: &str) -> Option<(&Override, Layer)> {
        self.layer
            .as_ref()?
            .effective_override(action, self.profile.as_deref())
    }

    /// The current chord of a leader and where it comes from.
    pub fn leader_chord(&self, id: &str) -> Option<(String, Layer)> {
        let decl = self.manifest.leader(id)?;
        match self
            .layer
            .as_ref()
            .and_then(|l| l.leaders.as_ref())
            .and_then(|m| m.get(id))
        {
            Some(k) => Some((k.clone(), Layer::User)),
            None => Some((decl.default.clone(), Layer::Default)),
        }
    }

    /// The bindings an action has after layering; `None` when it is disabled.
    pub fn specs(&self, decl: &ActionDecl) -> Option<(Vec<BindingSpec>, Layer)> {
        match self.effective_override(&decl.id) {
            Some((o, _)) if o.is_disabled() => None,
            Some((o, layer)) => Some((o.specs().to_vec(), layer)),
            None => Some((decl.defaults.clone(), Layer::Default)),
        }
    }

    /// The layer state of an action or `@leader` (profiles count as the user's choice).
    pub fn layer_state(&self, action: &str) -> LayerState {
        if let Some(id) = action.strip_prefix('@') {
            return match self.leader_chord(id) {
                Some((k, Layer::Default)) => LayerState {
                    source: "default",
                    bindings: vec![k],
                },
                Some((k, _)) => LayerState {
                    source: "user",
                    bindings: vec![k],
                },
                None => LayerState {
                    source: "default",
                    bindings: Vec::new(),
                },
            };
        }
        match self.effective_override(action) {
            Some((o, _)) if o.is_disabled() => LayerState {
                source: "disabled",
                bindings: Vec::new(),
            },
            Some((o, _)) => LayerState {
                source: "user",
                bindings: spec_keys(o.specs()),
            },
            None => LayerState {
                source: "default",
                bindings: self
                    .manifest
                    .action(action)
                    .map(|a| spec_keys(&a.defaults))
                    .unwrap_or_default(),
            },
        }
    }

    /// The scope a binding spec of `decl` is active in.
    pub fn spec_scope(decl: &ActionDecl, spec: &BindingSpec) -> Scope {
        match (&decl.context, spec.scope()) {
            (Some(c), _) => Scope::Context(c.clone()),
            (None, Some("global")) => Scope::Global,
            (None, Some(_)) => Scope::App,
            (None, None) => match decl.scopes.first() {
                Some(ScopeKind::Global) => Scope::Global,
                _ => Scope::App,
            },
        }
    }

    fn expand(&self, step: &Step) -> Option<Step> {
        match step {
            Step::Leader(l) => Chord::parse(&self.leader_chord(l)?.0).ok().map(Step::Chord),
            Step::Chord(_) => None,
        }
    }

    fn expanded(&self, step: &Step) -> Step {
        self.expand(step).unwrap_or_else(|| step.clone())
    }
}

/// Per-binding validity (§7, §4.2, §5): the first problem, as a conflict.
fn validate(
    tool: &ToolState,
    decl: &ActionDecl,
    b: &Binding,
    explicit: Option<&str>,
) -> Option<Conflict> {
    let invalid = |reason: Reason| {
        let mut c = Conflict::about(Code::InvalidBinding, b, With::None);
        c.reason = Some(reason);
        Some(c)
    };
    if b.layer >= Layer::User && !decl.bindable {
        return invalid(Reason::NotBindable);
    }
    if decl.context.is_some() && explicit.is_some() {
        return invalid(Reason::ScopeNotAllowed);
    }
    if decl.context.is_none() {
        let allowed = decl.scopes.iter().any(|s| match s {
            ScopeKind::Global => b.scope == Scope::Global,
            ScopeKind::App => b.scope == Scope::App,
        });
        if !allowed {
            return invalid(Reason::ScopeNotAllowed);
        }
    }
    if decl.bindable {
        for (i, step) in b.seq.steps().iter().enumerate() {
            let step = tool.expanded(step);
            if let Step::Chord(c) = &step
                && let Some(r) = reserved::chord(c, &b.scope, i == 0)
            {
                return Some(Conflict::about(
                    r.code,
                    b,
                    With::Reserved {
                        owner: r.owner,
                        keys: c.to_string(),
                        name: r.name,
                    },
                ));
            }
        }
    }
    if b.seq
        .steps()
        .iter()
        .any(|s| matches!(s, Step::Leader(l) if tool.leader_chord(l).is_none()))
    {
        return invalid(Reason::UnknownLeader);
    }
    if b.scope == Scope::Global {
        let beyond_register = b.seq.len() > 1
            || b.trigger != Trigger::Press
            || b.seq.steps().iter().any(|s| match s {
                Step::Chord(c) => c.mods.side_specific() || !matches!(c.key, Key::Named(_)),
                Step::Leader(_) => true,
            });
        let backend = tool.manifest.backend;
        if backend == Backend::Focus || (backend == Backend::Register && beyond_register) {
            return invalid(Reason::BackendUnsupported);
        }
    }
    if b.trigger != Trigger::Press && b.seq.len() > 1 {
        return invalid(Reason::TriggerUnsupported);
    }
    None
}

/// Single-binding warnings (§9.3): `os_shortcut`, `altgr_collision`, `ime_toggle`.
fn binding_warning(b: &Binding) -> Option<Conflict> {
    let chords = b.seq.steps().iter().filter_map(|s| match s {
        Step::Chord(c) => Some(c),
        Step::Leader(_) => None,
    });
    for (i, c) in chords.enumerate() {
        let (m, k) = (&c.mods, &c.key);
        let named = |n: &str| k.is_named(n);
        if i == 0
            && matches!(b.scope, Scope::Global | Scope::App)
            && b.seq.len() == 1
            && ((m.exactly(false, false, false) && named("PrintScreen"))
                || (m.exactly(false, true, false) && (named("PrintScreen") || named("Space"))))
        {
            return Some(Conflict::about(Code::OsShortcut, b, With::None));
        }
        if m.ctrl.on() && m.alt.on() && !m.altgr && !m.win.on() && k.is_printable() {
            return Some(Conflict::about(Code::AltgrCollision, b, With::None));
        }
        if (m.exactly(true, false, false) && named("Space"))
            || (m.exactly(false, false, true) && named("Space"))
            || (m.exactly(false, true, false) && matches!(k, Key::Code("Backquote")))
        {
            return Some(Conflict::about(Code::ImeToggle, b, With::None));
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
fn relation(tool: &ToolState, a: &Binding, b: &Binding) -> Rel {
    let seq = |x: &Binding, skip: usize| -> Vec<Step> {
        x.seq.steps()[skip..]
            .iter()
            .map(|s| tool.expanded(s))
            .collect()
    };
    let (sa, sb) = match (a.table(), b.table()) {
        (Some(x), Some(y)) if x == y => (seq(a, 1), seq(b, 1)),
        (None, None) => (seq(a, 0), seq(b, 0)),
        _ => return Rel::None,
    };
    let n = sa.len().min(sb.len());
    if n == 0 || !(0..n).all(|i| steps_overlap(&sa[i], &sb[i])) {
        Rel::None
    } else if sa.len() == sb.len() {
        Rel::Equal
    } else {
        Rel::Prefix
    }
}

/// The first step a key press must match, with leaders resolved (cross-tool checks).
fn first_step(tool: &ToolState, b: &Binding) -> Option<Step> {
    if b.table().is_some() {
        return None; // the leader binding itself carries the first step
    }
    b.seq.steps().first().map(|s| tool.expanded(s))
}

/// The result of [`analyze`].
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    /// In the contract's order: errors, warnings, infos; then tool, action, keys, code.
    pub conflicts: Vec<Conflict>,
    /// Bindings that would fire (valid, not suppressed, not the subject of an error).
    pub active: Vec<Binding>,
}

impl Analysis {
    pub fn count(&self, s: Severity) -> usize {
        self.conflicts.iter().filter(|c| c.severity() == s).count()
    }

    pub fn is_active(&self, tool: &str, action: &str, keys: &str) -> bool {
        self.active
            .iter()
            .any(|b| b.tool == tool && b.action == action && b.keys == keys)
    }
}

struct World<'a> {
    tools: &'a [ToolState],
    taken: &'a [Step],
}

/// Runs the full check (§9.2) over `tools`; `taken` are chords other programs own (known only
/// from a failed registration, §9.2).
pub fn analyze(tools: &[ToolState], taken: &[Step]) -> Analysis {
    let w = World { tools, taken };
    let mut a = w.analyze();
    for c in &mut a.conflicts {
        c.message = message(tools, c);
    }
    a
}

impl World<'_> {
    fn tool(&self, id: &str) -> &ToolState {
        self.tools
            .iter()
            .find(|t| t.id() == id)
            .expect("binding of a known tool")
    }

    /// Bindings of one tool before cross-binding analysis, plus per-binding conflicts.
    fn candidates(&self, tool: &ToolState, out: &mut Vec<Binding>, conflicts: &mut Vec<Conflict>) {
        for l in &tool.manifest.leaders {
            let (keys, layer) = tool.leader_chord(&l.id).expect("declared leader");
            if !tool.scope_enabled(&Scope::App) {
                continue;
            }
            let Ok(seq) = Sequence::parse(&keys) else {
                continue;
            };
            let b = Binding {
                tool: tool.id().to_string(),
                action: format!("@{}", l.id),
                seq,
                keys,
                scope: Scope::App,
                trigger: Trigger::Press,
                repeat: false,
                layer,
                is_leader: true,
            };
            if let Some(w) = binding_warning(&b) {
                conflicts.push(w);
            }
            out.push(b);
        }
        let mut overrides: Vec<(&Override, Layer)> = Vec::new();
        if let Some(l) = &tool.layer {
            overrides.extend(l.overrides_in(None).iter().map(|o| (o, Layer::User)));
            if let Some(p) = &tool.profile {
                overrides.extend(l.overrides_in(Some(p)).iter().map(|o| (o, Layer::Profile)));
            }
        }
        for (o, layer) in overrides {
            if tool.manifest.action(&o.action).is_none() {
                conflicts.push(Conflict {
                    code: Code::UnknownAction,
                    reason: None,
                    tool: tool.id().to_string(),
                    action: o.action.clone(),
                    keys: o.specs().first().map(|s| s.keys().to_string()),
                    scope: None,
                    source: layer,
                    with: With::None,
                    offers: Vec::new(),
                    message: String::new(),
                    subject: None,
                });
            }
        }
        for decl in &tool.manifest.actions {
            let Some((specs, layer)) = tool.specs(decl) else {
                continue;
            };
            for spec in &specs {
                let scope = ToolState::spec_scope(decl, spec);
                if !tool.scope_enabled(&scope) {
                    continue;
                }
                let Ok(seq) = Sequence::parse(spec.keys()) else {
                    continue; // unreachable for validated layers and manifests
                };
                let b = Binding {
                    tool: tool.id().to_string(),
                    action: decl.id.clone(),
                    keys: spec.keys().to_string(),
                    seq,
                    scope,
                    trigger: spec.trigger(),
                    repeat: matches!(spec, BindingSpec::Object(o) if o.repeat == Some(true)),
                    layer,
                    is_leader: false,
                };
                if let Some(c) = validate(tool, decl, &b, spec.scope()) {
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
        for tool in self.tools.iter().filter(|t| t.enabled()) {
            let mut cands = Vec::new();
            self.candidates(tool, &mut cands, &mut conflicts);
            // A user binding beats a default of another action (§9.1).
            let mut suppressed = vec![false; cands.len()];
            for (i, d) in cands.iter().enumerate() {
                if d.layer != Layer::Default || d.is_leader {
                    continue;
                }
                if let Some(u) = cands.iter().find(|u| {
                    u.layer > Layer::Default
                        && !u.is_leader
                        && u.action != d.action
                        && u.scope.overlaps(&d.scope)
                        && relation(tool, d, u) == Rel::Equal
                }) {
                    suppressed[i] = true;
                    conflicts.push(Conflict::about(Code::DefaultSuppressed, d, u.kim_ref()));
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
                            Code::DuplicateBinding
                        } else {
                            Code::SequencePrefix
                        };
                        let a_first = if a.is_leader != b.is_leader {
                            b.is_leader
                        } else if a.layer != b.layer {
                            a.layer > b.layer
                        } else if code == Code::SequencePrefix {
                            a.seq.len() < b.seq.len()
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
                        conflicts.push(Conflict::about(Code::ContextOverride, app, ctx.kim_ref()));
                    }
                }
            }
            let passthrough = tool.manifest.has_passthrough();
            for (b, off) in cands.into_iter().zip(inactive) {
                if off {
                    continue;
                }
                if passthrough
                    && b.scope == Scope::App
                    && b.table().is_none()
                    && let Some(Step::Chord(c)) = b.seq.steps().first()
                    && let Key::Named(n) = c.key
                    && n.len() == 1
                    && n.as_bytes()[0].is_ascii_uppercase()
                    && (c.mods.exactly(true, false, false) || c.mods.exactly(false, true, false))
                {
                    conflicts.push(Conflict::about(Code::ShadowsTerminalInput, &b, With::None));
                }
                active.push(b);
            }
        }
        // Across tools (§9.2): only global bindings collide.
        for (i, g) in active.iter().enumerate() {
            if g.scope != Scope::Global {
                continue;
            }
            let Some(gs) = first_step(self.tool(&g.tool), g) else {
                continue;
            };
            for t in self.taken {
                if steps_overlap(&gs, t) {
                    conflicts.push(Conflict::about(
                        Code::Taken,
                        g,
                        With::OtherProgram {
                            keys: t.to_string(),
                        },
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
                if !steps_overlap(&gs, &bs) {
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
                    conflicts.push(Conflict::about(Code::CrossToolGlobal, s, o.kim_ref()));
                } else {
                    conflicts.push(Conflict::about(Code::GlobalShadowsApp, b, g.kim_ref()));
                }
            }
        }
        for c in &mut conflicts {
            c.offers = self.offers(c, &active);
        }
        sort(&mut conflicts);
        Analysis { conflicts, active }
    }

    /// Non-destructive offers (§9.5).
    fn offers(&self, c: &Conflict, active: &[Binding]) -> Vec<Offer> {
        let sev = c.severity();
        if sev == Severity::Info {
            return Vec::new();
        }
        if matches!(c.code, Code::UnknownAction | Code::InvalidBinding) {
            return vec![Offer::Reset];
        }
        let mut out = Vec::new();
        if let Some(s) = &c.subject {
            out.extend(self.suggestions(s, active).into_iter().map(Offer::Rebind));
            if !s.is_leader {
                out.push(Offer::Disable);
            }
        }
        if sev == Severity::Warning {
            out.push(Offer::Keep);
        }
        out
    }

    fn suggestions(&self, s: &Binding, active: &[Binding]) -> Vec<String> {
        let Some(chord) = s.seq.single_chord() else {
            return Vec::new();
        };
        let tool = self.tool(&s.tool);
        let decl = tool.manifest.action(&s.action);
        let mut out: Vec<String> = Vec::new();
        for (ctrl, alt) in [(false, false), (true, false), (false, true)] {
            if chord.mods.altgr && (ctrl || alt) {
                continue;
            }
            let mut c = *chord;
            let add = |side: &mut Side, on: bool| {
                if on && !side.on() {
                    *side = Side::Any;
                }
            };
            add(&mut c.mods.ctrl, ctrl);
            add(&mut c.mods.alt, alt);
            add(&mut c.mods.shift, true);
            let keys = c.to_string();
            if keys == s.keys || out.contains(&keys) {
                continue;
            }
            let cand = Binding {
                keys: keys.clone(),
                seq: Sequence::from_steps(vec![Step::Chord(c)]),
                ..s.clone()
            };
            let valid = match decl {
                Some(d) => validate(tool, d, &cand, None).is_none(),
                None => reserved::chord(&c, &cand.scope, true).is_none(),
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
        if cand.scope == Scope::Global && self.taken.iter().any(|t| steps_overlap(&cs, t)) {
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
                    first_step(self.tool(&b.tool), b).is_none_or(|bs| !steps_overlap(&cs, &bs))
                } else {
                    true
                }
            })
    }
}

/// Checks one binding a user wants to set, before any conflict with other bindings (§9.6 1):
/// the first per-binding problem (reserved chord, scope, backend, trigger, leader), with its
/// offers computed against the current bindings. `action` may be `@<leader>`; then `spec` is the
/// leader's new chord. Errors only on bad syntax.
pub fn validate_spec(
    tools: &[ToolState],
    taken: &[Step],
    idx: usize,
    action: &str,
    spec: &BindingSpec,
    layer: Layer,
) -> Result<Option<Conflict>, crate::keys::ParseError> {
    let tool = &tools[idx];
    let seq = Sequence::parse(spec.keys())?;
    let leader = action.starts_with('@');
    let decl = tool.manifest.action(action);
    let scope = match decl {
        Some(d) => ToolState::spec_scope(d, spec),
        None => Scope::App,
    };
    let b = Binding {
        tool: tool.id().to_string(),
        action: action.to_string(),
        keys: spec.keys().to_string(),
        seq,
        scope,
        trigger: spec.trigger(),
        repeat: false,
        layer,
        is_leader: leader,
    };
    let found = match decl {
        Some(d) => validate(tool, d, &b, spec.scope()),
        None => b.seq.steps().first().and_then(|s| match s {
            Step::Chord(c) => reserved::chord(c, &Scope::App, true).map(|r| {
                Conflict::about(
                    r.code,
                    &b,
                    With::Reserved {
                        owner: r.owner,
                        keys: c.to_string(),
                        name: r.name,
                    },
                )
            }),
            Step::Leader(_) => None,
        }),
    };
    Ok(found.map(|mut c| {
        let w = World { tools, taken };
        let active = w.analyze().active;
        c.offers = w.offers(&c, &active);
        c.message = message(tools, &c);
        c
    }))
}

/// The contract's order (§9.4): errors, warnings, infos; then tool, action, keys and code.
pub fn sort(conflicts: &mut [Conflict]) {
    conflicts.sort_by(|a, b| {
        (a.severity(), &a.tool, &a.action, &a.keys, a.code.as_str()).cmp(&(
            b.severity(),
            &b.tool,
            &b.action,
            &b.keys,
            b.code.as_str(),
        ))
    });
}

fn title<'a>(tools: &'a [ToolState], tool: &str, action: &'a str) -> &'a str {
    if let Some(l) = action.strip_prefix('@') {
        return tools
            .iter()
            .find(|t| t.id() == tool)
            .and_then(|t| t.manifest.leader(l))
            .map_or(action, |d| d.title.as_str());
    }
    tools
        .iter()
        .find(|t| t.id() == tool)
        .and_then(|t| t.manifest.action(action))
        .map_or(action, |a| a.title.as_str())
}

/// A human sentence for a conflict. Names tools, actions and keys; never typed text.
fn message(tools: &[ToolState], c: &Conflict) -> String {
    let keys = c.keys.as_deref().unwrap_or("");
    let me = format!("{}'s \"{}\"", c.tool, title(tools, &c.tool, &c.action));
    let other = match &c.with {
        With::Kim { tool, action, .. } => format!("{tool}'s \"{}\"", title(tools, tool, action)),
        _ => String::new(),
    };
    match c.code {
        Code::OsReserved | Code::KimReserved | Code::ImeReserved => {
            let (owner, name) = match &c.with {
                With::Reserved { owner, name, .. } => (
                    match owner {
                        Owner::Os => "the operating system",
                        Owner::Ime => "the input method",
                        Owner::Kim | Owner::Emergency => "Kim",
                    },
                    *name,
                ),
                _ => ("Kim", ""),
            };
            format!("{keys} is reserved by {owner} ({name}); {me} cannot use it.")
        }
        Code::InvalidBinding => format!(
            "{keys} cannot be used for {me}: {}.",
            c.reason.map_or("invalid", Reason::sentence)
        ),
        Code::DuplicateBinding => {
            format!("{keys} is also bound to {other}; {me} does not fire on {keys}.")
        }
        Code::SequencePrefix => {
            let ok = c.with.kim().map_or("", |(_, _, k)| k);
            format!("{keys} and {ok} ({other}) start with the same keys; {me} does not fire.")
        }
        Code::CrossToolGlobal => format!(
            "{me} and {other} both want the global chord {keys}; the first tool to register it gets it."
        ),
        Code::GlobalShadowsApp => {
            let wt = c.with.kim().map_or("", |(t, _, _)| t);
            format!(
                "While {wt} runs, {keys} triggers {other} everywhere, so {me} never receives {keys}."
            )
        }
        Code::Taken => format!(
            "Another program already registered {keys}; {me} stays inactive. Kim never takes a shortcut away."
        ),
        Code::HookOverridesGlobal => {
            format!("{me} sees {keys} before {other} because it uses a keyboard hook.")
        }
        Code::OsShortcut => format!(
            "{keys} replaces what Windows does for {keys} while {} runs.",
            c.tool
        ),
        Code::AltgrCollision => format!(
            "{keys} may block a character typed with AltGr on some keyboard layouts ({me})."
        ),
        Code::ImeToggle => {
            format!("{keys} may block switching the input method on some systems ({me}).")
        }
        Code::UnknownAction => format!(
            "hotkeys.json overrides `{}`, which this version of {} does not have; it is ignored and kept.",
            c.action, c.tool
        ),
        Code::ContextOverride => {
            format!("Inside its context {other} uses {keys}, so {me} does not fire there.")
        }
        Code::DefaultSuppressed => {
            format!("Your binding {keys} for {other} replaces the default {keys} of {me}.")
        }
        Code::ShadowsTerminalInput => {
            let leader = tools
                .iter()
                .find(|t| t.id() == c.tool)
                .and_then(|t| {
                    t.manifest
                        .leaders
                        .first()
                        .and_then(|l| t.leader_chord(&l.id))
                })
                .map(|(k, _)| k);
            match leader {
                Some(l) => format!(
                    "Programs in a {} pane do not receive {keys}; press {l} {keys} to send it.",
                    c.tool
                ),
                None => format!("Programs in a {} pane do not receive {keys}.", c.tool),
            }
        }
    }
}

/// One tool row of a check report.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ToolEntry {
    pub tool: String,
    pub version: Option<String>,
    /// `self` (the running tool's compiled-in manifest) or `manifest` (a shipped file).
    pub source: &'static str,
    pub enabled: bool,
}

/// `hotkeys.check` data (`check_data` of hotkeys-cli-v1), filtered by severity.
pub fn report_json(entries: &[ToolEntry], a: &Analysis, min: Severity) -> Value {
    let mut entries = entries.to_vec();
    entries.sort_by(|x, y| x.tool.cmp(&y.tool));
    let shown: Vec<&Conflict> = a.conflicts.iter().filter(|c| c.severity() <= min).collect();
    let count = |s: Severity| shown.iter().filter(|c| c.severity() == s).count();
    json!({
        "tools": entries.iter().map(|t| json!({"tool": t.tool, "version": t.version, "source": t.source, "enabled": t.enabled})).collect::<Vec<_>>(),
        "conflicts": shown.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        "summary": {"errors": count(Severity::Error), "warnings": count(Severity::Warning), "infos": count(Severity::Info)}
    })
}
