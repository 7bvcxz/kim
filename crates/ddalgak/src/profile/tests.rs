//! Step 75 profile tests: CRUD and switching, search and sort, the side-by-side diff, export
//! (portable, inert, without secrets), import (corrupt files, a newer version, a migration, a
//! missing program, a profile with a deleted mode, duplicate conflicting hotkeys, ID clashes,
//! never activated), and versions with save points, restore and rollback (`DDALGAK-FR-030…032`,
//! `DDALGAK-SEC-006`, `DDALGAK-SEC-013`, `DDALGAK-NFR-005`, `DDALGAK-NFR-008`).

use super::*;
use crate::store::{Lock, Origin};
use crate::validate::check;
use std::path::PathBuf;

const GRANT: &str = "2026-11-04T09:00:00Z";

fn doc() -> Document {
    serde_json::from_value(json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 4,
        "active_profile": "work", "settings": { "engine_on_start": "on" },
        "profiles": [
            { "id": "work", "title": "Work", "description": "Office keyboard", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation", "parent": "base" } ],
              "rules": [
                { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true,
                  "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl" } ],
                  "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
                { "id": "nav-left", "title": "H is Left in nav", "enabled": true, "modes": ["nav"],
                  "trigger": { "keys": "code:KeyH" }, "steps": [ { "type": "remap", "to": "Left" } ],
                  "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
                { "id": "pause", "title": "F13 pauses", "enabled": true,
                  "trigger": { "keys": "F13" }, "steps": [ { "type": "engine", "op": "pause" } ] }
              ] },
            { "id": "gaming", "title": "Gaming", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Arrows" } ],
              "rules": [
                { "id": "g-caps", "title": "CapsLock is blocked", "enabled": true,
                  "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "block" } ] },
                { "id": "g-pause", "title": "F13 pauses", "enabled": true,
                  "trigger": { "keys": "F13" }, "steps": [ { "type": "engine", "op": "pause" } ] }
              ] }
        ]
    }))
    .unwrap()
}

/// Applies `op` the way the CLI and the editor do: validate before and after and refuse any
/// introduced error.
fn change(
    d: &mut Document,
    op: impl FnOnce(&mut Document) -> Result<Change, EditError>,
) -> Result<Change, EditError> {
    let before = check(&d.to_value(), &Context::default());
    let mut copy = d.clone();
    let c = op(&mut copy)?;
    let after = check(&copy.to_value(), &Context::default());
    edit::review(&before, &after)?;
    *d = copy;
    Ok(c)
}

fn usable(d: &Document) -> store::Loaded {
    let l = store::interpret(
        d.to_text().as_bytes(),
        &Context::default(),
        store::MIGRATIONS,
    );
    assert!(l.state.usable(), "{:?}", l.state);
    l
}

#[test]
fn ids_come_from_titles_and_are_unique() {
    assert_eq!(id_from_title("Gaming Mode!"), "gaming-mode");
    assert_eq!(id_from_title("  Dev / Rust  "), "dev-rust");
    assert_eq!(id_from_title("업무"), "profile", "nothing ASCII is left");
    assert_eq!(id_from_title("2nd screen"), "p-2nd-screen");
    assert_eq!(id_from_title(&"x".repeat(60)).len(), 32);
    let mut d = doc();
    let a = change(&mut d, |d| add(d, None, "Work", None)).unwrap();
    assert_eq!(a.created_profile.as_deref(), Some("work-2"));
    let b = change(&mut d, |d| add(d, None, "업무", Some("회사 키보드"))).unwrap();
    assert_eq!(b.created_profile.as_deref(), Some("profile"));
    let c = change(&mut d, |d| add(d, None, "업무", None)).unwrap();
    assert_eq!(c.created_profile.as_deref(), Some("profile-2"));
    let c = change(&mut d, |d| add(d, None, "업무 2", None)).unwrap();
    assert_eq!(
        c.created_profile.as_deref(),
        Some("p-2"),
        "only the digit is ASCII"
    );
    let p = d.profile("profile").unwrap();
    assert_eq!(
        (p.title.as_str(), p.description.as_deref()),
        ("업무", Some("회사 키보드"))
    );
    assert_eq!(p.modes.len(), 1);
    assert_eq!(
        d.active_profile, "work",
        "a new profile is never active by itself"
    );
    usable(&d);
}

