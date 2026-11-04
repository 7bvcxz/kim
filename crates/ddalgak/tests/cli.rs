//! CLI contract tests for `ddalgak` and the debug test host of `ddalgak-gui` (Step 66,
//! docs/DDALGAK_ENGINE.md, docs/CLI_CONTRACT.md). Every `--json` document is validated against
//! docs/contracts/schemas/ddalgak-cli-v1.schema.json. `DDALGAK_BLESS=1` writes the examples in
//! docs/contracts/examples/ddalgak-cli-v1/ from these real runs.
//!
//! Machine safety (the maintainer uses this PC): every test uses its own sandbox folders, its own
//! pipe endpoint and single-instance name, a private registry key below `HKCU\Software\mouv-test`
//! (never the real Run key), and the **debug test host** (`--test-host`): no tray icon, no window,
//! a hook that never consumes a key, and an app that ends by itself. Hosts are killed on every
//! path by a guard. Only the obscure keys F13–F20 are injected, and only when an interactive
//! desktop is available; otherwise the test says **skipped (locked)**.
//!
//! Requirements: `DDALGAK-FR-002`, `DDALGAK-FR-003`, `DDALGAK-FR-004`, `DDALGAK-FR-017`,
//! `DDALGAK-FR-060`, `DDALGAK-SEC-001`, `DDALGAK-SEC-004`, `DDALGAK-SEC-007`, `DDALGAK-SEC-010`,
//! `DDALGAK-SEC-011`, `KIM-FR-012`.
//!
//! Step 67 (rule store and CRUD, the `rule …`/`rules …` commands; `DDALGAK-FR-005`…`009`,
//! `DDALGAK-SEC-006`): CRUD with offers and refusals, two CLIs at once, a crash in the middle of a
//! save, a corrupt file (safe mode, file kept), read-only file and folder, and the reload of a
//! running app that keeps its hook installed. Every sandbox is its own `%APPDATA%`, so the user's
//! real rules are never read or written.
#![cfg(windows)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

use ddalgak::inject::OutKey;
use ddalgak::startup::Store;
use ddalgak::win::startup::{Registry, TEST_PREFIX};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const VK_F13: u16 = 0x7C;
const VK_F14: u16 = 0x7D;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema() -> &'static Value {
    static S: OnceLock<Value> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(
            &std::fs::read_to_string(
                repo().join("docs/contracts/schemas/ddalgak-cli-v1.schema.json"),
            )
            .unwrap(),
        )
        .unwrap()
    })
}

fn validate(doc: &Value) {
    let errors = jsonschema::Validator::new(schema()).validate(doc).unwrap();
    assert!(
        errors.is_empty(),
        "schema violations {errors:#?}\nin {doc:#}"
    );
}

