//! The floating pin engine through the real `noeul` binary (Step 53, ADR 0029; NOEUL-FR-060,
//! NOEUL-FR-061, NOEUL-FR-065, NOEUL-FR-067, NOEUL-FR-112, NOEUL-SEC-005, NOEUL-SEC-013).
//!
//! Safety on this shared machine: every instance runs in an isolated profile on a private
//! endpoint with `--no-tray` and no global shortcut (nothing is registered; the pins' own keys
//! work), with the debug-build
//! test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): pins are cut from a synthetic picture, lie far
//! off-screen (around -20000,-20000), are never topmost and can never be activated, and nothing
//! reads the screen or the clipboard. The tests drive the pins only through the CLI, the IPC and
//! messages posted to the instance's own windows; no input is synthesized and no menu is ever
//! shown (the hook records a menu request instead of opening it on the user's screen). Every
//! process a test starts is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, kill_pid, validate};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CONTEXTMENU, WM_DISPLAYCHANGE,
    WM_KEYDOWN, WM_MOUSEWHEEL, WM_NCMBUTTONUP, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
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

fn list(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["pin", "list"]);
    assert_eq!(code, 0, "{doc:#}");
    assert_valid("noeul-ipc-v1#pin_list_result", &doc["data"]);
    doc["data"].clone()
}

fn pin<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["pins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("no {id}: {list:#}"))
}

/// One top-level window of a process.
#[derive(Debug, Clone)]
struct Win {
    hwnd: HWND,
    class: String,
    rect: (i32, i32, i32, i32),
    visible: bool,
    ex: u32,
}

/// The top-level windows of `pid`, from the top of the z-order down.
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
                    visible: IsWindowVisible(hwnd) != 0,
                    ex: GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
                })
            }
        })
        .collect()
}

fn pin_windows(pid: u32) -> Vec<Win> {
    windows_of(pid)
        .into_iter()
        .filter(|w| w.class == "NoeulPin")
        .collect()
}

fn rect_tuple(r: &Value) -> (i32, i32, i32, i32) {
    let n = |k: &str| i32::try_from(r[k].as_i64().unwrap()).unwrap();
    (n("x"), n("y"), n("width"), n("height"))
}

fn foreground_pid() -> u32 {
    let mut pid = 0u32;
    // SAFETY: plain queries.
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &raw mut pid) };
    pid
}

/// Waits until `f` holds (the instance handles posted messages asynchronously).
fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    // SAFETY: posts a pointer-free message to a window of the instance this test started.
    assert_ne!(unsafe { PostMessageW(hwnd, msg, wp, lp) }, 0, "posted");
}

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.extend(files_under(&e.path()));
            } else {
                out.push(e.path().display().to_string());
            }
        }
    }
    out
}

