//! Behaviour beyond the contract fixtures (Step 46, issue #51): several tools wanting F1,
//! modifier sequences and chords, layouts, profiles, lost registrations as reported conflicts,
//! and rollback of invalid configurations. Requirements: KIM-FR-042, KIM-FR-043, KIM-FR-044,
//! KIM-FR-046, KIM-NFR-022, KIM-SEC-020, KIM-SEC-023.

// Test fixtures favor readability over avoiding clones.
#![allow(
    clippy::needless_pass_by_value,
    clippy::redundant_clone,
    clippy::cloned_ref_to_slice_refs
)]

use kim_hotkeys::check::{Code, Severity, ToolState, analyze};
use kim_hotkeys::edit::{self, Edit, EditError, Options, Switch};
use kim_hotkeys::keys::{Sequence, Step};
use kim_hotkeys::layer::UserLayer;
use kim_hotkeys::manifest::{BindingSpec, Manifest};
use kim_hotkeys::store::{self, ConfigState};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn example(tool: &str) -> Manifest {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/contracts/examples/kim-hotkey-manifest-v1")
        .join(format!("{tool}.json"));
    Manifest::parse(&std::fs::read(p).unwrap()).unwrap()
}

fn manifest(v: Value) -> Manifest {
    Manifest::from_value(v).unwrap()
}

fn layer(v: Value) -> UserLayer {
    serde_json::from_value(v).unwrap()
}

fn tool(m: Manifest, l: Option<Value>) -> ToolState {
    ToolState::new(m, l.map(layer))
}

fn codes(tools: &[ToolState]) -> Vec<(String, String, String, String)> {
    analyze(tools, &[])
        .conflicts
        .iter()
        .map(|c| {
            (
                c.code.as_str().to_string(),
                c.tool.clone(),
                c.action.clone(),
                c.keys.clone().unwrap_or_default(),
            )
        })
        .collect()
}

fn text(keys: &[&str]) -> Vec<BindingSpec> {
    keys.iter()
        .map(|k| BindingSpec::Text((*k).into()))
        .collect()
}

