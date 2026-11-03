//! The override workflow (docs/HOTKEY_CONTRACT.md §9.6): `set`, `disable`, `reset` per action
//! or leader, and the tool-wide switches. Every function here is pure: it returns the new user
//! layer and the change records, or a refusal, and never writes a file. The caller writes the
//! layer only when the change was accepted and `--dry-run` was not given, so a refused or
//! invalid change can never alter working bindings.

use crate::check::{
    Code, Conflict, LayerState, Offer, Severity, ToolState, analyze, validate_spec,
};
use crate::keys::{Chord, Step};
use crate::layer::{Layer, Override, UserLayer};
use crate::manifest::BindingSpec;
use crate::scope::Scope;
use serde_json::{Value, json};

/// Why a change was refused; each maps to a registered CLI error code (§10.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditError {
    HotkeyInvalid,
    HotkeyReserved,
    ActionNotFound,
    HotkeyUnsupported,
    HotkeyConflict,
}

impl EditError {
    pub fn code(self) -> &'static str {
        match self {
            EditError::HotkeyInvalid => "hotkey_invalid",
            EditError::HotkeyReserved => "hotkey_reserved",
            EditError::ActionNotFound => "action_not_found",
            EditError::HotkeyUnsupported => "hotkey_unsupported",
            EditError::HotkeyConflict => "hotkey_conflict",
        }
    }

    pub fn exit_code(self) -> u8 {
        match self {
            EditError::HotkeyInvalid | EditError::HotkeyReserved => 2,
            EditError::ActionNotFound => 3,
            EditError::HotkeyUnsupported => 5,
            EditError::HotkeyConflict => 7,
        }
    }
}

/// A refused change: nothing may be written.
#[derive(Clone, PartialEq, Debug)]
pub struct Refusal {
    pub error: EditError,
    pub message: String,
    pub hint: Option<String>,
    /// The conflicts that block the change.
    pub conflicts: Vec<Conflict>,
}

fn refuse(error: EditError, message: String, hint: Option<String>) -> Refusal {
    Refusal {
        error,
        message,
        hint,
        conflicts: Vec::new(),
    }
}

/// One action's layer state before and after a change (§10.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub action: String,
    pub before: LayerState,
    pub after: LayerState,
}

impl Change {
    pub fn to_json(&self) -> Value {
        json!({"action": self.action, "before": self.before.to_json(), "after": self.after.to_json()})
    }
}

/// An accepted change.
#[derive(Clone, PartialEq, Debug)]
pub struct Applied {
    /// The tool's user layer after the change (`None`: there is still no file).
    pub layer: Option<UserLayer>,
    pub changes: Vec<Change>,
    /// Conflicts involving the changed action (or the tool, for switches) afterwards.
    pub conflicts: Vec<Conflict>,
    /// Whether the layer differs from before (otherwise there is nothing to write).
    pub changed: bool,
}

/// A per-action change.
#[derive(Clone, PartialEq, Debug)]
pub enum Edit {
    /// Replace the action's bindings; `action` may be `@<leader>` with one chord.
    Set {
        action: String,
        bindings: Vec<BindingSpec>,
    },
    /// Give the action no binding.
    Disable { action: String },
    /// Remove the user's override: the default applies again.
    Reset { action: String },
}

impl Edit {
    pub fn action(&self) -> &str {
        match self {
            Edit::Set { action, .. } | Edit::Disable { action } | Edit::Reset { action } => action,
        }
    }
}

/// Explicit conflict resolutions and the layer being edited.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Options {
    /// `--replace`: take a same-tool binding away from the other action.
    pub replace: bool,
    /// `--allow-conflict`: accept a cross-tool global conflict.
    pub allow_conflict: bool,
    /// Edit this profile's overrides instead of the user layer.
    pub profile: Option<String>,
}

