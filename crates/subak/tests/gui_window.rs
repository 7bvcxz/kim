//! The Subak window (`subak-gui`, Step 16, ADR 0039) against real Hosts on private endpoints.
//!
//! Every window here is the debug build's off-screen test window (`--test-offscreen`): placed at
//! (-20000, -20000), a tool window without a taskbar button, never activated, closed by itself
//! after `--test-max-seconds`. It is driven only through its own window messages — `WM_CHAR`
//! and `WM_KEYDOWN` posted to it, `WM_COPYDATA` test commands — never with synthesized input,
//! and it never touches the system clipboard. Its frames are its own content rendered to memory;
//! nothing captures the screen. Each test uses its own `APPDATA` sandbox for `gui.json`, kills
//! only the processes it started, and stops its Host on drop.
//!
//! Requirements: `SUBAK-FR-003`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-037`,
//! `SUBAK-FR-046`, `SUBAK-NFR-002`; since Step 17 (tabs, the tab keys, safe close, renames,
//! `hotkeys.json`): `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-033`,
//! `SUBAK-FR-034`. A "running Claude process" is played by `standin_helper` (this test binary
//! started again in a pane), never by Claude Code.
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
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SIZE,
};

const COPYDATA_TEST: usize = 0x5355_424B;
const VK_RETURN: usize = 0x0D;

fn gui_exe() -> &'static str {
    env!("CARGO_BIN_EXE_subak-gui")
}

