//! E2E — Step 73: Ddalgak enable, disable, pause and emergency recovery
//! (docs/DDALGAK_ENGINE.md §16, docs/DDALGAK_SAFETY.md §4, §7, §10, ADR 0046).
//! Requirements: `DDALGAK-FR-002`, `DDALGAK-FR-003`, `DDALGAK-FR-004`, `DDALGAK-FR-016`,
//! `DDALGAK-NFR-001`, `DDALGAK-SEC-003`, `DDALGAK-SEC-005`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app): an agent turns a rule, a mode and a
//! whole profile off and on again headlessly. `status` shows the disabled profile and the codes
//! `mode_disabled` and `profile_disabled`; a rule that switches to the disabled mode is inactive.
//! It is refused, with nothing written: the default mode (`mode_invalid`), two scopes at once and
//! consent without a rule (`usage`), an unknown mode or profile. `--dry-run` writes nothing.
//! `enable`, `disable` and `release` without a scope need the app (`engine_not_running`).
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key, a
//! discarding injector): `disable` is remembered across a restart even with `engine_on_start: on`
//! and `enable` clears it; a disabled profile stays disabled through starts and a global enable;
//! a synthetic session lock (a `WM_WTSSESSION_CHANGE` posted to the host's own hidden window)
//! keeps the engine state and shows `session_locked`; the agent kills the host while the engine
//! is on, and the next start is off (`unclean_exit`) and, on an unlocked desktop, releases an
//! obscure key (F20) Windows still considers pressed; a synthetic sign-out stops the engine
//! cleanly and the next start ("reboot") follows `engine_on_start`; `release` answers with
//! counts; two seconds of idle wake no engine thread; with a debug-lowered hook timeout one
//! injected key turns the engine off with `hook_lost`.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; synthetic session
//! messages go only to the test host's own window; the only real input is the obscure keys F13,
//! F17 and F20, always released; nothing is locked, signed out or rebooted; only the test's own
//! host is killed; no window, tray icon or indicator appears.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug, require_e2e};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const VK_F13: u16 = 0x7C;
const VK_F17: u16 = 0x80;
const VK_F20: u16 = 0x83;
const WM_ENDSESSION: u32 = 0x0016;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;

