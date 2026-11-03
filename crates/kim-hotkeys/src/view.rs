//! The `hotkeys list` and `hotkeys show` data (docs/HOTKEY_CONTRACT.md §10.2, schema
//! `hotkeys-cli-v1` `list_data`, `show_data`), shared by every tool's CLI and GUI list.

use crate::check::{Analysis, ToolState};
use crate::manifest::ActionDecl;
use crate::scope::Scope;
use crate::store::ConfigState;
use serde_json::{Value, json};

/// The reserved navigation actions every Kim GUI has (§3.5): listed, never rebindable.
pub const RESERVED_NAVIGATION: &[(&str, &str, &[&str])] = &[
    ("nav.next-control", "Next control", &["Tab"]),
    ("nav.previous-control", "Previous control", &["Shift+Tab"]),
    ("nav.next-region", "Next region", &["F6"]),
    ("nav.previous-region", "Previous region", &["Shift+F6"]),
    (
        "nav.activate",
        "Activate the focused control",
        &["Enter", "Space"],
    ),
    ("nav.cancel", "Cancel or close", &["Escape"]),
    (
        "nav.context-menu",
        "Open the context menu",
        &["Shift+F10", "ContextMenu"],
    ),
    ("window.close", "Close the window", &["Alt+F4"]),
];

/// What a tool knows about the registration of a global binding (§10.2): `registered`,
/// `taken`, `not_running` or `unknown`. A CLI that cannot ask its running instance says
/// `unknown`.
pub type Registration<'a> = &'a dyn Fn(&str, &str) -> &'static str;

/// `source` of an action (§10.2).
pub fn action_source(tool: &ToolState, decl: &ActionDecl) -> &'static str {
    match tool.effective_override(&decl.id) {
        Some((o, _)) if o.is_disabled() => "disabled",
        Some((_, crate::layer::Layer::Profile)) => "profile",
        Some(_) => "user",
        None if decl.defaults.is_empty() => "unbound",
        None => "default",
    }
}

/// One `action_view`.
pub fn action_view(
    tool: &ToolState,
    analysis: &Analysis,
    decl: &ActionDecl,
    registration: Registration<'_>,
) -> Value {
    let scope = match &decl.context {
        Some(c) => format!("context:{c}"),
        None => match decl.scopes.first() {
            Some(crate::manifest::ScopeKind::Global) => "global".into(),
            _ => "app".into(),
        },
    };
    let bindings: Vec<Value> = tool
        .specs(decl)
        .map(|(specs, _)| specs)
        .unwrap_or_default()
        .iter()
        .map(|s| {
            let sc = ToolState::spec_scope(decl, s);
            json!({
                "keys": s.keys(),
                "scope": sc.to_string(),
                "trigger": s.trigger().as_str(),
                "active": analysis.is_active(tool.id(), &decl.id, s.keys()),
                "registration": if sc == Scope::Global { registration(&decl.id, s.keys()) } else { "not_applicable" },
            })
        })
        .collect();
    json!({
        "id": decl.id,
        "title": decl.title,
        "scope": scope,
        "bindable": decl.bindable,
        "source": action_source(tool, decl),
        "defaults": decl.defaults.iter().map(|d| d.keys()).collect::<Vec<_>>(),
        "bindings": bindings,
    })
}

/// The reserved navigation actions the manifest does not declare itself.
pub fn reserved_views(tool: &ToolState) -> Vec<Value> {
    RESERVED_NAVIGATION
        .iter()
        .filter(|(id, _, _)| tool.manifest.action(id).is_none())
        .map(|(id, title, keys)| reserved_view(tool, id, title, keys))
        .collect()
}

fn reserved_view(tool: &ToolState, id: &str, title: &str, keys: &[&str]) -> Value {
    // Fixed safety keys stay on even when the tool's shortcuts are off (§8.3 6).
    let _ = tool;
    json!({
        "id": id, "title": title, "scope": "app", "bindable": false, "source": "reserved",
        "defaults": keys,
        "bindings": keys.iter().map(|k| json!({"keys": k, "scope": "app", "trigger": "press",
            "active": true, "registration": "not_applicable"})).collect::<Vec<_>>(),
    })
}

/// `hotkeys.list` data.
pub fn list_data(
    tool: &ToolState,
    analysis: &Analysis,
    path: &str,
    state: ConfigState,
    registration: Registration<'_>,
) -> Value {
    let disabled_scopes: Vec<&str> = tool
        .layer
        .as_ref()
        .and_then(|l| l.disabled_scopes.as_ref())
        .map(|d| {
            d.iter()
                .map(String::as_str)
                .filter(|s| matches!(*s, "global" | "app" | "context"))
                .collect()
        })
        .unwrap_or_default();
    let leaders: Vec<Value> = tool
        .manifest
        .leaders
        .iter()
        .filter_map(|l| tool.leader_chord(&l.id).map(|(k, layer)| (l, k, layer)))
        .map(|(l, keys, layer)| {
            json!({"id": l.id, "keys": keys,
                   "source": if layer == crate::layer::Layer::Default { "default" } else { "user" }})
        })
        .collect();
    let mut actions: Vec<Value> = tool
        .manifest
        .actions
        .iter()
        .map(|a| action_view(tool, analysis, a, registration))
        .collect();
    actions.extend(reserved_views(tool));
    json!({
        "tool": tool.id(),
        "enabled": tool.enabled(),
        "config": {"path": path, "state": state.as_str()},
        "disabled_scopes": disabled_scopes,
        "leaders": leaders,
        "actions": actions,
    })
}

/// `hotkeys.show` data, or `None` when the tool has no such action.
pub fn show_data(
    tool: &ToolState,
    analysis: &Analysis,
    action: &str,
    registration: Registration<'_>,
) -> Option<Value> {
    let view = match tool.manifest.action(action) {
        Some(d) => action_view(tool, analysis, d, registration),
        None => {
            let (id, title, keys) = RESERVED_NAVIGATION
                .iter()
                .find(|(id, _, _)| *id == action)?;
            reserved_view(tool, id, title, keys)
        }
    };
    let conflicts: Vec<Value> = analysis
        .conflicts
        .iter()
        .filter(|c| c.involves(tool.id(), action))
        .map(crate::check::Conflict::to_json)
        .collect();
    Some(json!({"tool": tool.id(), "action": view, "conflicts": conflicts}))
}