/// A private `APPDATA` for one test's `gui.json`.
fn sandbox(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "subak-gui-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// One off-screen test window and its event stream.
struct Gui {
    child: Child,
    pid: u32,
    hwnd: isize,
    events: mpsc::Receiver<Value>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Gui {
    fn start(ep: &str, target: Option<&str>, appdata: &Path) -> Self {
        Self::start_with(ep, target, appdata, &[])
    }

    fn start_with(ep: &str, target: Option<&str>, appdata: &Path, extra: &[&str]) -> Self {
        let mut cmd = Command::new(gui_exe());
        cmd.args([
            "--test-offscreen",
            "--test-max-seconds",
            "120",
            "--endpoint",
            ep,
        ]);
        cmd.args(extra);
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
        let log = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        std::thread::spawn({
            let log = Arc::clone(&log);
            move || {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    log.lock().unwrap().push(line.clone());
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
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
        };
        let ready = g.wait("ready", |_| true);
        g.hwnd = ready["hwnd"].as_u64().unwrap() as isize;
        assert_eq!(ready["pid"], pid);
        g
    }

    fn history(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// The next event named `name` for which `pred` holds (earlier events are skipped).
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

    fn state(&self, phase: &str) -> Value {
        self.wait("state", |v| v["phase"] == phase)
    }

    fn post(&self, msg: u32, wp: usize, lp: isize) {
        // SAFETY: posting a message to the test's own off-screen window.
        let ok = unsafe { PostMessageW(self.hwnd as _, msg, wp, lp) };
        assert_ne!(ok, 0, "PostMessageW");
    }

    /// Types text as `WM_CHAR` messages (UTF-16 units), as a keyboard layout produces them.
    fn type_text(&self, s: &str) {
        for u in s.encode_utf16() {
            self.post(WM_CHAR, usize::from(u), 1);
        }
    }

    fn key(&self, vk: usize) {
        self.post(WM_KEYDOWN, vk, 1);
    }

    /// Sends a test command and returns its reply (`dump`, or `shot`).
    fn cmd(&self, c: &Value) -> Value {
        let body = c.to_string();
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_TEST,
            cbData: body.len() as u32,
            lpData: body.as_ptr() as *mut _,
        };
        let mut result = 0usize;
        // SAFETY: a synchronous WM_COPYDATA to the test window; `cds` and `body` outlive it.
        let ok = unsafe {
            SendMessageTimeoutW(
                self.hwnd as _,
                WM_COPYDATA,
                0,
                (&raw const cds) as isize,
                SMTO_ABORTIFHUNG,
                10_000,
                &raw mut result,
            )
        };
        assert_ne!(ok, 0, "WM_COPYDATA {c}");
        let reply = if c["cmd"] == "shot" { "shot" } else { "dump" };
        self.wait(reply, |_| true)
    }

    fn dump(&self) -> Value {
        self.cmd(&json!({"cmd": "dump"}))
    }

    /// Dumps until `pred` holds.
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

    /// Closes the window as the user would (`WM_CLOSE`: the close button, Alt+F4).
    fn close(&mut self) -> i32 {
        self.post(WM_CLOSE, 0, 0);
        self.exit_code()
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

    /// Ends the window's process at once (a crash): `TerminateProcess`.
    fn crash(&mut self) {
        let p = Proc::open(self.pid);
        p.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn screen_text(d: &Value) -> String {
    d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_line(d: &Value, line: &str) -> bool {
    d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().trim() == line)
}

/// `subak --endpoint <ep> --json <args>`.
fn sub(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    run(&a)
}

/// A Terminal running cmd.exe; returns its id and pane pid.
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

fn pane(t: &Value) -> &Value {
    &t["windows"][0]["panes"][0]
}

/// The Host-side screen of a Terminal (`subak capture`): what the pane really printed.
fn capture(ep: &str, target: &str) -> String {
    let (code, doc) = sub(ep, &["capture", target]);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + LONG;
    while !f() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Processes whose parent is `pid`, by name.
fn children(pid: u32, name: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, parent, n)| *parent == pid && n == name)
        .map(|(p, _, _)| p)
        .collect()
}

#[test]
fn the_window_opens_an_existing_terminal_and_types_into_its_process_without_a_new_shell() {
    let host = Host::start("gui-attach", &["--no-shell"]);
    let ep = host.ep.clone();
    let (tid, pid) = new_terminal(&ep, "work");
    let shells_before = children(host.proc.pid, "cmd.exe");
    assert_eq!(
        shells_before,
        [pid],
        "the Terminal's cmd.exe is the Host's only shell"
    );
    let appdata = sandbox("attach");
    let gui = Gui::start(&ep, Some("work"), &appdata);
    let attached = gui.state("attached");
    assert_eq!(attached["terminal"]["id"], tid.as_str());
    assert_eq!(
        attached["detail"]["pane_pid"], pid,
        "the same shell process"
    );
    // The Host counts the window as an interactive `gui` client of that pane.
    let st = host.status();
    let me = st["clients"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "gui")
        .unwrap_or_else(|| panic!("no gui client: {st}"));
    assert_eq!(me["name"], "subak-gui");
    assert_eq!(me["pid"], gui.pid);
    assert_eq!(me["attached"], true);
    assert_eq!(me["interactive"], true);
    // Input reaches the running process: typed into the window, run by that cmd.exe, printed
    // in the window and in the Host's own screen of the pane.
    gui.type_text("echo %SUBAK_TERMINAL_ID%-typed\r");
    let want = format!("{tid}-typed");
    let d = gui.until("the echo appears", |d| has_line(d, &want));
    assert_eq!(d["phase"], "attached");
    wait_for("the Host's screen shows it", || {
        capture(&ep, "work").contains(&want)
    });
    // Korean typed as characters (as an IME commits them) arrives as UTF-8 and renders in two
    // cells per syllable.
    gui.type_text("echo 한글 입력\r");
    gui.until("Korean echo", |d| has_line(d, "한글 입력"));
    wait_for("Korean in the Host", || {
        capture(&ep, "work").contains("한글 입력")
    });
    // No second shell, no new pane, the same Terminal.
    let t = info(&ep, "work");
    assert_eq!(pid_of(&pane(&t)["pid"]), pid);
    assert_eq!(t["windows"].as_array().unwrap().len(), 1);
    assert_eq!(t["windows"][0]["panes"].as_array().unwrap().len(), 1);
    assert_eq!(
        children(host.proc.pid, "cmd.exe"),
        [pid],
        "no duplicate shell"
    );
    let (_, ls) = sub(&ep, &["ls"]);
    assert_eq!(ls["data"]["terminals"].as_array().unwrap().len(), 1);
    println!(
        "EVIDENCE attach: Host pid {}, Terminal {tid} pane pid {pid}; subak-gui pid {} attached as `gui` client; typed input echoed in the window and in `subak capture`; cmd.exe children of the Host before/after: {shells_before:?}/{:?}",
        host.proc.pid,
        gui.pid,
        children(host.proc.pid, "cmd.exe")
    );
}

#[test]
fn closing_or_killing_the_window_never_ends_the_host_or_the_pane() {
    let host = Host::start("gui-close", &["--no-shell"]);
    let ep = host.ep.clone();
    let (tid, pid) = new_terminal(&ep, "keep");
    let appdata = sandbox("close");
    // 1. Close with the window's close message (the X button, Alt+F4): a detach.
    let mut gui = Gui::start(&ep, Some("keep"), &appdata);
    gui.state("attached");
    gui.type_text("echo before-close\r");
    gui.until("echo", |d| has_line(d, "before-close"));
    let gui_pid = gui.pid;
    assert_eq!(gui.close(), 0);
    assert!(host.proc.alive(), "the Host keeps running");
    let pane_proc = Proc::open(pid);
    assert!(pane_proc.alive(), "the shell keeps running");
    wait_for("the window's client is gone", || {
        info(&ep, "keep")["clients"].as_u64() == Some(0)
    });
    let t = info(&ep, "keep");
    assert_eq!(t["state"], "running");
    assert_eq!(pid_of(&pane(&t)["pid"]), pid);
    // The preferences remember the Terminal; a new window reopens it without being told.
    let prefs: Value =
        serde_json::from_slice(&std::fs::read(appdata.join("mouv/subak/gui.json")).unwrap())
            .unwrap();
    validate("subak-gui-v1", &prefs);
    assert_eq!(prefs["last_terminal"]["id"], tid.as_str());
    // 2. A crash: TerminateProcess on the window's own process.
    let mut gui = Gui::start(&ep, None, &appdata);
    let attached = gui.state("attached");
    assert_eq!(
        attached["detail"]["pane_pid"], pid,
        "reopened the same Terminal"
    );
    // The screen came back from the Host's snapshot.
    gui.until("the earlier output is replayed", |d| {
        has_line(d, "before-close")
    });
    let crashed_pid = gui.pid;
    gui.crash();
    assert!(
        host.proc.alive(),
        "a crashed window leaves the Host running"
    );
    assert!(pane_proc.alive(), "and the shell");
    wait_for("the crashed client is dropped", || {
        info(&ep, "keep")["clients"].as_u64() == Some(0)
    });
    // 3. Reconnect after the crash: the same pane, same pid, still interactive.
    let gui = Gui::start(&ep, None, &appdata);
    let attached = gui.state("attached");
    assert_eq!(attached["detail"]["pane_pid"], pid);
    gui.type_text("echo after-crash\r");
    gui.until("echo after the crash", |d| has_line(d, "after-crash"));
    assert_eq!(host.status()["host"]["pid"], host.proc.pid, "the same Host");
    println!(
        "EVIDENCE close/crash: Host pid {} and pane pid {pid} alive after WM_CLOSE of subak-gui pid {gui_pid} and TerminateProcess of pid {crashed_pid}; reopened by pid {} (same pane, input works)",
        host.proc.pid, gui.pid
    );
}

#[test]
fn a_detached_window_reattaches_and_a_returning_host_is_reconnected() {
    let host = Host::start("gui-reconnect", &["--no-shell"]);
    let ep = host.ep.clone();
    let (_, pid) = new_terminal(&ep, "one");
    let appdata = sandbox("reconnect");
    let gui = Gui::start(&ep, Some("one"), &appdata);
    gui.state("attached");
    // Another client detaches the window (`subak detach`): its screen stays, with a banner.
    let (code, doc) = sub(&ep, &["detach", "one"]);
    assert_eq!(code, 0, "{doc}");
    let ended = gui.state("ended");
    assert!(
        ended["detail"]["why"]
            .as_str()
            .unwrap()
            .contains("DetachedBy"),
        "{ended}"
    );
    let d = gui.dump();
    assert!(
        d["banner"].as_str().unwrap().contains("Enter: reattach"),
        "{d}"
    );
    // Enter reattaches the same pane.
    gui.key(VK_RETURN);
    let again = gui.state("attached");
    assert_eq!(again["detail"]["pane_pid"], pid);
    // The Host goes away (stopped with its Terminals): the window keeps the last screen and a
    // banner, and retries without starting a Host.
    let (code, doc) = sub(&ep, &["host", "stop", "--kill-terminals", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    host.proc.wait(Duration::from_secs(15));
    let gone = gui.wait("state", |v| {
        v["phase"] == "disconnected" || v["phase"] == "ended"
    });
    if gone["phase"] == "ended" {
        gui.state("disconnected");
    }
    let d = gui.until("the disconnected banner", |d| d["phase"] == "disconnected");
    assert!(
        d["banner"].as_str().unwrap().contains("Reconnecting"),
        "{d}"
    );
    std::thread::sleep(Duration::from_secs(2));
    let (code, _) = sub(&ep, &["host", "status"]);
    assert_ne!(
        code, 0,
        "the window did not start a Host while reconnecting"
    );
    // A Host comes back (`subak new` starts one) with another Terminal: the window reconnects
    // by itself and opens it as its tab (the old one is gone, and it says so).
    let (_, pid2) = new_terminal(&ep, "two");
    let notice = gui.wait("notice", |v| {
        v["text"]
            .as_str()
            .is_some_and(|t| t.contains("`one` is no longer running"))
    });
    assert!(notice["text"].is_string());
    let attached = gui.state("attached");
    assert_eq!(attached["detail"]["pane_pid"], pid2);
    let d = gui.dump();
    assert_eq!(d["tabs"].as_array().unwrap().len(), 1, "{d}");
    let (_, st) = sub(&ep, &["host", "status"]);
    let new_host = Proc::open(pid_of(&st["data"]["host"]["pid"]));
    drop(gui);
    let _ = sub(&ep, &["host", "stop", "--kill-terminals", "--yes"]);
    new_host.wait(Duration::from_secs(15));
}

#[test]
fn state_screens_empty_tabs_and_failure() {
    // Empty: a Host with no Terminal; a Terminal created later opens by itself.
    let host = Host::start("gui-states", &["--no-shell"]);
    let ep = host.ep.clone();
    let appdata = sandbox("states");
    let gui = Gui::start(&ep, None, &appdata);
    gui.state("empty");
    let d = gui.dump();
    assert!(
        screen_text(&d).contains("No Terminal is open in this window"),
        "{d}"
    );
    assert!(
        screen_text(&d).contains("Ctrl+T (or the + above) starts one"),
        "{d}"
    );
    assert_eq!(d["tabs"].as_array().unwrap().len(), 0);
    assert_eq!(d["title"], "Subak");
    let (_, pid) = new_terminal(&ep, "later");
    let attached = gui.state("attached");
    assert_eq!(attached["detail"]["pane_pid"], pid);
    let d = gui.dump();
    assert!(
        d["title"].as_str().unwrap().ends_with("later — Subak"),
        "{d}"
    );
    drop(gui);
    // Two Terminals and nothing named or remembered: both are tabs, the first one opens
    // (Step 17 replaces Step 16's chooser).
    new_terminal(&ep, "second");
    let gui = Gui::start(&ep, None, &sandbox("states-tabs"));
    let attached = gui.state("attached");
    assert_eq!(attached["terminal"]["name"], "later");
    let d = gui.until("two tabs", |d| d["tabs"].as_array().unwrap().len() == 2);
    let bar = d["bar"]["text"].as_str().unwrap();
    assert!(bar.contains("later") && bar.contains("second"), "{bar}");
    assert_eq!(d["active"], 0);
    drop(gui);
    // Failure: the endpoint is held by a process that is not a Subak Host.
    let squat_ep = endpoint("gui-squat");
    let name = pipe_name(&squat_ep);
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT};
    // SAFETY: NUL-terminated name; default security (this user); closed below.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(server, INVALID_HANDLE_VALUE);
    let gui = Gui::start(&squat_ep, None, &sandbox("states-fail"));
    let failed = gui.state("failed");
    assert_eq!(failed["detail"]["code"], "host_identity_mismatch");
    let d = gui.dump();
    let text = screen_text(&d);
    assert!(
        text.contains("Cannot reach the Subak Host") && text.contains("Press Enter to try again"),
        "{text}"
    );
    drop(gui);
    // SAFETY: the squatter's handle, closed once.
    unsafe { CloseHandle(server) };
}

#[test]
fn keyboard_paste_copy_scroll_ime_and_focus() {
    let host = Host::start("gui-input", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "keys");
    let gui = Gui::start(&ep, Some("keys"), &sandbox("input"));
    gui.state("attached");
    // Paste (the test window's fake clipboard, never the system one). A line that ends with a
    // line break would run at once, so since Step 19 the window asks first, Cancel focused;
    // Shift+Tab focuses Paste and Enter answers. CR LF becomes CR.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "echo pasted-text\r\n"}));
    assert_eq!(d["clipboard"], "echo pasted-text\r\n");
    assert_eq!(d["ask"]["focus"], "Cancel", "{}", d["ask"]);
    gui.cmd(&json!({"cmd": "key", "vk": 0x09, "shift": true}));
    gui.cmd(&json!({"cmd": "key", "vk": VK_RETURN}));
    gui.until("paste ran", |d| has_line(d, "pasted-text"));
    wait_for("paste in the Host", || {
        capture(&ep, "keys").contains("pasted-text")
    });
    // Copy a selection with Ctrl+Shift+C (the window's shortcut, not the program's Ctrl+C).
    let d = gui.dump();
    let row = d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .position(|l| l.as_str().unwrap().trim() == "pasted-text")
        .unwrap();
    gui.cmd(&json!({"cmd": "select", "from": [0, row], "to": [10, row]}));
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x43, "ctrl": true, "shift": true}));
    assert_eq!(d["clipboard"], "pasted-text");
    assert_eq!(d["selection"], Value::Null, "copying clears the selection");
    // Scrolling back with the wheel and to the bottom with Ctrl+Shift+End.
    gui.type_text("for /l %i in (1,1,80) do @echo line %i\r");
    gui.until("long output", |d| has_line(d, "line 80"));
    let d = gui.cmd(&json!({"cmd": "wheel", "notches": 2}));
    assert_eq!(d["scroll_offset"], 6, "3 lines per notch");
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x23, "ctrl": true, "shift": true}));
    assert_eq!(d["scroll_offset"], 0);
    // Cursor keys go to the program in the xterm encoding: Up recalls the last command.
    gui.type_text("cls\r");
    gui.until("cleared", |d| !has_line(d, "line 80"));
    gui.type_text("echo recall-me\r");
    gui.until("echo", |d| has_line(d, "recall-me"));
    gui.key(0x26); // VK_UP
    gui.type_text("\r");
    gui.until("the recalled command ran twice", |d| {
        d["screen"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l.as_str().unwrap().trim() == "recall-me")
            .count()
            == 2
    });
    // IME: the composition is drawn inline and sent only when committed.
    let d = gui.cmd(&json!({"cmd": "ime", "composition": "ㅎ"}));
    assert_eq!(d["composition"], "ㅎ");
    let d = gui.cmd(&json!({"cmd": "ime", "composition": "하"}));
    assert_eq!(d["composition"], "하");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !capture(&ep, "keys").contains("하"),
        "nothing is sent while composing"
    );
    // Typed synchronously (a test command), so it stays in order with the IME commands.
    gui.cmd(&json!({"cmd": "type", "text": "echo "}));
    let d = gui.cmd(&json!({"cmd": "ime", "result": "한", "composition": "ㄱ"}));
    assert_eq!(d["composition"], "ㄱ");
    let d = gui.cmd(&json!({"cmd": "ime", "result": "글", "composition": ""}));
    assert_eq!(d["composition"], "");
    gui.type_text("\r");
    gui.until("the committed syllables ran", |d| has_line(d, "한글"));
    // Focus: the cursor becomes hollow and the state is tracked.
    let d = gui.cmd(&json!({"cmd": "focus", "on": true}));
    assert_eq!(d["focused"], true);
    let d = gui.cmd(&json!({"cmd": "focus", "on": false}));
    assert_eq!(d["focused"], false);
}

