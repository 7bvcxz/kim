//! E2E — Step 66: the Ddalgak engine (docs/DDALGAK_ENGINE.md, ADR 0024). Requirements:
//! `DDALGAK-FR-002`, `DDALGAK-FR-003`, `DDALGAK-FR-004`, `DDALGAK-FR-017`, `DDALGAK-FR-060`,
//! `DDALGAK-SEC-003`, `DDALGAK-SEC-004`, `DDALGAK-SEC-010`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, no app): an agent asks for capabilities and status, tries to
//! turn the engine on and gets `engine_not_running`; no `ddalgak-gui.exe` was started.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key,
//! ends by itself): with engine-control rules on `F13`/`F14` (`injected: accept`) and
//! `engine.toggle` bound to `Ctrl+Alt+Shift+F13`, the agent starts the app, sees the shortcut
//! registered, turns the engine on, pauses and resumes it, checks health, and — when an
//! interactive desktop is available — injects `F13` (paused by the rule) and `F14` (turned off by
//! the rule). Then it quits the app; the run marker is gone. On a locked workstation the
//! injection part reports **skipped (locked)**.
//!
//! Scenario C: after an unclean exit or an emergency off the app starts with the engine off and
//! says why, even with `engine_on_start: on`.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoints, only `F13`/`F14`
//! injected (and released), only `Ctrl+Alt+Shift+F13` registered (never pressed). Every app is
//! quit or killed by a guard.
#![cfg(windows)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schema() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/schemas/ddalgak-cli-v1.schema.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

