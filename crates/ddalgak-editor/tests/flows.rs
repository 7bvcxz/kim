//! Headless flow tests of the Ddalgak rule editor (egui_kittest: no window, no GPU), driven
//! through the accessibility tree like a keyboard or screen-reader user, against an isolated
//! rules folder (docs/DDALGAK_GUI_JOURNEYS.md J1–J4, J7; `DDALGAK-FR-050…053`,
//! `DDALGAK-NFR-007`, `DDALGAK-SEC-013`).
//!
//! Machine safety: nothing here sends a key to another program, moves the pointer, opens a
//! program or a website, or starts the tray app. The engine is `FakeEngine` (it reloads with the
//! engine's own loader and answers in the app's reply shape) or `NoEngine`; "Test" is the
//! simulate mode.

use ddalgak::paths::Paths;
use ddalgak_editor::form::Effect;
use ddalgak_editor::i18n::Lang;
use ddalgak_editor::listing::{Grouping, StateFilter};
use ddalgak_editor::repo::Repo;
use ddalgak_editor::sync::{Engine, FakeEngine, NoEngine, SaveState};
use ddalgak_editor::{EditorApp, Options};
use eframe::egui::{self, Key, Modifiers, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Sandbox {
    root: PathBuf,
    paths: Paths,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let root =
            std::env::temp_dir().join(format!("ddalgak-editor-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Sandbox {
            paths: Paths::under(&root),
            root,
        }
    }

    fn write_rules(&self, doc: &Value) {
        std::fs::create_dir_all(&self.paths.config).unwrap();
        std::fs::write(self.paths.rules(), serde_json::to_vec_pretty(doc).unwrap()).unwrap();
    }

    fn rules(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.paths.rules()).unwrap()).unwrap()
    }

    fn fake(&self) -> Arc<FakeEngine> {
        Arc::new(FakeEngine::new(self.paths.rules()))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const GRANT: &str = "2026-11-04T09:00:00Z";

#[allow(clippy::needless_pass_by_value)]
fn doc(rules: Value) -> Value {
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 1,
        "active_profile": "work",
        "profiles": [ { "id": "work", "title": "Work", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
            "rules": rules } ]
    })
}

fn three_rules() -> Value {
    doc(json!([
        { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true,
          "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl", "tap": "Escape" } ],
          "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
        { "id": "pause", "title": "F13 pauses", "enabled": true,
          "trigger": { "keys": "Ctrl+Alt+Shift+F13" }, "steps": [ { "type": "engine", "op": "toggle_pause" } ] },
        { "id": "thanks", "title": "Thanks", "enabled": false, "modes": ["nav"],
          "trigger": { "keys": "Ctrl+Shift+F2" }, "steps": [ { "type": "type_text", "text": "감사합니다." } ] }
    ]))
}

fn app(sb: &Sandbox, engine: Arc<dyn Engine>) -> EditorApp {
    EditorApp::new(Options::for_test(sb.paths.clone(), engine))
}

fn harness(app: EditorApp) -> Harness<'static, EditorApp> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_ui_state(|ui, app: &mut EditorApp| app.ui(ui), app)
}

/// Runs frames, waits for the app's answers, runs again.
fn settle(h: &mut Harness<'static, EditorApp>) {
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(10)));
    h.run();
}

fn click(h: &mut Harness<'static, EditorApp>, label: &str) {
    h.get_by_label(label).click();
    h.run();
}

/// The last button with this label (a dialog's button is drawn after the panel behind it).
fn click_last_button(h: &mut Harness<'static, EditorApp>, label: &str) {
    h.get_all_by_role_and_label(Role::Button, label)
        .last()
        .unwrap_or_else(|| panic!("no button {label}"))
        .click();
    h.run();
}

fn rule<'a>(d: &'a Value, id: &str) -> &'a Value {
    d["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["rules"].as_array().unwrap())
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no rule {id}"))
}

