//! The editor's export and import hooks (docs/DDALGAK_RULE_MODEL.md §10,
//! docs/DDALGAK_GUI_JOURNEYS.md J7, `DDALGAK-FR-032`, `DDALGAK-SEC-013`).
//!
//! - **Export** writes the selected rules as a portable `ddalgak.rules` document wrapped in one
//!   profile, with an `exported` block and **without** `consent`, `settings` or `revision`.
//! - **Import** reads such a document, validates it, and plans what would be added to one
//!   profile: every rule **turned off and without consent**, clashing IDs renamed (`<id>-2`),
//!   missing modes added. The plan names each launch target exactly and says how much a typing
//!   rule types, so the user reviews them; applying it only changes the rules document. Nothing
//!   is opened, typed or run by an import, and a rule can be turned on only through the consent
//!   dialog. Whole-profile import and export with a diff are Step 75.

use crate::form::{RuleForm, launch_targets};
use ddalgak::edit::{Change, EditError};
use ddalgak::model::{Document, Mode, Profile, RuleEntry, is_rule_id};
use ddalgak::store::{self, StoreError};
use ddalgak::validate::Context;
use serde_json::json;
use std::path::Path;

/// The rules of `ids` as a portable document (one profile `exported`).
pub fn export_rules(doc: &Document, ids: &[String], title: &str) -> Document {
    let mut modes: Vec<Mode> = Vec::new();
    let mut rules = Vec::new();
    let mut default_mode = None;
    for id in ids {
        let Some((pi, ri)) = doc.find_rule(id) else {
            continue;
        };
        let p = &doc.profiles[pi];
        default_mode.get_or_insert_with(|| p.default_mode.clone());
        for m in &p.modes {
            if !modes.iter().any(|x| x.id == m.id) {
                let mut m = m.clone();
                m.enabled = None;
                modes.push(m);
            }
        }
        let mut e = p.rules[ri].clone();
        // An export is never active somewhere else by itself (RULE_MODEL §10, `09-export.json`).
        e.set_enabled(false);
        match &mut e {
            RuleEntry::Typed(r) => {
                r.consent = None;
                r.created_at = None;
                r.updated_at = None;
            }
            RuleEntry::Raw(serde_json::Value::Object(m)) => {
                m.remove("consent");
            }
            RuleEntry::Raw(_) => {}
        }
        rules.push(e);
    }
    let mut out = Document::empty();
    out.revision = None;
    out.settings = None;
    out.exported = Some(json!({
        "exported_at": ddalgak::time::now_rfc3339(),
        "by_version": ddalgak::VERSION,
    }));
    let p = &mut out.profiles[0];
    p.id = "exported".into();
    p.title = title.chars().take(80).collect();
    if let Some(d) = default_mode {
        p.default_mode = d;
    }
    if !modes.is_empty() {
        p.modes = modes;
    }
    p.rules = rules;
    out.active_profile = "exported".into();
    out
}

/// Writes an export file atomically.
pub fn write_export(path: &Path, doc: &Document) -> Result<(), StoreError> {
    store::write_atomic(path, doc.to_text().as_bytes())
}

/// One rule an import would add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub id: String,
    /// Set when the ID was taken and the rule gets a new one.
    pub renamed_from: Option<String>,
    pub title: String,
    pub summary: String,
    /// The exact programs and websites it would open once approved and enabled.
    pub launch_targets: Vec<String>,
    /// It types or presses keys in other programs once approved.
    pub injects: bool,
}

/// What an import would do (shown before anything is written).
#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub target_profile: String,
    pub rules: Vec<Incoming>,
    pub new_modes: Vec<String>,
    /// Entries kept out: rules that do not fit this version's format.
    pub skipped: Vec<String>,
    entries: Vec<RuleEntry>,
    modes: Vec<Mode>,
}

/// Reads an export file and plans adding its rules to `target` of `current`.
pub fn plan_import(bytes: &[u8], current: &Document, target: &str) -> Result<ImportPlan, String> {
    let l = store::interpret(bytes, &Context::default(), store::MIGRATIONS);
    if !l.state.usable() {
        return Err(format!(
            "not a usable rules file: {}",
            l.state.problem().unwrap_or_default()
        ));
    }
    let doc = l.doc.ok_or("not a usable rules file")?;
    let tp: &Profile = current
        .profile(target)
        .ok_or_else(|| format!("no profile `{target}`"))?;
    let mut taken: std::collections::BTreeSet<String> = current
        .profiles
        .iter()
        .flat_map(|p| p.rules.iter().filter_map(|r| r.id().map(str::to_string)))
        .collect();
    let mut plan = ImportPlan {
        target_profile: target.to_string(),
        rules: Vec::new(),
        new_modes: Vec::new(),
        skipped: Vec::new(),
        entries: Vec::new(),
        modes: Vec::new(),
    };
    for p in &doc.profiles {
        for e in &p.rules {
            // An enabled rule without consent fails the schema as a stored rule; it still fits
            // the types, and it is imported turned off and without consent anyway.
            let typed = match e {
                RuleEntry::Typed(r) => Some((**r).clone()),
                RuleEntry::Raw(v) => serde_json::from_value::<ddalgak::model::Rule>(v.clone()).ok(),
            };
            let Some(mut rule) = typed else {
                plan.skipped.push(e.title().unwrap_or("a rule").to_string());
                continue;
            };
            let original = rule.id.clone();
            let mut id = original.clone();
            let mut n = 2;
            while taken.contains(&id) || !is_rule_id(&id) {
                id = format!("{}-{n}", original.chars().take(44).collect::<String>());
                n += 1;
            }
            taken.insert(id.clone());
            rule.id = id.clone();
            rule.enabled = false;
            rule.consent = None;
            rule.created_at = None;
            rule.updated_at = None;
            for m in rule.modes.iter().flatten().chain(
                rule.steps
                    .iter()
                    .filter_map(|s| match s {
                        ddalgak::model::Action::Mode(m) => Some(m),
                        _ => None,
                    })
                    .flat_map(|m| m.mode.iter().chain(m.modes.iter().flatten())),
            ) {
                if !tp.modes.iter().any(|x| &x.id == m) && !plan.modes.iter().any(|x| &x.id == m) {
                    let mut mode = p
                        .modes
                        .iter()
                        .find(|x| &x.id == m)
                        .cloned()
                        .unwrap_or_else(|| Mode::new(m.clone(), m.clone()));
                    mode.parent = None;
                    mode.enabled = None;
                    plan.new_modes.push(mode.title.clone());
                    plan.modes.push(mode);
                }
            }
            let form = RuleForm::edit(target, &rule);
            plan.rules.push(Incoming {
                id: id.clone(),
                renamed_from: (id != original).then_some(original),
                title: rule.title.clone(),
                summary: form.summary(Some(tp)),
                launch_targets: launch_targets(&rule),
                injects: rule
                    .permissions_needed()
                    .contains(&ddalgak::model::Permission::InputInjection),
            });
            plan.entries.push(RuleEntry::Typed(Box::new(rule)));
        }
    }
    Ok(plan)
}

