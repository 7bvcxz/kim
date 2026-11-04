//! E2E — Step 71: Ddalgak mouse movement, scrolling and click actions (docs/DDALGAK_RULE_MODEL.md
//! §5.7–§5.9, docs/DDALGAK_ENGINE.md §14, ADR 0040). Requirements: `DDALGAK-FR-015`,
//! `DDALGAK-FR-060`, `DDALGAK-SEC-001`, `DDALGAK-SEC-004`, `DDALGAK-SEC-005`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app): an agent writes a keyboard-mouse
//! profile headlessly with the Step 67 syntax — a glide while held with speed and acceleration, a
//! grid jump that repeats while held, a jump to the middle of the primary monitor, a wheel rule, a
//! held left button for dragging, a back-button click at a point beside the desktop, and two
//! mouse steps in order given as JSON — each with `--allow input-injection`. It is refused, with
//! nothing written: a move by a distance and to a point at once, a move while held without a
//! direction, a continuous grid move, a speed beyond 5000 px/s, an unknown wheel direction, a
//! held button with repeat or on a sequence. `--dry-run` writes nothing; without `--allow` a rule
//! is saved disabled (`consent_required`). `rule list` shows every mouse rule active, `rule check`
//! finds no error, and the stored steps are canonical.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key,
//! a simulated pointer and a discarding injector): the engine loads the mouse rules active and
//! installs **no** mouse hook (mouse actions never track the pointer). On an unlocked desktop an
//! injected F13 (an obscure key, sent as another program) is refused by the mouse rule
//! (`skipped_injected_input`: injecting rules accept physical keys only); on a locked desktop that
//! part is reported as skipped. Pause and resume keep the rules; the agent quits the app.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; **the real pointer
//! is never moved, scrolled or clicked** (the test host's mouse actions go to a simulated pointer
//! and a discarding backend; the only real input is the obscure key F13); every app is quit or
//! killed by a guard.
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
        assert!(out.stderr.is_empty(), "{args:?}: stderr with --json");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn rules_bytes(&self) -> Vec<u8> {
        std::fs::read(self.sb.path().join("Roaming/mouv/ddalgak/rules.json")).unwrap_or_default()
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

fn env<'a>(sb: &'a TempDir, cli: PathBuf, tag: &str) -> Env<'a> {
    Env {
        sb,
        cli,
        endpoint: format!("kim-e2e-step71-{tag}-{}", std::process::id()),
    }
}

const RULES: &[(&str, &str, &[&str])] = &[
    (
        "glide-right",
        "F13",
        &[
            "--action",
            "mouse_move",
            "--dx",
            "1",
            "--dy",
            "0",
            "--speed",
            "600",
            "--acceleration",
            "3",
        ],
    ),
    (
        "cell-left",
        "F14",
        &[
            "--action",
            "mouse_move",
            "--dx",
            "-1",
            "--grid",
            "16x9",
            "--repeat-ms",
            "150",
            "--delay-ms",
            "400",
        ],
    ),
    (
        "centre",
        "F15",
        &[
            "--action",
            "mouse_move",
            "--x",
            "960",
            "--y",
            "540",
            "--relative-to",
            "primary_monitor",
        ],
    ),
    (
        "wheel",
        "F16",
        &[
            "--action",
            "mouse_scroll",
            "--direction",
            "down",
            "--notches",
            "3",
        ],
    ),
    (
        "drag",
        "F17",
        &[
            "--action",
            "mouse_click",
            "--button",
            "left",
            "--op",
            "hold",
        ],
    ),
    (
        "back",
        "F18",
        &[
            "--action",
            "mouse_click",
            "--button",
            "x1",
            "--x",
            "-30000",
            "--y",
            "300",
        ],
    ),
    (
        "corner-double",
        "F19",
        &[
            "--step",
            r#"{"type":"mouse_move","mode":"absolute","to":{"x":24,"y":24,"relative_to":"active_window"}}"#,
            "--step",
            r#"{"type":"mouse_click","button":"left","op":"double_click"}"#,
        ],
    ),
];