/// Writes a real output as a contract example, with this machine's build folder and the test's
/// unique names replaced by stable placeholders (no local paths in the repository).
fn bless(name: &str, doc: &Value) {
    if std::env::var("DDALGAK_BLESS").as_deref() != Ok("1") {
        return;
    }
    let dir = repo().join("docs/contracts/examples/ddalgak-cli-v1");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = serde_json::to_string_pretty(doc).unwrap();
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_ddalgak"))
        .parent()
        .unwrap()
        .display()
        .to_string()
        .replace('/', "\\");
    let escaped = |s: &str| s.replace('\\', "\\\\");
    text = text.replace(
        &escaped(&bin_dir),
        &escaped(r"C:\Users\you\AppData\Local\Programs\mouv\ddalgak\versions\0.0.1"),
    );
    let temp = std::env::temp_dir()
        .display()
        .to_string()
        .trim_end_matches(['\\', '/'])
        .to_string();
    text = text.replace(
        &escaped(&temp),
        &escaped(r"C:\Users\you\AppData\Local\Temp"),
    );
    let unique = regex::Regex::new(r"ddalgak-(cli|test)-([a-z]+)-[0-9]+-[0-9]+").unwrap();
    text = unique.replace_all(&text, "ddalgak-$1-$2").to_string();
    validate(&serde_json::from_str(&text).unwrap());
    std::fs::write(dir.join(format!("{name}.json")), format!("{text}\n")).unwrap();
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

/// Own folders, endpoint and registry key; removed on drop.
struct Sandbox {
    root: PathBuf,
    endpoint: String,
    startup_key: String,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        let id = format!("{tag}-{}-{}", std::process::id(), nanos());
        let root = std::env::temp_dir().join(format!("ddalgak-cli-{id}"));
        std::fs::create_dir_all(&root).unwrap();
        Sandbox {
            root,
            endpoint: format!("ddalgak-test-{id}"),
            startup_key: format!("{TEST_PREFIX}ddalgak-cli-{id}\\Run"),
        }
    }

    fn config(&self) -> PathBuf {
        self.root.join("Roaming").join("mouv").join("ddalgak")
    }

    fn local(&self) -> PathBuf {
        self.root.join("Local").join("mouv").join("ddalgak")
    }

    fn env(&self, c: &mut Command) {
        c.env("APPDATA", self.root.join("Roaming"))
            .env("LOCALAPPDATA", self.root.join("Local"))
            .env("DDALGAK_DEV_STARTUP_KEY", &self.startup_key)
            .env_remove("KIM_NO_HOTKEYS")
            .creation_flags(CREATE_NO_WINDOW);
    }

    fn write_rules(&self, doc: &Value) {
        std::fs::create_dir_all(self.config()).unwrap();
        std::fs::write(self.config().join("rules.json"), doc.to_string()).unwrap();
    }

    fn write_hotkeys(&self, doc: &Value) {
        std::fs::create_dir_all(self.config()).unwrap();
        std::fs::write(self.config().join("hotkeys.json"), doc.to_string()).unwrap();
    }

    /// `ddalgak <args> --endpoint <mine>`.
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ddalgak"));
        c.args(args);
        // Rule changes ask a running app to reload: always this sandbox's app, never the user's.
        let talks_to_app = [
            "status", "health", "engine", "pause", "resume", "quit", "rule", "rules",
        ]
        .iter()
        .any(|c| args.first() == Some(c));
        if talks_to_app && !args.contains(&"--endpoint") {
            c.args(["--endpoint", &self.endpoint]);
        }
        self.env(&mut c);
        c.output().unwrap()
    }

    /// Runs with `--json`, checks the one-document rule and the schema, returns (exit, doc).
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut a = args.to_vec();
        a.push("--json");
        let out = self.run(&a);
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{a:?}: not one JSON document ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        assert!(
            out.stderr.is_empty(),
            "{a:?}: stderr must stay empty with --json: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        validate(&doc);
        let code = out.status.code().unwrap_or(-1);
        if doc["ok"] == false {
            assert_eq!(doc["error"]["exit_code"], code, "{a:?}");
        } else {
            assert_eq!(code, 0, "{a:?}");
        }
        (code, doc)
    }

    /// Starts the debug test host and waits until it answers.
    fn host(&self) -> Host {
        let child = self.spawn_host();
        let mut h = Host { child };
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            let (_, d) = self.json(&["status"]);
            if d["data"]["app"]["running"] == true {
                return h;
            }
            if let Ok(Some(status)) = h.child.try_wait() {
                panic!("the test host exited early: {status}");
            }
            assert!(Instant::now() < until, "the test host did not answer");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn spawn_host(&self) -> Child {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ddalgak-gui"));
        c.args([
            "--test-host",
            "--test-max-seconds",
            "60",
            "--endpoint",
            &self.endpoint,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
        self.env(&mut c);
        c.spawn().unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let parent = self.startup_key.trim_end_matches("\\Run").to_string();
        if let Some(r) = Registry::test_key(&parent) {
            let _ = r.delete_test_key();
        }
    }
}

/// A started test host; killed (and waited for) on every path.
struct Host {
    child: Child,
}

impl Host {
    fn wait_exit(&mut self, limit: Duration) -> Option<i32> {
        let until = Instant::now() + limit;
        while Instant::now() < until {
            if let Ok(Some(s)) = self.child.try_wait() {
                return s.code();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn rules_doc(on_start: &str) -> Value {
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "default",
        "settings": { "engine_on_start": on_start },
        "profiles": [ { "id": "default", "title": "Test", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" } ],
            "rules": [
                { "id": "pause-key", "title": "F13 pauses or resumes", "enabled": true,
                  "trigger": { "keys": "F13", "injected": "accept" },
                  "steps": [ { "type": "engine", "op": "toggle_pause" } ] },
                { "id": "off-key", "title": "F14 turns the engine off", "enabled": true,
                  "trigger": { "keys": "F14", "injected": "accept" },
                  "steps": [ { "type": "engine", "op": "disable" } ] },
                { "id": "physical-only", "title": "F15 pauses (physical keys only)", "enabled": true,
                  "trigger": { "keys": "F15" },
                  "steps": [ { "type": "engine", "op": "pause" } ] },
                { "id": "caps-remap", "title": "CapsLock acts as Ctrl", "enabled": true,
                  "trigger": { "keys": "CapsLock" },
                  "steps": [ { "type": "remap", "to": "LCtrl" } ],
                  "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } }
            ] } ]
    })
}

fn hotkeys_doc() -> Value {
    json!({ "schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
        "overrides": [
            { "action": "engine.toggle", "bindings": ["Ctrl+Alt+Shift+F13"] },
            { "action": "engine.pause", "bindings": ["Ctrl+Alt+Shift+F14"] },
            { "action": "editor.open", "bindings": ["Ctrl+Alt+Shift+F15"] }
        ] })
}

fn elevated() -> bool {
    ddalgak::win::is_elevated()
}

#[test]
fn version_help_capabilities_and_offline_contract() {
    let sb = Sandbox::new("offline");
    let v = sb.run(&["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&v.stdout).trim(),
        format!("ddalgak {}", ddalgak::VERSION)
    );
    let h = String::from_utf8_lossy(&sb.run(&["--help"]).stdout).to_string();
    assert!(
        h.contains("never starts the app") && h.contains("Ctrl+Alt+Shift+Escape"),
        "{h}"
    );
    let (_, caps) = sb.json(&["capabilities"]);
    bless("capabilities", &caps);
    // Without the app: status works from the files, everything else says engine_not_running.
    let (code, st) = sb.json(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(st["data"]["app"]["running"], false);
    assert_eq!(st["data"]["engine"]["off_reason"], "app_not_running");
    assert_eq!(st["data"]["rules"]["file"], "missing");
    bless("status-not-running", &st);
    for args in [
        vec!["engine", "start"],
        vec!["engine", "stop"],
        vec!["pause"],
        vec!["health"],
        vec!["quit"],
    ] {
        let (code, d) = sb.json(&args);
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (3, Some("engine_not_running")),
            "{args:?}"
        );
        if args == ["engine", "start"] {
            bless("error-engine-not-running", &d);
        }
    }
    let (code, d) = sb.json(&["engine", "fly"]);
    assert_eq!((code, d["command"].as_str()), (2, Some("engine.fly")));
    bless("error-usage", &d);
    // Human errors go to stderr, nothing to stdout.
    let out = sb.run(&["engine", "start"]);
    assert!(out.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("ddalgak: error: Ddalgak is not running")
    );
    // Headless means headless: the CLI never started an app (KIM-FR-012, DDALGAK-FR-060).
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["app"]["running"], false);
}

#[test]
fn startup_at_sign_in_uses_only_a_private_test_key() {
    let real_before = Registry::run_key().get().unwrap();
    let sb = Sandbox::new("startup");
    let (_, d) = sb.json(&["startup", "status"]);
    assert_eq!(d["data"]["enabled"], false);
    assert!(
        d["data"]["location"]
            .as_str()
            .unwrap()
            .contains("mouv-test"),
        "{d}"
    );
    let (_, d) = sb.json(&["startup", "enable", "--dry-run"]);
    assert_eq!(
        (
            d["data"]["dry_run"].as_bool(),
            d["data"]["changed"].as_bool()
        ),
        (Some(true), Some(true))
    );
    bless("startup-enable-dry-run", &d);
    let store = Registry::test_key(&sb.startup_key).unwrap();
    assert_eq!(store.get().unwrap(), None, "a dry run writes nothing");
    let (_, d) = sb.json(&["startup", "enable"]);
    assert_eq!(d["data"]["changed"], true);
    let written = store.get().unwrap().unwrap();
    assert!(
        written.ends_with("ddalgak-gui.exe\" --startup"),
        "{written}"
    );
    let (_, d) = sb.json(&["startup", "enable"]);
    assert_eq!(d["data"]["changed"], false, "idempotent");
    let (_, d) = sb.json(&["startup", "status"]);
    assert_eq!(
        (
            d["data"]["enabled"].as_bool(),
            d["data"]["foreign"].as_bool()
        ),
        (Some(true), Some(false))
    );
    bless("startup-status", &d);
    let (_, d) = sb.json(&["startup", "disable"]);
    assert_eq!(d["data"]["changed"], true);
    assert_eq!(store.get().unwrap(), None);
    // The real Run key was never touched.
    assert_eq!(Registry::run_key().get().unwrap(), real_before);
}

#[test]
fn the_app_lifecycle_through_the_cli() {
    let sb = Sandbox::new("lifecycle");
    sb.write_rules(&rules_doc("off"));
    sb.write_hotkeys(&hotkeys_doc());
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    let d = &st["data"];
    assert_eq!(d["app"]["test_mode"], true);
    assert_eq!(d["app"]["tray"], false, "the test host shows no tray icon");
    assert_eq!(d["engine"]["state"], "off");
    assert_eq!(
        d["engine"]["off_reason"],
        if elevated() { "elevated" } else { "default" }
    );
    assert_eq!(
        d["rules"]["active_rules"],
        json!(["pause-key", "off-key", "physical-only"])
    );
    assert_eq!(d["rules"]["inactive"][0]["rule"], "caps-remap");
    assert_eq!(d["rules"]["inactive"][0]["code"], "step_unsupported");
    // Ddalgak's own shortcuts are registered with RegisterHotKey (obscure chords only; never
    // pressed by this test), unless another program owns them.
    let global = d["shortcuts"]["global"].as_array().unwrap();
    for (action, keys) in [
        ("engine.toggle", "Ctrl+Alt+Shift+F13"),
        ("engine.pause", "Ctrl+Alt+Shift+F14"),
    ] {
        let s = global.iter().find(|s| s["action"] == action).unwrap();
        assert_eq!(s["keys"], keys);
        assert!(
            ["registered", "taken"].contains(&s["registration"].as_str().unwrap()),
            "{s}"
        );
    }
    assert_eq!(
        global
            .iter()
            .find(|s| s["action"] == "editor.open")
            .unwrap()["registration"],
        "unavailable"
    );
    // Privacy: status names rule IDs and codes, never rule titles or keys of rules.
    assert!(!st.to_string().contains("CapsLock acts as Ctrl"));
    bless("status-off", &st);
    // A second instance for the same session (here: endpoint) exits without touching the first.
    let mut second = sb.spawn_host();
    let until = Instant::now() + Duration::from_secs(10);
    let code = loop {
        if let Some(s) = second.try_wait().unwrap() {
            break s.code();
        }
        assert!(Instant::now() < until, "the second instance did not exit");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(7), "app_already_running");
    let mut err = String::new();
    std::io::Read::read_to_string(second.stderr.as_mut().unwrap(), &mut err).unwrap();
    assert!(err.contains("app_already_running"), "{err}");
    // Refusals and dry runs.
    let (code, d) = sb.json(&["engine", "pause"]);
    assert_eq!((code, d["error"]["code"].as_str()), (7, Some("engine_off")));
    bless("error-engine-off", &d);
    let (code, d) = sb.json(&["engine", "start", "--dry-run"]);
    if elevated() {
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (5, Some("engine_elevated"))
        );
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        let (_, q) = sb.json(&["quit"]);
        assert_eq!(q["data"]["quitting"], true);
        assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
        return;
    }
    assert_eq!(
        d["data"]["would"],
        json!({ "request": "start", "from": "off", "to": "running", "changed": true })
    );
    bless("engine-start-dry-run", &d);
    assert_eq!(
        sb.json(&["status"]).1["data"]["engine"]["state"],
        "off",
        "a dry run changes nothing"
    );
    let marker = sb.local().join("engine.running");
    assert!(!marker.exists());
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["change"]["to"], "running");
    assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    assert_eq!(d["data"]["engine"]["hook"]["observe_only"], true);
    bless("engine-start", &d);
    assert!(
        marker.exists(),
        "the run marker exists while the engine is on"
    );
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["change"]["changed"], false, "idempotent");
    let (_, d) = sb.json(&["pause"]);
    assert_eq!(d["data"]["change"]["to"], "paused");
    assert_eq!(
        d["data"]["engine"]["hook"]["installed"], true,
        "pause keeps the hook"
    );
    let (_, d) = sb.json(&["engine", "resume"]);
    assert_eq!(d["data"]["change"]["to"], "running");
    let (_, d) = sb.json(&["health"]);
    let h = &d["data"];
    assert_eq!(h["engine"]["hook"]["thread_alive"], true);
    assert_eq!(h["app"]["verified"], true);
    assert_eq!(
        h["counters"]["consumed"], 0,
        "the test host never consumes a key"
    );
    assert!(
        h["problems"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["code"] != "hook_thread_ended")
    );
    bless("health", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(
        st["data"]["engine"]["last_change"],
        json!({ "request": "resume", "origin": "cli" })
    );
    bless("status-running", &st);
    let (_, d) = sb.json(&["engine", "stop"]);
    assert_eq!(d["data"]["change"]["to"], "off");
    assert_eq!(d["data"]["engine"]["hook"]["installed"], false);
    assert_eq!(d["data"]["engine"]["off_reason"], "stopped");
    assert!(!marker.exists(), "a clean stop removes the run marker");
    let (_, d) = sb.json(&["quit", "--dry-run"]);
    assert_eq!(d["data"]["quitting"], false);
    let (_, d) = sb.json(&["quit"]);
    assert_eq!(d["data"]["quitting"], true);
    bless("quit", &d);
    assert_eq!(
        host.wait_exit(Duration::from_secs(10)),
        Some(0),
        "the app ends cleanly"
    );
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["app"]["running"], false);
}

#[test]
fn start_policy_markers_and_invalid_rules_keep_the_engine_off() {
    let sb = Sandbox::new("policy");
    sb.write_rules(&rules_doc("on"));
    std::fs::create_dir_all(sb.local()).unwrap();
    // A previous run that did not stop cleanly: the engine stays off despite engine_on_start.
    std::fs::write(sb.local().join("engine.running"), "2026-11-04T00:00:00Z\n").unwrap();
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "off");
    let expected = if elevated() {
        "elevated"
    } else {
        "unclean_exit"
    };
    assert_eq!(st["data"]["start"]["off_reason"], expected);
    bless("status-unclean-exit", &st);
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
    drop(host);
    if !elevated() {
        // A clean start with engine_on_start: on turns the engine on (and only then).
        let mut host = sb.host();
        let (_, st) = sb.json(&["status"]);
        assert_eq!(st["data"]["engine"]["state"], "running");
        assert_eq!(st["data"]["engine"]["last_change"]["origin"], "startup");
        sb.json(&["quit"]);
        assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
        assert!(!sb.local().join("engine.running").exists());
    }
    // The last stop was an emergency: off, and it says why.
    std::fs::write(
        sb.local().join("engine.emergency"),
        "2026-11-04T00:00:00Z\n",
    )
    .unwrap();
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "off");
    assert_eq!(
        st["data"]["start"]["off_reason"],
        if elevated() { "elevated" } else { "emergency" }
    );
    // An invalid rules file never turns the engine on and is never rewritten.
    std::fs::write(sb.config().join("rules.json"), b"{ broken").unwrap();
    let (code, d) = sb.json(&["engine", "start"]);
    if elevated() {
        assert_eq!(code, 5);
    } else {
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (4, Some("rules_invalid"))
        );
        bless("error-rules-invalid", &d);
    }
    assert_eq!(
        std::fs::read(sb.config().join("rules.json")).unwrap(),
        b"{ broken"
    );
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

