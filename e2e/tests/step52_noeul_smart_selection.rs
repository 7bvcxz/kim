//! E2E — Step 52: Noeul's smart selection and pixel precision (NOEUL_UX_SPEC §4.8,
//! docs/adr/0035-noeul-smart-selection-and-precision.md).
//! Requirements: NOEUL-FR-040…043, NOEUL-FR-050, NOEUL-SEC-004, NOEUL-SEC-007, NOEUL-SEC-012.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the
//! precision keys headlessly — `selection.mark` (`Space`), `pointer.move-*` (`code:KeyW` …),
//! `detect.cycle` (`E`), `magnifier.toggle` (`M`), `history.previous`/`history.next` (`Comma`,
//! `Period`) in `hotkeys list` — and the two capture settings in `config show`; a dry run of
//! `capture.detection` writes nothing, an invalid value is refused, `capture.magnifier off` is
//! written and `config reset capture --yes` undoes it.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1` and
//! `NOEUL_DEV_DETECT_PIDS` naming this process): the scenario creates its **own** off-screen
//! windows inside the synthetic desktop — a plain one and one protected from capture
//! (`WDA_MONITOR`) — on a thread that pumps their messages. `capture --interactive` opens the
//! overlay; hovering the plain window highlights it, the protected one is reported and never
//! highlighted, a click selects the plain window and `Enter` pins exactly it. A second capture
//! steps back to that rectangle with `Comma`, the physical `W`/`D` keys move the pointer one
//! pixel, and `Space` … `Space` marks a rectangle that `Enter` pins.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window (everything lies around
//! -20000,-20000 and is never activated), no input synthesized (messages are posted to Noeul's
//! own overlay window), the real cursor never moves, the user's windows are never listed, and no
//! hotkey is registered at all. Every process and window started here is ended.
#![allow(unsafe_code)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
#[cfg(windows)]
const X0: i32 = -20_000;
#[cfg(windows)]
const Y0: i32 = -20_000;

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn valid(name: &str, v: &Value) {
    let (file, def) = name
        .split_once('#')
        .map_or((name, None), |(f, d)| (f, Some(d)));
    let mut s = schema(file);
    if let Some(d) = def {
        s = json!({"$ref": format!("#/$defs/{d}"), "$defs": s["$defs"].clone()});
    }
    let errors = jsonschema::Validator::new(&s).validate(v).unwrap();
    assert!(errors.is_empty(), "violates {name}: {errors:?}\n{v:#}");
}

