//! A sandbox that runs the real `noeul` binary in an isolated profile.
//!
//! Safety rules for this machine (the maintainer uses it): every sandbox has its own `APPDATA`,
//! `LOCALAPPDATA`, `TEMP` and pipe endpoint, so it never reaches a real Noeul; instances always
//! run with `--no-tray`; and the profile's `hotkeys.json` is written **before** any instance
//! starts, binding every global action to an obscure `Ctrl+Alt+Shift+F2x` chord, disabling it, or
//! turning shortcuts off. A guard ([`Sandbox::write_keys`]) computes what an instance would
//! register from that file with Noeul's own planner and fails the test if anything else, such
//! as the default `F1` or `F3`, would be registered. Overlay tests use the debug-build test hook
//! ([`Sandbox::synthetic`]): the overlay shows a synthetic picture far off-screen, never takes
//! focus and never reads the screen. Every process a sandbox starts is ended when it drops.
#![allow(dead_code)]

#[path = "../../../../xtask/src/jsonschema.rs"]
#[allow(clippy::all)]
pub mod jsonschema;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_noeul")
}

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn schema(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(repo().join(format!("docs/contracts/schemas/{name}.schema.json"))).unwrap(),
    )
    .unwrap()
}

/// Validates `v` against a schema, or one `$defs` entry of it (`file#def`).
pub fn validate(name: &str, v: &Value) -> Vec<String> {
    let (file, def) = name
        .split_once('#')
        .map_or((name, None), |(f, d)| (f, Some(d)));
    let mut s = schema(file);
    if let Some(d) = def {
        s = serde_json::json!({"$ref": format!("#/$defs/{d}"), "$defs": s["$defs"].clone()});
    }
    jsonschema::Validator::new(&s).validate(v).unwrap()
}

/// How the profile's shortcuts are set up before an instance starts.
#[derive(Debug, Clone, Copy)]
pub enum Keys<'a> {
    /// `paste.float` on this obscure chord; `capture.region` disabled.
    Chord(&'a str),
    /// `capture.region` and `paste.float` on these obscure chords.
    Both { capture: &'a str, paste: &'a str },
    /// Every shortcut off in hotkeys.json (`"enabled": false`).
    Off,
    /// Both global actions disabled, the window keys (overlay, pins) on: nothing is registered.
    NoGlobals,
}

/// The only chords a test may register: `Ctrl+Alt+Shift+F20` … `F24` (keys missing from
/// ordinary keyboards; each Step's tests use their own).
pub fn obscure(keys: &str) -> bool {
    keys.strip_prefix("Ctrl+Alt+Shift+F2")
        .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
}

#[derive(Debug)]
pub struct Sandbox {
    pub root: PathBuf,
    pub endpoint: String,
    /// The debug-build test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`) for every command.
    pub synthetic: bool,
    children: Vec<Child>,
    pids: Vec<u32>,
}