struct Env<'a> {
    sb: &'a TempDir,
    cli: PathBuf,
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

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut c = self.cmd(&self.cli);
        c.args(args);
        if ["status", "health", "engine", "pause", "resume", "quit"].contains(&args[0]) {
            c.args(["--endpoint", &self.endpoint]);
        }
        c.arg("--json");
        let out = c.output().unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        let errors = jsonschema::Validator::new(&schema())
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{doc:#}");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn config(&self) -> PathBuf {
        self.sb.path().join("Roaming/mouv/ddalgak")
    }

    fn local(&self) -> PathBuf {
        self.sb.path().join("Local/mouv/ddalgak")
    }

    fn start_host(&self) -> Host {
        let mut c = self.cmd(&bin_debug("ddalgak-gui"));
        c.args([
            "--test-host",
            "--test-max-seconds",
            "90",
            "--endpoint",
            &self.endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let mut h = Host(c.spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if self.json(&["status"]).1["data"]["app"]["running"] == true {
                return h;
            }
            assert!(h.0.try_wait().unwrap().is_none(), "the app exited early");
            assert!(Instant::now() < until, "the app never answered");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn quit(&self, mut h: Host) {
        let (_, d) = self.json(&["quit"]);
        assert_eq!(d["data"]["quitting"], true);
        let until = Instant::now() + Duration::from_secs(10);
        while h.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "the app did not quit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

struct Host(Child);

impl Drop for Host {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn gui_processes() -> usize {
    let out = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq ddalgak-gui.exe", "/NH", "/FO", "CSV"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .matches("ddalgak-gui.exe")
        .count()
}

fn rules(on_start: &str) -> Value {
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 1,
        "active_profile": "default", "settings": { "engine_on_start": on_start },
        "profiles": [ { "id": "default", "title": "E2E", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" } ],
            "rules": [
                { "id": "pause-key", "title": "F13 toggles pause", "enabled": true,
                  "trigger": { "keys": "F13", "injected": "accept" },
                  "steps": [ { "type": "engine", "op": "toggle_pause" } ] },
                { "id": "off-key", "title": "F14 turns the engine off", "enabled": true,
                  "trigger": { "keys": "F14", "injected": "accept" },
                  "steps": [ { "type": "engine", "op": "disable" } ] }
            ] } ]
    })
}

fn env<'a>(sb: &'a TempDir, cli: PathBuf, tag: &str) -> Env<'a> {
    Env {
        sb,
        cli,
        endpoint: format!("ddalgak-e2e66-{tag}-{}", std::process::id()),
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_headless_agent_never_starts_the_app() {
    let sb = TempDir::new("e2e66-a");
    let e = env(&sb, bin("ddalgak"), "a");
    let before = gui_processes();
    let (code, caps) = e.json(&["capabilities"]);
    assert_eq!(code, 0);
    assert_eq!(caps["data"]["tool"], "ddalgak");
    let (code, st) = e.json(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(st["data"]["engine"]["off_reason"], "app_not_running");
    let (code, d) = e.json(&["engine", "start"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (3, Some("engine_not_running"))
    );
    assert_eq!(gui_processes(), before, "the CLI started no app");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_drives_the_engine_of_the_running_app() {
    let sb = TempDir::new("e2e66-b");
    let e = env(&sb, bin_debug("ddalgak"), "b");
    std::fs::create_dir_all(e.config()).unwrap();
    std::fs::write(e.config().join("rules.json"), rules("off").to_string()).unwrap();
    std::fs::write(
        e.config().join("hotkeys.json"),
        json!({ "schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
                "overrides": [ { "action": "engine.toggle", "bindings": ["Ctrl+Alt+Shift+F13"] } ] })
        .to_string(),
    )
    .unwrap();
    let host = e.start_host();
    let (_, st) = e.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "off");
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["pause-key", "off-key"])
    );
    let reg = &st["data"]["shortcuts"]["global"][0];
    assert_eq!(reg["action"], "engine.toggle");
    assert!(["registered", "taken"].contains(&reg["registration"].as_str().unwrap()));
    if st["data"]["app"]["elevated"] == true {
        let (code, _) = e.json(&["engine", "start"]);
        assert_eq!(code, 5, "engine_elevated");
        e.quit(host);
        return;
    }
    assert_eq!(
        e.json(&["engine", "start"]).1["data"]["change"]["to"],
        "running"
    );
    assert!(e.local().join("engine.running").exists());
    assert_eq!(e.json(&["pause"]).1["data"]["change"]["to"], "paused");
    assert_eq!(e.json(&["resume"]).1["data"]["change"]["to"], "running");
    let (_, h) = e.json(&["health"]);
    assert_eq!(h["data"]["engine"]["hook"]["thread_alive"], true);
    assert_eq!(h["data"]["counters"]["consumed"], 0);
    let wait = |want: &str| {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let (_, st) = e.json(&["status"]);
            if st["data"]["engine"]["state"] == want {
                return st;
            }
            assert!(Instant::now() < until, "never {want}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let tap = |vk: u16| {
        ddalgak::win::inject::send_as_other_program(&[
            ddalgak::inject::OutKey { vk, up: false },
            ddalgak::inject::OutKey { vk, up: true },
        ]) == 2
    };
    if !ddalgak::win::input_desktop_available() || !tap(0x7C) {
        eprintln!("skipped (locked): no interactive input desktop; injection not exercised");
        assert_eq!(e.json(&["engine", "stop"]).1["data"]["change"]["to"], "off");
    } else {
        let st = wait("paused");
        assert_eq!(
            st["data"]["engine"]["last_change"]["origin"],
            "rule:pause-key"
        );
        if tap(0x7D) {
            wait("off");
        } else {
            eprintln!("skipped (SendInput refused) at F14; stopping through the CLI");
            e.json(&["engine", "stop"]);
        }
    }
    assert!(
        !e.local().join("engine.running").exists(),
        "clean stop removes the marker"
    );
    e.quit(host);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn after_a_crash_or_an_emergency_the_engine_starts_off() {
    let sb = TempDir::new("e2e66-c");
    let e = env(&sb, bin_debug("ddalgak"), "c");
    std::fs::create_dir_all(e.config()).unwrap();
    std::fs::create_dir_all(e.local()).unwrap();
    std::fs::write(e.config().join("rules.json"), rules("on").to_string()).unwrap();
    for (marker, reason) in [
        ("engine.running", "unclean_exit"),
        ("engine.emergency", "emergency"),
    ] {
        std::fs::write(e.local().join(marker), "2026-11-04T00:00:00Z\n").unwrap();
        let host = e.start_host();
        let (_, st) = e.json(&["status"]);
        assert_eq!(st["data"]["engine"]["state"], "off");
        let elevated = st["data"]["app"]["elevated"] == true;
        assert_eq!(
            st["data"]["start"]["off_reason"],
            if elevated { "elevated" } else { reason }
        );
        e.quit(host);
        let _ = std::fs::remove_file(e.local().join("engine.emergency"));
    }
}
