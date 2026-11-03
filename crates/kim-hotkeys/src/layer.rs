//! The sparse user layer `hotkeys.json` (`kim.hotkeys` v1, docs/HOTKEY_CONTRACT.md §8.2).
//!
//! It holds only the user's changes. Unknown fields (top level, overrides, binding objects,
//! profiles) are kept when the document is rewritten, and overrides for actions a tool version
//! does not have are kept too (§8.2).

use crate::keys::{Chord, Sequence, Step, is_action_id, is_tool_id};
use crate::manifest::{BindingSpec, Manifest};
use crate::reserved;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// `schema` of a user layer document.
pub const LAYER_SCHEMA: &str = "kim.hotkeys";
/// The newest `schema_version` this library reads and writes.
pub const LAYER_VERSION: u64 = 1;

/// Which layer a binding comes from (§8.2). Later layers win.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Layer {
    Default,
    User,
    Profile,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Default => "default",
            Layer::User => "user",
            Layer::Profile => "profile",
        }
    }
}

/// One action's override: replacement bindings, or `disabled: true`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Override {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<Vec<BindingSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Override {
    pub fn disabled(action: &str) -> Override {
        Override {
            action: action.to_string(),
            bindings: None,
            disabled: Some(true),
            extra: Map::new(),
        }
    }

    pub fn bound(action: &str, bindings: Vec<BindingSpec>) -> Override {
        Override {
            action: action.to_string(),
            bindings: Some(bindings),
            disabled: None,
            extra: Map::new(),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled == Some(true)
    }

    /// The replacement bindings (empty when disabled).
    pub fn specs(&self) -> &[BindingSpec] {
        self.bindings.as_deref().unwrap_or(&[])
    }
}

/// A named overlay applied over the user layer while the tool says it is active.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub overrides: Vec<Override>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The `kim.hotkeys` document.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct UserLayer {
    pub schema: String,
    pub schema_version: u64,
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaders: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<Vec<Override>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profiles: Option<BTreeMap<String, Profile>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl UserLayer {
    /// An empty layer: the tool's defaults apply.
    pub fn new(tool: &str) -> UserLayer {
        UserLayer {
            schema: LAYER_SCHEMA.into(),
            schema_version: LAYER_VERSION,
            tool: tool.into(),
            enabled: None,
            disabled_scopes: None,
            leaders: None,
            overrides: None,
            profiles: None,
            extra: Map::new(),
        }
    }

    /// The tool-wide switch (§12.4).
    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    pub fn scope_disabled(&self, family: &str) -> bool {
        self.disabled_scopes
            .as_ref()
            .is_some_and(|d| d.iter().any(|s| s == family))
    }

    /// The overrides of the user layer (`profile` = `None`) or of one profile.
    pub fn overrides_in(&self, profile: Option<&str>) -> &[Override] {
        match profile {
            None => self.overrides.as_deref().unwrap_or(&[]),
            Some(p) => self
                .profiles
                .as_ref()
                .and_then(|ps| ps.get(p))
                .map_or(&[], |p| p.overrides.as_slice()),
        }
    }

    /// The override that applies to `action` with `profile` active (profile wins, §8.2).
    pub fn effective_override(
        &self,
        action: &str,
        profile: Option<&str>,
    ) -> Option<(&Override, Layer)> {
        if let Some(p) = profile
            && let Some(o) = self
                .overrides_in(Some(p))
                .iter()
                .find(|o| o.action == action)
        {
            return Some((o, Layer::Profile));
        }
        self.overrides_in(None)
            .iter()
            .find(|o| o.action == action)
            .map(|o| (o, Layer::User))
    }

    /// Replaces (`Some`) or removes (`None`) the override for `action` in the user layer or a
    /// profile. Removing an override that does not exist changes nothing.
    pub fn set_override(&mut self, profile: Option<&str>, action: &str, entry: Option<Override>) {
        let exists = self
            .overrides_in(profile)
            .iter()
            .any(|o| o.action == action);
        if entry.is_none() && !exists {
            return;
        }
        let list = match profile {
            None => self.overrides.get_or_insert_with(Vec::new),
            Some(p) => {
                &mut self
                    .profiles
                    .get_or_insert_with(BTreeMap::new)
                    .entry(p.to_string())
                    .or_insert_with(|| Profile {
                        overrides: Vec::new(),
                        extra: Map::new(),
                    })
                    .overrides
            }
        };
        let pos = list.iter().position(|o| o.action == action);
        match (pos, entry) {
            (Some(i), Some(e)) => list[i] = e,
            (Some(i), None) => {
                list.remove(i);
            }
            (None, Some(e)) => list.push(e),
            (None, None) => {}
        }
    }

    /// Everything the `kim-hotkeys-v1` schema and the contract's extra rules reject (§8.3 2),
    /// as `path: problem` lines. Unknown actions are not problems (they are reported as
    /// `unknown_action` conflicts and kept). `manifest` adds the check for undeclared leaders.
    pub fn problems(&self, tool: &str, manifest: Option<&Manifest>) -> Vec<String> {
        let mut out = Vec::new();
        if self.schema != LAYER_SCHEMA {
            out.push(format!("schema: must be `{LAYER_SCHEMA}`"));
        }
        if self.schema_version != LAYER_VERSION {
            out.push(format!("schema_version: must be {LAYER_VERSION}"));
        }
        if !is_tool_id(&self.tool) || self.tool != tool {
            out.push(format!("tool: must be `{tool}`"));
        }
        if let Some(d) = &self.disabled_scopes {
            if d.len() > 3 {
                out.push("disabled_scopes: at most three entries".into());
            }
            for (i, s) in d.iter().enumerate() {
                if !matches!(s.as_str(), "global" | "app" | "context") {
                    out.push(format!(
                        "disabled_scopes[{i}]: `{s}` is not global, app or context"
                    ));
                }
            }
        }
        for (id, keys) in self.leaders.iter().flatten() {
            if let Some(m) = manifest
                && m.leader(id).is_none()
            {
                out.push(format!("leaders.{id}: {tool} has no leader `{id}`"));
            }
            match Chord::parse(keys) {
                Err(e) => out.push(format!("leaders.{id}: {}", e.message)),
                Ok(c) => {
                    if let Some(r) =
                        reserved::scope_independent(&Sequence::from_steps(vec![Step::Chord(c)]))
                    {
                        out.push(format!("leaders.{id}: `{keys}` is reserved ({})", r.name));
                    }
                }
            }
        }
        override_problems(self.overrides_in(None), "overrides", &mut out);
        for (name, p) in self.profiles.iter().flatten() {
            override_problems(
                &p.overrides,
                &format!("profiles.{name}.overrides"),
                &mut out,
            );
        }
        out
    }
}

