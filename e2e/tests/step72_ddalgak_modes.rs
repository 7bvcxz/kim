//! E2E — Step 72: Ddalgak user-defined modes, layers and mode toggles
//! (docs/DDALGAK_RULE_MODEL.md §5.10, §7.1, §7.3, docs/DDALGAK_ENGINE.md §15, ADR 0043).
//! Requirements: `DDALGAK-FR-016`, `DDALGAK-FR-020`, `DDALGAK-FR-060`, `DDALGAK-SEC-005`,
//! `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app): an agent builds a layered profile
//! headlessly — a navigation layer over Base, a symbols layer over it, an exclusive gaming mode, a
//! one-shot mode and one that times out — and the rules that switch, toggle, cycle, hold and
//! return, plus remaps limited to modes. `mode list` shows each mode's policy and chain, `mode
//! show` the rules that use it; rename and copy keep IDs and references. It is refused, with
//! nothing written: a parent cycle and a default mode with a parent (`mode_invalid`), deleting the
//! default mode, a parent of a layer or a mode a rule switches to (`mode_in_use`), a delete without
//! `--yes` (`confirmation_required`), an unknown mode. `--dry-run` writes nothing; `rule check`
//! finds no error; `mode set` without the app is `engine_not_running`.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key, a
//! discarding injector): the engine starts in the default mode; `mode set`, `toggle` and `reset`
//! change the live mode and `status` shows it with its chain; a pause returns to the default mode;
//! deleting the active mode makes the reload fall back to the default mode; a stop and a start
//! never restore a mode. On an unlocked desktop injected obscure keys (F13 held, F14 tapped, sent
//! as another program) set a mode and hold a layer that ends with the key-up; on a locked desktop
//! that part is reported as skipped. The agent quits the app.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; the only real input
//! is the obscure keys F13 and F14, always released; no window, tray icon or indicator appears;
//! every app is quit or killed by a guard.
#![cfg(windows)]

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
const VK_F14: u16 = 0x7D;

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
        let (exit, d) = self.json(args);
        assert_eq!(d["error"]["code"], code, "{args:?}: {d:#}");
        assert_eq!(d["error"]["exit_code"], exit);
    }

    fn rules_bytes(&self) -> Vec<u8> {
        std::fs::read(self.sb.path().join("Roaming/mouv/ddalgak/rules.json")).unwrap_or_default()
    }

    fn mode(&self) -> Value {
        self.ok(&["mode", "status"])["data"]["mode"].clone()
    }

    fn wait_mode(&self, want: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let m = self.mode();
            if m["active"] == want {
                return m;
            }
            assert!(Instant::now() < until, "never in {want}: {m:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn start_host(&self) -> Host {
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

fn env<'a>(sb: &'a TempDir, cli: PathBuf, tag: &str) -> Env<'a> {
    Env {
        sb,
        cli,
        endpoint: format!("kim-e2e-step72-{tag}-{}", std::process::id()),
    }
}

