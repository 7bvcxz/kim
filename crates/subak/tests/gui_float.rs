//! The floating icon, tray and Terminal manager (`subak-gui --float`, Step 18, ADR 0045) against
//! real Hosts on private endpoints.
//!
//! Every floating UI here is the debug build's off-screen test mode (`--test-offscreen`): its
//! windows sit on a synthetic monitor layout far off every real monitor, never activate, never
//! take the focus or the mouse capture; the tray is the recording fake (no icon appears in the
//! notification area); menus are reported, not shown. It is driven only through its own window
//! messages — `WM_LBUTTONDOWN`/`WM_MOUSEMOVE`/`WM_LBUTTONUP` for clicks and drags,
//! `WM_KEYDOWN`/`WM_CHAR` posted to the manager's controls, `WM_DISPLAYCHANGE`, `WM_COPYDATA`
//! test commands — never with synthesized input. Windows it starts for a Terminal are test
//! windows too. Each test uses its own `APPDATA` sandbox, kills only what it started and stops
//! its Host on drop.
//!
//! Requirements: `SUBAK-FR-030`, `SUBAK-FR-032`, `SUBAK-FR-003`, `SUBAK-FR-014`,
//! `SUBAK-FR-034`, `SUBAK-FR-046`, `SUBAK-NFR-002`, `KIM-FR-012`.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::BufRead;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CHAR, WM_CLOSE, WM_COPYDATA,
    WM_DISPLAYCHANGE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

const COPYDATA_TEST: usize = 0x5355_424B;
const COPYDATA_OPEN: usize = 0x5355_424F;
const MK_LBUTTON: usize = 0x0001;
const VK_TAB: usize = 0x09;
const VK_RETURN: usize = 0x0D;
const VK_ESCAPE: usize = 0x1B;
const VK_END: usize = 0x23;
const VK_HOME: usize = 0x24;
const VK_UP: usize = 0x26;
const VK_DOWN: usize = 0x28;
const VK_DELETE: usize = 0x2E;
const VK_F2: usize = 0x71;
const VK_N: usize = 0x4E;
// The icon's menu commands.
const CMD_ON_TOP: u64 = 4;
const CMD_SHOW_ICON: u64 = 5;
const CMD_SHOW_TRAY: u64 = 6;
const CMD_QUIT: u64 = 7;

fn gui_exe() -> &'static str {
    env!("CARGO_BIN_EXE_subak-gui")
}

/// A private `APPDATA` for one test's `float.json` and `gui.json`.
fn sandbox(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "subak-float-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lp(x: i32, y: i32) -> isize {
    ((y as u16 as isize) << 16) | (x as u16 as isize)
}

fn post(hwnd: isize, msg: u32, wp: usize, l: isize) {
    // SAFETY: posting a message to a test-owned off-screen window.
    let ok = unsafe { PostMessageW(hwnd as _, msg, wp, l) };
    assert_ne!(ok, 0, "PostMessageW {msg:#x}");
}

/// A synchronous `WM_COPYDATA` to a test-owned window; returns its answer.
fn copydata(hwnd: isize, kind: usize, body: &Value) -> usize {
    let text = body.to_string();
    let cds = COPYDATASTRUCT {
        dwData: kind,
        cbData: text.len() as u32,
        lpData: text.as_ptr() as *mut _,
    };
    let mut result = 0usize;
    // SAFETY: `cds` and `text` outlive the synchronous call.
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd as _,
            WM_COPYDATA,
            0,
            (&raw const cds) as isize,
            SMTO_ABORTIFHUNG,
            10_000,
            &raw mut result,
        )
    };
    assert_ne!(ok, 0, "WM_COPYDATA {body}");
    result
}

/// A `subak-gui` test process (the floating UI, or a window) and its JSON-line events. The
/// floating UI's stdout also carries the events of the test windows it starts.
struct Gui {
    child: Child,
    pid: u32,
    hwnd: isize,
    events: mpsc::Receiver<Value>,
    log: Arc<Mutex<Vec<String>>>,
    /// Processes it started (test windows): killed on drop.
    started: Arc<Mutex<Vec<u32>>>,
}