#[test]
fn add_and_edit_refuse_bad_input_and_change_nothing() {
    let mut d = doc();
    let before = d.clone();
    for (id, title) in [
        (None, ""),
        (None, "   "),
        (Some("Bad Id"), "X"),
        (Some("work"), "X"),
    ] {
        let e = change(&mut d, |d| add(d, id, title, None)).unwrap_err();
        assert!(matches!(e.code(), "usage" | "rule_conflict"), "{e:?}");
    }
    let e = change(&mut d, |d| add(d, None, &"가".repeat(81), None)).unwrap_err();
    assert_eq!(e.code(), "usage");
    let e = change(&mut d, |d| add(d, None, "X", Some(&"d".repeat(501)))).unwrap_err();
    assert_eq!(e.code(), "usage");
    assert_eq!(d, before);
    // Rename and describe; removing the description; an unknown profile.
    let f = ProfileFields {
        title: Some("Office".into()),
        description: Some(None),
    };
    let c = change(&mut d, |d| edit(d, "work", &f)).unwrap();
    assert!(c.changed);
    let p = d.profile("work").unwrap();
    assert_eq!(
        (p.title.as_str(), p.description.as_deref()),
        ("Office", None)
    );
    let c = change(&mut d, |d| edit(d, "work", &f)).unwrap();
    assert!(!c.changed, "the same values again change nothing");
    let e = change(&mut d, |d| edit(d, "nope", &f)).unwrap_err();
    assert_eq!(e.code(), "profile_not_found");
    // At most 64 profiles.
    let mut full = doc();
    for i in 0..62 {
        change(&mut full, |d| add(d, None, &format!("P{i}"), None)).unwrap();
    }
    let e = change(&mut full, |d| add(d, None, "One more", None)).unwrap_err();
    assert_eq!(e.code(), "usage");
}

#[test]
fn duplicate_copies_modes_and_rules_under_new_rule_ids() {
    let mut d = doc();
    let c = change(&mut d, |d| duplicate(d, "work", None, None)).unwrap();
    assert_eq!(c.created_profile.as_deref(), Some("work-2"));
    assert_eq!(c.rules, ["caps-2", "nav-left-2", "pause-2"]);
    let copy = d.profile("work-2").unwrap();
    assert_eq!(copy.title, "Work (copy)");
    assert_eq!(copy.modes, d.profile("work").unwrap().modes);
    let r = copy.rules[0].rule().unwrap();
    assert!(r.enabled && r.consent.is_some(), "enabled and consent stay");
    assert_eq!(r.trigger.display(), "CapsLock");
    assert_eq!(d.active_profile, "work");
    // A chosen ID and name; taken IDs are refused.
    let c = change(&mut d, |d| {
        duplicate(d, "gaming", Some("game2"), Some("게임 2"))
    })
    .unwrap();
    assert_eq!(c.created_profile.as_deref(), Some("game2"));
    assert_eq!(d.profile("game2").unwrap().title, "게임 2");
    let e = change(&mut d, |d| duplicate(d, "gaming", Some("work"), None)).unwrap_err();
    assert_eq!(e.code(), "rule_conflict");
    let e = change(&mut d, |d| duplicate(d, "nope", None, None)).unwrap_err();
    assert_eq!(e.code(), "profile_not_found");
    usable(&d);
}

#[test]
fn delete_never_removes_the_active_or_the_only_profile() {
    let mut d = doc();
    let e = change(&mut d, |d| delete(d, "work")).unwrap_err();
    assert_eq!(e.code(), "profile_in_use");
    let c = change(&mut d, |d| delete(d, "gaming")).unwrap();
    assert_eq!(c.rules, ["g-caps", "g-pause"]);
    assert_eq!(d.profiles.len(), 1);
    let e = change(&mut d, |d| delete(d, "work")).unwrap_err();
    assert_eq!(e.code(), "profile_in_use");
    assert!(e.message().contains("only profile"), "{}", e.message());
    let e = change(&mut d, |d| delete(d, "gaming")).unwrap_err();
    assert_eq!(e.code(), "profile_not_found");
}

