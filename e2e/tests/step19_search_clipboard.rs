//! E2E — Step 19: search, clipboard, links and "Save output…" in the Subak window
//! (docs/SUBAK_GUI.md §12, ADR 0048). `SUBAK-FR-047`, `SUBAK-SEC-013`, `SUBAK-SEC-014`.
//!
//! Scenario A: a user prints output in a window and closes it; a new window attaches later and
//! searches that history with `Ctrl+Shift+F` (incremental, Korean, older/newer, a regular
//! expression, a refused broken pattern), then `Esc` keeps the current match selected.
//!
//! Scenario B: the same user pastes two lines (the window asks; Cancel sends nothing, Paste runs
//! both), copies a line with colours (plain text and HTML Format), Ctrl+clicks a web address
//! (handed to the OS "open" verb — here the window's recording fake) and a credential-bearing one
//! (refused), and saves the output with the secret-looking values hidden.
//!
//! The window is the **debug** build's off-screen test window (`--test-offscreen`), driven only
//! with its own window messages (`WM_CHAR`, mouse messages with `MK_CONTROL`, `WM_COPYDATA` test
//! commands). Its clipboard, link opener and save dialog are recording fakes: the system
//! clipboard is never used, no browser opens, no dialog is shown, nothing leaves the machine.
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
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);
const WM_CLOSE: u32 = 0x0010;
const WM_CHAR: u32 = 0x0102;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_COPYDATA: u32 = 0x004A;
const MK_LBUTTON: usize = 0x0001;
const MK_CONTROL: usize = 0x0008;
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
    format!("subak-e2e19-{tag}-{}", std::process::id())
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
    fn open(exe: &Path, ep: &str, target: &str, appdata: &Path) -> Self {
        let mut child = Command::new(exe)
            .args([
                "--test-offscreen",
                "--test-max-seconds",
                "120",
                "--endpoint",
                ep,
                target,
            ])
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
        w.wait("state", |v| v["phase"] == "attached");
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

    /// A test command; returns the window's state after it.
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

    fn key(&self, vk: u64, ctrl: bool, shift: bool, alt: bool) -> Value {
        self.cmd(&json!({"cmd": "key", "vk": vk, "ctrl": ctrl, "shift": shift, "alt": alt}))
    }

    fn shows_line(&self, line: &str) -> Value {
        let until = Instant::now() + LONG;
        loop {
            let d = self.cmd(&json!({"cmd": "dump"}));
            if d["screen"]
                .as_array()
                .unwrap()
                .iter()
                .any(|l| l.as_str().unwrap().trim() == line)
            {
                return d;
            }
            assert!(Instant::now() < until, "`{line}` not in the window: {d:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
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

    /// Ctrl+click on the (ASCII) text `needle` in its last screen row.
    fn ctrl_click(&self, d: &Value, needle: &str, offset: usize) {
        let (row, col) = d["screen"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .filter_map(|(r, l)| l.as_str().unwrap().find(needle).map(|c| (r, c)))
            .next_back()
            .unwrap_or_else(|| panic!("`{needle}` not on the screen"));
        let cw = d["cell"][0].as_u64().unwrap() as usize;
        let ch = d["cell"][1].as_u64().unwrap() as usize;
        let pad = d["padding"].as_u64().unwrap() as usize;
        let top = d["bar"]["height"].as_u64().unwrap() as usize;
        let x = pad + (col + offset) * cw + cw / 2;
        let y = top + pad + row * ch + ch / 2;
        let lp = ((y as isize) << 16) | x as isize;
        self.post(WM_LBUTTONDOWN, MK_LBUTTON | MK_CONTROL, lp);
        self.post(WM_LBUTTONUP, MK_CONTROL, lp);
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
fn a_later_window_searches_output_kept_by_the_host() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("find");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e19-find");
    let (code, doc) = json(&subak, &ep, &["new", "work", "--size", "100x30"]);
    assert_eq!(code, 0, "{doc}");
    {
        let mut w = Window::open(&gui, &ep, "work", appdata.path());
        let until = Instant::now() + LONG;
        while !w.cmd(&json!({"cmd": "dump"}))["screen"]
            .to_string()
            .contains('>')
        {
            assert!(Instant::now() < until, "no prompt");
            std::thread::sleep(Duration::from_millis(100));
        }
        w.type_text("for /l %i in (1,1,200) do @echo build step %i 빌드 tag-%i\r");
        w.shows_line("build step 200 빌드 tag-200");
        assert_eq!(w.close(), 0);
    }
    let w = Window::open(&gui, &ep, "work", appdata.path());
    w.shows_line("build step 200 빌드 tag-200");
    let d = w.key(0x46, true, true, false);
    assert!(!d["find"].is_null(), "Ctrl+Shift+F opens the search bar");
    let d = w.cmd(&json!({"cmd": "type", "text": "tag-1"}));
    // tag-1, tag-10…19, tag-100…199
    assert_eq!(d["find"]["matches"], 111, "{}", d["find"]);
    assert_eq!(d["find"]["status"], "111 of 111");
    let d = w.key(0x0D, false, true, false);
    assert_eq!(
        d["find"]["status"], "1 of 111",
        "Shift+Enter wraps to the oldest"
    );
    assert!(d["scroll_offset"].as_u64().unwrap() > 0);
    w.key(0x08, true, false, false);
    let d = w.cmd(&json!({"cmd": "type", "text": "빌드"}));
    assert!(d["find"]["matches"].as_u64().unwrap() >= 200);
    w.key(0x08, true, false, false);
    w.key(0x52, false, false, true);
    let d = w.cmd(&json!({"cmd": "type", "text": r"tag-\d\d$"}));
    assert_eq!(d["find"]["matches"], 90, "{}", d["find"]);
    let d = w.cmd(&json!({"cmd": "type", "text": "["}));
    assert!(
        d["find"]["status"]
            .as_str()
            .unwrap()
            .starts_with("Not a valid regular expression")
    );
    let d = w.key(0x1B, false, false, false);
    assert!(d["find"].is_null());
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn paste_copy_links_and_save_output_ask_first_and_stay_local() {
    let subak = bin_debug("subak");
    let gui = bin_debug("subak-gui");
    let ep = endpoint("clip");
    let _c = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let appdata = TempDir::new("e2e19-clip");
    let out = TempDir::new("e2e19-out");
    let (code, doc) = json(&subak, &ep, &["new", "clip", "--size", "100x30"]);
    assert_eq!(code, 0, "{doc}");
    let w = Window::open(&gui, &ep, "clip", appdata.path());
    // Two lines would run at once: the window asks, Cancel is focused and sends nothing.
    let d = w.cmd(&json!({"cmd": "paste", "text": "echo e2e19-one\r\necho e2e19-two\r\n"}));
    assert_eq!(d["ask"]["focus"], "Cancel", "{}", d["ask"]);
    w.key(0x0D, false, false, false);
    std::thread::sleep(Duration::from_millis(500));
    let (_, cap) = json(&subak, &ep, &["capture", "clip"]);
    assert!(!cap.to_string().contains("e2e19-one\""));
    // Paste (Shift+Tab, Enter): both run.
    w.cmd(&json!({"cmd": "paste", "text": "echo e2e19-one\r\necho e2e19-two\r\n"}));
    w.key(0x09, false, true, false);
    w.key(0x0D, false, false, false);
    w.shows_line("e2e19-two");
    // Copy a line with colours: plain text and HTML Format on the (fake) clipboard.
    let d = w.shows_line("e2e19-one");
    let row = d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .rposition(|l| l.as_str().unwrap().trim() == "e2e19-one")
        .unwrap();
    w.cmd(&json!({"cmd": "select", "from": [0, row], "to": [8, row]}));
    let d = w.cmd(&json!({"cmd": "menu", "id": 11}));
    let formats = d["clipboard_formats"].as_array().unwrap();
    assert_eq!(formats[0], json!(["CF_UNICODETEXT", "e2e19-one"]));
    assert_eq!(formats[1][0], "HTML Format");
    // Ctrl+click a web address: handed to the OS open verb (the fake records it).
    w.type_text("echo docs at https://example.com/e2e19 and https://u:p@evil.example/\r");
    let d = w.shows_line("docs at https://example.com/e2e19 and https://u:p@evil.example/");
    w.ctrl_click(&d, "https://example.com", 6);
    let ev = w.wait("opened", |_| true);
    assert_eq!(ev["verb"], "open");
    assert_eq!(ev["target"], "https://example.com/e2e19");
    // A credential-bearing address is refused, not opened.
    w.ctrl_click(&d, "https://u:p@", 4);
    let ev = w.wait("link", |e| e["valid"] == false);
    assert_eq!(ev["target"], "https://u:p@evil.example/");
    let d = w.key(0x1B, false, false, false);
    assert_eq!(d["opened"].as_array().unwrap().len(), 1);
    // Save output with the secret-looking values hidden, to the path the (fake) dialog gives.
    w.type_text("echo API_KEY=sk-abcdefghijklmnopqrstuvwxyz0123\r");
    w.shows_line("API_KEY=sk-abcdefghijklmnopqrstuvwxyz0123");
    let file = out.path().join("clip.txt");
    w.cmd(&json!({"cmd": "save_dialog", "path": file}));
    let d = w.cmd(&json!({"cmd": "menu", "id": 17}));
    assert!(
        d["ask"]["words"]
            .as_str()
            .unwrap()
            .contains("secret-looking value")
    );
    w.key(0x09, false, false, false);
    w.key(0x0D, false, false, false);
    let until = Instant::now() + LONG;
    while !file.exists() {
        assert!(Instant::now() < until, "the export was not written");
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("API_KEY=***"), "{text}");
    assert!(!text.contains("abcdefghijklmnopqrstuvwxyz0123"), "{text}");
    assert!(text.contains("e2e19-two"));
}
