//! E2E — Step 20: Subak's settings, appearance, shell profiles and the Milestone B journey
//! (docs/SUBAK_GUI.md §13, docs/MILESTONE_B_VALIDATION.md, ADR 0051). `SUBAK-FR-011`,
//! `SUBAK-FR-031`, `SUBAK-FR-046`, `SUBAK-FR-048`, `SUBAK-FR-049`, `KIM-FR-013`, `KIM-SEC-013`.
//!
//! Scenario A (settings and profiles from the CLI, then the window): a user defines a shell
//! profile in `config.toml`, checks it with `subak config check`, sets the light theme and a
//! larger text with `subak config set`, starts the profile with `subak new --profile` (its
//! folder and environment arrive), opens the window (it draws `kim-light` at the chosen size),
//! starts the default profile with `Ctrl+T`, chooses "Kim dark" in the window's settings menu,
//! closes and reopens the window: every choice is still there; the Terminals kept running.
//!
//! Scenario B (the Milestone B journey): start → open → create, rename, list and end Terminals
//! from the window and the CLI → several tabs → close the window (a detach) → reopen → the same
//! Terminals with the same processes; a broken `config.toml` is reported and never rewritten.
//!
//! The windows are the **debug** build's off-screen test windows (`--test-offscreen`), driven
//! only with their own window messages and `WM_COPYDATA` test commands. A private endpoint and
//! `APPDATA` sandbox: the user's Host and settings are never touched; everything started is
//! stopped. Windows only.
#![cfg(windows)]
#![allow(unsafe_code)]

use kim_e2e::{TempDir, bin_debug};
use serde_json::{Value, json};
use std::io::BufRead;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);
const WM_CLOSE: u32 = 0x0010;
const WM_CHAR: u32 = 0x0102;
const WM_COPYDATA: u32 = 0x004A;
const SMTO_ABORTIFHUNG: u32 = 0x0002;
const COPYDATA_TEST: usize = 0x5355_424B;

#[repr(C)]
struct CopyData {
    data: usize,
    len: u32,
    ptr: *const u8,
}

#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageW(hwnd: isize, msg: u32, wp: usize, lp: isize) -> i32;
    fn SendMessageTimeoutW(
        hwnd: isize,
        msg: u32,
        wp: usize,
        lp: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;
}

fn endpoint(tag: &str) -> String {
    format!("subak-e2e20-{tag}-{}", std::process::id())
}

/// `subak --json <args>` with this scenario's `APPDATA` (debug build, next to the window).
fn json(exe: &Path, appdata: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(exe)
        .arg("--json")
        .args(args)
        .env("APPDATA", appdata)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), doc)
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    appdata: &'a Path,
    ep: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = json(
            self.exe,
            self.appdata,
            &[
                "host",
                "stop",
                "--endpoint",
                self.ep,
                "--kill-terminals",
                "--yes",
            ],
        );
    }
}

/// One off-screen test window.
struct Window {
    child: Child,
    hwnd: isize,
    events: mpsc::Receiver<Value>,
}