/// The layered profile of both scenarios.
fn build_profile(e: &Env<'_>) {
    let before = e.rules_bytes();
    e.ok(&[
        "mode",
        "add",
        "--title",
        "Navigation",
        "--id",
        "nav",
        "--dry-run",
    ]);
    assert_eq!(e.rules_bytes(), before, "a dry run writes nothing");
    for args in [
        &[
            "mode",
            "add",
            "--title",
            "Navigation",
            "--id",
            "nav",
            "--parent",
            "base",
            "--icon",
            "N",
        ][..],
        &[
            "mode", "add", "--title", "Symbols", "--id", "sym", "--parent", "nav", "--color",
            "#E8C27A",
        ],
        &["mode", "add", "--title", "Gaming", "--id", "game"],
        &[
            "mode",
            "add",
            "--title",
            "Accent",
            "--id",
            "accent",
            "--one-shot",
        ],
        &[
            "mode",
            "add",
            "--title",
            "Number pad",
            "--id",
            "numpad",
            "--timeout-ms",
            "10000",
        ],
    ] {
        e.ok(args);
    }
    let rules: [&[&str]; 8] = [
        &[
            "--id",
            "hold-nav",
            "--trigger",
            "F13",
            "--accept-injected",
            "--action",
            "mode",
            "--op",
            "hold",
            "--target-mode",
            "nav",
        ],
        &[
            "--id",
            "to-game",
            "--trigger",
            "F14",
            "--accept-injected",
            "--action",
            "mode",
            "--op",
            "set",
            "--target-mode",
            "game",
        ],
        &[
            "--id",
            "toggle-sym",
            "--trigger",
            "Ctrl+Alt+Shift+F15",
            "--action",
            "mode",
            "--op",
            "toggle",
            "--target-mode",
            "sym",
        ],
        &[
            "--id",
            "cycle",
            "--trigger",
            "Ctrl+Alt+Shift+F16",
            "--action",
            "mode",
            "--op",
            "cycle",
            "--target-mode",
            "base",
            "--target-mode",
            "game",
        ],
        &[
            "--id",
            "back",
            "--trigger",
            "Ctrl+Alt+Shift+F17",
            "--action",
            "mode",
            "--op",
            "previous",
        ],
        &[
            "--id",
            "home",
            "--trigger",
            "Ctrl+Alt+Shift+F18",
            "--mode",
            "game",
            "--action",
            "mode",
            "--op",
            "default",
        ],
        &[
            "--id",
            "nav-h",
            "--trigger",
            "code:KeyH",
            "--mode",
            "nav",
            "--action",
            "remap",
            "--to",
            "Left",
            "--allow",
            "input-injection",
        ],
        &[
            "--id",
            "sym-h",
            "--trigger",
            "code:KeyH",
            "--mode",
            "sym",
            "--action",
            "remap",
            "--to",
            "F20",
            "--allow",
            "input-injection",
        ],
    ];
    for r in rules {
        let mut a = vec!["rule", "add"];
        a.extend_from_slice(r);
        e.ok(&a);
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_builds_modes_and_layers_headlessly() {
    require_e2e();
    let sb = TempDir::new("step72-a");
    let e = env(&sb, bin("ddalgak"), "a");
    build_profile(&e);
    let list = e.ok(&["mode", "list"]);
    let modes = list["data"]["modes"].as_array().unwrap();
    let row = |id: &str| modes.iter().find(|m| m["id"] == id).unwrap().clone();
    assert_eq!(row("sym")["chain"], json!(["sym", "nav", "base"]));
    assert_eq!(row("game")["policy"], "exclusive");
    assert_eq!(row("nav")["policy"], "layer");
    assert_eq!(row("accent")["one_shot"], true);
    assert_eq!(row("numpad")["timeout_ms"], 10000);
    assert_eq!(row("base")["default"], true);
    let show = e.ok(&["mode", "show", "nav"]);
    assert_eq!(show["data"]["mode"]["switched_by"], json!(["hold-nav"]));
    assert_eq!(show["data"]["mode"]["layers"], json!(["sym"]));
    e.ok(&["mode", "rename", "sym", "Signs"]);
    let d = e.ok(&["mode", "copy", "nav", "--id", "nav2"]);
    assert_eq!(d["data"]["created"], "nav2");
    let before = e.rules_bytes();
    e.refused(&["mode", "edit", "base", "--parent", "sym"], "mode_invalid");
    e.refused(&["mode", "edit", "nav", "--parent", "sym"], "mode_invalid");
    e.refused(&["mode", "delete", "base", "--yes"], "mode_in_use");
    e.refused(&["mode", "delete", "nav", "--yes"], "mode_in_use");
    e.refused(&["mode", "delete", "game", "--yes"], "mode_in_use");
    e.refused(&["mode", "delete", "nav2"], "confirmation_required");
    e.refused(&["mode", "show", "ghost"], "mode_not_found");
    e.refused(
        &[
            "rule",
            "add",
            "--trigger",
            "F19",
            "--action",
            "mode",
            "--op",
            "set",
            "--target-mode",
            "ghost",
        ],
        "mode_not_found",
    );
    assert_eq!(e.rules_bytes(), before, "refusals write nothing");
    e.ok(&["mode", "delete", "nav2", "--delete-rules", "--yes"]);
    let check = e.ok(&["rule", "check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0, "{check:#}");
    let st = e.ok(&["mode", "status"]);
    assert_eq!(
        (
            st["data"]["mode"]["active"].as_str(),
            st["data"]["mode"]["live"].as_bool()
        ),
        (Some("base"), Some(false))
    );
    e.refused(&["mode", "set", "nav"], "engine_not_running");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_switches_modes_releases_and_never_restores_them() {
    require_e2e();
    let sb = TempDir::new("step72-b");
    let e = env(&sb, bin("ddalgak"), "b");
    build_profile(&e);
    let _host = e.start_host();
    e.refused(&["mode", "set", "nav"], "engine_off");
    if ddalgak::win::is_elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        e.json(&["quit"]);
        return;
    }
    e.ok(&["engine", "start"]);
    assert_eq!(
        e.mode()["active"],
        "base",
        "every start begins in the default mode"
    );
    let d = e.ok(&["mode", "set", "sym"]);
    assert_eq!(d["data"]["change"]["to"], "sym");
    let m = e.mode();
    assert_eq!(m["chain"], json!(["sym", "nav", "base"]));
    assert_eq!(m["indicator"]["label"], "Signs");
    e.ok(&["mode", "toggle", "sym"]);
    assert_eq!(e.mode()["active"], "base");
    e.ok(&["mode", "set", "game"]);
    e.ok(&["pause"]);
    assert_eq!(e.mode()["origin"], "pause");
    assert_eq!(e.mode()["active"], "base", "a pause is the safe reset");
    e.ok(&["resume"]);
    e.ok(&["mode", "add", "--title", "Temporary", "--id", "temp"]);
    e.ok(&["mode", "set", "temp"]);
    e.ok(&["mode", "delete", "temp", "--yes"]);
    assert_eq!(e.wait_mode("base")["origin"], "reload");
    e.ok(&["mode", "set", "game"]);
    e.ok(&["mode", "reset"]);
    assert_eq!(e.mode()["active"], "base");
    e.ok(&["mode", "set", "game"]);
    e.ok(&["engine", "stop"]);
    e.ok(&["engine", "start"]);
    assert_eq!(
        e.mode()["active"],
        "base",
        "a restart never restores a mode"
    );
    let send = |vk: u16, up: bool| {
        ddalgak::win::inject::send_as_other_program(&[ddalgak::inject::OutKey { vk, up }]) == 1
    };
    if ddalgak::win::input_desktop_available() && send(VK_F14, false) && send(VK_F14, true) {
        assert_eq!(e.wait_mode("game")["origin"], "rule:to-game");
        if send(VK_F13, false) {
            let m = e.wait_mode("nav");
            assert_eq!(
                (m["held"].clone(), m["latched"].clone()),
                (json!(["nav"]), json!("game"))
            );
            assert!(send(VK_F13, true));
            assert_eq!(e.wait_mode("game")["origin"], "released:hold-nav");
        } else {
            let _ = send(VK_F13, true);
            eprintln!("skipped (SendInput refused) at F13");
        }
    } else {
        let _ = send(VK_F14, true);
        eprintln!("skipped (locked or SendInput refused): mode rules through the real hook");
    }
    let (_, d) = e.json(&["quit"]);
    assert_eq!(d["data"]["quitting"], true, "{d:#}");
}