impl Gui {
    fn start(ep: &str, appdata: &Path, args: &[&str]) -> Self {
        let mut cmd = Command::new(gui_exe());
        cmd.args([
            "--test-offscreen",
            "--test-max-seconds",
            "150",
            "--endpoint",
            ep,
        ]);
        cmd.args(args);
        let mut child = cmd
            .env("APPDATA", appdata)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start subak-gui");
        let (tx, events) = mpsc::channel();
        let log = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        std::thread::spawn({
            let log = Arc::clone(&log);
            let started = Arc::clone(&started);
            move || {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    log.lock().unwrap().push(line.clone());
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        let pid = |k: &str| v[k].as_u64().map(|p| p as u32);
                        if matches!(v["event"].as_str(), Some("opened" | "float_started"))
                            && let Some(p) = pid("pid")
                        {
                            started.lock().unwrap().push(p);
                        }
                        let _ = tx.send(v);
                    }
                }
            }
        });
        let pid = child.id();
        let mut g = Self {
            child,
            pid,
            hwnd: 0,
            events,
            log,
            started,
        };
        let ready = g.wait("ready", |v| v["pid"] == pid);
        g.hwnd = ready["hwnd"].as_u64().unwrap() as isize;
        g
    }

    fn history(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    fn wait(&self, name: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(v) if v["event"] == name && pred(&v) => return v,
                Ok(_) => {}
                Err(_) => panic!(
                    "subak-gui pid {}: no `{name}` event as expected; events so far:\n{}",
                    self.pid,
                    self.history()
                ),
            }
        }
    }

    /// A test command to `hwnd` (this process's window, or a test window it started); returns
    /// the `dump` that follows (the floating UI's has `conn`, a window's `phase`).
    fn cmd_to(&self, hwnd: isize, c: &Value, window: bool) -> Value {
        copydata(hwnd, COPYDATA_TEST, c);
        if c["cmd"] == "shot" {
            return self.wait("shot", |_| true);
        }
        self.wait("dump", |v| {
            if window {
                v["phase"].is_string()
            } else {
                v["conn"].is_string()
            }
        })
    }

    fn cmd(&self, c: &Value) -> Value {
        self.cmd_to(self.hwnd, c, false)
    }

    fn dump(&self) -> Value {
        self.cmd(&json!({"cmd": "dump"}))
    }

    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let d = self.dump();
            if pred(&d) {
                return d;
            }
            assert!(
                Instant::now() < until,
                "subak-gui: {what} did not happen; last dump:\n{d:#}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// A click on the floating icon: a press and a release without moving.
    fn click_icon(&self) {
        post(self.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp(20, 20));
        post(self.hwnd, WM_LBUTTONUP, 0, lp(20, 20));
    }

    /// The manager's control `name` (its window handle) from a dump.
    fn ctl(d: &Value, name: &str) -> isize {
        d["manager"]["controls"]
            .as_array()
            .unwrap_or_else(|| panic!("no manager: {d:#}"))
            .iter()
            .find(|c| c["control"] == name)
            .unwrap_or_else(|| panic!("no control {name}: {d:#}"))["hwnd"]
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

    fn exit_code(&mut self) -> i32 {
        let until = Instant::now() + LONG;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code().unwrap_or(-1);
            }
            assert!(Instant::now() < until, "subak-gui did not exit");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn started(&self) -> Vec<u32> {
        self.started.lock().unwrap().clone()
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let mut pids = self.started();
        pids.sort_unstable();
        pids.dedup();
        let running = processes();
        for pid in pids {
            // Only a test window it started (a pid may have been reused by now).
            if running
                .iter()
                .any(|(p, _, n)| *p == pid && n == "subak-gui.exe")
                && let Some(p) = Proc::try_open(pid)
            {
                p.kill();
            }
        }
    }
}

/// A window (`subak-gui` without `--float`) wrapped for its dumps.
struct Window(Gui);

impl Window {
    fn start(ep: &str, appdata: &Path) -> Self {
        Self(Gui::start(ep, appdata, &[]))
    }

    fn dump(&self) -> Value {
        self.0.cmd_to(self.0.hwnd, &json!({"cmd": "dump"}), true)
    }

    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let d = self.dump();
            if pred(&d) {
                return d;
            }
            assert!(Instant::now() < until, "window: {what}; last dump:\n{d:#}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `subak --endpoint <ep> --json <args>`.
fn sub(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    run(&a)
}

fn new_terminal(ep: &str, name: &str) -> (String, u32) {
    let (code, doc) = sub(ep, &["new", name, "--size", "100x30"]);
    assert_eq!(code, 0, "{doc}");
    let t = &doc["data"]["terminal"];
    (
        t["id"].as_str().unwrap().to_string(),
        pid_of(&t["windows"][0]["panes"][0]["pid"]),
    )
}

fn info(ep: &str, target: &str) -> Value {
    let (code, doc) = sub(ep, &["info", target]);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["terminal"].clone()
}

/// (id, name, state, pane pid) of every Terminal.
fn ls(ep: &str) -> Vec<(String, String, String, u64)> {
    let (code, doc) = sub(ep, &["ls"]);
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
                t["pid"].as_u64().unwrap_or(0),
            )
        })
        .collect()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + LONG;
    while !f() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(50));
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

fn float_json(appdata: &Path) -> Value {
    let p = appdata.join("mouv").join("subak").join("float.json");
    serde_json::from_str(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p:?}: {e}")))
        .unwrap()
}

