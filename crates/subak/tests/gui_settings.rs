//! Step 20 — the Subak window's settings, themes, fonts, motion, profiles and polish against real
//! Hosts on private endpoints (docs/SUBAK_GUI.md §13, ADR 0051). `SUBAK-FR-011`, `SUBAK-FR-031`,
//! `SUBAK-FR-046`, `SUBAK-FR-048`, `SUBAK-FR-049`, `KIM-NFR-012`, `KIM-FR-013`.
//!
//! As in `gui_window.rs`, every window is the debug build's off-screen test window
//! (`--test-offscreen`: placed at (-20000, -20000), a tool window that never activates), driven
//! only by its own window messages and `WM_COPYDATA` test commands — never synthesized input —
//! with recording fakes for the clipboard and the opener. Each test has its own `APPDATA`
//! sandbox, so `config.toml` and `gui.json` are test-private; the maintainer's settings are never
//! read or written. High Contrast, the light app mode and "Show animations" are simulated with
//! the `system` test command (the window ignores the machine's own while testing). Frames are
//! the window's own content rendered to memory; nothing captures the screen.
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
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_RBUTTONDOWN,
};

const COPYDATA_TEST: usize = 0x5355_424B;
const WM_MOUSELEAVE: u32 = 0x02A3;
const MK_LBUTTON: usize = 0x0001;

fn gui_exe() -> &'static str {
    env!("CARGO_BIN_EXE_subak-gui")
}

/// A private `APPDATA` for one test's `config.toml` and `gui.json`.
fn sandbox(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "subak-gui-settings-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(d.join("mouv").join("subak")).unwrap();
    d
}

fn config_path(appdata: &Path) -> PathBuf {
    appdata.join("mouv").join("subak").join("config.toml")
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
        g
    }

    fn history(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    /// Whether an event named `name` was reported so far.
    fn seen(&self, needle: &str) -> bool {
        self.log.lock().unwrap().iter().any(|l| l.contains(needle))
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

    fn state(&self, phase: &str) -> Value {
        self.wait("state", |v| v["phase"] == phase)
    }

    fn post(&self, msg: u32, wp: usize, lp: isize) {
        // SAFETY: posting a message to the test's own off-screen window.
        let ok = unsafe { PostMessageW(self.hwnd as _, msg, wp, lp) };
        assert_ne!(ok, 0, "PostMessageW");
    }

    fn type_text(&self, s: &str) {
        for u in s.encode_utf16() {
            self.post(WM_CHAR, usize::from(u), 1);
        }
    }

    fn cmd(&self, c: &Value) -> Value {
        self.send(c);
        let reply = if c["cmd"] == "shot" { "shot" } else { "dump" };
        self.wait(reply, |_| true)
    }

    /// A test command whose processing reports `name` (before its own reply): that event, then
    /// the reply (dump).
    fn cmd_then(&self, c: &Value, name: &str, pred: impl Fn(&Value) -> bool) -> (Value, Value) {
        self.send(c);
        let ev = self.wait(name, pred);
        let d = self.wait("dump", |_| true);
        (ev, d)
    }

    /// The first logged event named `name` (also one a `wait` skipped).
    fn logged(&self, name: &str) -> Option<Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["event"] == name)
    }

    /// Sends a test command synchronously (`WM_COPYDATA`); its events and reply follow.
    fn send(&self, c: &Value) {
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

    /// The window regains the focus: it reads config.toml (and hotkeys.json) again.
    fn refocus(&self) -> Value {
        self.cmd(&json!({"cmd": "focus", "on": false}));
        self.cmd(&json!({"cmd": "focus", "on": true}))
    }

    /// Waits until no transition runs (a frame shot shows the settled state).
    fn settled(&self) -> Value {
        self.until("no transition runs", |d| {
            d["motion"]["active"].as_array().is_some_and(Vec::is_empty)
        })
    }

    /// A frame of the window's own content: (width, height, pixel getter).
    fn shot(
        &self,
        dir: &Path,
        name: &str,
    ) -> (u32, u32, impl Fn(u32, u32) -> (u8, u8, u8) + use<>) {
        let path = dir.join(format!("{name}.bmp"));
        let r = self.cmd(&json!({"cmd": "shot", "path": path}));
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
    }

    fn close(&mut self) -> i32 {
        self.post(WM_CLOSE, 0, 0);
        let until = Instant::now() + LONG;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code().unwrap_or(-1);
            }
            assert!(Instant::now() < until, "subak-gui did not exit");
            std::thread::sleep(Duration::from_millis(25));
        }
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

