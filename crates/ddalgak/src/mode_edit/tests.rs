//! Step 72 mode CRUD tests: add, edit, rename, copy (with and without rules), delete (refused
//! while used, with `--delete-rules`, of the active mode), the review that refuses introduced
//! mode problems, the editor draft and the form fields (`DDALGAK-FR-016`).

use super::*;
use crate::model::Rule;
use crate::validate::{Context, check};
use serde_json::json;

fn doc() -> Document {
    serde_json::from_value(json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 4,
        "active_profile": "p",
        "profiles": [ { "id": "p", "title": "P", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" },
                       { "id": "nav", "title": "Navigation", "parent": "base", "icon": "N" } ],
            "rules": [
                { "id": "hold-nav", "title": "hold", "enabled": true, "trigger": { "keys": "CapsLock" },
                  "steps": [ { "type": "mode", "op": "hold", "mode": "nav" } ] },
                { "id": "nav-left", "title": "left", "enabled": true, "modes": ["nav"],
                  "trigger": { "keys": "code:KeyH" }, "steps": [ { "type": "remap", "to": "Left" } ],
                  "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" } } },
                { "id": "both", "title": "both", "enabled": true, "modes": ["nav", "base"],
                  "trigger": { "keys": "F13" }, "steps": [ { "type": "engine", "op": "pause" } ] }
            ] } ]
    }))
    .unwrap()
}

fn title(t: &str) -> ModeFields {
    ModeFields {
        title: Some(t.into()),
        ..ModeFields::default()
    }
}

/// Applies `op` like the CLI does: validate before and after and refuse introduced errors.
fn apply(
    d: &mut Document,
    op: impl FnOnce(&mut Document) -> Result<Change, EditError>,
) -> Result<Change, EditError> {
    let before = check(&d.to_value(), &Context::default());
    let mut copy = d.clone();
    let c = op(&mut copy)?;
    let after = check(&copy.to_value(), &Context::default());
    review(&before, &after)?;
    *d = copy;
    Ok(c)
}

#[test]
fn add_makes_ids_from_titles_and_refuses_bad_input() {
    let mut d = doc();
    let c = apply(&mut d, |d| add(d, None, None, &title("Symbols & Signs"))).unwrap();
    assert_eq!(c.created_mode.as_deref(), Some("symbols-signs"));
    let c = apply(&mut d, |d| add(d, None, None, &title("Symbols & Signs"))).unwrap();
    assert_eq!(c.created_mode.as_deref(), Some("symbols-signs-2"), "unique");
    let c = apply(&mut d, |d| add(d, None, None, &title("한글 모드"))).unwrap();
    assert_eq!(
        c.created_mode.as_deref(),
        Some("mode"),
        "a title without ASCII letters"
    );
    let f = ModeFields {
        title: Some("Gaming".into()),
        icon: Some(Some("🎮".into())),
        color: Some(Some("#b7a6f5".into())),
        timeout_ms: Some(Some(60_000)),
        one_shot: Some(true),
        ..ModeFields::default()
    };
    apply(&mut d, |d| add(d, None, Some("game".into()), &f)).unwrap();
    let g = d.profiles[0].modes.iter().find(|m| m.id == "game").unwrap();
    assert_eq!(g.color.as_deref(), Some("#B7A6F5"), "stored in upper case");
    assert_eq!((g.one_shot, g.timeout_ms), (Some(true), Some(60_000)));
    // Refusals: taken ID, bad ID, bad fields, no title, a missing profile.
    let e = apply(&mut d, |d| add(d, None, Some("game".into()), &title("x"))).unwrap_err();
    assert_eq!(e.code(), "rule_conflict");
    for (id, f) in [
        (Some("Bad ID".to_string()), title("x")),
        (None, ModeFields::default()),
        (
            None,
            ModeFields {
                icon: Some(Some("a b".into())),
                ..title("x")
            },
        ),
        (
            None,
            ModeFields {
                color: Some(Some("red".into())),
                ..title("x")
            },
        ),
        (
            None,
            ModeFields {
                timeout_ms: Some(Some(10)),
                ..title("x")
            },
        ),
        (
            None,
            ModeFields {
                parent: Some(Some("Not An ID".into())),
                ..title("x")
            },
        ),
    ] {
        let e = apply(&mut d, |d| add(d, None, id.clone(), &f)).unwrap_err();
        assert_eq!(e.code(), "usage", "{id:?} {f:?}");
    }
    assert_eq!(
        add(&mut d, Some("nope"), None, &title("x"))
            .unwrap_err()
            .code(),
        "profile_not_found"
    );
    // At most 32 modes per profile.
    let mut full = doc();
    for i in 0..30 {
        add(&mut full, None, Some(format!("m{i}")), &title("m")).unwrap();
    }
    assert_eq!(
        add(&mut full, None, None, &title("one too many"))
            .unwrap_err()
            .code(),
        "usage"
    );
}