#[test]
fn the_icon_starts_only_on_request_shows_no_host_and_follows_the_cli_without_starting_one() {
    let ep = endpoint("float-nohost");
    let sb = sandbox("nohost");
    let mut f = Gui::start(&ep, &sb, &["--float"]);
    let d = f.until("no Host", |d| d["conn"] == "no-host");
    let icon = &d["icon"];
    // A small layered tool popup that never activates, pinned on top, off every real monitor.
    assert_eq!(icon["toolwindow"], true, "{d:#}");
    assert_eq!(icon["noactivate"], true);
    assert_eq!(icon["layered"], true);
    assert_eq!(icon["topmost"], true);
    assert_eq!(icon["visible"], true);
    assert_eq!(icon["size"], 40);
    assert!(icon["pos"][0].as_i64().unwrap() < -10_000);
    assert_eq!(icon["badge"], "no-host");
    assert!(
        icon["name"].as_str().unwrap().contains("not running"),
        "{d:#}"
    );
    assert_eq!(d["tray"]["shown"], false);
    assert!(d["tray"]["calls"].as_array().unwrap().is_empty());
    // The icon starts no Host.
    std::thread::sleep(Duration::from_millis(1500));
    let (_, doc) = sub(&ep, &["ls"]);
    assert!(doc["data"]["host"].is_null(), "{doc}");
    // The manager in the no-Host state: a sentence and its action, never a blank list.
    f.click_icon();
    f.wait("manager", |v| v["open"] == true);
    let d = f.dump();
    let empty = d["manager"]["controls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["control"] == "empty")
        .unwrap()
        .clone();
    assert_eq!(empty["visible"], true, "{d:#}");
    assert!(
        empty["text"].as_str().unwrap().contains("not running"),
        "{empty}"
    );
    assert!(empty["text"].as_str().unwrap().contains("(Ctrl+N)"));
    assert_eq!(d["manager"]["toolwindow"], true);
    assert_eq!(
        d["manager"]["noactivate"], true,
        "the test manager never activates"
    );
    // A Terminal from the CLI (which starts the Host); hovering the icon retries at once.
    let (work, work_pid) = new_terminal(&ep, "work");
    post(f.hwnd, WM_MOUSEMOVE, 0, lp(5, 5));
    let d = f.until("the icon follows the CLI", |d| {
        d["conn"] == "connected" && d["entries"].as_array().unwrap().len() == 1
    });
    assert_eq!(d["entries"][0]["id"], work.as_str());
    assert_eq!(d["icon"]["badge"], "none");
    assert_eq!(d["icon"]["name"], "Subak — 1 Terminal, 1 running");
    let d = f.until("the row", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 1)
    });
    assert_eq!(d["manager"]["items"][0], "work, running · cmd.exe");
    // Another client's Terminal appears through the events.
    new_terminal(&ep, "둘");
    let d = f.until("둘", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 2)
    });
    assert_eq!(d["manager"]["items"][1], "둘, running · cmd.exe");
    assert_eq!(d["summary"], "2 Terminals · 2 running");
    // Esc in the empty search field closes the manager; it ended nothing.
    Gui::key(Gui::ctl(&d, "search"), VK_ESCAPE);
    f.wait("manager", |v| v["open"] == false);
    // Quit from the icon's menu: the process ends, every Terminal keeps running.
    copydata(
        f.hwnd,
        COPYDATA_TEST,
        &json!({"cmd": "menu", "id": CMD_QUIT}),
    );
    assert_eq!(f.exit_code(), 0);
    let after = ls(&ep);
    assert_eq!(after.len(), 2);
    assert!(after.iter().all(|t| t.2 == "running"));
    assert_eq!(after[0].3, u64::from(work_pid), "the same process");
    let _ = sub(&ep, &["host", "stop", "--kill-terminals", "--yes"]);
}