#[test]
fn several_tools_requesting_f1() {
    // Noeul's global F1 (capture), Subak's app F1 (help), Kim Manager's app F1 (help), and a
    // user who also bound Ddalgak's engine toggle to F1 globally.
    let ddalgak = tool(
        example("ddalgak"),
        Some(
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
                    "overrides": [{"action": "engine.toggle", "bindings": ["F1"]}]}),
        ),
    );
    let tools = vec![
        tool(example("kim"), None),
        tool(example("noeul"), None),
        tool(example("subak"), None),
        ddalgak,
    ];
    let a = analyze(&tools, &[]);
    let f1: Vec<(&str, &str, &str, Severity)> = a
        .conflicts
        .iter()
        .filter(|c| c.keys.as_deref() == Some("F1"))
        .map(|c| {
            (
                c.code.as_str(),
                c.tool.as_str(),
                c.action.as_str(),
                c.severity(),
            )
        })
        .collect();
    // Two global F1s: one error, the user's binding (higher layer) is the subject.
    assert!(f1.contains(&(
        "cross_tool_global",
        "ddalgak",
        "engine.toggle",
        Severity::Error
    )));
    // Each global F1 shadows each app F1 of another tool.
    for shadowed in [("kim", "help.show"), ("subak", "help.show")] {
        let n = f1
            .iter()
            .filter(|c| c.0 == "global_shadows_app" && (c.1, c.2) == shadowed)
            .count();
        assert_eq!(
            n, 2,
            "{shadowed:?} is shadowed by Noeul and by Ddalgak: {f1:?}"
        );
    }
    // The cross-tool error offers other keys, never a change to the other tool.
    let err = a
        .conflicts
        .iter()
        .find(|c| c.code == Code::CrossToolGlobal)
        .unwrap();
    assert_eq!(
        err.to_json()["offers"][0],
        json!({"kind": "rebind", "keys": "Shift+F1"})
    );
    assert!(err.message.contains("first tool to register"));
    // Setting Ddalgak's toggle to F1 again is refused without --allow-conflict, and nothing in
    // Noeul's layer is touched either way.
    let mut tools2 = tools.clone();
    tools2[3].layer = None;
    let set = Edit::Set {
        action: "engine.toggle".into(),
        bindings: text(&["F1"]),
    };
    let r = edit::apply(&tools2, &[], 3, &set, &Options::default()).unwrap_err();
    assert_eq!(r.error, EditError::HotkeyConflict);
    assert!(r.hint.unwrap().contains("noeul hotkeys set capture.region"));
    let ok = edit::apply(
        &tools2,
        &[],
        3,
        &set,
        &Options {
            allow_conflict: true,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(ok.conflicts.iter().any(|c| c.code == Code::CrossToolGlobal));
    assert_eq!(
        tools2[1].layer, None,
        "noeul's configuration is never edited"
    );
}

fn app_tool(actions: Value, backend: &str) -> Manifest {
    manifest(json!({
        "schema": "kim.hotkey-manifest", "schema_version": 1, "hotkey_contract": 1,
        "tool": "demo", "version": "1.0.0", "backend": backend,
        "contexts": [{"id": "overlay", "title": "Overlay"}],
        "actions": actions
    }))
}

#[test]
fn modifier_sequences_and_chords() {
    let m = app_tool(
        json!([
            {"id": "a.save", "title": "Save", "scopes": ["app"], "defaults": ["Ctrl+K Ctrl+S"]},
            {"id": "a.kill", "title": "Kill", "scopes": ["app"], "defaults": ["Ctrl+K"]},
            {"id": "a.left", "title": "Left only", "scopes": ["app"], "defaults": ["LCtrl+J"]},
            {"id": "a.right", "title": "Right only", "scopes": ["app"], "defaults": ["RCtrl+J"]},
            {"id": "a.any", "title": "Any side", "scopes": ["app"], "defaults": ["Ctrl+Shift+J"]},
            {"id": "a.any2", "title": "Any side too", "scopes": ["app"], "defaults": ["LCtrl+Shift+J"]},
            {"id": "a.global", "title": "Global", "scopes": ["global"], "defaults": []}
        ]),
        "register",
    );
    let c = codes(&[tool(m.clone(), None)]);
    // `Ctrl+K` is a strict prefix of `Ctrl+K Ctrl+S`: the shorter one is the subject.
    assert!(c.contains(&(
        "sequence_prefix".into(),
        "demo".into(),
        "a.kill".into(),
        "Ctrl+K".into()
    )));
    // LCtrl and RCtrl never overlap; generic Ctrl overlaps a side.
    assert!(!c.iter().any(|x| x.2 == "a.left" || x.2 == "a.right"));
    assert!(
        c.iter()
            .any(|x| x.0 == "duplicate_binding" && (x.2 == "a.any2" || x.2 == "a.any"))
    );
    // A register backend cannot do global sequences, side-specific modifiers or release triggers.
    let tools = [tool(m, None)];
    for (keys, trigger) in [
        ("Ctrl+K Ctrl+G", None),
        ("LCtrl+F8", None),
        ("Ctrl+F8", Some("release")),
    ] {
        let mut spec = json!({"keys": keys, "scope": "global"});
        if let Some(t) = trigger {
            spec["trigger"] = json!(t);
        }
        let e = Edit::Set {
            action: "a.global".into(),
            bindings: vec![serde_json::from_value(spec).unwrap()],
        };
        let r = edit::apply(&tools, &[], 0, &e, &Options::default()).unwrap_err();
        assert_eq!(r.error, EditError::HotkeyUnsupported, "{keys}");
    }
    // A plain global chord is fine.
    let e = Edit::Set {
        action: "a.global".into(),
        bindings: text(&["Ctrl+Alt+Shift+F24"]),
    };
    assert!(edit::apply(&tools, &[], 0, &e, &Options::default()).is_ok());
}

#[test]
fn layouts_logical_physical_and_character_keys() {
    let m = app_tool(
        json!([
            {"id": "a.undo", "title": "Undo", "scopes": ["app"], "defaults": ["Ctrl+Z"]},
            {"id": "a.pos", "title": "Undo by position", "scopes": ["app"], "defaults": ["Ctrl+code:KeyZ"]},
            {"id": "a.at", "title": "AltGr-ish", "scopes": ["app"], "defaults": []},
            {"id": "o.help", "title": "Help in overlay", "context": "overlay", "defaults": ["char:?"]},
            {"id": "o.q", "title": "Quit overlay", "context": "overlay", "defaults": ["char:q"]},
            {"id": "o.q2", "title": "Q in overlay", "context": "overlay", "defaults": ["Q"]}
        ]),
        "focus",
    );
    let c = codes(&[tool(m.clone(), None)]);
    // Logical Z and physical KeyZ depend on the layout: never reported as duplicates.
    assert!(
        !c.iter().any(|x| x.2 == "a.undo" || x.2 == "a.pos"),
        "{c:?}"
    );
    // char:q and Q (no Shift) are the same key on Latin layouts.
    assert!(c.iter().any(|x| x.0 == "duplicate_binding"));
    let tools = [tool(m, None)];
    let set = |k: &str| Edit::Set {
        action: "a.at".into(),
        bindings: text(&[k]),
    };
    // Ctrl+Alt+Q may be AltGr+Q (`@` on German layouts): allowed with a warning.
    let ok = edit::apply(&tools, &[], 0, &set("Ctrl+Alt+Q"), &Options::default()).unwrap();
    assert!(ok.conflicts.iter().any(|c| c.code == Code::AltgrCollision));
    // `char:` as the first step of an app binding would make typing impossible.
    let r = edit::apply(&tools, &[], 0, &set("char:?"), &Options::default()).unwrap_err();
    assert_eq!(r.error, EditError::HotkeyReserved);
    // IME mode keys are never bound; IME toggles warn.
    let r = edit::apply(&tools, &[], 0, &set("Ctrl+HangulMode"), &Options::default()).unwrap_err();
    assert_eq!(r.error, EditError::HotkeyReserved);
    let ok = edit::apply(&tools, &[], 0, &set("Ctrl+Space"), &Options::default()).unwrap();
    assert!(ok.conflicts.iter().any(|c| c.code == Code::ImeToggle));
    // Stored text is the portable key name, never a virtual-key number (KIM-NFR-022).
    let doc = serde_json::to_string(&ok.layer.unwrap()).unwrap();
    assert!(doc.contains("Ctrl+Space") && !doc.contains("0x"));
}

#[test]
fn profiles_overlay_the_user_layer() {
    let base = json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
        "overrides": [{"action": "terminal.new", "bindings": ["Ctrl+Shift+N"]}],
        "profiles": {"wsl": {"overrides": [{"action": "terminal.new", "disabled": true}]}}});
    let mut t = tool(example("subak"), Some(base));
    assert_eq!(t.layer_state("terminal.new").bindings, ["Ctrl+Shift+N"]);
    t.profile = Some("wsl".into());
    assert_eq!(t.layer_state("terminal.new").source, "disabled");
    let a = analyze(&[t.clone()], &[]);
    assert!(!a.active.iter().any(|b| b.action == "terminal.new"));
    // Switching the profile back restores the user binding without touching the file.
    t.profile = None;
    assert!(analyze(&[t.clone()], &[]).is_active("subak", "terminal.new", "Ctrl+Shift+N"));
    // `set --profile` writes into that profile only; `reset --profile` removes it again.
    let opts = Options {
        profile: Some("work".into()),
        ..Options::default()
    };
    let e = Edit::Set {
        action: "tab.next".into(),
        bindings: text(&["Ctrl+PageDown"]),
    };
    let ok = edit::apply(&[t.clone()], &[], 0, &e, &opts).unwrap();
    let l = ok.layer.unwrap();
    assert_eq!(l.overrides_in(Some("work"))[0].action, "tab.next");
    assert!(l.overrides_in(None).iter().all(|o| o.action != "tab.next"));
    let mut t2 = t.clone();
    t2.layer = Some(l);
    let back = edit::apply(
        &[t2],
        &[],
        0,
        &Edit::Reset {
            action: "tab.next".into(),
        },
        &opts,
    )
    .unwrap();
    assert!(back.layer.unwrap().overrides_in(Some("work")).is_empty());
}

