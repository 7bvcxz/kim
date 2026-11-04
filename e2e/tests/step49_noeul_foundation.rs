//! E2E — Step 49: Noeul app foundation (docs/adr/0023-noeul-app-foundation.md). Requirements:
//! NOEUL-FR-001…003, NOEUL-FR-100, NOEUL-FR-110…112, NOEUL-FR-114, NOEUL-SEC-003, NOEUL-SEC-011.
//!
//! Scenario A (release `noeul`, isolated profile): an agent discovers Noeul headlessly —
//! capabilities, version, status while nothing runs, settings paths, the shortcut list — and
//! nothing is started or written.
//!
//! Scenario B: the user starts Noeul (no tray icon, private endpoint), a second start hands over
//! to it, `health` verifies it, its global shortcut is registered and moved to another key
//! through `noeul hotkeys set` while it runs, a capture request is checked with `--dry-run`, and
//! `quit` ends the process and releases everything.
//!
//! Scenario C: instance commands without an instance fail honestly (`noeul_not_running`), and a
//! region outside the desktop is a usage error.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, nothing captured, and
//! only the obscure chords `Ctrl+Alt+Shift+F22`/`F21` are ever registered. Every process started
//! here is ended.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin};
use serde_json::{Value, json};
use std::path::Path;
use std::process::{Command, Stdio};

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn valid(name: &str, v: &Value) {
    let (file, def) = name
        .split_once('#')
        .map_or((name, None), |(f, d)| (f, Some(d)));
    let mut s = schema(file);
    if let Some(d) = def {
        s = json!({"$ref": format!("#/$defs/{d}"), "$defs": s["$defs"].clone()});
    }
    let errors = jsonschema::Validator::new(&s).validate(v).unwrap();
    assert!(errors.is_empty(), "violates {name}: {errors:?}\n{v:#}");
}

struct Noeul {
    sb: TempDir,
    endpoint: String,
    pids: Vec<u64>,
}

impl Noeul {
    fn new(tag: &str) -> Noeul {
        let sb = TempDir::new(tag);
        let cfg = sb.path().join("Roaming").join("mouv").join("noeul");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(sb.path().join("Local")).unwrap();
        // Never the default F3: an obscure chord, written before anything starts.
        std::fs::write(
            cfg.join("hotkeys.json"),
            serde_json::to_vec(
                &json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
                "overrides": [{"action": "paste.float", "bindings": ["Ctrl+Alt+Shift+F22"]}]}),
            )
            .unwrap(),
        )
        .unwrap();
        Noeul {
            endpoint: format!("noeul-e2e-{tag}-{}", std::process::id()),
            sb,
            pids: Vec::new(),
        }
    }

    fn raw(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let p = self.sb.path();
        Command::new(bin("noeul"))
            .args(args)
            .args(["--json", "--endpoint", &self.endpoint])
            .env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env("KIM_NO_INPUT", "1")
            .env_remove("KIM_NO_HOTKEYS")
            .stdin(Stdio::null())
            .output()
    }

    /// `noeul <args> --json`, validated against cli-envelope-v1; the exit code matches.
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let out = self.raw(args).unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        valid("cli-envelope-v1", &doc);
        let code = out.status.code().unwrap_or(-1);
        let want = if doc["ok"] == true {
            0
        } else {
            doc["error"]["exit_code"].as_i64().unwrap()
        };
        assert_eq!(i64::from(code), want, "{args:?}: {doc:#}");
        (code, doc)
    }
}

