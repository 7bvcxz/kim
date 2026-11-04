//! E2E — Step 16: the Subak window (docs/SUBAK_GUI.md, ADR 0039).
//! `SUBAK-FR-003`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-037`, `SUBAK-NFR-002`.
//!
//! Scenario A: a user creates a Terminal with `subak new`, opens it with `subak-gui <name>`,
//! types a command into the window and closes it. The window attached to the same shell process
//! (no second cmd.exe), the input ran in it, and after closing the Terminal still runs with the
//! same pid.
//!
//! Scenario B: the window is reopened without a name (it remembers the Terminal), then its
//! process is ended with `TerminateProcess` (a crash), and reopened again: the Host and the pane
//! keep their pids throughout, and the reopened window shows the earlier output and takes input.
//!
//! The window is the **debug** build's off-screen test window (`--test-offscreen`: placed at
//! (-20000, -20000), a tool window without a taskbar button, never activated), driven only with
//! its own window messages (`WM_CHAR`, `WM_CLOSE`, `WM_COPYDATA` dump requests). Nothing is
//! shown on the desktop, no input is synthesized, the system clipboard is not used. Windows
//! only; a private endpoint and `APPDATA` sandbox; everything started is stopped.
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
    format!("subak-e2e16-{tag}-{}", std::process::id())
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

fn pane_pid(exe: &Path, ep: &str, target: &str) -> (String, u64) {
    let (code, doc) = json(exe, ep, &["info", target]);
    assert_eq!(code, 0, "{doc}");
    let t = &doc["data"]["terminal"];
    (
        t["state"].as_str().unwrap().to_string(),
        t["windows"][0]["panes"][0]["pid"].as_u64().unwrap_or(0),
    )
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

    fn attached(&self) -> u64 {
        self.wait("state", |v| v["phase"] == "attached")["detail"]["pane_pid"]
            .as_u64()
            .unwrap()
    }

    fn type_text(&self, s: &str) {
        for u in s.encode_utf16() {
            // SAFETY: posts a character to this scenario's own off-screen window.
            let posted = unsafe { PostMessageW(self.hwnd, WM_CHAR, usize::from(u), 1) };
            assert_ne!(posted, 0);
        }
    }

    fn dump(&self) -> Value {
        let body = json!({"cmd": "dump"}).to_string();
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

    fn shows_line(&self, line: &str) {
        let until = Instant::now() + LONG;
        loop {
            let d = self.dump();
            if d["screen"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l.as_str().unwrap().trim() == line)
            {
                return;
            }
            assert!(Instant::now() < until, "`{line}` not in the window: {d:#}");
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

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_window_opens_an_existing_terminal_types_into_it_and_closing_keeps_it_running() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("open");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e16-open");
    let (code, doc) = json(&subak, &ep, &["new", "work", "--size", "100x30"]);
    assert_eq!(code, 0, "{doc}");
    let (_, pid) = pane_pid(&subak, &ep, "work");
    let mut w = Window::open(&gui, &ep, Some("work"), appdata.path());
    assert_eq!(
        w.attached(),
        pid,
        "the window shows the Terminal's own shell"
    );
    w.type_text("echo e2e16-%SUBAK_TERMINAL_ID:~0,2%\r");
    w.shows_line("e2e16-t-");
    let (code, cap) = json(&subak, &ep, &["capture", "work"]);
    assert_eq!(code, 0, "{cap}");
    assert!(
        cap.to_string().contains("e2e16-t-"),
        "the shell ran it: {cap}"
    );
    let (_, ls) = json(&subak, &ep, &["ls"]);
    assert_eq!(ls["data"]["terminals"].as_array().unwrap().len(), 1);
    assert_eq!(w.close(), 0);
    assert_eq!(
        pane_pid(&subak, &ep, "work"),
        ("running".to_string(), pid),
        "closing the window is a detach"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_crashed_window_leaves_the_pane_and_a_new_one_reconnects() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("crash");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e16-crash");
    let (code, doc) = json(&subak, &ep, &["new", "keep"]);
    assert_eq!(code, 0, "{doc}");
    let (_, pid) = pane_pid(&subak, &ep, "keep");
    let (_, st) = json(&subak, &ep, &["host", "status"]);
    let host_pid = st["data"]["host"]["pid"].as_u64().unwrap();
    // Opened by name once, so the next window remembers it.
    let mut w = Window::open(&gui, &ep, Some("keep"), appdata.path());
    assert_eq!(w.attached(), pid);
    w.type_text("echo before-crash\r");
    w.shows_line("before-crash");
    assert_eq!(w.close(), 0);
    let mut w = Window::open(&gui, &ep, None, appdata.path());
    assert_eq!(w.attached(), pid, "reopened the remembered Terminal");
    w.shows_line("before-crash");
    // A crash: the process ends at once.
    w.child.kill().unwrap();
    let _ = w.child.wait();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(pane_pid(&subak, &ep, "keep"), ("running".to_string(), pid));
    let (_, st) = json(&subak, &ep, &["host", "status"]);
    assert_eq!(
        st["data"]["host"]["pid"].as_u64(),
        Some(host_pid),
        "the same Host"
    );
    let w = Window::open(&gui, &ep, None, appdata.path());
    assert_eq!(w.attached(), pid);
    w.type_text("echo after-crash\r");
    w.shows_line("after-crash");
}
