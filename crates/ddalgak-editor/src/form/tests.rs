//! The rule form: every effect's validation, the canonical rule it saves, round trips through
//! the stored rule, summaries and the simulation (`DDALGAK-FR-050`, `DDALGAK-FR-052`).

use super::*;
use ddalgak::model::{Document, Mode};
use ddalgak::store;
use ddalgak::validate::Context;

fn profile() -> Profile {
    let mut d = Document::empty();
    let mut p = d.profiles.remove(0);
    p.modes.push(Mode::new("nav", "Navigation"));
    p
}

fn with(effect: Effect, keys: &str) -> RuleForm {
    let mut f = RuleForm::new("default");
    f.fields.trigger.keys = keys.into();
    f.fields.effect = Some(effect);
    f.fields.title = "A rule".into();
    f.fields.title_auto = false;
    f
}

fn paths(f: &RuleForm) -> Vec<&'static str> {
    f.problems(Some(&profile()))
        .iter()
        .map(|p| p.path)
        .collect()
}

/// The errors the document checks find for a rule (inside a document of its own).
fn document_errors(rule: &Rule) -> Vec<String> {
    let mut d = Document::empty();
    d.profiles[0].modes.push(Mode::new("nav", "Navigation"));
    let mut r = rule.clone();
    r.enabled = false;
    d.profiles[0]
        .rules
        .push(ddalgak::model::RuleEntry::Typed(Box::new(r)));
    let l = store::interpret(
        d.to_text().as_bytes(),
        &Context::default(),
        store::MIGRATIONS,
    );
    let mut out: Vec<String> = l
        .report
        .all()
        .filter(|p| p.severity == validate::Severity::Error)
        .map(|p| p.code.to_string())
        .collect();
    if !l.report.schema_invalid.is_empty() {
        out.push("schema".into());
    }
    out
}