static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Sandbox {
    pub fn new(tag: &str) -> Sandbox {
        Sandbox::with_keys(tag, Keys::Off)
    }

    pub fn with_keys(tag: &str, keys: Keys<'_>) -> Sandbox {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = format!("{tag}-{}-{n}", std::process::id());
        let root = std::env::temp_dir().join(format!("noeul-test-{id}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Roaming").join("mouv").join("noeul")).unwrap();
        std::fs::create_dir_all(root.join("Local")).unwrap();
        std::fs::create_dir_all(root.join("Temp")).unwrap();
        let sb = Sandbox {
            root,
            endpoint: format!("noeul-test-{id}"),
            synthetic: false,
            children: Vec::new(),
            pids: Vec::new(),
        };
        sb.write_keys(keys);
        sb
    }

    pub fn hotkeys_file(&self) -> PathBuf {
        self.root
            .join("Roaming")
            .join("mouv")
            .join("noeul")
            .join("hotkeys.json")
    }

    /// A sandbox whose instances use the debug-build test hook: a synthetic desktop far
    /// off-screen, overlay and pins that never take focus, no screen read.
    pub fn synthetic(tag: &str, keys: Keys<'_>) -> Sandbox {
        let mut sb = Sandbox::with_keys(tag, keys);
        sb.synthetic = true;
        sb
    }

    /// The sandbox's `TEMP` folder (it must stay empty: no temporary image files).
    pub fn temp_dir(&self) -> PathBuf {
        self.root.join("Temp")
    }

    /// Writes the safe `hotkeys.json`, then checks with Noeul's own planner that an instance
    /// would register nothing but obscure chords (never `F1`, `F3` or another common key).
    pub fn write_keys(&self, keys: Keys<'_>) {
        let schema = |overrides: Value| {
            serde_json::json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
                "overrides": overrides})
        };
        let doc = match keys {
            Keys::Chord(c) => {
                assert!(obscure(c), "tests register only obscure chords, not {c}");
                schema(serde_json::json!([
                    {"action": "capture.region", "disabled": true},
                    {"action": "paste.float", "bindings": [c]},
                ]))
            }
            Keys::Both { capture, paste } => {
                assert!(obscure(capture) && obscure(paste), "obscure chords only");
                schema(serde_json::json!([
                    {"action": "capture.region", "bindings": [capture]},
                    {"action": "paste.float", "bindings": [paste]},
                ]))
            }
            Keys::Off => serde_json::json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "enabled": false}),
            Keys::NoGlobals => schema(serde_json::json!([
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
            ])),
        };
        std::fs::write(
            self.hotkeys_file(),
            serde_json::to_vec_pretty(&doc).unwrap(),
        )
        .unwrap();
        // The guard: what would an instance register from this profile?
        let paths = noeul::paths::Paths::under(&self.root);
        let (state, loaded) = noeul::hotkeys::load(&paths);
        assert!(
            !loaded.state.safe_mode(),
            "the test's hotkeys.json must be valid: {:?}",
            loaded.problems
        );
        let (wanted, _) = noeul::hotkeys::global_plan(&state);
        for w in &wanted {
            assert!(
                obscure(&w.keys),
                "a test would register {} for {}: only Ctrl+Alt+Shift+F2x chords are allowed",
                w.keys,
                w.action
            );
        }
        if matches!(keys, Keys::Off) {
            assert!(!state.enabled(), "shortcuts are off");
        }
        if matches!(keys, Keys::NoGlobals) {
            assert!(wanted.is_empty(), "no global hotkey is registered");
        }
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(exe());
        c.args(args)
            .arg("--endpoint")
            .arg(&self.endpoint)
            .env("APPDATA", self.root.join("Roaming"))
            .env("LOCALAPPDATA", self.root.join("Local"))
            .env("TEMP", self.temp_dir())
            .env("TMP", self.temp_dir())
            .env_remove("KIM_NO_HOTKEYS")
            .env("KIM_NO_INPUT", "1")
            .stdin(Stdio::null());
        if self.synthetic {
            c.env("NOEUL_DEV_SYNTHETIC_DESKTOP", "1");
        } else {
            c.env_remove("NOEUL_DEV_SYNTHETIC_DESKTOP");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    /// `noeul <args> --json`: (exit code, document); the envelope is validated.
    pub fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut full = args.to_vec();
        full.push("--json");
        let out = self.run(&full);
        let code = out.status.code().unwrap_or(-1);
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{full:?}: not JSON ({e}): {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        let errors = validate("cli-envelope-v1", &doc);
        assert!(
            errors.is_empty(),
            "{full:?} violates cli-envelope-v1: {errors:?}\n{doc:#}"
        );
        let want = if doc["ok"] == true {
            0
        } else {
            doc["error"]["exit_code"].as_i64().unwrap() as i32
        };
        assert_eq!(
            code, want,
            "{full:?}: exit code matches the document\n{doc:#}"
        );
        (code, doc)
    }

    /// Starts `noeul run --no-tray` as a child this sandbox owns, and waits until it answers.
    pub fn instance(&mut self) -> u32 {
        let child = self
            .cmd(&["run", "--no-tray"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        self.children.push(child);
        self.wait_healthy(pid);
        pid
    }

    pub fn wait_healthy(&self, pid: u32) {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            let (code, doc) = self.json(&["health"]);
            if code == 0 {
                assert_eq!(doc["data"]["pid"], pid);
                return;
            }
            assert!(
                Instant::now() < until,
                "instance {pid} never became healthy: {doc:#}"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// Takes ownership of a process this sandbox started indirectly (`noeul start`).
    pub fn adopt(&mut self, pid: u32) {
        self.pids.push(pid);
    }

    /// Ends every instance this sandbox started: politely, then by force.
    pub fn stop_all(&mut self) {
        let _ = self.run(&["quit", "--yes"]);
        for mut c in self.children.drain(..) {
            let until = Instant::now() + Duration::from_secs(5);
            while Instant::now() < until {
                if let Ok(Some(_)) = c.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        for pid in self.pids.drain(..) {
            #[cfg(windows)]
            if !noeul::win::wait_exit(pid, Duration::from_secs(5)) {
                kill_pid(pid);
            }
            #[cfg(not(windows))]
            let _ = pid;
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.stop_all();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Force-ends one process this test started (last resort).
#[cfg(windows)]
pub fn kill_pid(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x0800_0000)
        .status();
}