fn ids(d: &Value) -> Vec<String> {
    d["profiles"][0]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// J1: the first start shows one sentence and three starters, creates nothing, and says that
/// Ddalgak is not running.
#[test]
fn j1_first_start_is_safe_and_a_starter_is_only_a_draft() {
    let sb = Sandbox::new("j1");
    let mut h = harness(app(&sb, Arc::new(NoEngine)));
    settle(&mut h);
    assert!(h.query_by_label_contains("No rules yet").is_some());
    assert!(
        h.query_by_label_contains("Emergency off: Ctrl+Alt+Shift+Esc")
            .is_some()
    );
    assert!(h.query_by_label("○ Ddalgak is not running").is_some());
    assert!(
        !sb.paths.rules().exists(),
        "opening the editor writes nothing"
    );
    click(&mut h, "CapsLock acts as Ctrl");
    let f = h.state().form().expect("the starter opens the form");
    assert_eq!(f.fields.effect, Some(Effect::Remap));
    assert!(h.state().has_unsaved_changes());
    assert!(
        !sb.paths.rules().exists(),
        "a starter is a draft until saved"
    );
    assert!(h.query_by_label_contains("draft: not saved").is_some());
}

/// J2/J3/J4: capture the keys, pick a tile, fill the form, simulate, save and enable; consent is
/// asked first (Cancel focused), and "saved and active" appears only after the app's reload.
#[test]
fn j2_create_a_rule_with_capture_tile_form_simulation_and_consent() {
    let sb = Sandbox::new("j2");
    let fake = sb.fake();
    let mut h = harness(app(&sb, fake.clone()));
    settle(&mut h);
    click(&mut h, "+ New rule");
    // The capture box records on key-down.
    h.key_press_modifiers(Modifiers::CTRL | Modifiers::SHIFT, Key::F2);
    h.run();
    assert_eq!(
        h.state().form().unwrap().fields.trigger.keys,
        "Ctrl+Shift+F2"
    );
    assert!(
        h.query_by_label("Control plus Shift plus F2").is_some(),
        "announced in words"
    );
    h.key_press(Key::Escape); // ends the capture; the value stays
    h.run();
    assert!(
        h.state().form().is_some(),
        "Esc in the capture box does not close the form"
    );
    click(&mut h, "Next ▸");
    click(&mut h, "Type text");
    assert_eq!(
        h.state().form().unwrap().fields.effect,
        Some(Effect::TypeText)
    );
    h.get_by_label("Text")
        .set_text_value_accesskit("감사합니다.\n김민수 드림");
    h.run();
    assert!(
        h.query_all_by_label_contains("Types 13 characters on 2 lines")
            .next()
            .is_some()
    );
    click(&mut h, "4 Try");
    click(&mut h, "▶ Simulate");
    let sim = h.state().simulation().expect("simulated").clone();
    assert!(sim.fires);
    assert_eq!(sim.typed.as_deref(), Some("감사합니다.\n김민수 드림"));
    assert!(
        !sb.paths.rules().exists(),
        "simulating writes and types nothing"
    );
    click(&mut h, "Save and enable");
    // Consent first: the safe choice has the focus.
    assert!(h.query_by_label("Allow this rule?").is_some());
    assert!(
        h.get_all_by_role_and_label(Role::Button, "Cancel")
            .last()
            .unwrap()
            .is_focused()
    );
    assert!(
        !sb.paths.rules().exists(),
        "nothing is written before Allow"
    );
    click_last_button(&mut h, "Allow");
    let saved = sb.rules();
    let r = &saved["profiles"][0]["rules"][0];
    assert_eq!(r["enabled"], true);
    assert_eq!(r["trigger"]["keys"], "Ctrl+Shift+F2");
    assert_eq!(r["consent"]["input-injection"]["via"], "gui");
    settle(&mut h);
    let rev = saved["revision"].as_u64().unwrap();
    assert_eq!(h.state().save_state(), &SaveState::Active { revision: rev });
    assert!(h.query_by_label_contains("saved and active").is_some());
    assert!(fake.with(|s| s.calls.iter().any(|c| c == "rules.reload")));
    assert!(h.state().form().is_none());
}

/// DDALGAK-FR-052: a save shows "saved; waiting" until the app's reload answer arrives, then
/// "saved and active"; with the engine off or Ddalgak not running it says so instead.
#[test]
fn a_save_is_acknowledged_only_after_the_engine_reload() {
    let sb = Sandbox::new("ack");
    sb.write_rules(&three_rules());
    let fake = sb.fake();
    let mut h = harness(app(&sb, fake.clone()));
    settle(&mut h);
    assert_eq!(h.state().save_state(), &SaveState::Active { revision: 1 });
    fake.with(|s| s.delay_ms = 400);
    click(
        &mut h,
        "Ctrl+Alt+Shift+F13  →  Pauses all rules, or resumes them when paused",
    );
    click(&mut h, "Disable");
    let rev = sb.rules()["revision"].as_u64().unwrap();
    assert_eq!(rev, 2);
    assert_eq!(h.state().save_state(), &SaveState::Waiting { revision: 2 });
    assert!(
        h.query_by_label_contains("saved; waiting for Ddalgak to load it")
            .is_some()
    );
    assert!(
        h.query_by_label_contains("saved and active").is_none(),
        "never shown before the engine confirmed"
    );
    settle(&mut h);
    assert_eq!(h.state().save_state(), &SaveState::Active { revision: 2 });
    // The engine is off: saved and loaded, not active.
    fake.with(|s| {
        s.delay_ms = 0;
        s.engine = "off".into();
    });
    click(&mut h, "Enable");
    settle(&mut h);
    assert_eq!(
        h.state().save_state(),
        &SaveState::Loaded {
            revision: 3,
            engine: "off".into()
        }
    );
    // Ddalgak is not running.
    fake.with(|s| s.running = false);
    click(&mut h, "Disable");
    settle(&mut h);
    assert_eq!(
        h.state().save_state(),
        &SaveState::NotRunning { revision: 4 }
    );
    assert!(
        h.query_by_label_contains("Ddalgak is not running, so nothing is active")
            .is_some()
    );
}

/// J7: edit, duplicate, enable with consent, disable, delete with confirmation and undo.
#[test]
fn edit_duplicate_enable_disable_delete_and_undo() {
    let sb = Sandbox::new("crud");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    // Edit opens the panel at step 3 with the rule's values.
    click(
        &mut h,
        "CapsLock  →  CapsLock acts as Left Ctrl; a quick tap is Escape",
    );
    click(&mut h, "Edit");
    assert!(h.state().form().unwrap().id.is_some());
    h.get_by_label("Name")
        .set_text_value_accesskit("Caps is Ctrl");
    h.run();
    assert!(h.state().has_unsaved_changes());
    click(&mut h, "4 Try");
    click(&mut h, "Save");
    let d = sb.rules();
    assert_eq!(rule(&d, "caps")["title"], "Caps is Ctrl");
    assert_eq!(
        rule(&d, "caps")["consent"]["input-injection"]["via"],
        "gui",
        "consent kept"
    );
    // Duplicate: a disabled copy right after the original.
    click(&mut h, "Duplicate");
    let d = sb.rules();
    let all = ids(&d);
    assert_eq!(all.len(), 4);
    let copy = all[1].clone();
    assert_eq!(rule(&d, &copy)["title"], "Caps is Ctrl (copy)");
    assert_eq!(rule(&d, &copy)["enabled"], false);
    // Enabling the disabled snippet needs consent: Cancel keeps it off, Allow enables it.
    click(&mut h, "Ctrl+Shift+F2  →  Types 6 characters on 1 line");
    click(&mut h, "Enable");
    assert!(h.query_by_label("Allow this rule?").is_some());
    click_last_button(&mut h, "Cancel");
    assert_eq!(rule(&sb.rules(), "thanks")["enabled"], false);
    click(&mut h, "Enable");
    click_last_button(&mut h, "Allow");
    let d = sb.rules();
    assert_eq!(rule(&d, "thanks")["enabled"], true);
    assert_eq!(
        rule(&d, "thanks")["consent"]["input-injection"]["via"],
        "gui"
    );
    // Delete asks first, Cancel focused; then an Undo toast that stays until dismissed.
    click(&mut h, "Delete");
    assert!(h.query_by_label("Delete rules").is_some());
    assert!(
        h.get_all_by_role_and_label(Role::Button, "Cancel")
            .last()
            .unwrap()
            .is_focused()
    );
    click_last_button(&mut h, "Delete (1)");
    assert!(!ids(&sb.rules()).contains(&"thanks".to_string()));
    assert!(
        h.query_by_label_contains("Deleted 1 rule(s): Thanks")
            .is_some()
    );
    for _ in 0..5 {
        h.run();
    }
    assert!(
        h.query_by_label_contains("Deleted 1 rule(s)").is_some(),
        "the toast stays"
    );
    click_last_button(&mut h, "Undo");
    let d = sb.rules();
    assert!(ids(&d).contains(&"thanks".to_string()), "undo restores it");
    assert_eq!(rule(&d, "thanks")["enabled"], true);
}

/// Every effect tile opens its own form, and its problems are shown in words with Save blocked
/// until the form is valid.
#[test]
fn every_action_form_validates_in_the_window() {
    let sb = Sandbox::new("forms");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    let tiles = [
        ("Act as another key", Effect::Remap),
        ("Press a shortcut", Effect::SendKeys),
        ("Type text", Effect::TypeText),
        ("Open a program", Effect::OpenProgram),
        ("Open a website", Effect::OpenUrl),
        ("Mouse", Effect::Mouse),
        ("Switch mode", Effect::Mode),
        ("Pause Ddalgak", Effect::Engine),
        ("Block this key", Effect::Block),
    ];
    for (i, (tile, effect)) in tiles.into_iter().enumerate() {
        click(&mut h, "+ New rule");
        h.key_press(Key::Escape);
        h.run();
        let keys = format!("Ctrl+Alt+Shift+F{}", 13 + i);
        h.get_by_label("Type instead: When I press")
            .set_text_value_accesskit(&keys);
        h.run();
        click(&mut h, "Next ▸");
        click(&mut h, tile);
        assert_eq!(
            h.state().form().unwrap().fields.effect,
            Some(effect),
            "{tile}"
        );
        let needs_input = !matches!(effect, Effect::Engine | Effect::Block | Effect::Mouse);
        click(&mut h, "4 Try");
        let blocked = !h
            .get_by_role_and_label(Role::Button, "Save and enable")
            .accesskit_node()
            .is_disabled();
        assert_eq!(
            !blocked, needs_input,
            "{tile}: Save blocked while incomplete"
        );
        if needs_input {
            assert!(
                h.query_all_by_label_contains("✕ ").next().is_some(),
                "{tile}: a problem is shown in words"
            );
        }
        // Fill the required field and the form is ready.
        click(&mut h, "3 Details");
        match effect {
            Effect::Remap => h
                .get_by_label("Type instead: Acts as")
                .set_text_value_accesskit("F20"),
            Effect::SendKeys => h
                .get_by_label("Type instead: Shortcut (up to eight chords)")
                .set_text_value_accesskit("Ctrl+C"),
            Effect::TypeText => h.get_by_label("Text").set_text_value_accesskit("hello"),
            Effect::OpenProgram => h
                .get_by_label("Program (.exe)")
                .set_text_value_accesskit(r"C:\Windows\System32\notepad.exe"),
            Effect::OpenUrl => h
                .get_by_label("Website")
                .set_text_value_accesskit("example.com"),
            Effect::Mode => {
                h.state_mut().form_mut().unwrap().fields.mode.mode = "nav".into();
            }
            _ => {}
        }
        h.run();
        let f = h.state().form().unwrap();
        assert!(
            f.problems(None)
                .iter()
                .all(|p| p.path.starts_with("steps[0].mode")),
            "{tile}: {:?}",
            f.problems(None)
        );
        click(&mut h, "Cancel");
        if h.state().dialog_open() {
            click(&mut h, "Discard changes");
        }
        assert!(h.state().form().is_none(), "{tile}");
    }
    assert_eq!(sb.rules()["revision"], 1, "trying the forms wrote nothing");
}

/// Search, filters, grouping and bulk enable; the list shows only summaries of personal content.
#[test]
fn search_filter_group_and_bulk_enable() {
    let sb = Sandbox::new("search");
    sb.write_rules(&doc(json!([
        { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true,
          "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl" } ],
          "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
        { "id": "sig", "title": "Signature", "enabled": false,
          "trigger": { "keys": "Ctrl+Shift+F3" }, "steps": [ { "type": "type_text", "text": "private words" } ],
          "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
        { "id": "p1", "title": "Pause one", "enabled": false, "modes": ["nav"],
          "trigger": { "keys": "Ctrl+Alt+Shift+F14" }, "steps": [ { "type": "engine", "op": "pause" } ] },
        { "id": "p2", "title": "Pause two", "enabled": false, "when": { "apps": [ { "exe": "Code.exe" } ] },
          "trigger": { "keys": "Ctrl+Alt+Shift+F15" }, "steps": [ { "type": "engine", "op": "pause" } ] }
    ])));
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    assert!(
        h.query_by_label_contains("private words").is_none(),
        "snippet text is not shown"
    );
    // Ctrl+F focuses the search box (rules.search), typing filters.
    h.key_press_modifiers(Modifiers::CTRL, Key::F);
    h.run();
    assert!(h.get_by_label("Search rules").is_focused());
    h.get_by_label("Search rules").type_text("caps");
    h.run();
    assert_eq!(h.state().visible_ids(), ["caps"]);
    h.state_mut().filter_mut().query = "private".into();
    h.run();
    assert!(h.state().visible_ids().is_empty());
    click(&mut h, "Also search snippet text");
    assert_eq!(h.state().visible_ids(), ["sig"]);
    h.state_mut().filter_mut().query.clear();
    h.state_mut().filter_mut().snippet_text = false;
    h.state_mut().filter_mut().state = StateFilter::Disabled;
    h.run();
    assert_eq!(h.state().visible_ids(), ["sig", "p1", "p2"]);
    h.state_mut().filter_mut().state = StateFilter::All;
    h.state_mut().filter_mut().app = Some("code.exe".into());
    h.run();
    assert_eq!(h.state().visible_ids(), ["p2"]);
    h.state_mut().filter_mut().app = None;
    h.state_mut().set_grouping(Grouping::Mode);
    h.run();
    assert!(h.query_by_label("Every mode").is_some());
    assert!(h.query_by_label("Navigation").is_some());
    h.state_mut().set_grouping(Grouping::App);
    h.run();
    assert!(h.query_by_label("Code.exe").is_some());
    h.state_mut().set_grouping(Grouping::None);
    h.run();
    // Bulk: select two with Ctrl+click, enable both at once.
    h.get_by_label_contains("Ctrl+Alt+Shift+F14").click();
    h.run();
    h.get_by_label_contains("Ctrl+Alt+Shift+F15")
        .click_modifiers(Modifiers::CTRL);
    h.run();
    assert_eq!(h.state().selected(), ["p1", "p2"]);
    assert!(h.query_by_label("2 selected").is_some());
    click(&mut h, "Enable");
    let d = sb.rules();
    assert_eq!(rule(&d, "p1")["enabled"], true);
    assert_eq!(rule(&d, "p2")["enabled"], true);
    assert_eq!(d["revision"], 2, "one change for the bulk action");
    // The app-limited rule is inactive (conditions arrive in Step 77): its badge says so.
    settle(&mut h);
    assert!(
        h.query_by_label_contains("inactive: condition_unsupported")
            .is_some()
    );
}

/// Drag ordering and Alt+Up/Alt+Down reorder within the profile (the list order is the last
/// precedence step).
#[test]
fn reorder_with_keys_and_drag() {
    let sb = Sandbox::new("order");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    click(
        &mut h,
        "CapsLock  →  CapsLock acts as Left Ctrl; a quick tap is Escape",
    );
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowDown);
    h.run();
    assert_eq!(ids(&sb.rules()), ["pause", "caps", "thanks"]);
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowUp);
    h.run();
    assert_eq!(ids(&sb.rules()), ["caps", "pause", "thanks"]);
    // Drag the third rule's handle onto the top half of the first row.
    let handle = h
        .get_by_label("Drag to reorder (or Alt+Up / Alt+Down): Thanks")
        .rect()
        .center();
    let target = h
        .get_by_label("CapsLock  →  CapsLock acts as Left Ctrl; a quick tap is Escape")
        .rect();
    h.drag_at(handle);
    h.run();
    for i in 0..6 {
        h.hover_at(handle + egui::vec2(0.0, (target.top() + 2.0 - handle.y) * (i as f32) / 5.0));
        h.run();
    }
    h.drop_at(egui::pos2(target.center().x, target.top() + 2.0));
    h.run();
    assert_eq!(ids(&sb.rules()), ["thanks", "caps", "pause"]);
}