fn override_problems(list: &[Override], at: &str, out: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    for (i, o) in list.iter().enumerate() {
        let here = format!("{at}[{i}]");
        if !is_action_id(&o.action) {
            out.push(format!("{here}.action: `{}` is not an action ID", o.action));
        }
        if !seen.insert(o.action.as_str()) {
            out.push(format!("{here}: a second override for `{}`", o.action));
        }
        match (&o.bindings, o.disabled) {
            (Some(_), Some(_)) | (None, None) => out.push(format!(
                "{here}: needs exactly one of `bindings` and `disabled: true`"
            )),
            (None, Some(false)) => out.push(format!("{here}.disabled: must be true")),
            (Some(b), None) if b.is_empty() || b.len() > 8 => {
                out.push(format!("{here}.bindings: 1 to 8 bindings"));
            }
            _ => {}
        }
        for (j, b) in o.specs().iter().enumerate() {
            out.extend(b.problems(&format!("{here}.bindings[{j}]"), false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let doc = json!({
            "schema": "kim.hotkeys", "schema_version": 1, "tool": "subak", "future": {"x": 1},
            "overrides": [
                {"action": "tab.next", "bindings": [{"keys": "Ctrl+PageDown", "note": "mine"}], "why": "habit"},
                {"action": "removed.action", "disabled": true}
            ],
            "profiles": {"wsl": {"overrides": [], "color": "red"}}
        });
        let layer: UserLayer = serde_json::from_value(doc.clone()).unwrap();
        assert!(layer.problems("subak", None).is_empty());
        assert_eq!(serde_json::to_value(&layer).unwrap(), doc);
    }

    #[test]
    fn set_override_replaces_removes_and_keeps_order() {
        let mut l = UserLayer::new("kim");
        l.set_override(None, "help.show", None);
        assert_eq!(l.overrides, None, "removing nothing writes nothing");
        l.set_override(None, "help.show", Some(Override::disabled("help.show")));
        l.set_override(
            None,
            "catalog.refresh",
            Some(Override::bound(
                "catalog.refresh",
                vec![BindingSpec::Text("Ctrl+R".into())],
            )),
        );
        l.set_override(None, "help.show", None);
        assert_eq!(l.overrides_in(None).len(), 1);
        l.set_override(
            Some("work"),
            "catalog.refresh",
            Some(Override::disabled("catalog.refresh")),
        );
        let (o, layer) = l
            .effective_override("catalog.refresh", Some("work"))
            .unwrap();
        assert!(o.is_disabled() && layer == Layer::Profile);
        let (o, layer) = l.effective_override("catalog.refresh", None).unwrap();
        assert!(!o.is_disabled() && layer == Layer::User);
    }
}
