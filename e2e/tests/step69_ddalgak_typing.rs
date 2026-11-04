//! E2E — Step 69: Ddalgak typed text, sent keys and hotstrings (docs/DDALGAK_RULE_MODEL.md §3.4,
//! §5.3, §5.4, docs/DDALGAK_ENGINE.md §12, ADR 0034). Requirements: `DDALGAK-FR-011`,
//! `DDALGAK-FR-012`, `DDALGAK-FR-070`, `DDALGAK-SEC-001`, `DDALGAK-SEC-002`, `DDALGAK-SEC-011`,
//! `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app, no hook): an agent builds snippets
//! headlessly — a Korean sign-off with a new line, a tab and an emoji (`--escapes`), a pasted
//! address block (`--method paste`, `--allow clipboard`), a template with `{date}` and the
//! insertion point (`--variables`), the hotstrings `btw` (case conformity), `;sig` (immediate,
//! case-sensitive) and `;ac` (sends Ctrl+A Ctrl+C) — each with `--allow input-injection`. It is
//! refused, with nothing written: an environment or clipboard variable, a control character, a
//! hotstring with a space (`hotkey_invalid`), a second `BTW` (`rule_conflict`), a hotstring that
//! remaps, `--trigger` with `--hotstring` (`usage`). `rule list`, `rule show`, `rule check` and
//! `status` never contain the snippet text; `rule show --show-text` does, because the agent asked.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key,
//! a worker that never injects and never touches the clipboard, ends by itself): the agent turns
//! the engine on with those rules; `status` lists them all active, the activity is empty and no
//! output contains the text. Then it quits the app.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; **nothing is typed
//! or injected and the clipboard is never touched** (the test host's worker sends nothing and its
//! clipboard is always busy); every app is quit or killed by a guard.
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
const SECRET: &str = "E2E-SECRET-감사합니다";

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
    a.extend(["--allow", "input-injection"]);
    e.json(&a)
}

fn no_secret(what: &str, d: &Value) {
    let s = d.to_string();
    assert!(
        !s.contains("E2E-SECRET") && !s.contains("감사합니다"),
        "{what} must not contain the snippet: {s}"
    );
}

