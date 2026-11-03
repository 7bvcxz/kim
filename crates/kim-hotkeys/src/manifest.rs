//! The hotkey manifest a tool compiles in and ships as `hotkeys.manifest.json`
//! (docs/HOTKEY_CONTRACT.md §3.4, schema `kim-hotkey-manifest-v1`), and the binding specs that
//! manifests and user layers share (§8.2).

use crate::keys::{Chord, Sequence, is_action_id, is_name_id, is_tool_id};
use crate::reserved;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `schema` of a manifest document.
pub const MANIFEST_SCHEMA: &str = "kim.hotkey-manifest";
/// File name of a shipped manifest, next to `capabilities.json` in an installed version.
pub const MANIFEST_FILE: &str = "hotkeys.manifest.json";
/// The hotkey contract version this library implements.
pub const HOTKEY_CONTRACT: u64 = 1;

/// The strongest input mechanism a tool uses (§4.2). Ordered by capability.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// The tool's own window events: `app` and `context` scopes.
    Focus,
    /// `focus` plus `RegisterHotKey` for global single chords.
    Register,
    /// `register` plus a low-level keyboard hook (Ddalgak only).
    Hook,
}

/// When a binding fires (§5.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    #[default]
    Press,
    Release,
    Hold,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Press => "press",
            Trigger::Release => "release",
            Trigger::Hold => "hold",
        }
    }
}

/// A binding as stored: canonical text, or an object with options (§8.2).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BindingSpec {
    Text(String),
    Object(BindingObject),
}

/// The object form of a binding. Unknown fields are kept when the file is rewritten.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct BindingObject {
    pub keys: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl BindingSpec {
    pub fn keys(&self) -> &str {
        match self {
            BindingSpec::Text(k) => k,
            BindingSpec::Object(o) => &o.keys,
        }
    }

    /// The explicitly chosen scope (`global`/`app`), if any.
    pub fn scope(&self) -> Option<&str> {
        match self {
            BindingSpec::Text(_) => None,
            BindingSpec::Object(o) => o.scope.as_deref(),
        }
    }

    pub fn trigger(&self) -> Trigger {
        match self {
            BindingSpec::Text(_) => Trigger::Press,
            BindingSpec::Object(o) => o.trigger.unwrap_or_default(),
        }
    }

    /// Problems with this spec that a schema would report (syntax, reserved chords in any scope,
    /// option combinations), each prefixed with `at`.
    pub fn problems(&self, at: &str, allow_reserved: bool) -> Vec<String> {
        let mut out = Vec::new();
        match Sequence::parse(self.keys()) {
            Err(e) => out.push(format!("{at}: {}", e.message)),
            Ok(seq) => {
                if !allow_reserved && let Some(r) = reserved::scope_independent(&seq) {
                    out.push(format!("{at}: `{}` is reserved ({})", self.keys(), r.name));
                }
            }
        }
        if let BindingSpec::Object(o) = self {
            if let Some(s) = &o.scope
                && s != "global"
                && s != "app"
            {
                out.push(format!("{at}: scope must be `global` or `app`, not `{s}`"));
            }
            if let Some(h) = o.hold_ms {
                if o.trigger != Some(Trigger::Hold) {
                    out.push(format!("{at}: hold_ms needs trigger `hold`"));
                }
                if !(200..=5000).contains(&h) {
                    out.push(format!("{at}: hold_ms must be 200 to 5000"));
                }
            }
            if o.repeat == Some(true) && o.trigger.is_some_and(|t| t != Trigger::Press) {
                out.push(format!("{at}: repeat is only allowed with trigger `press`"));
            }
        }
        out
    }
}

/// A named UI part with its own bindings (§3.2).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ContextDecl {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub passthrough: bool,
}

/// A named chord that starts a key table (§3.3).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LeaderDecl {
    pub id: String,
    pub title: String,
    pub default: String,
}

/// Where an action may be bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Global,
    App,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

/// One GUI operation a shortcut can invoke (§3.1, §3.4).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ActionDecl {
    pub id: String,
    pub title: String,
    /// The scopes a user may choose, default first. Empty for context actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<ScopeKind>,
    /// The context the action lives in, instead of `scopes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub defaults: Vec<BindingSpec>,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub bindable: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub destructive: bool,
}

/// A tool's declared actions, contexts, leaders and defaults.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: String,
    pub schema_version: u64,
    pub hotkey_contract: u64,
    pub tool: String,
    pub version: String,
    pub backend: Backend,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<ContextDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leaders: Vec<LeaderDecl>,
    pub actions: Vec<ActionDecl>,
}

/// Why a manifest cannot be used.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ManifestError {
    /// The manifest needs a newer hotkey contract or schema than this library knows.
    pub newer: bool,
    pub problems: Vec<String>,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problems.join("; "))
    }
}