#[test]
fn edits_that_would_break_the_mode_graph_are_refused_with_mode_invalid() {
    let mut d = doc();
    // A cycle: base → nav → base.
    let cyc = ModeFields {
        parent: Some(Some("nav".into())),
        ..ModeFields::default()
    };
    let e = apply(&mut d, |d| edit(d, None, "base", &cyc)).unwrap_err();
    assert_eq!(e.code(), "mode_invalid");
    assert!(e.problems().iter().any(|p| p.code == "mode_cycle"));
    // An unknown parent.
    let missing = ModeFields {
        parent: Some(Some("ghost".into())),
        ..ModeFields::default()
    };
    let e = apply(&mut d, |d| edit(d, None, "nav", &missing)).unwrap_err();
    assert_eq!(e.code(), "mode_invalid");
    // Making a layer the default mode needs its parent cleared first.
    let make = ModeFields {
        make_default: true,
        ..ModeFields::default()
    };
    let e = apply(&mut d, |d| edit(d, None, "nav", &make)).unwrap_err();
    assert_eq!(e.code(), "mode_invalid");
    let make = ModeFields {
        make_default: true,
        parent: Some(None),
        ..ModeFields::default()
    };
    apply(&mut d, |d| edit(d, None, "nav", &make)).unwrap();
    assert_eq!(d.profiles[0].default_mode, "nav");
    // A timeout on the default mode is refused too.
    let t = ModeFields {
        timeout_ms: Some(Some(5000)),
        ..ModeFields::default()
    };
    assert_eq!(
        apply(&mut d, |d| edit(d, None, "nav", &t))
            .unwrap_err()
            .code(),
        "mode_invalid"
    );
    // Nothing changed is a no-op, not an error.
    let c = apply(&mut d, |d| edit(d, None, "base", &title("Base"))).unwrap();
    assert!(!c.changed);
    assert_eq!(
        edit(&mut d, None, "ghost", &title("x")).unwrap_err().code(),
        "mode_not_found"
    );
}

#[test]
fn rename_keeps_the_id_and_every_reference() {
    let mut d = doc();
    let c = apply(&mut d, |d| rename(d, None, "nav", "Arrows")).unwrap();
    assert_eq!(c.modes, ["nav"]);
    let p = &d.profiles[0];
    assert_eq!(p.modes[1].title, "Arrows");
    assert_eq!(p.modes[1].id, "nav");
    assert_eq!(p.modes[1].icon.as_deref(), Some("N"), "other fields stay");
    assert!(check(&d.to_value(), &Context::default()).count(Severity::Error) == 0);
}

#[test]
fn copy_duplicates_the_mode_and_the_rules_limited_to_it_alone() {
    let mut d = doc();
    let c = apply(&mut d, |d| copy(d, None, "nav", None, None, true)).unwrap();
    assert_eq!(c.created_mode.as_deref(), Some("navigation-copy"));
    assert_eq!(c.rules.len(), 1, "only nav-left is limited to nav alone");
    let p = &d.profiles[0];
    let m = p.modes.iter().find(|m| m.id == "navigation-copy").unwrap();
    assert_eq!(m.title, "Navigation (copy)");
    assert_eq!(
        (m.parent.as_deref(), m.icon.as_deref()),
        (Some("base"), Some("N"))
    );
    let r: &Rule = p.rules.last().unwrap().rule().unwrap();
    assert_eq!(
        r.modes.as_deref(),
        Some(&["navigation-copy".to_string()][..])
    );
    assert!(
        r.enabled && r.consent.is_some(),
        "same targets: consent and state kept"
    );
    assert_ne!(r.id, "nav-left");
    assert_eq!(
        check(&d.to_value(), &Context::default()).count(Severity::Error),
        0
    );
    // Without rules, and with a chosen ID and title.
    let c = apply(&mut d, |d| {
        copy(
            d,
            None,
            "nav",
            Some("nav2".into()),
            Some("Nav 2".into()),
            false,
        )
    })
    .unwrap();
    assert!(c.rules.is_empty());
    assert_eq!(c.created_mode.as_deref(), Some("nav2"));
}