#[test]
fn dpi_resize_minimize_zoom_and_preferences() {
    let host = Host::start("gui-dpi", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "dpi");
    let appdata = sandbox("dpi");
    let mut gui = Gui::start(&ep, Some("dpi"), &appdata);
    gui.state("attached");
    let size_of_pane = || {
        let t = info(&ep, "dpi");
        (
            pane(&t)["size"]["cols"].as_u64().unwrap(),
            pane(&t)["size"]["rows"].as_u64().unwrap(),
        )
    };
    let grid = |d: &Value| {
        (
            d["grid"][0].as_u64().unwrap(),
            d["grid"][1].as_u64().unwrap(),
        )
    };
    let d = gui.until("the default grid", |d| grid(d) == (120, 30));
    wait_for("the pane follows the window's grid", || {
        size_of_pane() == (120, 30)
    });
    let dpi0 = d["dpi"].as_u64().unwrap();
    let cell0 = d["cell"].clone();
    // A synthetic move to a 200 % monitor: glyphs are rasterized again at twice the size (never
    // scaled as bitmaps), the window keeps its logical size (its outer size doubles, as Windows
    // suggests), and the pane follows the grid that holds.
    let client0 = d["client"][0].as_u64().unwrap();
    let d = gui.cmd(&json!({"cmd": "dpi", "dpi": 192}));
    assert_eq!(d["dpi"], 192);
    let big = |d: &Value| d["client"][0].as_u64().unwrap() >= client0 * 192 / dpi0 - 64;
    let d = gui.until("the 200 % layout", big);
    let h200 = d["cell"][1].as_u64().unwrap();
    assert!(
        h200 >= 2 * cell0[1].as_u64().unwrap() * 96 / dpi0 - 1,
        "{cell0} → {}",
        d["cell"]
    );
    assert_eq!(d["padding"].as_u64().unwrap(), 16);
    let g200 = grid(&d);
    assert!(
        g200.0.abs_diff(120) <= 12 && g200.1.abs_diff(30) <= 3,
        "about the same grid: {g200:?}"
    );
    wait_for("the pane follows the 200 % grid", || size_of_pane() == g200);
    let d = gui.cmd(&json!({"cmd": "dpi", "dpi": 96}));
    assert_eq!(d["dpi"], 96);
    let d = gui.until("back at 100 %", |d| {
        d["cell"][1].as_u64().unwrap() < h200 && d["padding"] == 8
    });
    wait_for("the pane follows the 100 % grid", || {
        size_of_pane() == grid(&d)
    });
    let cell = (
        d["cell"][0].as_u64().unwrap(),
        d["cell"][1].as_u64().unwrap(),
    );
    // The tab bar (32 logical px at 100 %) sits above the pane.
    let bar = d["bar"]["height"].as_u64().unwrap();
    assert!((32..=40).contains(&bar), "{}", d["bar"]);
    // A resize to 80 × 24 cells: the pane follows.
    let (w, h) = (80 * cell.0 + 16, 24 * cell.1 + 16 + bar);
    gui.cmd(&json!({"cmd": "size", "width": w, "height": h}));
    gui.until("80x24", |d| grid(d) == (80, 24));
    wait_for("the pane is 80x24", || size_of_pane() == (80, 24));
    // Minimized (WM_SIZE with SIZE_MINIMIZED and a 0 × 0 client area): the pane keeps its size.
    gui.post(WM_SIZE, 1, 0);
    let d = gui.until("minimized", |d| d["minimized"] == true);
    assert_eq!(grid(&d), (80, 24));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(size_of_pane(), (80, 24), "never resized to nothing");
    gui.post(WM_SIZE, 0, ((h as isize) << 16) | w as isize);
    gui.until("restored", |d| d["minimized"] == false);
    // Zoom in (Ctrl+Plus): larger text, fewer cells, saved.
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0xBB, "ctrl": true}));
    assert_eq!(d["font_size"], 14.0);
    let g = grid(&d);
    assert!(g.0 <= 80 && g.1 < 24, "{g:?}");
    wait_for("the pane follows the zoom", || size_of_pane() == g);
    let client = (
        d["client"][0].as_u64().unwrap(),
        d["client"][1].as_u64().unwrap(),
    );
    assert_eq!(gui.close(), 0);
    let path = appdata.join("mouv").join("subak").join("gui.json");
    let prefs: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    validate("subak-gui-v1", &prefs);
    assert_eq!(prefs["font_size"], 14.0);
    assert_eq!(prefs["window"]["dpi"], 96);
    assert_eq!(prefs["window"]["maximized"], false);
    assert_eq!(prefs["last_terminal"]["name"], "dpi");
    // The off-screen test placement is saved as it was; a normal start would not use it
    // (it is on no monitor), but the size is restored.
    assert_eq!(prefs["window"]["x"], -20000);
    // Unknown keys survive a rewrite; a new window uses the saved size and text size.
    let mut edited = prefs;
    edited["added_by_a_later_version"] = json!({"keep": true});
    std::fs::write(&path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
    let mut gui = Gui::start(&ep, None, &appdata);
    gui.state("attached");
    let d = gui.dump();
    assert_eq!(d["font_size"], 14.0);
    assert_eq!(
        (
            d["client"][0].as_u64().unwrap(),
            d["client"][1].as_u64().unwrap()
        ),
        client,
        "the saved window size"
    );
    assert_eq!(gui.close(), 0);
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["added_by_a_later_version"], json!({"keep": true}));
    // An invalid file: defaults, a notice, and the file is left as it is.
    std::fs::write(&path, b"{ not json").unwrap();
    let mut gui = Gui::start(&ep, None, &appdata);
    gui.state("attached");
    let d = gui.dump();
    assert_eq!(d["font_size"], 13.0);
    assert!(
        d["notice"].as_str().unwrap().contains("not a JSON object"),
        "{d}"
    );
    assert!(
        d["banner"].as_str().unwrap().contains("any key hides this"),
        "{d}"
    );
    assert_eq!(gui.close(), 0);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"{ not json",
        "never overwritten"
    );
}