impl Manifest {
    /// Parses and validates a manifest document (JSON text).
    pub fn parse(bytes: &[u8]) -> Result<Manifest, ManifestError> {
        let fail = |p: String| ManifestError {
            newer: false,
            problems: vec![p],
        };
        let v: Value = serde_json::from_slice(bytes).map_err(|e| fail(format!("not JSON: {e}")))?;
        Manifest::from_value(v)
    }

    pub fn from_value(v: Value) -> Result<Manifest, ManifestError> {
        let newer = v
            .get("schema_version")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 1)
            || v.get("hotkey_contract")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > HOTKEY_CONTRACT);
        if newer {
            return Err(ManifestError {
                newer: true,
                problems: vec!["the manifest needs a newer hotkey contract".into()],
            });
        }
        let m: Manifest = serde_json::from_value(v).map_err(|e| ManifestError {
            newer: false,
            problems: vec![e.to_string()],
        })?;
        let problems = m.problems();
        if problems.is_empty() {
            Ok(m)
        } else {
            Err(ManifestError {
                newer: false,
                problems,
            })
        }
    }

    /// What the `kim-hotkey-manifest-v1` schema would reject, plus duplicate IDs.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.schema != MANIFEST_SCHEMA {
            out.push(format!("schema must be `{MANIFEST_SCHEMA}`"));
        }
        if self.schema_version != 1 || self.hotkey_contract != HOTKEY_CONTRACT {
            out.push("schema_version and hotkey_contract must be 1".into());
        }
        if !is_tool_id(&self.tool) {
            out.push(format!("`{}` is not a tool ID", self.tool));
        }
        for (i, c) in self.contexts.iter().enumerate() {
            if !is_name_id(&c.id) || c.title.is_empty() {
                out.push(format!("contexts[{i}]: bad id or empty title"));
            }
        }
        for (i, l) in self.leaders.iter().enumerate() {
            if !is_name_id(&l.id) || l.title.is_empty() {
                out.push(format!("leaders[{i}]: bad id or empty title"));
            }
            match Chord::parse(&l.default) {
                Err(e) => out.push(format!("leaders[{i}].default: {}", e.message)),
                Ok(c) => {
                    let seq = Sequence::from_steps(vec![crate::keys::Step::Chord(c)]);
                    if let Some(r) = reserved::scope_independent(&seq) {
                        out.push(format!("leaders[{i}].default is reserved ({})", r.name));
                    }
                }
            }
        }
        if self.actions.is_empty() {
            out.push("a manifest declares at least one action".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, a) in self.actions.iter().enumerate() {
            let at = format!("actions[{i}]");
            if !is_action_id(&a.id) {
                out.push(format!("{at}: `{}` is not an action ID", a.id));
            }
            if !seen.insert(a.id.as_str()) {
                out.push(format!("{at}: `{}` is declared twice", a.id));
            }
            if a.title.is_empty() || a.title.chars().count() > 80 {
                out.push(format!("{at}: title must be 1 to 80 characters"));
            }
            match (&a.context, a.scopes.is_empty()) {
                (Some(c), true) => {
                    if !self.contexts.iter().any(|x| &x.id == c) {
                        out.push(format!("{at}: context `{c}` is not declared"));
                    }
                }
                (None, false) if a.scopes.len() <= 2 => {}
                _ => out.push(format!(
                    "{at}: an action has either 1-2 scopes or a context"
                )),
            }
            if a.defaults.len() > 8 {
                out.push(format!("{at}: at most 8 defaults"));
            }
            for (j, d) in a.defaults.iter().enumerate() {
                out.extend(d.problems(&format!("{at}.defaults[{j}]"), !a.bindable));
            }
        }
        out
    }

    pub fn action(&self, id: &str) -> Option<&ActionDecl> {
        self.actions.iter().find(|a| a.id == id)
    }

    pub fn leader(&self, id: &str) -> Option<&LeaderDecl> {
        self.leaders.iter().find(|l| l.id == id)
    }

    /// Whether a context passes unbound keys to a program (§3.2).
    pub fn has_passthrough(&self) -> bool {
        self.contexts.iter().any(|c| c.passthrough)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(name: &str) -> Vec<u8> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/kim-hotkey-manifest-v1")
            .join(name);
        std::fs::read(p).unwrap()
    }

    #[test]
    fn contract_manifests_parse() {
        for n in ["kim.json", "subak.json", "noeul.json", "ddalgak.json"] {
            let m = Manifest::parse(&example(n)).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(!m.actions.is_empty());
        }
    }

    #[test]
    fn invalid_contract_manifests_are_rejected() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/invalid");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let bytes = std::fs::read(e.path()).unwrap();
            assert!(
                Manifest::parse(&bytes).is_err(),
                "{} must be rejected",
                e.path().display()
            );
        }
    }
}
