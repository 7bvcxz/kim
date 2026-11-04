//! The signature flow through the real `noeul` binary (Step 51; NOEUL-FR-020…026,
//! NOEUL-FR-030…032, NOEUL-NFR-012, NOEUL-SEC-005; NOEUL_UX_SPEC §4.3).
//!
//! Safety on this shared machine: every instance runs in an isolated profile on a private
//! endpoint with `--no-tray` and **no global shortcut** (both global actions are disabled before
//! it starts, so nothing is registered; the overlay's own keys stay on, some of them rebound to
//! plain letters because a posted key message cannot carry `Ctrl`). Every instance uses the
//! debug-build test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the overlay and the pins show a
//! synthetic picture far off-screen (around -20000,-20000), are never topmost and never take
//! focus; Copy goes to the hook's in-memory stand-in, never the user's clipboard; the selection's
//! menu is recorded, never shown; Save is answered with a file in the sandbox's quick-save
//! folder, never a dialog. The tests drive the overlay only with messages posted to Noeul's own
//! windows — no input is synthesized — and every process a test starts is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, HTCAPTION, PostMessageW, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_RBUTTONDOWN,
    WS_EX_NOACTIVATE, WS_EX_TOPMOST,
};

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
const X0: i32 = -20_000;
const Y0: i32 = -20_000;

fn assert_valid(name: &str, v: &Value) {
    let e = validate(name, v);
    assert!(e.is_empty(), "violates {name}: {e:?}\n{v:#}");
}

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    assert_valid("noeul-ipc-v1#status_result", &doc["data"]);
    doc["data"].clone()
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One top-level window of the instance.
#[derive(Debug, Clone)]
struct Win {
    hwnd: HWND,
    class: String,
    rect: (i32, i32, i32, i32),
    ex: u32,
}

fn windows_of(pid: u32) -> Vec<Win> {
    unsafe extern "system" fn each(h: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
        unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
        1
    }
    let mut all: Vec<isize> = Vec::new();
    // SAFETY: the callback only touches `all`, passed by pointer for the duration of the call.
    unsafe { EnumWindows(Some(each), (&raw mut all) as LPARAM) };
    all.into_iter()
        .filter_map(|h| {
            let hwnd = h as HWND;
            let mut owner = 0u32;
            // SAFETY: plain queries on a window handle (a stale one makes them fail harmlessly).
            unsafe {
                GetWindowThreadProcessId(hwnd, &raw mut owner);
                if owner != pid {
                    return None;
                }
                let mut name = [0u16; 64];
                let n = GetClassNameW(hwnd, name.as_mut_ptr(), 64);
                let mut r = RECT::default();
                GetWindowRect(hwnd, &raw mut r);
                Some(Win {
                    hwnd,
                    class: String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]),
                    rect: (r.left, r.top, r.right - r.left, r.bottom - r.top),
                    ex: GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                })
            }
        })
        .collect()
}

fn of_class(pid: u32, class: &str) -> Vec<Win> {
    windows_of(pid)
        .into_iter()
        .filter(|w| w.class == class)
        .collect()
}

/// Posts a pointer-free message to a window of the instance this test started; `false` when
/// the window is gone (the overlay closes on the second press of a double-click).
fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> bool {
    // SAFETY: a pointer-free message to a window this test's own child process owns.
    unsafe { PostMessageW(hwnd, msg, wp, lp) != 0 }
}

/// Client coordinates (the overlay's client (0,0) is the picture's top-left corner).
fn lp(x: i32, y: i32) -> isize {
    ((y as u16 as u32) << 16 | (x as u16 as u32)) as isize
}

/// The user's double-click time (the instance reads the same setting).
fn double_click_time() -> Duration {
    // SAFETY: a plain query.
    Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }))
}

