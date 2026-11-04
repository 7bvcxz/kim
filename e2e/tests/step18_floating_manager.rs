//! E2E — Step 18: the floating icon and Terminal manager (`subak-gui --float`,
//! docs/SUBAK_GUI.md §11, ADR 0045). `SUBAK-FR-030`, `SUBAK-FR-032`, `SUBAK-FR-003`,
//! `SUBAK-FR-014`, `SUBAK-NFR-002`, `KIM-FR-012`.
//!
//! Scenario A (no Host, then a working session): a user starts the floating icon before any
//! Host runs. It shows "not running" and starts nothing. They create a Terminal from a console;
//! the icon follows. In the manager they search, open it (a new window), create another one
//! (`Ctrl+N`, opened in the same window), rename it (`F2`) and delete it (asked first; `Esc`
//! keeps it; consent ends exactly that one). Closing the manager and quitting the icon end
//! nothing.
//!
//! Scenario B (placement): the icon is dragged with window messages on a synthetic two-monitor
//! layout, snaps to an edge, keeps its place across runs, and comes back onto a monitor when its
//! monitor disappears (`WM_DISPLAYCHANGE`).
//!
//! The floating UI is the **debug** build's off-screen test mode (`--test-offscreen`: windows on
//! a synthetic layout far off every real monitor, never activated, a fake tray, menus reported
//! not shown), driven only with its own window messages and `WM_COPYDATA` test commands. Nothing
//! is shown on the desktop, no input is synthesized, nothing is added to the notification area.
//! Windows only; a private endpoint and `APPDATA` sandbox; everything started is stopped.
#![cfg(windows)]
#![allow(unsafe_code)]

use kim_e2e::{TempDir, bin_debug};
use serde_json::{Value, json};
use std::io::BufRead;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);
const WM_CHAR: u32 = 0x0102;
const WM_KEYDOWN: u32 = 0x0100;
const WM_COPYDATA: u32 = 0x004A;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_DISPLAYCHANGE: u32 = 0x007E;
const SMTO_ABORTIFHUNG: u32 = 0x0002;
const COPYDATA_TEST: usize = 0x5355_424B;
const MK_LBUTTON: usize = 0x0001;
const VK_TAB: usize = 0x09;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;
const VK_END: usize = 0x23;
const VK_DELETE: usize = 0x2E;
const VK_F2: usize = 0x71;

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
    format!("subak-e2e18-{tag}-{}", std::process::id())
}

fn lp(x: i32, y: i32) -> isize {
    ((y as u16 as isize) << 16) | (x as u16 as isize)
}

fn post(hwnd: isize, msg: u32, wp: usize, l: isize) {
    // SAFETY: posts a message to this scenario's own off-screen window.
    let ok = unsafe { PostMessageW(hwnd, msg, wp, l) };
    assert_ne!(ok, 0);
}

/// `subak --json --endpoint <ep> <args>` (debug build, next to the debug floating UI).
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