#[test]
fn apply_switches_only_the_active_profile() {
    let mut d = doc();
    let before = d.clone();
    let c = change(&mut d, |d| apply(d, "gaming")).unwrap();
    assert!(c.changed);
    assert_eq!(c.profiles, ["work", "gaming"]);
    assert_eq!(d.active_profile, "gaming");
    assert_eq!(d.profiles, before.profiles, "nothing else changed");
    assert_eq!(d.settings, before.settings);
    let c = change(&mut d, |d| apply(d, "gaming")).unwrap();
    assert!(!c.changed);
    let e = change(&mut d, |d| apply(d, "nope")).unwrap_err();
    assert_eq!(e.code(), "profile_not_found");
    // The engine's view follows: the gaming rules are the active bindings.
    let rl = crate::rules::from_store(&usable(&d));
    assert_eq!(rl.profile, "gaming");
    let ids: Vec<&str> = rl.bindings.iter().map(|b| b.rule.as_str()).collect();
    assert!(ids.contains(&"g-caps") && !ids.contains(&"caps"), "{ids:?}");
    // A turned-off profile can be applied; the note says no rule runs.
    d.profiles[0].enabled = Some(false);
    let c = change(&mut d, |d| apply(d, "work")).unwrap();
    assert!(
        c.notes.iter().any(|n| n.contains("turned off")),
        "{:?}",
        c.notes
    );
}

#[test]
fn browse_searches_names_ids_and_descriptions_and_sorts() {
    let mut d = doc();
    change(&mut d, |d| {
        add(
            d,
            Some("dev"),
            "개발 Development",
            Some("Rust and terminals"),
        )
    })
    .unwrap();
    let report = check(&d.to_value(), &Context::default());
    let all = rows(&d, Some(&report));
    assert_eq!(all.len(), 3);
    assert!(all[0].active && !all[1].active);
    assert_eq!((all[0].rules, all[0].rules_on, all[0].modes), (3, 3, 2));
    let ids = |v: Vec<Row>| v.into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(
        ids(browse(&all, "", Sort::Order)),
        ["work", "gaming", "dev"]
    );
    assert_eq!(
        ids(browse(&all, "", Sort::Title)),
        ["gaming", "work", "dev"]
    );
    assert_eq!(ids(browse(&all, "", Sort::Id)), ["dev", "gaming", "work"]);
    assert_eq!(
        ids(browse(&all, "", Sort::Rules)),
        ["work", "gaming", "dev"]
    );
    assert_eq!(
        ids(browse(&all, "OFFICE", Sort::Order)),
        ["work"],
        "description"
    );
    assert_eq!(
        ids(browse(&all, "개발", Sort::Order)),
        ["dev"],
        "Korean name"
    );
    assert_eq!(ids(browse(&all, "gam", Sort::Order)), ["gaming"]);
    assert!(browse(&all, "zzz", Sort::Order).is_empty());
    assert_eq!(Sort::parse("rules"), Some(Sort::Rules));
    assert_eq!(Sort::parse("size"), None);
    let j = all[0].to_json();
    assert_eq!(j["errors"], 0);
}

#[test]
fn the_switch_diff_lines_rules_up_by_trigger_and_shows_no_content() {
    let d = doc();
    let rows = side_by_side(d.profile("work"), d.profile("gaming"), Align::Trigger);
    let pick = |k: &str| rows.iter().find(|r| r.key == k).unwrap();
    assert_eq!(
        pick("CapsLock").status,
        DiffStatus::Changed,
        "remap vs block"
    );
    assert_eq!(
        pick("F13").status,
        DiffStatus::Same,
        "same effect, other ID"
    );
    assert_eq!(pick("code:KeyH [nav]").status, DiffStatus::OnlyLeft);
    let caps = pick("CapsLock");
    assert_eq!(caps.left.as_ref().unwrap().steps, ["remap"]);
    assert_eq!(caps.right.as_ref().unwrap().steps, ["block"]);
    let j = diff_json(d.profile("work"), d.profile("gaming"), Align::Trigger);
    assert_eq!(j["counts"]["changed"], 1);
    assert_eq!(j["counts"]["only_left"], 1);
    assert_eq!(j["right"]["modes"], json!(["base", "nav"]));
    // By rule ID (two versions of one profile): a removed, a changed and an added rule.
    let old = d.profile("work").unwrap().clone();
    let mut new = old.clone();
    new.rules.remove(1);
    new.rules[0].rule_mut().unwrap().title = "Caps is Ctrl".into();
    new.rules
        .push(d.profile("gaming").unwrap().rules[0].clone());
    let rows = side_by_side(Some(&old), Some(&new), Align::RuleId);
    let st: Vec<(&str, DiffStatus)> = rows.iter().map(|r| (r.key.as_str(), r.status)).collect();
    assert_eq!(
        st,
        [
            ("caps", DiffStatus::Changed),
            ("nav-left", DiffStatus::OnlyLeft),
            ("pause", DiffStatus::Same),
            ("g-caps", DiffStatus::OnlyRight)
        ]
    );
    // A diff against nothing (a deleted profile coming back).
    let rows = side_by_side(None, Some(&old), Align::RuleId);
    assert!(rows.iter().all(|r| r.status == DiffStatus::OnlyRight));
}