/// The Win32 calls the scenario needs: its own target windows, finding Noeul's overlay window
/// and posting it the messages a mouse and keyboard would produce (never `SendInput`).
#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;
    type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

    #[repr(C)]
    struct WndClassExW {
        size: u32,
        style: u32,
        proc_: Option<WndProc>,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu: *const u16,
        class: *const u16,
        icon_small: *mut c_void,
    }

    #[repr(C)]
    #[derive(Default)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(cb: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>, lp: isize)
        -> i32;
        fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
        fn GetClassNameW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
        fn PostMessageW(h: Hwnd, msg: u32, wp: usize, lp: isize) -> i32;
        fn RegisterClassExW(wc: *const WndClassExW) -> u16;
        fn CreateWindowExW(
            ex: u32,
            class: *const u16,
            title: *const u16,
            style: u32,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            parent: Hwnd,
            menu: *mut c_void,
            instance: *mut c_void,
            param: *const c_void,
        ) -> Hwnd;
        fn DefWindowProcW(h: Hwnd, msg: u32, wp: usize, lp: isize) -> isize;
        fn DestroyWindow(h: Hwnd) -> i32;
        fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
        fn SetWindowDisplayAffinity(h: Hwnd, affinity: u32) -> i32;
        fn GetWindowRect(h: Hwnd, r: *mut Rect) -> i32;
        fn PeekMessageW(m: *mut [u64; 8], h: Hwnd, min: u32, max: u32, remove: u32) -> i32;
        fn TranslateMessage(m: *const [u64; 8]) -> i32;
        fn DispatchMessageW(m: *const [u64; 8]) -> isize;
        fn SetThreadDpiAwarenessContext(ctx: isize) -> isize;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const SW_SHOWNOACTIVATE: i32 = 4;
    const WDA_MONITOR: u32 = 1;
    const PM_REMOVE: u32 = 1;
    const PER_MONITOR_AWARE_V2: isize = -4;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One of this scenario's own windows, off-screen, shown without activation.
    pub fn own_window(x: i32, y: i32, w: i32, h: i32, protected: bool) -> Hwnd {
        let class = wide("NoeulE2eStep52Target");
        // SAFETY: plain values; the class name is copied by the system; registering twice
        // fails harmlessly.
        unsafe {
            SetThreadDpiAwarenessContext(PER_MONITOR_AWARE_V2);
            let wc = WndClassExW {
                size: u32::try_from(size_of::<WndClassExW>()).unwrap(),
                style: 0,
                proc_: Some(DefWindowProcW),
                cls_extra: 0,
                wnd_extra: 0,
                instance: GetModuleHandleW(std::ptr::null()),
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu: std::ptr::null(),
                class: class.as_ptr(),
                icon_small: std::ptr::null_mut(),
            };
            RegisterClassExW(&raw const wc);
            let title = wide("NOEUL-E2E-SECRET");
            let h = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            assert!(!h.is_null());
            ShowWindow(h, SW_SHOWNOACTIVATE);
            if protected {
                assert_ne!(SetWindowDisplayAffinity(h, WDA_MONITOR), 0);
            }
            h
        }
    }

    pub fn rect(h: Hwnd) -> (i32, i32, i32, i32) {
        let mut r = Rect::default();
        // SAFETY: a window of this scenario; `r` is writable.
        unsafe { GetWindowRect(h, &raw mut r) };
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    }

    pub fn pump() {
        let mut m = [0u64; 8];
        // SAFETY: an 8×8-byte buffer is larger than MSG; this thread's messages are dispatched.
        unsafe {
            while PeekMessageW(&raw mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&raw const m);
                DispatchMessageW(&raw const m);
            }
        }
    }

    pub fn destroy(h: Hwnd) {
        // SAFETY: our own window, on the thread that created it.
        unsafe { DestroyWindow(h) };
    }

    /// The top-level windows of `pid` with window class `class`.
    pub fn windows(pid: u32, class: &str) -> Vec<Hwnd> {
        unsafe extern "system" fn each(h: Hwnd, data: isize) -> i32 {
            // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
            unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
            1
        }
        let mut all: Vec<isize> = Vec::new();
        // SAFETY: the callback only touches `all`, passed by pointer for the duration.
        unsafe { EnumWindows(Some(each), (&raw mut all) as isize) };
        all.into_iter()
            .map(|h| h as Hwnd)
            .filter(|&h| {
                let mut owner = 0u32;
                let mut name = [0u16; 64];
                // SAFETY: plain queries; the class name is read only for Noeul's own windows.
                unsafe {
                    GetWindowThreadProcessId(h, &raw mut owner);
                    if owner != pid {
                        return false;
                    }
                    let n = GetClassNameW(h, name.as_mut_ptr(), 64);
                    String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]) == class
                }
            })
            .collect()
    }

    /// Posts a pointer-free message to a window of the instance this scenario started.
    pub fn post(h: Hwnd, msg: u32, wp: usize, lp: isize) -> bool {
        // SAFETY: a pointer-free message to a window this scenario's own child process owns.
        unsafe { PostMessageW(h, msg, wp, lp) != 0 }
    }
}

struct Noeul {
    sb: TempDir,
    exe: PathBuf,
    endpoint: String,
    synthetic: bool,
}