#[test]
fn a_pipe_squatter_is_refused_before_anything_is_sent() {
    let sb = Sandbox::new("squat");
    // This test process (not ddalgak-gui.exe) serves the endpoint.
    let mut l = ddalgak::win::pipe::Listener::bind(&sb.endpoint).unwrap();
    let server = std::thread::spawn(move || {
        let conn = l.accept().unwrap().unwrap();
        ddalgak::win::pipe::read_line(&conn, 4096, Instant::now() + Duration::from_secs(3))
    });
    let (code, d) = sb.json(&["status"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (4, Some("app_identity_mismatch"))
    );
    bless("error-app-identity-mismatch", &d);
    let received = server.join().unwrap();
    assert!(
        received.is_err(),
        "the CLI sent nothing to the squatter: {received:?}"
    );
}

#[test]
fn injected_input_reaches_only_accepting_rules_in_the_real_app() {
    if !ddalgak::win::input_desktop_available() {
        eprintln!(
            "skipped (locked): no interactive input desktop (workstation locked, secure desktop \
             or a non-interactive session); SendInput and hooks cannot be exercised"
        );
        return;
    }
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        return;
    }
    let sb = Sandbox::new("inject");
    sb.write_rules(&rules_doc("off"));
    let mut host = sb.host();
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running");
    // Injected as another program would (no Ddalgak marker), obscure keys only, always released.
    let tap = |vk: u16| {
        ddalgak::win::inject::send_as_other_program(&[
            OutKey { vk, up: false },
            OutKey { vk, up: true },
        ])
    };
    let wait_state = |want: &str| {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let (_, st) = sb.json(&["status"]);
            if st["data"]["engine"]["state"] == want {
                return st;
            }
            assert!(Instant::now() < until, "engine never became {want}: {st:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    // The desktop can lock between the check and any call, and UIPI refuses input while an
    // elevated window has focus: a refused SendInput ends the test with what was verified so far.
    let refused = |step: &str| {
        eprintln!(
            "skipped (SendInput refused) at `{step}`: locked desktop or an elevated window has focus"
        );
    };
    let mut verified = Vec::new();
    'steps: {
        if tap(VK_F13) != 2 {
            refused("F13 pauses");
            break 'steps;
        }
        let st = wait_state("paused");
        let activity = st["data"]["activity"].as_array().unwrap();
        assert!(
            activity
                .iter()
                .any(|a| a["rule"] == "pause-key" && a["outcome"] == "ok"),
            "{activity:?}"
        );
        assert_eq!(
            st["data"]["engine"]["last_change"]["origin"],
            "rule:pause-key"
        );
        bless("status-paused-by-rule", &st);
        verified.push("an injected F13 reached the accepting rule; the real app paused");
        if tap(VK_F13) != 2 {
            refused("F13 resumes while paused");
            break 'steps;
        }
        wait_state("running");
        verified.push("engine-control rules keep working while paused");
        if tap(VK_F14) != 2 {
            refused("F14 turns the engine off");
            break 'steps;
        }
        let st = wait_state("off");
        assert_eq!(st["data"]["engine"]["hook"]["installed"], false);
        assert_eq!(st["data"]["engine"]["off_reason"], "stopped");
        verified.push("a rule turned the engine off and the hook was removed");
    }
    eprintln!("verified with real SendInput: {verified:?}");
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

// ---------------------------------------------------------------------------------------------
// Step 67: the rule store and the rule commands.
// ---------------------------------------------------------------------------------------------

const VK_F17: u16 = 0x80;

impl Sandbox {
    fn rules_path(&self) -> PathBuf {
        self.config().join("rules.json")
    }

    fn rules_bytes(&self) -> Vec<u8> {
        std::fs::read(self.rules_path()).unwrap_or_default()
    }

    /// `ddalgak <args> --json --endpoint <mine>` without waiting (for concurrent runs).
    fn spawn_cli(&self, args: &[&str], extra_env: &[(&str, &std::ffi::OsStr)]) -> Child {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ddalgak"));
        c.args(args)
            .args(["--json", "--endpoint", &self.endpoint])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            c.env(k, v);
        }
        self.env(&mut c);
        c.spawn().unwrap()
    }

    fn rule_ids(&self) -> Vec<String> {
        let (_, d) = self.json(&["rule", "list"]);
        d["data"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }
}

fn add_engine(sb: &Sandbox, id: &str, keys: &str, op: &str) -> Value {
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        keys,
        "--action",
        "engine",
        "--op",
        op,
        "--id",
        id,
        "--accept-injected",
    ]);
    assert_eq!(code, 0, "{d:#}");
    d
}