/// Many independent pins: create, list, hide, show, move, zoom, close one while the others stay,
/// reopen, close all; the windows are what the list says, off-screen, never topmost under the
/// hook, never activated; nothing reaches the disk.
#[test]
fn many_pins_are_independent_windows_driven_by_cli_ipc_and_their_own_messages() {
    let mut sb = Sandbox::synthetic("pins-many", Keys::NoGlobals);
    let pid = sb.instance();
    let st = status(&sb);
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug-build test hook is on"
    );
    let primary = &st["desktop"]["monitors"][0];
    assert_eq!(
        primary["work"]["height"], 368,
        "the synthetic taskbar is reported"
    );
    assert!(
        primary["rect"]["x"].as_i64().unwrap() <= -10_000,
        "far off-screen"
    );

    // Three pins of three regions.
    let regions = [
        (X0 + 10, Y0 + 10, 100, 60),
        (X0 + 150, Y0 + 10, 80, 50),
        (X0 + 300, Y0 + 10, 120, 90),
    ];
    for (i, (x, y, w, h)) in regions.iter().enumerate() {
        let r = format!("{x},{y},{w},{h}");
        let (code, doc) = sb.json(&["capture", "--region", &r, "--pin"]);
        assert_eq!(code, 0, "{doc:#}");
        assert_eq!(doc["data"]["pin"]["id"], format!("pin-{}", i + 1));
        assert_eq!(doc["data"]["pin"]["zoom"], 100);
    }
    let l = list(&sb);
    assert_eq!(l["pins"].as_array().unwrap().len(), 3);
    for (i, (x, y, w, h)) in regions.iter().enumerate() {
        let p = pin(&l, &format!("pin-{}", i + 1));
        assert_eq!(
            rect_tuple(&p["rect"]),
            (*x, *y, *w, *h),
            "exact place and size"
        );
        assert_eq!(
            (
                p["zoom"].as_u64(),
                p["visible"].as_bool(),
                p["on_screen"].as_bool()
            ),
            (Some(100), Some(true), Some(true))
        );
    }
    let wins = pin_windows(pid);
    assert_eq!(wins.len(), 3, "one window per pin: {wins:?}");
    for w in &wins {
        assert!(w.visible);
        assert_ne!(
            w.ex & WS_EX_TOOLWINDOW,
            0,
            "no taskbar entry, not in Alt+Tab"
        );
        assert_ne!(w.ex & WS_EX_LAYERED, 0);
        assert_eq!(w.ex & WS_EX_TOPMOST, 0, "never on top under the test hook");
        assert_ne!(
            w.ex & WS_EX_NOACTIVATE,
            0,
            "never activated under the test hook"
        );
        assert!(
            w.rect.0 <= -10_000 && w.rect.1 <= -10_000,
            "off-screen: {w:?}"
        );
        assert!(
            regions
                .iter()
                .any(|r| common::framed((r.0, r.1, r.2, r.3)) == w.rect),
            "{w:?}"
        );
    }
    let by_rect = |rect: (i32, i32, i32, i32)| {
        pin_windows(pid)
            .into_iter()
            .find(|w| w.rect == common::framed(rect))
            .unwrap_or_else(|| panic!("no window at {rect:?}"))
    };
    let w1 = by_rect((regions[0].0, regions[0].1, regions[0].2, regions[0].3)).hwnd;
    let w2 = by_rect((regions[1].0, regions[1].1, regions[1].2, regions[1].3)).hwnd;

    // Hide one: only its window disappears; a dry run hides nothing.
    let (code, dry) = sb.json(&["pin", "hide", "pin-2", "--dry-run"]);
    assert_eq!((code, dry["data"]["hidden"].clone()), (0, json!(["pin-2"])));
    assert!(pin_windows(pid).iter().all(|w| w.visible));
    let (code, h) = sb.json(&["pin", "hide", "pin-2"]);
    assert_eq!(code, 0, "{h:#}");
    assert_valid("noeul-ipc-v1#pin_hide_result", &h["data"]);
    assert_eq!(pin(&list(&sb), "pin-2")["visible"], false);
    let wins = pin_windows(pid);
    assert_eq!(wins.iter().filter(|w| w.visible).count(), 2);
    assert!(!wins.iter().find(|w| w.hwnd == w2).unwrap().visible);
    assert_eq!(status(&sb)["pins"]["hidden"], 1);
    let (code, s) = sb.json(&["pin", "show", "--all"]);
    assert_eq!((code, s["data"]["shown"].clone()), (0, json!(["pin-2"])));
    assert!(pin_windows(pid).iter().all(|w| w.visible));

    // Move and zoom pin-1 through its own window's key and mouse messages.
    post(w1, WM_KEYDOWN, 0x25, 0); // Left: pin.move-left
    eventually("pin-1 moved left", || {
        rect_tuple(&pin(&list(&sb), "pin-1")["rect"]).0 == regions[0].0 - 1
    });
    post(w1, WM_KEYDOWN, 0xBB, 0); // Plus: pin.zoom-in (anchor: its corner, the pointer is far)
    eventually("pin-1 zoomed in", || {
        pin(&list(&sb), "pin-1")["zoom"] == 110
    });
    let p1 = pin(&list(&sb), "pin-1").clone();
    assert_eq!(
        rect_tuple(&p1["rect"]),
        (regions[0].0 - 1, regions[0].1, 110, 66),
        "the window grew from its corner"
    );
    assert_eq!(
        p1["image"],
        json!({"width": 100, "height": 60}),
        "the picture is untouched"
    );
    eventually("the window took the new size", || {
        pin_windows(pid).iter().any(|w| {
            w.hwnd == w1 && w.rect == common::framed((regions[0].0 - 1, regions[0].1, 110, 66))
        })
    });
    // One notch towards the user zooms out (the pointer is not over the pin: around its corner).
    post(w1, WM_MOUSEWHEEL, 0xFF88usize << 16, 0);
    eventually("pin-1 zoomed out", || {
        pin(&list(&sb), "pin-1")["zoom"] == 100
    });
    post(w1, WM_KEYDOWN, 0xBB, 0);
    eventually("pin-1 zoomed in again", || {
        pin(&list(&sb), "pin-1")["zoom"] == 110
    });
    post(w1, WM_NCMBUTTONUP, 2, 0); // middle-click: back to 100 %
    eventually("pin-1 back to 100 %", || {
        pin(&list(&sb), "pin-1")["zoom"] == 100
    });
    // The menu request reaches the core but is never shown under the hook.
    post(w1, WM_CONTEXTMENU, w1 as usize, -1);
    eventually("the menu request was recorded", || {
        status(&sb)["last_action"] == json!({"action": "pin.menu", "result": "not_shown"})
    });
    // The other pins did not move.
    let l = list(&sb);
    for (i, r) in regions.iter().enumerate().skip(1) {
        assert_eq!(rect_tuple(&pin(&l, &format!("pin-{}", i + 1))["rect"]), *r);
    }

    // A display change reaches the core; the synthetic layout did not change, nothing moves.
    let control = windows_of(pid)
        .into_iter()
        .find(|w| w.class == "NoeulControl")
        .expect("the control window");
    post(control.hwnd, WM_DISPLAYCHANGE, 32, 0);
    eventually("the display change was handled", || {
        status(&sb)["last_action"] == json!({"action": "display.change", "result": "ok"})
    });
    assert_eq!(status(&sb)["pins"]["off_screen"], 0);
    let (code, s) = sb.json(&["pin", "show", "--all", "--on-screen"]);
    assert_eq!(code, 0, "{s:#}");
    assert_valid("noeul-ipc-v1#pin_show_result", &s["data"]);
    assert_eq!(s["data"]["moved"], json!([]), "nothing was lost");

    // Close one: the others stay, exactly as they were.
    let (code, c) = sb.json(&["pin", "close", "pin-2", "--yes"]);
    assert_eq!((code, c["data"]["closed"].clone()), (0, json!(["pin-2"])));
    let wins = pin_windows(pid);
    assert_eq!(wins.len(), 2);
    assert!(wins.iter().all(|w| w.hwnd != w2 && w.visible));
    let l = list(&sb);
    assert_eq!(l["reopenable"], "pin-2");
    assert_eq!(rect_tuple(&pin(&l, "pin-3")["rect"]), regions[2]);
    // Reopen brings it back with its ID and place.
    let (code, re) = sb.json(&["pin", "reopen"]);
    assert_eq!((code, re["data"]["pin"].as_str()), (0, Some("pin-2")));
    assert_eq!(rect_tuple(&pin(&list(&sb), "pin-2")["rect"]), regions[1]);
    // Memory: exactly the three pictures; nothing else is held.
    let bytes: i64 = regions.iter().map(|r| i64::from(r.2 * r.3 * 4)).sum();
    assert_eq!(status(&sb)["memory"]["bytes"], bytes);

    // No pin ever became the foreground window.
    assert_ne!(
        foreground_pid(),
        pid,
        "the instance never took the foreground"
    );

    // Close all: no window, no picture (one kept for reopen), nothing on disk.
    let (code, c) = sb.json(&["pin", "close", "--all", "--yes"]);
    assert_eq!(code, 0, "{c:#}");
    assert!(pin_windows(pid).is_empty());
    let st = status(&sb);
    assert_eq!(st["pins"]["open"], 0);
    assert_eq!(st["memory"]["frames"], 1, "only the reopen memory");
    assert!(
        files_under(&sb.temp_dir()).is_empty(),
        "no temporary image files"
    );
    assert!(
        files_under(&sb.root.join("Local")).is_empty(),
        "no pin reached the disk: {:?}",
        files_under(&sb.root.join("Local"))
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// The restart policy: pins live in memory only. After a quit or a crash, a new instance has no
/// pins, no reopen memory and starts its IDs again; nothing was written to restore from.
#[test]
fn pins_do_not_survive_a_restart_or_a_crash() {
    let mut sb = Sandbox::synthetic("pins-restart", Keys::NoGlobals);
    let pid = sb.instance();
    for x in [10, 200] {
        let r = format!("{},{},40,30", X0 + x, Y0 + 20);
        let (code, doc) = sb.json(&["capture", "--region", &r, "--pin"]);
        assert_eq!(code, 0, "{doc:#}");
    }
    let (code, q) = sb.json(&["quit"]);
    assert_eq!(
        (code, q["error"]["code"].as_str()),
        (7, Some("confirmation_required")),
        "open pins need --yes; pins are not backed up"
    );
    let (code, q) = sb.json(&["quit", "--yes"]);
    assert_eq!((code, q["data"]["closed_pins"].as_u64()), (0, Some(2)));
    assert!(pin_windows(pid).is_empty());

    let pid = sb.instance();
    let l = list(&sb);
    assert_eq!(
        l,
        json!({"pins": [], "reopenable": null, "group": "default"}),
        "nothing restored"
    );
    let (code, doc) = sb.json(&["pin", "reopen"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("pin_not_found"))
    );
    let r = format!("{},{},10,10", X0 + 5, Y0 + 5);
    let (code, doc) = sb.json(&["capture", "--region", &r, "--pin"]);
    assert_eq!(
        (code, doc["data"]["pin"]["id"].as_str()),
        (0, Some("pin-1"))
    );

    // A crash (killed by force) leaves no pin window and nothing to restore.
    kill_pid(pid);
    assert!(noeul::win::wait_exit(pid, Duration::from_secs(5)));
    assert!(
        pin_windows(pid).is_empty(),
        "the windows went with the process"
    );
    sb.instance();
    assert_eq!(list(&sb)["pins"], json!([]));
    assert!(
        files_under(&sb.root.join("Local")).is_empty(),
        "no backup was ever written: {:?}",
        files_under(&sb.root.join("Local"))
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// The new commands report honestly: no instance, usage errors, unknown pins, and a clipboard
/// pin's dry run that reads nothing.
#[test]
fn pin_commands_report_honestly() {
    let mut sb = Sandbox::synthetic("pins-honest", Keys::NoGlobals);
    for args in [
        &["pin", "hide", "--all"][..],
        &["pin", "show", "pin-1"],
        &["pin", "--clipboard"],
        &["pin", "--clipboard", "--dry-run"],
    ] {
        let (code, doc) = sb.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    for args in [
        &["pin"][..],
        &["pin", "hide"],
        &["pin", "show", "pin-1", "--all"],
        &["pin", "--at", "1,2"],
        &["pin", "--clipboard", "--at", "1;2"],
    ] {
        let (code, doc) = sb.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (2, Some("usage")),
            "{args:?}"
        );
    }
    sb.instance();
    let (code, doc) = sb.json(&["pin", "hide", "pin-9"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("pin_not_found"))
    );
    let (code, doc) = sb.json(&["pin", "hide", "--all"]);
    assert_eq!((code, doc["data"]["hidden"].clone()), (0, json!([])));
    // A dry run of a clipboard pin opens nothing and reads nothing.
    let (code, doc) = sb.json(&["pin", "--clipboard", "--dry-run", "--at", "-19990,-19990"]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(doc["command"], "pin");
    assert_valid("noeul-ipc-v1#pin_create_result", &doc["data"]);
    assert_eq!(doc["data"]["pin"], Value::Null);
    assert_eq!(status(&sb)["pins"]["open"], 0);
    let human = String::from_utf8_lossy(&sb.run(&["pin", "list"]).stdout).to_string();
    assert!(human.contains("No pins are open"), "{human}");

    // The IPC methods over the real pipe, every answer checked against the schema.
    let mut s = noeul::win::client::open(&sb.endpoint).expect("the instance answers");
    let mut ask = |method: &str, params: Value| -> Value {
        let msg = noeul::ipc::request(1, method, params.clone());
        assert_valid("noeul-ipc-v1", &msg);
        let r = s
            .request(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e:?}"));
        assert_valid("noeul-ipc-v1", &noeul::ipc::ok(1, r.clone()));
        r
    };
    let region = json!({"x": X0 + 20, "y": Y0 + 30, "width": 64, "height": 48});
    let r = ask(
        "pin.create",
        json!({"source": "region", "region": region, "dry_run": true}),
    );
    assert_eq!(r["pin"], Value::Null, "a dry run captures nothing");
    let r = ask("pin.create", json!({"source": "region", "region": region}));
    assert_eq!(
        (r["pin"]["id"].as_str(), r["pin"]["rect"].clone()),
        (Some("pin-1"), region)
    );
    let r = ask("pin.list", json!({}));
    assert_eq!(r["pins"][0]["image"], json!({"width": 64, "height": 48}));
    let r = ask("pin.hide", json!({"id": "pin-1"}));
    assert_eq!(r["hidden"], json!(["pin-1"]));
    let r = ask("pin.show", json!({"all": true, "on_screen": true}));
    assert_eq!(r["shown"], json!(["pin-1"]));
    let r = ask("pin.close", json!({"all": true}));
    assert_eq!(r["closed"], json!(["pin-1"]));
    let r = ask("status", json!({}));
    assert_eq!(
        (r["pins"]["open"].as_u64(), r["pins"]["reopenable"].as_str()),
        (Some(0), Some("pin-1"))
    );
    drop(s);
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