/// (id, name, state) of every Terminal.
fn terminals(exe: &Path, ep: &str) -> Vec<(String, String, String)> {
    let (code, doc) = json(exe, ep, &["ls"]);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["id"].as_str().unwrap().to_string(),
                t["name"].as_str().unwrap().to_string(),
                t["state"].as_str().unwrap().to_string(),
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

/// The off-screen floating UI and its events (and those of the test windows it starts).
struct Float {
    child: Child,
    hwnd: isize,
    events: mpsc::Receiver<Value>,
    started: Arc<Mutex<Vec<u32>>>,
}

impl Float {
    fn start(exe: &Path, ep: &str, appdata: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(exe)
            .args([
                "--float",
                "--test-offscreen",
                "--test-max-seconds",
                "180",
                "--endpoint",
                ep,
            ])
            .args(extra)
            .env("APPDATA", appdata)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start subak-gui --float");
        let (tx, events) = mpsc::channel();
        let started = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        std::thread::spawn({
            let started = Arc::clone(&started);
            move || {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v["event"] == "opened"
                            && let Some(p) = v["pid"].as_u64()
                        {
                            started.lock().unwrap().push(p as u32);
                        }
                        let _ = tx.send(v);
                    }
                }
            }
        });
        let pid = child.id();
        let mut f = Self {
            child,
            hwnd: 0,
            events,
            started,
        };
        f.hwnd = f.wait("ready", |v| v["pid"] == pid)["hwnd"]
            .as_u64()
            .unwrap() as isize;
        f
    }

    fn wait(&self, name: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(v) if v["event"] == name && pred(&v) => return v,
                Ok(_) => {}
                Err(_) => panic!("subak-gui --float: no `{name}` event"),
            }
        }
    }

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
        self.wait("dump", |v| v["conn"].is_string())
    }

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

    fn click(&self) {
        post(self.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp(20, 20));
        post(self.hwnd, WM_LBUTTONUP, 0, lp(20, 20));
    }

    fn exit_code(&mut self) -> i32 {
        let until = Instant::now() + LONG;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code().unwrap_or(-1);
            }
            assert!(Instant::now() < until, "subak-gui --float did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Float {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // The test windows it started (they also close themselves after their limit).
        for pid in self.started.lock().unwrap().iter() {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
    }
}

fn ctl(d: &Value, name: &str) -> isize {
    d["manager"]["controls"]
        .as_array()
        .expect("manager open")
        .iter()
        .find(|c| c["control"] == name)
        .expect("control")["hwnd"]
        .as_u64()
        .unwrap() as isize
}

fn key(hwnd: isize, vk: usize) {
    post(hwnd, WM_KEYDOWN, vk, 1);
}

fn type_text(hwnd: isize, s: &str) {
    for u in s.encode_utf16() {
        post(hwnd, WM_CHAR, usize::from(u), 1);
    }
}

