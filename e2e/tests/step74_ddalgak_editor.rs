//! E2E — Step 74: the Ddalgak rule editor (docs/DDALGAK_GUI_JOURNEYS.md J1–J4 and J7 and its
//! §7 implementation notes, ADR 0049). Requirements: `DDALGAK-FR-050`, `DDALGAK-FR-051`,
//! `DDALGAK-FR-052`, `DDALGAK-FR-053`, `DDALGAK-FR-060`, `DDALGAK-NFR-007`, `DDALGAK-SEC-013`.
//!
//! Scenario A (release `ddalgak-editor`): `--help` and `--version` answer without a window; an
//! unknown option is a usage error (exit 2).
//!
//! Scenario B (debug test host of `ddalgak-gui`, release `ddalgak`, the editor's own code driven
//! headlessly through its accessibility tree with egui_kittest, isolated `%APPDATA%`): the editor
//! talks to the real test host over the identity-checked pipe. A user creates a rule with the
//! form (keys typed, the "Pause Ddalgak" tile, Try, Save and enable); the status line says
//! "saved and active" only after the running app's reload answer, and `ddalgak status` agrees
//! (the active revision is the saved one, the rule is among the active rules). The agent turns the
//! rule off with `ddalgak rule disable`: the editor's folder watcher sees it and the row says
//! "off". The engine buttons of the editor turn the engine on and off; with the engine off the
//! status says "saved and loaded; the engine is off". An import adds an `open_url` rule turned
//! off and without consent; nothing is opened (the test host records every URL it would open in
//! its launch folder, which stays empty).
//!
//! Machine safety: no window opens (the editor runs headlessly in the test process), no key is
//! sent, the test host never consumes a key, only the test's own host is started and ended.
#![cfg(windows)]

use ddalgak::paths::Paths;
use ddalgak_editor::sync::{PipeEngine, SaveState};
use ddalgak_editor::{EditorApp, Options};
use eframe::egui::{self, Key, accesskit::Role};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use kim_e2e::{TempDir, bin, bin_debug, require_e2e};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Env<'a> {
    sb: &'a TempDir,
    endpoint: String,
}

impl Env<'_> {
    fn cmd(&self, exe: &Path) -> Command {
        let mut c = Command::new(exe);
        let p = self.sb.path();
        c.env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env_remove("KIM_NO_HOTKEYS")
            .creation_flags(CREATE_NO_WINDOW);
        c
    }

    fn cli(&self, args: &[&str]) -> Value {
        let out = self
            .cmd(&bin("ddalgak"))
            .args(args)
            .args(["--endpoint", &self.endpoint, "--json"])
            .output()
            .unwrap();
        let d: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        assert_eq!(d["ok"], true, "{args:?}: {d:#}");
        d["data"].clone()
    }

    fn paths(&self) -> Paths {
        Paths::under(self.sb.path())
    }

    fn rules(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.paths().rules()).unwrap()).unwrap()
    }
}

struct Host(Child);

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn harness(app: EditorApp) -> Harness<'static, EditorApp> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .build_ui_state(|ui, app: &mut EditorApp| app.ui(ui), app)
}

fn settle(h: &mut Harness<'static, EditorApp>) {
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(15)));
    h.run();
}