/// Scenario A, shared with B: the rules an agent writes headlessly.
fn build_rules(e: &Env) {
    let add = |id: &str, keys: &str, action: &[&str], extra: &[&str]| {
        let mut a = vec!["rule", "add", "--id", id, "--trigger", keys];
        a.extend_from_slice(action);
        a.extend_from_slice(extra);
        e.json(&a)
    };
    // A dry run writes nothing.
    let (code, _) = add(
        "glide-right",
        "F13",
        RULES[0].2,
        &["--allow", "input-injection", "--dry-run"],
    );
    assert_eq!(code, 0);
    assert!(e.rules_bytes().is_empty(), "a dry run writes nothing");
    for (id, keys, action) in RULES {
        let (code, d) = add(id, keys, action, &["--allow", "input-injection"]);
        assert_eq!(code, 0, "{id}: {d:#}");
    }
    let before = e.rules_bytes();
    for (args, code) in [
        (
            &[
                "--action",
                "mouse_move",
                "--dx",
                "1",
                "--x",
                "3",
                "--y",
                "4",
            ][..],
            "usage",
        ),
        (
            &[
                "--action",
                "mouse_move",
                "--dx",
                "0",
                "--dy",
                "0",
                "--speed",
                "600",
            ],
            "rule_invalid",
        ),
        (
            &[
                "--action",
                "mouse_move",
                "--dx",
                "1",
                "--grid",
                "8x8",
                "--speed",
                "600",
            ],
            "rule_invalid",
        ),
        (
            &["--action", "mouse_move", "--dx", "1", "--speed", "9000"],
            "rule_invalid",
        ),
        (
            &["--action", "mouse_scroll", "--direction", "sideways"],
            "usage",
        ),
        (
            &[
                "--repeat",
                "--action",
                "mouse_click",
                "--button",
                "left",
                "--op",
                "hold",
            ],
            "rule_invalid",
        ),
    ] {
        let (exit, d) = add("refused", "F20", args, &["--allow", "input-injection"]);
        assert_eq!(
            (exit, d["error"]["code"].as_str()),
            (2, Some(code)),
            "{args:?}: {d:#}"
        );
    }
    let (exit, d) = add(
        "held-sequence",
        "F20 F21",
        &[
            "--action",
            "mouse_click",
            "--button",
            "left",
            "--op",
            "hold",
        ],
        &["--allow", "input-injection"],
    );
    assert_eq!(
        (exit, d["error"]["code"].as_str()),
        (2, Some("rule_invalid"))
    );
    assert_eq!(e.rules_bytes(), before, "refusals write nothing");
    // Without --allow: saved disabled.
    let (exit, d) = add(
        "no-consent",
        "F20",
        &["--action", "mouse_scroll", "--direction", "up"],
        &[],
    );
    assert_eq!(
        (exit, d["error"]["code"].as_str()),
        (7, Some("consent_required"))
    );
    let (_, d) = e.json(&["rule", "delete", "no-consent", "--yes"]);
    assert_eq!(d["ok"], true, "{d:#}");
    // As stored: canonical steps.
    let doc: Value = serde_json::from_slice(&e.rules_bytes()).unwrap();
    let rules = doc["profiles"][0]["rules"].as_array().unwrap();
    let step = |id: &str| rules.iter().find(|r| r["id"] == id).unwrap()["steps"][0].clone();
    assert_eq!(
        step("glide-right"),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,
               "while_held":{"speed":600,"acceleration":3}})
    );
    assert_eq!(
        step("cell-left"),
        json!({"type":"mouse_move","mode":"relative","dx":-1,"dy":0,"unit":"grid",
               "grid":{"columns":16,"rows":9},"while_held":{"repeat_ms":150,"delay_ms":400}})
    );
    assert_eq!(
        step("drag"),
        json!({"type":"mouse_click","button":"left","op":"hold"})
    );
    let (_, list) = e.json(&["rule", "list"]);
    let rows = list["data"]["rules"].as_array().unwrap();
    assert_eq!(rows.len(), RULES.len());
    assert!(
        rows.iter().all(|r| r["state"] == "active"),
        "every mouse rule runs: {list:#}"
    );
    let (_, check) = e.json(&["rule", "check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0, "{check:#}");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_writes_a_keyboard_mouse_profile_headlessly() {
    require_e2e();
    let sb = TempDir::new("step71-a");
    let e = env(&sb, bin("ddalgak"), "a");
    build_rules(&e);
    // No app runs: the CLI never starts one.
    let (code, d) = e.json(&["engine", "start"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (3, Some("engine_not_running"))
    );
    let (_, st) = e.json(&["status"]);
    assert_eq!(st["data"]["app"]["running"], false);
    assert_eq!(st["data"]["rules"]["active"], RULES.len());
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_loads_mouse_rules_without_a_mouse_hook_and_moves_no_real_pointer() {
    require_e2e();
    let sb = TempDir::new("step71-b");
    let e = env(&sb, bin("ddalgak"), "b");
    build_rules(&e);
    let _host = e.start_host();
    if ddalgak::win::is_elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        e.json(&["quit"]);
        return;
    }
    let (_, d) = e.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    let (_, st) = e.json(&["status"]);
    let ids: Vec<&str> = RULES.iter().map(|r| r.0).collect();
    assert_eq!(st["data"]["rules"]["active_rules"], json!(ids), "{st:#}");
    assert_eq!(
        st["data"]["engine"]["hook"]["mouse_installed"], false,
        "mouse actions never install the mouse hook"
    );
    if ddalgak::win::input_desktop_available()
        && ddalgak::win::inject::send_as_other_program(&[
            ddalgak::inject::OutKey {
                vk: VK_F13,
                up: false,
            },
            ddalgak::inject::OutKey {
                vk: VK_F13,
                up: true,
            },
        ]) == 2
    {
        let until = Instant::now() + Duration::from_secs(10);
        let last = loop {
            let (_, st) = e.json(&["status"]);
            if let Some(a) = st["data"]["activity"].as_array().and_then(|a| a.last()) {
                break a.clone();
            }
            assert!(Instant::now() < until, "no activity: {st:#}");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            (last["rule"].as_str(), last["outcome"].as_str()),
            (Some("glide-right"), Some("skipped_injected_input"))
        );
    } else {
        eprintln!("skipped (locked or SendInput refused): the injected-input check");
    }
    assert_eq!(e.json(&["pause"]).1["data"]["change"]["to"], "paused");
    assert_eq!(e.json(&["resume"]).1["data"]["change"]["to"], "running");
    let (_, st) = e.json(&["status"]);
    assert_eq!(st["data"]["rules"]["active"], RULES.len());
    let (_, d) = e.json(&["quit"]);
    assert_eq!(d["data"]["quitting"], true, "{d:#}");
}
