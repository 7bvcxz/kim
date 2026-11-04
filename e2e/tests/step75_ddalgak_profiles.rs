//! E2E — Step 75: Ddalgak profiles — save, load, version, switch, export and import
//! (docs/DDALGAK_RULE_MODEL.md §10, §13, docs/DDALGAK_ENGINE.md §17, ADR 0052).
//! Requirements: `DDALGAK-FR-030`, `DDALGAK-FR-031`, `DDALGAK-FR-032`, `DDALGAK-FR-060`,
//! `DDALGAK-SEC-005`, `DDALGAK-SEC-006`, `DDALGAK-SEC-011`, `DDALGAK-SEC-013`,
//! `DDALGAK-NFR-005`, `DDALGAK-NFR-008`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app): an agent keeps three complete
//! profiles (work, gaming, development) headlessly: creates, renames, duplicates and searches
//! them; previews a switch (`--dry-run`: the side-by-side diff, nothing written) and switches;
//! deletes are refused for the active profile (`profile_in_use`); a save point is made, a broken
//! hand edit is kept by `rules backup`, `profile history` names the newest working version and
//! `profile rollback --yes` restores it. It exports two profiles (no consent, settings,
//! revision or environment value in the file, every rule off) and imports them into a second,
//! empty profile store through the preview: never active, every rule off and without consent,
//! renamed IDs where they clash. A corrupt, a truncated and a newer file are refused
//! (`import_invalid`, `rules_from_newer_version`) with nothing written. Every output validates
//! against `ddalgak-cli-v1` and none carries the environment secret.
//!
//! Scenario B (debug test host of `ddalgak-gui`: no tray icon, a hook that never consumes a key):
//! `profile apply` makes the running app switch; the reload answer names the new profile
//! (`switched: true`) and the engine keeps its state and hook; the mode after a switch is the
//! new profile's default even when the old one latched a mode with the same ID; an import into
//! the running app never switches; the agent kills its own host right after a switch was saved
//! and the next start runs the new profile with the engine off (`unclean_exit`).
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; no key is injected,
//! nothing is opened or run (the launch rule's program is never started), only the test's own
//! host is killed; no window or tray icon appears.
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
const SECRET: &str = "E2E-Step75-비밀-token";