#[test]
fn rendered_frames_of_the_window_content() {
    // The window renders its own content to BMP (never a screen capture). Pixels are checked
    // against the design tokens; with SUBAK_GUI_SHOTS=<dir> the frames are kept for review.
    let host = Host::start("gui-shots", &["--no-shell"]);
    let ep = host.ep.clone();
    let dir = std::env::var_os("SUBAK_GUI_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| sandbox("shots"));
    std::fs::create_dir_all(&dir).unwrap();
    let gui = Gui::start(&ep, None, &sandbox("shots-prefs"));
    gui.state("empty");
    let shot = |name: &str| {
        let path = dir.join(format!("{name}.bmp"));
        let r = gui.cmd(&json!({"cmd": "shot", "path": path}));
        assert_eq!(r["ok"], true, "{r}");
        let bytes = std::fs::read(&path).unwrap();
        let (w, h) = (
            u32::from_le_bytes(bytes[18..22].try_into().unwrap()),
            u32::from_le_bytes(bytes[22..26].try_into().unwrap()),
        );
        let row = (w as usize * 3).div_ceil(4) * 4;
        let px = move |x: u32, y: u32| {
            let i = 54 + (h - 1 - y) as usize * row + x as usize * 3;
            (bytes[i + 2], bytes[i + 1], bytes[i])
        };
        (w, h, px)
    };
    let (w, h, px) = shot("gui_empty");
    assert!(w > 400 && h > 200);
    let top = gui.dump()["bar"]["height"].as_u64().unwrap() as u32;
    assert_eq!(px(1, 1), (0x15, 0x15, 0x1B), "bg.raised: the tab bar");
    assert_eq!(
        px(1, top + 1),
        (0x0E, 0x0E, 0x12),
        "bg.base around the text"
    );
    new_terminal(&ep, "shots");
    gui.state("attached");
    gui.type_text("echo 한글 Subak ┌─┐\r");
    gui.until("output", |d| has_line(d, "한글 Subak ┌─┐"));
    let (_, _, px) = shot("gui_attached");
    assert_eq!(px(w - 2, h - 2), (0x0E, 0x0E, 0x12), "terminal.background");
    // The active tab: a lilac marker on top, bg.base joining the pane below it.
    let d = gui.dump();
    let tab = d["bar"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "tab:0")
        .unwrap()
        .clone();
    let cw = d["bar"]["cell"][0].as_u64().unwrap() as u32;
    let x = d["bar"]["pad_x"].as_u64().unwrap() as u32 + tab[0].as_u64().unwrap() as u32 * cw + 2;
    assert_eq!(
        px(x, 0),
        (0xB7, 0xA6, 0xF5),
        "accent: the active tab's marker"
    );
    assert_eq!(
        px(x, top - 1),
        (0x0E, 0x0E, 0x12),
        "the active tab joins the pane"
    );
    let (_, _) = sub(&ep, &["detach", "shots"]);
    gui.state("ended");
    let (_, _, px) = shot("gui_detached_banner");
    // The banner: the info colour as a 3 px marker at the left, a tinted fill.
    assert_eq!(px(1, top + 4), (0x8A, 0xB4, 0xF8), "status.info marker");
    let fill = px(w / 2, top + 2);
    assert_ne!(fill, (0x0E, 0x0E, 0x12), "the banner is not the background");
    println!("EVIDENCE shots: {}", dir.display());
}

/// Idle cost of the window, off-screen, attached to a live Terminal with no output: CPU time
/// and memory sampled like Step 15's `term_bench --only idle` (ADR 0036: CPU renderer + GDI
/// window 3.40 MiB private, 1 thread; egui + glow 107.0 MiB, 12 threads). This is the debug
/// build; the release numbers are in docs/SUBAK_GUI.md §7.
#[test]
fn idle_window_costs_no_cpu_and_little_memory() {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{GetProcessHandleCount, GetProcessTimes};
    let host = Host::start("gui-idle", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "idle");
    let gui = Gui::start(&ep, Some("idle"), &sandbox("idle"));
    gui.state("attached");
    gui.until("the prompt", |d| screen_text(d).contains('>'));
    let p = Proc::open(gui.pid);
    let sample = || {
        let ft = |f: FILETIME| {
            ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 10_000.0
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
    let threads = || threads_of(gui.pid);
    std::thread::sleep(Duration::from_secs(2));
    let (cpu0, ..) = sample();
    let secs = 10;
    std::thread::sleep(Duration::from_secs(secs));
    let (cpu1, private, ws, handles) = sample();
    let n = threads();
    let mib = |b: usize| b as f64 / 1_048_576.0;
    println!(
        "EVIDENCE idle (debug build, off-screen, attached, {secs} s): CPU {:.1} ms; private {:.2} MiB; working set {:.2} MiB; threads {n}; handles {handles}",
        cpu1 - cpu0,
        mib(private),
        mib(ws)
    );
    assert!(
        cpu1 - cpu0 <= 47.0,
        "idle CPU {:.1} ms over {secs} s",
        cpu1 - cpu0
    );
    assert!(mib(private) < 48.0, "private {:.1} MiB", mib(private));
    assert!(n <= 8, "{n} threads");
}

/// Threads of a process (Toolhelp).
fn threads_of(pid: u32) -> usize {
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

// ---------------------------------------------------------------------------------------------
// Step 17: tabs, the tab keys, safe close (docs/SUBAK_GUI.md §2, ADR 0042). `SUBAK-FR-011`,
// `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-033`,
// `SUBAK-FR-034`.

const VK_TAB: u64 = 0x09;
const VK_ESCAPE: u64 = 0x1B;
const VK_APPS: u64 = 0x5D;

/// The stand-in for a long-running interactive program such as Claude Code (never the real
/// one): runs only inside a pane with `SUBAK_TEST_STANDIN` set, says it is ready, and answers
/// every input line with the hex of its bytes (`standin-got-<hex>`) until `quit`. Its root
/// program is not a shell, so the Host counts its Terminal as busy (SESSION_MODEL §4).
#[test]
fn standin_helper() {
    if std::env::var_os("SUBAK_TEST_STANDIN").is_none() {
        return;
    }
    println!("standin-ready-pid-{}", std::process::id());
    let mut line = Vec::new();
    loop {
        line.clear();
        match std::io::stdin().lock().read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let t: Vec<u8> = line
            .iter()
            .copied()
            .filter(|b| *b != b'\r' && *b != b'\n')
            .collect();
        if t == b"quit" {
            break;
        }
        let hex: String = t.iter().map(|b| format!("{b:02x}")).collect();
        println!("standin-got-{hex}");
    }
}

/// A Terminal running the stand-in; returns its id and pane pid.
fn new_standin(ep: &str, name: &str) -> (String, u32) {
    let me = std::env::current_exe().unwrap().display().to_string();
    let (code, doc) = sub(
        ep,
        &[
            "new",
            name,
            "--size",
            "100x30",
            "--env",
            "SUBAK_TEST_STANDIN=1",
            "--",
            &me,
            "standin_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let t = &doc["data"]["terminal"];
    let id = t["id"].as_str().unwrap().to_string();
    wait_for("the stand-in is ready", || {
        capture(ep, &id).contains("standin-ready-pid-")
    });
    (id, pid_of(&t["windows"][0]["panes"][0]["pid"]))
}

/// The Terminals the Host lists: (id, name, state).
fn terminals(ep: &str) -> Vec<(String, String, String)> {
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
            )
        })
        .collect()
}

fn tab_names(d: &Value) -> Vec<String> {
    d["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn shown(d: &Value) -> String {
    d["terminal"]["name"].as_str().unwrap_or("").to_string()
}

impl Gui {
    /// A key press through the window's dispatch (`char`: what TranslateMessage would make of
    /// it when the window does not handle it).
    fn press(&self, vk: u64, ctrl: bool, shift: bool) -> Value {
        self.cmd(&json!({"cmd": "key", "vk": vk, "ctrl": ctrl, "shift": shift}))
    }

    /// A left click at a client point, as real mouse messages to the window.
    fn click(&self, x: u32, y: u32) {
        let lp = ((y as isize) << 16) | x as isize;
        self.post(WM_LBUTTONDOWN, 1, lp);
        self.post(WM_LBUTTONUP, 0, lp);
    }

    /// The client point in the middle of a tab bar span (`tab:0`, `close:1`, `new`).
    fn bar_point(&self, d: &Value, what: &str) -> (u32, u32) {
        let span = d["bar"]["spans"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s[2] == what)
            .unwrap_or_else(|| panic!("no `{what}` in {}", d["bar"]))
            .clone();
        let cw = d["bar"]["cell"][0].as_u64().unwrap();
        let pad = d["bar"]["pad_x"].as_u64().unwrap();
        let mid = (span[0].as_u64().unwrap() + span[1].as_u64().unwrap()) * cw / 2;
        (
            (pad + mid) as u32,
            (d["bar"]["height"].as_u64().unwrap() / 2) as u32,
        )
    }
}

#[test]
fn tabs_follow_the_headless_cli_and_ctrl_tab_switches_without_ending_anything() {
    let host = Host::start("gui-tabs", &["--no-shell"]);
    let ep = host.ep.clone();
    let (id1, pid1) = new_terminal(&ep, "one");
    let (id2, pid2) = new_terminal(&ep, "two");
    let gui = Gui::start(&ep, None, &sandbox("tabs"));
    let attached = gui.state("attached");
    assert_eq!(attached["detail"]["pane_pid"], pid1, "the first tab opens");
    let d = gui.until("two tabs", |d| tab_names(d) == ["one", "two"]);
    assert_eq!(d["active"], 0);
    assert!(d["bar"]["text"].as_str().unwrap().contains("● one"), "{d}");
    // `subak new` from a console: a third tab appears; the shown tab stays (keys never move to a
    // Terminal the user did not choose).
    let (id3, _) = new_terminal(&ep, "three");
    let d = gui.until("the CLI's new Terminal is a tab", |d| {
        tab_names(d) == ["one", "two", "three"]
    });
    assert_eq!(shown(&d), "one");
    // `subak rename`: the tab follows.
    let (code, doc) = sub(&ep, &["rename", "two", "deux"]);
    assert_eq!(code, 0, "{doc}");
    gui.until("the rename shows", |d| {
        tab_names(d) == ["one", "deux", "three"]
    });
    // Ctrl+Tab: the next tab; the window detaches `one` and attaches `deux`; nothing ends.
    gui.press(VK_TAB, true, false);
    let d = gui.until("deux is shown", |d| {
        d["phase"] == "attached" && shown(d) == "deux"
    });
    assert_eq!(d["active"], 1);
    wait_for("one has no client, deux has the window", || {
        info(&ep, "one")["clients"].as_u64() == Some(0)
            && info(&ep, "deux")["clients"].as_u64() == Some(1)
    });
    gui.type_text("echo typed-into-deux\r");
    wait_for("typed into deux only", || {
        capture(&ep, "deux").contains("typed-into-deux")
    });
    assert!(!capture(&ep, "one").contains("typed-into-deux"));
    // Ctrl+Shift+Tab: back; Ctrl+Tab twice wraps around.
    gui.press(VK_TAB, true, true);
    gui.until("one again", |d| {
        d["phase"] == "attached" && shown(d) == "one"
    });
    gui.press(VK_TAB, true, false);
    gui.press(VK_TAB, true, false);
    gui.until("three", |d| d["phase"] == "attached" && shown(d) == "three");
    gui.press(VK_TAB, true, false);
    gui.until("wrapped to one", |d| {
        d["phase"] == "attached" && shown(d) == "one"
    });
    // `subak kill three`: its tab goes; `subak stop deux`: its tab says stopped.
    let (code, doc) = sub(&ep, &["kill", "three", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    gui.until("three's tab is gone", |d| tab_names(d) == ["one", "deux"]);
    let (code, doc) = sub(&ep, &["stop", "deux", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    let d = gui.until("deux is stopped", |d| d["tabs"][1]["state"] == "stopped");
    assert!(d["bar"]["text"].as_str().unwrap().contains("■ deux"), "{d}");
    // The stored tab: its state screen; Ctrl+W removes its record (nothing runs in it).
    gui.press(VK_TAB, true, false);
    let d = gui.until("the stored screen", |d| d["phase"] == "stored");
    assert!(screen_text(&d).contains("`deux` is stopped"), "{d}");
    assert!(screen_text(&d).contains("Ctrl+W removes it"), "{d}");
    gui.press(u64::from(b'W'), true, false);
    gui.until("deux removed, one shown", |d| {
        tab_names(d) == ["one"] && d["phase"] == "attached"
    });
    assert_eq!(
        terminals(&ep),
        [(id1, "one".into(), "running".into())],
        "only the stored record was removed"
    );
    // `one` kept its process the whole time; nothing else was touched.
    assert_eq!(pid_of(&pane(&info(&ep, "one"))["pid"]), pid1);
    assert!(Proc::open(pid1).alive());
    let _ = (id2, id3, pid2);
    println!(
        "EVIDENCE tabs: CLI new/rename/kill/stop followed by the tabs; Ctrl+Tab/Ctrl+Shift+Tab switched attachments (one pid {pid1} kept); Ctrl+W on the stopped tab removed only its record"
    );
}

#[test]
fn ctrl_t_creates_and_ctrl_w_closes_asking_first_when_busy_never_hitting_another_terminal() {
    let host = Host::start("gui-close-tabs", &["--no-shell"]);
    let ep = host.ep.clone();
    let (idle_id, idle_pid) = new_terminal(&ep, "idle");
    let gui = Gui::start(&ep, Some("idle"), &sandbox("close-tabs"));
    gui.state("attached");
    // Ctrl+T: a new Terminal (the Host's default shell), shown at once.
    gui.press(u64::from(b'T'), true, false);
    let d = gui.until("the new Terminal is shown", |d| {
        d["phase"] == "attached" && tab_names(d).len() == 2 && shown(d) != "idle"
    });
    let created = shown(&d);
    assert_eq!(terminals(&ep).len(), 2);
    let created_info = info(&ep, &created);
    let size = &pane(&created_info)["size"];
    assert_eq!(
        (size["cols"].as_u64(), size["rows"].as_u64()),
        (d["grid"][0].as_u64(), d["grid"][1].as_u64()),
        "created at the window's size"
    );
    // Ctrl+W on an idle shell: ended without a question (ADR 0009), only that Terminal.
    gui.press(u64::from(b'W'), true, false);
    let d = gui.until("back on idle", |d| {
        tab_names(d) == ["idle"] && d["phase"] == "attached"
    });
    assert!(d["confirm"].is_null());
    assert_eq!(
        terminals(&ep),
        [(idle_id.clone(), "idle".into(), "running".into())]
    );
    // Repeated create and close leave nothing behind.
    for _ in 0..3 {
        gui.press(u64::from(b'T'), true, false);
        // Wait until the new Terminal is the shown one: until then Ctrl+W would close `idle`,
        // the tab the user still sees.
        gui.until("created and shown", |d| {
            d["phase"] == "attached" && tab_names(d).len() == 2 && shown(d) != "idle"
        });
        gui.press(u64::from(b'W'), true, false);
        gui.until("closed", |d| {
            d["phase"] == "attached" && tab_names(d) == ["idle"]
        });
    }
    assert_eq!(terminals(&ep).len(), 1);
    // A busy Terminal: the stand-in for a running Claude process.
    let (busy_id, busy_pid) = new_standin(&ep, "claude");
    gui.until("its tab", |d| tab_names(d) == ["idle", "claude"]);
    gui.press(VK_TAB, true, false);
    gui.until("claude shown", |d| {
        d["phase"] == "attached" && shown(d) == "claude"
    });
    // Ctrl+W asks first: the Host refused without consent; nothing ended.
    gui.press(u64::from(b'W'), true, false);
    let c = gui.wait("confirm", |_| true);
    assert_eq!(c["id"], busy_id.as_str());
    let d = gui.dump();
    assert_eq!(
        d["confirm"]["focus"], "cancel",
        "the safe choice is the default"
    );
    let text = d["banner_text"].as_str().unwrap();
    assert!(
        text.contains("End `claude`?") && text.contains("process") && text.contains("[ Cancel"),
        "{text}"
    );
    assert!(Proc::open(busy_pid).alive());
    // Keys do not leak into the program while the question is open; Esc cancels. (Typed through
    // the window's own character path synchronously, so they surely come before the Esc.)
    gui.cmd(&json!({"cmd": "type", "text": "leak\r"}));
    gui.press(VK_ESCAPE, false, false);
    let d = gui.until("cancelled", |d| d["confirm"].is_null());
    assert_eq!(d["phase"], "attached");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !capture(&ep, "claude").contains(
            &"leak"
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
        "typing during the question never reached the program"
    );
    assert!(Proc::open(busy_pid).alive(), "Esc ends nothing");
    // Enter on the default (Cancel) ends nothing either; a click on Cancel neither.
    gui.press(u64::from(b'W'), true, false);
    gui.wait("confirm", |_| true);
    gui.cmd(&json!({"cmd": "key", "vk": 0x0D}));
    gui.until("cancelled by Enter", |d| d["confirm"].is_null());
    assert!(Proc::open(busy_pid).alive());
    gui.press(u64::from(b'W'), true, false);
    gui.wait("confirm", |_| true);
    let d = gui.dump();
    let cancel = d["confirm"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == "cancel")
        .unwrap()
        .clone();
    let b = &d["confirm"]["banner"];
    let (cw, pad, bh) = (
        b["cell"].as_u64().unwrap(),
        b["pad"].as_u64().unwrap(),
        b["height"].as_u64().unwrap(),
    );
    let x = pad + (cancel[0].as_u64().unwrap() + 2) * cw;
    let y = d["bar"]["height"].as_u64().unwrap() + bh / 2;
    gui.click(x as u32, y as u32);
    gui.until("cancelled by a click", |d| d["confirm"].is_null());
    assert!(Proc::open(busy_pid).alive());
    // Consent: Tab to "End Terminal", Enter. Only `claude` ends, by its id.
    gui.press(u64::from(b'W'), true, false);
    gui.wait("confirm", |_| true);
    gui.press(VK_TAB, false, false);
    assert_eq!(gui.dump()["confirm"]["focus"], "end");
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x0D}));
    assert!(d["confirm"].is_null());
    assert!(
        gui.history().contains(&format!(
            "\"choice\":\"end\",\"event\":\"confirm_answer\",\"id\":\"{busy_id}\""
        )),
        "{}",
        gui.history()
    );
    gui.until("claude is gone", |d| {
        tab_names(d) == ["idle"] && d["phase"] == "attached"
    });
    assert!(
        Proc::try_open(busy_pid).is_none_or(|p| p.wait(LONG)),
        "the stand-in ended"
    );
    assert_eq!(
        terminals(&ep),
        [(idle_id, "idle".into(), "running".into())],
        "the other Terminal was never touched"
    );
    assert_eq!(pid_of(&pane(&info(&ep, "idle"))["pid"]), idle_pid);
    println!(
        "EVIDENCE close: Ctrl+T created at the window size; Ctrl+W ended idle shells at once; the busy stand-in (pid {busy_pid}) was asked about (Esc, Enter on Cancel and a click on Cancel ended nothing), then ended by consent; `idle` pid {idle_pid} untouched"
    );
}

#[test]
fn the_window_x_and_a_tab_x_only_detach_and_hidden_tabs_come_back() {
    let host = Host::start("gui-hide", &["--no-shell"]);
    let ep = host.ep.clone();
    let (_, pid_a) = new_terminal(&ep, "a");
    let (_, pid_b) = new_standin(&ep, "b");
    let appdata = sandbox("hide");
    let mut gui = Gui::start(&ep, Some("b"), &appdata);
    gui.state("attached");
    let d = gui.until("two tabs", |d| tab_names(d).len() == 2);
    // The × of the shown tab (real mouse messages): a detach; the Terminal keeps running.
    let (x, y) = gui.bar_point(&d, "close:1");
    gui.click(x, y);
    let d = gui.until("b hidden", |d| {
        tab_names(d) == ["a"] && d["phase"] == "attached"
    });
    assert_eq!(d["hidden"][0]["name"], "b");
    wait_for("b has no client", || {
        info(&ep, "b")["clients"].as_u64() == Some(0)
    });
    assert_eq!(info(&ep, "b")["state"], "running");
    assert!(Proc::open(pid_b).alive(), "hiding never ends a Terminal");
    // The tab menu (the Menu key, a real key message) offers it again; the test window reports
    // the menu instead of showing one on the desktop.
    gui.key(VK_APPS as usize);
    let menu = gui.wait("menu", |_| true);
    let labels: Vec<&str> = menu["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Rename…\tCtrl+Shift+R")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.starts_with("Close Terminal")),
        "{labels:?}"
    );
    assert!(labels.contains(&"Show hidden: b"), "{labels:?}");
    let show = menu["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "Show hidden: b")
        .unwrap()["id"]
        .clone();
    gui.cmd(&json!({"cmd": "menu", "id": show}));
    let d = gui.until("b is back and shown", |d| {
        tab_names(d) == ["a", "b"] && d["phase"] == "attached" && shown(d) == "b"
    });
    assert!(d["hidden"].as_array().unwrap().is_empty());
    // A middle click hides a tab too; the menu's Move right orders tabs (this window only).
    let (x, y) = gui.bar_point(&d, "tab:0");
    let lp = ((y as isize) << 16) | x as isize;
    gui.post(WM_MBUTTONUP, 0, lp);
    gui.until("a hidden", |d| tab_names(d) == ["b"]);
    assert!(Proc::open(pid_a).alive());
    // The window's X (WM_CLOSE): a detach of everything; both Terminals keep running.
    assert_eq!(gui.close(), 0);
    wait_for("no client left", || {
        info(&ep, "a")["clients"].as_u64() == Some(0)
            && info(&ep, "b")["clients"].as_u64() == Some(0)
    });
    for (name, pid) in [("a", pid_a), ("b", pid_b)] {
        assert_eq!(info(&ep, name)["state"], "running", "{name}");
        assert_eq!(pid_of(&pane(&info(&ep, name))["pid"]), pid, "{name}");
    }
    // A new window keeps `a` hidden (since Step 18 hiding is kept across runs in gui.json); it
    // still runs, and the tab menu or the Terminal manager shows it again.
    let gui = Gui::start(&ep, None, &appdata);
    gui.state("attached");
    let d = gui.until("a still hidden", |d| {
        tab_names(d) == ["b"] && d["hidden"].as_array().is_some_and(|h| h.len() == 1)
    });
    assert_eq!(d["hidden"][0]["name"], "a");
    println!(
        "EVIDENCE hide: tab x, middle click and the window X detached only; a pid {pid_a} and b (stand-in) pid {pid_b} kept running"
    );
}

#[test]
fn rename_in_place_and_keys_for_the_program_reach_it_once_a_shortcut_is_off() {
    let host = Host::start("gui-rename", &["--no-shell"]);
    let ep = host.ep.clone();
    new_standin(&ep, "agent");
    new_terminal(&ep, "taken");
    let appdata = sandbox("rename");
    let gui = Gui::start(&ep, Some("agent"), &appdata);
    gui.state("attached");
    // Ctrl+Shift+R: the field takes the keys (text capture); Korean typed as WM_CHAR.
    gui.press(u64::from(b'R'), true, true);
    let d = gui.until("editing", |d| !d["renaming"].is_null());
    assert_eq!(d["renaming"]["text"], "agent");
    for _ in 0..5 {
        gui.press(0x08, false, false); // Backspace
    }
    gui.type_text("에이전트");
    let d = gui.until("typed", |d| d["renaming"]["text"] == "에이전트");
    assert!(
        d["bar"]["text"].as_str().unwrap().contains("에이전트"),
        "{d}"
    );
    gui.cmd(&json!({"cmd": "key", "vk": 0x0D}));
    gui.wait("renamed", |v| v["ok"] == true);
    wait_for("the Host has the new name", || {
        terminals(&ep).iter().any(|t| t.1 == "에이전트")
    });
    let d = gui.until("the field closed", |d| d["renaming"].is_null());
    assert_eq!(tab_names(&d), ["에이전트", "taken"]);
    // A name in use: refused by the Host, the field stays with the reason; Esc cancels.
    gui.press(u64::from(b'R'), true, true);
    gui.until("editing", |d| !d["renaming"].is_null());
    gui.cmd(&json!({"cmd": "key", "vk": 0x23})); // End
    for _ in 0..4 {
        gui.press(0x08, false, false);
    }
    gui.type_text("TAKEN");
    gui.until("typed", |d| d["renaming"]["text"] == "TAKEN");
    gui.cmd(&json!({"cmd": "key", "vk": 0x0D}));
    let r = gui.wait("renamed", |_| true);
    assert_eq!(r["ok"], false);
    assert!(
        r["error"].as_str().unwrap().contains("name_conflict"),
        "{r}"
    );
    let d = gui.dump();
    assert!(!d["renaming"].is_null(), "the field stays open");
    gui.press(VK_ESCAPE, false, false);
    gui.until("closed", |d| d["renaming"].is_null());
    // Nothing typed into the field reached the program.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !capture(&ep, "에이전트").contains("standin-got-"),
        "{}",
        capture(&ep, "에이전트")
    );
    // Ctrl+T is a shortcut: it never reaches the program. With it disabled through
    // `subak hotkeys` (re-read when the window gets focus), the program gets it (0x14).
    let env = [("APPDATA", appdata.to_str().unwrap())];
    let (code, doc) = run_env(&["hotkeys", "disable", "terminal.new"], &env);
    assert_eq!(code, 0, "{doc}");
    gui.cmd(&json!({"cmd": "focus", "on": false}));
    let d = gui.cmd(&json!({"cmd": "focus", "on": true}));
    assert_eq!(
        d["hotkeys"]["state"], "user",
        "re-read when the window got focus"
    );
    assert!(
        !d["shortcuts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["action"] == "terminal.new"),
        "{}",
        d["shortcuts"]
    );
    let before = terminals(&ep).len();
    gui.cmd(&json!({"cmd": "key", "vk": u32::from(b'T'), "ctrl": true, "char": "\u{14}"}));
    gui.type_text("x\r");
    wait_for("the program got Ctrl+T", || {
        capture(&ep, "에이전트").contains("standin-got-1478")
    });
    assert_eq!(terminals(&ep).len(), before, "no Terminal was created");
    // Ctrl+Tab is still a shortcut: never sent to the program.
    gui.press(VK_TAB, true, false);
    gui.until("taken shown", |d| {
        d["phase"] == "attached" && shown(d) == "taken"
    });
    println!(
        "EVIDENCE rename: in-place rename to Korean via the Host, name_conflict kept the field; a disabled Ctrl+T reached the stand-in as 0x14"
    );
}

#[test]
fn shortcuts_off_safe_mode_and_focus_transitions() {
    let host = Host::start("gui-keys-off", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "p");
    new_terminal(&ep, "q");
    // A broken hotkeys.json: safe mode (defaults) and a notice; the file is left alone.
    let appdata = sandbox("keys-off");
    let dir = appdata.join("mouv").join("subak");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hotkeys.json"), b"{ broken").unwrap();
    let gui = Gui::start(&ep, Some("p"), &appdata);
    gui.state("attached");
    let d = gui.dump();
    assert_eq!(d["hotkeys"]["state"], "invalid");
    assert!(
        d["notice"].as_str().unwrap().contains("hotkeys.json"),
        "{d}"
    );
    gui.press(VK_TAB, true, false);
    gui.until("defaults work in safe mode", |d| shown(d) == "q");
    assert_eq!(
        std::fs::read(dir.join("hotkeys.json")).unwrap(),
        b"{ broken"
    );
    drop(gui);
    // A user sequence: its first key waits (and is swallowed); focus loss cancels it.
    std::fs::write(
        dir.join("hotkeys.json"),
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
            "overrides": [{"action": "tab.next", "bindings": ["Ctrl+K Ctrl+N"]}]})
        .to_string(),
    )
    .unwrap();
    let gui = Gui::start(&ep, Some("p"), &appdata);
    gui.state("attached");
    let d = gui.press(u64::from(b'K'), true, false);
    assert_eq!(d["hotkeys"]["pending"], "Ctrl+K");
    gui.cmd(&json!({"cmd": "focus", "on": false}));
    let d = gui.cmd(&json!({"cmd": "focus", "on": true}));
    assert!(d["hotkeys"]["pending"].is_null(), "focus loss cancels");
    assert_eq!(shown(&d), "p");
    gui.press(u64::from(b'K'), true, false);
    gui.press(u64::from(b'N'), true, false);
    gui.until("the sequence switched tabs", |d| shown(d) == "q");
    // Focus follows the shown tab: typing goes to the program of the tab now shown.
    gui.until("q attached", |d| d["phase"] == "attached");
    gui.type_text("echo focus-on-q\r");
    wait_for("q got it", || capture(&ep, "q").contains("focus-on-q"));
    assert!(!capture(&ep, "p").contains("focus-on-q"));
    drop(gui);
    // --no-hotkeys: no shortcut at all; Ctrl+Tab stays on the tab.
    let gui = Gui::start_with(&ep, Some("p"), &appdata, &["--no-hotkeys"]);
    gui.state("attached");
    let d = gui.dump();
    assert_eq!(d["hotkeys"]["off"], "--no-hotkeys");
    assert!(d["shortcuts"].as_array().unwrap().is_empty());
    gui.press(VK_TAB, true, false);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(shown(&gui.dump()), "p");
}

// ---------------------------------------------------------------------------------------------
// Step 19: search, selection, the clipboard, links and "Save output…" (ADR 0048,
// docs/SUBAK_GUI.md §12). `SUBAK-FR-047`, `SUBAK-SEC-013`, `SUBAK-SEC-014`. The window's
// clipboard, link opener and save dialog are recording fakes in the test window: no test here
// touches the real clipboard, opens a browser or shows a dialog.

const MK_LBUTTON: usize = 0x0001;
const MK_RBUTTON: usize = 0x0002;
const MK_CONTROL: usize = 0x0008;

/// The client point (as an `lParam`) in the middle of pane cell (`col`, `row`).
fn cell_lparam(d: &Value, col: usize, row: usize) -> isize {
    let cw = d["cell"][0].as_u64().unwrap() as usize;
    let ch = d["cell"][1].as_u64().unwrap() as usize;
    let pad = d["padding"].as_u64().unwrap() as usize;
    let top = d["bar"]["height"].as_u64().unwrap() as usize;
    let (x, y) = (pad + col * cw + cw / 2, top + pad + row * ch + ch / 2);
    ((y as isize) << 16) | x as isize
}

/// The last screen row holding `needle`, and the (ASCII) column where it starts.
fn last_on_screen(d: &Value, needle: &str) -> (usize, usize) {
    d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter_map(|(r, l)| l.as_str().unwrap().find(needle).map(|c| (r, c)))
        .next_back()
        .unwrap_or_else(|| panic!("`{needle}` is not on the screen:\n{}", screen_text(d)))
}

impl Gui {
    fn key_with(&self, vk: u64, ctrl: bool, shift: bool, alt: bool) -> Value {
        self.cmd(&json!({"cmd": "key", "vk": vk, "ctrl": ctrl, "shift": shift, "alt": alt}))
    }

    /// Typed synchronously (a test command): into the search field while it is open.
    fn typed(&self, s: &str) -> Value {
        self.cmd(&json!({"cmd": "type", "text": s}))
    }
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn search_finds_output_printed_while_no_window_was_attached_and_follows_new_output() {
    let host = Host::start("gui-find", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "find");
    // A first window prints 300 lines and closes: the output stays in the Host's history only.
    {
        let mut g = Gui::start(&ep, Some("find"), &sandbox("find1"));
        g.state("attached");
        g.until("the prompt", |d| screen_text(d).contains('>'));
        g.type_text("for /l %i in (1,1,300) do @echo line %i 한글 marker-%i\r");
        g.until("300 lines", |d| has_line(d, "line 300 한글 marker-300"));
        assert_eq!(g.close(), 0);
    }
    let gui = Gui::start(&ep, Some("find"), &sandbox("find2"));
    gui.state("attached");
    gui.until("the history came back", |d| {
        has_line(d, "line 300 한글 marker-300")
    });
    // Ctrl+Shift+F opens the bar over the pane; typing searches at once, the newest match
    // first, selected, every match highlighted.
    let d = gui.key_with(0x46, true, true, false);
    assert_eq!(d["find"]["query"], "", "{}", d["find"]);
    let d = gui.typed("marker-2");
    assert_eq!(d["find"]["matches"], 111, "{}", d["find"]);
    assert_eq!(d["find"]["status"], "111 of 111");
    assert_eq!(d["selection"], "marker-2");
    assert_eq!(d["highlights"], 111);
    assert!(
        d["banner"].as_str().unwrap().starts_with("find:"),
        "{}",
        d["banner"]
    );
    // Nothing reached cmd.exe while typing in the field.
    assert!(!capture(&ep, "find").contains(">marker-2"));
    // Enter: the next older match; Shift+Enter: newer, wrapping around to the oldest, which
    // the view scrolls back to.
    let d = gui.key_with(0x0D, false, false, false);
    assert_eq!(d["find"]["status"], "110 of 111");
    let d = gui.key_with(0x0D, false, true, false);
    assert_eq!(d["find"]["status"], "111 of 111");
    let d = gui.key_with(0x0D, false, true, false);
    assert_eq!(d["find"]["status"], "1 of 111");
    assert!(
        d["scroll_offset"].as_u64().unwrap() > 200,
        "{}",
        d["scroll_offset"]
    );
    // Alt+R: a regular expression; Ctrl+Backspace clears the field first.
    gui.key_with(0x08, true, false, false);
    let d = gui.key_with(0x52, false, false, true);
    assert_eq!(d["find"]["regex"], true);
    let d = gui.typed(r"marker-1\d$");
    assert_eq!(d["find"]["matches"], 10, "{}", d["find"]);
    // A broken pattern is said in words and searches nothing.
    let d = gui.typed("(");
    assert!(
        d["find"]["status"]
            .as_str()
            .unwrap()
            .starts_with("Not a valid regular expression"),
        "{}",
        d["find"]
    );
    assert_eq!(d["highlights"], 0);
    // AltGr characters (Ctrl+Alt, `@` on a German layout) type into the field; a Ctrl chord
    // that is not a shortcut types nothing.
    gui.key_with(0x08, true, false, false);
    gui.key_with(0x52, false, false, true);
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x51, "ctrl": true, "alt": true, "char": "@"}));
    assert_eq!(d["find"]["query"], "@", "{}", d["find"]);
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x4B, "ctrl": true, "char": "k"}));
    assert_eq!(d["find"]["query"], "@", "{}", d["find"]);
    gui.key_with(0x08, false, false, false);
    // Korean text, as a literal.
    let d = gui.typed("한글");
    assert!(
        d["find"]["matches"].as_u64().unwrap() >= 300,
        "{}",
        d["find"]
    );
    assert_eq!(d["selection"], "한글");
    // Esc closes the bar: the highlights go, the current match stays selected, and opening the
    // bar again starts from that selection.
    let d = gui.key_with(0x1B, false, false, false);
    assert!(d["find"].is_null());
    assert_eq!(d["highlights"], 0);
    assert_eq!(d["selection"], "한글");
    let d = gui.key_with(0x46, true, true, false);
    assert_eq!(
        d["find"]["query"], "한글",
        "a one-line selection is the query"
    );
    gui.key_with(0x1B, false, false, false);
    // An open search follows output the program prints on its own.
    gui.cmd(&json!({"cmd": "key", "vk": 0x23, "ctrl": true, "shift": true}));
    gui.typed("for /l %i in (1,1,4) do @(echo late-hit %i & ping -n 2 127.0.0.1 >nul)\r");
    let d = gui.key_with(0x46, true, true, false);
    assert_eq!(d["find"]["query"], "", "typing cleared the selection");
    gui.typed("late-hit");
    // A selection made meanwhile is left alone by the refreshes.
    let d = gui.cmd(&json!({"cmd": "select", "from": [0, 0], "to": [3, 0]}));
    let mine = d["selection"].clone();
    assert!(!mine.is_null());
    let d = gui.until("the late output found while the bar is open", |d| {
        d["find"]["matches"] == 5
    });
    assert_eq!(
        d["selection"], mine,
        "output did not take the user's selection"
    );
    assert!(d["find"]["status"].as_str().unwrap().ends_with("of 5"));
    assert!(has_line(&d, "late-hit 4"));
}