#[test]
fn delete_refuses_modes_in_use_and_can_take_its_rules_along() {
    let mut d = doc();
    // The default mode, a parent of a layer, a mode a rule switches to: refused.
    let e = apply(&mut d, |d| delete(d, None, "base", false)).unwrap_err();
    assert_eq!(e.code(), "mode_in_use");
    let e = apply(&mut d, |d| delete(d, None, "nav", true)).unwrap_err();
    assert_eq!(e.code(), "mode_in_use");
    assert!(e.message().contains("hold-nav"), "{}", e.message());
    // Once nothing switches to it: rules limited to it are listed unless --delete-rules.
    d.profiles[0].rules.remove(0);
    let e = apply(&mut d, |d| delete(d, None, "nav", false)).unwrap_err();
    assert_eq!(e.code(), "mode_in_use");
    let EditError::ModeInUse { users, .. } = &e else {
        panic!("{e:?}")
    };
    assert_eq!(users, &["nav-left".to_string(), "both".to_string()]);
    let c = apply(&mut d, |d| delete(d, None, "nav", true)).unwrap();
    assert_eq!(c.rules, ["nav-left", "both"]);
    let p = &d.profiles[0];
    assert!(p.modes.iter().all(|m| m.id != "nav"));
    assert!(
        p.rules.iter().all(|r| r.id() != Some("nav-left")),
        "deleted"
    );
    let both = p
        .rules
        .iter()
        .find_map(|r| r.rule().filter(|r| r.id == "both"))
        .unwrap();
    assert_eq!(
        both.modes.as_deref(),
        Some(&["base".to_string()][..]),
        "kept, without nav"
    );
    assert_eq!(
        check(&d.to_value(), &Context::default()).count(Severity::Error),
        0
    );
    // A layer whose parent is gone cannot exist: delete refused it above. A mode that is merely
    // active in the running engine can be deleted (the engine falls back on reload).
    let mut d = doc();
    apply(&mut d, |d| {
        add(d, None, Some("game".into()), &title("Gaming"))
    })
    .unwrap();
    apply(&mut d, |d| delete(d, None, "game", false)).unwrap();
    assert_eq!(
        delete(&mut d, None, "game", false).unwrap_err().code(),
        "mode_not_found"
    );
}

#[test]
fn the_editor_draft_checks_like_the_store_and_saves_only_changes() {
    let d = doc();
    let p = &d.profiles[0];
    let mut draft = ModeDraft::of(&p.modes[1]);
    assert!(draft.can_save(p).is_err(), "nothing changed");
    draft.title = "Arrows".into();
    assert!(draft.can_save(p).is_ok());
    assert_eq!(
        draft.summary(p),
        "Arrows: a layer over Base: keys it does not change keep their rules there"
    );
    draft.timeout_ms = "abc".into();
    assert_eq!(draft.problems(p)[0].path, "timeout_ms");
    draft.timeout_ms = "5000".into();
    draft.one_shot = true;
    assert!(
        draft
            .summary(p)
            .ends_with("; ends after the next key; Base returns after 5 s without a key")
    );
    draft.parent = "nav".into();
    let probs = draft.problems(p);
    assert!(probs.iter().any(|x| x.path == "parent"), "{probs:?}");
    let mut new = ModeDraft::default();
    assert!(!new.problems(p).is_empty(), "a new mode needs a name");
    new.title = "Gaming".into();
    new.color = "#123456".into();
    assert!(new.can_save(p).is_ok());
    new.color = "blue".into();
    assert_eq!(new.problems(p)[0].path, "color");
}