/// Applies one per-action change to tool `idx` (§9.6).
pub fn apply(
    tools: &[ToolState],
    taken: &[Step],
    idx: usize,
    edit: &Edit,
    opts: &Options,
) -> Result<Applied, Refusal> {
    let mut base = tools.to_vec();
    base[idx].profile = opts.profile.clone();
    let tool = &base[idx];
    let tool_id = tool.id().to_string();
    let action = edit.action();
    let profile = opts.profile.as_deref();
    let mut next = base.clone();
    if let Some(id) = action.strip_prefix('@') {
        let Some((current, _)) = tool.leader_chord(id) else {
            return Err(refuse(
                EditError::ActionNotFound,
                format!("{tool_id} has no leader `@{id}`"),
                Some(format!("run `{tool_id} hotkeys list` to see its leaders")),
            ));
        };
        let leaders = |l: &mut UserLayer| l.leaders.get_or_insert_with(Default::default).clone();
        match edit {
            Edit::Disable { .. } => {
                return Err(refuse(
                    EditError::HotkeyInvalid,
                    format!("the leader @{id} cannot be disabled"),
                    Some(format!(
                        "a leader is changed, never disabled: `{tool_id} hotkeys set @{id} <keys>` (now {current})"
                    )),
                ));
            }
            Edit::Reset { .. } => {
                if let Some(l) = next[idx].layer.as_mut()
                    && let Some(map) = l.leaders.as_mut()
                {
                    map.remove(id);
                    if map.is_empty() {
                        l.leaders = None;
                    }
                }
            }
            Edit::Set { bindings, .. } => {
                let [spec] = bindings.as_slice() else {
                    return Err(refuse(
                        EditError::HotkeyInvalid,
                        format!("the leader @{id} is one chord"),
                        None,
                    ));
                };
                let Ok(chord) = Chord::parse(spec.keys()) else {
                    return Err(refuse(
                        EditError::HotkeyInvalid,
                        format!("`{}` is not a single chord", spec.keys()),
                        Some("a leader is one chord such as Ctrl+B, without char: keys".into()),
                    ));
                };
                if !matches!(spec, BindingSpec::Text(_)) {
                    return Err(refuse(
                        EditError::HotkeyInvalid,
                        format!("the leader @{id} takes no scope or trigger"),
                        None,
                    ));
                }
                if let Some(c) = validate_spec(tools, taken, idx, action, spec, Layer::User)
                    .ok()
                    .flatten()
                {
                    return Err(refused_binding(c));
                }
                let layer = next[idx]
                    .layer
                    .get_or_insert_with(|| UserLayer::new(&tool_id));
                let mut map = leaders(layer);
                map.insert(id.to_string(), chord.to_string());
                layer.leaders = Some(map);
            }
        }
        return finish(&base, next, taken, idx, action, edit, opts);
    }
    let Some(decl) = tool.manifest.action(action) else {
        return Err(refuse(
            EditError::ActionNotFound,
            format!("{tool_id} has no action `{action}`"),
            Some(format!("run `{tool_id} hotkeys list` to see its actions")),
        ));
    };
    if !decl.bindable && !matches!(edit, Edit::Reset { .. }) {
        return Err(refuse(
            EditError::HotkeyReserved,
            format!("the keys of `{action}` are fixed and cannot be changed"),
            None,
        ));
    }
    let entry = match edit {
        Edit::Reset { .. } => None,
        Edit::Disable { .. } => Some(Override::disabled(action)),
        Edit::Set { bindings, .. } => {
            if bindings.is_empty() || bindings.len() > 8 {
                return Err(refuse(
                    EditError::HotkeyInvalid,
                    "give one to eight bindings".into(),
                    Some(format!(
                        "to remove every binding, run `{tool_id} hotkeys disable {action}`"
                    )),
                ));
            }
            let layer = if profile.is_some() {
                Layer::Profile
            } else {
                Layer::User
            };
            for (i, spec) in bindings.iter().enumerate() {
                let problems = spec.problems(&format!("binding {}", i + 1), true);
                if let Some(p) = problems.first() {
                    return Err(refuse(EditError::HotkeyInvalid, p.clone(), None));
                }
                match validate_spec(&base, taken, idx, action, spec, layer) {
                    Err(e) => {
                        return Err(refuse(
                            EditError::HotkeyInvalid,
                            e.to_string(),
                            Some("see `docs/HOTKEY_CONTRACT.md` §5 for key names".into()),
                        ));
                    }
                    Ok(Some(c)) => return Err(refused_binding(c)),
                    Ok(None) => {}
                }
            }
            Some(Override::bound(action, bindings.clone()))
        }
    };
    match (next[idx].layer.as_mut(), entry) {
        (Some(l), e) => l.set_override(profile, action, e),
        (None, Some(e)) => {
            let mut l = UserLayer::new(&tool_id);
            l.set_override(profile, action, Some(e));
            next[idx].layer = Some(l);
        }
        (None, None) => {}
    }
    finish(&base, next, taken, idx, action, edit, opts)
}

/// Maps a per-binding problem to its CLI error (§10.4).
fn refused_binding(c: Conflict) -> Refusal {
    use crate::check::Reason;
    let error = match (c.code, c.reason) {
        (Code::InvalidBinding, Some(Reason::BackendUnsupported | Reason::TriggerUnsupported)) => {
            EditError::HotkeyUnsupported
        }
        (Code::InvalidBinding, Some(Reason::NotBindable)) => EditError::HotkeyReserved,
        (Code::InvalidBinding, _) => EditError::HotkeyInvalid,
        _ => EditError::HotkeyReserved,
    };
    Refusal {
        error,
        message: c.message.clone(),
        hint: rebind_hint(&c),
        conflicts: vec![c],
    }
}

