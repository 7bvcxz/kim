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
//! `SUBAK-FR-046`, `SUBAK-NFR-002`.
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
    WM_KEYDOWN, WM_SIZE,
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
        let mut cmd = Command::new(gui_exe());
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
    // Paste (the test window's private clipboard, never the system one): CR LF becomes CR.
    let d = gui.cmd(&json!({"cmd": "paste", "text": "echo pasted-text\r\n"}));
    assert_eq!(d["clipboard"], "echo pasted-text\r\n");
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
