//! E2E — Step 68: Ddalgak remaps and blocks (docs/DDALGAK_RULE_MODEL.md §5.1, §5.2,
//! docs/DDALGAK_ENGINE.md §11, ADR 0031). Requirements: `DDALGAK-FR-010`, `DDALGAK-FR-020`,
//! `DDALGAK-FR-060`, `DDALGAK-FR-071`, `DDALGAK-SEC-004`, `DDALGAK-SEC-005`, `DDALGAK-SEC-009`,
//! `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app, no hook): an agent builds a remap set
//! headlessly with the final rule syntax — CapsLock as Left Ctrl with a tap for Escape, Right Alt
//! as 한/영, Ctrl+Shift+J as Down, mouse button X1 as Alt+Left, F14 also holding F16
//! (`--pass-through`), Insert blocked — each with `--allow input-injection` where it injects.
//! It is refused, with nothing written: a primary-button trigger and a sequence with a mouse
//! button (`hotkey_invalid`), a `Win` chord (`hotkey_reserved`), a remap to its own trigger and a
//! dual role that also passes the original key through (`rule_invalid`), a remap to the Secure
//! Attention Sequence (`rule_invalid`), a second CapsLock rule (`rule_conflict` with offers).
//! `rule list` shows every remap and the block `active`; `rule check` is clean.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key,
//! a worker that never injects, ends by itself): with an obscure remap `F19` → `F20` and a
//! blocked `MouseX2`, the agent turns the engine on and sees both rules active and the mouse
//! hook installed; disabling the mouse rule removes the mouse hook. When an interactive desktop
//! is available it injects `F19` as another program: remaps take physical input only, so the
//! rule is skipped (`skipped_injected_input`) and `F20` never goes down. Then it quits the app.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; only `F19` is
//! injected (down and up), no mouse input is injected, the test host injects nothing and consumes
//! nothing; every app is quit or killed by a guard.
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
const VK_F19: u16 = 0x82;
const VK_F20: u16 = 0x83;

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
        c.args(args).args(["--endpoint", &self.endpoint, "--json"]);
        let out = c.output().unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        let errors = jsonschema::Validator::new(&schema())
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{doc:#}");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn rules(&self) -> PathBuf {
        self.sb.path().join("Roaming/mouv/ddalgak/rules.json")
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

fn add(e: &Env, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["rule", "add"];
    a.extend_from_slice(args);
    e.json(&a)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_builds_remaps_headlessly_and_is_refused_unsafe_ones() {
    let sb = TempDir::new("step68-remaps");
    let e = Env {
        sb: &sb,
        cli: bin("ddalgak"),
        endpoint: format!("ddalgak-e2e-step68-a-{}", std::process::id()),
    };
    let allow = ["--allow", "input-injection"];
    for (id, trigger, rest) in [
        ("caps", "CapsLock", vec!["--to", "LCtrl", "--tap", "Escape"]),
        ("hangul", "RAlt", vec!["--to", "HangulMode"]),
        ("j-down", "Ctrl+Shift+J", vec!["--to", "Down"]),
        ("x1-back", "XButton1", vec!["--to", "Alt+Left"]),
        ("f14-f16", "F14", vec!["--to", "F16", "--pass-through"]),
    ] {
        let mut a = vec!["--id", id, "--trigger", trigger, "--action", "remap"];
        a.extend(rest);
        a.extend(allow);
        let (code, d) = add(&e, &a);
        assert_eq!(code, 0, "{id}: {d:#}");
    }
    let (code, _) = add(
        &e,
        &[
            "--id",
            "no-insert",
            "--trigger",
            "Insert",
            "--action",
            "block",
        ],
    );
    assert_eq!(code, 0, "a block needs no consent");
    assert_eq!(
        e.json(&["rule", "show", "x1-back"]).1["data"]["rule"]["trigger"]["keys"],
        "MouseX1",
        "stored in canonical form"
    );
    // Refusals: nothing is written.
    let before = std::fs::read(e.rules()).unwrap();
    for (args, code, err) in [
        (
            vec!["--trigger", "MouseLeft", "--action", "remap", "--to", "F16"],
            2,
            "hotkey_invalid",
        ),
        (
            vec!["--trigger", "MouseX1 F13", "--action", "block"],
            2,
            "hotkey_invalid",
        ),
        (
            vec!["--trigger", "Win+MouseX2", "--action", "block"],
            2,
            "hotkey_reserved",
        ),
        (
            vec!["--trigger", "F13", "--action", "remap", "--to", "F13"],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F15",
                "--action",
                "remap",
                "--to",
                "LCtrl",
                "--tap",
                "Escape",
                "--pass-through",
            ],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F15",
                "--action",
                "remap",
                "--to",
                "Ctrl+Alt+Delete",
            ],
            2,
            "rule_invalid",
        ),
        (
            vec!["--trigger", "CapsLock", "--action", "block"],
            7,
            "rule_conflict",
        ),
    ] {
        let mut a = args.clone();
        a.extend(allow);
        let (c, d) = add(&e, &a);
        assert_eq!(
            (c, d["error"]["code"].as_str()),
            (code, Some(err)),
            "{args:?}: {d:#}"
        );
        if err == "rule_conflict" {
            assert!(
                !d["error"]["details"].as_array().unwrap().is_empty(),
                "offers: {d:#}"
            );
        }
    }
    assert_eq!(
        std::fs::read(e.rules()).unwrap(),
        before,
        "nothing was written"
    );
    let (_, d) = e.json(&["rule", "list"]);
    let states: Vec<(String, String)> = d["data"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().into(),
                r["state"].as_str().unwrap().into(),
            )
        })
        .collect();
    for (id, state) in &states {
        assert_eq!(state, "active", "{id}: {states:?}");
    }
    assert_eq!(states.len(), 6);
    let (_, d) = e.json(&["rule", "check"]);
    assert_eq!(d["data"]["ok"], true, "{d:#}");
}