/// Keyboard only (DDALGAK-NFR-007): every step of a journey works without the mouse.
#[test]
fn keyboard_only_navigation() {
    let sb = Sandbox::new("keys");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    // Down selects the first rule, Down again the second.
    h.key_press(Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected(), ["caps"]);
    h.key_press(Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected(), ["pause"]);
    // Shift+Down extends, Ctrl+A selects all shown, Space toggles the focused one.
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    h.run();
    assert_eq!(h.state().selected(), ["pause", "thanks"]);
    h.key_press_modifiers(Modifiers::CTRL, Key::A);
    h.run();
    assert_eq!(h.state().selected().len(), 3);
    h.key_press(Key::Space);
    h.run();
    assert_eq!(h.state().selected().len(), 2);
    h.key_press(Key::ArrowUp);
    h.run();
    // Enter edits, Esc closes an unchanged form.
    h.key_press(Key::Enter);
    h.run();
    assert!(h.state().form().is_some());
    h.key_press(Key::Escape);
    h.run();
    assert!(h.state().form().is_none());
    // Delete asks; Esc cancels the dialog.
    h.key_press(Key::Delete);
    h.run();
    assert!(h.state().dialog_open());
    h.key_press(Key::Escape);
    h.run();
    assert!(!h.state().dialog_open());
    assert_eq!(ids(&sb.rules()).len(), 3);
    // Ctrl+D duplicates, Ctrl+Z undoes it.
    h.key_press_modifiers(Modifiers::CTRL, Key::D);
    h.run();
    assert_eq!(ids(&sb.rules()).len(), 4);
    h.key_press_modifiers(Modifiers::CTRL, Key::Z);
    h.run();
    assert_eq!(ids(&sb.rules()).len(), 3);
    // F5 tests the selected rule in simulate mode.
    h.key_press(Key::ArrowDown);
    h.run();
    h.key_press(Key::F5);
    h.run();
    assert!(h.state().simulation().is_some());
    // F6 cycles the regions; F1 lists the shortcuts.
    h.key_press(Key::F6);
    h.run();
    h.key_press(Key::F1);
    h.run();
    assert!(
        h.query_by_role_and_label(Role::Label, "Keyboard shortcuts")
            .is_some()
    );
    assert!(h.query_by_label("Ctrl+N").is_some());
    h.key_press(Key::Escape);
    h.run();
    // Ctrl+N: a new rule with the capture box recording; Tab leaves it, nothing is trapped.
    h.key_press_modifiers(Modifiers::CTRL, Key::N);
    h.run();
    h.key_press_modifiers(
        Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT,
        Key::F16,
    );
    h.run();
    assert_eq!(
        h.state().form().unwrap().fields.trigger.keys,
        "Ctrl+Alt+Shift+F16"
    );
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::F18);
    h.run();
    assert_eq!(
        h.state().form().unwrap().fields.trigger.keys,
        "Ctrl+Alt+Shift+F16",
        "after Tab the capture box records nothing"
    );
}