/// Applies a plan to the document (inside a store transaction).
pub fn apply_import(doc: &mut Document, plan: &ImportPlan) -> Result<Change, EditError> {
    let pi = doc
        .profile_index(&plan.target_profile)
        .ok_or_else(|| EditError::ProfileNotFound(plan.target_profile.clone()))?;
    let p = &mut doc.profiles[pi];
    for m in &plan.modes {
        if !p.modes.iter().any(|x| x.id == m.id) {
            p.modes.push(m.clone());
        }
    }
    let mut ids = Vec::new();
    for e in &plan.entries {
        if let Some(id) = e.id() {
            if doc.has_rule_id(id) {
                return Err(EditError::Conflict {
                    message: format!("a rule with the ID `{id}` appeared meanwhile; import again"),
                    problems: Vec::new(),
                });
            }
            ids.push(id.to_string());
        }
        doc.profiles[pi].rules.push(e.clone());
    }
    Ok(Change {
        changed: !ids.is_empty(),
        rules: ids,
        notes: vec!["imported rules are turned off until you review and enable them".into()],
        ..Change::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current() -> Document {
        let v = json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 3,
            "active_profile": "work", "settings": { "engine_on_start": "on" },
            "profiles": [ { "id": "work", "title": "Work", "default_mode": "base",
                "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
                "rules": [
                    { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true, "modes": ["nav"],
                      "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl" } ],
                      "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
                    { "id": "site", "title": "F14 opens example", "enabled": true,
                      "trigger": { "keys": "Ctrl+Alt+Shift+F14" }, "steps": [ { "type": "open_url", "url": "https://example.com/" } ],
                      "consent": { "launch-programs": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui",
                        "fingerprint": "sha256:0000000000000000000000000000000000000000000000000000000000000000" } } }
                ] } ]
        });
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn export_carries_no_consent_settings_or_revision_and_is_a_valid_document() {
        let doc = current();
        let out = export_rules(&doc, &["caps".into(), "site".into()], "Shared");
        let v = out.to_value();
        assert!(v.get("revision").is_none() && v.get("settings").is_none());
        assert!(v["exported"]["exported_at"].is_string());
        assert_eq!(v["profiles"][0]["rules"].as_array().unwrap().len(), 2);
        assert!(!out.to_text().contains("consent"));
        let l = store::interpret(
            out.to_text().as_bytes(),
            &Context::default(),
            store::MIGRATIONS,
        );
        assert!(l.state.usable(), "{:?}", l.state);
    }

    #[test]
    fn import_adds_rules_turned_off_without_consent_and_names_every_target() {
        let doc = current();
        let file = export_rules(&doc, &["caps".into(), "site".into()], "Shared").to_text();
        let mut fresh = current();
        fresh.profiles[0].modes.retain(|m| m.id == "base");
        fresh.profiles[0].rules.retain(|r| r.id() == Some("site"));
        let plan = plan_import(file.as_bytes(), &fresh, "work").unwrap();
        assert_eq!(plan.rules.len(), 2);
        // `site` is taken: renamed. `caps` keeps its ID.
        assert_eq!(plan.rules[1].id, "site-2");
        assert_eq!(plan.rules[1].renamed_from.as_deref(), Some("site"));
        assert_eq!(plan.rules[1].launch_targets, ["https://example.com/"]);
        assert!(plan.rules[0].injects);
        assert_eq!(plan.new_modes, ["Navigation"]);
        let mut d = fresh.clone();
        let c = apply_import(&mut d, &plan).unwrap();
        assert_eq!(c.rules, ["caps", "site-2"]);
        for id in ["caps", "site-2"] {
            let (pi, ri) = d.find_rule(id).unwrap();
            let r = d.profiles[pi].rules[ri].rule().unwrap();
            assert!(!r.enabled && r.consent.is_none(), "{id}");
        }
        // The engine runs none of them: they are disabled (and would need consent first).
        let l = store::interpret(
            d.to_text().as_bytes(),
            &Context::default(),
            store::MIGRATIONS,
        );
        let rl = ddalgak::rules::from_store(&l);
        assert!(
            rl.bindings
                .iter()
                .all(|b| b.rule != "caps" && b.rule != "site-2")
        );
        // A file that is not a rules document is refused with the reason.
        assert!(plan_import(b"{\"x\":1}", &fresh, "work").is_err());
        assert!(plan_import(b"[", &fresh, "work").is_err());
    }
}