#[test]
fn the_form_fields_are_fields_of_the_published_schema_and_keyboard_operable() {
    let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
    let mode = &schema["$defs"]["mode"]["properties"];
    for f in MODE_FIELDS {
        assert!(mode.get(f.path).is_some(), "{} is not a mode field", f.path);
        assert!(!f.label.is_empty() && !f.help.is_empty());
        assert!(
            ["text", "color", "choice", "toggle", "number"].contains(&f.input),
            "{}: every input is reachable from the keyboard",
            f.path
        );
    }
    assert!(MODE_FIELDS[0].required && MODE_FIELDS.iter().skip(1).all(|f| !f.required));
    let step = &schema["$defs"]["step_mode"]["properties"];
    for f in SWITCH_FIELDS {
        let name = f.path.strip_prefix("steps[0].").unwrap();
        assert!(
            step.get(name).is_some(),
            "{} is not a mode step field",
            f.path
        );
    }
    let ops = step["op"]["enum"].as_array().unwrap();
    for op in ["set", "toggle", "cycle", "hold", "previous", "default"] {
        assert!(ops.iter().any(|o| o == op));
        assert!(SWITCH_FIELDS[0].help.contains(match op {
            "set" => "Switch to",
            "toggle" => "toggle",
            "cycle" => "cycle",
            "hold" => "hold",
            "previous" => "previous",
            _ => "default",
        }));
    }
}

#[test]
fn rows_describe_policy_chain_users_and_problems() {
    let mut d = doc();
    d.profiles[0].modes[0].timeout_ms = Some(5000); // the default mode may not have one
    let l = crate::store::interpret(
        d.to_text().as_bytes(),
        &Context::default(),
        crate::store::MIGRATIONS,
    );
    let p = &d.profiles[0];
    let set = ModeSet::from_profile(p);
    let base = row(p, &set, 0, Some("nav"), &l.report);
    assert_eq!(base["policy"], "exclusive");
    assert_eq!(base["layers"], json!(["nav"]));
    assert_eq!(base["active"], false);
    assert_eq!(base["problems"][0]["code"], "mode_invalid");
    let nav = row(p, &set, 1, Some("nav"), &l.report);
    assert_eq!(nav["chain"], json!(["nav", "base"]));
    assert_eq!(nav["switched_by"], json!(["hold-nav"]));
    assert_eq!(nav["rules"], 2);
    assert_eq!(nav["active"], true);
    assert_eq!(rules_of(p, "nav"), ["nav-left", "both"]);
    assert_eq!(id_from_title(p, "Navigation"), "navigation");
    assert_eq!(id_from_title(p, "base"), "base-2");
    assert_eq!(id_from_title(p, "9 lives"), "mode-9-lives");
}

#[test]
fn modes_and_profiles_are_enabled_and_disabled_but_never_the_default_mode() {
    // Step 73.
    let mut d = doc();
    let c = apply(&mut d, |d| set_enabled(d, None, "nav", false)).unwrap();
    assert!(c.changed);
    assert_eq!(c.modes, ["nav"]);
    assert_eq!(c.rules, ["nav-left"], "the rules limited to it alone");
    assert_eq!(d.profiles[0].modes[1].enabled, Some(false));
    let c = apply(&mut d, |d| set_enabled(d, None, "nav", false)).unwrap();
    assert!(!c.changed, "already disabled");
    let c = apply(&mut d, |d| set_enabled(d, None, "nav", true)).unwrap();
    assert!(c.changed);
    assert_eq!(
        d.profiles[0].modes[1].enabled, None,
        "enabled stores no field"
    );
    let e = apply(&mut d, |d| set_enabled(d, None, "base", false)).unwrap_err();
    assert_eq!(e.code(), "mode_invalid");
    assert_eq!(
        apply(&mut d, |d| set_enabled(d, None, "nope", false))
            .unwrap_err()
            .code(),
        "mode_not_found"
    );
    let c = apply(&mut d, |d| set_profile_enabled(d, "p", false)).unwrap();
    assert!(c.changed);
    assert!(!d.profiles[0].is_enabled());
    assert!(
        d.profiles[0]
            .rules
            .iter()
            .all(|r| r.enabled() == Some(true)),
        "each rule keeps its own setting"
    );
    let c = apply(&mut d, |d| set_profile_enabled(d, "p", true)).unwrap();
    assert!(c.changed && d.profiles[0].enabled.is_none());
    assert_eq!(
        apply(&mut d, |d| set_profile_enabled(d, "q", false))
            .unwrap_err()
            .code(),
        "profile_not_found"
    );
}
