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
        let talks_to_app = ["status", "health", "engine", "pause", "resume", "quit"]
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