/// Unsaved drafts are never lost silently: leaving asks first (Keep editing focused); closing the
/// window with changes asks too.
#[test]
fn draft_safety_and_undo() {
    let sb = Sandbox::new("draft");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    click(
        &mut h,
        "CapsLock  →  CapsLock acts as Left Ctrl; a quick tap is Escape",
    );
    click(&mut h, "Edit");
    h.get_by_label("Name").set_text_value_accesskit("Changed");
    h.run();
    click(
        &mut h,
        "Ctrl+Alt+Shift+F13  →  Pauses all rules, or resumes them when paused",
    );
    assert!(h.query_by_label("Discard your changes?").is_some());
    assert!(
        h.get_by_role_and_label(Role::Button, "Keep editing")
            .is_focused()
    );
    click(&mut h, "Keep editing");
    assert_eq!(h.state().form().unwrap().fields.title, "Changed");
    assert!(
        !h.state_mut().request_close(),
        "closing with changes asks first"
    );
    h.run();
    click(&mut h, "Keep editing");
    click(&mut h, "Cancel");
    click(&mut h, "Discard changes");
    assert!(h.state().form().is_none());
    assert_eq!(
        rule(&sb.rules(), "caps")["title"],
        "CapsLock acts as Ctrl",
        "nothing written"
    );
    assert!(h.state_mut().request_close());
}