fn launch_doc(path: &str, env_value: &str) -> Document {
    let mut d = doc();
    let mut rule: crate::model::Rule = serde_json::from_value(json!({
        "id": "open-tool", "title": "F16 opens the tool", "enabled": true,
        "trigger": { "keys": "Ctrl+Alt+Shift+F16" },
        "steps": [ { "type": "open_program", "path": path, "working_dir": r"C:\Users\kim\Projects",
                     "env": [ { "name": "TOOL_TOKEN", "value": env_value } ] } ]
    }))
    .unwrap();
    let fp = validate::launch_fingerprint(&rule.to_value());
    rule.set_grant(
        Permission::LaunchPrograms,
        Some(
            serde_json::from_value(json!({ "granted_at": GRANT, "via": "gui", "fingerprint": fp }))
                .unwrap(),
        ),
    );
    d.profiles[0].rules.push(RuleEntry::Typed(Box::new(rule)));
    d
}

fn vars(name: &str) -> Option<OsString> {
    match name {
        "LOCALAPPDATA" => Some(r"C:\Users\kim\AppData\Local".into()),
        "APPDATA" => Some(r"C:\Users\kim\AppData\Roaming".into()),
        "USERPROFILE" => Some(r"C:\Users\kim".into()),
        "ProgramFiles" => Some(r"C:\Program Files".into()),
        _ => None,
    }
}

#[test]
fn export_is_portable_inert_and_keeps_secrets_out() {
    let d = launch_doc(
        r"C:\Users\kim\AppData\Local\Programs\Tool\tool.exe",
        "s3cr3t-token",
    );
    usable(&d);
    let x = export(
        &d,
        &["work".into(), "gaming".into()],
        ExportOptions::default(),
        &vars,
    )
    .unwrap();
    assert_eq!(x.profiles, ["work", "gaming"]);
    assert_eq!(x.rules, 6);
    assert_eq!(x.portable_paths, 2, "the program and its working folder");
    assert_eq!(x.env_values_removed, 1);
    assert_eq!(x.consent_removed, 3);
    let text = x.doc.to_text();
    assert!(!text.contains("s3cr3t-token"), "no environment value");
    assert!(
        !text.contains("consent") && !text.contains("\"revision\"") && !text.contains("settings")
    );
    assert!(
        text.contains(r"%LocalAppData%\\Programs\\Tool\\tool.exe"),
        "{text}"
    );
    assert!(text.contains(r"%UserProfile%\\Projects"), "{text}");
    let v = x.doc.to_value();
    assert_eq!(v["exported"]["by_version"], crate::VERSION);
    assert_eq!(v["exported"]["withheld"], json!(["env_values"]));
    assert_eq!(v["active_profile"], "work");
    for p in v["profiles"].as_array().unwrap() {
        for r in p["rules"].as_array().unwrap() {
            assert_eq!(r["enabled"], false, "{r}");
        }
    }
    let l = usable(&x.doc);
    assert_eq!(
        l.report.count(Severity::Error),
        0,
        "{:?}",
        l.report.problems
    );
    // Environment values only when asked for; an unknown profile is refused.
    let with = export(
        &d,
        &["work".into()],
        ExportOptions { env_values: true },
        &vars,
    )
    .unwrap();
    assert!(with.doc.to_text().contains("s3cr3t-token"));
    assert!(
        with.doc
            .exported
            .as_ref()
            .unwrap()
            .get("withheld")
            .is_none()
    );
    let e = export(&d, &["nope".into()], ExportOptions::default(), &vars).unwrap_err();
    assert_eq!(e.code(), "profile_not_found");
    assert_eq!(
        export(&d, &[], ExportOptions::default(), &vars)
            .unwrap_err()
            .code(),
        "usage"
    );
}