fn schema() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/schemas/ddalgak-cli-v1.schema.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// Work (active), gaming and development; the work profile opens a program with a secret
/// environment value (never started by this test).
fn rules_doc(on_start: &str) -> Value {
    let mut launch = json!({ "id": "w-tool", "title": "F16 opens the tool", "enabled": true,
        "trigger": { "keys": "Ctrl+Alt+Shift+F16" },
        "steps": [ { "type": "open_program", "path": r"C:\Kim-E2E-Step75\never-started.exe",
                     "env": [ { "name": "TOOL_TOKEN", "value": SECRET } ] } ] });
    let fp = ddalgak::validate::launch_fingerprint(&launch);
    launch["consent"] = json!({ "launch-programs": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui", "fingerprint": fp } });
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "work",
        "settings": { "engine_on_start": on_start },
        "profiles": [
            { "id": "work", "title": "Work", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
              "rules": [
                { "id": "w-pause", "title": "F13 pauses", "enabled": true,
                  "trigger": { "keys": "F13" }, "steps": [ { "type": "engine", "op": "toggle_pause" } ] },
                launch
              ] },
            { "id": "gaming", "title": "Gaming", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Arrows" } ],
              "rules": [
                { "id": "g-off", "title": "F13 turns the engine off", "enabled": true,
                  "trigger": { "keys": "F13" }, "steps": [ { "type": "engine", "op": "disable" } ] }
              ] },
            { "id": "dev", "title": "Development", "default_mode": "base",
              "modes": [ { "id": "base", "title": "Base" } ], "rules": [] }
        ]
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
        assert!(
            !doc.to_string().contains(SECRET),
            "{args:?}: the environment value appeared"
        );
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

    fn config(&self) -> PathBuf {
        self.sb.path().join("Roaming/mouv/ddalgak")
    }

    fn rules_bytes(&self) -> Vec<u8> {
        std::fs::read(self.config().join("rules.json")).unwrap_or_default()
    }

    fn write_rules(&self, doc: &Value) {
        std::fs::create_dir_all(self.config()).unwrap();
        std::fs::write(self.config().join("rules.json"), doc.to_string()).unwrap();
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
        endpoint: format!("kim-e2e-step75-{tag}-{}", std::process::id()),
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_keeps_versions_switches_exports_and_imports_profiles_headlessly() {
    require_e2e();
    let sb = TempDir::new("step75-cli");
    let e = env(&sb, bin("ddalgak"), "a");
    e.write_rules(&rules_doc("off"));
    // Browse.
    let d = e.ok(&["profile", "list", "--sort", "rules"]);
    assert_eq!(d["data"]["total"], 3);
    assert_eq!(d["data"]["profiles"][0]["id"], "work");
    let d = e.ok(&["profile", "list", "--search", "DEVELOP"]);
    assert_eq!(d["data"]["profiles"][0]["id"], "dev");
    // Create, rename, duplicate.
    e.ok(&[
        "profile",
        "add",
        "--title",
        "Writing",
        "--description",
        "글쓰기",
    ]);
    e.ok(&["profile", "rename", "writing", "Writing (Korean)"]);
    let d = e.ok(&["profile", "duplicate", "work", "--id", "work-home"]);
    assert_eq!(d["data"]["rules"], json!(["w-pause-2", "w-tool-2"]));
    // Preview, switch, refusals.
    let before = e.rules_bytes();
    let d = e.ok(&["profile", "apply", "gaming", "--dry-run"]);
    assert_eq!(d["data"]["preview"]["align"], "trigger");
    assert_eq!(e.rules_bytes(), before, "a preview writes nothing");
    let d = e.ok(&["profile", "apply", "gaming"]);
    assert_eq!(d["data"]["to"], "gaming");
    e.refused(&["profile", "delete", "gaming", "--yes"], "profile_in_use");
    e.refused(&["profile", "delete", "work-home"], "confirmation_required");
    e.ok(&["profile", "delete", "work-home", "--yes"]);
    // Save point, a broken hand edit, rollback to the last working version.
    let d = e.ok(&["profile", "save", "work", "--label", "e2e before"]);
    let save_point = d["data"]["save_point"].as_str().unwrap().to_string();
    let mut doc: Value = serde_json::from_slice(&e.rules_bytes()).unwrap();
    doc["profiles"][0]["rules"][0]["modes"] = json!(["deleted-mode"]);
    doc["revision"] = json!(doc["revision"].as_u64().unwrap() + 1);
    e.write_rules(&doc);
    e.ok(&["rules", "backup"]);
    let d = e.ok(&["profile", "history", "work"]);
    assert_eq!(d["data"]["versions"][0]["working"], false);
    assert_eq!(d["data"]["rollback_to"], save_point.as_str());
    let d = e.ok(&["profile", "rollback", "work", "--yes"]);
    assert_eq!(d["data"]["source"], save_point.as_str());
    assert_eq!(e.ok(&["rule", "check"])["data"]["ok"], true);
    // Export: portable, inert, without the secret.
    let file = sb.path().join("공유 profiles.json");
    let fs = file.display().to_string();
    let d = e.ok(&["profile", "export", "work", "gaming", "--output", &fs]);
    assert_eq!(d["data"]["env_values_removed"], 1);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(SECRET) && !text.contains("consent"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert!(v.get("settings").is_none() && v.get("revision").is_none());
    // Import into a second, empty store: preview first, never active.
    let other = TempDir::new("step75-cli-other");
    let e2 = env(&other, bin("ddalgak"), "b");
    let d = e2.ok(&["profile", "import", &fs, "--dry-run"]);
    assert_eq!(d["data"]["counts"]["profiles"], 2);
    assert_eq!(d["data"]["counts"]["need_consent"], 1);
    e2.ok(&["profile", "import", &fs]);
    let d = e2.ok(&["profile", "list"]);
    assert_eq!(d["data"]["active"], "default", "an import never switches");
    let saved: Value = serde_json::from_slice(&e2.rules_bytes()).unwrap();
    for p in saved["profiles"].as_array().unwrap().iter().skip(1) {
        for r in p["rules"].as_array().unwrap() {
            assert_eq!(r["enabled"], false);
            assert!(r.get("consent").is_none());
        }
    }
    // Into the first store again: every ID clashes and is renamed.
    let d = e.ok(&["profile", "import", &fs, "--dry-run"]);
    assert_eq!(d["data"]["profiles"][0]["id"], "work-2");
    // Corrupt, truncated and newer files.
    for (name, bytes) in [
        ("garbage.json", b"\x00\x01 not json".to_vec()),
        ("truncated.json", text.as_bytes()[..text.len() / 3].to_vec()),
    ] {
        let f = sb.path().join(name);
        std::fs::write(&f, bytes).unwrap();
        e.refused(
            &["profile", "import", &f.display().to_string()],
            "import_invalid",
        );
    }
    let newer = sb.path().join("newer.json");
    std::fs::write(
        &newer,
        text.replace("\"schema_version\": 1", "\"schema_version\": 3"),
    )
    .unwrap();
    e.refused(
        &["profile", "import", &newer.display().to_string()],
        "rules_from_newer_version",
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_running_app_switches_profiles_atomically_and_never_by_an_import() {
    require_e2e();
    let sb = TempDir::new("step75-app");
    let e = env(&sb, bin("ddalgak"), "app");
    e.write_rules(&rules_doc("on"));
    let host = e.start_host();
    let st = e.ok(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "running");
    assert_eq!(st["data"]["profile"]["id"], "work");
    // A latched mode in work, then a switch: gaming starts in its default mode.
    e.ok(&["mode", "set", "nav"]);
    let d = e.ok(&["profile", "apply", "gaming"]);
    assert_eq!(d["data"]["switched"], true, "{d:#}");
    let st = e.ok(&["status"]);
    assert_eq!(st["data"]["profile"]["id"], "gaming");
    assert_eq!(st["data"]["rules"]["active_rules"], json!(["g-off"]));
    assert_eq!(st["data"]["engine"]["state"], "running");
    assert_eq!(st["data"]["engine"]["hook"]["installed"], true);
    assert_eq!(e.ok(&["mode", "status"])["data"]["mode"]["active"], "base");
    // An import never switches the running app.
    let file = sb.path().join("dev.json");
    e.ok(&[
        "profile",
        "export",
        "dev",
        "--output",
        &file.display().to_string(),
    ]);
    e.ok(&["profile", "import", &file.display().to_string()]);
    assert_eq!(e.ok(&["status"])["data"]["profile"]["id"], "gaming");
    // The app killed right after a switch was saved: the new profile, engine off.
    e.ok(&["profile", "apply", "work"]);
    drop(host);
    let host = e.start_host();
    let st = e.ok(&["status"]);
    assert_eq!(st["data"]["profile"]["id"], "work");
    assert_eq!(st["data"]["engine"]["state"], "off");
    assert_eq!(st["data"]["start"]["off_reason"], "unclean_exit");
    e.ok(&["quit"]);
    drop(host);
}