#[test]
fn paste_asks_before_lines_that_run_or_hidden_control_sequences_and_reads_only_text() {
    let host = Host::start("gui-paste", &["--no-shell"]);
    let ep = host.ep.clone();
    let (id, _) = new_standin(&ep, "paste");
    let gui = Gui::start(&ep, Some("paste"), &sandbox("paste"));
    gui.state("attached");
    let got = |s: &str| format!("standin-got-{}", hex(s));
    // One line without a line break: pasted at once, no question.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "plain"}));
    assert!(d["ask"].is_null(), "{}", d["ask"]);
    gui.typed("\r");
    wait_for("the plain paste", || {
        capture(&ep, &id).contains(&got("plain"))
    });
    // Two lines would run at once: the question names them, Cancel is focused; Esc and Enter
    // on Cancel send nothing.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "one\r\ntwo\r\n"}));
    assert_eq!(
        d["ask"]["words"],
        "Paste 2 lines that run at once? First line: `one`"
    );
    assert_eq!(d["ask"]["focus"], "Cancel");
    assert_eq!(
        d["ask"]["answers"],
        json!(["Cancel", "PasteOneLine", "Paste"])
    );
    let d = gui.key_with(VK_ESCAPE, false, false, false);
    assert!(d["ask"].is_null());
    gui.cmd(&json!({"cmd": "paste", "text": "one\r\ntwo\r\n"}));
    gui.key_with(0x0D, false, false, false);
    // Keys typed while it asks reach nothing.
    gui.cmd(&json!({"cmd": "paste", "text": "one\r\ntwo\r\n"}));
    gui.typed("zzz");
    let d = gui.key_with(VK_ESCAPE, false, false, false);
    assert!(d["ask"].is_null());
    std::thread::sleep(Duration::from_millis(400));
    let c = capture(&ep, &id);
    assert!(!c.contains(&got("one")) && !c.contains("7a7a7a"), "{c}");
    // Paste (Shift+Tab to it): both lines run.
    gui.cmd(&json!({"cmd": "paste", "text": "one\r\ntwo\r\n"}));
    gui.key_with(VK_TAB, false, true, false);
    gui.key_with(0x0D, false, false, false);
    wait_for("both lines", || {
        let c = capture(&ep, &id);
        c.contains(&got("one")) && c.contains(&got("two"))
    });
    // As one line: line breaks become spaces, nothing runs until Enter.
    gui.cmd(&json!({"cmd": "paste", "text": "  cd a \r\n make b\r\n"}));
    gui.key_with(VK_TAB, false, false, false);
    let d = gui.key_with(0x0D, false, false, false);
    assert!(d["ask"].is_null());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!capture(&ep, &id).contains(&got("cd a make b")));
    gui.typed("\r");
    wait_for("one line", || {
        capture(&ep, &id).contains(&got("cd a make b"))
    });
    // A hidden escape sequence: named in the question; pasting removes it.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "echo safe\u{1b}[2Jx"}));
    assert!(
        d["ask"]["words"]
            .as_str()
            .unwrap()
            .starts_with("Paste 1 hidden control character (an escape sequence; removed)"),
        "{}",
        d["ask"]
    );
    assert_eq!(d["ask"]["answers"], json!(["Cancel", "Paste"]));
    gui.key_with(VK_TAB, false, true, false);
    gui.key_with(0x0D, false, false, false);
    gui.typed("\r");
    wait_for("the cleaned paste", || {
        capture(&ep, &id).contains(&got("echo safe[2Jx"))
    });
    // Only plain text is ever pasted: HTML and files alone paste nothing and say so; rich
    // text next to plain text pastes the plain text.
    gui.cmd(
        &json!({"cmd": "clipboard", "formats": [["HTML Format", "<b>x</b>"],
        ["CF_HDROP", "C:\\x.txt"]]}),
    );
    let d = gui.key_with(0x56, true, true, false);
    assert_eq!(d["notice"], "The clipboard holds no text to paste");
    gui.cmd(
        &json!({"cmd": "clipboard", "formats": [["Rich Text Format", "{\\rtf1 rich}"],
        ["CF_UNICODETEXT", "just text"]]}),
    );
    gui.key_with(0x2D, false, true, false); // Shift+Insert
    gui.typed("\r");
    wait_for("the plain text of a rich copy", || {
        capture(&ep, &id).contains(&got("just text"))
    });
    // A large paste asks too.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "x".repeat(6000)}));
    assert!(
        d["ask"]["words"].as_str().unwrap().contains("6 KB of text"),
        "{}",
        d["ask"]
    );
    gui.key_with(VK_ESCAPE, false, false, false);
    gui.typed("quit\r");
}

