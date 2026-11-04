//! The colour picker through the real `noeul` binary (Step 59, ADR 0053; NOEUL-FR-044,
//! NOEUL-FR-071, NOEUL-FR-092, NOEUL-NFR-013, NOEUL-SEC-002, NOEUL-SEC-008; NOEUL_UX_SPEC §4.14).
//!
//! Safety on this shared machine, as for the other overlay tests: an isolated profile on a
//! private endpoint, `--no-tray`, both global actions disabled before the instance starts, and the
//! debug-build test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the overlay shows the synthetic picture
//! far off-screen (around -20000,-20000), is never topmost and never takes focus, so the picker
//! only ever reads **known synthetic pixels**, never the user's screen. Copies go to the hook's
//! stand-in clipboard, a folder of this sandbox (`NOEUL_DEV_CLIPBOARD_DIR`), and `noeul pin
//! --clipboard` (the command-line twin of `F3`) reads it back: **the user's clipboard is never
//! touched**. The overlay is driven only with messages posted to Noeul's own window (no
//! synthesized input, the real cursor never moves), and every process started is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use noeul::capture::Synthetic;
use serde_json::{Value, json};
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, PostMessageW,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`): 640×400 at
/// 100 %, and a 480×300 monitor at 150 % to its right (a gap below it).
const X0: i32 = -20_000;
const Y0: i32 = -20_000;

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
    // SAFETY: the callback only touches `all`; only windows of the instance this test started
    // are looked at further.
    unsafe { EnumWindows(Some(each), (&raw mut all) as LPARAM) };
    all.into_iter()
        .map(|h| h as HWND)
        .find(|&h| {
            let mut owner = 0u32;
            // SAFETY: plain queries on windows of the test's own child.
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

/// A key press with its scan code.
fn key(ov: HWND, vk: usize, scancode: isize) {
    post(ov, WM_KEYDOWN, vk, (scancode << 16) | 1);
}

const C: (usize, isize) = (0x43, 0x2E);
const K: (usize, isize) = (0x4B, 0x25);
const ESC: (usize, isize) = (0x1B, 0x01);

fn open(sb: &Sandbox, pid: u32) -> HWND {
    let (code, doc) = sb.json(&["capture", "--interactive"]);
    assert_eq!(
        (code, doc["data"]["opened"].as_bool()),
        (0, Some(true)),
        "{doc:#}"
    );
    overlay_window(pid)
}

/// Moves the overlay's pointer to `(x, y)` and waits until the magnifier shows it.
fn hover(sb: &Sandbox, ov: HWND, x: i32, y: i32) {
    post(ov, WM_MOUSEMOVE, 0, lp(x, y));
    eventually("the magnifier is over the point", || {
        let o = overlay(sb);
        o["pointer"] == json!({"x": x, "y": y}) && o["magnifier"]["shown"] == true
    });
}

/// The known synthetic pixel at `(x, y)` as `(r, g, b)`.
fn known(x: i32, y: i32) -> (u8, u8, u8) {
    let [b, g, r, _] = Synthetic::pixel(x, y);
    (r, g, b)
}

/// The text on the stand-in clipboard (what Noeul copied last).
fn clipboard_text(dir: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(dir.join("CF_UNICODETEXT")).ok()
}

/// `C`: waits until the capture ended with the colour copied, and returns the copied text.
fn copy_and_end(sb: &Sandbox, ov: HWND, clip: &std::path::Path) -> String {
    key(ov, C.0, C.1);
    eventually("the colour was copied and the capture ended", || {
        let s = status(sb);
        s["captures"]["overlay"].is_null()
            && s["last_action"] == json!({"action": "color.copy", "result": "copied"})
    });
    clipboard_text(clip).expect("the stand-in clipboard holds the colour")
}

/// `noeul pin --clipboard` (what `F3` does): the new pin's kind.
fn paste(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["pin", "--clipboard", "--at", "-19990,-19990"]);
    assert_eq!(code, 0, "{doc:#}");
    let e = validate("noeul-ipc-v1#pin_create_result", &doc["data"]);
    assert!(e.is_empty(), "{e:?}\n{doc:#}");
    doc["data"].clone()
}

/// The Name UI Automation gives `hwnd`, read from this (another) process as a screen reader
/// does: `IUIAutomation::ElementFromHandle` and `IUIAutomationElement::get_CurrentName` (vtable
/// slots 6 and 23 of UIAutomationClient.h).
fn uia_name(hwnd: HWND) -> Option<String> {
    use windows_sys::Win32::Foundation::{SysFreeString, SysStringLen};
    use windows_sys::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows_sys::core::{BSTR, GUID, HRESULT};
    type FnHwnd = unsafe extern "system" fn(*mut c_void, HWND, *mut *mut c_void) -> HRESULT;
    type FnName = unsafe extern "system" fn(*mut c_void, *mut BSTR) -> HRESULT;
    type FnRelease = unsafe extern "system" fn(*mut c_void) -> u32;
    let clsid = GUID::from_u128(0xff48dba4_60ef_4201_aa87_54103eef594e);
    let iid = GUID::from_u128(0x30cbe57d_d9d0_452a_ab13_7ac5ac4825ee);
    // SAFETY: COM initialised for this thread and balanced below; documented class and interface
    // ids; every vtable slot is called with its signature on a live object; each object and
    // string is released once.
    unsafe {
        let slot = |o: *mut c_void, i: usize| *(*(o as *const *const *const c_void)).add(i);
        if CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) < 0 {
            return None;
        }
        let mut auto: *mut c_void = std::ptr::null_mut();
        let mut name = None;
        if CoCreateInstance(
            &raw const clsid,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &raw const iid,
            &raw mut auto,
        ) >= 0
            && !auto.is_null()
        {
            let from = std::mem::transmute::<*const c_void, FnHwnd>(slot(auto, 6));
            let mut e: *mut c_void = std::ptr::null_mut();
            if from(auto, hwnd, &raw mut e) >= 0 && !e.is_null() {
                let get = std::mem::transmute::<*const c_void, FnName>(slot(e, 23));
                let mut b: BSTR = std::ptr::null();
                if get(e, &raw mut b) >= 0 && !b.is_null() {
                    let n = SysStringLen(b) as usize;
                    name = Some(String::from_utf16_lossy(std::slice::from_raw_parts(b, n)));
                    SysFreeString(b);
                }
                std::mem::transmute::<*const c_void, FnRelease>(slot(e, 2))(e);
            }
            std::mem::transmute::<*const c_void, FnRelease>(slot(auto, 2))(auto);
        }
        CoUninitialize();
        name
    }
}

/// Acceptance (issue #64): the on-screen picker returns the exact RGB/HEX of known pixels on both
/// monitors (100 % and 150 %) and in a gap, the format switch and the `[color]` settings apply,
/// every copied text pins back as a colour swatch through `noeul pin --clipboard` (F3's twin), a
/// selection is never lost to `C`, nothing reports the colour, the overlay's UI Automation Name
/// carries it, and the instance never takes the focus.
#[test]
fn the_picker_copies_known_pixels_and_they_pin_back_as_swatches() {
    let mut sb = Sandbox::synthetic("color", Keys::NoGlobals);
    // `Shift+C` cannot be posted without a real Shift key: the format switch is also bound to K
    // (the keys stay the user's, HOTKEY_CONTRACT §8). The globals stay disabled.
    std::fs::write(
        sb.hotkeys_file(),
        serde_json::to_vec_pretty(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
                {"action": "color.format-next", "bindings": ["Shift+C", "K"]}]}))
        .unwrap(),
    )
    .unwrap();
    let clip: PathBuf = sb.root.join("clipboard");
    std::fs::create_dir_all(&clip).unwrap();
    sb.env
        .push(("NOEUL_DEV_CLIPBOARD_DIR".into(), clip.display().to_string()));
    let pid = sb.instance();

    // HEX on the 100 % primary: exactly the known pixel; the capture ends and keeps nothing.
    let ov = open(&sb, pid);
    let (x, y) = (X0 + 123, Y0 + 45);
    hover(&sb, ov, x, y);
    let o = overlay(&sb);
    assert_eq!(
        o["color"],
        json!({"format": "hex", "hex_case": "upper", "alpha": false})
    );
    let (r, g, b) = known(x, y);
    let hex = format!("#{r:02X}{g:02X}{b:02X}");
    // The overlay's Name in the UI Automation tree carries what the magnifier shows.
    let name = uia_name(ov).expect("UI Automation names the overlay");
    assert_eq!(
        name,
        format!(
            "Capture overlay. Magnifier at {x}, {y}: {hex}, rgb({r}, {g}, {b}); copy format HEX"
        )
    );
    assert_eq!(copy_and_end(&sb, ov, &clip), hex);
    let s = status(&sb);
    assert_eq!(s["captures"]["colors"], 1);
    assert_eq!(s["captures"]["history"], 0);
    assert_eq!(s["memory"]["frames"], 0, "the frozen picture is freed");
    assert!(
        !s.to_string().contains(&hex[1..]),
        "status never names the colour"
    );
    let pinned = paste(&sb);
    assert_eq!(pinned["kind"], "color", "{pinned:#}");

    // RGB on the 150 % monitor (K switches the format; the readout marks it).
    let ov = open(&sb, pid);
    let (x, y) = (X0 + 640 + 100, Y0 + 50);
    hover(&sb, ov, x, y);
    key(ov, K.0, K.1);
    eventually("RGB chosen", || overlay(&sb)["color"]["format"] == "rgb");
    let (r, g, b) = known(x, y);
    let rgb = copy_and_end(&sb, ov, &clip);
    assert_eq!(rgb, format!("rgb({r}, {g}, {b})"));
    assert_eq!(paste(&sb)["kind"], "color");

    // The next capture keeps RGB; K again: HSL, which pins back as a swatch too.
    let ov = open(&sb, pid);
    assert_eq!(overlay(&sb)["color"]["format"], "rgb", "remembered");
    hover(&sb, ov, x, y);
    key(ov, K.0, K.1);
    eventually("HSL chosen", || overlay(&sb)["color"]["format"] == "hsl");
    let hsl = copy_and_end(&sb, ov, &clip);
    assert!(hsl.starts_with("hsl(") && hsl.ends_with("%)"), "{hsl}");
    assert_eq!(paste(&sb)["kind"], "color");

    // The settings: lowercase HEX with alpha, from the next capture (a running Noeul reloads).
    for (k, v) in [
        ("color.format", "hex"),
        ("color.hex_case", "lower"),
        ("color.alpha", "on"),
    ] {
        let (code, doc) = sb.json(&["config", "set", k, v]);
        assert_eq!(code, 0, "{doc:#}");
    }
    eventually("the instance reloaded the settings", || {
        status(&sb)["settings"]["color"]
            == json!({"format": "hex", "hex_case": "lower", "alpha": "on"})
    });
    let ov = open(&sb, pid);
    let (x, y) = (X0 + 7, Y0 + 399);
    hover(&sb, ov, x, y);
    let (r, g, b) = known(x, y);
    assert_eq!(
        copy_and_end(&sb, ov, &clip),
        format!("#{r:02x}{g:02x}{b:02x}ff")
    );
    // A gap of the virtual desktop (below the 150 % monitor) is opaque black.
    let ov = open(&sb, pid);
    hover(&sb, ov, X0 + 640 + 100, Y0 + 350);
    assert_eq!(copy_and_end(&sb, ov, &clip), "#000000ff");

    // With a selection, C copies but the capture stays with it; Esc then cancels.
    let ov = open(&sb, pid);
    post(ov, WM_MOUSEMOVE, 0, lp(X0 + 20, Y0 + 20));
    post(ov, WM_LBUTTONDOWN, 1, lp(X0 + 20, Y0 + 20));
    post(ov, WM_MOUSEMOVE, 1, lp(X0 + 120, Y0 + 90));
    post(ov, WM_LBUTTONUP, 0, lp(X0 + 120, Y0 + 90));
    let sel = json!({"x": X0 + 20, "y": Y0 + 20, "width": 101, "height": 71});
    eventually("selected", || overlay(&sb)["selection"] == sel);
    hover(&sb, ov, X0 + 300, Y0 + 300);
    key(ov, C.0, C.1);
    eventually("copied, the capture kept", || {
        status(&sb)["last_action"] == json!({"action": "color.copy", "result": "copied"})
    });
    let (r, g, b) = known(X0 + 300, Y0 + 300);
    assert_eq!(
        clipboard_text(&clip).unwrap(),
        format!("#{r:02x}{g:02x}{b:02x}ff")
    );
    let o = overlay(&sb);
    assert_eq!(o["selection"], sel, "the selection is kept");
    assert_eq!(status(&sb)["captures"]["colors"], 5);
    key(ov, ESC.0, ESC.1);
    eventually("cancelled", || overlay(&sb).is_null());

    // Nothing was ever pinned but the swatches, the instance never took the focus, and the
    // TEMP folder stayed empty.
    let (_, pins) = sb.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"].as_array().unwrap().len(), 3);
    for p in pins["data"]["pins"].as_array().unwrap() {
        assert_eq!(p["content"], "color");
    }
    // SAFETY: plain queries.
    let owner = unsafe {
        let mut p = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), &raw mut p);
        p
    };
    assert_ne!(owner, pid, "Noeul never took the focus");
    assert_eq!(std::fs::read_dir(sb.temp_dir()).unwrap().count(), 0);
    sb.stop_all();
}