fn import(bytes: &[u8], into: &Document) -> Result<ImportPlan, ImportError> {
    plan_import(bytes, into, &Context::default(), &ImportOptions::default())
}

/// Imports like the CLI: plan, apply, review (problems of imported rules do not refuse).
fn import_into(d: &mut Document, plan: &ImportPlan) -> Change {
    let before = check(&d.to_value(), &Context::default());
    let mut copy = d.clone();
    let c = apply_import(&mut copy, plan).unwrap();
    let after = check(&copy.to_value(), &Context::default());
    review_import(&before, &after, &plan.rule_ids()).unwrap();
    *d = copy;
    c
}

#[test]
fn corrupt_and_newer_files_are_refused_with_the_reason() {
    let d = doc();
    let cases: [(&[u8], &str); 6] = [
        (b"", "not valid JSON"),
        (b"{\"schema\": \"ddalgak.rules\", \"profiles\": [", "not valid JSON"),
        (b"[1, 2]", "not a `ddalgak.rules` document"),
        (b"{\"schema\": \"kim.hotkeys\"}", "not a `ddalgak.rules` document"),
        (
            b"{\"schema\": \"ddalgak.rules\", \"schema_version\": 1, \"hotkey_contract\": 1, \"active_profile\": \"x\", \"profiles\": []}",
            "",
        ),
        (
            b"{\"schema\": \"ddalgak.rules\", \"schema_version\": 1, \"hotkey_contract\": 1, \"active_profile\": \"nope\", \"profiles\": [{\"id\": \"a\", \"title\": \"A\", \"default_mode\": \"base\", \"modes\": [{\"id\": \"base\", \"title\": \"B\"}], \"rules\": []}]}",
            "",
        ),
    ];
    for (bytes, why) in cases {
        match import(bytes, &d) {
            Err(ImportError::Invalid(m)) => assert!(m.contains(why), "{m}"),
            other => panic!("{:?}: {other:?}", String::from_utf8_lossy(bytes)),
        }
    }
    // A truncated real export.
    let x = export(&d, &["work".into()], ExportOptions::default(), &vars)
        .unwrap()
        .doc
        .to_text();
    assert!(matches!(
        import(&x.as_bytes()[..x.len() / 2], &d),
        Err(ImportError::Invalid(_))
    ));
    let newer = x.replace("\"schema_version\": 1", "\"schema_version\": 2");
    assert_eq!(
        import(newer.as_bytes(), &d).unwrap_err(),
        ImportError::Newer(2)
    );
    assert!(
        import(newer.as_bytes(), &d)
            .unwrap_err()
            .to_string()
            .contains("newer Ddalgak")
    );
}