fn names(d: &Value) -> Vec<String> {
    d["visible"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_floating_manager_follows_the_host_and_manages_terminals_without_ending_others() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("manager");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e18-manager");
    let mut f = Float::start(&gui, &ep, appdata.path(), &[]);
    let d = f.until("no Host", |d| d["conn"] == "no-host");
    assert_eq!(d["icon"]["badge"], "no-host");
    assert_eq!(d["tray"]["shown"], false);
    let (_, ls) = json(&subak, &ep, &["ls"]);
    assert!(
        ls["data"]["host"].is_null(),
        "the icon started no Host: {ls}"
    );
    // A console creates a Terminal (starting the Host); hovering the icon retries at once.
    let (code, doc) = json(&subak, &ep, &["new", "work", "--size", "100x30"]);
    assert_eq!(code, 0, "{doc}");
    post(f.hwnd, WM_MOUSEMOVE, 0, lp(5, 5));
    f.until("the icon follows", |d| {
        d["conn"] == "connected" && d["entries"].as_array().unwrap().len() == 1
    });
    // The manager: search, then open `work` in a new window.
    f.click();
    f.wait("manager", |v| v["open"] == true);
    let d = f.until("one row", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
    });
    let search = ctl(&d, "search");
    type_text(search, "wo");
    f.until("found", |d| names(d) == ["work"]);
    key(search, VK_RETURN);
    let opened = f.wait("opened", |v| v["how"] == "spawned");
    let window = opened["pid"].as_u64().unwrap();
    f.wait("manager", |v| v["open"] == false);
    f.wait("ready", |v| v["pid"] == window);
    // Ctrl+N: a new Terminal, shown in the same window.
    f.click();
    f.until("open", |d| d["manager"].is_object());
    f.cmd(&json!({"cmd": "key", "to": "list", "vk": 0x4E, "ctrl": true}));
    let opened = f.wait("opened", |v| v["how"] == "existing");
    assert_eq!(opened["pid"].as_u64(), Some(window));
    let created = opened["terminal"].as_str().unwrap().to_string();
    assert_eq!(terminals(&subak, &ep).len(), 2);
    // F2: rename it.
    f.click();
    let d = f.until("two rows", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 2)
    });
    let list = ctl(&d, "list");
    key(list, VK_END);
    f.until("the new one selected", |d| {
        d["selected"] == created.as_str()
    });
    key(list, VK_F2);
    let d = f.until("rename field", |d| d["mode"]["mode"] == "rename");
    let field = ctl(&d, "rename-field");
    type_text(field, "e2e18-빌드");
    key(field, VK_RETURN);
    f.wait("renamed", |v| v["ok"] == true);
    assert!(
        terminals(&subak, &ep)
            .iter()
            .any(|t| t.0 == created && t.1 == "e2e18-빌드")
    );
    // Delete: asked first; Esc keeps it; consent ends exactly that one.
    key(list, VK_DELETE);
    let d = f.until("asked", |d| d["mode"]["mode"] == "ask");
    assert_eq!(d["mode"]["focus"], "cancel");
    key(list, VK_ESCAPE);
    f.until("kept", |d| d["mode"]["mode"] == "browse");
    assert_eq!(terminals(&subak, &ep).len(), 2);
    key(list, VK_DELETE);
    f.until("asked", |d| d["mode"]["mode"] == "ask");
    key(list, VK_TAB);
    key(list, VK_RETURN);
    f.until("ended", |d| d["entries"].as_array().unwrap().len() == 1);
    let now = terminals(&subak, &ep);
    assert_eq!(now.len(), 1);
    assert_eq!((now[0].1.as_str(), now[0].2.as_str()), ("work", "running"));
    // Quit the floating UI: `work` keeps running.
    f.cmd(&json!({"cmd": "close"}));
    assert_eq!(f.exit_code(), 0);
    assert!(
        terminals(&subak, &ep)
            .iter()
            .any(|t| t.1 == "work" && t.2 == "running")
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_icon_is_dragged_snaps_keeps_its_place_and_comes_back_when_a_monitor_goes() {
    let gui = bin_debug("subak-gui");
    let ep = endpoint("place");
    let appdata = TempDir::new("e2e18-place");
    let mons = "-30000,-30000,-28080,-28960,96;-28080,-30000,-25520,-28600,144";
    let mut f = Float::start(&gui, &ep, appdata.path(), &["--test-monitors", mons]);
    let pos = |d: &Value| {
        (
            d["icon"]["pos"][0].as_i64().unwrap() as i32,
            d["icon"]["pos"][1].as_i64().unwrap() as i32,
        )
    };
    let (x0, y0) = pos(&f.cmd(&json!({"cmd": "dump"})));
    // Drag towards the left edge: it snaps onto it.
    post(f.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp(20, 20));
    post(
        f.hwnd,
        WM_MOUSEMOVE,
        MK_LBUTTON,
        lp(-30000 + 5 + 20 - x0, 20 - 150),
    );
    post(f.hwnd, WM_LBUTTONUP, 0, lp(20, 20));
    let dragged = f.wait("dragged", |_| true);
    assert_eq!(dragged["pos"], json!([-30000, y0 - 150]));
    // A new run comes back there.
    f.cmd(&json!({"cmd": "close"}));
    assert_eq!(f.exit_code(), 0);
    let mut f = Float::start(&gui, &ep, appdata.path(), &["--test-monitors", mons]);
    let d = f.cmd(&json!({"cmd": "dump"}));
    assert_eq!(pos(&d), (-30000, y0 - 150));
    // Its monitor goes (a layout of the other one only): it comes back onto the remaining one.
    f.cmd(&json!({"cmd": "monitors", "list": [[-28080, -30000, -25520, -28600, 144]]}));
    post(f.hwnd, WM_DISPLAYCHANGE, 32, lp(2560, 1440));
    let ev = f.wait("display", |_| true);
    assert_eq!(ev["moved"], true);
    let d = f.cmd(&json!({"cmd": "dump"}));
    let (x, y) = pos(&d);
    assert_eq!(d["icon"]["size"], 60, "the remaining monitor is 150 %");
    assert!(
        x >= -28080 && x + 60 <= -25520 && y >= -30000 && y + 60 <= -28600,
        "{d:#}"
    );
    f.cmd(&json!({"cmd": "close"}));
    assert_eq!(f.exit_code(), 0);
}