#[test]
fn the_manager_searches_opens_renames_and_deletes_by_id_asking_first() {
    let host = Host::start("float-crud", &["--no-shell"]);
    let ep = host.ep.clone();
    let (alpha, alpha_pid) = new_terminal(&ep, "alpha");
    let (beta, _) = new_terminal(&ep, "beta");
    let (code, doc) = sub(
        &ep,
        &["new", "busy", "--", "ping", "-n", "1000", "127.0.0.1"],
    );
    assert_eq!(code, 0, "{doc}");
    let busy = doc["data"]["terminal"]["id"].as_str().unwrap().to_string();
    let sb = sandbox("crud");
    let f = Gui::start(&ep, &sb, &["--float"]);
    f.until("three Terminals", |d| {
        d["entries"].as_array().unwrap().len() == 3
    });
    f.click_icon();
    f.wait("manager", |v| v["open"] == true);
    let d = f.until("three rows", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 3)
    });
    // UI Automation names: the system controls' texts and the list box's item strings.
    let text = |name: &str| {
        d["manager"]["controls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["control"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(text("search")["class"], "Edit");
    assert_eq!(text("search-label")["text"], "Search");
    assert_eq!(text("list")["class"], "ListBox");
    assert_eq!(text("list")["text"], "Terminals");
    assert_eq!(text("new")["text"], "New Terminal");
    assert_eq!(text("open")["text"], "Open");
    assert_eq!(text("rename")["text"], "Rename");
    assert_eq!(text("delete")["text"], "Delete…");
    assert_eq!(text("close")["text"], "Close manager");
    assert_eq!(text("pin")["text"], "Pin (keep open: off)");
    assert_eq!(d["manager"]["title"], "Subak — Terminals");
    let items = d["manager"]["items"].as_array().unwrap();
    assert_eq!(items[0], "alpha, running · cmd.exe");
    assert!(
        items[2]
            .as_str()
            .unwrap()
            .to_lowercase()
            .starts_with("busy, running · busy: ping.exe"),
        "{items:?}"
    );
    assert_eq!(d["manager"]["focus"], "search");
    assert_eq!(d["selected"], alpha.as_str());
    let search = Gui::ctl(&d, "search");
    let list = Gui::ctl(&d, "list");
    // Quick search: typed into the field; Esc clears it (the manager stays).
    Gui::type_text(search, "BE");
    let d = f.until("filtered", |d| names(d) == ["beta"]);
    assert_eq!(d["selected"], beta.as_str());
    Gui::key(search, VK_ESCAPE);
    let d = f.until("cleared", |d| d["filter"] == "" && names(d).len() == 3);
    assert!(d["manager"].is_object(), "Esc with text only clears it");
    // Arrows move the selection from the field; typing in the list goes to the search.
    Gui::key(search, VK_DOWN);
    f.until("busy selected", |d| d["selected"] == busy.as_str());
    Gui::type_text(list, "al");
    let d = f.until("typed into search", |d| names(d) == ["alpha"]);
    assert_eq!(d["manager"]["focus"], "search");
    Gui::key(search, VK_ESCAPE);
    f.until("cleared", |d| names(d).len() == 3);
    Gui::key(search, VK_UP);
    f.until("alpha selected", |d| d["selected"] == alpha.as_str());
    // Open (Enter): no window yet, so a new one is started for alpha, and the manager closes.
    Gui::key(search, VK_RETURN);
    let opened = f.wait("opened", |v| v["how"] == "spawned");
    assert_eq!(opened["terminal"], alpha.as_str());
    let win_pid = opened["pid"].as_u64().unwrap() as u32;
    f.wait("manager", |v| v["open"] == false);
    let ready = f.wait("ready", |v| v["pid"] == win_pid);
    let win_hwnd = ready["hwnd"].as_u64().unwrap() as isize;
    wait_for("the new window shows alpha", || {
        info(&ep, &alpha)["clients"] == 1
    });
    // Open beta while that window runs: the window shows it (its tab), no second window.
    f.click_icon();
    f.wait("manager", |v| v["open"] == true);
    let d = f.until("rows", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 3)
    });
    let list = Gui::ctl(&d, "list");
    Gui::key(list, VK_DOWN);
    f.until("beta selected", |d| d["selected"] == beta.as_str());
    Gui::key(list, VK_RETURN);
    // The window answers during the request, so its event comes first on the shared output.
    let req = f.wait("open_request", |v| v["terminal"] == beta.as_str());
    assert_eq!(req["foreground"], "skipped (test)");
    let opened = f.wait("opened", |v| v["how"] == "existing");
    assert_eq!(opened["pid"], win_pid);
    assert_eq!(opened["terminal"], beta.as_str());
    assert!(opened["allow_foreground"].is_boolean());
    wait_for("the window switched to beta", || {
        info(&ep, &beta)["clients"] == 1 && info(&ep, &alpha)["clients"] == 0
    });
    let w = f.cmd_to(win_hwnd, &json!({"cmd": "dump"}), true);
    assert_eq!(w["terminal"]["id"], beta.as_str(), "{w:#}");
    // Rename (F2): the field over the row, Korean text, Enter saves by id.
    f.click_icon();
    f.wait("manager", |v| v["open"] == true);
    let d = f.until("rows", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 3)
    });
    let list = Gui::ctl(&d, "list");
    Gui::key(list, VK_HOME);
    Gui::key(list, VK_DOWN);
    f.until("beta selected", |d| d["selected"] == beta.as_str());
    Gui::key(list, VK_F2);
    let d = f.until("rename field", |d| d["mode"]["mode"] == "rename");
    assert_eq!(d["manager"]["rename_text"], "beta");
    let field = Gui::ctl(&d, "rename-field");
    Gui::type_text(field, "베타");
    f.until("typed", |d| d["manager"]["rename_text"] == "베타");
    Gui::key(field, VK_RETURN);
    f.wait("renamed", |v| v["ok"] == true);
    assert_eq!(info(&ep, &beta)["name"], "베타");
    let d = f.until("renamed row", |d| {
        d["manager"]["items"][1]
            .as_str()
            .is_some_and(|s| s.starts_with("베타, running"))
    });
    // Delete the busy one: asked first with what ends; Esc keeps it.
    let list = Gui::ctl(&d, "list");
    Gui::key(list, VK_END);
    f.until("busy selected", |d| d["selected"] == busy.as_str());
    Gui::key(list, VK_DELETE);
    let d = f.until("asked", |d| d["mode"]["mode"] == "ask");
    assert_eq!(d["mode"]["focus"], "cancel", "Cancel is the default");
    assert!(
        d["mode"]["text"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("ping.exe"),
        "{d:#}"
    );
    assert_eq!(d["manager"]["focus"], "cancel");
    // Keys typed during the question reach nothing.
    Gui::type_text(Gui::ctl(&d, "search"), "zz");
    Gui::key(list, VK_ESCAPE);
    let d = f.until("cancelled", |d| d["mode"]["mode"] == "browse");
    assert_eq!(d["filter"], "");
    assert!(ls(&ep).iter().any(|t| t.0 == busy && t.2 == "running"));
    // Delete again, Tab to "End Terminal", Enter: exactly that Terminal ends.
    Gui::key(list, VK_DELETE);
    f.until("asked", |d| d["mode"]["mode"] == "ask");
    Gui::key(list, VK_TAB);
    f.until("End focused", |d| d["mode"]["focus"] == "end");
    Gui::key(list, VK_RETURN);
    f.until("busy gone", |d| d["entries"].as_array().unwrap().len() == 2);
    let now = ls(&ep);
    assert!(!now.iter().any(|t| t.0 == busy));
    assert!(
        now.iter()
            .any(|t| t.0 == alpha && t.3 == u64::from(alpha_pid))
    );
    assert!(now.iter().any(|t| t.0 == beta));
    // An idle shell asks too (DESIGN_SYSTEM §3.6), and consent ends it without force.
    Gui::key(list, VK_END);
    let d = f.until("beta selected", |d| d["selected"] == beta.as_str());
    let list = Gui::ctl(&d, "list");
    Gui::key(list, VK_DELETE);
    let d = f.until("asked", |d| d["mode"]["mode"] == "ask");
    assert_eq!(
        d["mode"]["text"],
        "End `베타`? Its shell (cmd.exe) will be closed."
    );
    Gui::key(list, VK_TAB);
    Gui::key(list, VK_RETURN);
    f.until("beta gone", |d| d["entries"].as_array().unwrap().len() == 1);
    assert_eq!(ls(&ep).len(), 1);
    // The window that showed beta keeps running with alpha's tab only.
    drop(f);
    assert!(ls(&ep).iter().any(|t| t.0 == alpha && t.2 == "running"));
    drop(host);
}