fn rebind_hint(c: &Conflict) -> Option<String> {
    let keys: Vec<&str> = c
        .offers
        .iter()
        .filter_map(|o| match o {
            Offer::Rebind(k) => Some(k.as_str()),
            _ => None,
        })
        .collect();
    match keys.as_slice() {
        [] => None,
        [a] => Some(format!("try {a}")),
        [a, b, ..] => Some(format!("try {a} or {b}")),
    }
}

/// Re-checks the new layers, resolves `--replace` and refuses error conflicts (§9.6 2–4).
fn finish(
    base: &[ToolState],
    mut next: Vec<ToolState>,
    taken: &[Step],
    idx: usize,
    action: &str,
    edit: &Edit,
    opts: &Options,
) -> Result<Applied, Refusal> {
    let tool_id = base[idx].id().to_string();
    let profile = opts.profile.as_deref();
    let is_set = matches!(edit, Edit::Set { .. });
    let mut changes = vec![(action.to_string(), base[idx].layer_state(action))];
    let analysis = loop {
        let analysis = analyze(&next, taken);
        let blocking: Vec<&Conflict> = analysis
            .conflicts
            .iter()
            .filter(|c| is_set && c.severity() == Severity::Error && c.involves(&tool_id, action))
            .filter(|c| !(opts.allow_conflict && c.code == Code::CrossToolGlobal))
            .collect();
        let Some(c) = blocking.first() else {
            break analysis;
        };
        // --replace takes the binding away from the other action of the same tool.
        let (other_tool, other_action, other_keys) = if c.tool == tool_id && c.action == action {
            match c.with() {
                crate::check::With::Kim {
                    tool, action, keys, ..
                } => (tool.clone(), action.clone(), keys.clone()),
                _ => (String::new(), String::new(), String::new()),
            }
        } else {
            (
                c.tool.clone(),
                c.action.clone(),
                c.keys.clone().unwrap_or_default(),
            )
        };
        let replaceable = opts.replace
            && matches!(c.code, Code::DuplicateBinding | Code::SequencePrefix)
            && other_tool == tool_id
            && !other_action.is_empty()
            && !other_action.starts_with('@')
            && !changes.iter().any(|(a, _)| *a == other_action);
        if !replaceable {
            return Err(conflict_refusal(&tool_id, action, &blocking));
        }
        let before = next[idx].layer_state(&other_action);
        let decl = next[idx].manifest.action(&other_action).cloned();
        let rest: Vec<BindingSpec> = match next[idx].effective_override(&other_action) {
            Some((o, _)) => o.specs().to_vec(),
            None => decl.map(|d| d.defaults).unwrap_or_default(),
        }
        .into_iter()
        .filter(|s| s.keys() != other_keys)
        .collect();
        let entry = if rest.is_empty() {
            Override::disabled(&other_action)
        } else {
            Override::bound(&other_action, rest)
        };
        next[idx]
            .layer
            .get_or_insert_with(|| UserLayer::new(&tool_id))
            .set_override(profile, &other_action, Some(entry));
        changes.push((other_action, before));
    };
    Ok(Applied {
        changed: next[idx].layer != base[idx].layer,
        changes: changes
            .into_iter()
            .map(|(a, before)| Change {
                after: next[idx].layer_state(&a),
                action: a,
                before,
            })
            .collect(),
        conflicts: analysis
            .conflicts
            .into_iter()
            .filter(|c| c.involves(&tool_id, action))
            .collect(),
        layer: next[idx].layer.clone(),
    })
}

fn conflict_refusal(tool: &str, action: &str, blocking: &[&Conflict]) -> Refusal {
    let first = blocking[0];
    let mine = first.tool == tool && first.action == action;
    let keys = if mine {
        first.keys.clone().unwrap_or_default()
    } else {
        match first.with() {
            crate::check::With::Kim { keys, .. } => keys.clone(),
            _ => first.keys.clone().unwrap_or_default(),
        }
    };
    let mut hints = Vec::new();
    if mine && let Some(h) = rebind_hint(first) {
        hints.push(h);
    }
    for c in blocking {
        let (t, a) = if c.tool == tool && c.action == action {
            match c.with() {
                crate::check::With::Kim { tool, action, .. } => (tool.as_str(), action.as_str()),
                _ => continue,
            }
        } else {
            (c.tool.as_str(), c.action.as_str())
        };
        if c.code == Code::CrossToolGlobal {
            hints.push(format!(
                "or run `{t} hotkeys set {a} <keys>`; --allow-conflict keeps both (the first tool to register wins)"
            ));
        } else if a.starts_with('@') {
            hints.push(format!(
                "{a} is a leader and cannot be replaced; choose other keys or change the leader with `{t} hotkeys set {a} <keys>`"
            ));
        } else if t == tool {
            hints.push(format!("--replace takes {keys} away from {a}"));
        }
    }
    Refusal {
        error: EditError::HotkeyConflict,
        message: format!("{keys} conflicts with another binding"),
        hint: (!hints.is_empty()).then(|| hints.join("; ")),
        conflicts: blocking.iter().map(|c| (*c).clone()).collect(),
    }
}

