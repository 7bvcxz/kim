//! Pin transforms, click-through, thumbnails and groups through the real `noeul` binary (Step 54,
//! ADR 0044; NOEUL-FR-061, NOEUL-FR-062, NOEUL-FR-063, NOEUL-FR-064, NOEUL-NFR-001,
//! NOEUL-NFR-007, NOEUL-SEC-013).
//!
//! Safety on this shared machine: every instance runs in an isolated profile on a private
//! endpoint with `--no-tray` and no global shortcut, with the debug-build test hook
//! (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): pins are cut from a synthetic picture and lie far off-screen
//! (around -20000,-20000), are never topmost, can never be activated and never take the mouse
//! capture. Click-through is checked on these off-screen windows only (their extended style and
//! the system's hit-testing at a point no monitor shows). The tests drive the pins only through
//! the CLI, the IPC and messages posted to the instance's own windows; no input is synthesized,
//! no menu is shown, the clipboard is never touched. Every process a test starts is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, framed, validate};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, HTRIGHT, IsWindowVisible, PostMessageW, WM_KEYDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCLBUTTONDOWN, WS_EX_TRANSPARENT, WindowFromPoint,
};

/// The synthetic desktop's primary monitor (100 %) starts here; its second monitor (150 %) is
/// to its right, from X0 + 640.
const X0: i32 = -20_000;
const Y0: i32 = -20_000;

fn assert_valid(name: &str, v: &Value) {
    let e = validate(name, v);
    assert!(e.is_empty(), "violates {name}: {e:?}\n{v:#}");
}

fn ok(sb: &Sandbox, args: &[&str]) -> Value {
    let (code, doc) = sb.json(args);
    assert_eq!(code, 0, "{args:?}: {doc:#}");
    doc["data"].clone()
}

fn list(sb: &Sandbox) -> Value {
    let l = ok(sb, &["pin", "list"]);
    assert_valid("noeul-ipc-v1#pin_list_result", &l);
    l
}

fn pin<'a>(l: &'a Value, id: &str) -> &'a Value {
    l["pins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("no {id}: {l:#}"))
}

fn rect_tuple(r: &Value) -> (i32, i32, i32, i32) {
    let n = |k: &str| i32::try_from(r[k].as_i64().unwrap()).unwrap();
    (n("x"), n("y"), n("width"), n("height"))
}

#[derive(Debug, Clone, Copy)]
struct Win {
    hwnd: HWND,
    rect: (i32, i32, i32, i32),
    visible: bool,
    ex: u32,
}

/// The pin windows of `pid`.
fn pin_windows(pid: u32) -> Vec<Win> {
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
                let mut name = [0u16; 64];
                let n = GetClassNameW(hwnd, name.as_mut_ptr(), 64);
                let class = String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]);
                if owner != pid || class != "NoeulPin" {
                    return None;
                }
                let mut r = RECT::default();
                GetWindowRect(hwnd, &raw mut r);
                Some(Win {
                    hwnd,
                    rect: (r.left, r.top, r.right - r.left, r.bottom - r.top),
                    visible: IsWindowVisible(hwnd) != 0,
                    ex: GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                })
            }
        })
        .collect()
}

fn window_at(pid: u32, rect: (i32, i32, i32, i32), margin: i32) -> Win {
    let want = (
        rect.0 - margin,
        rect.1 - margin,
        rect.2 + 2 * margin,
        rect.3 + 2 * margin,
    );
    pin_windows(pid)
        .into_iter()
        .find(|w| w.rect == want)
        .unwrap_or_else(|| panic!("no pin window at {want:?}: {:?}", pin_windows(pid)))
}

fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    // SAFETY: posts a pointer-free message to a window of the instance this test started.
    assert_ne!(unsafe { PostMessageW(hwnd, msg, wp, lp) }, 0, "posted");
}

fn lp(x: i32, y: i32) -> isize {
    ((x as u16 as isize) & 0xFFFF) | ((y as u16 as isize) << 16)
}

/// Waits until `f` holds (the instance handles posted messages asynchronously).
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// User plus kernel CPU time of a process, in 100 ns units.
fn cpu_time(pid: u32) -> u64 {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a query handle on the instance this test started, closed below.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        assert!(!h.is_null(), "the instance can be queried");
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(h, &raw mut c, &raw mut e, &raw mut k, &raw mut u);
        CloseHandle(h);
        let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
        t(k) + t(u)
    }
}