/// A change made headlessly (the CLI's code path: the same store transaction) appears in the
/// open editor; the app is asked to reload; a form open on that rule refuses to overwrite it.
#[test]
fn a_cli_change_appears_and_is_never_overwritten() {
    let sb = Sandbox::new("cli");
    sb.write_rules(&three_rules());
    let fake = sb.fake();
    let mut opts = Options::for_test(sb.paths.clone(), fake.clone());
    opts.watch = true;
    let mut h = harness(EditorApp::new(opts));
    settle(&mut h);
    click(
        &mut h,
        "CapsLock  →  CapsLock acts as Left Ctrl; a quick tap is Escape",
    );
    click(&mut h, "Edit");
    h.get_by_label("Name").set_text_value_accesskit("Mine");
    h.run();
    let reloads = fake.with(|s| s.calls.iter().filter(|c| *c == "rules.reload").count());
    // "ddalgak rule edit caps --title …" and "rule disable pause" from another process.
    let cli = Repo::new(sb.paths.clone());
    cli.commit("rule.edit", |d| {
        ddalgak::edit::edit(d, "caps", &json!({ "title": "Renamed by the CLI" }), &[])
    })
    .unwrap();
    cli.commit("rule.disable", |d| {
        ddalgak::edit::set_enabled(d, &["pause".into()], false, &[])
    })
    .unwrap();
    // The folder watcher notices (real ReadDirectoryChanges notifications, no polling).
    let until = Instant::now() + Duration::from_secs(10);
    while h.state().revision() != 3 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        h.run();
    }
    assert_eq!(h.state().revision(), 3, "the CLI's change appears");
    settle(&mut h);
    assert!(fake.with(|s| s.calls.iter().filter(|c| *c == "rules.reload").count()) > reloads);
    assert_eq!(h.state().save_state(), &SaveState::Active { revision: 3 });
    assert!(
        h.state()
            .rows()
            .iter()
            .any(|r| r.title == "Renamed by the CLI")
    );
    assert!(h.state().external_change());
    assert!(
        h.query_by_label("The rules changed outside the editor.")
            .is_some()
    );
    click(&mut h, "4 Try");
    click(&mut h, "Save");
    assert!(
        matches!(h.state().banner(), Some(ddalgak_editor::app::Banner::Error { code, .. }) if code == "rule_conflict"),
        "{:?}",
        h.state().banner()
    );
    assert_eq!(rule(&sb.rules(), "caps")["title"], "Renamed by the CLI");
}