#[test]
fn import_never_activates_grants_or_enables_and_renames_clashes() {
    let src = launch_doc(r"C:\Program Files\Tool\tool.exe", "v");
    let file = export(
        &src,
        &["work".into(), "gaming".into()],
        ExportOptions::default(),
        &vars,
    )
    .unwrap()
    .doc
    .to_text();
    // Into the same document: every profile and rule ID is taken.
    let mut d = src.clone();
    let plan = import(file.as_bytes(), &d).unwrap();
    let ids: Vec<(&str, Option<&str>)> = plan
        .profiles
        .iter()
        .map(|p| (p.id.as_str(), p.renamed_from.as_deref()))
        .collect();
    assert_eq!(
        ids,
        [("work-2", Some("work")), ("gaming-2", Some("gaming"))]
    );
    let r0 = &plan.profiles[0].rules[0];
    assert_eq!(
        (r0.id.as_str(), r0.renamed_from.as_deref()),
        ("caps-2", Some("caps"))
    );
    assert_eq!(r0.permissions, [Permission::InputInjection]);
    let tool = plan.profiles[0]
        .rules
        .iter()
        .find(|r| r.id == "open-tool-2")
        .unwrap();
    assert_eq!(tool.permissions, [Permission::LaunchPrograms]);
    assert_eq!(tool.targets, [r"%ProgramFiles%\Tool\tool.exe"]);
    let j = tool.to_json(false);
    assert_eq!(j["targets"], json!({ "redacted": true, "count": 1 }));
    assert!(
        !j.to_string().contains("tool.exe"),
        "targets only on request"
    );
    assert!(plan.exported_at.is_some());
    let c = import_into(&mut d, &plan);
    assert_eq!(c.profiles, ["work-2", "gaming-2"]);
    assert_eq!(d.active_profile, "work", "an import never switches");
    assert_eq!(d.settings, src.settings, "nor changes settings");
    for id in ["work-2", "gaming-2"] {
        for r in &d.profile(id).unwrap().rules {
            let r = r.rule().unwrap();
            assert!(
                !r.enabled && r.consent.is_none(),
                "{} is off and without consent",
                r.id
            );
        }
    }
    let l = usable(&d);
    let rl = crate::rules::from_store(&l);
    assert!(
        rl.bindings.iter().all(|b| !b.rule.ends_with("-2")),
        "nothing imported runs"
    );
    // A chosen ID and name for a one-profile file.
    let one = export(&src, &["gaming".into()], ExportOptions::default(), &vars)
        .unwrap()
        .doc
        .to_text();
    let opts = ImportOptions {
        id: Some("game".into()),
        title: Some("게임".into()),
    };
    let p = plan_import(one.as_bytes(), &d, &Context::default(), &opts).unwrap();
    assert_eq!(
        (p.profiles[0].id.as_str(), p.profiles[0].title.as_str()),
        ("game", "게임")
    );
    let e = plan_import(file.as_bytes(), &d, &Context::default(), &opts).unwrap_err();
    assert!(matches!(e, ImportError::Invalid(m) if m.contains("2 profiles")));
    // An ID taken between the plan and the apply refuses the import.
    let mut raced = d.clone();
    change(&mut raced, |d| add(d, Some("game"), "Raced", None)).unwrap();
    let e = apply_import(&mut raced, &p).unwrap_err();
    assert_eq!(e.code(), "rule_conflict");
}

#[test]
fn import_reports_a_missing_program_a_deleted_mode_and_conflicting_hotkeys() {
    let file = json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "active_profile": "dev",
        "exported": { "exported_at": "2026-11-04T11:00:00Z", "by_version": "0.0.1" },
        "profiles": [ { "id": "dev", "title": "Development", "default_mode": "gone",
            "modes": [ { "id": "base", "title": "Base" } ],
            "rules": [
                { "id": "build", "title": "F14 builds", "enabled": false,
                  "trigger": { "keys": "Ctrl+Alt+Shift+F14" },
                  "steps": [ { "type": "open_program", "path": r"C:\Ddalgak-Test-Missing\build.exe" } ] },
                { "id": "build-again", "title": "F14 also builds", "enabled": false,
                  "trigger": { "keys": "Ctrl+Alt+Shift+F14" },
                  "steps": [ { "type": "engine", "op": "pause" } ] },
                { "id": "deleted-mode", "title": "J in a deleted mode", "enabled": false,
                  "modes": ["vim"], "trigger": { "keys": "Ctrl+Alt+Shift+F15" },
                  "steps": [ { "type": "mode", "op": "set", "mode": "insert" } ] },
                { "id": "own-shortcut", "title": "uses the editor shortcut", "enabled": false,
                  "trigger": { "keys": "Ctrl+Alt+Shift+F17" }, "steps": [ { "type": "engine", "op": "pause" } ] },
                { "id": "future", "title": "a newer step type", "enabled": false,
                  "trigger": { "keys": "Ctrl+Alt+Shift+F18" }, "steps": [ { "type": "window", "op": "maximize" } ] }
            ] } ]
    });
    let d = doc();
    let ctx = Context {
        shortcuts: vec![("editor.open".into(), "Ctrl+Alt+Shift+F17".into())],
        check_targets: true,
    };
    let plan = plan_import(
        file.to_string().as_bytes(),
        &d,
        &ctx,
        &ImportOptions::default(),
    )
    .unwrap();
    let p = &plan.profiles[0];
    // The deleted default mode and the deleted modes the rules use come back empty.
    assert_eq!(p.restored_modes, ["gone", "vim", "insert"]);
    assert_eq!(p.modes, ["base", "gone", "vim", "insert"]);
    let rule = |id: &str| p.rules.iter().find(|r| r.id == id).unwrap();
    assert_eq!(rule("build").missing, 1, "a missing program");
    assert_eq!(rule("build").permissions, [Permission::LaunchPrograms]);
    assert_eq!(
        rule("build-again").conflicts,
        [Conflict::Rule("build".into())]
    );
    assert_eq!(
        rule("own-shortcut").conflicts,
        [Conflict::Shortcut("editor.open".into())]
    );
    assert_eq!(rule("deleted-mode").inactive, None, "its modes exist again");
    assert_eq!(
        rule("future").inactive.as_deref(),
        Some("step_unsupported"),
        "a newer step type stays inactive even when turned on"
    );
    let counts = plan.counts();
    assert_eq!(counts["conflicts"], 2);
    assert_eq!(counts["inactive"], 1);
    assert_eq!(counts["missing_programs"], 1);
    assert_eq!(counts["restored_modes"], 3);
    // Applied: the document stays valid, nothing runs, the conflicts wait until enabled.
    let mut d2 = d;
    import_into(&mut d2, &plan);
    let l = usable(&d2);
    assert_eq!(d2.profile("dev").unwrap().default_mode, "gone");
    assert!(
        l.report.file.is_empty(),
        "the import keeps the file loadable: {:?}",
        l.report.file
    );
    // Turning both F14 rules on is refused as a conflict, the way any change is.
    let e = change(&mut d2, |d| {
        edit::set_enabled(
            d,
            &["build-again".into(), "build".into()],
            true,
            &[Permission::LaunchPrograms],
        )
    })
    .unwrap_err();
    assert_eq!(e.code(), "rule_conflict", "{e:?}");
}