#[test]
fn copy_formats_keyboard_selection_select_all_and_the_pane_menu() {
    let host = Host::start("gui-select", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "select");
    let gui = Gui::start(&ep, Some("select"), &sandbox("select"));
    gui.state("attached");
    gui.until("the prompt", |d| screen_text(d).contains('>'));
    gui.type_text("echo alpha beta 한글\r");
    let d = gui.until("the output", |d| has_line(d, "alpha beta 한글"));
    let (out_row, _) = last_on_screen(&d, "alpha beta 한글");
    let (prompt_row, _) = last_on_screen(&d, ">");
    // Mark mode (Ctrl+Shift+M) starts at the cursor; the arrows move, Shift+End selects to the
    // line's end, Enter copies and leaves.
    let d = gui.key_with(0x4D, true, true, false);
    assert_eq!(d["marking"], true);
    for _ in out_row..prompt_row {
        gui.key_with(0x26, false, false, false);
    }
    gui.key_with(0x24, false, false, false);
    let d = gui.key_with(0x23, false, true, false);
    assert_eq!(d["selection"], "alpha beta 한글");
    let d = gui.key_with(0x0D, false, false, false);
    assert_eq!(d["marking"], false);
    assert_eq!(d["clipboard"], "alpha beta 한글");
    assert_eq!(
        d["clipboard_formats"],
        json!([["CF_UNICODETEXT", "alpha beta 한글"]])
    );
    // Ctrl+Shift+Right in mark mode grows by words.
    gui.key_with(0x4D, true, true, false);
    for _ in out_row..prompt_row {
        gui.key_with(0x26, false, false, false);
    }
    gui.key_with(0x24, false, false, false);
    gui.key_with(0x27, true, true, false);
    let d = gui.key_with(0x27, true, true, false);
    assert_eq!(d["selection"], "alpha beta");
    gui.key_with(VK_ESCAPE, false, false, false);
    // Copy with colours (the pane menu's item): plain text and HTML Format.
    gui.cmd(&json!({"cmd": "select", "from": [0, out_row], "to": [20, out_row]}));
    let d = gui.cmd(&json!({"cmd": "menu", "id": 11}));
    let formats = d["clipboard_formats"].as_array().unwrap();
    assert_eq!(formats.len(), 2, "{formats:?}");
    assert_eq!(formats[0], json!(["CF_UNICODETEXT", "alpha beta 한글"]));
    assert_eq!(formats[1][0], "HTML Format");
    let html = formats[1][1].as_str().unwrap();
    assert!(html.starts_with("Version:0.9\r\nStartHTML:"), "{html}");
    assert!(
        html.contains("<!--StartFragment--><pre") && html.contains("alpha beta 한글"),
        "{html}"
    );
    // Select all (Ctrl+Shift+A) takes the history and the screen; Ctrl+Shift+C copies it with
    // CR LF line ends.
    let d = gui.key_with(0x41, true, true, false);
    assert!(
        d["selection"]
            .as_str()
            .unwrap()
            .contains("alpha beta 한글\n")
    );
    let d = gui.key_with(0x43, true, true, false);
    let all = d["clipboard"].as_str().unwrap();
    assert!(all.contains("alpha beta 한글\r\n"), "{all}");
    assert!(!all.contains("한글\n") || all.contains("한글\r\n"));
    // The Menu key opens the pane menu with the window's keys, then the tab's items.
    gui.key(VK_APPS as usize);
    let menu = gui.wait("menu", |_| true);
    let labels: Vec<&str> = menu["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    for want in [
        "Copy\tCtrl+Shift+C",
        "Copy with colours (HTML)",
        "Paste\tCtrl+Shift+V",
        "Paste as one line",
        "Select all\tCtrl+Shift+A",
        "Select with the keyboard\tCtrl+Shift+M",
        "Find…\tCtrl+Shift+F",
        "Save output…",
        "Rename…\tCtrl+Shift+R",
    ] {
        assert!(labels.contains(&want), "{want} in {labels:?}");
    }
}