impl Drop for Noeul {
    fn drop(&mut self) {
        let _ = self.raw(&["quit", "--yes"]);
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Last resort, only for a pid that is still this scenario's noeul.exe.
        for pid in &self.pids {
            let _ = Command::new("taskkill")
                .args([
                    "/F",
                    "/FI",
                    &format!("PID eq {pid}"),
                    "/FI",
                    "IMAGENAME eq noeul.exe",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_headless_discovery_starts_and_writes_nothing() {
    let n = Noeul::new("s49a");
    let before = n.sb.entry_count();
    let (_, caps) = n.json(&["capabilities"]);
    valid("tool-capabilities-v1", &caps["data"]);
    assert_eq!(caps["data"]["tool"], "noeul");
    let (_, v) = n.json(&["version"]);
    assert_eq!(v["data"]["ipc_protocol"]["max"], 1);
    let (code, st) = n.json(&["status"]);
    assert_eq!((code, st["data"]["running"].as_bool()), (0, Some(false)));
    valid("noeul-ipc-v1#offline_status", &st["data"]);
    let (_, cfg) = n.json(&["config", "show"]);
    assert!(
        cfg["data"]["paths"]["hotkeys_file"]
            .as_str()
            .unwrap()
            .ends_with("hotkeys.json")
    );
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    assert_eq!(list["data"]["actions"][0]["id"], "paste.float");
    assert_eq!(
        list["data"]["actions"][0]["bindings"][0]["registration"],
        "not_running"
    );
    assert_eq!(
        n.sb.entry_count(),
        before,
        "headless commands wrote nothing"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_start_hand_over_rebind_and_quit() {
    let mut n = Noeul::new("s49b");
    let (code, start) = n.json(&["start", "--no-tray"]);
    assert_eq!(code, 0, "{start:#}");
    let pid = start["data"]["pid"].as_u64().unwrap();
    n.pids.push(pid);
    assert_eq!(start["data"]["started"], true);
    let (_, again) = n.json(&["start", "--no-tray"]);
    assert_eq!(
        (
            again["data"]["already_running"].as_bool(),
            again["data"]["pid"].as_u64()
        ),
        (Some(true), Some(pid))
    );
    let (code, health) = n.json(&["health"]);
    assert_eq!(
        (code, health["data"]["identity"]["image"].as_str()),
        (0, Some("noeul.exe"))
    );
    let (_, st) = n.json(&["status"]);
    valid("noeul-ipc-v1#status_result", &st["data"]);
    let reg = &st["data"]["hotkeys"]["registrations"][0];
    assert_eq!(
        (reg["keys"].as_str(), reg["status"].as_str()),
        (Some("Ctrl+Alt+Shift+F22"), Some("registered"))
    );
    assert_eq!(st["data"]["tray"]["shown"], false);

    let (_, set) = n.json(&["hotkeys", "set", "paste.float", "Ctrl+Alt+Shift+F21"]);
    assert_eq!(set["data"]["written"], true);
    let (_, st) = n.json(&["status"]);
    let reg = &st["data"]["hotkeys"]["registrations"][0];
    assert_eq!(reg["keys"], "Ctrl+Alt+Shift+F21", "applied while running");
    let (_, list) = n.json(&["hotkeys", "list"]);
    assert_eq!(
        list["data"]["actions"][0]["bindings"][0]["registration"],
        reg["status"]
    );

    let desktop = st["data"]["desktop"]["state"].as_str().unwrap().to_string();
    let (code, dry) = n.json(&["capture", "--region", "0,0,1,1", "--pin", "--dry-run"]);
    if desktop == "available" {
        assert_eq!((code, dry["data"]["dry_run"].as_bool()), (0, Some(true)));
    } else {
        assert_eq!(
            dry["error"]["code"], "desktop_unavailable",
            "honest refusal on {desktop}"
        );
    }
    let (_, pins) = n.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"], json!([]), "a dry run pins nothing");

    let (code, q) = n.json(&["quit"]);
    assert_eq!(code, 0, "{q:#}");
    assert_eq!(q["data"]["exited"], true);
    let (_, st) = n.json(&["status"]);
    assert_eq!(st["data"]["running"], false);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn c_honest_errors_without_an_instance() {
    let n = Noeul::new("s49c");
    for args in [
        &["pin", "list"][..],
        &["health"],
        &["capture", "--region", "0,0,1,1", "--pin"],
        &["quit", "--yes"],
    ] {
        let (code, doc) = n.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    let (code, doc) = n.json(&["capture", "--region", "0,0,0,1", "--pin"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
}