/// Korean labels (DDALGAK-FR-053) and the language switch.
#[test]
fn korean_labels() {
    let sb = Sandbox::new("ko");
    sb.write_rules(&three_rules());
    let mut opts = Options::for_test(sb.paths.clone(), sb.fake());
    opts.lang = Lang::Ko;
    let mut h = harness(EditorApp::new(opts));
    settle(&mut h);
    for label in ["규칙", "모드", "+ 새 규칙", "가져오기…", "English"] {
        assert!(
            h.query_by_role_and_label(Role::Button, label).is_some(),
            "{label}"
        );
    }
    assert!(h.query_by_label("● 실행 중").is_some());
    click(&mut h, "+ 새 규칙");
    assert!(h.query_by_label("이 키를 누르면").is_some());
    h.key_press(Key::Escape);
    h.run();
    click(&mut h, "2 동작");
    for tile in [
        "다른 키로 바꾸기",
        "글자 입력",
        "프로그램 열기",
        "웹사이트 열기",
        "마우스",
        "모드 전환",
        "딸깍 일시 정지",
    ] {
        assert!(h.query_by_label(tile).is_some(), "{tile}");
    }
    click(&mut h, "취소");
    click(&mut h, "English");
    assert!(h.query_by_label("+ New rule").is_some());
}

/// Many rules and modes: 1000 rules (a profile's limit) over 32 modes open, filter and scroll
/// with only the visible rows drawn.
#[test]
fn many_rules_and_modes_stay_usable() {
    let sb = Sandbox::new("many");
    let modes: Vec<Value> = (0..32)
        .map(|i| json!({ "id": format!("m{i}"), "title": format!("Mode {i}") }))
        .collect();
    let rules: Vec<Value> = (0..1000)
        .map(|i| {
            json!({ "id": format!("r{i}"), "title": format!("Rule {i}"), "enabled": false,
            "modes": [format!("m{}", i % 32)],
            "trigger": { "keys": format!("Ctrl+Alt+Shift+F{}", 13 + i % 8) },
            "steps": [ { "type": "engine", "op": "pause" } ] })
        })
        .collect();
    sb.write_rules(
        &json!({ "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "p", "profiles": [ { "id": "p", "title": "P",
        "default_mode": "m0", "modes": modes, "rules": rules } ] }),
    );
    let t = Instant::now();
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    let opened = t.elapsed();
    assert_eq!(h.state().rows().len(), 1000);
    let t = Instant::now();
    h.state_mut().filter_mut().query = "rule 99".into();
    h.run();
    let frame = t.elapsed();
    // Every word must match: "rule" and "99" (Rule 99, 199, …, 899 and 990…999).
    assert_eq!(h.state().visible_ids().len(), 19);
    // Debug-build budgets; the release build is several times faster.
    assert!(opened < Duration::from_secs(30), "opening took {opened:?}");
    assert!(
        frame < Duration::from_secs(5),
        "a search frame took {frame:?}"
    );
    // Bulk over the shown rules is one change.
    h.key_press_modifiers(Modifiers::CTRL, Key::A);
    h.run();
    assert_eq!(h.state().selected().len(), 19);
}

/// Export writes a portable file without consent; import adds the rules turned off, names every
/// launch target and runs nothing (DDALGAK-SEC-013).
#[test]
fn export_and_import_never_open_or_run_anything() {
    let sb = Sandbox::new("io");
    sb.write_rules(&doc(json!([
        { "id": "site", "title": "Open example", "enabled": true,
          "trigger": { "keys": "Ctrl+Alt+Shift+F17" }, "steps": [ { "type": "open_url", "url": "https://example.com/" } ],
          "consent": { "launch-programs": { "granted_at": GRANT, "via": "gui",
              "fingerprint": ddalgak::validate::launch_fingerprint(&json!({ "steps": [ { "type": "open_url", "url": "https://example.com/" } ] })) } } }
    ])));
    let fake = sb.fake();
    let mut h = harness(app(&sb, fake.clone()));
    settle(&mut h);
    click(&mut h, "Export…");
    let file = sb.root.join("shared.json");
    h.get_by_label("File")
        .set_text_value_accesskit(&file.display().to_string());
    h.run();
    click_last_button(&mut h, "Export…");
    let exported: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(!exported.to_string().contains("consent"));
    assert_eq!(exported["profiles"][0]["rules"][0]["enabled"], false);
    click(&mut h, "Close");
    // Import into another, empty store.
    let other = Sandbox::new("io-target");
    let mut h2 = harness(app(&other, other.fake()));
    settle(&mut h2);
    click(&mut h2, "Import…");
    h2.get_by_label("File")
        .set_text_value_accesskit(&file.display().to_string());
    h2.run();
    click(&mut h2, "Check the file");
    assert!(
        h2.query_by_label_contains("https://example.com/").is_some(),
        "the target is named"
    );
    click(&mut h2, "Add them turned off");
    let d = other.rules();
    let r = &d["profiles"][0]["rules"][0];
    assert_eq!(r["enabled"], false);
    assert!(r.get("consent").is_none());
    settle(&mut h2);
    assert!(
        h2.query_all_by_label_contains("? needs review")
            .next()
            .is_some()
    );
    // Enabling it asks for consent naming the exact website.
    h2.get_all_by_label_contains("Opens example.com")
        .next()
        .unwrap()
        .click();
    h2.run();
    click(&mut h2, "Enable");
    assert!(
        h2.query_by_label("Let this rule open this program or website?")
            .is_some()
    );
    assert!(h2.query_by_label("https://example.com/").is_some());
    click_last_button(&mut h2, "Cancel");
    assert_eq!(other.rules()["profiles"][0]["rules"][0]["enabled"], false);
    // No call opened anything: the fake app only saw status and reloads.
    for f in [&fake, &other.fake()] {
        assert!(f.with(|s| s.calls.iter().all(|c| c == "status" || c == "rules.reload")));
    }
}

