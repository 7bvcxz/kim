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
//!
//! Step 75: every real-input step whose result a person's input on this desktop can change (a
//! held modifier keeps an exact trigger such as `F13` from matching) runs as attempts
//! (`common/quiet.rs`), judged with the test host's own count of physical events (`health`
//! counters): only an attempt without physical input may fail the test, a disturbed one is
//! retried and reported.
#![cfg(windows)]

#[path = "common/quiet.rs"]
mod quiet;

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
            "status", "health", "engine", "pause", "resume", "quit", "rule", "rules", "mode",
            "enable", "disable", "release",
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
        self.host_with(&[])
    }

    /// Starts the debug test host with extra options (`--test-launch-root <dir>`).
    fn host_with(&self, extra: &[&str]) -> Host {
        let turn = turn();
        let child = self.spawn_host_with(extra);
        let mut h = Host { child, _turn: turn };
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
        self.spawn_host_with(&[])
    }

    fn spawn_host_with(&self, extra: &[&str]) -> Child {
        let mut c = Command::new(env!("CARGO_BIN_EXE_ddalgak-gui"));
        c.args([
            "--test-host",
            "--test-max-seconds",
            "60",
            "--endpoint",
            &self.endpoint,
        ])
        .args(extra)
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

/// A started test host; killed (and waited for) on every path. It holds a real-input turn: every
/// test host's hook sees every key injected in the session, so hosts and injections of parallel
/// tests (and of the same tests run from another checkout) take turns (Step 73).
struct Host {
    child: Child,
    _turn: Turn,
}

std::thread_local! {
    static TURN_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A turn of a test that injects keys or runs a test host (see [`Host`]): this process's lock
/// and the session-wide mutex the hook tests use too. Nested turns of one thread are free.
struct Turn {
    _local: Option<std::sync::MutexGuard<'static, ()>>,
    session: windows_sys::Win32::Foundation::HANDLE,
}

impl Drop for Turn {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::ReleaseMutex;
        TURN_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        if !self.session.is_null() {
            // SAFETY: a mutex this thread owns (acquired in `turn`), released and closed once.
            unsafe {
                ReleaseMutex(self.session);
                CloseHandle(self.session);
            }
        }
    }
}

#[allow(unsafe_code)]
fn turn() -> Turn {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    let depth = TURN_DEPTH.with(|d| {
        let n = d.get();
        d.set(n + 1);
        n
    });
    if depth > 0 {
        return Turn {
            _local: None,
            session: std::ptr::null_mut(),
        };
    }
    let local = TURN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let name = ddalgak::win::wide("Local\\mouv-ddalgak-real-input-tests");
    // SAFETY: NUL-terminated name, default security, not initially owned.
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let mut session = std::ptr::null_mut();
    if !h.is_null() {
        // SAFETY: a valid mutex handle; waits at most three minutes for another checkout's test.
        match unsafe { WaitForSingleObject(h, 180_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => session = h,
            _ => {
                eprintln!("note: another real-input test kept its turn for 3 minutes; going ahead");
                // SAFETY: closes the handle opened above (not owned).
                unsafe { CloseHandle(h) };
            }
        }
    }
    Turn {
        _local: Some(local),
        session,
    }
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
                { "id": "f19-remap", "title": "F19 acts as F20 (an obscure remap)", "enabled": true,
                  "trigger": { "keys": "F19" },
                  "steps": [ { "type": "remap", "to": "F20" } ],
                  "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
                { "id": "delay-later", "title": "F18 waits (a later Step: ordered runs)", "enabled": true,
                  "trigger": { "keys": "F18" },
                  "steps": [ { "type": "delay", "ms": 100 } ] }
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

/// Taps an obscure key as another program would (no Ddalgak marker); `false` when `SendInput`
/// refused it.
fn tap_other(vk: u16) -> bool {
    ddalgak::win::inject::send_as_other_program(&[
        OutKey { vk, up: false },
        OutKey { vk, up: true },
    ]) == 2
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
        json!(["pause-key", "off-key", "physical-only", "f19-remap"]),
        "remaps run from Step 68"
    );
    assert_eq!(d["rules"]["inactive"][0]["rule"], "delay-later");
    assert_eq!(d["rules"]["inactive"][0]["code"], "step_unsupported");
    // Ddalgak's own shortcuts are registered with RegisterHotKey (obscure chords only; never
    // pressed by this test), unless another program owns them.
    let global = d["shortcuts"]["global"].as_array().unwrap();
    // Since Step 74 `editor.open` is registered too; the test host never opens a window for it.
    for (action, keys) in [
        ("engine.toggle", "Ctrl+Alt+Shift+F13"),
        ("engine.pause", "Ctrl+Alt+Shift+F14"),
        ("editor.open", "Ctrl+Alt+Shift+F15"),
    ] {
        let s = global.iter().find(|s| s["action"] == action).unwrap();
        assert_eq!(s["keys"], keys);
        assert!(
            ["registered", "taken"].contains(&s["registration"].as_str().unwrap()),
            "{s}"
        );
    }
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
    // The desktop can lock between the check and any call, and UIPI refuses input while an
    // elevated window has focus: a refused SendInput ends the test with what was verified so far.
    // F13 and F14 are exact triggers: a modifier a person holds keeps them from matching, so each
    // tap runs as attempts (Step 75).
    let state = |want: &'static str| move |d: &Value| d["engine"]["state"] == want;
    let mut verified = Vec::new();
    'steps: {
        let Some(st) = sb
            .tap_until("F13 pauses", VK_F13, state("paused"))
            .verified("F13 pauses")
        else {
            break 'steps;
        };
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
        if sb
            .tap_until("F13 resumes while paused", VK_F13, state("running"))
            .verified("F13 resumes while paused")
            .is_none()
        {
            break 'steps;
        }
        verified.push("engine-control rules keep working while paused");
        let Some(st) = sb
            .tap_until("F14 turns the engine off", VK_F14, state("off"))
            .verified("F14 turns the engine off")
        else {
            break 'steps;
        };
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
    // Remaps run from Step 68: valid, enabled and active.
    assert_eq!(
        (d["data"]["state"].as_str(), d["data"]["code"].as_str()),
        (Some("active"), None)
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
            ("caps", "active")
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
fn the_mouse_hook_exists_only_while_a_mouse_button_rule_does() {
    // Step 68 (ADR 0031): the low-level mouse hook is the `mouse-hook` permission and is
    // installed only while the engine is on and an active rule has a mouse-button trigger. The
    // test host never consumes an event and never injects, so the user's mouse keeps working.
    let sb = Sandbox::new("mouse");
    sb.write_rules(&json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "default",
        "profiles": [ { "id": "default", "title": "Test", "default_mode": "base",
            "modes": [ { "id": "base", "title": "Base" } ],
            "rules": [
                { "id": "f20-pause", "title": "F20 pauses", "enabled": true,
                  "trigger": { "keys": "F20" }, "steps": [ { "type": "engine", "op": "pause" } ] }
            ] } ]
    }));
    let _host = sb.host();
    if elevated() {
        eprintln!("elevated: the engine stays off, so neither hook is installed; nothing to test");
        return;
    }
    let hooks = |sb: &Sandbox| {
        let (_, st) = sb.json(&["status"]);
        (
            st["data"]["engine"]["hook"]["installed"] == true,
            st["data"]["engine"]["hook"]["mouse_installed"] == true,
        )
    };
    let wait_for = |sb: &Sandbox, want: (bool, bool)| {
        let until = Instant::now() + Duration::from_secs(10);
        while hooks(sb) != want {
            assert!(Instant::now() < until, "hooks never became {want:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    assert_eq!(
        d["data"]["engine"]["hook"]["mouse_installed"], false,
        "no mouse rule: no mouse hook"
    );
    // A mouse-button rule saved while the engine runs: the reload installs the mouse hook.
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "MouseX2",
        "--action",
        "block",
        "--id",
        "no-x2",
    ]);
    assert_eq!(code, 0, "{d:#}");
    wait_for(&sb, (true, true));
    let (_, h) = sb.json(&["health"]);
    assert_eq!(h["data"]["engine"]["hook"]["mouse_installed"], true);
    assert!(
        h["data"]["problems"].as_array().unwrap().is_empty(),
        "{h:#}"
    );
    // Disabled: the mouse hook goes away, the keyboard hook stays.
    let (code, _) = sb.json(&["rule", "disable", "no-x2"]);
    assert_eq!(code, 0);
    wait_for(&sb, (true, false));
    // Enabled again, then the engine is stopped: both hooks are removed.
    sb.json(&["rule", "enable", "no-x2"]);
    wait_for(&sb, (true, true));
    let (_, d) = sb.json(&["engine", "stop"]);
    assert_eq!(d["data"]["engine"]["hook"]["installed"], false);
    assert_eq!(d["data"]["engine"]["hook"]["mouse_installed"], false);
    // Started with the mouse rule already there: installed at once.
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(
        d["data"]["engine"]["hook"]["mouse_installed"], true,
        "{d:#}"
    );
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
        let paused = sb.tap_until("the reloaded rule fires", VK_F17, |d| {
            d["engine"]["state"] == "paused"
        });
        if let Some(st) = paused.verified("the reloaded rule fires") {
            assert_eq!(
                st["data"]["engine"]["last_change"]["origin"],
                "rule:live-key"
            );
            eprintln!("verified with real SendInput: a reloaded rule fired in the running hook");
            let _ = tap_other(VK_F17);
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

/// Step 69: typing rules and hotstrings through the real CLI (and loaded by the real test host,
/// which injects nothing). Snippet text never appears in any output unless `--show-text` asks.
#[test]
fn typing_rules_and_hotstrings_never_put_the_text_in_any_output() {
    const SECRET: &str = "SECRET-감사합니다";
    let sb = Sandbox::new("typing");
    let no_secret = |what: &str, d: &Value| {
        let s = d.to_string();
        assert!(
            !s.contains("SECRET") && !s.contains("감사합니다"),
            "{what} must not contain the snippet: {s}"
        );
    };
    let text = format!("{SECRET}\\n둘째 줄\\t끝 \\u{{1F44D}}");
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "sign",
        "--trigger",
        "Ctrl+Shift+F2",
        "--action",
        "type_text",
        "--text",
        &text,
        "--escapes",
        "--allow",
        "input-injection",
    ]);
    assert_eq!(code, 0, "{d:#}");
    no_secret("rule add", &d);
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "btw",
        "--hotstring",
        "btw",
        "--action",
        "type_text",
        "--text",
        "by the way",
        "--allow",
        "input-injection",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "today",
        "--hotstring",
        ";dd",
        "--immediate",
        "--case",
        "sensitive",
        "--action",
        "type_text",
        "--text",
        "{date} {cursor}",
        "--variables",
        "--method",
        "paste",
        "--allow",
        "input-injection",
        "--allow",
        "clipboard",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let (code, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "ac",
        "--hotstring",
        ";ac",
        "--immediate",
        "--action",
        "send_keys",
        "--keys",
        "ctrl+a ctrl+c",
        "--allow",
        "input-injection",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let stored: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    let rules = &stored["profiles"][0]["rules"];
    assert_eq!(
        rules[0]["steps"][0]["text"],
        format!("{SECRET}\n둘째 줄\t끝 👍")
    );
    assert_eq!(
        rules[0]["title"],
        "Ctrl+Shift+F2: types 21 characters on 2 lines"
    );
    assert_eq!(
        rules[2]["trigger"],
        json!({ "hotstring": { "abbreviation": ";dd", "case": "sensitive", "immediate": true } })
    );
    assert_eq!(rules[2]["steps"][0]["variables"], true);
    assert_eq!(rules[3]["steps"][0]["keys"], "Ctrl+A Ctrl+C");
    // Refused, with nothing written and no text in the message.
    let before = sb.rules_bytes();
    let refusals: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "SECRET {clipboard}",
                "--variables",
            ],
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "SECRET\\u{7}",
                "--escapes",
            ],
            "rule_invalid",
        ),
        (
            vec![
                "--hotstring",
                "b t",
                "--action",
                "type_text",
                "--text",
                "SECRET",
            ],
            "hotkey_invalid",
        ),
        (
            vec![
                "--hotstring",
                "BTW",
                "--action",
                "type_text",
                "--text",
                "SECRET",
            ],
            "rule_conflict",
        ),
        (
            vec!["--hotstring", "x", "--action", "remap", "--to", "F16"],
            "rule_invalid",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--hotstring",
                "x",
                "--action",
                "type_text",
                "--text",
                "SECRET",
            ],
            "usage",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--case",
                "sensitive",
                "--action",
                "type_text",
                "--text",
                "SECRET",
            ],
            "usage",
        ),
        (
            vec![
                "--trigger",
                "F13",
                "--action",
                "type_text",
                "--text",
                "C:\\Users\\SECRET",
                "--escapes",
            ],
            "usage",
        ),
    ];
    for (args, want) in refusals {
        let mut a = vec!["rule", "add", "--allow", "input-injection"];
        a.extend(args.iter().copied());
        let (code, d) = sb.json(&a);
        assert_ne!(code, 0, "{a:?}");
        assert_eq!(d["error"]["code"], want, "{a:?}: {d:#}");
        if want != "usage" {
            no_secret("a refusal", &d);
        }
        assert_eq!(sb.rules_bytes(), before, "{a:?}: nothing written");
    }
    // Read commands: counts, IDs and triggers only.
    let (_, list) = sb.json(&["rule", "list"]);
    no_secret("rule list", &list);
    let rows = list["data"]["rules"].as_array().unwrap();
    assert_eq!(rows[1]["trigger"], "\"btw\" (hotstring)");
    assert_eq!(rows[0]["steps"], json!(["type_text"]));
    assert!(rows.iter().all(|r| r["state"] == "active"), "{rows:?}");
    let (_, show) = sb.json(&["rule", "show", "sign"]);
    no_secret("rule show", &show);
    assert_eq!(show["data"]["text_redacted"], true);
    assert_eq!(
        show["data"]["rule"]["steps"][0]["text"],
        json!({ "redacted": true, "preview": { "characters": 21, "lines": 2, "method": "unicode" } })
    );
    let (_, shown) = sb.json(&["rule", "show", "sign", "--show-text"]);
    assert_eq!(shown["data"]["text_redacted"], false);
    assert!(shown.to_string().contains("SECRET"), "explicitly asked for");
    for args in [
        vec!["rule", "check"],
        vec!["status"],
        vec!["rules", "history"],
        vec!["rules", "backup"],
    ] {
        let (_, d) = sb.json(&args);
        no_secret(&args.join(" "), &d);
    }
    // Human output of the read commands keeps the text out as well.
    for a in [vec!["rule", "list"], vec!["rule", "show", "sign"]] {
        let out = sb.run(&a);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains("SECRET"), "{a:?}: {text}");
    }
    // Edits switch between keys and a hotstring.
    let (code, d) = sb.json(&[
        "rule",
        "edit",
        "btw",
        "--hotstring",
        "bytheway",
        "--omit-end",
    ]);
    assert_eq!(code, 0, "{d:#}");
    let stored: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    assert_eq!(
        stored["profiles"][0]["rules"][1]["trigger"],
        json!({ "hotstring": { "abbreviation": "bytheway", "omit_end": true } })
    );
    let (code, d) = sb.json(&["rule", "edit", "btw", "--trigger", "F13"]);
    assert_eq!(code, 0, "{d:#}");
    let stored: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    assert_eq!(
        stored["profiles"][0]["rules"][1]["trigger"],
        json!({ "keys": "F13" })
    );
    // The real test host loads them all as active rules and injects nothing.
    let _host = sb.host();
    if elevated() {
        eprintln!("elevated: the engine stays off; the load is checked by status only");
    } else {
        let (_, d) = sb.json(&["engine", "start"]);
        assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    }
    let (_, st) = sb.json(&["status"]);
    no_secret("status with the app", &st);
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["sign", "btw", "today", "ac"]),
        "{st:#}"
    );
    assert_eq!(st["data"]["activity"], json!([]), "nothing ran");
}