/// A tool-wide change (§10.1, §12.4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Switch {
    /// `enabled: false`: no binding is active and nothing is registered.
    DisableAll,
    /// Turn the tool-wide switch back on.
    EnableAll,
    /// Turn one scope family off (`global`, `app` or `context`).
    DisableScope(String),
    EnableScope(String),
    /// Discard every user change (the caller keeps a backup).
    ResetAll,
}

/// The effective state of an action, including the tool-wide and scope switches.
pub fn effective_state(tool: &ToolState, action: &str) -> LayerState {
    let disabled = LayerState {
        source: "disabled",
        bindings: Vec::new(),
    };
    if !tool.enabled() {
        return disabled;
    }
    if action.starts_with('@') {
        return if tool.scope_enabled(&Scope::App) {
            tool.layer_state(action)
        } else {
            disabled
        };
    }
    let Some(decl) = tool.manifest.action(action) else {
        return tool.layer_state(action);
    };
    match tool.specs(decl) {
        None => disabled,
        Some((specs, layer)) => {
            let kept: Vec<String> = specs
                .iter()
                .filter(|s| tool.scope_enabled(&ToolState::spec_scope(decl, s)))
                .map(|s| s.keys().to_string())
                .collect();
            if kept.is_empty() && !specs.is_empty() {
                disabled
            } else {
                LayerState {
                    source: if layer == Layer::Default {
                        "default"
                    } else {
                        "user"
                    },
                    bindings: kept,
                }
            }
        }
    }
}

/// Applies a tool-wide switch to tool `idx`. Changes list every action (and leader) whose
/// effective bindings differ.
pub fn apply_switch(tools: &[ToolState], taken: &[Step], idx: usize, switch: &Switch) -> Applied {
    let mut next = tools.to_vec();
    let tool_id = tools[idx].id().to_string();
    let family_ok = |f: &str| matches!(f, "global" | "app" | "context");
    match switch {
        Switch::DisableAll => {
            next[idx]
                .layer
                .get_or_insert_with(|| UserLayer::new(&tool_id))
                .enabled = Some(false);
        }
        Switch::EnableAll => {
            if let Some(l) = next[idx].layer.as_mut() {
                l.enabled = None;
            }
        }
        Switch::DisableScope(f) if family_ok(f) => {
            let l = next[idx]
                .layer
                .get_or_insert_with(|| UserLayer::new(&tool_id));
            let list = l.disabled_scopes.get_or_insert_with(Vec::new);
            if !list.contains(f) {
                list.push(f.clone());
            }
        }
        Switch::EnableScope(f) => {
            if let Some(l) = next[idx].layer.as_mut()
                && let Some(list) = l.disabled_scopes.as_mut()
            {
                list.retain(|s| s != f);
                if list.is_empty() {
                    l.disabled_scopes = None;
                }
            }
        }
        Switch::DisableScope(_) => {}
        Switch::ResetAll => {
            let mut fresh = UserLayer::new(&tool_id);
            if let Some(old) = &tools[idx].layer {
                fresh.extra = old.extra.clone();
                next[idx].layer = Some(fresh);
            } else if tools[idx].safe_mode {
                // Replaces a broken file (the caller keeps it as the backup).
                next[idx].layer = Some(fresh);
            }
            next[idx].safe_mode = false;
        }
    }
    let (before, after) = (&tools[idx], &next[idx]);
    let mut changes = Vec::new();
    let names = before
        .manifest
        .leaders
        .iter()
        .map(|l| format!("@{}", l.id))
        .chain(before.manifest.actions.iter().map(|a| a.id.clone()));
    for a in names {
        let (b, n) = (effective_state(before, &a), effective_state(after, &a));
        // An action without bindings before and after is no change worth listing.
        if b != n && !(b.bindings.is_empty() && n.bindings.is_empty()) {
            changes.push(Change {
                action: a,
                before: b,
                after: n,
            });
        }
    }
    let analysis = analyze(&next, taken);
    Applied {
        changed: next[idx].layer != tools[idx].layer,
        changes,
        conflicts: analysis
            .conflicts
            .into_iter()
            .filter(|c| c.tool == tool_id)
            .collect(),
        layer: next[idx].layer.clone(),
    }
}

/// The chord of a single-step binding text, for registration (`None` for sequences).
pub fn single_chord(keys: &str) -> Option<Chord> {
    match crate::keys::Sequence::parse(keys).ok()?.steps() {
        [Step::Chord(c)] => Some(*c),
        _ => None,
    }
}