/// "Test" (F5, the detail's button) runs the simulate mode: a launch rule names what it would
/// open and opens nothing; a mouse rule moves only the simulated pointer.
#[test]
fn test_start_uses_the_simulation() {
    let sb = Sandbox::new("test");
    sb.write_rules(&doc(json!([
        { "id": "np", "title": "Open Notepad", "enabled": false,
          "trigger": { "keys": "Ctrl+Alt+Shift+F18" }, "steps": [ { "type": "open_program", "path": "C:\\Windows\\System32\\notepad.exe" } ] },
        { "id": "mv", "title": "Pointer right", "enabled": false,
          "trigger": { "keys": "Ctrl+Alt+Shift+F19" }, "steps": [ { "type": "mouse_move", "mode": "relative", "dx": 100, "dy": 0 } ] }
    ])));
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    h.get_by_label_contains("Opens notepad.exe").click();
    h.run();
    click(&mut h, "Test");
    let sim = h.state().simulation().unwrap().clone();
    assert!(
        sim.lines.iter().any(|l| l.contains("nothing was opened")),
        "{sim:?}"
    );
    h.get_by_label_contains("Ctrl+Alt+Shift+F19").click();
    h.run();
    h.key_press(Key::F5);
    h.run();
    let sim = h.state().simulation().unwrap().clone();
    let (from, to) = sim.pointer.unwrap();
    assert_eq!(to.x - from.x, 100, "{sim:?}");
    assert!(
        sim.lines
            .iter()
            .any(|l| l.contains("the real pointer did not move"))
    );
}

/// The Modes page: enable and disable a mode, add one; the default mode cannot be disabled.
#[test]
fn modes_page() {
    let sb = Sandbox::new("modes");
    sb.write_rules(&three_rules());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    click(&mut h, "Modes");
    assert!(h.query_by_label("Navigation").is_some());
    click(&mut h, "Disable");
    let d = sb.rules();
    assert_eq!(d["profiles"][0]["modes"][1]["enabled"], false);
    click(&mut h, "Enable");
    h.get_by_label("New mode name")
        .set_text_value_accesskit("Symbols");
    h.run();
    click(&mut h, "+ New mode");
    let d = sb.rules();
    assert!(
        d["profiles"][0]["modes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["title"] == "Symbols")
    );
}

/// Step 75 (J6, `DDALGAK-FR-030…032`, `DDALGAK-SEC-013`): two complete profiles for the browser.
fn two_profiles() -> Value {
    let site = json!({ "steps": [ { "type": "open_url", "url": "https://example.com/" } ] });
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 1,
        "active_profile": "work",
        "profiles": [
            { "id": "work", "title": "Work", "description": "Office keyboard", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
              "rules": [
                { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true,
                  "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl" } ],
                  "consent": { "input-injection": { "granted_at": GRANT, "via": "gui" } } },
                { "id": "site", "title": "Open example", "enabled": true,
                  "trigger": { "keys": "Ctrl+Alt+Shift+F17" }, "steps": [ { "type": "open_url", "url": "https://example.com/" } ],
                  "consent": { "launch-programs": { "granted_at": GRANT, "via": "gui",
                      "fingerprint": ddalgak::validate::launch_fingerprint(&site) } } }
              ] },
            { "id": "gaming", "title": "Gaming", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" } ],
              "rules": [
                { "id": "g-caps", "title": "CapsLock is blocked", "enabled": true,
                  "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "block" } ] }
              ] }
        ]
    })
}