// ---------------------------------------------------------------------------------------------
// Step 70: program and website launches.
// ---------------------------------------------------------------------------------------------

#[path = "common/marker.rs"]
mod marker;

const VK_F15: u16 = 0x7E;
const VK_F16: u16 = 0x7F;

/// The CLI writes launch rules (normalized, refused, consented, redacted), and the real debug
/// test host runs them: it starts only the test's own marker program from its launch folder,
/// hidden, and records website hand-offs instead of opening a browser.
#[test]
#[allow(clippy::too_many_lines)]
fn launch_rules_through_the_cli_and_the_real_test_host() {
    const SECRET: &str = "s3cret-토큰";
    let sb = Sandbox::new("launch");
    let built = marker::build(&sb.root.join("build"));
    let launch_root = sb.root.join("테스트 폴더 with spaces");
    std::fs::create_dir_all(&launch_root).unwrap();
    let exe = launch_root.join("마커 app.exe");
    std::fs::copy(&built, &exe).unwrap();
    let marker_file = launch_root.join("결과 marker.txt");
    let no_secret = |what: &str, d: &str| {
        assert!(
            !d.contains("s3cret") && !d.contains("토큰") && !d.contains("결과 marker"),
            "{what} must not show a launch target: {d}"
        );
    };
    // Refused with nothing written: other schemes, credentials, shell hosts, relative paths,
    // alternate data streams, bad variable names.
    for args in [
        ["--action", "open_url", "--url", "javascript:alert(1)"],
        [
            "--action",
            "open_url",
            "--url",
            "file:///C:/Windows/win.ini",
        ],
        ["--action", "open_url", "--url", "ms-settings:privacy"],
        ["--action", "open_url", "--url", "zoommtg://zoom.us/join"],
        [
            "--action",
            "open_url",
            "--url",
            "https://user:s3cret@example.com/",
        ],
        [
            "--action",
            "open_program",
            "--path",
            r"C:\Windows\System32\cmd.exe",
        ],
        ["--action", "open_program", "--path", "notepad.exe"],
        ["--action", "open_program", "--path", r"C:\notes.txt:x.exe"],
    ] {
        let mut a = vec![
            "rule",
            "add",
            "--trigger",
            "F20",
            "--allow",
            "launch-programs",
        ];
        a.extend(args);
        let (exit, d) = sb.json(&a);
        assert_eq!(exit, 2, "{a:?}: {d:#}");
        assert_eq!(d["error"]["code"], "usage", "{a:?}: {d:#}");
        no_secret("a refusal", &d.to_string());
    }
    let (exit, d) = sb.json(&[
        "rule",
        "add",
        "--trigger",
        "F20",
        "--action",
        "open_program",
        "--path",
        r"C:\x.exe",
        "--env",
        "1BAD=x",
    ]);
    assert_eq!(
        (exit, d["error"]["code"].as_str()),
        (2, Some("usage")),
        "{d:#}"
    );
    assert!(sb.rule_ids().is_empty(), "nothing was written");
    // A program with a quoted path (as "Copy as path" gives it), arguments and an override.
    let exe_arg = exe.display().to_string();
    let marker_arg = marker_file.display().to_string();
    let env_arg = format!("DDALGAK_TEST_VAR={SECRET}");
    let quoted = format!("\"{exe_arg}\"");
    let (exit, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "open-marker",
        "--trigger",
        "F15",
        "--accept-injected",
        "--action",
        "open_program",
        "--path",
        &quoted,
        "--arg",
        &marker_arg,
        "--arg",
        "첫 번째 인자",
        "--env",
        &env_arg,
        "--allow",
        "launch-programs",
    ]);
    assert_eq!(exit, 0, "{d:#}");
    no_secret("rule add (program)", &d.to_string());
    // A website typed without a scheme and with a token in its query.
    let typed = format!("Example.COM/reset?token={SECRET}");
    let (exit, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "open-site",
        "--trigger",
        "F16",
        "--accept-injected",
        "--action",
        "open_url",
        "--url",
        &typed,
        "--allow",
        "launch-programs",
    ]);
    assert_eq!(exit, 0, "{d:#}");
    no_secret("rule add (website)", &d.to_string());
    // Without --allow: saved disabled, consent_required.
    let (exit, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "no-consent",
        "--trigger",
        "F17",
        "--action",
        "open_url",
        "--url",
        "https://example.org/",
    ]);
    assert_eq!(
        (exit, d["error"]["code"].as_str()),
        (7, Some("consent_required")),
        "{d:#}"
    );
    // As stored: normalized, consent bound to exactly these targets.
    let doc: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    let rules = doc["profiles"][0]["rules"].as_array().unwrap();
    let rule = |id: &str| rules.iter().find(|r| r["id"] == id).unwrap().clone();
    let site = rule("open-site");
    let normalized = ddalgak::launch::normalize_url(&typed).unwrap();
    assert!(
        normalized.starts_with("https://example.com/reset?token=s3cret-%ED"),
        "{normalized}"
    );
    assert_eq!(site["steps"][0]["url"], normalized.as_str());
    assert_eq!(
        site["consent"]["launch-programs"]["fingerprint"],
        ddalgak::validate::launch_fingerprint(&site).as_str()
    );
    assert_eq!(site["title"], "F16: opens example.com in your browser");
    let prog = rule("open-marker");
    assert_eq!(prog["steps"][0]["path"], exe_arg.as_str(), "quotes removed");
    assert_eq!(
        prog["steps"][0]["env"],
        json!([{ "name": "DDALGAK_TEST_VAR", "value": SECRET }])
    );
    assert_eq!(rule("no-consent")["enabled"], false);
    // Shown redacted unless --show-targets.
    let (_, d) = sb.json(&["rule", "show", "open-site"]);
    no_secret("rule show", &d.to_string());
    assert_eq!(d["data"]["targets_redacted"], true);
    assert_eq!(
        d["data"]["rule"]["steps"][0]["url"],
        json!({ "redacted": true, "shown": "https://example.com/…" })
    );
    let (_, d) = sb.json(&["rule", "show", "open-marker"]);
    no_secret("rule show", &d.to_string());
    let step = &d["data"]["rule"]["steps"][0];
    assert_eq!(
        step["path"],
        json!({ "redacted": true, "program": "마커 app.exe" })
    );
    assert_eq!(step["args"], json!({ "redacted": true, "count": 2 }));
    assert_eq!(
        step["env"],
        json!({ "redacted": true, "names": ["DDALGAK_TEST_VAR"] })
    );
    let (_, d) = sb.json(&["rule", "show", "open-site", "--show-targets"]);
    assert_eq!(d["data"]["rule"]["steps"][0]["url"], normalized.as_str());
    assert_eq!(d["data"]["targets_redacted"], false);
    for args in [
        &["rule", "list"][..],
        &["rule", "check"],
        &["status"],
        &["rules", "history"],
    ] {
        let (_, d) = sb.json(args);
        no_secret(&args.join(" "), &d.to_string());
        let human = sb.run(args);
        no_secret(&args.join(" "), &String::from_utf8_lossy(&human.stdout));
    }
    let human = sb.run(&["rule", "show", "open-marker"]);
    no_secret("rule show (human)", &String::from_utf8_lossy(&human.stdout));
    // The real test host: loading the rules starts nothing.
    let lr = launch_root.display().to_string();
    let mut host = sb.host_with(&["--test-launch-root", &lr]);
    let record = ddalgak::launch::TestHostLauncher::record_file(&launch_root);
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        sb.json(&["quit"]);
        return;
    }
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    let (_, st) = sb.json(&["status"]);
    no_secret("status with the app", &st.to_string());
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["open-marker", "open-site"]),
        "{st:#}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !marker_file.exists() && !record.exists(),
        "loading launched nothing"
    );
    if !ddalgak::win::input_desktop_available() {
        eprintln!("skipped (locked): the launch triggers need SendInput on an unlocked desktop");
        sb.json(&["quit"]);
        assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
        return;
    }
    let mut verified: Vec<String> = Vec::new();
    // F15 and F16 are exact triggers: a modifier a person holds keeps them from matching, so
    // each launch runs as attempts judged with the physical input the host saw (Step 75).
    'steps: {
        let t = Instant::now();
        let started = quiet::attempts(
            "an injected F15 starts the hidden test program",
            || sb.physical(),
            |_| {
                quiet::sent(tap_other(VK_F15), "F15")?;
                marker::wait(&marker_file, Duration::from_secs(10)).ok_or_else(|| {
                    quiet::Fail::Failed("the hidden marker program did not run".into())
                })
            },
            || {},
        );
        let Some(m) = started.verified("F15 starts the test program") else {
            break 'steps;
        };
        marker::kill_if_running(m.pid, &exe);
        assert_eq!(m.args, ["첫 번째 인자"]);
        assert_eq!(m.env, SECRET);
        verified.push(format!(
            "an injected F15 started the hidden test program from a path with spaces and Korean characters ({:?} to its marker)",
            t.elapsed()
        ));
        // A double press: the second one opens nothing (rate_limited, or busy while running).
        // A disturbed attempt (a held modifier dropped one press) starts over with an empty
        // record once the one-second rate limit has run out.
        let read_opened = || -> Vec<Value> {
            std::fs::read_to_string(&record)
                .unwrap_or_default()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        };
        let site_outcomes = || -> Vec<String> {
            let (_, st) = sb.json(&["status"]);
            st["data"]["activity"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["rule"] == "open-site")
                .map(|a| a["outcome"].as_str().unwrap().to_string())
                .collect()
        };
        let double = quiet::attempts(
            "an injected F16 double press hands the URL off once",
            || sb.physical(),
            |_| {
                let before = site_outcomes().len();
                quiet::sent(tap_other(VK_F16), "F16")?;
                let _ = tap_other(VK_F16);
                quiet::check(
                    quiet::eventually(10_000, || !read_opened().is_empty()),
                    || "no hand-off was recorded".into(),
                )?;
                std::thread::sleep(Duration::from_millis(300));
                let opened = read_opened();
                // Physical input can only keep a press from matching: more than one hand-off
                // is always a failure.
                assert!(opened.len() <= 1, "more than one hand-off: {opened:?}");
                assert_eq!(
                    opened,
                    [json!({ "verb": "open", "target": normalized, "show": "normal" })],
                    "the verb open on the normalized URL, no browser opened"
                );
                let mut outcomes = site_outcomes();
                let until = Instant::now() + Duration::from_secs(5);
                while outcomes.len() < before + 2 && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(50));
                    outcomes = site_outcomes();
                }
                let new = outcomes[before.min(outcomes.len())..].to_vec();
                quiet::check(
                    new.iter().any(|o| o == "ok")
                        && new.iter().any(|o| o == "rate_limited" || o == "busy"),
                    || format!("one press ran, the other was dropped: {new:?}"),
                )
            },
            || {
                let _ = std::fs::remove_file(&record);
                std::thread::sleep(Duration::from_millis(1100));
            },
        );
        if double.verified("F16 double press").is_none() {
            break 'steps;
        }
        verified.push(
            "an injected F16 handed the normalized URL to the recording shell with the verb open, once"
                .into(),
        );
        let until = Instant::now() + Duration::from_secs(5);
        let st = loop {
            let (_, st) = sb.json(&["status"]);
            let n = st["data"]["activity"].as_array().map_or(0, Vec::len);
            if n >= 3 || Instant::now() > until {
                break st;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        no_secret("status after the launches", &st.to_string());
        let outcomes: Vec<(String, String)> = st["data"]["activity"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["rule"].as_str().unwrap().to_string(),
                    a["outcome"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert!(
            outcomes.contains(&("open-marker".into(), "ok".into())),
            "{outcomes:?}"
        );
        assert!(
            outcomes.contains(&("open-site".into(), "ok".into())),
            "{outcomes:?}"
        );
        assert!(
            outcomes
                .iter()
                .any(|(r, o)| r == "open-site" && (o == "rate_limited" || o == "busy")),
            "{outcomes:?}"
        );
        verified.push("the double press was dropped".into());
    }
    eprintln!("verified with the real test host: {verified:?}");
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

/// Step 71: mouse rules through the CLI (flags and `--step` JSON), the refusals that write
/// nothing, and the real test host loading them active without a mouse hook. The test host's
/// mouse actions see a simulated pointer and inject nothing; the only real input is an obscure
/// key injected as "another program", which mouse rules refuse (`skipped_injected_input`). No
/// test moves, scrolls or clicks the real pointer.
#[test]
fn mouse_rules_through_the_cli_and_the_real_test_host() {
    let sb = Sandbox::new("mouse");
    let add = |id: &str, keys: &str, action: &[&str]| {
        let mut a = vec!["rule", "add", "--id", id, "--trigger", keys];
        a.extend_from_slice(action);
        a.extend_from_slice(&["--allow", "input-injection"]);
        sb.json(&a)
    };
    let ok = |id: &str, keys: &str, action: &[&str]| {
        let (exit, d) = add(id, keys, action);
        assert_eq!(exit, 0, "{id}: {d:#}");
        d
    };
    ok(
        "glide-right",
        "F13",
        &[
            "--action",
            "mouse_move",
            "--dx",
            "1",
            "--speed",
            "600",
            "--acceleration",
            "3",
        ],
    );
    ok(
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
        ],
    );
    ok(
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
    );
    ok(
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
    );
    ok(
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
    );
    let d = ok(
        "back",
        "F18",
        &[
            "--action",
            "mouse_click",
            "--button",
            "x1",
            "--x",
            "-200",
            "--y",
            "300",
        ],
    );
    assert_eq!(d["data"]["created"], "back", "{d:#}");
    // Several mouse steps in order, as JSON.
    ok(
        "click-twice-there",
        "F19",
        &[
            "--step",
            r#"{"type":"mouse_move","mode":"absolute","to":{"x":24,"y":24,"relative_to":"active_window"}}"#,
            "--step",
            r#"{"type":"mouse_click","button":"right","op":"double_click"}"#,
        ],
    );
    let before = sb.rules_bytes();
    // Refused, with nothing written: both a distance and a point, no direction while held, a
    // continuous grid move, a speed beyond the limit, an unknown direction, a held button with
    // repeat or on a sequence.
    for (args, code) in [
        (
            vec![
                "--action",
                "mouse_move",
                "--dx",
                "1",
                "--x",
                "3",
                "--y",
                "4",
            ],
            "usage",
        ),
        (
            vec!["--action", "mouse_move", "--dx", "0", "--speed", "600"],
            "rule_invalid",
        ),
        (
            vec![
                "--action",
                "mouse_move",
                "--dx",
                "1",
                "--grid",
                "16x9",
                "--speed",
                "600",
            ],
            "rule_invalid",
        ),
        (
            vec!["--action", "mouse_move", "--dx", "1", "--speed", "9000"],
            "rule_invalid",
        ),
        (
            vec!["--action", "mouse_scroll", "--direction", "sideways"],
            "usage",
        ),
        (
            vec![
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
        let (exit, d) = add("refused", "F20", &args);
        assert_eq!(
            (exit, d["error"]["code"].as_str()),
            (2, Some(code)),
            "{args:?}: {d:#}"
        );
    }
    let (exit, d) = sb.json(&[
        "rule",
        "add",
        "--id",
        "held-sequence",
        "--trigger",
        "F20 F21",
        "--action",
        "mouse_click",
        "--button",
        "left",
        "--op",
        "hold",
        "--allow",
        "input-injection",
    ]);
    assert_eq!(
        (exit, d["error"]["code"].as_str()),
        (2, Some("rule_invalid")),
        "{d:#}"
    );
    assert_eq!(sb.rules_bytes(), before, "refusals write nothing");
    // As stored: canonical steps and titles that name directions and numbers.
    let doc: Value = serde_json::from_slice(&sb.rules_bytes()).unwrap();
    let rules = doc["profiles"][0]["rules"].as_array().unwrap();
    let rule = |id: &str| rules.iter().find(|r| r["id"] == id).unwrap().clone();
    assert_eq!(
        rule("cell-left")["steps"][0],
        json!({"type":"mouse_move","mode":"relative","dx":-1,"dy":0,"unit":"grid",
               "grid":{"columns":16,"rows":9},"while_held":{"repeat_ms":150}})
    );
    assert_eq!(
        rule("glide-right")["title"],
        "Moves the pointer right at 600 px/s while F13 is held"
    );
    assert_eq!(rule("wheel")["title"], "F16: scrolls down 3 notches");
    assert_eq!(
        rule("click-twice-there")["title"],
        "F19: moves the pointer to (24, 24) in the active window (+1 more)"
    );
    let (_, list) = sb.json(&["rule", "list"]);
    let states: Vec<(String, String)> = list["data"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["state"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        states.iter().all(|(_, s)| s == "active"),
        "every mouse rule runs from Step 71: {states:?}"
    );
    let (_, check) = sb.json(&["rule", "check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0, "{check:#}");
    // The real test host: loaded active, no mouse hook (mouse actions need none), nothing moved.
    let mut host = sb.host();
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        sb.json(&["quit"]);
        return;
    }
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["rules"]["active"], 7, "{st:#}");
    assert_eq!(st["data"]["rules"]["inactive"], json!([]));
    assert_eq!(
        st["data"]["engine"]["hook"]["mouse_installed"], false,
        "mouse actions never install the mouse hook (no pointer tracking)"
    );
    if ddalgak::win::input_desktop_available() {
        // An exact trigger: a modifier a person holds keeps it from matching (Step 75).
        let what = "mouse rules refuse another program's key";
        let seen = sb.tap_until(what, VK_F13, |d| {
            d["activity"].as_array().is_some_and(|a| !a.is_empty())
        });
        if let Some(st) = seen.verified(what) {
            let outcome = st["data"]["activity"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone();
            assert_eq!(
                (outcome["rule"].as_str(), outcome["outcome"].as_str()),
                (Some("glide-right"), Some("skipped_injected_input")),
                "mouse rules accept physical keys only"
            );
        }
    } else {
        eprintln!("skipped (locked): the injected-input check");
    }
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

/// Step 72 (`DDALGAK-FR-016`, `DDALGAK-FR-060`, `DDALGAK-SEC-005`): mode CRUD through the real CLI
/// without the app (refusals write nothing), then the active mode of the real test host: set,
/// toggle, the safe reset by pause, deleting the active mode, a restart that never restores a
/// mode, and on an unlocked desktop mode rules fired by injected obscure keys (a held layer that
/// ends with its key-up).
#[test]
fn modes_through_the_cli_and_the_real_test_host() {
    let sb = Sandbox::new("modes");
    let ok = |args: &[&str]| {
        let (exit, d) = sb.json(args);
        assert_eq!(exit, 0, "{args:?}: {d:#}");
        d
    };
    let err = |args: &[&str], code: &str| {
        let (exit, d) = sb.json(args);
        assert_eq!(d["error"]["code"], code, "{args:?}: {d:#}");
        assert_ne!(exit, 0);
        d
    };
    // A dry run writes nothing, not even a file.
    let d = ok(&[
        "mode",
        "add",
        "--title",
        "Navigation",
        "--id",
        "nav",
        "--dry-run",
    ]);
    assert_eq!(d["data"]["created"], "nav");
    assert!(sb.rules_bytes().is_empty(), "a dry run creates no file");
    let d = ok(&[
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
        "--color",
        "#b7a6f5",
    ]);
    assert_eq!(
        (
            d["data"]["created"].as_str(),
            d["data"]["changed"].as_bool()
        ),
        (Some("nav"), Some(true))
    );
    bless("mode-add", &d);
    ok(&["mode", "add", "--title", "Symbols", "--parent", "nav"]);
    ok(&["mode", "add", "--title", "Gaming", "--id", "game"]);
    ok(&[
        "rule",
        "add",
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
    ]);
    ok(&[
        "rule",
        "add",
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
    ]);
    ok(&[
        "rule",
        "add",
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
    ]);
    let list = ok(&["mode", "list"]);
    let modes = list["data"]["modes"].as_array().unwrap();
    assert_eq!(modes.len(), 4);
    assert_eq!(modes[2]["chain"], json!(["symbols", "nav", "base"]));
    assert_eq!(modes[1]["switched_by"], json!(["hold-nav"]));
    assert_eq!(
        list["data"]["active"],
        Value::Null,
        "no app, no active mode"
    );
    bless("mode-list", &list);
    let show = ok(&["mode", "show", "nav"]);
    assert_eq!(show["data"]["mode"]["rule_ids"], json!(["nav-h"]));
    bless("mode-show", &show);
    // Rename and copy keep IDs and references.
    ok(&["mode", "rename", "symbols", "Signs"]);
    let d = ok(&["mode", "copy", "nav", "--id", "nav2"]);
    assert_eq!(
        d["data"]["rules"].as_array().unwrap().len(),
        1,
        "nav-h was copied"
    );
    ok(&[
        "mode",
        "edit",
        "nav2",
        "--timeout-ms",
        "5000",
        "--one-shot",
        "--no-icon",
    ]);
    let (_, rl) = sb.json(&["rule", "list", "--mode", "symbols"]);
    let ids: Vec<&str> = rl["data"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"nav-h"),
        "Signs falls back to Navigation's rules: {ids:?}"
    );
    // Refusals write nothing.
    let before = sb.rules_bytes();
    let d = err(
        &["mode", "edit", "base", "--parent", "symbols"],
        "mode_invalid",
    );
    assert_eq!(d["error"]["exit_code"], 2);
    bless("error-mode-invalid", &d);
    err(&["mode", "delete", "base", "--yes"], "mode_in_use");
    let d = err(&["mode", "delete", "nav", "--yes"], "mode_in_use");
    assert!(
        d["error"]["details"].to_string().contains("symbols"),
        "{d:#}"
    );
    bless("error-mode-in-use", &d);
    err(&["mode", "delete", "game"], "confirmation_required");
    err(&["mode", "show", "ghost"], "mode_not_found");
    err(
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
    err(
        &["mode", "add", "--title", "Bad", "--timeout-ms", "10"],
        "usage",
    );
    assert_eq!(sb.rules_bytes(), before, "refusals write nothing");
    let d = ok(&["mode", "delete", "nav2", "--delete-rules", "--yes"]);
    assert_eq!(d["data"]["rules"].as_array().unwrap().len(), 1);
    bless("mode-delete", &d);
    let (_, check) = sb.json(&["rule", "check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0, "{check:#}");
    // Without the app: the mode the engine starts in; changing it needs the app.
    let st = ok(&["mode", "status"]);
    assert_eq!(st["data"]["mode"]["live"], false);
    assert_eq!(st["data"]["mode"]["active"], "base");
    bless("mode-status-not-running", &st);
    err(&["mode", "set", "nav"], "engine_not_running");

    // The real test host.
    let mut host = sb.host();
    err(&["mode", "set", "nav"], "engine_off");
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        sb.json(&["quit"]);
        return;
    }
    ok(&["engine", "start"]);
    let mode = |sb: &Sandbox| {
        let (_, st) = sb.json(&["mode", "status"]);
        st["data"]["mode"].clone()
    };
    let m = mode(&sb);
    assert_eq!(
        (m["active"].as_str(), m["live"].as_bool()),
        (Some("base"), Some(true))
    );
    let d = ok(&["mode", "set", "game", "--dry-run"]);
    assert_eq!(d["data"]["change"]["to"], Value::Null);
    assert_eq!(mode(&sb)["active"], "base", "a dry run changes nothing");
    let d = ok(&["mode", "set", "game"]);
    assert_eq!(
        d["data"]["change"],
        json!({"op": "set", "from": "base", "to": "game", "changed": true})
    );
    bless("mode-set", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["profile"]["mode"], "game");
    assert_eq!(st["data"]["profile"]["mode_state"]["origin"], "cli");
    let d = ok(&["mode", "toggle", "game"]);
    assert_eq!(
        d["data"]["change"]["to"], "base",
        "toggled back to the previous mode"
    );
    // `mode list` names the live mode.
    ok(&["mode", "set", "symbols"]);
    let list = ok(&["mode", "list"]);
    assert_eq!(list["data"]["active"], "symbols");
    let st = mode(&sb);
    assert_eq!(st["chain"], json!(["symbols", "nav", "base"]));
    bless("mode-status", &ok(&["mode", "status"]));
    // A pause is the safe reset to the default mode.
    ok(&["pause"]);
    let m = mode(&sb);
    assert_eq!(
        (m["active"].as_str(), m["origin"].as_str()),
        (Some("base"), Some("pause"))
    );
    ok(&["resume"]);
    // Deleting the active mode: the reload returns the engine to the default mode.
    ok(&["mode", "add", "--title", "Temporary", "--id", "temp"]);
    ok(&["mode", "set", "temp"]);
    ok(&["mode", "delete", "temp", "--yes"]);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let m = mode(&sb);
        if m["active"] == "base" {
            assert_eq!(m["origin"], "reload");
            break;
        }
        assert!(
            Instant::now() < until,
            "the deleted mode stayed active: {m:#}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    err(&["mode", "set", "temp"], "mode_not_found");
    ok(&["mode", "reset"]);
    // A restart never restores a mode (ADR 0043).
    ok(&["mode", "set", "game"]);
    ok(&["engine", "stop"]);
    let m = mode(&sb);
    assert_eq!(
        (m["active"].as_str(), m["live"].as_bool()),
        (Some("base"), Some(false))
    );
    err(&["mode", "set", "game"], "engine_off");
    ok(&["engine", "start"]);
    assert_eq!(mode(&sb)["active"], "base");
    // Mode rules on obscure keys through the real hook, as another program's input. Exact
    // triggers: a modifier a person holds keeps them from matching, so each step runs as
    // attempts judged with the physical input the host saw (Step 75).
    let wait = |want: &str| -> quiet::Step<Value> {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let m = mode(&sb);
            if m["active"] == want {
                return Ok(m);
            }
            if Instant::now() >= until {
                return Err(quiet::Fail::Failed(format!("never {want}: {m:#}")));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let send =
        |vk: u16, up: bool| ddalgak::win::inject::send_as_other_program(&[OutKey { vk, up }]) == 1;
    if ddalgak::win::input_desktop_available() {
        'modes: {
            let set = quiet::attempts(
                "F14 sets the game mode",
                || sb.physical(),
                |_| {
                    quiet::sent(send(VK_F14, false), "F14 down")?;
                    quiet::sent(send(VK_F14, true), "F14 up")?;
                    wait("game")
                },
                || {},
            );
            let Some(m) = set.verified("F14 sets the game mode") else {
                let _ = send(VK_F14, true);
                break 'modes;
            };
            assert_eq!(m["origin"], "rule:to-game");
            let held = quiet::attempts(
                "F13 holds the nav layer until its key-up",
                || sb.physical(),
                |_| {
                    quiet::sent(send(VK_F13, false), "F13 down")?;
                    let during = wait("nav");
                    assert!(send(VK_F13, true), "F13 must be released");
                    let during = during?;
                    let after = wait("game")?;
                    quiet::check(after["origin"] == "released:hold-nav", || {
                        format!("the layer ended with its key-up: {after:#}")
                    })?;
                    Ok(during)
                },
                || {},
            );
            if let Some(m) = held.verified("F13 holds the nav layer") {
                assert_eq!(m["held"], json!(["nav"]));
                assert_eq!(m["latched"], "game");
                eprintln!("verified with real SendInput: a set and a held layer through the hook");
            }
        }
    } else {
        eprintln!("skipped (locked): mode rules through the real hook");
    }
    let (_, st) = sb.json(&["status"]);
    bless("status-running-in-a-mode", &st);
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

// ---------------------------------------------------------------------------------------------
// Step 73: enable, disable, pause and recovery (DDALGAK-FR-002…004, DDALGAK-SEC-003,
// DDALGAK-SEC-005, DDALGAK-NFR-001). Session messages are synthetic and go only to this test's
// own host window (found by process ID and class); the workstation is never locked, nobody is
// signed out, nothing reboots. Only the test's own host is killed. Only F13–F20 are injected.
// ---------------------------------------------------------------------------------------------

const WM_QUERYENDSESSION: u32 = 0x0011;
const WM_ENDSESSION: u32 = 0x0016;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 0x7;
const VK_F20: u16 = 0x83;

/// The hidden window of the test host with process `pid` (never another program's window).
#[allow(unsafe_code)]
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
        // SAFETY: `lp` is the `Find` passed below, alive for the whole enumeration.
        let f = unsafe { &mut *(lp as *mut Find) };
        let mut pid = 0u32;
        // SAFETY: a window handle from the enumeration; a writable u32.
        unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
        if pid != f.pid {
            return 1;
        }
        let mut class = [0u16; 64];
        // SAFETY: a valid window; the buffer holds 64 UTF-16 units.
        let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
        let name = String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)]);
        if name == "mouv.ddalgak.host" {
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

/// Posts a message without pointers to the test host's own window.
#[allow(unsafe_code)]
fn post(hwnd: isize, msg: u32, wparam: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
    // SAFETY: a window of this test's own host process; the message carries no pointers.
    unsafe { PostMessageW(hwnd as _, msg, wparam, 0) != 0 }
}

#[allow(unsafe_code)]
fn key_down(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    // SAFETY: reads the async state of one of the test's obscure keys.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
}

impl Sandbox {
    /// Starts the test host with extra environment variables (debug-only test settings).
    fn host_env(&self, vars: &[(&str, &str)]) -> Host {
        let turn = turn();
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
        for (k, v) in vars {
            c.env(k, v);
        }
        let mut h = Host {
            child: c.spawn().unwrap(),
            _turn: turn,
        };
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

    /// Step 75: the physical (not injected) input events the test host's hook has seen.
    fn physical(&self) -> u64 {
        let (_, h) = self.json(&["health"]);
        h["data"]["counters"]["physical"].as_u64().unwrap_or(0)
    }

    /// Polls `status` until `cond` holds (at most `ms`): the status, or the last one as a failed
    /// attempt.
    fn status_within(
        &self,
        ms: u64,
        what: &str,
        cond: impl Fn(&Value) -> bool,
    ) -> quiet::Step<Value> {
        let until = Instant::now() + Duration::from_millis(ms);
        loop {
            let (_, st) = self.json(&["status"]);
            if cond(&st["data"]) {
                return Ok(st);
            }
            if Instant::now() >= until {
                return Err(quiet::Fail::Failed(format!("never {what}: {st:#}")));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Step 75: taps `vk` as another program until `status` satisfies `cond`, as attempts judged
    /// with the physical input the host saw (`quiet::attempts`).
    fn tap_until(
        &self,
        what: &str,
        vk: u16,
        cond: impl Fn(&Value) -> bool,
    ) -> quiet::Outcome<Value> {
        quiet::attempts(
            what,
            || self.physical(),
            |_| {
                quiet::sent(tap_other(vk), what)?;
                self.status_within(5000, what, &cond)
            },
            || {},
        )
    }

    /// Waits until `status` satisfies `cond` (5 s at most) and returns it.
    fn status_until(&self, what: &str, cond: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let (_, st) = self.json(&["status"]);
            if cond(&st["data"]) {
                return st;
            }
            assert!(Instant::now() < until, "never {what}: {st:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Modes and rules for the scope tests: `base` (default) and `nav`, an engine-control rule on
/// F13, a rule limited to `nav`, a rule that switches to `nav`.
fn scope_doc(on_start: &str) -> Value {
    json!({
        "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
        "revision": 1, "active_profile": "default",
        "settings": { "engine_on_start": on_start },
        "profiles": [ { "id": "default", "title": "Test", "default_mode": "base",
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
                  "steps": [ { "type": "mode", "op": "set", "mode": "nav" } ] }
            ] } ]
    })
}

fn inactive_code(d: &Value, rule: &str) -> Option<String> {
    d["rules"]["inactive"]
        .as_array()?
        .iter()
        .find(|i| i["rule"] == rule)
        .and_then(|i| i["code"].as_str())
        .map(str::to_string)
}

#[test]
fn enable_disable_and_pause_per_scope_never_reenable_by_themselves() {
    let sb = Sandbox::new("scopes");
    sb.write_rules(&scope_doc("on"));
    let ok = |args: &[&str]| {
        let (exit, d) = sb.json(args);
        assert_eq!(exit, 0, "{args:?}: {d:#}");
        d
    };
    let err = |args: &[&str], code: &str, exit: i32| {
        let (e, d) = sb.json(args);
        assert_eq!(
            (e, d["error"]["code"].as_str()),
            (exit, Some(code)),
            "{d:#}"
        );
        d
    };
    // Rules, modes and profiles without the app (rules.json through the rule store).
    let before = sb.rules_bytes();
    let d = ok(&["disable", "--rule", "pause-key", "--dry-run"]);
    assert_eq!(d["data"]["scope"], "rule");
    assert_eq!(sb.rules_bytes(), before, "a dry run writes nothing");
    let d = ok(&["disable", "--rule", "pause-key"]);
    assert_eq!(d["data"]["rules"], json!(["pause-key"]));
    bless("disable-rule", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["rules"]["disabled"], 1);
    ok(&["enable", "--rule", "pause-key"]);
    let d = ok(&["disable", "--mode", "nav"]);
    assert_eq!(
        (d["data"]["scope"].as_str(), d["data"]["modes"].clone()),
        (Some("mode"), json!(["nav"]))
    );
    assert_eq!(d["data"]["rules"], json!(["nav-only"]));
    bless("disable-mode", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(
        inactive_code(&st["data"], "nav-only").as_deref(),
        Some("mode_disabled")
    );
    assert_eq!(
        inactive_code(&st["data"], "to-nav").as_deref(),
        Some("mode_disabled"),
        "nothing switches to a disabled mode"
    );
    let (_, list) = sb.json(&["rule", "list"]);
    let state = |id: &str| {
        list["data"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .map(|r| (r["state"].clone(), r["code"].clone()))
            .unwrap()
    };
    assert_eq!(state("to-nav"), (json!("inactive"), json!("mode_disabled")));
    err(&["disable", "--mode", "base"], "mode_invalid", 2);
    err(&["mode", "disable", "nowhere"], "mode_not_found", 3);
    err(&["disable", "--profile", "nowhere"], "profile_not_found", 3);
    err(
        &["disable", "--rule", "pause-key", "--mode", "nav"],
        "usage",
        2,
    );
    err(&["disable", "--allow", "input-injection"], "usage", 2);
    let d = ok(&["mode", "enable", "nav"]);
    assert_eq!(d["command"], "mode.enable");
    let d = ok(&["disable", "--profile", "default"]);
    assert_eq!(d["data"]["scope"], "profile");
    bless("disable-profile", &d);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["profile"]["enabled"], false);
    assert_eq!(st["data"]["rules"]["active"], 0);
    assert!(
        st["data"]["rules"]["inactive"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["code"] == "profile_disabled")
    );
    // The running app: the profile stays disabled through starts, enables and restarts.
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    let running = if elevated() { "off" } else { "running" };
    assert_eq!(
        st["data"]["engine"]["state"], running,
        "engine_on_start: on"
    );
    assert_eq!(st["data"]["profile"]["enabled"], false);
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        sb.json(&["quit"]);
        assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
        return;
    }
    assert_eq!(st["data"]["indicator"]["attention"], "profile_disabled");
    bless("status-profile-disabled", &st);
    let (_, d) = sb.json(&["enable"]);
    assert_eq!(d["data"]["scope"], "engine");
    assert!(
        d["warnings"][0]
            .as_str()
            .is_some_and(|w| w.contains("disabled")),
        "a global enable says the profile is still disabled: {d:#}"
    );
    ok(&["enable", "--profile", "default"]);
    let st = sb.status_until("the profile enabled", |d| d["rules"]["active"] == 3);
    assert_eq!(st["data"]["profile"]["enabled"], true);
    // A disabled mode refuses a switch from the CLI.
    ok(&["mode", "disable", "nav"]);
    sb.status_until("nav disabled", |d| {
        inactive_code(d, "to-nav").as_deref() == Some("mode_disabled")
    });
    let d = err(&["mode", "set", "nav"], "mode_disabled", 7);
    bless("error-mode-disabled", &d);
    ok(&["mode", "enable", "nav"]);
    sb.status_until("nav enabled", |d| d["rules"]["active"] == 3);
    ok(&["mode", "set", "nav"]);
    // Disabling the active mode returns the engine to the default mode on the reload.
    ok(&["mode", "disable", "nav"]);
    sb.status_until("back in base", |d| d["profile"]["mode"] == "base");
    ok(&["mode", "enable", "nav"]);
    // Pause and resume are temporary: the engine stays on, the hook stays installed.
    let d = ok(&["pause"]);
    assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["indicator"]["shape"], "bars");
    ok(&["resume"]);
    // Release: nothing is held; counts only.
    let d = ok(&["release", "--dry-run"]);
    assert_eq!(d["data"]["dry_run"], true);
    let d = ok(&["release"]);
    assert_eq!(d["data"]["held_by_ddalgak"], 0);
    assert_eq!(d["data"]["key_state_reset"], true);
    bless("release", &d);
    // The global disable is remembered: a restart keeps the engine off despite engine_on_start.
    let d = ok(&["disable", "--dry-run"]);
    assert_eq!(d["data"]["would"]["to"], "off");
    let d = ok(&["disable"]);
    assert_eq!(d["data"]["change"]["to"], "off");
    bless("disable", &d);
    assert!(sb.local().join("engine.disabled").exists());
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
    drop(host);
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "off");
    assert_eq!(st["data"]["start"]["off_reason"], "disabled");
    assert_eq!(st["data"]["engine"]["off_reason"], "disabled");
    bless("status-disabled-by-user", &st);
    let d = ok(&["enable"]);
    assert_eq!(d["data"]["change"]["to"], "running");
    bless("enable", &d);
    assert!(!sb.local().join("engine.disabled").exists());
    // Quitting while on is not a turn-off: the next start follows engine_on_start again.
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
    drop(host);
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "running");
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

#[test]
fn crash_lock_logoff_and_reboot_recover_without_re_enabling() {
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        return;
    }
    // One turn for the whole test: it injects between two hosts too.
    let _turn = turn();
    let sb = Sandbox::new("recover");
    sb.write_rules(&rules_doc("on"));
    let mut host = sb.host();
    let pid = host.child.id();
    let hwnd = host_window(pid).expect("the test host's own window");
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "running");
    assert!(st["data"]["start"]["recovered"].is_null(), "a clean start");
    // A synthetic session lock: keys released, rules wait, the engine state stays.
    assert!(post(hwnd, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK));
    let st = sb.status_until("locked", |d| d["session"]["locked"] == true);
    assert_eq!(st["data"]["engine"]["state"], "running");
    assert_eq!(st["data"]["indicator"]["attention"], "session_locked");
    bless("status-session-locked", &st);
    let send =
        |vk: u16, up: bool| ddalgak::win::inject::send_as_other_program(&[OutKey { vk, up }]) == 1;
    let desktop = ddalgak::win::input_desktop_available();
    // F13 is an exact trigger: a modifier a person holds keeps it from matching (Step 75).
    if desktop {
        let what = "a rule waits while the session is locked";
        let skipped = sb.tap_until(what, VK_F13, |d| {
            d["activity"]
                .as_array()
                .is_some_and(|a| a.iter().any(|e| e["outcome"] == "skipped_secure_desktop"))
        });
        if let Some(st) = skipped.verified(what) {
            assert_eq!(
                st["data"]["engine"]["state"], "running",
                "nothing fired while locked"
            );
            eprintln!("verified with real SendInput: {what}");
        }
    } else {
        eprintln!("skipped (locked): a key while locked");
    }
    assert!(post(hwnd, WM_WTSSESSION_CHANGE, 0x8)); // WTS_SESSION_UNLOCK
    sb.status_until("unlocked", |d| d["session"]["locked"] == false);
    if desktop {
        let what = "rules run again after the unlock";
        let paused = sb.tap_until(what, VK_F13, |d| d["engine"]["state"] == "paused");
        if paused.verified(what).is_some() {
            sb.json(&["resume"]);
            eprintln!("verified with real SendInput: {what}");
        }
    }
    // A crash: the test kills its own host while the engine is on.
    host.child.kill().unwrap();
    let _ = host.child.wait();
    drop(host);
    assert!(
        sb.local().join("engine.running").exists(),
        "the run marker stays"
    );
    // A key the killed app "left pressed": F20 (an obscure key) held by injection. Whatever
    // happens below, the test lets go of it.
    struct LetGo(u16);
    impl Drop for LetGo {
        fn drop(&mut self) {
            if key_down(self.0) {
                let _ = ddalgak::win::inject::send_as_other_program(&[OutKey {
                    vk: self.0,
                    up: true,
                }]);
            }
        }
    }
    let _let_go = LetGo(VK_F20);
    let stuck = desktop && send(VK_F20, false) && key_down(VK_F20);
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(
        st["data"]["engine"]["state"], "off",
        "never back on after a crash"
    );
    assert_eq!(st["data"]["start"]["off_reason"], "unclean_exit");
    assert_eq!(st["data"]["indicator"]["shape"], "ring_struck");
    let rec = &st["data"]["start"]["recovered"];
    assert!(
        rec["checked"].as_u64().unwrap() >= 1,
        "F20 is a remap output: {rec}"
    );
    if stuck {
        assert_eq!(rec["released"], 1, "the stale F20 was released: {rec}");
        assert!(!key_down(VK_F20), "F20 is up again");
        eprintln!("verified with real SendInput: a key left pressed by a killed app is released");
    } else {
        let _ = send(VK_F20, true);
        eprintln!("skipped (locked or SendInput refused): a stale key after a crash");
    }
    bless("status-unclean-exit-recovered", &st);
    // A sign-out: Windows asks, then ends the session; the engine stops cleanly.
    sb.json(&["engine", "start"]);
    let hwnd = host_window(host.child.id()).expect("the test host's own window");
    assert!(post(hwnd, WM_QUERYENDSESSION, 0));
    assert!(post(hwnd, WM_ENDSESSION, 1));
    let st = sb.status_until("stopped by the session end", |d| {
        d["engine"]["state"] == "off"
    });
    assert_eq!(st["data"]["engine"]["last_change"]["origin"], "session");
    assert!(!sb.local().join("engine.running").exists(), "a clean stop");
    assert!(
        !sb.local().join("engine.disabled").exists(),
        "a sign-out is not the user's turn-off"
    );
    // Windows then ends the process; the "reboot": the next start follows engine_on_start.
    host.child.kill().unwrap();
    let _ = host.child.wait();
    drop(host);
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "running");
    assert_eq!(st["data"]["start"]["off_reason"], "default");
    // The user turns it off, then the machine "reboots": it stays off.
    sb.json(&["disable"]);
    host.child.kill().unwrap();
    let _ = host.child.wait();
    drop(host);
    let mut host = sb.host();
    let (_, st) = sb.json(&["status"]);
    assert_eq!(st["data"]["engine"]["state"], "off");
    assert_eq!(st["data"]["start"]["off_reason"], "disabled");
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

#[test]
#[allow(unsafe_code)]
fn a_taken_shortcut_is_reported_and_never_stolen() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
    };
    const VK_F16: u16 = 0x7F;
    // This test thread plays "another program" that owns Ctrl+Alt+Shift+F16.
    // SAFETY: a thread hotkey (no window) with an obscure chord, unregistered below.
    let mine = unsafe {
        RegisterHotKey(
            std::ptr::null_mut(),
            0x7301,
            MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT,
            u32::from(VK_F16),
        )
    } != 0;
    if !mine {
        eprintln!("skipped: another program already owns Ctrl+Alt+Shift+F16");
        return;
    }
    let sb = Sandbox::new("taken");
    sb.write_rules(&rules_doc("off"));
    sb.write_hotkeys(
        &json!({ "schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
        "overrides": [ { "action": "engine.toggle", "bindings": ["Ctrl+Alt+Shift+F16"] } ] }),
    );
    let mut host = sb.host();
    let reg = |st: &Value| {
        st["data"]["shortcuts"]["global"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["action"] == "engine.toggle")
            .map(|s| s["registration"].clone())
    };
    let (_, st) = sb.json(&["status"]);
    assert_eq!(reg(&st), Some(json!("taken")));
    assert_eq!(st["data"]["indicator"]["attention"], "shortcut_taken");
    let (_, h) = sb.json(&["health"]);
    assert!(
        h["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "shortcut_taken")
    );
    // The other program lets go: Ddalgak still does not take it (nothing is retried).
    // SAFETY: the hotkey this thread registered above.
    unsafe { UnregisterHotKey(std::ptr::null_mut(), 0x7301) };
    std::thread::sleep(Duration::from_millis(200));
    let (_, st) = sb.json(&["status"]);
    assert_eq!(reg(&st), Some(json!("taken")), "never retried or stolen");
    // The engine is still reachable without the shortcut.
    let (code, _) = sb.json(&["enable"]);
    assert_eq!(code, if elevated() { 5 } else { 0 });
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

#[test]
fn the_engine_has_no_polling_loop_in_any_state() {
    // DDALGAK-NFR-001: threads wake only for input, posted messages and requests. Measured
    // between two `health` requests 2 s apart, with the engine off, running, paused and running
    // in a (synthetically) locked session.
    let sb = Sandbox::new("idle");
    sb.write_rules(&rules_doc("off"));
    let mut host = sb.host();
    let hwnd = host_window(host.child.id()).expect("the test host's own window");
    let wake = |d: &Value| {
        let w = &d["data"]["counters"]["wakeups"];
        (
            w["hook_thread"].as_u64().unwrap(),
            w["worker"].as_u64().unwrap(),
            w["host"].as_u64().unwrap(),
        )
    };
    let mut states = vec!["off"];
    if !elevated() {
        states.extend(["running", "paused", "locked"]);
    }
    for state in states {
        match state {
            "running" => {
                sb.json(&["enable"]);
            }
            "paused" => {
                sb.json(&["pause"]);
            }
            "locked" => {
                sb.json(&["resume"]);
                assert!(post(hwnd, WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK));
                sb.status_until("locked", |d| d["session"]["locked"] == true);
            }
            _ => {}
        }
        // Let the change settle (the worker gets its rules, the hook thread its table).
        std::thread::sleep(Duration::from_millis(300));
        let (_, a) = sb.json(&["health"]);
        std::thread::sleep(Duration::from_secs(2));
        let (_, b) = sb.json(&["health"]);
        let (h0, w0, m0) = wake(&a);
        let (h1, w1, m1) = wake(&b);
        eprintln!(
            "{state}: 2 s idle: hook thread +{}, worker +{}, app +{} wake-ups",
            h1 - h0,
            w1 - w0,
            m1 - m0
        );
        assert_eq!(h1 - h0, 0, "{state}: the hook thread has no timer");
        assert_eq!(w1 - w0, 0, "{state}: the worker has no timer");
        if state == "off" {
            // Off: no watchers; the app wakes for the second request only.
            assert!(m1 - m0 <= 3, "{state}: app wake-ups {}", m1 - m0);
        }
    }
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}

#[test]
fn a_hook_that_reaches_the_timeout_turns_the_engine_off() {
    if elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        return;
    }
    if !ddalgak::win::input_desktop_available() {
        eprintln!("skipped (locked): the hook sees no key event to measure");
        return;
    }
    let sb = Sandbox::new("hooklost");
    sb.write_rules(&rules_doc("off"));
    // Debug-only: a 1 µs threshold makes any ordinary callback count as one that reached
    // LowLevelHooksTimeout, so the whole path runs without slowing a single real key.
    let mut host = sb.host_env(&[("DDALGAK_DEV_HOOK_TIMEOUT_US", "1")]);
    sb.json(&["enable"]);
    let (_, h) = sb.json(&["health"]);
    assert_eq!(h["data"]["engine"]["hook"]["timeout_us"], 1);
    let send =
        |vk: u16, up: bool| ddalgak::win::inject::send_as_other_program(&[OutKey { vk, up }]) == 1;
    let mut off = None;
    for _ in 0..5 {
        if !(send(VK_F17, false) && send(VK_F17, true)) {
            eprintln!("skipped (SendInput refused): no key event reached the hook");
            sb.json(&["quit"]);
            let _ = host.wait_exit(Duration::from_secs(10));
            return;
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            let (_, st) = sb.json(&["status"]);
            if st["data"]["engine"]["state"] == "off" {
                off = Some(st);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if off.is_some() {
            break;
        }
    }
    let st = off.expect("the engine turned itself off");
    assert_eq!(st["data"]["engine"]["off_reason"], "hook_lost");
    assert_eq!(st["data"]["engine"]["last_change"]["origin"], "hook_lost");
    assert_eq!(st["data"]["engine"]["hook"]["installed"], false);
    assert_eq!(st["data"]["indicator"]["shape"], "ring_struck");
    assert_eq!(st["data"]["indicator"]["attention"], "hook_lost");
    bless("status-hook-lost", &st);
    let (_, h) = sb.json(&["health"]);
    assert!(h["data"]["counters"]["hook_timeouts"].as_u64().unwrap() >= 1);
    assert!(
        h["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "hook_lost")
    );
    assert!(
        !sb.local().join("engine.disabled").exists(),
        "a lost hook is not the user's turn-off"
    );
    // Turning it on again installs a fresh hook (never by itself).
    let (_, d) = sb.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["hook"]["installed"], true);
    sb.json(&["quit"]);
    assert_eq!(host.wait_exit(Duration::from_secs(10)), Some(0));
}