#[test]
fn imports_migrate_older_versions_and_keep_unknown_fields() {
    fn v0_to_v1(mut v: Value) -> Result<Value, String> {
        let o = v.as_object_mut().ok_or("not an object")?;
        let name = o.remove("name").ok_or("no name")?;
        o.insert("schema_version".into(), json!(1));
        o.insert("hotkey_contract".into(), json!(1));
        o.insert("active_profile".into(), json!("old"));
        o.insert(
            "profiles".into(),
            json!([{ "id": "old", "title": name, "default_mode": "base",
                     "modes": [{ "id": "base", "title": "Base" }], "rules": [],
                     "from_the_future": { "kept": true } }]),
        );
        Ok(v)
    }
    let chain: &[Migration] = &[v0_to_v1];
    let file = json!({ "schema": "ddalgak.rules", "schema_version": 0, "name": "Old layout" });
    let d = doc();
    let plan = plan_import_with(
        file.to_string().as_bytes(),
        &d,
        &Context::default(),
        &ImportOptions::default(),
        chain,
    )
    .unwrap();
    assert_eq!(plan.migrated_from, Some(0));
    assert_eq!(plan.profiles[0].title, "Old layout");
    assert_eq!(
        plan.profiles[0].profile.extra["from_the_future"],
        json!({ "kept": true })
    );
    // Without the chain the old version is not supported.
    assert!(matches!(
        import(file.to_string().as_bytes(), &d),
        Err(ImportError::Invalid(_))
    ));
}