/// Every transform through the CLI and the pin's own messages is reversible and touches only its
/// pin; the windows hold the outline and shadow around the exact picture, also at 150 %.
#[test]
fn transforms_through_the_cli_and_the_windows_messages_are_reversible() {
    let mut sb = Sandbox::synthetic("pins-transform", Keys::NoGlobals);
    let pid = sb.instance();
    let a = (X0 + 20, Y0 + 20, 100, 60);
    let b = (X0 + 300, Y0 + 20, 40, 40);
    let c = (X0 + 700, Y0 + 40, 60, 30); // on the 150 % monitor
    for (x, y, w, h) in [a, b, c] {
        ok(
            &sb,
            &["capture", "--region", &format!("{x},{y},{w},{h}"), "--pin"],
        );
    }
    let start = list(&sb);
    assert_eq!(
        pin(&start, "pin-1")["orientation"],
        json!({"rotation": 0, "mirrored": false})
    );
    assert_eq!(pin(&start, "pin-1")["opacity"], 100);
    assert_eq!(start["group"], "default");
    let w1 = window_at(pid, a, common::PIN_MARGIN);
    window_at(pid, b, common::PIN_MARGIN);
    // At 150 % the outline stays one pixel and the shadow is 18 pixels: a 19-pixel margin.
    window_at(pid, c, 19);

    // The CLI: a quarter turn right and half opacity, then back.
    let r = ok(
        &sb,
        &["pin", "set", "pin-1", "--rotate", "cw", "--opacity", "50"],
    );
    assert_valid("noeul-ipc-v1#pin_set_result", &r);
    assert_eq!(r["changed"], json!(["pin-1"]));
    let p = pin(&list(&sb), "pin-1").clone();
    assert_eq!(p["orientation"]["rotation"], 90);
    assert_eq!(p["opacity"], 50);
    let turned = rect_tuple(&p["rect"]);
    assert_eq!(
        (turned.2, turned.3),
        (60, 100),
        "a quarter turn swaps the sides"
    );
    assert_eq!(
        (turned.0 + 30, turned.1 + 50),
        (a.0 + 50, a.1 + 30),
        "around its centre"
    );
    eventually("the window turned", || {
        pin_windows(pid).iter().any(|w| w.rect == framed(turned))
    });
    ok(
        &sb,
        &["pin", "set", "pin-1", "--rotate", "ccw", "--opacity", "100"],
    );
    assert_eq!(
        rect_tuple(&pin(&list(&sb), "pin-1")["rect"]),
        a,
        "back in place"
    );

    // The pin's own keys: 3 flips, 3 again flips back; T makes a thumbnail, T again ends it.
    post(w1.hwnd, WM_KEYDOWN, 0x33, 0);
    eventually("flipped", || {
        pin(&list(&sb), "pin-1")["orientation"]["mirrored"] == true
    });
    post(w1.hwnd, WM_KEYDOWN, 0x33, 0);
    eventually("flipped back", || {
        pin(&list(&sb), "pin-1")["orientation"]["mirrored"] == false
    });
    post(w1.hwnd, WM_KEYDOWN, 0x54, 0);
    eventually("a thumbnail", || {
        pin(&list(&sb), "pin-1")["thumbnail"] == true
    });
    assert_eq!(
        rect_tuple(&pin(&list(&sb), "pin-1")["rect"]),
        (a.0, a.1, 96, 58),
        "the whole picture in 96 logical pixels at its corner"
    );
    post(w1.hwnd, WM_KEYDOWN, 0x54, 0);
    eventually("full size", || {
        pin(&list(&sb), "pin-1")["thumbnail"] == false
    });

    // Ctrl+wheel: towards the user makes it fainter (wheel delta -120 with MK_CONTROL).
    post(w1.hwnd, WM_MOUSEWHEEL, (0xFF88usize << 16) | 0x0008, 0);
    eventually("fainter", || pin(&list(&sb), "pin-1")["opacity"] == 90);
    post(w1.hwnd, WM_MOUSEWHEEL, (120usize << 16) | 0x0008, 0);
    eventually("opaque again", || {
        pin(&list(&sb), "pin-1")["opacity"] == 100
    });

    // The border: press the right outline, move 50 pixels right, release: 150 %, left side kept.
    let m = common::PIN_MARGIN;
    let (ox, oy) = (a.0 - m, a.1 - m);
    post(
        w1.hwnd,
        WM_NCLBUTTONDOWN,
        HTRIGHT as usize,
        lp(a.0 + 101, a.1 + 30),
    );
    post(w1.hwnd, WM_MOUSEMOVE, 0, lp(a.0 + 151 - ox, a.1 + 30 - oy));
    post(w1.hwnd, WM_LBUTTONUP, 0, lp(a.0 + 151 - ox, a.1 + 30 - oy));
    eventually("resized by its border", || {
        pin(&list(&sb), "pin-1")["zoom"] == 150
    });
    assert_eq!(
        rect_tuple(&pin(&list(&sb), "pin-1")["rect"]),
        (a.0, a.1, 150, 90)
    );
    ok(&sb, &["pin", "set", "pin-1", "--zoom", "100"]);

    // Precise placement, then back.
    ok(
        &sb,
        &[
            "pin",
            "set",
            "pin-2",
            "--at",
            &format!("{},{}", b.0 + 7, b.1 + 3),
        ],
    );
    assert_eq!(
        rect_tuple(&pin(&list(&sb), "pin-2")["rect"]),
        (b.0 + 7, b.1 + 3, 40, 40)
    );
    ok(
        &sb,
        &["pin", "set", "pin-2", "--at", &format!("{},{}", b.0, b.1)],
    );

    // Everything undone: the list is the one we started with.
    let mut now = list(&sb);
    let mut was = start;
    for l in [&mut now, &mut was] {
        for p in l["pins"].as_array_mut().unwrap() {
            p["created_at"] = Value::Null;
        }
    }
    assert_eq!(now, was, "every transform is reversible");
    // A refused value changes nothing and exits 2.
    let (code, doc) = sb.json(&["pin", "set", "pin-1", "--opacity", "5"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, doc) = sb.json(&["pin", "set", "pin-1"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// Click-through on the instance's own off-screen windows: the system hit-tests through a
/// click-through pin and stops when it is clickable again; the way back works from the CLI and
/// from a second start, and nothing ever took the foreground.
#[test]
fn click_through_lets_the_mouse_pass_and_always_comes_back() {
    let mut sb = Sandbox::synthetic("pins-click-through", Keys::NoGlobals);
    let pid = sb.instance();
    // A spot on the synthetic 150 % monitor that no other test of this file uses (their
    // instances run at the same time, at the same synthetic coordinates).
    let a = (X0 + 1010, Y0 + 230, 80, 50);
    ok(
        &sb,
        &[
            "capture",
            "--region",
            &format!("{},{},{},{}", a.0, a.1, a.2, a.3),
            "--pin",
        ],
    );
    let w = window_at(pid, a, 19);
    let probe = POINT {
        x: a.0 + 40,
        y: a.1 + 25,
    };
    // SAFETY: a plain query of the window under a point no monitor shows.
    let hit = || unsafe { WindowFromPoint(probe) };
    assert_eq!(hit(), w.hwnd, "the pin is hit");
    assert_eq!(w.ex & WS_EX_TRANSPARENT, 0);

    let r = ok(&sb, &["pin", "set", "pin-1", "--click-through", "on"]);
    assert_eq!(r["changed"], json!(["pin-1"]));
    eventually("the window lets clicks through", || {
        pin_windows(pid)[0].ex & WS_EX_TRANSPARENT != 0
    });
    assert_ne!(hit(), w.hwnd, "the mouse passes through the pin");
    let st = ok(&sb, &["status"]);
    assert_eq!(st["pins"]["click_through"], 1);
    // The way back from the CLI.
    ok(&sb, &["pin", "set", "--all", "--click-through", "off"]);
    eventually("clickable again", || {
        pin_windows(pid)[0].ex & WS_EX_TRANSPARENT == 0
    });
    assert_eq!(hit(), w.hwnd);
    // The way back from a second start (until the settings window of N5 exists).
    ok(&sb, &["pin", "set", "pin-1", "--click-through", "on"]);
    let again = ok(&sb, &["run", "--no-tray"]);
    assert_eq!(again["handed_over"], true);
    assert_eq!(again["activate"]["clickable"], json!(["pin-1"]));
    assert_eq!(pin(&list(&sb), "pin-1")["click_through"], false);
    eventually("clickable after the second start", || {
        pin_windows(pid)[0].ex & WS_EX_TRANSPARENT == 0
    });
    // The keyboard still reaches a pin's window after click-through (its `Ctrl+Shift+T` would
    // turn it off; a posted message carries no modifiers, so `T` stands in: the thumbnail).
    // Turning click-through on from the pin's own key without the tray or a `pins.clickable`
    // key is refused by the core (app::tests::click_through_always_has_a_way_back).
    post(w.hwnd, WM_KEYDOWN, 0x54, 0);
    eventually("the key reached the pin", || {
        pin(&list(&sb), "pin-1")["thumbnail"] == true
    });
    ok(&sb, &["pin", "set", "pin-1", "--thumbnail", "off"]);
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// Groups through the CLI: switch, list, hide/show/close by group (closing several needs
/// `--yes`).
#[test]
fn groups_switch_and_act_together() {
    let mut sb = Sandbox::synthetic("pins-groups", Keys::NoGlobals);
    let pid = sb.instance();
    for x in [10, 120, 230] {
        ok(
            &sb,
            &[
                "capture",
                "--region",
                &format!("{},{},50,40", X0 + x, Y0 + 60),
                "--pin",
            ],
        );
    }
    ok(&sb, &["pin", "set", "pin-3", "--move-to-group", "research"]);
    let g = ok(&sb, &["pin", "group", "list"]);
    assert_valid("noeul-ipc-v1#group_list_result", &g);
    assert_eq!(g["groups"].as_array().unwrap().len(), 2);
    let s = ok(&sb, &["pin", "group", "switch", "research", "--dry-run"]);
    assert_eq!(s["hidden"], json!(["pin-1", "pin-2"]));
    assert!(
        pin_windows(pid).iter().all(|w| w.visible),
        "a dry run hides nothing"
    );
    let s = ok(&sb, &["pin", "group", "switch", "research"]);
    assert_valid("noeul-ipc-v1#group_switch_result", &s);
    assert_eq!(pin_windows(pid).iter().filter(|w| w.visible).count(), 1);
    ok(
        &sb,
        &[
            "capture",
            "--region",
            &format!("{},{},20,20", X0 + 400, Y0 + 60),
            "--pin",
        ],
    );
    assert_eq!(
        pin(&list(&sb), "pin-4")["group"],
        "research",
        "new pins join the current group"
    );
    let r = ok(&sb, &["pin", "show", "--group", "default"]);
    assert_eq!(r["shown"], json!(["pin-1", "pin-2"]));
    let (code, doc) = sb.json(&["pin", "close", "--group", "research"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required")),
        "closing several pins at once asks"
    );
    let r = ok(&sb, &["pin", "close", "--group", "research", "--yes"]);
    assert_eq!(r["closed"], json!(["pin-3", "pin-4"]));
    assert_eq!(pin_windows(pid).len(), 2);
    let human = String::from_utf8_lossy(&sb.run(&["pin", "list"]).stdout).to_string();
    assert!(
        human.contains("GROUP") && human.contains("default"),
        "{human}"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// Hidden pins neither render nor wake up: with twelve pins hidden (some turned, faded,
/// thumbnails), the instance uses next to no CPU while it waits (NOEUL-NFR-001, NOEUL-NFR-007).
#[test]
fn hidden_pins_use_no_cpu() {
    let mut sb = Sandbox::synthetic("pins-hidden-cpu", Keys::NoGlobals);
    let pid = sb.instance();
    for i in 0..12 {
        let (x, y) = (X0 + 10 + (i % 6) * 100, Y0 + 10 + (i / 6) * 120);
        ok(
            &sb,
            &["capture", "--region", &format!("{x},{y},90,100"), "--pin"],
        );
    }
    ok(
        &sb,
        &["pin", "set", "--all", "--rotate", "cw", "--opacity", "40"],
    );
    ok(&sb, &["pin", "set", "pin-1", "--thumbnail", "on"]);
    ok(&sb, &["pin", "hide", "--all"]);
    assert!(pin_windows(pid).iter().all(|w| !w.visible));
    // Let the last requests settle, then measure while nobody asks anything.
    std::thread::sleep(Duration::from_millis(300));
    let before = cpu_time(pid);
    let t = Instant::now();
    std::thread::sleep(Duration::from_secs(3));
    let used = cpu_time(pid) - before;
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "twelve hidden pins: {:.2} ms CPU in {secs:.1} s",
        used as f64 / 10_000.0
    );
    assert!(
        used <= 150_000,
        "hidden pins wake nothing: {used} × 100 ns of CPU in {secs:.1} s"
    );
    let st = ok(&sb, &["status"]);
    assert_eq!(st["pins"]["hidden"], 12);
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
