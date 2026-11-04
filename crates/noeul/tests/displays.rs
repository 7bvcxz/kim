//! Displays through the real `noeul` binary (Step 58, ADR 0050; NOEUL-FR-016, NOEUL-FR-017,
//! NOEUL-FR-067, NOEUL-FR-090…NOEUL-FR-093, NOEUL-SEC-004): monitor, window and region captures
//! from the CLI compared pixel by pixel, the pointer, the debounce of display notifications, and
//! pins and the overlay after attach, detach and a new scale.
//!
//! Safety on this shared machine: an isolated profile on a private endpoint, `--no-tray`, no
//! global shortcut, and the debug-build test hook. The synthetic desktop lies far off-screen
//! (around -20000,-20000) and takes its **layout from a file** (`NOEUL_DEV_SYNTHETIC_LAYOUT`) that
//! this test rewrites to attach, remove and rescale monitors — the real displays are never touched
//! and nothing reads the real screen. The "active window" is a window of this test, off-screen
//! inside the synthetic desktop, named in `NOEUL_DEV_ACTIVE_WINDOW` and owned by a process in
//! `NOEUL_DEV_DETECT_PIDS`, so the instance never looks at the user's foreground window. Display
//! notifications are messages posted to Noeul's own windows; pictures are compared from the
//! sandbox's quick-save folder, never the user's.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use noeul::capture::Synthetic;
use noeul::frame::Rect;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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
    GetWindowRect, GetWindowThreadProcessId, MSG, PM_REMOVE, PeekMessageW, PostMessageW,
    RegisterClassExW, SMTO_ABORTIFHUNG, SW_SHOWNOACTIVATE, SendMessageTimeoutW, ShowWindow,
    TranslateMessage, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP,
};

/// The synthetic desktop's origin (`noeul::capture::OFF_SCREEN`).
const X0: i32 = -20_000;
const Y0: i32 = -20_000;

/// The first layout: a 100 % primary, a 150 % monitor to its left (negative coordinates), a
/// portrait 125 % HDR monitor to its right that reaches higher. Positions relative to (X0, Y0).
fn layout() -> Value {
    json!({
        "monitors": [
            {"x": 0, "y": 0, "width": 640, "height": 400, "primary": true,
             "work": {"x": 0, "y": 0, "width": 640, "height": 368}},
            {"x": -480, "y": 50, "width": 480, "height": 300, "dpi": 144},
            {"x": 640, "y": -200, "width": 300, "height": 480, "dpi": 120, "rotation": 90, "hdr": true}
        ],
        "pointer": {"x": 30, "y": 20}
    })
}