/// The snippet set of both scenarios.
fn build(e: &Env) {
    let sign = format!("{SECRET}\\n김민수 드림\\t\\u{{1F44D}}");
    let rules: Vec<Vec<&str>> = vec![
        vec![
            "--id",
            "sign",
            "--trigger",
            "Ctrl+Shift+F2",
            "--action",
            "type_text",
            "--text",
            &sign,
            "--escapes",
        ],
        vec![
            "--id",
            "address",
            "--trigger",
            "Ctrl+Shift+F3",
            "--action",
            "type_text",
            "--text",
            "서울특별시 중구\\n세종대로 110",
            "--escapes",
            "--method",
            "paste",
            "--allow",
            "clipboard",
        ],
        vec![
            "--id",
            "dated",
            "--trigger",
            "Ctrl+Shift+F4",
            "--action",
            "type_text",
            "--text",
            "Seoul, {date}\\n\\nDear {cursor},",
            "--escapes",
            "--variables",
        ],
        vec![
            "--id",
            "btw",
            "--hotstring",
            "btw",
            "--action",
            "type_text",
            "--text",
            "by the way",
        ],
        vec![
            "--id",
            "sig",
            "--hotstring",
            ";sig",
            "--immediate",
            "--case",
            "sensitive",
            "--action",
            "type_text",
            "--text",
            SECRET,
        ],
        vec![
            "--id",
            "select-copy",
            "--hotstring",
            ";ac",
            "--immediate",
            "--action",
            "send_keys",
            "--keys",
            "ctrl+a ctrl+c",
        ],
    ];
    for a in &rules {
        let (code, d) = add(e, a);
        assert_eq!(code, 0, "{a:?}: {d:#}");
        no_secret("rule add", &d);
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_builds_snippets_and_hotstrings_and_never_sees_their_text() {
    let sb = TempDir::new("step69-typing");
    let e = Env {
        sb: &sb,
        cli: bin("ddalgak"),
        endpoint: format!("ddalgak-e2e-step69-a-{}", std::process::id()),
    };
    build(&e);
    let stored: Value = serde_json::from_slice(&std::fs::read(e.rules()).unwrap()).unwrap();
    let r = &stored["profiles"][0]["rules"];
    assert_eq!(
        r[0]["steps"][0]["text"],
        format!("{SECRET}\n김민수 드림\t👍"),
        "stored exactly"
    );
    assert_eq!(
        r[4]["trigger"],
        json!({ "hotstring": { "abbreviation": ";sig", "case": "sensitive", "immediate": true } })
    );
    // Refusals: nothing is written, no text in the answer.
    let before = std::fs::read(e.rules()).unwrap();
    for (args, code, err) in [
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "E2E-SECRET {env:USERNAME}",
                "--variables",
            ],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "E2E-SECRET {clipboard}",
                "--variables",
            ],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "E2E-SECRET\\u{1b}",
                "--escapes",
            ],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--hotstring",
                "b t w",
                "--action",
                "type_text",
                "--text",
                "x",
            ],
            2,
            "hotkey_invalid",
        ),
        (
            vec!["--hotstring", "BTW", "--action", "type_text", "--text", "x"],
            7,
            "rule_conflict",
        ),
        (
            vec!["--hotstring", "zz", "--action", "remap", "--to", "F16"],
            2,
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--hotstring",
                "zz",
                "--action",
                "type_text",
                "--text",
                "x",
            ],
            2,
            "usage",
        ),
    ] {
        let (c, d) = add(&e, &args);
        assert_eq!(
            (c, d["error"]["code"].as_str()),
            (code, Some(err)),
            "{args:?}: {d:#}"
        );
        no_secret("a refusal", &d);
    }
    assert_eq!(
        std::fs::read(e.rules()).unwrap(),
        before,
        "nothing was written"
    );
    let (_, d) = e.json(&["rule", "list"]);
    no_secret("rule list", &d);
    let rows = d["data"]["rules"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    for row in rows {
        assert_eq!(row["state"], "active", "{row:#}");
    }
    assert_eq!(rows[3]["trigger"], "\"btw\" (hotstring)");
    let (_, d) = e.json(&["rule", "show", "sign"]);
    no_secret("rule show", &d);
    assert_eq!(d["data"]["text_redacted"], true);
    assert_eq!(
        d["data"]["rule"]["steps"][0]["text"]["preview"],
        json!({ "characters": 25, "lines": 2, "method": "unicode" })
    );
    let (_, d) = e.json(&["rule", "show", "dated"]);
    assert_eq!(
        d["data"]["rule"]["steps"][0]["text"]["preview"],
        json!({ "characters": 25, "lines": 3, "method": "unicode",
                "variables": ["date"], "cursor_back": 1 })
    );
    let (_, d) = e.json(&["rule", "show", "sign", "--show-text"]);
    assert!(d.to_string().contains("E2E-SECRET"), "asked for explicitly");
    let (_, d) = e.json(&["rule", "check"]);
    no_secret("rule check", &d);
    assert_eq!(d["data"]["ok"], true, "{d:#}");
    let (_, d) = e.json(&["status"]);
    no_secret("status", &d);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_loads_typing_rules_and_types_nothing_by_itself() {
    let sb = TempDir::new("step69-app");
    let e = Env {
        sb: &sb,
        cli: bin_debug("ddalgak"),
        endpoint: format!("ddalgak-e2e-step69-b-{}", std::process::id()),
    };
    build(&e);
    let _host = e.start_host();
    let (_, st) = e.json(&["status"]);
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["sign", "address", "dated", "btw", "sig", "select-copy"])
    );
    if st["data"]["app"]["elevated"] == true {
        eprintln!("elevated: the engine stays off (engine_elevated); the rest is not tested");
        return;
    }
    let (_, d) = e.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    std::thread::sleep(Duration::from_millis(500));
    let (_, st) = e.json(&["status"]);
    no_secret("status with the app", &st);
    assert_eq!(st["data"]["activity"], json!([]), "nothing ran by itself");
    let (_, h) = e.json(&["health"]);
    no_secret("health", &h);
    assert_eq!(h["data"]["healthy"], true, "{h:#}");
    let (code, _) = e.json(&["quit"]);
    assert_eq!(code, 0);
}
