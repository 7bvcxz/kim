//! E2E — Step 17: the Subak window's tabs, the tab keys and safe close (docs/SUBAK_GUI.md §2,
//! ADR 0042). `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`,
//! `SUBAK-FR-033`, `SUBAK-FR-034`.
//!
//! Scenario A: a user has two Terminals (`subak new`), opens the window, and sees both as tabs.
//! From another console they create, rename and kill Terminals; the tabs follow. `Ctrl+Tab`
//! and `Ctrl+Shift+Tab` switch tabs; typed input reaches only the shown Terminal; nothing ends.
//!
//! Scenario B: `Ctrl+T` creates a Terminal at the window's size; `Ctrl+W` on a busy Terminal (a
//! long-running program, standing in for an agent session — never Claude Code itself) asks
//! first: `Esc` keeps it running, consent ends exactly that Terminal and no other. The window's
//! close button (`WM_CLOSE`) then leaves every Terminal running.
//!
//! The window is the **debug** build's off-screen test window (`--test-offscreen`: placed at
//! (-20000, -20000), a tool window without a taskbar button, never activated), driven only with
//! its own window messages (`WM_CHAR`, `WM_CLOSE`, `WM_COPYDATA` test commands for modified
//! keys and dumps). Nothing is shown on the desktop, no input is synthesized, the system
//! clipboard is not used. Windows only; a private endpoint and `APPDATA` sandbox; everything
//! started is stopped.
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
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;

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
    format!("subak-e2e17-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>` (debug build, next to the debug window).
fn json(exe: &Path, ep: &str, args: &[&str]) -> (i32, Value) {
    let out = Command::new(exe)
        .args(["--json", "--endpoint", ep])
        .args(args)
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

/// The Terminals the Host lists: (name, state, pane pid).
fn terminals(exe: &Path, ep: &str) -> Vec<(String, String, u64)> {
    let (code, doc) = json(exe, ep, &["ls"]);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap().to_string(),
                t["state"].as_str().unwrap().to_string(),
                t["windows"][0]["panes"][0]["pid"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = json(
            self.exe,
            self.ep,
            &["host", "stop", "--kill-terminals", "--yes"],
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
            "180",
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

    fn type_text(&self, s: &str) {
        for u in s.encode_utf16() {
            // SAFETY: posts a character to this scenario's own off-screen window.
            let posted = unsafe { PostMessageW(self.hwnd, WM_CHAR, usize::from(u), 1) };
            assert_ne!(posted, 0);
        }
    }

    /// A test command (a modified key, a dump); returns the dump that answers it.
    fn cmd(&self, c: &Value) -> Value {
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
        self.wait("dump", |_| true)
    }

    fn key(&self, vk: u32, ctrl: bool, shift: bool) -> Value {
        self.cmd(&json!({"cmd": "key", "vk": vk, "ctrl": ctrl, "shift": shift}))
    }

    /// Dumps until `pred` holds.
    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let d = self.cmd(&json!({"cmd": "dump"}));
            if pred(&d) {
                return d;
            }
            assert!(Instant::now() < until, "{what} did not happen: {d:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn close(&mut self) -> i32 {
        // SAFETY: the close message of this scenario's own window (a detach).
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
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

fn tab_names(d: &Value) -> Vec<String> {
    d["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn shown(d: &Value) -> &str {
    d["terminal"]["name"].as_str().unwrap_or("")
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn tabs_follow_the_cli_and_ctrl_tab_switches_without_ending_anything() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("tabs");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e17-tabs");
    for name in ["build", "server"] {
        let (code, doc) = json(&subak, &ep, &["new", name, "--size", "100x30"]);
        assert_eq!(code, 0, "{doc}");
    }
    let pids: Vec<u64> = terminals(&subak, &ep).iter().map(|t| t.2).collect();
    let w = Window::open(&gui, &ep, None, appdata.path());
    w.until("both tabs, build shown", |d| {
        tab_names(d) == ["build", "server"] && d["phase"] == "attached" && shown(d) == "build"
    });
    // Another console creates, renames and kills Terminals: the tabs follow.
    let (code, doc) = json(&subak, &ep, &["new", "logs"]);
    assert_eq!(code, 0, "{doc}");
    w.until("logs appears", |d| {
        tab_names(d) == ["build", "server", "logs"] && shown(d) == "build"
    });
    let (code, doc) = json(&subak, &ep, &["rename", "server", "서버"]);
    assert_eq!(code, 0, "{doc}");
    w.until("the rename shows", |d| {
        tab_names(d) == ["build", "서버", "logs"]
    });
    let (code, doc) = json(&subak, &ep, &["kill", "logs", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    w.until("logs goes", |d| tab_names(d) == ["build", "서버"]);
    // Ctrl+Tab: the next tab; input goes to it only.
    w.key(VK_TAB, true, false);
    w.until("서버 shown", |d| {
        d["phase"] == "attached" && shown(d) == "서버"
    });
    w.type_text("echo e2e17-only-here\r");
    let until = Instant::now() + LONG;
    loop {
        let (_, cap) = json(&subak, &ep, &["capture", "서버"]);
        if cap.to_string().contains("e2e17-only-here") {
            break;
        }
        assert!(Instant::now() < until, "the input reached 서버: {cap}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, cap) = json(&subak, &ep, &["capture", "build"]);
    assert!(!cap.to_string().contains("e2e17-only-here"));
    // Ctrl+Shift+Tab: back. Both Terminals still run with their own processes.
    w.key(VK_TAB, true, true);
    w.until("build again", |d| {
        d["phase"] == "attached" && shown(d) == "build"
    });
    let now = terminals(&subak, &ep);
    assert_eq!(now.len(), 2);
    assert!(now.iter().all(|t| t.1 == "running"), "{now:?}");
    assert_eq!(now.iter().map(|t| t.2).collect::<Vec<_>>(), pids);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn ctrl_t_creates_ctrl_w_asks_before_ending_a_busy_terminal_and_the_x_only_detaches() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("close");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e17-close");
    let (code, doc) = json(&subak, &ep, &["new", "keep"]);
    assert_eq!(code, 0, "{doc}");
    // A busy Terminal: a long-running program as its root (not a shell).
    let (code, doc) = json(
        &subak,
        &ep,
        &["new", "agent", "--", "ping.exe", "-n", "600", "127.0.0.1"],
    );
    assert_eq!(code, 0, "{doc}");
    let mut w = Window::open(&gui, &ep, Some("keep"), appdata.path());
    w.until("keep shown", |d| {
        d["phase"] == "attached" && shown(d) == "keep"
    });
    // Ctrl+T: a new Terminal at the window's size, shown at once.
    w.key(u32::from(b'T'), true, false);
    let d = w.until("the new tab", |d| {
        d["phase"] == "attached" && tab_names(d).len() == 3 && shown(d) != "keep"
    });
    let created = shown(&d).to_string();
    assert_eq!(terminals(&subak, &ep).len(), 3);
    // Ctrl+W on it (an idle shell): ended at once.
    w.key(u32::from(b'W'), true, false);
    w.until("closed", |d| tab_names(d) == ["keep", "agent"]);
    assert!(!terminals(&subak, &ep).iter().any(|t| t.0 == created));
    // Ctrl+W on the busy one: the window asks; Esc keeps it.
    w.key(VK_TAB, true, false);
    w.until("agent shown", |d| {
        d["phase"] == "attached" && shown(d) == "agent"
    });
    w.key(u32::from(b'W'), true, false);
    let d = w.until("the question", |d| !d["confirm"].is_null());
    assert_eq!(d["confirm"]["focus"], "cancel");
    w.key(VK_ESCAPE, false, false);
    w.until("no question", |d| d["confirm"].is_null());
    assert!(
        terminals(&subak, &ep)
            .iter()
            .any(|t| t.0 == "agent" && t.1 == "running")
    );
    // Consent: Tab to "End Terminal", Enter. Only `agent` ends.
    let keep_pid = terminals(&subak, &ep)
        .iter()
        .find(|t| t.0 == "keep")
        .unwrap()
        .2;
    w.key(u32::from(b'W'), true, false);
    w.until("the question", |d| !d["confirm"].is_null());
    w.key(VK_TAB, false, false);
    w.key(VK_RETURN, false, false);
    w.until("agent is gone", |d| tab_names(d) == ["keep"]);
    assert_eq!(
        terminals(&subak, &ep),
        [("keep".to_string(), "running".to_string(), keep_pid)]
    );
    // The window's X: a detach; `keep` runs on with the same process.
    assert_eq!(w.close(), 0);
    assert_eq!(
        terminals(&subak, &ep),
        [("keep".to_string(), "running".to_string(), keep_pid)]
    );
}