fn write_layout(sb: &Sandbox, v: &Value) {
    let p = sb.root.join("layout.json");
    let tmp = sb.root.join("layout.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    std::fs::rename(&tmp, &p).unwrap();
}

/// No global hotkey; `Q` quick-saves a pin into `<root>/shots`; the layout file and this test's
/// own window (when given) for the instance.
fn sandbox(tag: &str, window: Option<HWND>) -> Sandbox {
    let mut sb = Sandbox::synthetic(tag, Keys::NoGlobals);
    std::fs::write(
        sb.hotkeys_file(),
        serde_json::to_vec_pretty(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
                {"action": "pin.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]}]}))
        .unwrap(),
    )
    .unwrap();
    let paths = noeul::paths::Paths::under(&sb.root);
    let (state, loaded) = noeul::hotkeys::load(&paths);
    assert!(!loaded.state.safe_mode(), "{:?}", loaded.problems);
    assert!(
        noeul::hotkeys::global_plan(&state).0.is_empty(),
        "no global hotkey is registered (capture.monitor and capture.window have no default)"
    );
    let shots = sb.root.join("shots").display().to_string();
    let edited = noeul::settings::set(
        &paths.config_file(),
        &[("capture.quick_save_folder", &shots)],
    )
    .unwrap();
    noeul::settings::write(&paths.config_file(), &edited.text).unwrap();
    write_layout(&sb, &layout());
    let file = sb.root.join("layout.json").display().to_string();
    sb.env.push(("NOEUL_DEV_SYNTHETIC_LAYOUT".into(), file));
    sb.env.push((
        "NOEUL_DEV_DETECT_PIDS".into(),
        std::process::id().to_string(),
    ));
    if let Some(h) = window {
        sb.env
            .push(("NOEUL_DEV_ACTIVE_WINDOW".into(), (h as usize).to_string()));
    }
    sb
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// This test's own "active window": a plain popup inside the synthetic primary monitor, far
/// off-screen, never activated, on a thread that pumps its messages until dropped.
struct Target {
    hwnd: HWND,
    rect: Rect,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Target {
    fn spawn() -> Target {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            // SAFETY: this thread's DPI context, so its rectangle is in device pixels.
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let class = wide("NoeulDisplaysTarget");
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
            let title = wide("NOEUL-SECRET-WINDOW-TITLE");
            // SAFETY: our class; plain values; shown without activation, off-screen.
            let hwnd = unsafe {
                let h = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    class.as_ptr(),
                    title.as_ptr(),
                    WS_POPUP,
                    X0 + 100,
                    Y0 + 60,
                    200,
                    120,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    GetModuleHandleW(std::ptr::null()),
                    std::ptr::null(),
                );
                assert!(!h.is_null());
                ShowWindow(h, SW_SHOWNOACTIVATE);
                h
            };
            let mut r = RECT::default();
            // SAFETY: our window; `r` is writable.
            unsafe { GetWindowRect(hwnd, &raw mut r) };
            tx.send((
                hwnd as usize,
                Rect::new(
                    r.left,
                    r.top,
                    u32::try_from(r.right - r.left).unwrap(),
                    u32::try_from(r.bottom - r.top).unwrap(),
                ),
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
            // SAFETY: our own window, destroyed once on its thread.
            unsafe { DestroyWindow(hwnd as HWND) };
        });
        let (h, rect) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        Target {
            hwnd: h as HWND,
            rect,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The top-level windows of the instance with this class.
fn windows_of(pid: u32, class: &str) -> Vec<(HWND, Rect)> {
    unsafe extern "system" fn each(h: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
        unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
        1
    }
    let mut all: Vec<isize> = Vec::new();
    // SAFETY: the callback only touches `all`; only the instance's own windows are looked at.
    unsafe { EnumWindows(Some(each), (&raw mut all) as LPARAM) };
    all.into_iter()
        .filter_map(|h| {
            let hwnd = h as HWND;
            let mut owner = 0u32;
            // SAFETY: plain queries on a window of the instance this test started.
            unsafe {
                GetWindowThreadProcessId(hwnd, &raw mut owner);
                if owner != pid {
                    return None;
                }
                let mut name = [0u16; 64];
                let n = GetClassNameW(hwnd, name.as_mut_ptr(), 64);
                if String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]) != class {
                    return None;
                }
                let mut r = RECT::default();
                GetWindowRect(hwnd, &raw mut r);
                Some((
                    hwnd,
                    Rect::new(
                        r.left,
                        r.top,
                        u32::try_from(r.right - r.left).unwrap_or(0),
                        u32::try_from(r.bottom - r.top).unwrap_or(0),
                    ),
                ))
            }
        })
        .collect()
}

fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) {
    // SAFETY: a pointer-free message to a window of the instance this test started.
    assert_ne!(unsafe { PostMessageW(hwnd, msg, wp, lp) }, 0, "posted");
}

/// `WM_DPICHANGED` as Windows sends it: synchronously, with the suggested rectangle (which Noeul
/// ignores). It cannot be posted (its lParam is a pointer).
fn send_dpi_changed(hwnd: HWND, dpi: u32) {
    let mut r = RECT::default();
    // SAFETY: `r` outlives the call; the window belongs to the instance this test started, and
    // the call gives up after two seconds if that process does not answer.
    let ok = unsafe {
        let mut out = 0usize;
        GetWindowRect(hwnd, &raw mut r);
        SendMessageTimeoutW(
            hwnd,
            WM_DPICHANGED,
            ((dpi << 16) | dpi) as usize,
            (&raw const r) as isize,
            SMTO_ABORTIFHUNG,
            2000,
            &raw mut out,
        )
    };
    assert_ne!(ok, 0, "sent");
}

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    let e = validate("noeul-ipc-v1#status_result", &doc["data"]);
    assert!(e.is_empty(), "{e:?}\n{doc:#}");
    doc["data"].clone()
}

/// `noeul capture <args> --json`, checked against the schemas.
fn capture(sb: &Sandbox, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["capture"];
    full.extend_from_slice(args);
    let (code, doc) = sb.json(&full);
    if code == 0 {
        let e = validate("noeul-ipc-v1#capture_result", &doc["data"]);
        assert!(e.is_empty(), "{e:?}\n{doc:#}");
        assert_eq!(
            doc["warnings"], doc["data"]["warnings"],
            "the envelope carries them"
        );
    }
    (code, doc)
}

fn shots(sb: &Sandbox) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(sb.root.join("shots"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A saved PNG as `(width, height, BGRA)`.
fn png_bgra(path: &Path) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(path).unwrap()))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb, "opaque: no alpha");
    let mut out = Vec::new();
    for p in buf[..info.buffer_size()].as_chunks::<3>().0 {
        out.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    (info.width, info.height, out)
}

/// Quick-saves the pin whose window is `w` (its `Q` key) and returns the saved picture.
fn saved_picture(sb: &Sandbox, w: HWND) -> (u32, u32, Vec<u8>) {
    let before = shots(sb);
    post(w, WM_KEYDOWN, 0x51, 0); // Q: pin.quick-save in this sandbox
    eventually("the pin was quick-saved", || {
        shots(sb).len() == before.len() + 1
    });
    let path = shots(sb).into_iter().find(|p| !before.contains(p)).unwrap();
    // The file is complete once it can be decoded (it is written whole, then renamed).
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(bytes) = std::fs::read(&path)
            && png::Decoder::new(std::io::Cursor::new(bytes))
                .read_info()
                .is_ok()
        {
            return png_bgra(&path);
        }
        assert!(Instant::now() < until, "a readable PNG");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pin window that is new since `known`.
fn new_pin_window(pid: u32, known: &mut Vec<usize>) -> HWND {
    let mut found = None;
    eventually("a new pin window", || {
        found = windows_of(pid, "NoeulPin")
            .into_iter()
            .find(|(h, _)| !known.contains(&(*h as usize)));
        found.is_some()
    });
    let h = found.unwrap().0;
    known.push(h as usize);
    h
}

/// Every pixel of `img` (covering `r`) is the synthetic pattern, except where `except` says.
fn assert_picture(
    img: &(u32, u32, Vec<u8>),
    r: Rect,
    except: impl Fn(i32, i32) -> Option<[u8; 4]>,
) {
    assert_eq!((img.0, img.1), (r.width, r.height), "native pixel size");
    for y in 0..r.height as i32 {
        for x in 0..r.width as i32 {
            let (ax, ay) = (r.x + x, r.y + y);
            let i = ((y as u32 * r.width + x as u32) * 4) as usize;
            let want = except(ax, ay).unwrap_or_else(|| Synthetic::pixel(ax, ay));
            assert_eq!(img.2[i..i + 4], want, "({ax}, {ay})");
        }
    }
}

#[test]
fn monitor_window_and_region_captures_through_the_cli_are_pixel_exact() {
    let target = Target::spawn();
    let mut sb = sandbox("displays-capture", Some(target.hwnd));
    let pid = sb.instance();
    let mut known = Vec::new();

    // The layout as the instance reads it: three monitors, one turned and HDR.
    let st = status(&sb);
    let ms = st["desktop"]["monitors"].as_array().unwrap().clone();
    assert_eq!(ms.len(), 3);
    assert_eq!(
        (ms[2]["rotation"].as_u64(), ms[2]["hdr"].as_bool()),
        (Some(90), Some(true))
    );
    assert_eq!(ms[1]["dpi"], 144);
    assert_eq!(
        st["desktop"]["virtual_rect"],
        json!({"x": X0 - 480, "y": Y0 - 200, "width": 480 + 640 + 300, "height": 600})
    );

    // A whole monitor at 150 % left of the primary: native pixels, its logical size, a pin in
    // place.
    let t = Instant::now();
    let (code, doc) = capture(&sb, &["--monitor", "2", "--pin"]);
    let monitor_ms = t.elapsed().as_millis();
    assert_eq!(code, 0, "{doc:#}");
    let d = &doc["data"];
    let left = Rect::new(X0 - 480, Y0 + 50, 480, 300);
    assert_eq!(Rect::from_json(&d["region"]), Some(left));
    assert_eq!(
        d["mapping"]["logical"],
        json!({"x": 0, "y": 0, "width": 320, "height": 200})
    );
    assert_eq!(d["mapping"]["scale_percent"], 150);
    assert_eq!(d["warnings"], json!([]));
    assert_eq!(Rect::from_json(&d["pin"]["rect"]), Some(left));
    let img = saved_picture(&sb, new_pin_window(pid, &mut known));
    assert_picture(&img, left, |_, _| None);

    // The turned HDR monitor with the pointer asked for: the pointer is on the primary, so it is
    // not in the picture, and both limits are said.
    let (code, doc) = capture(&sb, &["--monitor", "3", "--pin", "--pointer"]);
    assert_eq!(code, 0, "{doc:#}");
    let d = &doc["data"];
    assert_eq!(d["monitor"]["rotation"], 90);
    assert_eq!(d["pointer"]["reason"], "outside");
    let w = d["warnings"].as_array().unwrap();
    assert!(
        w.iter().any(|w| w.as_str().unwrap().contains("HDR")),
        "{doc:#}"
    );
    assert!(
        w.iter()
            .any(|w| w.as_str().unwrap().contains("pointer is outside")),
        "{doc:#}"
    );
    let portrait = Rect::new(X0 + 640, Y0 - 200, 300, 480);
    let img = saved_picture(&sb, new_pin_window(pid, &mut known));
    assert_picture(&img, portrait, |_, _| None);

    // A region across the seam of 150 % and 100 %, partly over a gap, with the pointer: the gap
    // is opaque black, the pointer's hotspot is on its device pixel, nothing is rescaled.
    let region = Rect::new(X0 - 40, Y0 + 10, 100, 60);
    let (code, doc) = capture(
        &sb,
        &["--region", &region.to_string(), "--pin", "--pointer"],
    );
    assert_eq!(code, 0, "{doc:#}");
    let d = &doc["data"];
    assert_eq!(
        d["pointer"],
        json!({"drawn": true, "rect": {"x": X0 + 30, "y": Y0 + 20, "width": 12, "height": 19}, "reason": null})
    );
    assert_eq!(
        (
            d["mapping"]["spans"].as_u64(),
            d["mapping"]["mixed_dpi"].as_bool()
        ),
        (Some(2), Some(true))
    );
    let img = saved_picture(&sb, new_pin_window(pid, &mut known));
    assert_picture(&img, region, |x, y| {
        let (sx, sy) = (x - (X0 + 30), y - (Y0 + 20));
        if (0..12).contains(&sx)
            && (0..19).contains(&sy)
            && let Some(p) = noeul::displays::pointer_sprite_pixel(sx as usize, sy as usize)
        {
            return Some(p);
        }
        (x < X0 && y < Y0 + 50).then_some([0, 0, 0, 255])
    });

    // The active window: this test's own, cut from the (synthetic) screen at its frame.
    let (code, doc) = capture(&sb, &["--window", "active", "--pin"]);
    assert_eq!(code, 0, "{doc:#}");
    let d = &doc["data"];
    assert_eq!(Rect::from_json(&d["window"]["rect"]), Some(target.rect));
    assert_eq!(d["window"]["affinity"], "none");
    assert!(
        !doc.to_string().contains("NOEUL-SECRET"),
        "no window title anywhere (NOEUL-SEC-007)"
    );
    let img = saved_picture(&sb, new_pin_window(pid, &mut known));
    assert_picture(&img, target.rect, |_, _| None);

    // Honest refusals and the human output.
    let (code, doc) = capture(&sb, &["--monitor", "9", "--pin"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("monitor_not_found"))
    );
    let (code, doc) = capture(&sb, &["--monitor", "2"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (2, Some("usage")),
        "--pin is required"
    );
    let (code, _) = capture(&sb, &["--monitor", "zero", "--pin"]);
    assert_eq!(code, 2);
    let out = sb.run(&["capture", "--monitor", "2", "--pin", "--dry-run"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Would capture monitor 2") && text.contains("320×200 logical at 150 %"),
        "{text}"
    );
    assert_eq!(status(&sb)["pins"]["open"], 4);
    eprintln!("capture --monitor 2 --pin (480×300, CLI round trip): {monitor_ms} ms");
}

#[test]
fn display_changes_are_debounced_and_pins_and_the_overlay_follow() {
    let mut sb = sandbox("displays-change", None);
    let pid = sb.instance();
    let mut known = Vec::new();
    let (code, doc) = capture(&sb, &["--monitor", "2", "--pin"]);
    assert_eq!(code, 0, "{doc:#}");
    let w1 = new_pin_window(pid, &mut known);
    let primary_pin = Rect::new(X0 + 100, Y0 + 100, 50, 40);
    let (code, doc) = capture(&sb, &["--region", &primary_pin.to_string(), "--pin"]);
    assert_eq!(code, 0, "{doc:#}");
    let w2 = new_pin_window(pid, &mut known);
    let at = |dpi: u32, r: Rect| {
        let v = noeul::transform::View {
            dpi,
            ..noeul::transform::View::plain(r)
        };
        v.window()
    };
    let rect_of = |w: HWND| {
        windows_of(pid, "NoeulPin")
            .into_iter()
            .find(|(h, _)| *h == w)
            .map(|(_, r)| r)
            .unwrap()
    };
    assert_eq!(rect_of(w2), at(96, primary_pin), "drawn for 100 %");
    let control = windows_of(pid, "NoeulControl")[0].0;

    // A burst of notifications while the layout changes: the left monitor is unplugged and the
    // primary goes to 150 %. The instance looks at the displays once.
    let mut next = layout();
    next["monitors"] = json!([
        {"x": 0, "y": 0, "width": 640, "height": 400, "primary": true, "dpi": 144},
        layout()["monitors"][2].clone()
    ]);
    write_layout(&sb, &next);
    let t = Instant::now();
    for _ in 0..10 {
        post(control, WM_DISPLAYCHANGE, 32, 0);
    }
    eventually("the burst was handled", || {
        status(&sb)["displays"]["handled"] == 1
    });
    let settled_ms = t.elapsed().as_millis();
    std::thread::sleep(Duration::from_millis(600));
    let st = status(&sb);
    assert_eq!(
        st["displays"],
        json!({"changes": 10, "handled": 1, "pending": false}),
        "once"
    );
    assert!(
        settled_ms >= 200,
        "it waited for the layout to be quiet: {settled_ms} ms"
    );
    assert_eq!(st["desktop"]["monitors"].as_array().unwrap().len(), 2);
    assert_eq!(
        st["last_action"],
        json!({"action": "display.change", "result": "off_screen"})
    );
    assert_eq!(st["pins"]["off_screen"], 1);
    // The pin on the 150 % primary keeps its pixel size and place; its outline and shadow (and
    // the band that resizes it) are drawn for the new scale.
    assert_eq!(rect_of(w2), at(144, primary_pin));
    assert_ne!(at(144, primary_pin), at(96, primary_pin));
    let (code, l) = sb.json(&["pin", "list"]);
    assert_eq!(code, 0);
    assert_eq!(l["data"]["pins"][0]["on_screen"], false);
    assert_eq!(
        Rect::from_json(&l["data"]["pins"][1]["rect"]),
        Some(primary_pin)
    );

    // A pin told that its DPI changed is a notification too.
    send_dpi_changed(w2, 144);
    eventually("the DPI change was handled", || {
        status(&sb)["displays"]["handled"] == 2
    });
    // The lost pin comes back onto the primary on request, drawn for its scale.
    let (code, s) = sb.json(&["pin", "show", "--all", "--on-screen"]);
    assert_eq!((code, s["data"]["moved"].clone()), (0, json!(["pin-1"])));
    let r1 = rect_of(w1);
    assert!(r1.x >= X0 - 30 && r1.y >= Y0 - 30, "on the primary: {r1}");
    assert_eq!(status(&sb)["pins"]["off_screen"], 0);

    // An open overlay keeps its frozen picture through a new scale ...
    let (code, doc) = sb.json(&["capture", "--interactive"]);
    assert_eq!(
        (code, doc["data"]["opened"].as_bool()),
        (0, Some(true)),
        "{doc:#}"
    );
    let overlay = windows_of(pid, "NoeulOverlay")[0].0;
    let mut scaled = next.clone();
    scaled["monitors"][0]["dpi"] = json!(192);
    write_layout(&sb, &scaled);
    send_dpi_changed(overlay, 192);
    eventually("the scale change was handled", || {
        status(&sb)["displays"]["handled"] == 3
    });
    let st = status(&sb);
    assert!(st["captures"]["overlay"].is_object(), "still open: {st:#}");
    assert_eq!(
        st["last_action"],
        json!({"action": "display.change", "result": "ok"})
    );
    // ... and ends, with nothing kept, when a monitor is attached.
    let mut attached = scaled.clone();
    attached["monitors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"x": -1920, "y": 0, "width": 1920, "height": 1080}));
    write_layout(&sb, &attached);
    post(overlay, WM_DISPLAYCHANGE, 32, 0);
    eventually("the capture was cancelled", || {
        status(&sb)["captures"]["overlay"].is_null()
    });
    let st = status(&sb);
    assert_eq!(
        st["last_action"],
        json!({"action": "display.change", "result": "capture_cancelled"})
    );
    assert_eq!(st["captures"]["cancelled"], 1);
    assert_eq!(st["pins"]["open"], 2, "the pins stay");
    // The next capture covers the new layout.
    let (code, doc) = sb.json(&["capture", "--interactive"]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(
        doc["data"]["picture"],
        json!({"x": X0 - 1920, "y": Y0 - 200, "width": 1920 + 640 + 300, "height": 1280})
    );
    let (code, _) = sb.json(&["capture", "--cancel"]);
    assert_eq!(code, 0);
    eprintln!("ten WM_DISPLAYCHANGE to one handling: {settled_ms} ms");
}