#[test]
fn ctrl_click_opens_only_validated_web_addresses_with_the_open_verb() {
    let host = Host::start("gui-links", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "links");
    let gui = Gui::start(&ep, Some("links"), &sandbox("links"));
    gui.state("attached");
    gui.until("the prompt", |d| screen_text(d).contains('>'));
    gui.type_text("echo see https://example.com/a?b=1, or https://user:pw@evil.example/x\r");
    let line = "see https://example.com/a?b=1, or https://user:pw@evil.example/x";
    let d = gui.until("the output", |d| has_line(d, line));
    let (row, col) = last_on_screen(&d, "https://example.com");
    // A plain click does not open anything.
    let lp = cell_lparam(&d, col + 8, row);
    gui.post(WM_LBUTTONDOWN, MK_LBUTTON, lp);
    gui.post(WM_LBUTTONUP, 0, lp);
    let d = gui.dump();
    assert_eq!(d["opened"], json!([]));
    // Ctrl+click: handed to the (fake) OS opener with the verb `open` and the exact address,
    // without the sentence's comma.
    gui.post(WM_LBUTTONDOWN, MK_LBUTTON | MK_CONTROL, lp);
    gui.post(WM_LBUTTONUP, MK_CONTROL, lp);
    let d = gui.until("opened", |d| {
        d["opened"].as_array().is_some_and(|a| !a.is_empty())
    });
    assert_eq!(
        d["opened"],
        json!([{"verb": "open", "target": "https://example.com/a?b=1"}])
    );
    // An address with a user name and password is refused and says why; it can be copied.
    let (row2, col2) = last_on_screen(&d, "https://user:pw");
    let lp2 = cell_lparam(&d, col2 + 10, row2);
    gui.post(WM_LBUTTONDOWN, MK_LBUTTON | MK_CONTROL, lp2);
    gui.post(WM_LBUTTONUP, MK_CONTROL, lp2);
    let d = gui.until("refused", |d| !d["ask"].is_null());
    assert_eq!(
        d["ask"]["words"],
        "Not opened: it contains a user name or password. Link: https://user:pw@evil.example/x"
    );
    assert_eq!(d["ask"]["answers"], json!(["Cancel", "CopyLink"]));
    gui.key_with(VK_TAB, false, false, false);
    let d = gui.key_with(0x0D, false, false, false);
    assert_eq!(d["clipboard"], "https://user:pw@evil.example/x");
    assert_eq!(
        d["opened"].as_array().unwrap().len(),
        1,
        "nothing else opened"
    );
    // Ctrl+right click: the pane menu offers the link under the pointer.
    gui.post(WM_RBUTTONDOWN, MK_RBUTTON | MK_CONTROL, lp);
    gui.post(WM_RBUTTONUP, MK_CONTROL, lp);
    let menu = gui.wait("menu", |_| true);
    let items = menu["items"].as_array().unwrap();
    assert_eq!(items[0]["label"], "Open link https://example.com/a?b=1");
    assert_eq!(items[1]["label"], "Copy link");
    let d = gui.cmd(&json!({"cmd": "menu", "id": items[1]["id"]}));
    assert_eq!(d["clipboard"], "https://example.com/a?b=1");
    let d = gui.cmd(&json!({"cmd": "menu", "id": items[0]["id"]}));
    assert!(
        d["opened"].as_array().unwrap().len() <= 1,
        "the menu link was used once"
    );
}