fn schema() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/schemas/ddalgak-cli-v1.schema.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn rules_doc() -> Value {
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "default",
        "settings": { "engine_on_start": "on" },
        "profiles": [ { "id": "default", "title": "E2E", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
            "rules": [
                { "id": "pause-key", "title": "F13 pauses or resumes", "enabled": true,
                  "trigger": { "keys": "F13", "injected": "accept" },
                  "steps": [ { "type": "engine", "op": "toggle_pause" } ] },
                { "id": "nav-only", "title": "F15 pauses in nav", "enabled": true,
                  "trigger": { "keys": "F15" }, "modes": ["nav"],
                  "steps": [ { "type": "engine", "op": "pause" } ] },
                { "id": "to-nav", "title": "F16 switches to nav", "enabled": true,
                  "trigger": { "keys": "F16", "injected": "accept" },
                  "steps": [ { "type": "mode", "op": "set", "mode": "nav" } ] },
                { "id": "f19-remap", "title": "F19 acts as F20", "enabled": true,
                  "trigger": { "keys": "F19" }, "steps": [ { "type": "remap", "to": "F20" } ],
                  "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" } } }
            ] } ]
    })
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
            .env_remove("KIM_NO_INPUT")
            .creation_flags(CREATE_NO_WINDOW);
        c
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut c = self.cmd(&self.cli);
        c.args(args).args(["--endpoint", &self.endpoint, "--json"]);
        let out = c.output().unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        let errors = jsonschema::Validator::new(&schema())
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{doc:#}");
        assert!(out.stderr.is_empty(), "{args:?}: stderr with --json");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, d) = self.json(args);
        assert_eq!(code, 0, "{args:?}: {d:#}");
        d
    }

    fn refused(&self, args: &[&str], code: &str) {
        let before = self.rules_bytes();
        let (exit, d) = self.json(args);
        assert_eq!(d["error"]["code"], code, "{args:?}: {d:#}");
        assert_eq!(d["error"]["exit_code"], exit);
        assert_eq!(self.rules_bytes(), before, "{args:?}: nothing written");
    }

    fn rules_bytes(&self) -> Vec<u8> {
        std::fs::read(self.config().join("rules.json")).unwrap_or_default()
    }

    fn config(&self) -> PathBuf {
        self.sb.path().join("Roaming/mouv/ddalgak")
    }

    fn local(&self) -> PathBuf {
        self.sb.path().join("Local/mouv/ddalgak")
    }

    fn status(&self) -> Value {
        self.ok(&["status"])["data"].clone()
    }

    fn until(&self, what: &str, cond: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let d = self.status();
            if cond(&d) {
                return d;
            }
            assert!(Instant::now() < until, "never {what}: {d:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn start_host(&self, vars: &[(&str, &str)]) -> Host {
        let mut c = self.cmd(&bin_debug("ddalgak-gui"));
        c.args([
            "--test-host",
            "--test-max-seconds",
            "120",
            "--endpoint",
            &self.endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        for (k, v) in vars {
            c.env(k, v);
        }
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
}

struct Host(Child);

impl Host {
    /// Ends the agent's own test host the hard way (a crash, or Windows ending the session).
    fn kill(mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn env<'a>(sb: &'a TempDir, cli: PathBuf, tag: &str) -> Env<'a> {
    Env {
        sb,
        cli,
        endpoint: format!("kim-e2e-step73-{tag}-{}", std::process::id()),
    }
}

/// The test host's own hidden window (by process ID and class; never another program's).
fn host_window(pid: u32) -> Option<isize> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowThreadProcessId,
    };
    struct Find {
        pid: u32,
        found: Option<isize>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> i32 {
        // SAFETY: `lp` is the `Find` below, alive for the enumeration.
        let f = unsafe { &mut *(lp as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: a window handle from the enumeration; a writable u32.
        unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
        if pid != f.pid {
            return 1;
        }
        let mut class = [0u16; 64];
        // SAFETY: a valid window; 64 UTF-16 units of room.
        let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
        if String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)])
            == "mouv.ddalgak.host"
        {
            f.found = Some(hwnd as isize);
            return 0;
        }
        1
    }
    let mut f = Find { pid, found: None };
    // SAFETY: the callback only reads window information and writes to `f`.
    unsafe { EnumWindows(Some(each), (&raw mut f) as LPARAM) };
    f.found
}

fn post(hwnd: isize, msg: u32, wparam: usize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
    // SAFETY: the test host's own window; no pointers in the message.
    assert!(unsafe { PostMessageW(hwnd as _, msg, wparam, 0) } != 0);
}

fn send(vk: u16, up: bool) -> bool {
    ddalgak::win::inject::send_as_other_program(&[ddalgak::inject::OutKey { vk, up }]) == 1
}

fn key_down(vk: u16) -> bool {
    ddalgak::win::recovery::key_down(vk)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_switches_rules_modes_and_profiles_off_and_on_headlessly() {
    require_e2e();
    let sb = TempDir::new("step73-a");
    let e = env(&sb, bin("ddalgak"), "a");
    std::fs::create_dir_all(e.config()).unwrap();
    std::fs::write(e.config().join("rules.json"), rules_doc().to_string()).unwrap();
    let before = e.rules_bytes();
    e.ok(&["disable", "--profile", "default", "--dry-run"]);
    assert_eq!(e.rules_bytes(), before, "a dry run writes nothing");
    let d = e.ok(&["disable", "--rule", "pause-key"]);
    assert_eq!(d["data"]["scope"], "rule");
    assert_eq!(e.status()["rules"]["disabled"], 1);
    e.ok(&["enable", "--rule", "pause-key"]);
    let d = e.ok(&["disable", "--mode", "nav"]);
    assert_eq!(d["data"]["modes"], json!(["nav"]));
    let st = e.status();
    let code = |id: &str| {
        st["rules"]["inactive"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["rule"] == id)
            .map(|i| i["code"].clone())
    };
    assert_eq!(code("nav-only"), Some(json!("mode_disabled")));
    assert_eq!(code("to-nav"), Some(json!("mode_disabled")));
    e.refused(&["disable", "--mode", "base"], "mode_invalid");
    e.refused(
        &["disable", "--rule", "pause-key", "--profile", "default"],
        "usage",
    );
    e.refused(&["disable", "--allow", "input-injection"], "usage");
    e.refused(&["mode", "disable", "nowhere"], "mode_not_found");
    e.refused(&["disable", "--profile", "nowhere"], "profile_not_found");
    e.ok(&["mode", "enable", "nav"]);
    e.ok(&["disable", "--profile", "default"]);
    let st = e.status();
    assert_eq!(st["profile"]["enabled"], false);
    assert_eq!(st["rules"]["active"], 0);
    e.ok(&["enable", "--profile", "default"]);
    assert_eq!(e.status()["rules"]["active"], 4);
    for args in [&["enable"][..], &["disable"], &["release"], &["pause"]] {
        let (code, d) = e.json(args);
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (3, Some("engine_not_running")),
            "{args:?}: the CLI never starts the app"
        );
    }
    let (_, rc) = e.json(&["rule", "check"]);
    assert_eq!(rc["data"]["summary"]["errors"], 0);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_recovers_and_never_turns_itself_back_on() {
    require_e2e();
    if ddalgak::win::is_elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        return;
    }
    let sb = TempDir::new("step73-b");
    let e = env(&sb, bin("ddalgak"), "b");
    std::fs::create_dir_all(e.config()).unwrap();
    std::fs::write(e.config().join("rules.json"), rules_doc().to_string()).unwrap();
    let desktop = ddalgak::win::input_desktop_available();
    // 1. The user's turn-off survives a restart; enable clears it.
    let host = e.start_host(&[]);
    assert_eq!(e.status()["engine"]["state"], "running");
    e.ok(&["disable"]);
    assert!(e.local().join("engine.disabled").exists());
    e.ok(&["quit"]);
    drop(host);
    let host = e.start_host(&[]);
    let st = e.status();
    assert_eq!(
        (
            st["engine"]["state"].as_str(),
            st["start"]["off_reason"].as_str()
        ),
        (Some("off"), Some("disabled"))
    );
    e.ok(&["enable"]);
    // 2. A disabled profile stays disabled through a global enable.
    e.ok(&["disable", "--profile", "default"]);
    let st = e.until("the profile disabled", |d| d["profile"]["enabled"] == false);
    assert_eq!(st["indicator"]["attention"], "profile_disabled");
    e.ok(&["enable", "--profile", "default"]);
    e.until("rules active again", |d| d["rules"]["active"] == 4);
    // 3. A synthetic lock: the state stays, rules wait, nothing resumes by itself on unlock.
    let hwnd = host_window(host.0.id()).expect("the test host's own window");
    post(hwnd, WM_WTSSESSION_CHANGE, 0x7);
    let st = e.until("locked", |d| d["session"]["locked"] == true);
    assert_eq!(st["engine"]["state"], "running");
    post(hwnd, WM_WTSSESSION_CHANGE, 0x8);
    e.until("unlocked", |d| d["session"]["locked"] == false);
    if desktop && send(VK_F13, false) && send(VK_F13, true) {
        e.until("paused by F13", |d| d["engine"]["state"] == "paused");
        e.ok(&["resume"]);
    }
    // 4. A crash: the agent kills its own host while the engine is on.
    host.kill();
    let stuck = desktop && send(VK_F20, false) && key_down(VK_F20);
    let host = e.start_host(&[]);
    let st = e.status();
    if key_down(VK_F20) {
        // Never leave the test's key pressed, whatever the assertions below say.
        send(VK_F20, true);
    }
    assert_eq!(st["engine"]["state"], "off");
    assert_eq!(st["start"]["off_reason"], "unclean_exit");
    assert_eq!(st["indicator"]["shape"], "ring_struck");
    if stuck {
        assert_eq!(st["start"]["recovered"]["released"], 1);
    } else {
        eprintln!("skipped (locked or SendInput refused): the stale-key release");
    }
    // 5. A synthetic sign-out, then the "reboot": a clean stop, so engine_on_start applies.
    e.ok(&["engine", "start"]);
    post(
        host_window(host.0.id()).expect("the test host's own window"),
        WM_ENDSESSION,
        1,
    );
    e.until("stopped by the session end", |d| {
        d["engine"]["state"] == "off"
    });
    host.kill();
    let host = e.start_host(&[]);
    assert_eq!(e.status()["engine"]["state"], "running");
    // 6. Release answers with counts; idle wakes no engine thread.
    let d = e.ok(&["release"]);
    assert_eq!(d["data"]["held_by_ddalgak"], 0);
    std::thread::sleep(Duration::from_millis(300));
    let a = e.ok(&["health"]);
    std::thread::sleep(Duration::from_secs(2));
    let b = e.ok(&["health"]);
    for t in ["hook_thread", "worker"] {
        assert_eq!(
            a["data"]["counters"]["wakeups"][t], b["data"]["counters"]["wakeups"][t],
            "{t}: no timer while idle"
        );
    }
    e.ok(&["quit"]);
    drop(host);
    // 7. A hook that reaches the (debug-lowered) timeout turns the engine off.
    if desktop {
        let host = e.start_host(&[("DDALGAK_DEV_HOOK_TIMEOUT_US", "1")]);
        let mut off = false;
        for _ in 0..5 {
            if !(send(VK_F17, false) && send(VK_F17, true)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
            if e.status()["engine"]["state"] == "off" {
                off = true;
                break;
            }
        }
        if off {
            let st = e.status();
            assert_eq!(st["engine"]["off_reason"], "hook_lost");
            assert_eq!(st["indicator"]["attention"], "hook_lost");
        } else {
            eprintln!("skipped (SendInput refused): the lost-hook path");
        }
        e.ok(&["quit"]);
        drop(host);
    } else {
        eprintln!("skipped (locked): the lost-hook path");
    }
}