/// A sandbox whose instances use the test hook, register no global hotkey, quick-save into
/// `<root>/shots`, and have extra overlay keys a posted message can carry: `K` copies, `J`
/// saves, `Q` quick-saves.
fn sandbox(tag: &str) -> Sandbox {
    let sb = Sandbox::synthetic(tag, Keys::NoGlobals);
    std::fs::write(
        sb.hotkeys_file(),
        serde_json::to_vec_pretty(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
                {"action": "capture.copy", "bindings": ["Ctrl+C", "K"]},
                {"action": "capture.save", "bindings": ["Ctrl+S", "J"]},
                {"action": "capture.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]}]}))
        .unwrap(),
    )
    .unwrap();
    // The guard of the common sandbox: nothing would be registered.
    let paths = noeul::paths::Paths::under(&sb.root);
    let (state, loaded) = noeul::hotkeys::load(&paths);
    assert!(!loaded.state.safe_mode(), "{:?}", loaded.problems);
    assert!(
        noeul::hotkeys::global_plan(&state).0.is_empty(),
        "no global hotkey is registered"
    );
    let shots = sb.root.join("shots").display().to_string();
    let edited = noeul::settings::set(
        &paths.config_file(),
        &[("capture.quick_save_folder", &shots)],
    )
    .unwrap();
    noeul::settings::write(&paths.config_file(), &edited.text).unwrap();
    sb
}

/// Opens the overlay with `capture --interactive` and returns its window (off-screen, not
/// topmost, never activated).
fn open_overlay(sb: &Sandbox, pid: u32) -> HWND {
    let (code, doc) = sb.json(&["capture", "--interactive"]);
    assert_eq!(
        (code, doc["data"]["opened"].as_bool()),
        (0, Some(true)),
        "{doc:#}"
    );
    let w = of_class(pid, "NoeulOverlay")
        .into_iter()
        .next()
        .expect("the overlay window");
    assert_eq!(w.rect.0, X0, "the synthetic desktop, far off-screen");
    assert_eq!(w.ex & WS_EX_TOPMOST, 0, "never on top under the test hook");
    assert_ne!(
        w.ex & WS_EX_NOACTIVATE,
        0,
        "never activated under the test hook"
    );
    w.hwnd
}

/// A drag from `a` to `b` (client coordinates); waits until the selection is confirmed.
fn select(sb: &Sandbox, ov: HWND, a: (i32, i32), b: (i32, i32)) -> Value {
    assert!(post(ov, WM_MOUSEMOVE, 0, lp(a.0, a.1)));
    assert!(post(ov, WM_LBUTTONDOWN, 1, lp(a.0, a.1)));
    assert!(post(
        ov,
        WM_MOUSEMOVE,
        1,
        lp((a.0 + b.0) / 2, (a.1 + b.1) / 2)
    ));
    assert!(post(ov, WM_MOUSEMOVE, 1, lp(b.0, b.1)));
    assert!(post(ov, WM_LBUTTONUP, 0, lp(b.0, b.1)));
    let want = json!({"x": X0 + a.0.min(b.0), "y": Y0 + a.1.min(b.1),
        "width": (b.0 - a.0).abs() + 1, "height": (b.1 - a.1).abs() + 1});
    eventually("the selection is confirmed", || {
        let st = status(sb);
        st["captures"]["overlay"]["phase"] == "selected"
            && st["captures"]["overlay"]["selection"] == want
    });
    want
}

/// A plain click (press and release) at a client point.
fn click(ov: HWND, x: i32, y: i32) -> bool {
    post(ov, WM_LBUTTONDOWN, 1, lp(x, y)) & post(ov, WM_LBUTTONUP, 0, lp(x, y))
}

/// Two quick clicks; the second release may find the overlay gone.
fn double_click(ov: HWND, x: i32, y: i32) {
    assert!(click(ov, x, y));
    assert!(post(ov, WM_LBUTTONDOWN, 1, lp(x, y)));
    let _ = post(ov, WM_LBUTTONUP, 0, lp(x, y));
}

fn key(ov: HWND, vk: usize) -> bool {
    post(ov, WM_KEYDOWN, vk, 0)
}

/// Waits until the instance has handled every message posted so far (a status round trip
/// after the posts) and the overlay is in `phase`, or closed (`None`).
fn settled(sb: &Sandbox, open: bool) -> Value {
    let mut st = Value::Null;
    eventually("the overlay state settles", || {
        st = status(sb);
        st["captures"]["open"] == open
    });
    st
}

fn pin_list(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["pin", "list"]);
    assert_eq!(code, 0, "{doc:#}");
    doc["data"].clone()
}

/// FLOW-01, 03, 05, 07, 28: a double-click inside pins at once; outside, on a handle and a slow
/// second click never pin; the rest of a quadruple click cannot touch the new pin.
#[test]
fn a_double_click_inside_pins_and_nothing_else_does() {
    let mut sb = sandbox("dbl-pin");
    let pid = sb.instance();
    let st = status(&sb);
    assert_eq!(st["capture_backend"], "synthetic");
    assert!(
        st["hotkeys"]["registrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let ov = open_overlay(&sb, pid);
    let sel = select(&sb, ov, (100, 100), (399, 299));
    let t = double_click_time();
    // Outside, both presses; then on the top-left handle; then two slow clicks.
    double_click(ov, 500, 350);
    double_click(ov, 100, 100);
    std::thread::sleep(t + Duration::from_millis(200));
    assert!(click(ov, 250, 200));
    std::thread::sleep(t + Duration::from_millis(200));
    assert!(click(ov, 250, 200));
    std::thread::sleep(t + Duration::from_millis(200));
    let st = settled(&sb, true);
    assert_eq!(st["pins"]["open"], 0, "nothing was pinned: {st:#}");
    assert_eq!(
        st["captures"]["overlay"]["selection"], sel,
        "the selection stays"
    );
    // The signature double-click: the pin appears, the overlay is gone.
    double_click(ov, 250, 200);
    let st = settled(&sb, false);
    assert_eq!(st["pins"]["open"], 1);
    assert_eq!(
        (
            st["captures"]["pinned"].as_u64(),
            st["captures"]["copied"].as_u64()
        ),
        (Some(1), Some(0))
    );
    assert_eq!(
        st["last_action"],
        json!({"action": "capture.pin", "result": "pinned"})
    );
    let l = pin_list(&sb);
    assert_eq!(l["pins"][0]["rect"], sel, "same place and size");
    assert_eq!(l["pins"][0]["zoom"], 100);
    let pins = of_class(pid, "NoeulPin");
    assert_eq!(pins.len(), 1);
    assert_eq!(
        pins[0].rect,
        (X0 + 100, Y0 + 100, 300, 200),
        "the window is the selection"
    );
    assert!(
        of_class(pid, "NoeulOverlay").is_empty(),
        "the overlay window is destroyed"
    );
    // The third and fourth presses land on the new pin within the guard: swallowed.
    let p = pins[0].hwnd;
    post(
        p,
        WM_NCLBUTTONDOWN,
        HTCAPTION as usize,
        lp(X0 + 250, Y0 + 200),
    );
    post(
        p,
        WM_NCLBUTTONUP,
        HTCAPTION as usize,
        lp(X0 + 250, Y0 + 200),
    );
    post(
        p,
        WM_NCLBUTTONDOWN,
        HTCAPTION as usize,
        lp(X0 + 250, Y0 + 200),
    );
    let st = status(&sb);
    assert_eq!(st["pins"]["open"], 1, "exactly one pin, not closed");
    assert_eq!(pin_list(&sb)["pins"][0]["rect"], sel, "not moved");
    assert_eq!(
        st["memory"]["bytes"],
        300 * 200 * 4,
        "only the pin's pixels are kept"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// FLOW-17, 19, 20, 08, 23: the toolbar's keyboard focus (Enter on Copy copies), the Copy
/// button, the quick-save and save keys into the sandbox, and the menu that is never shown.
#[test]
fn the_toolbar_copy_and_save_are_separate_actions_that_never_pin() {
    let mut sb = sandbox("dbl-outputs");
    let pid = sb.instance();
    let shots = sb.root.join("shots");
    // Tab, Tab, Enter: Copy has the focus, so Enter copies.
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(key(ov, 0x09));
    eventually("Pin has the focus", || {
        status(&sb)["captures"]["overlay"]["focus"] == "pin"
    });
    assert!(key(ov, 0x09));
    eventually("Copy has the focus", || {
        status(&sb)["captures"]["overlay"]["focus"] == "copy"
    });
    assert!(key(ov, 0x0D));
    let st = settled(&sb, false);
    assert_eq!(
        (
            st["captures"]["copied"].as_u64(),
            st["pins"]["open"].as_u64()
        ),
        (Some(1), Some(0)),
        "copied, not pinned"
    );
    // The Copy button: below the selection, the second button (since Step 56 the bar is wider
    // than the space left of the selection, so it starts at the monitor's left edge).
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(click(ov, 86, 324));
    let st = settled(&sb, false);
    assert_eq!(st["captures"]["copied"], 2);
    // Quick save (`Q` here, Ctrl+Shift+S by default): a new PNG of the frozen pixels.
    let ov = open_overlay(&sb, pid);
    let sel = select(&sb, ov, (100, 100), (399, 299));
    assert!(key(ov, 0x51));
    let st = settled(&sb, false);
    assert_eq!(
        st["last_action"],
        json!({"action": "capture.quick-save", "result": "saved"})
    );
    let files: Vec<_> = std::fs::read_dir(&shots)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.starts_with("noeul-")
            && name.ends_with(".png")
            && name.len() == "noeul-20261104-090507.png".len(),
        "{name}"
    );
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(&files[0]).unwrap()))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height), (300, 200));
    let p = noeul::capture::Synthetic::pixel(X0 + 100, Y0 + 100);
    assert_eq!(
        &buf[..3],
        &[p[2], p[1], p[0]],
        "the frozen picture's first pixel"
    );
    assert!(sel["width"] == 300);
    // Save (`J` here, Ctrl+S by default): the hook answers the dialog with the quick-save folder.
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(key(ov, 0x4A));
    let st = settled(&sb, false);
    assert_eq!(
        st["last_action"],
        json!({"action": "capture.save", "result": "saved"})
    );
    assert_eq!(
        std::fs::read_dir(&shots).unwrap().count(),
        2,
        "never overwritten"
    );
    // A right-click inside asks for the menu: recorded, never shown; Escape leaves.
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(post(ov, WM_RBUTTONDOWN, 2, lp(200, 200)));
    eventually("the menu request was recorded", || {
        status(&sb)["last_action"] == json!({"action": "capture.menu", "result": "not_shown"})
    });
    assert!(key(ov, 0x1B));
    let st = settled(&sb, false);
    assert_eq!(
        (
            st["captures"]["saved"].as_u64(),
            st["pins"]["open"].as_u64()
        ),
        (Some(2), Some(0))
    );
    assert!(
        std::fs::read_dir(sb.temp_dir()).unwrap().next().is_none(),
        "no temporary image files"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// FLOW-27: another program keeps the clipboard open (the hook's stand-in says so): Copy keeps
/// the overlay with a message, the double-click pins as always.
#[test]
fn a_held_clipboard_never_affects_pin() {
    let mut sb = sandbox("dbl-busy");
    let child = sb
        .cmd(&["run", "--no-tray"])
        .env("NOEUL_DEV_CLIPBOARD", "busy")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    sb.adopt(pid);
    sb.wait_healthy(pid);
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(key(ov, 0x4B)); // K: copy
    eventually("the copy was refused", || {
        status(&sb)["last_action"]
            == json!({"action": "capture.copy", "result": "clipboard_unavailable"})
    });
    assert_eq!(status(&sb)["captures"]["open"], true, "the work is kept");
    double_click(ov, 250, 200);
    let st = settled(&sb, false);
    assert_eq!(st["pins"]["open"], 1, "pinned regardless of the clipboard");
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    drop(child);
}

/// FLOW-24 and the preset through `noeul config`: a running instance follows the gesture
/// setting from the next capture; the Snipaste-style preset makes Enter copy; reset undoes it.
#[test]
fn the_gesture_setting_and_the_preset_reach_the_running_instance() {
    let mut sb = sandbox("dbl-settings");
    let pid = sb.instance();
    let (code, set) = sb.json(&["config", "set", "gestures.double_click", "copy"]);
    assert_eq!(code, 0, "{set:#}");
    assert_eq!(set["data"]["instance"], "reloaded");
    assert_eq!(status(&sb)["settings"]["gestures"]["double_click"], "copy");
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    double_click(ov, 250, 200);
    let st = settled(&sb, false);
    assert_eq!(
        (
            st["captures"]["copied"].as_u64(),
            st["pins"]["open"].as_u64()
        ),
        (Some(1), Some(0)),
        "the double-click copies"
    );
    // The Snipaste-style preset: shown by name, Enter copies; a dry run writes nothing.
    let (code, dry) = sb.json(&["config", "set", "gestures.preset", "snipaste", "--dry-run"]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
    assert_eq!(dry["data"]["hotkeys"]["written"], false);
    let (code, on) = sb.json(&["config", "set", "gestures.preset", "snipaste"]);
    assert_eq!(code, 0, "{on:#}");
    assert_eq!(on["data"]["hotkeys"]["written"], true);
    // Shown by name in the shortcut list, with the gestures.
    let human = String::from_utf8_lossy(&sb.run(&["hotkeys", "list"]).stdout).to_string();
    assert!(human.contains("Snipaste-style gestures: on"), "{human}");
    assert!(
        human.contains("Double-click inside a selection  capture-overlay  copy"),
        "{human}"
    );
    let human = String::from_utf8_lossy(&sb.run(&["config", "show"]).stdout).to_string();
    assert!(human.contains("Snipaste-style gestures (on)"), "{human}");
    let (_, list) = sb.json(&["hotkeys", "list"]);
    let keys_of = |list: &Value, id: &str| -> Vec<String> {
        list["data"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap()["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["keys"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(keys_of(&list, "capture.pin"), ["Ctrl+T"]);
    assert_eq!(keys_of(&list, "capture.copy"), ["Ctrl+C", "Enter"]);
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    assert!(key(ov, 0x0D));
    let st = settled(&sb, false);
    assert_eq!(st["captures"]["copied"], 2, "Enter copies with the preset");
    assert_eq!(st["settings"]["gestures"]["preset"], "snipaste");
    // Reset: Noeul's gestures and keys again.
    let (code, doc) = sb.json(&["config", "reset", "gestures"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, off) = sb.json(&["config", "reset", "gestures", "--yes"]);
    assert_eq!(code, 0, "{off:#}");
    let (_, list) = sb.json(&["hotkeys", "list"]);
    assert_eq!(keys_of(&list, "capture.pin"), ["Enter"]);
    let ov = open_overlay(&sb, pid);
    select(&sb, ov, (100, 100), (399, 299));
    double_click(ov, 250, 200);
    let st = settled(&sb, false);
    assert_eq!(st["pins"]["open"], 1, "the double-click pins again");
    assert_eq!(st["settings"]["gestures"]["preset"], "noeul");
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// FLOW-29, 31, 32: rapid captures each give an independent pin; selections across the seam
/// and on the 150 % monitor pin exact device pixels; a middle-click pins too.
#[test]
fn rapid_captures_give_independent_pins_with_exact_pixels() {
    let mut sb = sandbox("dbl-rapid");
    let pid = sb.instance();
    // (from, to, where to double-click): near the top of the monitor the size chip sits inside
    // the selection, and on the 150 % monitor the toolbar sits inside its bottom edge; both are
    // not interior, so the clicks avoid them.
    let rects = [
        ((10, 10), (59, 49), (34, 43)),
        ((70, 10), (119, 49), (94, 43)),
        // Across the seam (100 % | 150 %); on the small 150 % test monitor the toolbar (two rows
        // since Step 56) sits inside the selection's bottom edge from x 640, so the click is left
        // of it.
        ((600, 100), (699, 199), (620, 150)),
        ((700, 50), (999, 249), (720, 120)), // on the 150 % monitor
    ];
    for (i, (a, b, (cx, cy))) in rects.iter().enumerate() {
        let ov = open_overlay(&sb, pid);
        let sel = select(&sb, ov, *a, *b);
        double_click(ov, *cx, *cy);
        let st = settled(&sb, false);
        assert_eq!(st["pins"]["open"], i + 1, "capture {i}: {st:#}");
        let l = pin_list(&sb);
        assert_eq!(l["pins"][i]["rect"], sel, "capture {i}: no lost image");
        assert_eq!(l["pins"][i]["id"], format!("pin-{}", i + 1));
    }
    let ov = open_overlay(&sb, pid);
    let sel = select(&sb, ov, (200, 150), (299, 229));
    assert!(post(ov, WM_MBUTTONDOWN, 0x10, lp(250, 190)));
    let st = settled(&sb, false);
    assert_eq!(st["pins"]["open"], 5);
    assert_eq!(pin_list(&sb)["pins"][4]["rect"], sel);
    assert_eq!(of_class(pid, "NoeulPin").len(), 5);
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