#[allow(unsafe_code)]
fn key_down(vk: u16) -> bool {
    // SAFETY: reads the async state of one of this test's own obscure keys.
    (unsafe { windows_sys_get_async(vk) } as u16) & 0x8000 != 0
}

#[allow(unsafe_code)]
unsafe fn windows_sys_get_async(vk: u16) -> i16 {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
    }
    // SAFETY: a documented user32 function without pointers.
    unsafe { GetAsyncKeyState(i32::from(vk)) }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_holds_remaps_and_the_mouse_hook_only_while_needed() {
    let sb = TempDir::new("step68-app");
    let e = Env {
        sb: &sb,
        cli: bin_debug("ddalgak"),
        endpoint: format!("ddalgak-e2e-step68-b-{}", std::process::id()),
    };
    std::fs::create_dir_all(e.rules().parent().unwrap()).unwrap();
    let consent =
        json!({ "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "cli" } });
    std::fs::write(
        e.rules(),
        json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "revision": 1, "active_profile": "default",
            "profiles": [ { "id": "default", "title": "E2E", "default_mode": "base",
                "modes": [ { "id": "base", "title": "Base" } ],
                "rules": [
                    { "id": "f19-f20", "title": "F19 acts as F20", "enabled": true,
                      "trigger": { "keys": "F19" }, "steps": [ { "type": "remap", "to": "F20" } ],
                      "consent": consent },
                    { "id": "no-x2", "title": "Mouse button X2 does nothing", "enabled": true,
                      "trigger": { "keys": "MouseX2" }, "steps": [ { "type": "block" } ] }
                ] } ]
        })
        .to_string(),
    )
    .unwrap();
    let _host = e.start_host();
    let (_, st) = e.json(&["status"]);
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["f19-f20", "no-x2"])
    );
    if st["data"]["app"]["elevated"] == true {
        eprintln!("elevated: the engine stays off (engine_elevated); the rest is not tested");
        return;
    }
    let (_, d) = e.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    assert_eq!(d["data"]["engine"]["hook"]["mouse_installed"], true);
    // Remaps take physical input only: another program's F19 is skipped and F20 never goes down.
    if ddalgak::win::input_desktop_available() {
        let sent = ddalgak::win::inject::send_as_other_program(&[
            ddalgak::inject::OutKey {
                vk: VK_F19,
                up: false,
            },
            ddalgak::inject::OutKey {
                vk: VK_F19,
                up: true,
            },
        ]);
        if sent == 2 {
            std::thread::sleep(Duration::from_millis(300));
            assert!(!key_down(VK_F20), "the test host injects nothing");
            let (_, st) = e.json(&["status"]);
            let skipped = st["data"]["activity"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["rule"] == "f19-f20" && a["outcome"] == "skipped_injected_input");
            assert!(skipped, "{st:#}");
        } else {
            eprintln!("skipped (SendInput refused): locked desktop or an elevated window");
        }
    } else {
        eprintln!("skipped (locked): no interactive input desktop for the injection part");
    }
    let (code, _) = e.json(&["rule", "disable", "no-x2"]);
    assert_eq!(code, 0);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let (_, st) = e.json(&["status"]);
        if st["data"]["engine"]["hook"]["mouse_installed"] == false {
            assert_eq!(st["data"]["engine"]["hook"]["installed"], true);
            break;
        }
        assert!(Instant::now() < until, "the mouse hook stayed: {st:#}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, d) = e.json(&["quit"]);
    assert_eq!(d["data"]["quitting"], true);
}
