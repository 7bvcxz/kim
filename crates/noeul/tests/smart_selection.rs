//! Smart selection and pixel precision through the real `noeul` binary (Step 52;
//! NOEUL-FR-040…043, NOEUL-FR-050, NOEUL-SEC-004, NOEUL-SEC-007; NOEUL_UX_SPEC §4.8).
//!
//! Safety on this shared machine, as for the other overlay tests: an isolated profile on a
//! private endpoint, `--no-tray`, no global shortcut (both global actions disabled before the
//! instance starts), and the debug-build test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the overlay
//! and the pins show a synthetic picture far off-screen (around -20000,-20000), are never topmost
//! and never take focus. The **detection targets are this test's own windows**, created
//! off-screen inside the synthetic desktop (a plain window with two buttons, a protected one, one
//! excluded from capture, a click-through layered one), never activated and destroyed at the end;
//! `NOEUL_DEV_DETECT_PIDS` names this process, so the instance lists only these windows and never
//! looks at the user's. The overlay is driven only with messages posted to Noeul's own window — no
//! input is synthesized and the real cursor never moves — and every process started is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, LWA_ALPHA, MSG, PM_REMOVE,
    PeekMessageW, PostMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSEXW, WS_CHILD, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
};

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
const X0: i32 = -20_000;
const Y0: i32 = -20_000;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn rect_of(h: HWND) -> Value {
    let mut r = RECT::default();
    // SAFETY: a window of this test; `r` is writable.
    unsafe { GetWindowRect(h, &raw mut r) };
    json!({"x": r.left, "y": r.top, "width": r.right - r.left, "height": r.bottom - r.top})
}