fn profile_ids(d: &Value) -> Vec<String> {
    d["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect()
}

/// J6: the profile browser (search, sort), a switch only after its side-by-side diff (Cancel
/// first), duplicate, rename, delete with undo, history with a restore, and an export imported
/// into another store through the validation preview: never active, rules off, no consent.
#[test]
fn j6_profiles_browse_switch_with_a_diff_history_export_and_import() {
    let sb = Sandbox::new("profiles");
    sb.write_rules(&two_profiles());
    let fake = sb.fake();
    let mut h = harness(app(&sb, fake.clone()));
    settle(&mut h);
    click(&mut h, "Profiles");
    assert_eq!(h.state().page(), ddalgak_editor::app::Page::Profiles);
    assert_eq!(h.state().profile_rows().len(), 2);
    assert!(
        h.query_by_label("● Active").is_some(),
        "the active profile in words"
    );
    // Search and sort.
    h.get_by_label("Search profiles")
        .set_text_value_accesskit("gam");
    h.run();
    let shown: Vec<String> = h.state().profile_rows().into_iter().map(|r| r.id).collect();
    assert_eq!(shown, ["gaming"]);
    h.get_by_label("Search profiles")
        .set_text_value_accesskit("");
    h.run();
    // Switch: the diff first, Cancel changes nothing.
    click(&mut h, "Switch…");
    assert!(h.query_by_label("Now: Work").is_some());
    assert!(h.query_by_label("After: Gaming").is_some());
    assert!(
        h.query_all_by_label_contains("CapsLock is blocked")
            .next()
            .is_some(),
        "the right side names what the key does after the switch"
    );
    assert!(
        h.query_all_by_label_contains("example.com")
            .next()
            .is_none(),
        "the diff shows no launch target"
    );
    click_last_button(&mut h, "Cancel");
    assert_eq!(sb.rules()["active_profile"], "work");
    click(&mut h, "Switch…");
    click(&mut h, "Switch to this profile");
    assert_eq!(sb.rules()["active_profile"], "gaming");
    settle(&mut h);
    assert!(
        h.query_all_by_label_contains("Switched; the running app confirmed it")
            .next()
            .is_some(),
        "the switch is confirmed only after the app's reload"
    );
    assert_eq!(h.state().switching_to(), None);
    // Duplicate (rules get new IDs), rename, delete with undo.
    h.get_all_by_role_and_label(Role::Button, "Duplicate")
        .next()
        .unwrap()
        .click();
    h.run();
    let d = sb.rules();
    assert_eq!(profile_ids(&d), ["work", "gaming", "work-2"]);
    assert_eq!(d["profiles"][2]["rules"][0]["id"], "caps-2");
    h.get_all_by_role_and_label(Role::Button, "Rename…")
        .last()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("Profile name")
        .set_text_value_accesskit("Work at home");
    h.run();
    click_last_button(&mut h, "Save");
    assert_eq!(sb.rules()["profiles"][2]["title"], "Work at home");
    h.get_all_by_role_and_label(Role::Button, "Delete profile")
        .last()
        .unwrap()
        .click();
    h.run();
    click_last_button(&mut h, "Delete profile");
    assert_eq!(profile_ids(&sb.rules()), ["work", "gaming"]);
    click(&mut h, "Undo");
    assert_eq!(profile_ids(&sb.rules()), ["work", "gaming", "work-2"]);
    settle(&mut h);
    // History of `work`: rename it, then restore the earlier version side by side.
    h.get_all_by_role_and_label(Role::Button, "Rename…")
        .next()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("Profile name")
        .set_text_value_accesskit("Office");
    h.run();
    click_last_button(&mut h, "Save");
    assert_eq!(sb.rules()["profiles"][0]["title"], "Office");
    h.get_all_by_role_and_label(Role::Button, "History…")
        .next()
        .unwrap()
        .click();
    h.run();
    let older = h
        .query_all_by_label_contains("profile.apply")
        .next()
        .expect("a version from before the rename");
    older.click();
    h.run();
    assert!(
        h.query_by_label("Now").is_some(),
        "the side-by-side comparison"
    );
    click(&mut h, "Restore this version");
    assert_eq!(
        sb.rules()["profiles"][0]["title"],
        "Work",
        "restored as a new change"
    );
    assert_eq!(
        sb.rules()["active_profile"],
        "gaming",
        "a restore never switches"
    );
    settle(&mut h);
    // A save point from the history.
    h.get_all_by_role_and_label(Role::Button, "History…")
        .next()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("Label for a saved version")
        .set_text_value_accesskit("before games");
    h.run();
    click(&mut h, "Save this version");
    assert!(
        h.query_all_by_label_contains("“before games”")
            .next()
            .is_some()
    );
    click_last_button(&mut h, "Close");
    // Export `work` and import it into another, empty store.
    h.get_all_by_role_and_label(Role::Button, "Export…")
        .next()
        .unwrap()
        .click();
    h.run();
    let file = sb.root.join("work.json");
    h.get_by_label("File")
        .set_text_value_accesskit(&file.display().to_string());
    h.run();
    click_last_button(&mut h, "Export…");
    let exported: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(!exported.to_string().contains("consent"));
    assert_eq!(exported["profiles"][0]["rules"][0]["enabled"], false);
    click_last_button(&mut h, "Close");
    let other = Sandbox::new("profiles-target");
    let other_fake = other.fake();
    let mut h2 = harness(app(&other, other_fake.clone()));
    settle(&mut h2);
    click(&mut h2, "Profiles");
    click(&mut h2, "Import profile…");
    h2.get_by_label("File")
        .set_text_value_accesskit(&file.display().to_string());
    h2.run();
    click(&mut h2, "Check the file");
    assert!(
        h2.query_all_by_label_contains("https://example.com/")
            .next()
            .is_some(),
        "the preview names the exact target"
    );
    assert!(
        h2.query_all_by_label_contains("needs your consent")
            .next()
            .is_some()
    );
    click(&mut h2, "Add them, not active");
    let d = other.rules();
    assert_eq!(profile_ids(&d), ["default", "work"]);
    assert_eq!(
        d["active_profile"], "default",
        "an import is never switched to"
    );
    for r in d["profiles"][1]["rules"].as_array().unwrap() {
        assert_eq!(r["enabled"], false);
        assert!(r.get("consent").is_none());
    }
    // A corrupt file is refused in the preview with the reason.
    let bad = other.root.join("bad.json");
    std::fs::write(&bad, b"{ \"schema\": \"ddalgak.rules\", ").unwrap();
    click(&mut h2, "Import profile…");
    h2.get_by_label("File")
        .set_text_value_accesskit(&bad.display().to_string());
    h2.run();
    click(&mut h2, "Check the file");
    assert!(
        h2.query_all_by_label_contains("cannot be imported")
            .next()
            .is_some()
    );
    assert!(h2.query_by_label("Add them, not active").is_none());
    // Nothing was opened or run: the apps only saw status requests and reloads.
    for f in [&fake, &other_fake] {
        assert!(f.with(|s| s.calls.iter().all(|c| c == "status" || c == "rules.reload")));
    }
}

/// The active profile cannot be deleted from the window either, and the Korean labels exist.
#[test]
fn the_active_profile_is_never_deleted_and_the_page_speaks_korean() {
    let sb = Sandbox::new("profiles-ko");
    sb.write_rules(&two_profiles());
    let mut h = harness(app(&sb, sb.fake()));
    settle(&mut h);
    click(&mut h, "Profiles");
    let first = h
        .get_all_by_role_and_label(Role::Button, "Delete profile")
        .next()
        .unwrap();
    assert!(
        first.accesskit_node().is_disabled(),
        "the active profile's Delete is disabled"
    );
    click(&mut h, "한국어");
    assert!(h.query_by_label("● 사용 중").is_some());
    assert!(h.query_by_label("프로필 가져오기…").is_some());
}