#[test]
fn save_output_asks_naming_the_secrets_found_and_writes_only_the_chosen_file() {
    let host = Host::start("gui-export", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "export");
    let gui = Gui::start(&ep, Some("export"), &sandbox("export"));
    gui.state("attached");
    gui.until("the prompt", |d| screen_text(d).contains('>'));
    gui.type_text("echo GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789\r");
    gui.type_text("echo build ok 빌드\r");
    gui.until("the output", |d| has_line(d, "build ok 빌드"));
    let out = sandbox("export-out");
    // "Save output…" asks first, naming what the secret detector found; Cancel is focused and
    // Esc saves nothing and opens no dialog.
    let d = gui.cmd(&json!({"cmd": "menu", "id": 17}));
    let words = d["ask"]["words"].as_str().unwrap().to_string();
    assert!(
        words.starts_with("Save ") && words.contains("of `export` to a file?"),
        "{words}"
    );
    assert!(words.contains("secret-looking values found"), "{words}");
    assert_eq!(
        d["ask"]["answers"],
        json!(["Cancel", "SaveHidden", "SaveExact"])
    );
    assert_eq!(d["ask"]["focus"], "Cancel");
    let d = gui.key_with(VK_ESCAPE, false, false, false);
    assert!(d["ask"].is_null());
    assert_eq!(d["save_dialog"], json!([]));
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
    // Save with them hidden: the (fake) save dialog suggests a name and answers a path.
    let hidden = out.join("hidden.txt");
    gui.cmd(&json!({"cmd": "save_dialog", "path": hidden}));
    gui.cmd(&json!({"cmd": "menu", "id": 17}));
    gui.key_with(VK_TAB, false, false, false);
    gui.key_with(0x0D, false, false, false);
    // (The event comes before the key's dump reply, so it is read from the history.)
    assert!(
        gui.history()
            .contains(r#""event":"saved_output","hidden":true"#),
        "{}",
        gui.history()
    );
    let text = std::fs::read_to_string(&hidden).unwrap();
    assert!(text.contains("GITHUB_TOKEN=***"), "{text}");
    assert!(!text.contains("ghp_"), "{text}");
    assert!(text.contains("build ok 빌드\r\n"), "{text}");
    let d = gui.dump();
    let asked = d["save_dialog"][0].as_str().unwrap();
    assert!(
        asked.starts_with("export-output-") && asked.ends_with(".txt"),
        "{asked}"
    );
    assert!(d["notice"].as_str().unwrap().contains("hidden"));
    // Save exactly: the token is there, as the user chose.
    let exact = out.join("exact.txt");
    gui.cmd(&json!({"cmd": "save_dialog", "path": exact}));
    gui.cmd(&json!({"cmd": "menu", "id": 17}));
    gui.key_with(VK_TAB, false, true, false);
    gui.key_with(0x0D, false, false, false);
    assert!(
        gui.history()
            .contains(r#""event":"saved_output","hidden":false"#)
    );
    let text = std::fs::read_to_string(&exact).unwrap();
    assert!(
        text.contains("GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
        "{text}"
    );
    // A cancelled dialog saves nothing.
    gui.cmd(&json!({"cmd": "save_dialog", "path": null}));
    gui.cmd(&json!({"cmd": "menu", "id": 17}));
    gui.key_with(VK_TAB, false, false, false);
    let d = gui.key_with(0x0D, false, false, false);
    assert_eq!(d["notice"], "Nothing was saved");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_large_history_stays_responsive_while_it_is_searched() {
    let host = Host::start("gui-huge", &["--no-shell"]);
    let ep = host.ep.clone();
    let (code, doc) = sub(
        &ep,
        &[
            "new",
            "huge",
            "--size",
            "100x30",
            "--history-limit",
            "20000",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let gui = Gui::start(&ep, Some("huge"), &sandbox("huge"));
    gui.state("attached");
    gui.until("the prompt", |d| screen_text(d).contains('>'));
    let t = Instant::now();
    gui.type_text(
        "for /l %i in (1,1,15000) do @echo row %i of a large history 한글 with some text\r",
    );
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        let d = gui.dump();
        if has_line(&d, "row 15000 of a large history 한글 with some text") {
            break;
        }
        assert!(Instant::now() < until, "the output did not finish");
        std::thread::sleep(Duration::from_millis(250));
    }
    let printed = t.elapsed();
    gui.key_with(0x46, true, true, false);
    // Each typed character searches all 15 000 retained lines again.
    let t = Instant::now();
    let d = gui.typed("row 1499 ");
    let typed = t.elapsed();
    assert_eq!(d["find"]["matches"], 1, "{}", d["find"]);
    let lines = d["find"]["lines_searched"].as_u64().unwrap();
    let us = d["find"]["elapsed_us"].as_u64().unwrap();
    assert!(lines >= 15_000, "{lines}");
    // A regular expression over every line.
    gui.key_with(0x08, true, false, false);
    gui.key_with(0x08, true, false, false);
    gui.key_with(0x52, false, false, true);
    let t = Instant::now();
    let d = gui.typed(r"row \d*5 of");
    let regex = t.elapsed();
    assert_eq!(d["find"]["matches"], 1500, "{}", d["find"]);
    let regex_us = d["find"]["elapsed_us"].as_u64().unwrap();
    // The window answers at once afterwards.
    let t = Instant::now();
    gui.dump();
    let answer = t.elapsed();
    println!(
        "EVIDENCE large history (debug build): 15 000 rows printed in {printed:?}; typing a \
         9-character query (9 searches) {typed:?}, the last search {lines} lines in {us} µs; \
         regex `row \\d*5 of` 1 500 matches in {regex_us} µs ({regex:?} for its 11 characters); \
         a dump round trip afterwards {answer:?}"
    );
    assert!(us < 2_000_000, "{us} µs");
    assert!(answer < Duration::from_millis(1000), "{answer:?}");
}