/// A rules store in a test folder; removed when dropped.
struct Sandbox(Files, PathBuf);

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn sandbox(tag: &str) -> Sandbox {
    let root = std::env::temp_dir().join(format!(
        "ddalgak-profile-{tag}-{}-{}",
        std::process::id(),
        crate::model::uuid_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    Sandbox(
        Files {
            rules: root.join("rules.json"),
        },
        root,
    )
}

fn save(files: &Files, d: &mut Document, cmd: &str) -> u64 {
    let lock: Lock = store::lock(files, store::lock_timeout()).unwrap();
    let base = store::read(files, &Context::default()).revision();
    store::save(
        files,
        &lock,
        d,
        base,
        &Origin {
            origin: "cli",
            command: cmd.into(),
        },
    )
    .unwrap()
    .revision
}

#[test]
fn versions_save_points_restore_and_rollback() {
    let sb = sandbox("versions");
    let files = &sb.0;
    let mut d = doc();
    save(files, &mut d, "test.start");
    // v2: a rule is added to work.
    change(&mut d, |d| {
        let r: crate::model::Rule = serde_json::from_value(json!({
            "id": "f14", "title": "F14 resumes", "enabled": true,
            "trigger": { "keys": "F14" }, "steps": [ { "type": "engine", "op": "resume" } ] }))
        .unwrap();
        d.profiles[0].rules.push(RuleEntry::Typed(Box::new(r)));
        Ok(Change {
            changed: true,
            ..Change::default()
        })
    })
    .unwrap();
    save(files, &mut d, "rule.add");
    // A change of another profile only: no new version of work.
    change(&mut d, |d| {
        edit(
            d,
            "gaming",
            &ProfileFields {
                title: Some("Games".into()),
                description: None,
            },
        )
    })
    .unwrap();
    save(files, &mut d, "profile.edit");
    // A save point of work.
    let lock = store::lock(files, store::lock_timeout()).unwrap();
    let (sp, rev) = store::save_point(files, &lock, "work", Some("before the big change"))
        .unwrap()
        .unwrap();
    drop(lock);
    assert!(sp.starts_with(files.savepoints()));
    assert_eq!(rev, d.revision());
    // v3: a broken version (a rule that refers to a mode that does not exist).
    let mut broken = serde_json::to_value(&d).unwrap();
    broken["profiles"][0]["rules"][0]["modes"] = json!(["missing"]);
    let mut broken: Document = serde_json::from_value(broken).unwrap();
    save(files, &mut broken, "hand.edit");
    let current = broken.profile("work").unwrap().clone();
    let vs = versions(files, "work", Some(&current));
    let shape: Vec<(bool, usize, bool, bool)> = vs
        .iter()
        .map(|v| (v.save_point, v.rules, v.working(), v.current))
        .collect();
    assert_eq!(
        shape,
        [
            (false, 4, false, true),
            (true, 4, true, false),
            (false, 4, true, false),
            (false, 3, true, false)
        ],
        "newest first: the broken current one, the save point, the version with F14, the first one"
    );
    assert_eq!(vs[1].label.as_deref(), Some("before the big change"));
    assert!(vs[1].to_json()["kind"] == "save_point");
    // Rollback goes to the newest working version that differs: the save point.
    let target = rollback_target(&vs, Some(&current)).unwrap();
    assert!(target.save_point);
    let mut now = broken.clone();
    let c = change(&mut now, |d| restore(d, &target.profile)).unwrap();
    assert!(c.changed);
    assert_eq!(now.profile("work").unwrap(), &vs[1].profile);
    assert_eq!(
        now.profile("gaming").unwrap().title,
        "Games",
        "other profiles stay"
    );
    // Restoring a deleted profile brings it back, not active.
    change(&mut now, |d| apply(d, "gaming")).unwrap();
    let old_work = vs[3].profile.clone();
    change(&mut now, |d| {
        d.active_profile = "gaming".into();
        delete(d, "work")
    })
    .unwrap();
    let c = change(&mut now, |d| restore(d, &old_work)).unwrap();
    assert!(
        c.notes.iter().any(|n| n.contains("deleted")),
        "{:?}",
        c.notes
    );
    assert_eq!(now.active_profile, "gaming");
    assert_eq!(now.profile("work").unwrap().rules.len(), 3);
    // A version whose rule IDs another profile took meanwhile gets fresh ones.
    let mut clash = now.clone();
    // (`nav-left` moves to gaming, which has a `nav` mode too.)
    let taken = clash.profile("work").unwrap().rules[1].clone();
    change(&mut clash, |d| {
        let pi = d.profile_index("gaming").unwrap();
        d.profiles[pi].rules.push(taken.clone());
        let wi = d.profile_index("work").unwrap();
        d.profiles[wi].rules.remove(1);
        Ok(Change {
            changed: true,
            ..Change::default()
        })
    })
    .unwrap();
    let c = change(&mut clash, |d| restore(d, &old_work)).unwrap();
    assert!(c.rules.contains(&"nav-left-2".to_string()), "{:?}", c.rules);
    assert!(
        c.notes.iter().any(|n| n.contains("another profile")),
        "{:?}",
        c.notes
    );
    usable(&clash);
}
