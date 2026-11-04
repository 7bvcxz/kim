//! E2E — Step 67: Ddalgak's rule store and rule commands (docs/DDALGAK_RULE_MODEL.md §13,
//! ADR 0028). Requirements: `DDALGAK-FR-005`, `DDALGAK-FR-006`, `DDALGAK-FR-007`,
//! `DDALGAK-FR-009`, `DDALGAK-FR-060`, `DDALGAK-NFR-008`, `DDALGAK-SEC-006`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app, no hook): an agent builds a rule set
//! headlessly — adds an engine rule with a generated ID, is refused a duplicate trigger with
//! rebind offers (nothing written), gets a remap saved disabled until it passes
//! `--allow input-injection`, renames a rule (the ID stays), duplicates, moves and deletes
//! (`--yes`), works in a draft that leaves the saved rules alone and applies it, and reads the
//! history. Every document is validated against `ddalgak-cli-v1`.
//!
//! Scenario B (release `ddalgak`): eight agents add rules at the same moment; none is lost and
//! the revisions count every write.
//!
//! Scenario C (debug `ddalgak`, whose test hook stops a save half-way): the writer is killed in
//! the middle of writing; `rules.json` is the previous version, the next save works. Then the
//! file is corrupted by hand: every command refuses with `rules_invalid` and never rewrites it,
//! `rules restore --backup --yes` brings back the last good version and keeps the bad file.
//!
//! Machine safety: everything happens in a temporary `%APPDATA%`/`%LOCALAPPDATA%`; no app is
//! started, no hook is installed, no key is injected, the user's real rules are never touched.
#![cfg(windows)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::Value;
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
    /// An endpoint no app serves: rule changes are simply saved.
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

    fn args<'b>(&'b self, args: &[&'b str]) -> Vec<&'b str> {
        let mut a = args.to_vec();
        a.extend(["--json", "--endpoint", &self.endpoint]);
        a
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut c = self.cmd(&self.cli);
        c.args(self.args(args));
        let out = c.output().unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        let errors = jsonschema::Validator::new(&schema())
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{doc:#}");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn spawn(&self, exe: &Path, args: &[&str]) -> Child {
        let mut c = self.cmd(exe);
        c.args(self.args(args))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        c.spawn().unwrap()
    }

    fn rules(&self) -> PathBuf {
        self.sb.path().join("Roaming/mouv/ddalgak/rules.json")
    }

    fn ids(&self) -> Vec<String> {
        self.json(&["rule", "list"]).1["data"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_manages_rules_headlessly() {
    let sb = TempDir::new("step67-crud");
    let e = Env {
        sb: &sb,
        cli: bin("ddalgak"),
        endpoint: format!("ddalgak-e2e-step67-{}", std::process::id()),
    };
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--trigger",
        "ctrl+shift+f13",
        "--action",
        "engine",
        "--op",
        "toggle_pause",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let pause = d["data"]["created"].as_str().unwrap().to_string();
    assert!(pause.starts_with("r-"));
    assert_eq!(d["data"]["app"]["running"], false);
    // A duplicate trigger is refused with offers; nothing is written.
    let before = std::fs::read(e.rules()).unwrap();
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--trigger",
        "Ctrl+Shift+F13",
        "--action",
        "engine",
        "--op",
        "pause",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("rule_conflict"))
    );
    assert_eq!(std::fs::read(e.rules()).unwrap(), before);
    // Consent: saved disabled, then approved explicitly.
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--trigger",
        "CapsLock",
        "--action",
        "remap",
        "--to",
        "LCtrl",
        "--id",
        "caps",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("consent_required"))
    );
    assert_eq!(
        e.json(&["rule", "show", "caps"]).1["data"]["state"],
        "disabled"
    );
    let (code, _) = e.json(&["rule", "enable", "caps", "--allow", "input-injection"]);
    assert_eq!(code, 0);
    // Rename keeps the ID; duplicate, move, delete.
    e.json(&["rule", "edit", &pause, "--title", "Pause or resume"]);
    let (_, d) = e.json(&["rule", "show", &pause]);
    assert_eq!(
        (
            d["data"]["rule"]["id"].as_str(),
            d["data"]["rule"]["title"].as_str()
        ),
        (Some(pause.as_str()), Some("Pause or resume"))
    );
    e.json(&[
        "rule",
        "duplicate",
        &pause,
        "--id",
        "copy",
        "--trigger",
        "Ctrl+Shift+F14",
    ]);
    e.json(&["rule", "enable", "copy"]);
    e.json(&["rule", "move", "copy", "--to", "start"]);
    assert_eq!(e.ids()[0], "copy");
    assert_eq!(
        e.json(&["rule", "delete", "copy"]).0,
        7,
        "needs --yes headless"
    );
    assert_eq!(e.json(&["rule", "delete", "copy", "--yes"]).0, 0);
    // A draft leaves the saved rules alone until it is applied.
    let saved = std::fs::read(e.rules()).unwrap();
    e.json(&["rule", "disable", &pause, "--draft"]);
    assert_eq!(std::fs::read(e.rules()).unwrap(), saved);
    assert_eq!(
        e.json(&["status"]).1["data"]["rules"]["draft"]["stale"],
        false
    );
    let (code, _) = e.json(&["rules", "draft", "apply"]);
    assert_eq!(code, 0);
    assert_eq!(
        e.json(&["rule", "show", &pause]).1["data"]["state"],
        "disabled"
    );
    let (_, d) = e.json(&["rules", "history"]);
    assert!(d["data"]["snapshots"].as_array().unwrap().len() >= 8);
    let (_, d) = e.json(&["rule", "check"]);
    assert_eq!(d["data"]["ok"], true);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn eight_agents_at_once_lose_nothing() {
    let sb = TempDir::new("step67-concurrent");
    let e = Env {
        sb: &sb,
        cli: bin("ddalgak"),
        endpoint: format!("ddalgak-e2e-step67c-{}", std::process::id()),
    };
    let keys = ["F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20"];
    let kids: Vec<Child> = keys
        .iter()
        .map(|k| {
            let trigger = format!("Alt+Shift+{k}");
            let id = format!("agent-{}", k.to_lowercase());
            e.spawn(
                &e.cli,
                &[
                    "rule",
                    "add",
                    "--trigger",
                    &trigger,
                    "--action",
                    "engine",
                    "--op",
                    "pause",
                    "--id",
                    &id,
                ],
            )
        })
        .collect();
    for k in kids {
        assert_eq!(k.wait_with_output().unwrap().status.code(), Some(0));
    }
    let (_, d) = e.json(&["rule", "list"]);
    assert_eq!(d["data"]["revision"], 8);
    assert_eq!(d["data"]["rules"].as_array().unwrap().len(), 8);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_killed_writer_and_a_corrupt_file_never_cost_the_last_good_rules() {
    let sb = TempDir::new("step67-crash");
    let e = Env {
        sb: &sb,
        cli: bin_debug("ddalgak"),
        endpoint: format!("ddalgak-e2e-step67k-{}", std::process::id()),
    };
    for (id, keys) in [("first", "F13"), ("second", "F14")] {
        let (code, _) = e.json(&[
            "rule",
            "add",
            "--trigger",
            keys,
            "--action",
            "engine",
            "--op",
            "pause",
            "--id",
            id,
        ]);
        assert_eq!(code, 0);
    }
    let good = std::fs::read(e.rules()).unwrap();
    // Kill a writer half-way through writing the new file (debug-build test hook).
    let marker = sb.path().join("crash.marker");
    let mut c = e.cmd(&e.cli);
    c.args(e.args(&[
        "rule",
        "add",
        "--trigger",
        "F15",
        "--action",
        "engine",
        "--op",
        "pause",
    ]))
    .env("DDALGAK_TEST_CRASH_IN_SAVE", &marker)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut writer = c.spawn().unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(
            Instant::now() < until,
            "the writer never reached the crash point"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    writer.kill().unwrap();
    let _ = writer.wait();
    assert_eq!(
        std::fs::read(e.rules()).unwrap(),
        good,
        "the previous version is intact"
    );
    assert_eq!(
        e.json(&["status"]).1["data"]["rules"]["saved"]["interrupted_save"],
        true
    );
    let (code, _) = e.json(&[
        "rule",
        "add",
        "--trigger",
        "F16",
        "--action",
        "engine",
        "--op",
        "pause",
        "--id",
        "third",
    ]);
    assert_eq!(code, 0, "the dead writer's lock was released by the OS");
    assert_eq!(e.ids(), ["first", "second", "third"]);
    // A corrupt file: safe mode, never rewritten, restorable, kept.
    std::fs::write(e.rules(), b"{ corrupt").unwrap();
    assert_eq!(
        e.json(&["rule", "list"]).1["error"]["code"],
        "rules_invalid"
    );
    assert_eq!(
        e.json(&["rule", "add", "--trigger", "F17", "--action", "block"])
            .0,
        4
    );
    assert_eq!(std::fs::read(e.rules()).unwrap(), b"{ corrupt");
    let (code, d) = e.json(&["rules", "restore", "--backup", "--yes"]);
    assert_eq!(code, 0, "{d:#}");
    let kept = d["data"]["kept"].as_str().unwrap();
    assert!(kept.starts_with("rules.corrupt-"));
    assert_eq!(e.ids(), ["first", "second"]);
}