/// `subak --json --endpoint <ep> <args>` (the test's own APPDATA is irrelevant to the Host).
fn sub(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    run(&a)
}

/// `subak --json <args>` with the window's APPDATA (config and profile commands, `new`).
fn sub_in(appdata: &Path, args: &[&str]) -> (i32, Value) {
    let a = appdata.display().to_string();
    let (code, doc) = run_env(args, &[("APPDATA", a.as_str())]);
    validate("subak-cli-v1", &doc);
    (code, doc)
}

fn set(appdata: &Path, key: &str, value: &str) {
    let (code, doc) = sub_in(appdata, &["config", "set", key, value]);
    assert_eq!(code, 0, "{doc}");
}

fn new_terminal(ep: &str, name: &str) -> (String, u32) {
    let (code, doc) = sub(ep, &["new", name, "--size", "100x30", "--no-profile"]);
    assert_eq!(code, 0, "{doc}");
    let t = &doc["data"]["terminal"];
    (
        t["id"].as_str().unwrap().to_string(),
        pid_of(&t["windows"][0]["panes"][0]["pid"]),
    )
}

fn pane_of(ep: &str, target: &str) -> Value {
    let (code, doc) = sub(ep, &["info", target]);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["terminal"]["windows"][0]["panes"][0].clone()
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

fn hex(c: (u8, u8, u8)) -> String {
    format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

/// The client point (as an `lParam`) in the middle of pane cell (`col`, `row`).
fn cell_lparam(d: &Value, col: usize, row: usize) -> isize {
    let cw = d["cell"][0].as_u64().unwrap() as usize;
    let ch = d["cell"][1].as_u64().unwrap() as usize;
    let pad = d["padding"].as_u64().unwrap() as usize;
    let top = d["bar"]["height"].as_u64().unwrap() as usize;
    let (x, y) = (pad + col * cw + cw / 2, top + pad + row * ch + ch / 2);
    ((y as isize) << 16) | x as isize
}

/// The client point (as an `lParam`) in the middle of the bar's span `what` (`tab:1`, …).
fn bar_lparam(d: &Value, what: &str) -> (isize, u32) {
    let span = d["bar"]["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s[2] == what)
        .unwrap_or_else(|| panic!("no span {what}: {}", d["bar"]))
        .clone();
    let cw = d["bar"]["cell"][0].as_u64().unwrap() as u32;
    let pad = d["bar"]["pad_x"].as_u64().unwrap() as u32;
    let x = pad + (span[0].as_u64().unwrap() as u32 + 2) * cw + cw / 2;
    let y = d["bar"]["height"].as_u64().unwrap() as u32 / 2;
    (((y as isize) << 16) | x as isize, x)
}

/// Every label of a menu event, with the submenu path (`Settings/Theme/Kim light`), and its id.
fn menu_items(v: &Value, path: &str, out: &mut Vec<(String, u64, bool)>) {
    for it in v.as_array().into_iter().flatten() {
        let label = it["label"].as_str().unwrap_or("");
        let p = if path.is_empty() {
            label.to_string()
        } else {
            format!("{path}/{label}")
        };
        out.push((
            p.clone(),
            it["id"].as_u64().unwrap_or(0),
            it["checked"] == true,
        ));
        menu_items(&it["items"], &p, out);
    }
}

fn find_item(items: &[(String, u64, bool)], path: &str) -> (u64, bool) {
    items
        .iter()
        .find(|(p, _, _)| p == path)
        .map(|(_, id, c)| (*id, *c))
        .unwrap_or_else(|| {
            panic!(
                "no menu item `{path}`; items:\n{}",
                items
                    .iter()
                    .map(|i| i.0.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

/// Opens the tab menu (recorded, never shown) and returns its items.
fn open_menu(gui: &Gui) -> Vec<(String, u64, bool)> {
    let (ev, _) = gui.cmd_then(&json!({"cmd": "menu", "tab": 0}), "menu", |_| true);
    let mut items = Vec::new();
    menu_items(&ev["items"], "", &mut items);
    items
}

fn shots_dir(tag: &str) -> PathBuf {
    std::env::var_os("SUBAK_GUI_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| sandbox(tag))
}

#[test]
fn themes_fonts_layout_opacity_and_high_contrast_in_rendered_frames() {
    let host = Host::start("gui-look", &["--no-shell"]);
    let ep = host.ep.clone();
    let (_, pid) = new_terminal(&ep, "look");
    let appdata = sandbox("look");
    let dir = shots_dir("look-shots");
    std::fs::create_dir_all(&dir).unwrap();
    set(&appdata, "appearance.theme", "kim-light");
    let mut gui = Gui::start(&ep, Some("look"), &appdata);
    gui.state("attached");
    gui.type_text("echo 한글 Subak\r");
    let d = gui.until("output", |d| screen_text(d).contains("한글 Subak"));
    assert_eq!(d["theme"], "kim-light");
    assert_eq!(d["theme_colors"]["chrome"], "kim-light");
    assert_eq!(d["icon"], true, "the watermelon window icon is set");
    let d = gui.settled();
    let (w, h, px) = gui.shot(&dir, "step20_light");
    let top = d["bar"]["height"].as_u64().unwrap() as u32;
    assert_eq!(hex(px(1, 1)), "ECEBF2", "kim-light bg.raised: the tab bar");
    assert_eq!(
        hex(px(w - 2, h - 2)),
        "F7F6FA",
        "kim-light terminal.background"
    );
    let (_, x) = bar_lparam(&d, "tab:0");
    assert_eq!(
        hex(px(x, 0)),
        "5F49C4",
        "kim-light accent: the active tab's marker"
    );
    assert_eq!(
        hex(px(x, top - 1)),
        "F7F6FA",
        "the active tab joins the pane"
    );
    // The CLI changes the theme; the window applies it when it gets the focus.
    set(&appdata, "appearance.theme", "kim-dark");
    let d = gui.refocus();
    assert_eq!(d["theme"], "kim-dark");
    gui.settled();
    let (w, h, px) = gui.shot(&dir, "step20_dark");
    assert_eq!(
        hex(px(w - 2, h - 2)),
        "0E0E12",
        "kim-dark terminal.background"
    );
    assert_eq!(hex(px(1, 1)), "15151B", "kim-dark bg.raised");
    // Line height, padding and the font family: the cells and the grid follow, the pane is
    // resized by the Host to the new grid, and its process is the same.
    let before = d["cell"][1].as_u64().unwrap();
    set(&appdata, "appearance.line_height", "1.6");
    set(&appdata, "appearance.padding", "0");
    let d = gui.refocus();
    let ratio = d["cell_height_ratio"].as_f64().unwrap();
    assert!((1.55..1.7).contains(&ratio), "{ratio}");
    assert!(d["cell"][1].as_u64().unwrap() > before);
    assert_eq!(d["padding"], 0);
    let grid = (
        d["grid"][0].as_u64().unwrap(),
        d["grid"][1].as_u64().unwrap(),
    );
    let until = Instant::now() + LONG;
    while (
        pane_of(&ep, "look")["size"]["cols"].as_u64().unwrap(),
        pane_of(&ep, "look")["size"]["rows"].as_u64().unwrap(),
    ) != grid
    {
        assert!(Instant::now() < until, "the pane follows the new grid");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        pid_of(&pane_of(&ep, "look")["pid"]),
        pid,
        "the same process"
    );
    set(&appdata, "appearance.font_family", "Courier New");
    let d = gui.refocus();
    assert_eq!(d["face"], "Courier New");
    let (_, _, px_courier) = gui.shot(&dir, "step20_courier");
    set(&appdata, "appearance.font_family", "No Such Font 20");
    let d = gui.refocus();
    assert!(
        d["notice"]
            .as_str()
            .unwrap_or("")
            .contains("is not installed"),
        "{d}"
    );
    assert_ne!(d["face"], "No Such Font 20");
    drop(px_courier);
    // Window opacity: a layered window with a constant alpha; High Contrast is always opaque.
    set(&appdata, "appearance.opacity", "80");
    gui.refocus();
    gui.until("80 % of 255", |d| d["opacity_applied"] == 204);
    set(&appdata, "appearance.theme", "high-contrast");
    let d = gui.refocus();
    assert_eq!(d["theme"], "high-contrast");
    gui.until("High Contrast is opaque", |d| d["opacity_applied"] == 255);
    // High Contrast (simulated as Windows reports it): `auto` takes the system colours.
    set(&appdata, "appearance.theme", "auto");
    gui.refocus();
    let d = gui.cmd(&json!({"cmd": "system", "high_contrast": true}));
    assert_eq!(d["theme"], "high-contrast");
    let sys = {
        use windows_sys::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColor};
        // SAFETY: a plain system colour query.
        let c = unsafe { GetSysColor(COLOR_WINDOW) };
        format!(
            "{:02X}{:02X}{:02X}",
            c & 0xFF,
            (c >> 8) & 0xFF,
            (c >> 16) & 0xFF
        )
    };
    assert_eq!(d["theme_colors"]["background"], sys.as_str());
    gui.settled();
    let (w, h, px) = gui.shot(&dir, "step20_high_contrast");
    assert_eq!(hex(px(w - 2, h - 2)), sys, "the system window colour");
    // The light app mode with `system`.
    let d = gui.cmd(&json!({"cmd": "system", "high_contrast": false, "light_mode": true}));
    assert_eq!(d["theme"], "kim-dark", "auto stays dark in light mode");
    set(&appdata, "appearance.theme", "system");
    let d = gui.refocus();
    assert_eq!(d["theme"], "kim-light");
    assert_eq!(gui.close(), 0);
    println!("EVIDENCE look: shots in {}", dir.display());
}

#[test]
fn transitions_run_briefly_and_reduced_motion_runs_none() {
    let host = Host::start("gui-motion", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "one");
    new_terminal(&ep, "two");
    let appdata = sandbox("motion");
    set(&appdata, "appearance.motion", "full");
    let gui = Gui::start(&ep, Some("one"), &appdata);
    gui.state("attached");
    gui.settled();
    // Ctrl+Tab: the marker fades in over 120 ms, then the timer stops.
    let (m, _) = gui.cmd_then(
        &json!({"cmd": "key", "vk": 0x09, "ctrl": true}),
        "motion",
        |v| v["what"] == "tab",
    );
    assert_eq!(m["ms"], 120);
    let t0 = Instant::now();
    gui.wait("motion_done", |_| true);
    assert!(t0.elapsed() < Duration::from_secs(3));
    let d = gui.settled();
    assert_eq!(d["motion"]["enabled"], true);
    // A banner fades in and rises over 160 ms.
    sub(&ep, &["detach", "two"]);
    gui.state("ended");
    let m = gui.wait("motion", |v| v["what"] == "banner");
    assert_eq!(m["ms"], 160);
    gui.wait("motion_done", |_| true);
    // Reduced motion (the setting): nothing runs, no timer, the final state at once.
    set(&appdata, "appearance.motion", "reduced");
    let d = gui.refocus();
    assert_eq!(d["motion"]["enabled"], false);
    gui.cmd(&json!({"cmd": "key", "vk": 0x0D}));
    gui.state("attached");
    let (m, _) = gui.cmd_then(
        &json!({"cmd": "key", "vk": 0x09, "ctrl": true}),
        "motion",
        |v| v["what"] == "tab",
    );
    assert_eq!(m["ms"], 0, "reduced motion: no transition");
    assert!(
        gui.dump()["motion"]["active"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // `system`: Windows' "Show animations" off means no motion either.
    set(&appdata, "appearance.motion", "system");
    gui.refocus();
    let d = gui.cmd(&json!({"cmd": "system", "animations": false}));
    assert_eq!(d["motion"]["enabled"], false);
    let (m, _) = gui.cmd_then(
        &json!({"cmd": "key", "vk": 0x09, "ctrl": true}),
        "motion",
        |v| v["what"] == "tab",
    );
    assert_eq!(m["ms"], 0);
    let d = gui.cmd(&json!({"cmd": "system", "animations": true}));
    assert_eq!(d["motion"]["enabled"], true);
}

/// The stand-in program of a profile here: `cmd.exe /d /k` printing a marker with the
/// profile's variable, in the profile's folder.
fn profile_toml(work: &Path) -> String {
    format!(
        r#"config_version = 1
[terminal]
default_profile = "echo"

[[profiles]]
name = "echo"
program = "cmd.exe"
args = ["/d", "/k", "echo profile-%SUBAK_PROFILE_MODE%-ok"]
cwd = '{}'
env = {{ SUBAK_PROFILE_MODE = "fast" }}

[[profiles]]
name = "second"
program = "cmd.exe"
args = ["/d", "/k", "echo second-profile-ok"]
"#,
        work.display()
    )
}

#[test]
fn profiles_the_settings_menu_and_mouse_and_keyboard_choices() {
    let host = Host::start("gui-profiles", &["--no-shell"]);
    let ep = host.ep.clone();
    let appdata = sandbox("profiles");
    let work = appdata.join("작업");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(config_path(&appdata), profile_toml(&work)).unwrap();
    let gui = Gui::start(&ep, None, &appdata);
    gui.state("empty");
    // Ctrl+T starts the default profile: its program, arguments, folder and environment.
    let (ev, _) = gui.cmd_then(
        &json!({"cmd": "key", "vk": u32::from(b'T'), "ctrl": true}),
        "new_terminal",
        |_| true,
    );
    assert_eq!(ev["profile"], "echo");
    gui.state("attached");
    let d = gui.until("the profile's output", |d| {
        screen_text(d).contains("profile-fast-ok")
    });
    let shown = d["terminal"]["name"].as_str().unwrap().to_string();
    let p = pane_of(&ep, &shown);
    assert_eq!(p["cwd"], work.display().to_string());
    assert!(p["env"]["set"].to_string().contains("SUBAK_PROFILE_MODE"));
    // The menu: a New Terminal item per profile and the settings menu, with checks.
    let items = open_menu(&gui);
    let (second, _) = find_item(&items, "New Terminal: second");
    find_item(&items, "New Terminal: echo (default)");
    let (_, checked) = find_item(
        &items,
        "Settings/Theme/Automatic (Kim dark; High Contrast when Windows uses it)",
    );
    assert!(checked, "the current theme is checked");
    find_item(
        &items,
        "Settings/Terminal font/Ligatures: not drawn (each cell is drawn on its own, ADR 0051)",
    );
    gui.cmd(&json!({"cmd": "menu", "id": second}));
    gui.until("the second profile", |d| {
        screen_text(d).contains("second-profile-ok")
    });
    // A settings choice from the menu: applied at once and saved in config.toml (line edit).
    let items = open_menu(&gui);
    let (light, _) = find_item(&items, "Settings/Theme/Kim light");
    let (_, d) = gui.cmd_then(
        &json!({"cmd": "menu", "id": light}),
        "settings_saved",
        |v| v["key"] == "appearance.theme",
    );
    assert_eq!(d["theme"], "kim-light");
    let text = std::fs::read_to_string(config_path(&appdata)).unwrap();
    assert!(text.contains("theme = \"kim-light\""), "{text}");
    assert!(text.contains("[[profiles]]"), "the profiles stay: {text}");
    assert!(gui.seen("\"event\":\"toast\""));
    let (code, doc) = sub_in(&appdata, &["config", "show"]);
    assert_eq!(code, 0);
    assert_eq!(
        doc["data"]["settings"][0]["value"], "kim-light",
        "the CLI agrees"
    );
    // Copy on select (from the menu): a mouse selection is copied when released, and stays.
    let items = open_menu(&gui);
    let (cos, on) = find_item(&items, "Settings/Copy on select");
    assert!(!on);
    gui.cmd(&json!({"cmd": "menu", "id": cos}));
    gui.type_text("echo alpha-beta gamma\r");
    let d = gui.until("echo", |d| screen_text(d).contains("alpha-beta gamma\n"));
    let row = d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .rposition(|l| l.as_str().unwrap().starts_with("alpha-beta gamma"))
        .unwrap();
    gui.post(WM_LBUTTONDOWN, MK_LBUTTON, cell_lparam(&d, 0, row));
    gui.post(WM_MOUSEMOVE, MK_LBUTTON, cell_lparam(&d, 9, row));
    gui.post(WM_LBUTTONUP, 0, cell_lparam(&d, 9, row));
    let d = gui.until("copied on select", |d| d["clipboard"] == "alpha-beta");
    assert_eq!(d["selection"], "alpha-beta", "the selection stays");
    // Word separators: only a blank ends a word, so a double click takes `alpha-beta` whole.
    sub_in(&appdata, &["config", "set", "terminal.word_separators= "]);
    let d = gui.refocus();
    assert_eq!(d["settings"]["word_separators"], " ");
    gui.post(WM_LBUTTONDOWN, MK_LBUTTON, cell_lparam(&d, 2, row));
    gui.post(WM_LBUTTONUP, 0, cell_lparam(&d, 2, row));
    gui.post(WM_LBUTTONDBLCLK, MK_LBUTTON, cell_lparam(&d, 2, row));
    gui.post(WM_LBUTTONUP, 0, cell_lparam(&d, 2, row));
    gui.until("a whole word", |d| d["selection"] == "alpha-beta");
    // Smart Ctrl+C (off by default): with a selection it copies; without, ^C reaches cmd.
    gui.cmd(&json!({"cmd": "clipboard", "formats": [["CF_UNICODETEXT", "x"]]}));
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x43, "ctrl": true, "char": "\u{3}"}));
    assert_ne!(d["clipboard"], "alpha-beta", "off: Ctrl+C is the program's");
    set(&appdata, "terminal.smart_ctrl_c", "true");
    gui.refocus();
    gui.cmd(&json!({"cmd": "select", "from": [0, row], "to": [4, row]}));
    let d = gui.cmd(&json!({"cmd": "key", "vk": 0x43, "ctrl": true, "char": "\u{3}"}));
    assert_eq!(d["clipboard"], "alpha", "on: Ctrl+C copies the selection");
    assert_eq!(d["selection"], Value::Null, "a copy clears the selection");
    // Right click opens the pane menu; middle click pastes.
    set(&appdata, "terminal.right_click", "menu");
    set(&appdata, "terminal.middle_click", "paste");
    let d = gui.refocus();
    gui.post(WM_RBUTTONDOWN, 0, cell_lparam(&d, 1, row));
    let ev = gui.wait("menu", |_| true);
    assert!(ev["items"].to_string().contains("Select all"), "{ev}");
    gui.cmd(&json!({"cmd": "clipboard", "formats": [["CF_UNICODETEXT", "echo middle-paste"]]}));
    gui.post(WM_MBUTTONDOWN, 0, cell_lparam(&d, 1, row));
    gui.until("pasted", |d| screen_text(d).contains("echo middle-paste"));
    // "Edit settings file…" opens config.toml with the recording opener (never Notepad here).
    let items = open_menu(&gui);
    let (edit, _) = find_item(&items, "Settings/Edit settings file (config.toml)…");
    let d = gui.cmd(&json!({"cmd": "menu", "id": edit}));
    let opened = d["opened"].as_array().unwrap();
    assert!(
        opened
            .iter()
            .any(|o| o["verb"] == "edit" && o["target"].as_str().unwrap().ends_with("config.toml")),
        "{d}"
    );
}

#[test]
fn hover_on_tabs_and_keyboard_access_to_the_settings() {
    let host = Host::start("gui-hover", &["--no-shell"]);
    let ep = host.ep.clone();
    new_terminal(&ep, "first");
    new_terminal(&ep, "second");
    let appdata = sandbox("hover");
    set(&appdata, "appearance.motion", "reduced");
    let gui = Gui::start(&ep, Some("first"), &appdata);
    gui.state("attached");
    let d = gui.until("two tabs", |d| d["tabs"].as_array().unwrap().len() == 2);
    let (lp, x) = bar_lparam(&d, "tab:1");
    gui.post(WM_MOUSEMOVE, 0, lp);
    let ev = gui.wait("hover", |v| v["tab"] == 1);
    assert_eq!(ev["tab"], 1);
    let dir = shots_dir("hover-shots");
    std::fs::create_dir_all(&dir).unwrap();
    let (_, _, px) = gui.shot(&dir, "step20_hover");
    let top = d["bar"]["height"].as_u64().unwrap() as u32;
    assert_eq!(
        hex(px(x, top / 2 + 3)),
        "22212C",
        "bg.hover under the pointer"
    );
    gui.post(WM_MOUSELEAVE, 0, 0);
    gui.wait("hover", |v| v["tab"].is_null());
    // The Menu key opens the pane menu, whose last item is the settings menu: every setting
    // of the menu is reachable by keyboard (Windows' menus are keyboard- and screen-reader-
    // accessible).
    let (ev, _) = gui.cmd_then(&json!({"cmd": "key", "vk": 0x5D}), "menu", |_| true);
    let mut items = Vec::new();
    menu_items(&ev["items"], "", &mut items);
    find_item(&items, "Settings/Motion/Reduced (no motion)");
    find_item(&items, "Settings/Window opacity/90 %");
}

#[test]
fn preferences_survive_restart_migrate_from_version_1_and_never_touch_running_terminals() {
    let host = Host::start("gui-persist", &["--no-shell"]);
    let ep = host.ep.clone();
    let (tid, pid) = new_terminal(&ep, "keep");
    let appdata = sandbox("persist");
    let dir = appdata.join("mouv").join("subak");
    // A gui.json written by Steps 16–19 (version 1, with the text size and theme).
    std::fs::write(
        dir.join("gui.json"),
        r#"{"schema":"subak.gui","schema_version":1,"font_size":16,"theme":"kim-dark",
            "window":null,"last_terminal":null,"hidden_terminals":[],"kept_key":true}"#,
    )
    .unwrap();
    let mut gui = Gui::start(&ep, Some("keep"), &appdata);
    let m = gui
        .logged("migrated")
        .expect("the version 1 file was migrated");
    assert_eq!(m["from"], 1);
    assert_eq!(m["to"], 2);
    gui.state("attached");
    let d = gui.dump();
    assert_eq!(d["font_size"], 16.0);
    assert_eq!(d["settings"]["theme"], "kim-dark");
    let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
    assert!(config.contains("font_size = 16") && config.contains("theme = \"kim-dark\""));
    let prefs: Value =
        serde_json::from_slice(&std::fs::read(dir.join("gui.json")).unwrap()).unwrap();
    validate("subak-gui-v2", &prefs);
    assert_eq!(
        prefs["kept_key"], true,
        "unknown keys survive the migration"
    );
    let backup = PathBuf::from(m["backup"].as_str().unwrap());
    let old = std::fs::read_to_string(&backup).unwrap();
    assert!(
        old.contains("\"schema_version\":1"),
        "the backup is the version 1 file"
    );
    // Preferences survive a restart: zoom and a theme chosen in the menu.
    gui.cmd(&json!({"cmd": "zoom", "step": 1}));
    let items = open_menu(&gui);
    let (light, _) = find_item(&items, "Settings/Theme/Kim light");
    gui.cmd(&json!({"cmd": "menu", "id": light}));
    let (op90, _) = find_item(&open_menu(&gui), "Settings/Window opacity/90 %");
    gui.cmd(&json!({"cmd": "menu", "id": op90}));
    assert_eq!(gui.close(), 0);
    let gui = Gui::start(&ep, None, &appdata);
    gui.state("attached");
    let d = gui.until("the saved opacity", |d| d["opacity_applied"] == 229);
    assert_eq!(d["font_size"], 17.0);
    assert_eq!(d["theme"], "kim-light");
    assert!(!gui.seen("\"event\":\"migrated\""), "migrated once");
    // Running Terminals are never touched by settings: the same process, environment and
    // state, whatever changes (the profile list included).
    let before = pane_of(&ep, "keep");
    set(&appdata, "appearance.theme", "kim-dark");
    set(&appdata, "terminal.copy_on_select", "true");
    std::fs::write(
        dir.join("config.toml"),
        std::fs::read_to_string(dir.join("config.toml")).unwrap()
            + "\n[[profiles]]\nname = \"later\"\nprogram = \"pwsh.exe\"\n",
    )
    .unwrap();
    gui.refocus();
    let after = pane_of(&ep, "keep");
    assert_eq!(pid_of(&after["pid"]), pid);
    assert_eq!(before["env"], after["env"]);
    assert_eq!(before["command"], after["command"]);
    let (code, doc) = sub(&ep, &["info", &tid]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["terminal"]["state"], "running");
    // A broken config.toml: the defaults, a notice naming it, and the file is never rewritten
    // (not even by the zoom keys, which then last for this window only).
    std::fs::write(dir.join("config.toml"), "[appearance\nfont_size = ").unwrap();
    let d = gui.refocus();
    assert_eq!(d["settings"]["state"], "invalid");
    assert!(
        d["notice"]
            .as_str()
            .unwrap_or("")
            .contains("not valid TOML"),
        "{d}"
    );
    assert_eq!(d["font_size"], 13.0);
    let (_, d) = gui.cmd_then(
        &json!({"cmd": "zoom", "step": 1}),
        "settings_not_saved",
        |_| true,
    );
    assert_eq!(d["font_size"], 14.0);
    assert_eq!(
        std::fs::read_to_string(dir.join("config.toml")).unwrap(),
        "[appearance\nfont_size = ",
        "never rewritten"
    );
    assert_eq!(pid_of(&pane_of(&ep, "keep")["pid"]), pid);
}