#[test]
fn chords_other_programs_own_are_reported_and_never_taken() {
    // Registration results feed back into the check as `taken` (§9.2).
    let noeul = tool(example("noeul"), None);
    let taken: Vec<Step> = vec![Sequence::parse("F3").unwrap().steps()[0].clone()];
    let a = analyze(&[noeul], &taken);
    let t = a.conflicts.iter().find(|c| c.code == Code::Taken).unwrap();
    assert_eq!(
        (t.action.as_str(), t.severity()),
        ("paste.float", Severity::Warning)
    );
    assert_eq!(
        t.to_json()["with"],
        json!({"other_program": true, "keys": "F3"})
    );
    assert!(t.message.contains("never takes"));
}

#[test]
fn tool_wide_switches_and_reset_all() {
    let t = tool(
        example("noeul"),
        Some(
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
                    "overrides": [{"action": "settings.open", "bindings": ["Ctrl+Shift+Comma"]}]}),
        ),
    );
    let off = edit::apply_switch(&[t.clone()], &[], 0, &Switch::DisableAll);
    assert_eq!(off.layer.as_ref().unwrap().enabled, Some(false));
    let mut t_off = t.clone();
    t_off.layer = off.layer;
    assert!(
        analyze(&[t_off.clone()], &[]).active.is_empty(),
        "no binding is active"
    );
    let on = edit::apply_switch(&[t_off], &[], 0, &Switch::EnableAll);
    assert_eq!(on.layer.as_ref().unwrap().enabled, None);
    let no_global =
        edit::apply_switch(&[t.clone()], &[], 0, &Switch::DisableScope("global".into()));
    let mut t_ng = t.clone();
    t_ng.layer = no_global.layer;
    let a = analyze(&[t_ng], &[]);
    assert!(
        a.active
            .iter()
            .all(|b| b.scope != kim_hotkeys::Scope::Global)
    );
    assert!(
        a.active.iter().any(|b| b.action == "capture.cancel"),
        "contexts stay on"
    );
    let reset = edit::apply_switch(&[t], &[], 0, &Switch::ResetAll);
    assert!(reset.layer.unwrap().overrides.is_none());
    assert_eq!(reset.changes[0].action, "settings.open");
}

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn invalid_configs_roll_back_to_the_previous_bindings() {
    let d = Dir(std::env::temp_dir().join(format!("kim-hotkeys-rollback-{}", std::process::id())));
    std::fs::create_dir_all(&d.0).unwrap();
    let p = d.0.join(store::FILE_NAME);
    let m = example("noeul");
    std::fs::write(
        &p,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
               "overrides": [{"action": "capture.region", "disabled": true}]})
        .to_string(),
    )
    .unwrap();
    let good = store::load(&p, &m);
    assert_eq!(good.state, ConfigState::User);
    // The user breaks the file while the tool runs: the previous bindings stay, the global
    // hotkey the user disabled is not re-enabled, and the file is left as it is.
    std::fs::write(&p, b"{\"schema\": \"kim.hotkeys\", oops").unwrap();
    let (kept, problems) = store::reload(&good, &m);
    assert_eq!(kept, good);
    assert!(!problems.is_empty());
    assert_eq!(
        std::fs::read(&p).unwrap(),
        b"{\"schema\": \"kim.hotkeys\", oops"
    );
    // Starting with the broken file: safe mode, app/context defaults, nothing global.
    let broken = store::load(&p, &m);
    assert!(broken.state.safe_mode());
    let mut s = ToolState::new(m.clone(), broken.layer);
    s.safe_mode = true;
    let a = analyze(&[s], &[]);
    assert!(
        a.active
            .iter()
            .all(|b| b.scope != kim_hotkeys::Scope::Global),
        "KIM-SEC-023"
    );
    assert!(a.active.iter().any(|b| b.action == "settings.open"));
    // A fixed file is picked up again.
    std::fs::write(
        &p,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul"}).to_string(),
    )
    .unwrap();
    let (fixed, problems) = store::reload(&broken_state(&p), &m);
    assert_eq!(fixed.state, ConfigState::User);
    assert!(problems.is_empty());
}