impl Noeul {
    /// A profile whose global actions are disabled before anything starts (nothing is ever
    /// registered); the overlay keys keep their defaults.
    fn new(tag: &str, exe: PathBuf, synthetic: bool) -> Noeul {
        let sb = TempDir::new(tag);
        let cfg = sb.path().join("Roaming").join("mouv").join("noeul");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(sb.path().join("Local")).unwrap();
        std::fs::create_dir_all(sb.path().join("Temp")).unwrap();
        std::fs::write(
            cfg.join("hotkeys.json"),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.region", "disabled": true},
                    {"action": "paste.float", "disabled": true}]}))
            .unwrap(),
        )
        .unwrap();
        Noeul {
            endpoint: format!("noeul-e2e-{tag}-{}", std::process::id()),
            sb,
            exe,
            synthetic,
        }
    }

    fn raw(&self, args: &[&str]) -> std::process::Output {
        let p = self.sb.path();
        let mut c = Command::new(&self.exe);
        c.args(args)
            .args(["--json", "--endpoint", &self.endpoint])
            .env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env("TEMP", p.join("Temp"))
            .env("TMP", p.join("Temp"))
            .env("KIM_NO_INPUT", "1")
            .env_remove("KIM_NO_HOTKEYS")
            .env_remove("NOEUL_DEV_CLIPBOARD")
            .stdin(Stdio::null());
        if self.synthetic {
            c.env("NOEUL_DEV_SYNTHETIC_DESKTOP", "1")
                .env("NOEUL_DEV_DETECT_PIDS", std::process::id().to_string());
        } else {
            c.env_remove("NOEUL_DEV_SYNTHETIC_DESKTOP")
                .env_remove("NOEUL_DEV_DETECT_PIDS");
        }
        c.output().unwrap()
    }

    /// `noeul <args> --json`, validated against cli-envelope-v1; the exit code matches.
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let out = self.raw(args);
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        valid("cli-envelope-v1", &doc);
        let code = out.status.code().unwrap_or(-1);
        let want = if doc["ok"] == true {
            0
        } else {
            doc["error"]["exit_code"].as_i64().unwrap()
        };
        assert_eq!(i64::from(code), want, "{args:?}: {doc:#}");
        (code, doc)
    }

    #[cfg(windows)]
    fn status(&self) -> Value {
        let (code, st) = self.json(&["status"]);
        assert_eq!(code, 0, "{st:#}");
        valid("noeul-ipc-v1#status_result", &st["data"]);
        st["data"].clone()
    }

    /// Waits for `f`, pumping the scenario's own windows meanwhile (their thread is this one).
    #[cfg(windows)]
    fn eventually(&self, what: &str, mut f: impl FnMut(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
            win::pump();
            let st = self.status();
            if f(&st) {
                return st;
            }
            assert!(
                Instant::now() < until,
                "timed out waiting for: {what}\n{st:#}"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

impl Drop for Noeul {
    fn drop(&mut self) {
        let out = self.raw(&["status"]);
        let pid = serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .and_then(|v| v["data"]["instance"]["pid"].as_u64());
        let _ = self.raw(&["quit", "--yes"]);
        std::thread::sleep(Duration::from_millis(500));
        // Last resort, only for a pid that is still this scenario's noeul.exe.
        if let Some(pid) = pid {
            let _ = Command::new("taskkill")
                .args([
                    "/F",
                    "/FI",
                    &format!("PID eq {pid}"),
                    "/FI",
                    "IMAGENAME eq noeul.exe",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_the_precision_keys_and_settings_are_discoverable_headlessly() {
    let n = Noeul::new("s52a", bin("noeul"), false);
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let keys = |id: &str| -> Vec<String> {
        list["data"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed"))["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["keys"].as_str().unwrap().to_string())
            .collect()
    };
    for (id, want) in [
        ("selection.mark", "Space"),
        ("pointer.move-up", "code:KeyW"),
        ("pointer.move-left", "code:KeyA"),
        ("pointer.move-down", "code:KeyS"),
        ("pointer.move-right", "code:KeyD"),
        ("detect.cycle", "E"),
        ("magnifier.toggle", "M"),
        ("history.previous", "Comma"),
        ("history.next", "Period"),
    ] {
        assert_eq!(keys(id), [want], "{id}");
    }
    let (_, show) = n.json(&["config", "show"]);
    assert_eq!(show["data"]["settings"]["detection"], "window");
    assert_eq!(show["data"]["settings"]["magnifier"], "on");
    let config = n.sb.path().join("Roaming/mouv/noeul/config.toml");
    let (code, dry) = n.json(&["config", "set", "capture.detection", "element", "--dry-run"]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
    assert!(!config.exists(), "a dry run writes nothing");
    let (code, doc) = n.json(&["config", "set", "capture.detection", "text"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, off) = n.json(&["config", "set", "capture.magnifier", "off"]);
    assert_eq!(code, 0, "{off:#}");
    let (_, show) = n.json(&["config", "show"]);
    assert_eq!(show["data"]["settings"]["magnifier"], "off");
    let (code, reset) = n.json(&["config", "reset", "capture", "--yes"]);
    assert_eq!(code, 0, "{reset:#}");
    let (_, show) = n.json(&["config", "show"]);
    assert_eq!(show["data"]["settings"]["magnifier"], "on");
}

#[cfg(windows)]
fn lp(x: i32, y: i32) -> isize {
    (((y - Y0) as u16 as u32) << 16 | ((x - X0) as u16 as u32)) as isize
}

#[cfg(windows)]
#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_detect_select_pin_history_and_keyboard_marking_on_the_synthetic_desktop() {
    use win::*;
    // This scenario's own targets, inside the synthetic desktop (never visible).
    let plain = own_window(X0 + 100, Y0 + 80, 300, 200, false);
    let protected = own_window(X0 + 700, Y0 + 50, 200, 150, true);
    pump();
    let (px, py, pw, ph) = rect(plain);
    let plain_rect = json!({"x": px, "y": py, "width": pw, "height": ph});
    let n = Noeul::new("s52b", kim_e2e::bin_debug("noeul"), true);
    let (code, start) = n.json(&["start", "--no-tray"]);
    assert_eq!(code, 0, "{start:#}");
    let pid = u32::try_from(start["data"]["pid"].as_u64().unwrap()).unwrap();
    let st = n.status();
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the screen is never read"
    );
    assert!(
        st["hotkeys"]["registrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let open = || -> Hwnd {
        let (code, doc) = n.json(&["capture", "--interactive"]);
        assert_eq!((code, doc["data"]["opened"].as_bool()), (0, Some(true)));
        windows(pid, "NoeulOverlay")[0]
    };
    let key = |ov: Hwnd, vk: usize, scancode: isize| {
        assert!(post(ov, WM_KEYDOWN, vk, (scancode << 16) | 1));
    };
    let ov = open();
    // Hover: the plain window is highlighted; the protected one is reported, not highlighted.
    assert!(post(ov, WM_MOUSEMOVE, 0, lp(px + 200, py + 150)));
    n.eventually("the plain window is detected", |st| {
        st["captures"]["overlay"]["detection"]["target"]
            == json!({"kind": "window", "rect": plain_rect})
    });
    assert!(post(ov, WM_MOUSEMOVE, 0, lp(X0 + 750, Y0 + 100)));
    n.eventually("the protected window is reported", |st| {
        let d = &st["captures"]["overlay"]["detection"];
        d["protected"] == true && d["target"].is_null()
    });
    // Click selects the plain window; Enter pins exactly it.
    for (msg, wp) in [(WM_MOUSEMOVE, 0), (WM_LBUTTONDOWN, 1), (WM_LBUTTONUP, 0)] {
        assert!(post(ov, msg, wp, lp(px + 200, py + 150)));
    }
    n.eventually("selected", |st| {
        st["captures"]["overlay"]["selection"] == plain_rect
    });
    key(ov, 0x0D, 0x1C);
    n.eventually("pinned", |st| st["captures"]["overlay"].is_null());
    let (_, pins) = n.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][0]["rect"], plain_rect);
    // History, pointer keys and keyboard marking in a second capture.
    let ov = open();
    key(ov, 0xBC, 0x33); // Comma
    n.eventually("history", |st| {
        st["captures"]["overlay"]["selection"] == plain_rect
    });
    assert!(post(ov, WM_MOUSEMOVE, 0, lp(X0 + 30, Y0 + 30)));
    key(ov, 0x20, 0x39); // Space: the first corner
    key(ov, 0x44, 0x20); // D
    key(ov, 0x44, 0x20); // D
    key(ov, 0x53, 0x1F); // S
    key(ov, 0x20, 0x39); // Space: the opposite corner
    let want = json!({"x": X0 + 30, "y": Y0 + 30, "width": 3, "height": 2});
    n.eventually("the marked rectangle", |st| {
        st["captures"]["overlay"]["selection"] == want
    });
    key(ov, 0x0D, 0x1C);
    n.eventually("pinned again", |st| st["captures"]["overlay"].is_null());
    let (_, pins) = n.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][1]["rect"], want);
    assert!(
        !format!("{}{pins}", n.status()).contains("NOEUL-E2E-SECRET"),
        "no window text"
    );
    drop(n);
    destroy(plain);
    destroy(protected);
}