/// The built rule passes the rules schema and the document checks inside a document.
fn valid_in_document(rule: &Rule) {
    let mut d = Document::empty();
    d.profiles[0].modes.push(Mode::new("nav", "Navigation"));
    let mut r = rule.clone();
    r.enabled = false; // consent is the dialog's business
    d.profiles[0]
        .rules
        .push(ddalgak::model::RuleEntry::Typed(Box::new(r)));
    let l = store::interpret(
        d.to_text().as_bytes(),
        &Context::default(),
        store::MIGRATIONS,
    );
    assert!(l.state.usable(), "{:?}", l.state);
    let errors: Vec<_> = l
        .report
        .all()
        .filter(|p| p.severity == validate::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(l.report.schema_invalid.is_empty(), "{:?}", l.report);
}

#[test]
fn the_trigger_is_captured_or_typed_and_checked_like_the_cli() {
    let mut f = with(Effect::Engine, "");
    assert!(paths(&f).contains(&"trigger.keys"));
    f.fields.trigger.keys = "ctrl+shift+f2".into();
    assert!(paths(&f).is_empty(), "{:?}", f.problems(None));
    assert_eq!(
        f.build("r1", None).unwrap().trigger.keys.as_deref(),
        Some("Ctrl+Shift+F2")
    );
    // Reserved chords are refused with the reason, before anything is saved.
    for bad in [
        "Win+L",
        "Ctrl+Alt+Delete",
        "F12",
        "Ctrl+Alt+Shift+Escape",
        "Ctrl+Bogus",
    ] {
        f.fields.trigger.keys = bad.into();
        assert!(paths(&f).contains(&"trigger.keys"), "{bad}");
        assert!(f.build("r1", None).is_err(), "{bad}");
    }
    // Hold time limits.
    f.fields.trigger.keys = "F13".into();
    f.fields.trigger.on = TriggerOn::Hold;
    f.fields.trigger.hold_ms = 100;
    assert!(paths(&f).contains(&"trigger.hold_ms"));
    f.fields.trigger.hold_ms = 800;
    let r = f.build("r1", None).unwrap();
    assert_eq!(r.trigger.on, Some(TriggerOn::Hold));
    assert_eq!(r.trigger.hold_ms, Some(800));
}

#[test]
fn act_as_another_key_and_block() {
    let mut f = with(Effect::Remap, "CapsLock");
    assert!(paths(&f).contains(&"steps[0].to"));
    f.fields.remap.to = "lctrl".into();
    f.fields.remap.tap = "esc".into();
    let r = f.build("caps", None).unwrap();
    assert_eq!(r.steps[0].to_value()["to"], "LCtrl");
    assert_eq!(r.steps[0].to_value()["tap"], "Escape");
    assert!(
        r.steps[0].to_value().get("tap_ms").is_none(),
        "the default is left out"
    );
    assert_eq!(
        f.summary(None),
        "CapsLock acts as Left Ctrl; a quick tap is Escape"
    );
    valid_in_document(&r);
    f.fields.remap.tap_ms = 50;
    assert!(paths(&f).contains(&"steps[0].tap_ms"));
    f.fields.remap.tap_ms = 300;
    f.fields.trigger.pass_through = true;
    assert!(
        paths(&f).contains(&"trigger.pass_through"),
        "not with a tap"
    );
    f.fields.trigger.pass_through = false;
    // The emergency chord is never an output: the document checks refuse it before Save.
    f.fields.remap.to = "Ctrl+Alt+Shift+Escape".into();
    let r = f.build("caps", None).unwrap();
    assert!(
        !document_errors(&r).is_empty(),
        "the emergency chord is refused"
    );
    let b = with(Effect::Block, "Insert");
    let r = b.build("ins", None).unwrap();
    assert_eq!(r.steps[0].kind(), "block");
    assert_eq!(b.summary(None), "Insert does nothing (blocked)");
    valid_in_document(&r);
}

#[test]
fn press_a_shortcut() {
    let mut f = with(Effect::SendKeys, "F13");
    assert!(paths(&f).contains(&"steps[0].keys"));
    f.fields.keys.keys = "ctrl+k ctrl+v".into();
    let r = f.build("sk", None).unwrap();
    assert_eq!(r.steps[0].to_value()["keys"], "Ctrl+K Ctrl+V");
    assert_eq!(f.summary(None), "Presses Ctrl+K, then Ctrl+V");
    valid_in_document(&r);
    f.fields.keys.step_delay_ms = 5000;
    assert!(paths(&f).contains(&"steps[0].step_delay_ms"));
}

#[test]
fn type_text_and_hotstrings() {
    let mut f = with(Effect::TypeText, "Ctrl+Shift+F2");
    assert!(paths(&f).contains(&"steps[0].text"));
    f.fields.text.text = "감사합니다.\n김민수 드림".into();
    let r = f.build("thanks", None).unwrap();
    assert_eq!(r.steps[0].to_value()["text"], "감사합니다.\n김민수 드림");
    assert_eq!(f.summary(None), "Types 13 characters on 2 lines");
    valid_in_document(&r);
    assert_eq!(r.permissions_needed(), [Permission::InputInjection]);
    f.fields.text.paste = true;
    let r = f.build("thanks", None).unwrap();
    assert_eq!(
        r.permissions_needed(),
        [Permission::InputInjection, Permission::Clipboard]
    );
    f.fields.text.variables = true;
    f.fields.text.text = "{nope}".into();
    assert!(paths(&f).contains(&"steps[0].text"), "unknown variable");
    f.fields.text.text = "Today: {date}{cursor}".into();
    assert!(paths(&f).is_empty());
    // A typed abbreviation instead of keys.
    f.fields.trigger.hotstring = true;
    f.fields.trigger.abbreviation = "btw".into();
    let r = f.build("btw", None).unwrap();
    assert_eq!(r.trigger.hotstring.as_ref().unwrap().abbreviation, "btw");
    assert!(r.trigger.keys.is_none());
    valid_in_document(&r);
    f.fields.trigger.abbreviation = "two words".into();
    assert!(paths(&f).contains(&"trigger.hotstring.abbreviation"));
    // Only typing effects take an abbreviation.
    let mut g = with(Effect::Engine, "");
    g.fields.trigger.hotstring = true;
    g.fields.trigger.abbreviation = "pp".into();
    assert!(paths(&g).contains(&"trigger.hotstring.abbreviation"));
}

#[test]
fn open_a_program_never_runs_it() {
    let mut f = with(Effect::OpenProgram, "Ctrl+Alt+Shift+F14");
    assert!(paths(&f).contains(&"steps[0].path"));
    f.fields.program.path = r"C:\Windows\System32\cmd.exe".into();
    assert!(paths(&f).contains(&"steps[0].path"), "shells are refused");
    f.fields.program.path = r#""C:\Windows\System32\notepad.exe""#.into();
    f.fields.program.args = "a.txt\n\nb c.txt".into();
    f.fields.program.window = "maximized".into();
    let r = f.build("np", None).unwrap();
    let s = r.steps[0].to_value();
    assert_eq!(
        s["path"], r"C:\Windows\System32\notepad.exe",
        "quotes from Copy as path are removed"
    );
    assert_eq!(s["args"], serde_json::json!(["a.txt", "b c.txt"]));
    assert_eq!(s["window"], "maximized");
    assert_eq!(r.permissions_needed(), [Permission::LaunchPrograms]);
    valid_in_document(&r);
    assert!(
        f.summary(None).starts_with("Opens notepad.exe"),
        "{}",
        f.summary(None)
    );
    // The simulation names it and opens nothing.
    let sim = f.simulate(None, &Layout::single(800, 600), Point::new(1, 1));
    assert!(sim.fires);
    assert!(
        sim.lines
            .iter()
            .any(|l| l.contains("notepad.exe") && l.contains("nothing was opened")),
        "{sim:?}"
    );
    // An app ID instead of a path.
    f.fields.program.path.clear();
    f.fields.program.args.clear();
    f.fields.program.app_id = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into();
    valid_in_document(&f.build("calc", None).unwrap());
    f.fields.program.app_id = "not an app".into();
    assert!(paths(&f).contains(&"steps[0].app_id"));
}

#[test]
fn open_a_website() {
    let mut f = with(Effect::OpenUrl, "Ctrl+Alt+Shift+F15");
    assert!(paths(&f).contains(&"steps[0].url"));
    f.fields.url = "GitHub.com/hellomouv".into();
    let r = f.build("gh", None).unwrap();
    assert_eq!(r.steps[0].to_value()["url"], "https://github.com/hellomouv");
    assert_eq!(f.summary(None), "Opens github.com in your browser");
    valid_in_document(&r);
    for bad in [
        "javascript:alert(1)",
        "file:///C:/x",
        "https://user:pw@example.com/",
    ] {
        f.fields.url = bad.into();
        assert!(paths(&f).contains(&"steps[0].url"), "{bad}");
    }
}

#[test]
fn the_mouse_forms_round_trip() {
    let mut f = with(Effect::Mouse, "Ctrl+Alt+Shift+Right");
    f.fields.mouse = MouseAction::MoveBy {
        dx: 40,
        dy: 0,
        grid: None,
        hold: HoldChoice::Continuous {
            speed: 600,
            acceleration: 0,
        },
    };
    let r = f.build("mv", None).unwrap();
    valid_in_document(&r);
    assert_eq!(RuleForm::edit("default", &r).fields.mouse, f.fields.mouse);
    assert!(f.summary(None).contains("600 px/s"), "{}", f.summary(None));
    let sim = f.simulate(None, &Layout::single(1920, 1080), Point::new(960, 540));
    let (from, to) = sim.pointer.unwrap();
    assert!(to.x > from.x, "the simulated pointer moved right: {sim:?}");
    for a in [
        mouse_default(1),
        mouse_default(2),
        mouse_default(3),
        MouseAction::MoveBy {
            dx: 1,
            dy: 0,
            grid: Some(Grid {
                columns: 4,
                rows: 4,
            }),
            hold: HoldChoice::Repeat {
                repeat_ms: 100,
                delay_ms: 300,
            },
        },
        MouseAction::Click {
            button: Button::Right,
            op: "double_click",
            at: Some(PointSpec {
                x: 5,
                y: 6,
                anchor: Anchor::ActiveWindow,
            }),
        },
    ] {
        f.fields.mouse = a;
        let r = f
            .build("mv", None)
            .unwrap_or_else(|p| panic!("{a:?}: {p:?}"));
        valid_in_document(&r);
        assert_eq!(RuleForm::edit("default", &r).fields.mouse, a);
    }
    f.fields.mouse = MouseAction::Scroll {
        dir: ScrollDir::Down,
        notches: 99,
    };
    assert!(paths(&f).contains(&"steps[0].notches"));
}

#[test]
fn switch_mode_and_pause_ddalgak() {
    let p = profile();
    let mut f = with(Effect::Mode, "CapsLock");
    assert!(paths(&f).contains(&"steps[0].mode"));
    f.fields.mode.mode = "nav".into();
    f.fields.mode.op = ModeOp::Hold;
    let r = f.build("nav", Some(&p)).unwrap();
    valid_in_document(&r);
    assert!(
        f.summary(Some(&p)).contains("Navigation"),
        "{}",
        f.summary(Some(&p))
    );
    f.fields.mode.mode = "nope".into();
    assert!(paths(&f).contains(&"steps[0].mode"));
    f.fields.mode.op = ModeOp::Cycle;
    f.fields.mode.modes = vec!["base".into()];
    assert!(paths(&f).contains(&"steps[0].modes"));
    f.fields.mode.modes.push("nav".into());
    valid_in_document(&f.build("cy", Some(&p)).unwrap());
    let sim = f.simulate(Some(&p), &Layout::single(10, 10), Point::new(0, 0));
    assert!(
        sim.lines
            .iter()
            .any(|l| l.contains("the engine's mode is unchanged"))
    );

    let mut e = with(Effect::Engine, "F13");
    for op in [
        EngineOp::Pause,
        EngineOp::Resume,
        EngineOp::TogglePause,
        EngineOp::Disable,
    ] {
        e.fields.engine = op;
        let r = e.build("en", None).unwrap();
        valid_in_document(&r);
        assert!(
            r.permissions_needed().is_empty(),
            "engine rules need no consent"
        );
    }
    assert_eq!(
        e.summary(None),
        "Turns Ddalgak off (only you can turn it on again)"
    );
}

#[test]
fn editing_keeps_fields_the_form_does_not_show_and_detects_changes() {
    let v = serde_json::json!({
        "id": "keep", "title": "Keep me", "enabled": true, "priority": 5,
        "trigger": { "keys": "F13", "injected": "accept", "repeat": true },
        "when": { "apps": [ { "exe": "Code.exe" } ], "window_title": { "contains": "x" } },
        "steps": [ { "type": "engine", "op": "pause" } ],
        "future_field": { "kept": true }
    });
    let rule: Rule = serde_json::from_value(v).unwrap();
    let mut f = RuleForm::edit("default", &rule);
    assert!(!f.dirty());
    assert_eq!(f.fields.apps, "Code.exe");
    f.fields.title = "Renamed".into();
    assert!(f.dirty());
    let r = f.build("ignored", None).unwrap();
    assert_eq!(r.id, "keep");
    assert_eq!(r.title, "Renamed");
    assert_eq!(r.priority, Some(5));
    assert_eq!(
        r.trigger.injected,
        Some(ddalgak::model::InjectedPolicy::Accept)
    );
    assert_eq!(r.trigger.repeat, Some(true));
    assert!(r.when.as_ref().unwrap().window_title.is_some());
    assert_eq!(r.extra["future_field"]["kept"], true);
    // Several steps are not edited in the form (Step 78); the reason says so.
    let mut two = rule;
    two.steps.push(two.steps[0].clone());
    let f2 = RuleForm::edit("default", &two);
    assert!(f2.unsupported.as_deref().unwrap().contains("2 steps"));
    assert!(f2.build("x", None).is_err());
}

#[test]
fn gui_consent_names_the_exact_targets_and_records_via_gui() {
    let mut f = with(Effect::OpenUrl, "Ctrl+Alt+Shift+F16");
    f.fields.url = "example.com".into();
    let mut r = f.build("site", None).unwrap();
    assert_eq!(validate::missing_consent(&r), [Permission::LaunchPrograms]);
    assert_eq!(launch_targets(&r), ["https://example.com/"]);
    grant_gui(&mut r, &[Permission::LaunchPrograms, Permission::Clipboard]);
    assert!(validate::missing_consent(&r).is_empty());
    let g = r.grant(Permission::LaunchPrograms).unwrap();
    assert_eq!(g.via, "gui");
    assert!(g.fingerprint.as_deref().unwrap().starts_with("sha256:"));
    assert!(
        r.grant(Permission::Clipboard).is_none(),
        "only what the rule needs"
    );
    // A changed target makes the consent stale.
    f.fields.url = "example.org".into();
    let mut changed = f.build("site", None).unwrap();
    changed.consent = r.consent.clone();
    assert_eq!(
        validate::missing_consent(&changed),
        [Permission::LaunchPrograms]
    );
}

#[test]
fn starters_are_drafts_with_sensible_defaults() {
    for e in [Effect::Remap, Effect::TypeText, Effect::OpenProgram] {
        let f = RuleForm::starter("default", e);
        assert!(f.dirty(), "a starter is an unsaved draft");
        assert!(f.problems(None).is_empty(), "{e:?}: {:?}", f.problems(None));
        assert!(!f.fields.title.is_empty());
    }
}

/// Acceptance 2: every action category the form saves is what the engine runs. The rules the
/// form builds (enabled, with the editor's consent) become active bindings in the engine's own
/// loader (`rules::from_store`, what the tray app applies on reload), none inactive.
#[test]
fn every_category_the_form_saves_becomes_an_active_engine_binding() {
    let p = profile();
    let mut forms = Vec::new();
    let mut f = with(Effect::Remap, "Ctrl+Alt+Shift+F13");
    f.fields.remap.to = "F20".into();
    forms.push(f);
    let mut f = with(Effect::SendKeys, "Ctrl+Alt+Shift+F14");
    f.fields.keys.keys = "Ctrl+C".into();
    forms.push(f);
    let mut f = with(Effect::TypeText, "Ctrl+Alt+Shift+F15");
    f.fields.text.text = "감사합니다.".into();
    forms.push(f);
    let mut f = with(Effect::OpenProgram, "Ctrl+Alt+Shift+F16");
    f.fields.program.path = r"C:\Windows\System32\notepad.exe".into();
    forms.push(f);
    let mut f = with(Effect::OpenUrl, "Ctrl+Alt+Shift+F17");
    f.fields.url = "example.com".into();
    forms.push(f);
    let mut f = with(Effect::Mouse, "Ctrl+Alt+Shift+F18");
    f.fields.mouse = MouseAction::MoveBy {
        dx: 10,
        dy: 0,
        grid: None,
        hold: HoldChoice::Once,
    };
    forms.push(f);
    let mut f = with(Effect::Mode, "Ctrl+Alt+Shift+F19");
    f.fields.mode.mode = "nav".into();
    forms.push(f);
    let mut f = with(Effect::Engine, "Ctrl+Alt+Shift+F20");
    f.fields.engine = EngineOp::Pause;
    forms.push(f);
    forms.push(with(Effect::Block, "Ctrl+Alt+Shift+F21"));
    let mut d = Document::empty();
    d.profiles[0].modes.push(Mode::new("nav", "Navigation"));
    let mut ids = Vec::new();
    for (i, f) in forms.iter().enumerate() {
        let id = format!("cat-{i}");
        let mut r = f
            .build(&id, Some(&p))
            .unwrap_or_else(|e| panic!("{i}: {e:?}"));
        r.enabled = true;
        let needed = r.permissions_needed();
        grant_gui(&mut r, &needed);
        d.profiles[0]
            .rules
            .push(ddalgak::model::RuleEntry::Typed(Box::new(r)));
        ids.push(id);
    }
    let l = store::interpret(
        d.to_text().as_bytes(),
        &Context::default(),
        store::MIGRATIONS,
    );
    assert!(l.state.usable(), "{:?}", l.state);
    let engine = ddalgak::rules::from_store(&l);
    assert!(engine.inactive.is_empty(), "{:?}", engine.inactive);
    for id in &ids {
        assert!(
            engine.bindings.iter().any(|b| &b.rule == id),
            "{id} is not an active binding"
        );
    }
}