#[test]
fn new_terminal_from_the_manager_starts_a_host_on_demand_and_opens_it() {
    let ep = endpoint("float-new");
    let sb = sandbox("new");
    let f = Gui::start(&ep, &sb, &["--float"]);
    f.until("no Host", |d| d["conn"] == "no-host");
    f.click_icon();
    let d = f.until("manager", |d| d["manager"].is_object());
    // Ctrl+N (the key command carries the modifier; a posted key has no Ctrl state).
    copydata(
        f.hwnd,
        COPYDATA_TEST,
        &json!({"cmd": "key", "to": "list", "vk": VK_N, "ctrl": true}),
    );
    let _ = d;
    let opened = f.wait("opened", |v| v["how"] == "spawned");
    let t = opened["terminal"].as_str().unwrap().to_string();
    let terms = ls(&ep);
    assert_eq!(terms.len(), 1, "exactly one Terminal");
    assert_eq!(terms[0].0, t);
    let win = opened["pid"].as_u64().unwrap() as u32;
    f.wait("ready", |v| v["pid"] == win);
    wait_for("the new window shows it", || info(&ep, &t)["clients"] == 1);
    drop(f);
    let _ = sub(&ep, &["host", "stop", "--kill-terminals", "--yes"]);
}

#[test]
fn drag_snaps_and_saves_and_display_and_dpi_changes_keep_the_icon_on_a_monitor() {
    let ep = endpoint("float-drag");
    let sb = sandbox("drag");
    // Two synthetic monitors far off every real one: 1920 × 1040 at 96 DPI and, to its right,
    // 2560 × 1400 at 144 DPI.
    let mons = "-30000,-30000,-28080,-28960,96;-28080,-30000,-25520,-28600,144";
    let mut f = Gui::start(&ep, &sb, &["--float", "--test-monitors", mons]);
    let d = f.dump();
    assert_eq!(
        d["icon"]["pos"],
        json!([-28080 - 40 - 24, -28960 - 40 - 24]),
        "{d:#}"
    );
    assert_eq!(d["icon"]["size"], 40);
    let pos = |d: &Value| {
        (
            d["icon"]["pos"][0].as_i64().unwrap() as i32,
            d["icon"]["pos"][1].as_i64().unwrap() as i32,
        )
    };
    // Moves the pressed cursor to a screen point (client coordinates of the icon's current place).
    let move_to = |f: &Gui, x: i32, y: i32| {
        let p = pos(&f.dump());
        post(f.hwnd, WM_MOUSEMOVE, MK_LBUTTON, lp(x - p.0, y - p.1));
    };
    let (x0, y0) = pos(&d);
    post(f.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp(20, 20));
    move_to(&f, x0 + 20 - 300, y0 + 20 - 200);
    let d = f.until("moved", |d| pos(d) == (x0 - 300, y0 - 200));
    assert!(d["manager"].is_null(), "a drag is not a click");
    // Within 8 px of the left edge: snapped onto it.
    move_to(&f, -30000 + 5 + 20, y0 + 20 - 200);
    f.until("snapped", |d| pos(d).0 == -30000);
    post(f.hwnd, WM_LBUTTONUP, 0, lp(20, 20));
    let dragged = f.wait("dragged", |_| true);
    assert_eq!(dragged["pos"], json!([-30000, y0 - 200]));
    assert!(
        f.dump()["manager"].is_null(),
        "releasing a drag opens nothing"
    );
    let saved = float_json(&sb);
    assert_eq!(
        saved["position"],
        json!({"x": -30000, "y": y0 - 200, "dpi": 96})
    );
    // Onto the 144 DPI monitor: 60 px there, saved with its DPI.
    post(f.hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp(20, 20));
    move_to(&f, -27000, -29500);
    let d = f.until("on the 150 % monitor", |d| d["icon"]["dpi"] == 144);
    assert_eq!(d["icon"]["size"], 60);
    post(f.hwnd, WM_LBUTTONUP, 0, lp(20, 20));
    f.wait("dragged", |v| v["dpi"] == 144);
    let saved = float_json(&sb)["position"].clone();
    assert_eq!(saved["dpi"], 144);
    // A new run comes back at the saved place.
    copydata(f.hwnd, COPYDATA_TEST, &json!({"cmd": "close"}));
    assert_eq!(f.exit_code(), 0);
    let f = Gui::start(&ep, &sb, &["--float", "--test-monitors", mons]);
    let d = f.dump();
    assert_eq!(d["icon"]["pos"], json!([saved["x"], saved["y"]]));
    assert_eq!(d["icon"]["size"], 60);
    // The 144 DPI monitor goes away (a synthetic layout, then a real WM_DISPLAYCHANGE): the icon
    // comes back fully onto the remaining monitor, at its DPI; the saved place is kept for when
    // the monitor returns.
    f.cmd(&json!({"cmd": "monitors", "list": [[-30000, -30000, -28080, -28960, 96]]}));
    post(f.hwnd, WM_DISPLAYCHANGE, 32, lp(1920, 1080));
    let ev = f.wait("display", |v| v["moved"] == true);
    assert_eq!(ev["dpi"], 96);
    let d = f.dump();
    let (x, y) = pos(&d);
    assert_eq!(d["icon"]["size"], 40);
    assert!(
        (-30000..=-28080 - 40).contains(&x) && (-30000..=-28960 - 40).contains(&y),
        "{d:#}"
    );
    assert_eq!(x, -28080 - 40, "at the remaining monitor's right edge");
    assert_eq!(
        float_json(&sb)["position"],
        saved,
        "the saved place is kept"
    );
    // A display change that leaves it on screen moves nothing.
    post(f.hwnd, WM_DISPLAYCHANGE, 32, lp(1920, 1080));
    f.wait("display", |v| v["moved"] == false);
    // A synthetic DPI change: 200 % is 80 px.
    let d = f.cmd(&json!({"cmd": "dpi", "dpi": 192}));
    assert_eq!(d["icon"]["size"], 80);
    // The manager opens beside the icon, inside the synthetic work area.
    f.cmd(&json!({"cmd": "dpi", "dpi": 96}));
    f.click_icon();
    let ev = f.wait("manager", |v| v["open"] == true);
    let r = &ev["rect"];
    assert!(
        r[0].as_i64().unwrap() >= -30000 && r[2].as_i64().unwrap() <= -28080,
        "{ev}"
    );
    assert!(
        r[1].as_i64().unwrap() >= -30000 && r[3].as_i64().unwrap() <= -28960,
        "{ev}"
    );
    // A layout on a real monitor is refused in the test mode.
    copydata(
        f.hwnd,
        COPYDATA_TEST,
        &json!({"cmd": "monitors", "list": [[0, 0, 1920, 1080, 96]]}),
    );
    f.wait("error", |v| {
        v["message"]
            .as_str()
            .is_some_and(|m| m.contains("off every real monitor"))
    });
}