fn details(d: &Value) -> String {
    d["error"]["details"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[test]
fn rules_are_managed_through_the_cli_without_the_app() {
    let sb = Sandbox::new("crud");
    let (_, d) = sb.json(&["rule", "list"]);
    assert_eq!(
        (d["data"]["file"].as_str(), d["data"]["revision"].as_u64()),
        (Some("missing"), Some(0))
    );
    // Create with a generated, stable ID.
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "ctrl+shift+f13",
        "--action",
        "engine",
        "--op",
        "toggle_pause",
        "--title",
        "Pause or resume",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let pause = d["data"]["created"].as_str().unwrap().to_string();
    assert!(pause.starts_with("r-") && pause.len() == 38, "{pause}");
    assert_eq!(d["data"]["revision"], json!({ "from": 0, "to": 1 }));
    assert_eq!(
        d["data"]["app"],
        json!({ "running": false }),
        "no app: simply saved"
    );
    bless("rule-add", &d);
    // A dry run checks everything and writes nothing.
    let before = sb.rules_bytes();
    let (_, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "F14",
        "--action",
        "engine",
        "--op",
        "disable",
        "--id",
        "off-key",
        "--dry-run",
    ]);
    assert_eq!(
        (
            d["data"]["dry_run"].as_bool(),
            d["data"]["revision"]["to"].is_null()
        ),
        (Some(true), true)
    );
    assert_eq!(sb.rules_bytes(), before, "a dry run writes nothing");
    bless("rule-add-dry-run", &d);
    add_engine(&sb, "off-key", "F14", "disable");
    // A trigger another enabled rule has is refused, with offers, and nothing is written.
    let before = sb.rules_bytes();
    let (code, d) = sb.json(&[
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
        (7, Some("rule_conflict")),
        "{d:#}"
    );
    assert!(
        d["error"]["message"].as_str().unwrap().contains(&pause),
        "{d:#}"
    );
    assert!(
        details(&d).contains("--trigger Ctrl+Alt+Shift+F13"),
        "{d:#}"
    );
    assert_eq!(sb.rules_bytes(), before);
    bless("error-rule-conflict", &d);
    // Reserved and invalid triggers.
    let (code, d) = sb.json(&["rule", "add", "--trigger", "Alt+Tab", "--action", "block"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (2, Some("hotkey_reserved"))
    );
    bless("error-hotkey-reserved", &d);
    let (code, d) = sb.json(&["rule", "add", "--trigger", "Hyper+Q", "--action", "block"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (2, Some("hotkey_invalid"))
    );
    assert_eq!(sb.rules_bytes(), before);
    // A rule that needs consent is saved disabled and the command says which --allow it needs.
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "CapsLock",
        "--action",
        "remap",
        "--to",
        "lctrl",
        "--id",
        "caps",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("consent_required")),
        "{d:#}"
    );
    assert!(
        d["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("--allow input-injection")
    );
    bless("error-consent-required", &d);
    let (_, d) = sb.json(&["rule", "show", "caps"]);
    assert_eq!(
        (
            d["data"]["rule"]["enabled"].as_bool(),
            d["data"]["state"].as_str()
        ),
        (Some(false), Some("disabled"))
    );
    let (code, d) = sb.json(&["rule", "enable", "caps"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("consent_required"))
    );
    let (code, d) = sb.json(&["rule", "enable", "caps", "--allow", "input-injection"]);
    assert_eq!(code, 0, "{d:#}");
    let (_, d) = sb.json(&["rule", "show", "caps"]);
    assert_eq!(
        d["data"]["rule"]["consent"]["input-injection"]["via"],
        "cli"
    );
    // Remaps run from Step 68: valid, enabled, and honestly inactive until then.
    assert_eq!(
        (d["data"]["state"].as_str(), d["data"]["code"].as_str()),
        (Some("inactive"), Some("step_unsupported"))
    );
    bless("rule-show", &d);
    let (_, d) = sb.json(&["rule", "list"]);
    let states: Vec<(&str, &str)> = d["data"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["id"].as_str().unwrap(), r["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        states,
        [
            (pause.as_str(), "active"),
            ("off-key", "active"),
            ("caps", "inactive")
        ]
    );
    bless("rule-list", &d);
    let (_, d) = sb.json(&["rule", "check"]);
    assert_eq!(
        (
            d["data"]["ok"].as_bool(),
            d["data"]["summary"]["errors"].as_u64()
        ),
        (Some(true), Some(0))
    );
    bless("rule-check", &d);
    // Rename: the title changes, the ID never does.
    let (code, d) = sb.json(&[
        "rule",
        "edit",
        "off-key",
        "--title",
        "F14 turns the engine off",
    ]);
    assert_eq!(code, 0, "{d:#}");
    bless("rule-edit", &d);
    let (_, d) = sb.json(&["rule", "show", "off-key"]);
    assert_eq!(d["data"]["rule"]["title"], "F14 turns the engine off");
    assert_eq!(d["data"]["rule"]["id"], "off-key");
    let (code, d) = sb.json(&["rule", "edit", "off-key", "--patch", r#"{"id":"other"}"#]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (2, Some("usage")),
        "IDs never change"
    );
    // Unknown fields written by a newer Ddalgak survive edits (and keep the rule inactive).
    let mut file: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    file["profiles"][0]["rules"][1]["x_future"] = json!({ "kept": true });
    file["x_top_level"] = json!(1);
    std::fs::write(sb.rules_path(), serde_json::to_vec_pretty(&file).unwrap()).unwrap();
    let (_, d) = sb.json(&["rule", "show", "off-key"]);
    assert_eq!(d["data"]["code"], "rule_unsupported");
    sb.json(&["rule", "edit", "off-key", "--description", "kept fields"]);
    let file: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    assert_eq!(
        file["profiles"][0]["rules"][1]["x_future"],
        json!({ "kept": true })
    );
    assert_eq!(file["x_top_level"], 1);
    sb.json(&["rule", "edit", "off-key", "--patch", r#"{"x_future":null}"#]);
    assert_eq!(
        sb.json(&["rule", "show", "off-key"]).1["data"]["state"],
        "active"
    );
    // Duplicate (a disabled copy), enabling it collides, move, delete.
    let (_, d) = sb.json(&["rule", "duplicate", "off-key", "--id", "off-copy"]);
    assert_eq!(d["data"]["created"], "off-copy");
    let (code, _) = sb.json(&["rule", "enable", "off-copy"]);
    assert_eq!(code, 7, "the copy has the same trigger");
    let (code, d) = sb.json(&["rule", "edit", "off-copy", "--trigger", "Shift+F14"]);
    assert_eq!(code, 0, "{d:#}");
    let (code, _) = sb.json(&["rule", "enable", "off-copy"]);
    assert_eq!(code, 0);
    let (_, d) = sb.json(&["rule", "move", "off-copy", "--to", "start"]);
    assert_eq!(d["data"]["changed"], true);
    assert_eq!(sb.rule_ids()[0], "off-copy");
    let (code, d) = sb.json(&["rule", "delete", "off-copy"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    bless("error-confirmation-required", &d);
    let (_, d) = sb.json(&["rule", "delete", "off-copy", "--dry-run"]);
    assert_eq!(d["data"]["dry_run"], true);
    assert!(sb.rule_ids().contains(&"off-copy".to_string()));
    let (code, d) = sb.json(&["rule", "delete", "off-copy", "--yes"]);
    assert_eq!(code, 0);
    bless("rule-delete", &d);
    assert!(!sb.rule_ids().contains(&"off-copy".to_string()));
    let (code, d) = sb.json(&["rule", "show", "off-copy"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (3, Some("rule_not_found"))
    );
    bless("error-rule-not-found", &d);
    // Revoking consent disables the rule; it stays.
    let (_, d) = sb.json(&["rule", "revoke", "caps", "input-injection"]);
    assert_eq!(d["data"]["changed"], true);
    assert_eq!(
        sb.json(&["rule", "show", "caps"]).1["data"]["state"],
        "disabled"
    );
    // A writer that read an older revision is refused.
    let rev = sb.json(&["rule", "list"]).1["data"]["revision"]
        .as_u64()
        .unwrap();
    let (code, d) = sb.json(&[
        "rule",
        "disable",
        "off-key",
        "--if-revision",
        &(rev - 1).to_string(),
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("rule_conflict"))
    );
    // Draft: changes that are neither saved nor active.
    let saved_before = sb.rules_bytes();
    let (_, d) = sb.json(&["rule", "disable", "off-key", "--draft"]);
    assert_eq!(
        (d["data"]["target"].as_str(), d["data"]["app"].is_null()),
        (Some("draft"), true)
    );
    assert_eq!(
        sb.rules_bytes(),
        saved_before,
        "a draft leaves rules.json alone"
    );
    let (_, st) = sb.json(&["status"]);
    assert_eq!(
        st["data"]["rules"]["draft"],
        json!({ "base_revision": rev, "stale": false })
    );
    bless("status-not-running-with-draft", &st);
    let (_, d) = sb.json(&["rules", "draft"]);
    assert_eq!(d["data"]["changes"]["changed"], json!(["off-key"]));
    bless("rules-draft", &d);
    assert_eq!(
        sb.json(&["rule", "show", "off-key", "--draft"]).1["data"]["state"],
        "disabled"
    );
    assert_eq!(
        sb.json(&["rule", "show", "off-key"]).1["data"]["state"],
        "active"
    );
    let (code, d) = sb.json(&["rules", "draft", "apply"]);
    assert_eq!(code, 0, "{d:#}");
    assert_eq!(d["data"]["revision"]["to"], rev + 1);
    bless("rules-draft-apply", &d);
    assert_eq!(
        sb.json(&["rule", "show", "off-key"]).1["data"]["state"],
        "disabled"
    );
    sb.json(&["rule", "enable", "off-key", "--draft"]);
    let (code, d) = sb.json(&["rules", "draft", "discard", "--yes"]);
    assert_eq!((code, d["data"]["discarded"].as_bool()), (0, Some(true)));
    bless("rules-draft-discard", &d);
    // History, backup and restore.
    let (_, d) = sb.json(&["rules", "history"]);
    let snaps = d["data"]["snapshots"].as_array().unwrap();
    assert!(snaps.len() >= 10, "{d:#}");
    assert_eq!(d["data"]["backup"]["exists"], true);
    bless("rules-history", &d);
    let (_, d) = sb.json(&["rules", "backup"]);
    assert!(
        d["data"]["snapshot"]
            .as_str()
            .unwrap()
            .ends_with("-backup.json")
    );
    bless("rules-backup", &d);
    let (code, d) = sb.json(&["rules", "restore", "--backup"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, d) = sb.json(&["rules", "restore", "--backup", "--yes"]);
    assert_eq!(code, 0, "{d:#}");
    assert_eq!(d["data"]["kept"], "rules.json.bak");
    bless("rules-restore", &d);
    assert_eq!(
        sb.json(&["rule", "show", "off-key"]).1["data"]["state"],
        "active",
        "the version before"
    );
    // Headless means headless: no app was started by any of this.
    assert_eq!(sb.json(&["status"]).1["data"]["app"]["running"], false);
}

#[test]
fn two_clis_editing_at_once_never_lose_a_change() {
    let sb = Sandbox::new("concurrent");
    let keys = ["F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20"];
    let children: Vec<(String, Child)> = keys
        .iter()
        .map(|k| {
            let id = format!("c-{}", k.to_lowercase());
            let trigger = format!("Ctrl+Shift+{k}");
            let c = sb.spawn_cli(
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
                &[],
            );
            (id, c)
        })
        .collect();
    for (id, c) in children {
        let out = c.wait_with_output().unwrap();
        let d: Value = serde_json::from_slice(&out.stdout).unwrap();
        validate(&d);
        assert_eq!(out.status.code(), Some(0), "{id}: {d:#}");
    }
    let (_, d) = sb.json(&["rule", "list"]);
    assert_eq!(d["data"]["revision"], 8, "eight writes, eight revisions");
    let ids: std::collections::BTreeSet<String> = d["data"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 8, "no lost update: {ids:?}");
    assert_eq!(
        sb.json(&["rules", "history"]).1["data"]["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        8
    );
    // Several CLIs renaming the same rule at once: every edit is applied, one after the other.
    let titles: Vec<String> = (0..6).map(|i| format!("title {i}")).collect();
    let edits: Vec<Child> = titles
        .iter()
        .map(|t| sb.spawn_cli(&["rule", "edit", "c-f13", "--title", t], &[]))
        .collect();
    for c in edits {
        let out = c.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    let (_, d) = sb.json(&["rule", "show", "c-f13"]);
    assert_eq!(d["data"]["revision"], 14);
    assert!(titles.contains(&d["data"]["rule"]["title"].as_str().unwrap().to_string()));
    // A long-held lock makes a writer give up with `busy`, without writing anything.
    let files = ddalgak::store::Files {
        rules: sb.rules_path(),
    };
    let held = ddalgak::store::lock(&files, Duration::from_secs(1)).unwrap();
    let before = sb.rules_bytes();
    let out = sb
        .spawn_cli(
            &["rule", "disable", "c-f14"],
            &[("DDALGAK_TEST_LOCK_TIMEOUT_MS", std::ffi::OsStr::new("300"))],
        )
        .wait_with_output()
        .unwrap();
    let d: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (out.status.code(), d["error"]["code"].as_str()),
        (Some(7), Some("busy")),
        "{d:#}"
    );
    bless("error-busy", &d);
    drop(held);
    assert_eq!(sb.rules_bytes(), before);
}

#[test]
fn a_crash_in_the_middle_of_a_save_leaves_a_valid_file() {
    let sb = Sandbox::new("crash");
    add_engine(&sb, "before-crash", "F13", "pause");
    let before = sb.rules_bytes();
    let marker = sb.root.join("in-the-middle.marker");
    let mut writer = sb.spawn_cli(
        &[
            "rule",
            "add",
            "--trigger",
            "F14",
            "--action",
            "engine",
            "--op",
            "pause",
            "--id",
            "lost",
        ],
        &[("DDALGAK_TEST_CRASH_IN_SAVE", marker.as_os_str())],
    );
    let until = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        if let Ok(Some(s)) = writer.try_wait() {
            panic!("the writer ended before the crash point: {s}");
        }
        assert!(
            Instant::now() < until,
            "the writer never reached the crash point"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Kill the writer half-way through writing the new file.
    writer.kill().unwrap();
    let _ = writer.wait();
    let tmp = sb.config().join("rules.json.tmp");
    assert!(
        tmp.exists(),
        "the interrupted write left its temporary file"
    );
    assert!(
        serde_json::from_slice::<Value>(&std::fs::read(&tmp).unwrap()).is_err(),
        "the temporary file is torn"
    );
    assert_eq!(
        sb.rules_bytes(),
        before,
        "rules.json is exactly the previous version"
    );
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["rules"]["saved"]["interrupted_save"], true);
    assert_eq!(st["data"]["rules"]["file"], "loaded");
    assert_eq!(sb.rule_ids(), ["before-crash"]);
    // The OS released the dead writer's lock; the next save works and replaces the leftover.
    add_engine(&sb, "after-crash", "F15", "pause");
    assert!(!tmp.exists());
    assert_eq!(sb.rule_ids(), ["before-crash", "after-crash"]);
    let (_, d) = sb.json(&["rule", "check"]);
    assert_eq!(d["data"]["ok"], true);
}

#[test]
fn a_corrupt_file_is_safe_mode_kept_and_restorable() {
    let sb = Sandbox::new("corrupt");
    add_engine(&sb, "first", "F13", "pause");
    add_engine(&sb, "second", "F14", "pause");
    let garbage = br#"{ "schema": "ddalgak.rules", "schema_version": 1, "profiles": [ broken"#;
    std::fs::write(sb.rules_path(), garbage).unwrap();
    let (code, d) = sb.json(&["rule", "list"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (4, Some("rules_invalid"))
    );
    assert!(
        d["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("rules restore --backup")
    );
    bless("error-rule-list-rules-invalid", &d);
    let (code, _) = sb.json(&["rule", "add", "--trigger", "F15", "--action", "block"]);
    assert_eq!(code, 4);
    assert_eq!(sb.rules_bytes(), garbage, "never rewritten automatically");
    let (code, d) = sb.json(&["rule", "check"]);
    assert_eq!(code, 0);
    assert_eq!(
        (d["data"]["file"].as_str(), d["data"]["ok"].as_bool()),
        (Some("invalid"), Some(false))
    );
    bless("rule-check-invalid-file", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["rules"]["saved"]["file"], "invalid");
    let (code, d) = sb.json(&["rules", "restore", "--backup", "--yes"]);
    assert_eq!(code, 0, "{d:#}");
    let kept = d["data"]["kept"].as_str().unwrap();
    assert!(kept.starts_with("rules.corrupt-"), "{kept}");
    assert_eq!(
        std::fs::read(sb.config().join(kept)).unwrap(),
        garbage,
        "the bad file is kept"
    );
    assert_eq!(
        sb.rule_ids(),
        ["first"],
        "the backup was the version before the last change"
    );
}

/// Denies the current user the right to add files to `dir` (icacls, no elevation) until dropped.
struct DenyWrite {
    dir: PathBuf,
    sid: String,
}

impl DenyWrite {
    fn new(dir: &Path) -> DenyWrite {
        let sid = ddalgak::win::user_sid().unwrap();
        let ok = Command::new("icacls")
            .arg(dir)
            .args(["/deny", &format!("*{sid}:(WD,AD)")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap()
            .success();
        assert!(ok, "icacls could not add the deny entry");
        DenyWrite {
            dir: dir.to_path_buf(),
            sid,
        }
    }
}

impl Drop for DenyWrite {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.dir)
            .args(["/remove:d", &format!("*{}", self.sid)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

#[test]
fn read_only_files_and_folders_are_refused_and_left_alone() {
    let sb = Sandbox::new("readonly");
    add_engine(&sb, "first", "F13", "pause");
    let before = sb.rules_bytes();
    let path = sb.rules_path();
    let mut p = std::fs::metadata(&path).unwrap().permissions();
    p.set_readonly(true);
    std::fs::set_permissions(&path, p.clone()).unwrap();
    let (code, d) = sb.json(&["rule", "add", "--trigger", "F14", "--action", "block"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (1, Some("permission_denied")),
        "{d:#}"
    );
    bless("error-permission-denied", &d);
    assert_eq!(sb.rules_bytes(), before);
    #[allow(clippy::permissions_set_readonly_false)]
    p.set_readonly(false);
    std::fs::set_permissions(&path, p).unwrap();
    // A folder the user may not add files to.
    {
        let _deny = DenyWrite::new(&sb.config());
        let (code, d) = sb.json(&["rule", "add", "--trigger", "F14", "--action", "block"]);
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (1, Some("permission_denied")),
            "{d:#}"
        );
        assert_eq!(sb.rules_bytes(), before);
        assert!(!sb.config().join("rules.json.tmp").exists());
    }
    let (code, _) = sb.json(&["rule", "add", "--trigger", "F14", "--action", "block"]);
    assert_eq!(code, 0, "writable again");
}

#[test]
fn a_running_app_reloads_valid_rules_and_keeps_its_hook() {
    let sb = Sandbox::new("reload");
    // Physical-input rules only on F18/F19: the parallel injection test of Step 66 injects F13
    // and F14 into the whole session and must not reach this app's rules.
    sb.write_rules(&json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "default",
        "profiles": [ { "id": "default", "title": "Test", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" } ],
            "rules": [
                { "id": "base-pause", "title": "F18 pauses", "enabled": true,
                  "trigger": { "keys": "F18" }, "steps": [ { "type": "engine", "op": "pause" } ] },
                { "id": "base-off", "title": "F19 turns off", "enabled": true,
                  "trigger": { "keys": "F19" }, "steps": [ { "type": "engine", "op": "disable" } ] }
            ] } ]
    }));
    let mut host = sb.host();
    let hook_on = !elevated();
    if hook_on {
        let (_, d) = sb.json(&["engine", "start"]);
        assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    } else {
        eprintln!("elevated: the engine stays off; the reload is tested with the engine off");
    }
    let hook_ok = |st: &Value| {
        if hook_on {
            assert_eq!(st["data"]["engine"]["state"], "running", "{st:#}");
            assert_eq!(st["data"]["engine"]["hook"]["installed"], true, "{st:#}");
        }
    };
    // A saved change is applied by the running app at once.
    let (_, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "F16",
        "--action",
        "engine",
        "--op",
        "pause",
        "--id",
        "new-key",
        "--accept-injected",
    ]);
    assert_eq!(d["data"]["app"]["running"], true, "{d:#}");
    assert_eq!(d["data"]["app"]["in_sync"], true, "{d:#}");
    assert_eq!(d["data"]["app"]["active_revision"], 2);
    bless("rule-add-app-running", &d);
    let (_, st) = sb.json(&["status"]);
    let r = &st["data"]["rules"];
    assert!(
        r["active_rules"]
            .as_array()
            .unwrap()
            .contains(&json!("new-key")),
        "{st:#}"
    );
    assert_eq!(
        (r["active_set"]["revision"].as_u64(), r["in_sync"].as_bool()),
        (Some(2), Some(true))
    );
    assert_eq!(r["last_reload"]["outcome"], "applied");
    hook_ok(&st);
    bless("status-after-reload", &st);
    let (_, h) = sb.json(&["health"]);
    if hook_on {
        assert_eq!(h["data"]["engine"]["hook"]["thread_alive"], true);
    }
    assert!(
        h["data"]["problems"].as_array().unwrap().is_empty(),
        "{h:#}"
    );
    // A broken file (written behind Ddalgak's back) keeps the previous active rules.
    std::fs::write(sb.rules_path(), b"{ not json").unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    let st = loop {
        let (_, st) = sb.json(&["status"]);
        if st["data"]["rules"]["saved"]["file"] == "invalid" {
            break st;
        }
        assert!(
            Instant::now() < until,
            "the app never noticed the change: {st:#}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let r = &st["data"]["rules"];
    assert!(
        r["active_rules"]
            .as_array()
            .unwrap()
            .contains(&json!("new-key")),
        "unrelated rules stay: {st:#}"
    );
    assert_eq!(
        (r["active_set"]["revision"].as_u64(), r["in_sync"].as_bool()),
        (Some(2), Some(false))
    );
    assert_eq!(r["last_reload"]["outcome"], "kept_previous");
    assert_eq!(r["last_reload"]["code"], "rules_invalid");
    hook_ok(&st);
    bless("status-kept-previous", &st);
    let (_, d) = sb.json(&["rules", "reload"]);
    assert_eq!(d["data"]["outcome"], "kept_previous");
    bless("rules-reload-kept-previous", &d);
    let (_, h) = sb.json(&["health"]);
    assert!(
        h["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "rules_invalid")
    );
    // Repair it from the backup: applied again, without restarting the app.
    let (code, d) = sb.json(&["rules", "restore", "--backup", "--yes"]);
    assert_eq!(code, 0, "{d:#}");
    assert_eq!(d["data"]["app"]["in_sync"], true, "{d:#}");
    let (_, d) = sb.json(&["rules", "reload"]);
    assert_eq!(
        (
            d["data"]["outcome"].as_str(),
            d["data"]["in_sync"].as_bool()
        ),
        (Some("unchanged"), Some(true))
    );
    bless("rules-reload", &d);
    let (_, d) = sb.json(&["rules", "reload", "--dry-run"]);
    assert_eq!(d["data"]["dry_run"], true);
    // With real input, when the desktop allows it: a rule added while the engine runs fires
    // through the swapped table in the same, still installed hook.
    if hook_on && ddalgak::win::input_desktop_available() {
        add_engine(&sb, "live-key", "F17", "toggle_pause");
        let tap = |vk: u16| {
            ddalgak::win::inject::send_as_other_program(&[
                OutKey { vk, up: false },
                OutKey { vk, up: true },
            ])
        };
        if tap(VK_F17) == 2 {
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                let (_, st) = sb.json(&["status"]);
                if st["data"]["engine"]["state"] == "paused" {
                    assert_eq!(
                        st["data"]["engine"]["last_change"]["origin"],
                        "rule:live-key"
                    );
                    eprintln!(
                        "verified with real SendInput: a reloaded rule fired in the running hook"
                    );
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "the reloaded rule did not fire: {st:#}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = tap(VK_F17);
        } else {
            eprintln!(
                "skipped (SendInput refused): locked desktop or an elevated window has focus"
            );
        }
    } else if hook_on {
        eprintln!("skipped (locked): no interactive input desktop for the live part");
    }
    if hook_on {
        let (_, d) = sb.json(&["engine", "stop"]);
        assert_eq!(d["data"]["engine"]["hook"]["installed"], false);
    }
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}