/// This test's own detection targets, on a thread that pumps their messages (UI Automation
/// asks them through window messages) until dropped; then they are destroyed.
struct Targets {
    plain: Value,
    button: Value,
    protected: Value,
    excluded: Value,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Targets {
    fn spawn() -> Targets {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            // SAFETY: this thread's DPI context, so rectangles are device pixels.
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let class = wide("NoeulSmartSelectionTarget");
            let wc = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap(),
                lpfnWndProc: Some(DefWindowProcW),
                // SAFETY: this executable's module handle.
                hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            // SAFETY: a fully initialized class description.
            unsafe { RegisterClassExW(&raw const wc) };
            let make = |x: i32, y: i32, w: i32, h: i32, ex: u32| {
                let title = wide("NOEUL-SECRET-TARGET");
                // SAFETY: our class; plain values; shown without activation.
                unsafe {
                    let hwnd = CreateWindowExW(
                        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | ex,
                        class.as_ptr(),
                        title.as_ptr(),
                        WS_POPUP,
                        X0 + x,
                        Y0 + y,
                        w,
                        h,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        std::ptr::null(),
                    );
                    assert!(!hwnd.is_null());
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    hwnd
                }
            };
            let plain = make(100, 80, 300, 200, 0);
            let button_class = wide("BUTTON");
            let mut buttons = Vec::new();
            for (i, y) in [20, 110].into_iter().enumerate() {
                let text = wide(&format!("NOEUL-SECRET-BUTTON-{i}"));
                // SAFETY: a standard child control of our window.
                let b = unsafe {
                    CreateWindowExW(
                        0,
                        button_class.as_ptr(),
                        text.as_ptr(),
                        WS_CHILD | WS_VISIBLE,
                        20,
                        y,
                        160,
                        60,
                        plain,
                        std::ptr::null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        std::ptr::null(),
                    )
                };
                assert!(!b.is_null());
                buttons.push(b);
            }
            let protected = make(700, 50, 200, 150, 0);
            let excluded = make(450, 250, 150, 100, 0);
            let through = make(20, 300, 100, 60, WS_EX_LAYERED | WS_EX_TRANSPARENT);
            // SAFETY: attributes of our own windows.
            unsafe {
                assert_ne!(SetWindowDisplayAffinity(protected, WDA_MONITOR), 0);
                assert_ne!(
                    SetWindowDisplayAffinity(excluded, WDA_EXCLUDEFROMCAPTURE),
                    0
                );
                SetLayeredWindowAttributes(through, 0, 255, LWA_ALPHA);
            }
            tx.send((
                rect_of(plain),
                rect_of(buttons[1]),
                rect_of(protected),
                rect_of(excluded),
            ))
            .unwrap();
            while !flag.load(Ordering::SeqCst) {
                // SAFETY: MSG is plain data; this thread's own messages are dispatched.
                unsafe {
                    let mut m: MSG = std::mem::zeroed();
                    while PeekMessageW(&raw mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                        TranslateMessage(&raw const m);
                        DispatchMessageW(&raw const m);
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            for h in buttons
                .into_iter()
                .chain([plain, protected, excluded, through])
            {
                // SAFETY: our own windows, destroyed once on their thread.
                unsafe { DestroyWindow(h) };
            }
        });
        let (plain, button, protected, excluded) =
            rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Targets {
            plain,
            button,
            protected,
            excluded,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Targets {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    let e = validate("noeul-ipc-v1#status_result", &doc["data"]);
    assert!(e.is_empty(), "{e:?}\n{doc:#}");
    doc["data"].clone()
}

fn overlay(sb: &Sandbox) -> Value {
    status(sb)["captures"]["overlay"].clone()
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The instance's overlay window (class `NoeulOverlay`).
fn overlay_window(pid: u32) -> HWND {
    unsafe extern "system" fn each(h: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
        unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
        1
    }
    let mut all: Vec<isize> = Vec::new();
    // SAFETY: the callback only touches `all`. Only windows of the instance this test started
    // are looked at further.
    unsafe { EnumWindows(Some(each), (&raw mut all) as LPARAM) };
    all.into_iter()
        .map(|h| h as HWND)
        .find(|&h| {
            let mut owner = 0u32;
            // SAFETY: plain queries; the class name is read only for the test's own child.
            unsafe {
                GetWindowThreadProcessId(h, &raw mut owner);
                if owner != pid {
                    return false;
                }
                let mut name = [0u16; 32];
                let n = GetClassNameW(h, name.as_mut_ptr(), 32);
                String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]) == "NoeulOverlay"
            }
        })
        .expect("the overlay window")
}

fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    // SAFETY: a pointer-free message to a window of the instance this test started.
    assert_ne!(unsafe { PostMessageW(hwnd, msg, wp, lp) }, 0);
}

/// Client coordinates of a screen point (the overlay's client (0,0) is the picture's corner).
fn lp(x: i32, y: i32) -> isize {
    (((y - Y0) as u16 as u32) << 16 | ((x - X0) as u16 as u32)) as isize
}

/// A key press with its scan code (for physical `code:` bindings).
fn key(ov: HWND, vk: usize, scancode: isize) {
    post(ov, WM_KEYDOWN, vk, (scancode << 16) | 1);
}

fn centre(r: &Value) -> (i32, i32) {
    let v = |k: &str| i32::try_from(r[k].as_i64().unwrap()).unwrap();
    (v("x") + v("width") / 2, v("y") + v("height") / 2)
}

fn open(sb: &Sandbox, pid: u32) -> HWND {
    let (code, doc) = sb.json(&["capture", "--interactive"]);
    assert_eq!(
        (code, doc["data"]["opened"].as_bool()),
        (0, Some(true)),
        "{doc:#}"
    );
    overlay_window(pid)
}

#[test]
fn detection_magnifier_pointer_keys_and_history_through_the_real_binary() {
    let targets = Targets::spawn();
    let mut sb = Sandbox::synthetic("smart", Keys::NoGlobals);
    sb.env.push((
        "NOEUL_DEV_DETECT_PIDS".into(),
        std::process::id().to_string(),
    ));
    let pid = sb.instance();
    let ov = open(&sb, pid);
    let o = overlay(&sb);
    assert_eq!(o["detection"]["mode"], "window");
    assert_eq!(
        o["detection"]["windows"], 2,
        "the plain and the protected window; the excluded and click-through ones are not offered"
    );
    // Hover the plain window: highlighted (J2), the magnifier shows.
    let (cx, cy) = centre(&targets.plain);
    post(ov, WM_MOUSEMOVE, 0, lp(cx + 100, cy + 50));
    eventually("the window is detected", || {
        let o = overlay(&sb);
        o["detection"]["target"] == json!({"kind": "window", "rect": targets.plain})
            && o["magnifier"]["shown"] == true
    });
    // The protected window: not highlighted, reported honestly.
    let (px, py) = centre(&targets.protected);
    post(ov, WM_MOUSEMOVE, 0, lp(px, py));
    eventually("the protected window is reported", || {
        let o = overlay(&sb);
        o["detection"]["protected"] == true && o["detection"]["target"].is_null()
    });
    // Excluded from capture: absent; nothing below it on the synthetic desktop.
    let (ex, ey) = centre(&targets.excluded);
    post(ov, WM_MOUSEMOVE, 0, lp(ex, ey));
    eventually("the excluded window is not detected", || {
        let o = overlay(&sb);
        o["pointer"] == json!({"x": ex, "y": ey})
            && o["detection"]["target"].is_null()
            && o["detection"]["protected"] == false
    });
    // A click on the plain window selects it; Enter pins it.
    post(ov, WM_MOUSEMOVE, 0, lp(cx + 100, cy + 50));
    post(ov, WM_LBUTTONDOWN, 1, lp(cx + 100, cy + 50));
    post(ov, WM_LBUTTONUP, 0, lp(cx + 100, cy + 50));
    eventually("the click selected the window", || {
        overlay(&sb)["selection"] == targets.plain
    });
    key(ov, 0x0D, 0x1C); // Enter
    eventually("pinned", || status(&sb)["captures"]["overlay"].is_null());
    let (_, pins) = sb.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][0]["rect"], targets.plain);
    assert_eq!(status(&sb)["captures"]["history"], 1);

    // Element detection (E) through UI Automation, against this test's own buttons.
    let ov = open(&sb, pid);
    key(ov, 0x45, 0x12); // E
    let (bx, by) = centre(&targets.button);
    post(ov, WM_MOUSEMOVE, 0, lp(bx, by));
    eventually("the button is detected as an element", || {
        let o = overlay(&sb);
        o["detection"]["mode"] == "element"
            && o["detection"]["target"] == json!({"kind": "element", "rect": targets.button})
    });
    // Physical W A S D move the pointer one pixel; detection pauses; the real cursor stays.
    key(ov, 0x57, 0x11); // W (at W's place)
    key(ov, 0x5A, 0x11); // Z at W's place (AZERTY): the same physical key
    key(ov, 0x44, 0x20); // D
    eventually("the pointer moved by the keys", || {
        let o = overlay(&sb);
        o["pointer"] == json!({"x": bx + 1, "y": by - 2}) && o["detection"]["suspended"] == true
    });
    // M hides the magnifier; Comma steps back to the pinned window's rectangle.
    key(ov, 0x4D, 0x32); // M
    key(ov, 0xBC, 0x33); // Comma
    eventually("history and magnifier", || {
        let o = overlay(&sb);
        o["magnifier"]["enabled"] == false
            && o["selection"] == targets.plain
            && o["history"] == json!({"count": 1, "position": 0})
    });
    // FLOW-18 by keyboard: Space marks a corner at the pointer, keys move, Space, Enter.
    post(ov, WM_MOUSEMOVE, 0, lp(X0 + 30, Y0 + 30));
    key(ov, 0x20, 0x39); // Space: a new first corner
    for _ in 0..4 {
        key(ov, 0x44, 0x20); // D
    }
    for _ in 0..3 {
        key(ov, 0x53, 0x1F); // S
    }
    key(ov, 0x20, 0x39); // Space: the opposite corner
    let want = json!({"x": X0 + 30, "y": Y0 + 30, "width": 5, "height": 4});
    eventually("the marked rectangle", || overlay(&sb)["selection"] == want);
    key(ov, 0x0D, 0x1C); // Enter
    eventually("pinned again", || {
        status(&sb)["captures"]["overlay"].is_null()
    });
    let (_, pins) = sb.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][1]["rect"], want);
    // Nothing of the targets' text reached the instance's output, and the instance never took
    // the keyboard focus (the user's program keeps it; under the hook the real cursor is never
    // warped, which the off-screen window test checks).
    let all = format!("{}{}", status(&sb), pins);
    assert!(!all.contains("NOEUL-SECRET"));
    // SAFETY: plain queries.
    let owner = unsafe {
        let mut p = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), &raw mut p);
        p
    };
    assert_ne!(owner, pid, "Noeul never took the focus");
    sb.stop_all();
    drop(targets);
}