#[test]
fn repeated_quick_open_close_leaks_nothing_and_the_idle_icon_and_manager_cost_no_cpu() {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{GetProcessHandleCount, GetProcessTimes};
    let host = Host::start("float-idle", &["--no-shell"]);
    let ep = host.ep.clone();
    for n in ["a", "b", "c"] {
        new_terminal(&ep, n);
    }
    let f = Gui::start(&ep, &sandbox("idle"), &["--float"]);
    f.until("connected", |d| d["entries"].as_array().unwrap().len() == 3);
    // Open and close once to load everything the manager needs, then 30 quick cycles.
    f.click_icon();
    f.click_icon();
    let before = f.until("closed", |d| d["manager"].is_null());
    for _ in 0..30 {
        f.click_icon();
        f.click_icon();
    }
    // Posted clicks are handled after this test's (sent) dump requests: wait until all are.
    let opens = || {
        f.history()
            .lines()
            .filter(|l| l.contains(r#""event":"manager""#) && l.contains(r#""open":true"#))
            .count()
    };
    wait_for("every click toggled the manager", || opens() == 31);
    let after = f.until("closed again", |d| d["manager"].is_null());
    let closes = f
        .history()
        .lines()
        .filter(|l| l.contains(r#""event":"manager""#) && l.contains(r#""open":false"#))
        .count();
    assert_eq!((opens(), closes), (31, 31));
    let (g0, g1) = (
        before["gdi_objects"].as_i64().unwrap(),
        after["gdi_objects"].as_i64().unwrap(),
    );
    let (u0, u1) = (
        before["user_objects"].as_i64().unwrap(),
        after["user_objects"].as_i64().unwrap(),
    );
    println!("EVIDENCE open/close x30: GDI objects {g0} -> {g1}, USER objects {u0} -> {u1}");
    assert!(g1 <= g0 + 2, "GDI objects leak: {g0} -> {g1}");
    assert!(u1 <= u0 + 2, "USER objects leak: {u0} -> {u1}");
    let p = Proc::open(f.pid);
    let sample = || {
        let ft = |t: FILETIME| {
            ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 10_000.0
        };
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let mut pm = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let mut handles = 0;
        use std::os::windows::io::AsRawHandle;
        let h = p.h.as_raw_handle();
        // SAFETY: queries on a process handle we own; out-structures sized correctly.
        unsafe {
            GetProcessTimes(h, &raw mut c, &raw mut e, &raw mut k, &raw mut u);
            GetProcessMemoryInfo(h, (&raw mut pm).cast(), pm.cb);
            GetProcessHandleCount(h, &raw mut handles);
        }
        (ft(k) + ft(u), pm.PrivateUsage, pm.WorkingSetSize, handles)
    };
    let mib = |b: usize| b as f64 / 1_048_576.0;
    let secs = 10;
    let measure = |what: &str| {
        std::thread::sleep(Duration::from_secs(1));
        let (c0, ..) = sample();
        std::thread::sleep(Duration::from_secs(secs));
        let (c1, private, ws, handles) = sample();
        let n = threads(f.pid);
        println!(
            "EVIDENCE idle {what} (debug build, off-screen, {secs} s): CPU {:.1} ms; private {:.2} MiB; working set {:.2} MiB; threads {n}; handles {handles}",
            c1 - c0,
            mib(private),
            mib(ws)
        );
        assert!(c1 - c0 <= 47.0, "{what}: idle CPU {:.1} ms", c1 - c0);
        assert!(
            mib(private) < 32.0,
            "{what}: private {:.1} MiB",
            mib(private)
        );
        assert!(n <= 8, "{what}: {n} threads");
    };
    measure("icon, manager closed");
    f.click_icon();
    f.until("open", |d| d["manager"].is_object());
    measure("icon and manager open");
    drop(f);
    drop(host);
}

/// Threads of a process (Toolhelp).
fn threads(pid: u32) -> usize {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    // SAFETY: Toolhelp snapshot, closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let mut e = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
        ..Default::default()
    };
    let mut n = 0;
    // SAFETY: `e.dwSize` is set.
    let mut more = unsafe { Thread32First(snap, &raw mut e) } != 0;
    while more {
        if e.th32OwnerProcessID == pid {
            n += 1;
        }
        // SAFETY: same snapshot and entry.
        more = unsafe { Thread32Next(snap, &raw mut e) } != 0;
    }
    // SAFETY: valid snapshot handle.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(snap) };
    n
}

#[test]
fn the_tray_is_a_fake_in_tests_and_the_menu_switches_pin_and_presence_and_quits_ending_nothing() {
    let host = Host::start("float-tray", &["--no-shell"]);
    let ep = host.ep.clone();
    let (t, pid) = new_terminal(&ep, "one");
    let sb = sandbox("tray");
    let mut f = Gui::start(&ep, &sb, &["--float", "--tray"]);
    let d = f.until("tray tip", |d| {
        d["tray"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["tip"] == "Subak — 1 Terminal, 1 running")
    });
    assert_eq!(d["tray"]["shown"], true);
    assert_eq!(d["tray"]["calls"][0]["call"], "add");
    assert_eq!(d["icon"]["visible"], false, "tray only");
    let p = float_json(&sb);
    assert_eq!(
        (p["icon"].clone(), p["tray"].clone()),
        (json!(false), json!(true))
    );
    // The tray's keyboard selection (NIN_KEYSELECT) opens the manager near it.
    copydata(
        f.hwnd,
        COPYDATA_TEST,
        &json!({"cmd": "tray", "code": 0x401, "x": -18100, "y": -18980}),
    );
    f.wait("manager", |v| v["open"] == true);
    // The menu says what is on.
    copydata(f.hwnd, COPYDATA_TEST, &json!({"cmd": "menu"}));
    let m = f.wait("menu", |_| true);
    let item = |id: u64| {
        m["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(item(CMD_ON_TOP)["checked"], true);
    assert_eq!(item(CMD_SHOW_ICON)["checked"], false);
    assert_eq!(item(CMD_SHOW_TRAY)["checked"], true);
    assert_eq!(item(CMD_QUIT)["label"], "Quit (Terminals keep running)");
    // Show the floating icon too; unpin (not on top): the icon and the open manager follow.
    f.cmd(&json!({"cmd": "menu", "id": CMD_SHOW_ICON}));
    f.until("icon shown", |d| d["icon"]["visible"] == true);
    f.cmd(&json!({"cmd": "menu", "id": CMD_ON_TOP}));
    f.until("icon unpinned", |d| d["icon"]["topmost"] == false);
    let d = f.until("manager unpinned", |d| d["manager"]["topmost"] == false);
    assert_eq!(d["prefs"]["on_top"], false);
    // The manager's pin keeps it open when it loses the focus; unpinned it closes.
    let d = f.cmd(&json!({"cmd": "key", "to": "list", "vk": 0x50, "ctrl": true}));
    assert_eq!(d["prefs"]["manager_pinned"], true);
    let d = f.cmd(&json!({"cmd": "deactivate"}));
    assert!(d["manager"].is_object(), "pinned: stays open");
    f.cmd(&json!({"cmd": "key", "to": "list", "vk": 0x50, "ctrl": true}));
    let d = f.cmd(&json!({"cmd": "deactivate"}));
    assert!(d["manager"].is_null(), "unpinned: closes on focus loss");
    // Tray off, then icon off: nothing is left, so the floating UI quits — and ends nothing.
    f.cmd(&json!({"cmd": "menu", "id": CMD_SHOW_TRAY}));
    let d = f.until("tray off", |d| d["tray"]["shown"] == false);
    assert_eq!(
        d["tray"]["calls"].as_array().unwrap().last().unwrap()["call"],
        "delete"
    );
    copydata(
        f.hwnd,
        COPYDATA_TEST,
        &json!({"cmd": "menu", "id": CMD_SHOW_ICON}),
    );
    assert_eq!(f.exit_code(), 0);
    let now = ls(&ep);
    assert_eq!(now.len(), 1);
    assert_eq!((now[0].0.as_str(), now[0].3), (t.as_str(), u64::from(pid)));
    assert!(host.proc.alive());
    let p = float_json(&sb);
    assert_eq!(
        (p["icon"].clone(), p["tray"].clone()),
        (json!(false), json!(false))
    );
}

#[test]
fn a_second_start_forwards_to_the_running_one_and_neither_cli_nor_host_starts_it() {
    let host = Host::start("float-single", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "one");
    let sb = sandbox("single");
    let f = Gui::start(&ep, &sb, &["--float"]);
    f.until("connected", |d| d["conn"] == "connected");
    // The Host and the CLI never started a floating UI or a window.
    let guis: Vec<_> = processes()
        .into_iter()
        .filter(|(_, parent, name)| name == "subak-gui.exe" && *parent == host.proc.pid)
        .collect();
    assert!(guis.is_empty(), "{guis:?}");
    // `subak-gui --manager` for the same endpoint forwards to the running one and exits.
    let out = Command::new(gui_exe())
        .args(["--manager", "--test-offscreen", "--endpoint", &ep])
        .env("APPDATA", &sb)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(r#""event":"forwarded""#), "{text}");
    f.wait("forwarded", |v| v["op"] == "manager");
    f.wait("manager", |v| v["open"] == true);
    // Requests for another endpoint are not answered; ours is.
    let other = json!({"op": "probe", "endpoint": "someone-else"});
    assert_eq!(copydata(f.hwnd, COPYDATA_OPEN, &other), 0);
    let ours = json!({"op": "probe", "endpoint": ep});
    assert_eq!(copydata(f.hwnd, COPYDATA_OPEN, &ours), 1);
    // An oversized request is ignored.
    let big = json!({"op": "probe", "endpoint": ep, "pad": "x".repeat(8000)});
    assert_eq!(copydata(f.hwnd, COPYDATA_OPEN, &big), 0);
}

#[test]
fn hidden_tabs_persist_across_runs_and_the_manager_shows_a_hidden_terminal_in_its_window() {
    let host = Host::start("float-hidden", &["--no-shell"]);
    let ep = host.ep.clone();
    let (one, _) = new_terminal(&ep, "one");
    let (two, _) = new_terminal(&ep, "two");
    let sb = sandbox("hidden");
    let mut w = Window::start(&ep, &sb);
    w.until("two tabs", |d| d["tabs"].as_array().unwrap().len() == 2);
    // Hide `two` (the tab menu's "Hide tab"): a detach; it keeps running.
    w.0.cmd_to(w.0.hwnd, &json!({"cmd": "menu", "id": 2, "tab": 1}), true);
    let d = w.until("hidden", |d| d["hidden"].as_array().unwrap().len() == 1);
    assert_eq!(d["hidden"][0]["id"], two.as_str());
    let gui: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.join("mouv").join("subak").join("gui.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(gui["hidden_terminals"], json!([two]));
    // A new run keeps it hidden.
    post(w.0.hwnd, WM_CLOSE, 0, 0);
    assert_eq!(w.0.exit_code(), 0);
    let w = Window::start(&ep, &sb);
    let d = w.until("one tab", |d| {
        d["tabs"].as_array().unwrap().len() == 1 && d["hidden"].as_array().unwrap().len() == 1
    });
    assert_eq!(d["tabs"][0]["id"], one.as_str());
    assert!(ls(&ep).iter().any(|t| t.0 == two && t.2 == "running"));
    // The manager lists it anyway and opening it shows its tab in that window.
    let f = Gui::start(&ep, &sb, &["--float"]);
    f.until("two entries", |d| {
        d["entries"].as_array().unwrap().len() == 2
    });
    f.click_icon();
    let d = f.until("rows", |d| {
        d["manager"]["items"]
            .as_array()
            .is_some_and(|i| i.len() == 2)
    });
    let list = Gui::ctl(&d, "list");
    Gui::key(list, VK_DOWN);
    f.until("two selected", |d| d["selected"] == two.as_str());
    Gui::key(list, VK_RETURN);
    let opened = f.wait("opened", |v| v["how"] == "existing");
    assert_eq!(opened["pid"], w.0.pid);
    w.0.wait("open_request", |v| v["terminal"] == two.as_str());
    let d = w.until("shown again", |d| {
        d["hidden"].as_array().unwrap().is_empty() && d["terminal"]["id"] == two.as_str()
    });
    assert_eq!(d["tabs"].as_array().unwrap().len(), 2);
    let gui: Value = serde_json::from_str(
        &std::fs::read_to_string(sb.join("mouv").join("subak").join("gui.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(gui["hidden_terminals"], json!([]));
}

#[test]
fn a_window_menu_starts_the_floating_ui_for_its_endpoint_once() {
    let host = Host::start("float-from-window", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "one");
    let sb = sandbox("from-window");
    let w = Window::start(&ep, &sb);
    w.until("one tab", |d| d["tabs"].as_array().unwrap().len() == 1);
    // "Floating icon and Terminal manager" in the tab menu (command 7): the user's choice.
    copydata(w.0.hwnd, COPYDATA_TEST, &json!({"cmd": "menu", "id": 7}));
    let started = w.0.wait("float_started", |_| true);
    let pid = started["pid"].as_u64().unwrap();
    // Its events arrive on the window's output (the test mode inherits it): it serves this
    // endpoint, off-screen, and connects to the running Host.
    let ready = w.0.wait("ready", |v| v["pid"] == pid);
    assert_eq!(ready["endpoint"], ep.as_str());
    assert!(ready["pos"][0].as_i64().unwrap() < -10_000);
    w.0.wait("connected", |_| true);
    // A second request only shows the running one: the new process forwards and exits.
    copydata(w.0.hwnd, COPYDATA_TEST, &json!({"cmd": "menu", "id": 7}));
    let second = w.0.wait("float_started", |_| true)["pid"].as_u64().unwrap();
    w.0.wait("forwarded", |v| v["op"] == "show");
    wait_for("the second one exits", || {
        Proc::try_open(second as u32).is_none_or(|p| !p.alive())
    });
    assert!(
        Proc::try_open(pid as u32).is_some_and(|p| p.alive()),
        "the first keeps running"
    );
}