impl Window {
    fn open(exe: &Path, ep: &str, target: Option<&str>, appdata: &Path) -> Self {
        let mut cmd = Command::new(exe);
        cmd.args([
            "--test-offscreen",
            "--test-max-seconds",
            "120",
            "--endpoint",
            ep,
        ]);
        if let Some(t) = target {
            cmd.arg(t);
        }
        let mut child = cmd
            .env("APPDATA", appdata)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start subak-gui");
        let (tx, events) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v);
                }
            }
        });
        let mut w = Self {
            child,
            hwnd: 0,
            events,
        };
        w.hwnd = w.wait("ready", |_| true)["hwnd"].as_u64().unwrap() as isize;
        w
    }

    fn wait(&self, name: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(v) if v["event"] == name && pred(&v) => return v,
                Ok(_) => {}
                Err(_) => panic!("subak-gui: no `{name}` event"),
            }
        }
    }

    fn post(&self, msg: u32, wp: usize, lp: isize) {
        // SAFETY: posts a message to this scenario's own off-screen window.
        let posted = unsafe { PostMessageW(self.hwnd, msg, wp, lp) };
        assert_ne!(posted, 0);
    }

    fn type_text(&self, s: &str) {
        for u in s.encode_utf16() {
            self.post(WM_CHAR, usize::from(u), 1);
        }
    }

    fn send(&self, c: &Value) {
        let body = c.to_string();
        let cds = CopyData {
            data: COPYDATA_TEST,
            len: body.len() as u32,
            ptr: body.as_ptr(),
        };
        let mut r = 0usize;
        // SAFETY: a synchronous WM_COPYDATA to this scenario's own window; `cds` and `body`
        // outlive the call.
        let ok = unsafe {
            SendMessageTimeoutW(
                self.hwnd,
                WM_COPYDATA,
                0,
                (&raw const cds) as isize,
                SMTO_ABORTIFHUNG,
                10_000,
                &raw mut r,
            )
        };
        assert_ne!(ok, 0, "WM_COPYDATA");
    }

    fn cmd(&self, c: &Value) -> Value {
        self.send(c);
        self.wait("dump", |_| true)
    }

    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let d = self.cmd(&json!({"cmd": "dump"}));
            if pred(&d) {
                return d;
            }
            assert!(Instant::now() < until, "{what}: {d:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The tab menu's item `path` (`Settings/Theme/Kim dark`): its command id.
    fn menu_id(&self, path: &str) -> u64 {
        self.send(&json!({"cmd": "menu", "tab": 0}));
        let ev = self.wait("menu", |_| true);
        self.wait("dump", |_| true);
        fn find(v: &Value, prefix: &str, path: &str) -> Option<u64> {
            for it in v.as_array()? {
                let p = if prefix.is_empty() {
                    it["label"].as_str()?.to_string()
                } else {
                    format!("{prefix}/{}", it["label"].as_str()?)
                };
                if p == path {
                    return it["id"].as_u64();
                }
                if let Some(id) = find(&it["items"], &p, path) {
                    return Some(id);
                }
            }
            None
        }
        find(&ev["items"], "", path).unwrap_or_else(|| panic!("no menu item {path}"))
    }

    fn close(&mut self) -> i32 {
        self.post(WM_CLOSE, 0, 0);
        let until = Instant::now() + LONG;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code().unwrap_or(-1);
            }
            assert!(Instant::now() < until, "subak-gui did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn screen(d: &Value) -> String {
    d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn settings_and_profiles_from_the_cli_and_the_window_survive_a_restart() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let appdata = TempDir::new("e2e20-settings");
    let ad = appdata.path();
    let ep = endpoint("settings");
    let _cleanup = Cleanup {
        exe: &subak,
        appdata: ad,
        ep: &ep,
    };
    let work = ad.join("작업");
    std::fs::create_dir_all(&work).unwrap();
    let dir = ad.join("mouv").join("subak");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        format!(
            "config_version = 1\n[terminal]\ndefault_profile = \"dev\"\n\n[[profiles]]\nname = \"dev\"\nprogram = \"cmd.exe\"\nargs = [\"/d\", \"/k\", \"echo e2e-%E2E_MODE%\"]\ncwd = '{}'\nenv = {{ E2E_MODE = \"profile\" }}\n",
            work.display()
        ),
    )
    .unwrap();
    let (code, doc) = json(&subak, ad, &["config", "check"]);
    assert_eq!((code, &doc["data"]["valid"]), (0, &json!(true)), "{doc}");
    for (k, v) in [
        ("appearance.theme", "kim-light"),
        ("appearance.font_size", "15"),
    ] {
        let (code, doc) = json(&subak, ad, &["config", "set", k, v]);
        assert_eq!(code, 0, "{doc}");
    }
    let (code, doc) = json(
        &subak,
        ad,
        &["new", "cli", "--profile", "dev", "--endpoint", &ep],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"], "dev");
    let pane = &doc["data"]["terminal"]["windows"][0]["panes"][0];
    assert_eq!(pane["cwd"], work.display().to_string());
    let pid = pane["pid"].as_u64().unwrap();
    // The window draws the settings; Ctrl+T starts the default profile.
    let mut w = Window::open(&gui, &ep, Some("cli"), ad);
    w.wait("state", |v| v["phase"] == "attached");
    let d = w.until("the profile's output", |d| {
        screen(d).contains("e2e-profile")
    });
    assert_eq!(d["theme"], "kim-light");
    assert_eq!(d["font_size"], 15.0);
    w.send(&json!({"cmd": "key", "vk": u32::from(b'T'), "ctrl": true}));
    assert_eq!(w.wait("new_terminal", |_| true)["profile"], "dev");
    w.until("two tabs", |d| d["tabs"].as_array().unwrap().len() == 2);
    // A choice in the window's settings menu is saved where the CLI reads it.
    let id = w.menu_id("Settings/Theme/Kim dark");
    w.send(&json!({"cmd": "menu", "id": id}));
    w.wait("settings_saved", |v| v["key"] == "appearance.theme");
    let (_, doc) = json(&subak, ad, &["config", "show"]);
    assert_eq!(doc["data"]["settings"][0]["value"], "kim-dark");
    assert_eq!(w.close(), 0);
    // Reopened: the same settings; the Terminals kept running with the same process.
    let mut w = Window::open(&gui, &ep, None, ad);
    w.wait("state", |v| v["phase"] == "attached");
    let d = w.until("two tabs again", |d| {
        d["tabs"].as_array().unwrap().len() == 2
    });
    assert_eq!(d["theme"], "kim-dark");
    assert_eq!(d["font_size"], 15.0);
    let (_, doc) = json(&subak, ad, &["info", "cli", "--endpoint", &ep]);
    assert_eq!(
        doc["data"]["terminal"]["windows"][0]["panes"][0]["pid"],
        pid
    );
    assert_eq!(w.close(), 0);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_milestone_b_journey_open_crud_tabs_close_and_reopen() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let appdata = TempDir::new("e2e20-journey");
    let ad = appdata.path();
    let ep = endpoint("journey");
    let _cleanup = Cleanup {
        exe: &subak,
        appdata: ad,
        ep: &ep,
    };
    // Open: no Host runs; the window starts one (without a shell) and shows the empty state.
    let mut w = Window::open(&gui, &ep, None, ad);
    w.wait("state", |v| v["phase"] == "empty");
    // Create from the window (Ctrl+T) and from the CLI; rename; list.
    w.send(&json!({"cmd": "key", "vk": u32::from(b'T'), "ctrl": true}));
    w.wait("state", |v| v["phase"] == "attached");
    let (code, doc) = json(&subak, ad, &["new", "build", "--endpoint", &ep]);
    assert_eq!(code, 0, "{doc}");
    let (code, _) = json(&subak, ad, &["rename", "build", "빌드", "--endpoint", &ep]);
    assert_eq!(code, 0);
    w.until("two tabs", |d| {
        d["tabs"].as_array().unwrap().len() == 2 && d["tabs"].to_string().contains("빌드")
    });
    w.type_text("echo journey-ok\r");
    w.until("typed", |d| screen(d).contains("journey-ok"));
    let (_, ls) = json(&subak, ad, &["ls", "--endpoint", &ep]);
    let pids: Vec<Value> = ls["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["pid"].clone())
        .collect();
    assert_eq!(pids.len(), 2);
    // Close the window: a detach; reopen: the same Terminals, the same processes.
    assert_eq!(w.close(), 0);
    let (_, ls2) = json(&subak, ad, &["ls", "--endpoint", &ep]);
    assert_eq!(ls2["data"]["terminals"].as_array().unwrap().len(), 2);
    let mut w = Window::open(&gui, &ep, None, ad);
    w.wait("state", |v| v["phase"] == "attached");
    let d = w.until("the earlier output", |d| screen(d).contains("journey-ok"));
    assert_eq!(d["tabs"].as_array().unwrap().len(), 2);
    let (_, ls3) = json(&subak, ad, &["ls", "--endpoint", &ep]);
    let again: Vec<Value> = ls3["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["pid"].clone())
        .collect();
    assert_eq!(pids, again, "the same processes after close and reopen");
    // End one Terminal from the CLI (delete); the tab goes.
    let (code, _) = json(&subak, ad, &["kill", "빌드", "--yes", "--endpoint", &ep]);
    assert_eq!(code, 0);
    w.until("one tab", |d| d["tabs"].as_array().unwrap().len() == 1);
    // A broken config.toml is reported and never rewritten (not even by the zoom keys).
    let cfg = ad.join("mouv").join("subak").join("config.toml");
    std::fs::write(&cfg, "[appearance\n").unwrap();
    w.cmd(&json!({"cmd": "focus", "on": false}));
    let d = w.cmd(&json!({"cmd": "focus", "on": true}));
    assert_eq!(d["settings"]["state"], "invalid");
    w.cmd(&json!({"cmd": "zoom", "step": 1}));
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "[appearance\n");
    assert_eq!(w.close(), 0);
}