fn broken_state(p: &Path) -> store::Loaded {
    store::Loaded {
        path: p.to_path_buf(),
        state: ConfigState::Invalid,
        layer: None,
        problems: vec!["was broken".into()],
        migrated_from: None,
    }
}

#[test]
fn refused_changes_never_alter_the_layer() {
    let before = json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim",
        "overrides": [{"action": "tool.install", "bindings": ["Ctrl+I"]}]});
    let t = tool(example("kim"), Some(before));
    let e = Edit::Set {
        action: "tool.update".into(),
        bindings: text(&["Ctrl+I"]),
    };
    let r = edit::apply(&[t.clone()], &[], 0, &e, &Options::default()).unwrap_err();
    assert_eq!(r.error, EditError::HotkeyConflict);
    assert!(r.hint.unwrap().contains("--replace"));
    // With --replace both changes are listed and tool.install is disabled.
    let ok = edit::apply(
        &[t],
        &[],
        0,
        &e,
        &Options {
            replace: true,
            ..Options::default()
        },
    )
    .unwrap();
    let changed: Vec<&str> = ok.changes.iter().map(|c| c.action.as_str()).collect();
    assert_eq!(changed, ["tool.update", "tool.install"]);
    assert_eq!(ok.changes[1].after.source, "disabled");
}