fn click(h: &mut Harness<'static, EditorApp>, label: &str) {
    h.get_by_label(label).click();
    h.run();
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_editor_binary_answers_help_and_version_without_a_window() {
    require_e2e();
    let exe = bin("ddalgak-editor");
    let run = |args: &[&str]| {
        Command::new(&exe)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let v = run(&["--version"]);
    assert!(v.status.success());
    assert_eq!(
        String::from_utf8_lossy(&v.stdout).trim(),
        format!("ddalgak-editor {}", ddalgak::VERSION)
    );
    let h = run(&["--help"]);
    assert!(h.status.success());
    assert!(String::from_utf8_lossy(&h.stdout).contains("never starts Ddalgak"));
    let bad = run(&["--bogus"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("(usage)"));
    let bad = run(&["--lang", "fr"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_editor_saves_through_the_store_and_shows_active_only_after_the_engine_reload() {
    require_e2e();
    let sb = TempDir::new("step74-editor");
    let env = Env {
        sb: &sb,
        endpoint: format!("kim-e2e-step74-{}", std::process::id()),
    };
    let launches = sb.path().join("launches");
    std::fs::create_dir_all(&launches).unwrap();
    let mut host = env.cmd(&bin_debug("ddalgak-gui"));
    host.args([
        "--test-host",
        "--test-max-seconds",
        "180",
        "--endpoint",
        &env.endpoint,
        "--test-launch-root",
    ])
    .arg(&launches)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let host = Host(host.spawn().unwrap());
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let out = env
            .cmd(&bin("ddalgak"))
            .args(["status", "--endpoint", &env.endpoint, "--json"])
            .output()
            .unwrap();
        let d: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        if d["data"]["app"]["running"] == true {
            break;
        }
        assert!(Instant::now() < until, "the test host never answered");
        std::thread::sleep(Duration::from_millis(100));
    }

    // The editor, headless, against the real app over the real pipe (identity checked).
    std::fs::create_dir_all(env.paths().config).unwrap();
    let mut opts = Options::for_test(
        env.paths(),
        Arc::new(PipeEngine {
            endpoint: env.endpoint.clone(),
        }),
    );
    opts.watch = true;
    let mut h = harness(EditorApp::new(opts));
    settle(&mut h);
    assert!(h.query_by_label("○ Off").is_some(), "the engine starts off");

    // J2: a rule from the form (keys typed, the "Pause Ddalgak" tile), saved and enabled.
    click(&mut h, "+ New rule");
    h.key_press(Key::Escape);
    h.run();
    h.get_by_label("Type instead: When I press")
        .set_text_value_accesskit("Ctrl+Alt+Shift+F17");
    h.run();
    click(&mut h, "Next ▸");
    click(&mut h, "Pause Ddalgak");
    click(&mut h, "4 Try");
    click(&mut h, "▶ Simulate");
    click(&mut h, "Save and enable");
    let saved = env.rules();
    let rev = saved["revision"].as_u64().unwrap();
    let id = saved["profiles"][0]["rules"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    settle(&mut h);
    // The engine is off: saved and loaded, not active.
    assert_eq!(
        h.state().save_state(),
        &SaveState::Loaded {
            revision: rev,
            engine: "off".into()
        }
    );
    // The editor's engine button turns it on; then the rule is active, and the CLI agrees.
    click(&mut h, "Turn on");
    settle(&mut h);
    assert_eq!(h.state().save_state(), &SaveState::Active { revision: rev });
    assert!(h.query_by_label_contains("saved and active").is_some());
    let st = env.cli(&["status"]);
    assert_eq!(st["engine"]["state"], "running");
    assert_eq!(st["rules"]["active_set"]["revision"], rev);
    assert!(
        st["rules"]["active_rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == id.as_str())
    );

    // J7 headless: the agent disables it with the CLI; the editor sees it (folder watcher).
    env.cli(&["rule", "disable", &id]);
    let until = Instant::now() + Duration::from_secs(10);
    while h.state().revision() == rev && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
        h.run();
    }
    settle(&mut h);
    assert_eq!(h.state().revision(), rev + 1, "the CLI's change appears");
    assert!(h.state().rows().iter().all(|r| !r.enabled));
    assert_eq!(
        h.state().save_state(),
        &SaveState::Active { revision: rev + 1 }
    );

    // An import adds an open_url rule turned off and without consent; nothing opens.
    let file = sb.path().join("shared.json");
    std::fs::write(
        &file,
        json!({ "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "s", "exported": { "exported_at": "2026-11-04T00:00:00Z", "by_version": "0.0.1" },
            "profiles": [ { "id": "s", "title": "Shared", "default_mode": "base",
                "modes": [ { "id": "base", "title": "Base" } ],
                "rules": [ { "id": "site", "title": "Open example", "enabled": true,
                    "trigger": { "keys": "Ctrl+Alt+Shift+F18" },
                    "steps": [ { "type": "open_url", "url": "https://example.com/" } ] } ] } ] })
        .to_string(),
    )
    .unwrap();
    click(&mut h, "Import…");
    h.get_by_label("File")
        .set_text_value_accesskit(&file.display().to_string());
    h.run();
    click(&mut h, "Check the file");
    assert!(
        h.query_by_label_contains("https://example.com/").is_some(),
        "the target is named before anything is added"
    );
    click(&mut h, "Add them turned off");
    settle(&mut h);
    let d = env.rules();
    let site = d["profiles"][0]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "site")
        .unwrap();
    assert_eq!(site["enabled"], false);
    assert!(site.get("consent").is_none());
    let st = env.cli(&["status"]);
    assert!(
        st["rules"]["active_rules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r != "site"),
        "an imported rule never runs"
    );
    // Turn the engine off again from the editor.
    h.get_by_role_and_label(Role::Button, "Turn off").click();
    h.run();
    settle(&mut h);
    assert_eq!(env.cli(&["status"])["engine"]["state"], "off");
    assert_eq!(
        std::fs::read_dir(&launches).unwrap().count(),
        0,
        "nothing was opened or started"
    );
    drop(host);
}
