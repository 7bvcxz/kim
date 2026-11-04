//! Annotation through the real `noeul` binary (Step 56; the privacy tools since Step 57,
//! NOEUL-FR-082; NOEUL-FR-080, NOEUL-FR-081, NOEUL-FR-083, NOEUL-NFR-014; NOEUL_UX_SPEC §4.9,
//! §4.10).
//!
//! Safety on this shared machine, as in `double_click.rs`: an isolated profile on a private
//! endpoint, `--no-tray`, **no global shortcut**, and the debug-build test hook
//! (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the overlay, the pins and the pin editor show synthetic
//! pictures far off-screen (around -20000,-20000), are never topmost and never take focus; quick
//! saves go to the sandbox. The tests drive Noeul only with messages posted to its own windows
//! (mouse, keys, `WM_CHAR`, `WM_IME_*`, the wheel) — no input is synthesized, the user's input
//! method is never switched and the clipboard is never touched — and every process a test starts
//! is ended.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use noeul::capture::{Monitor, Synthetic};
use noeul::frame::Rect;
use noeul::overlay::{EDIT_TOOLBAR, TOOLBAR, ToolbarItem, toolbar_layout};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CHAR, WM_IME_CHAR,
    WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
};

/// The synthetic desktop's primary monitor (100 %, 640 × 400) starts here.
const X0: i32 = -20_000;
const Y0: i32 = -20_000;
const RED: [u8; 4] = [0x4D, 0x48, 0xE5, 0xFF];

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    let e = validate("noeul-ipc-v1#status_result", &doc["data"]);
    assert!(e.is_empty(), "{e:?}\n{:#}", doc["data"]);
    doc["data"].clone()
}

/// Polls `f` until it holds. Every poll is a separate `noeul status` process and the instance
/// is one too, so on a loaded machine (a whole workspace's tests at once) a step can take
/// seconds: the limit is generous, and a real failure still fails — with the instance's last
/// status, so it says what the instance was doing instead of only what was awaited.
fn eventually(sb: &Sandbox, what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(60);
    while !f() {
        if Instant::now() >= until {
            let (_, doc) = sb.json(&["status"]);
            panic!(
                "timed out waiting for: {what}\nlast status: {:#}",
                doc["data"]
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Drags the selection (100,100)-(399,299) of the synthetic desktop and waits until it is
/// confirmed with exactly that rectangle (a modifier read from the user's real keyboard would
/// make it a square, for example, and move the toolbar the next click aims at).
fn select(sb: &Sandbox, ov: HWND) {
    drag(ov, (100, 100), (399, 299));
    eventually(sb, "the selection is confirmed", || {
        status(sb)["captures"]["overlay"]["phase"] == "selected"
    });
    assert_eq!(
        status(sb)["captures"]["overlay"]["selection"],
        Rect::new(X0 + 100, Y0 + 100, 300, 200).to_json(),
        "the dragged rectangle, unchanged by any key the user holds elsewhere"
    );
}

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

fn post(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> bool {
    // SAFETY: a pointer-free message to a window this test's own child process owns.
    unsafe { PostMessageW(hwnd, msg, wp, lp) != 0 }
}

fn lp(x: i32, y: i32) -> isize {
    ((y as u16 as u32) << 16 | (x as u16 as u32)) as isize
}

/// No global hotkey; overlay and pin keys a posted message can carry: `Q` quick-saves (overlay
/// and pin), `U` undoes, `Y` redoes, `1` chooses the arrow tool (tools have no default keys).
fn sandbox(tag: &str) -> Sandbox {
    let sb = Sandbox::synthetic(tag, Keys::NoGlobals);
    std::fs::write(
        sb.hotkeys_file(),
        serde_json::to_vec_pretty(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
                {"action": "capture.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]},
                {"action": "pin.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]},
                {"action": "annotate.undo", "bindings": ["Ctrl+Z", "U"]},
                {"action": "annotate.redo", "bindings": ["Ctrl+Y", "Y"]},
                {"action": "annotate.arrow", "bindings": ["1"]}]}))
        .unwrap(),
    )
    .unwrap();
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

fn shots(sb: &Sandbox) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(sb.root.join("shots"))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A saved PNG as `(width, height, BGRA)`.
fn png_bgra(path: &PathBuf) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(path).unwrap()))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    let mut out = Vec::new();
    for p in buf[..info.buffer_size()].as_chunks::<3>().0 {
        out.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    (info.width, info.height, out)
}

fn px(img: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.0 + x) * 4) as usize;
    [img.2[i], img.2[i + 1], img.2[i + 2], img.2[i + 3]]
}

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
    assert_eq!(w.ex & WS_EX_TOPMOST, 0);
    assert_ne!(w.ex & WS_EX_NOACTIVATE, 0);
    w.hwnd
}

/// A drag with the left button in a window's client coordinates.
fn drag(w: HWND, a: (i32, i32), b: (i32, i32)) {
    assert!(post(w, WM_MOUSEMOVE, 0, lp(a.0, a.1)));
    assert!(post(w, WM_LBUTTONDOWN, 1, lp(a.0, a.1)));
    assert!(post(
        w,
        WM_MOUSEMOVE,
        1,
        lp((a.0 + b.0) / 2, (a.1 + b.1) / 2)
    ));
    assert!(post(w, WM_MOUSEMOVE, 1, lp(b.0, b.1)));
    assert!(post(w, WM_LBUTTONUP, 0, lp(b.0, b.1)));
}

fn click(w: HWND, x: i32, y: i32) {
    assert!(post(w, WM_LBUTTONDOWN, 1, lp(x, y)) && post(w, WM_LBUTTONUP, 0, lp(x, y)));
}

fn key(w: HWND, vk: usize) {
    assert!(post(w, WM_KEYDOWN, vk, 0));
}

/// The centre of a toolbar button for a selection `s` (absolute) on the 100 % monitor, in the
/// client coordinates of a window whose top-left is `origin`.
fn button(items: &[ToolbarItem], s: Rect, item: ToolbarItem, origin: (i32, i32)) -> (i32, i32) {
    let m = Monitor::new(Rect::new(X0, Y0, 640, 400), true);
    let bar = toolbar_layout(items, s, &m, None);
    let i = items.iter().position(|b| *b == item).unwrap();
    let r = bar.buttons[i];
    (
        r.x + i32::try_from(r.width / 2).unwrap() - origin.0,
        r.y + i32::try_from(r.height / 2).unwrap() - origin.1,
    )
}

fn annotation(st: &Value) -> Value {
    if st["captures"]["overlay"].is_null() {
        st["pins"]["editor"]["annotation"].clone()
    } else {
        st["captures"]["overlay"]["annotation"].clone()
    }
}

/// Shapes, the arrow by a bound key, undo/redo, the wheel and Escape through the real overlay;
/// quick save writes exactly the frozen pixels with the annotations.
#[test]
fn shapes_drawn_in_the_overlay_are_saved_exactly() {
    let mut sb = sandbox("ann-save");
    let pid = sb.instance();
    let ov = open_overlay(&sb, pid);
    select(&sb, ov);
    let sel = Rect::new(X0 + 100, Y0 + 100, 300, 200);
    let (rx, ry) = button(
        &TOOLBAR,
        sel,
        ToolbarItem::Tool(noeul::annotation::Tool::Rectangle),
        (X0, Y0),
    );
    click(ov, rx, ry);
    eventually(&sb, "the rectangle tool", || {
        let st = status(&sb);
        st["captures"]["overlay"]["phase"] == "annotating" && annotation(&st)["tool"] == "rectangle"
    });
    drag(ov, (150, 150), (250, 220));
    key(ov, 0x31); // `1`: the arrow tool
    drag(ov, (300, 150), (380, 260));
    eventually(&sb, "two objects", || {
        annotation(&status(&sb))["objects"] == 2
    });
    let st = status(&sb);
    assert_eq!(annotation(&st)["kinds"]["rectangle"], 1);
    assert_eq!(annotation(&st)["kinds"]["arrow"], 1);
    assert_eq!(annotation(&st)["tool"], "arrow");
    key(ov, 0x55); // U: undo
    eventually(&sb, "undone", || annotation(&status(&sb))["objects"] == 1);
    key(ov, 0x59); // Y: redo
    eventually(&sb, "redone", || annotation(&status(&sb))["objects"] == 2);
    assert!(post(ov, WM_MOUSEWHEEL, 120 << 16, 0));
    eventually(&sb, "one level thicker", || {
        annotation(&status(&sb))["width"] == 4
    });
    key(ov, 0x1B); // Escape: leaves the tool, keeps the drawing
    eventually(&sb, "the tool is left", || {
        let st = status(&sb);
        st["captures"]["overlay"]["phase"] == "selected" && annotation(&st)["objects"] == 2
    });
    key(ov, 0x51); // Q: quick save
    eventually(&sb, "saved", || status(&sb)["captures"]["open"] == false);
    let files = shots(&sb);
    assert_eq!(files.len(), 1, "{files:?}");
    let img = png_bgra(&files[0]);
    assert_eq!((img.0, img.1), (300, 200));
    // The rectangle (150,150)-(250,220): its 3-pixel left edge at x 50..52 of the picture.
    for x in 50..53 {
        assert_eq!(px(&img, x, 80), RED, "left edge at {x}");
    }
    let inside = Synthetic::pixel(X0 + 160, Y0 + 180);
    assert_eq!(
        px(&img, 60, 80),
        inside,
        "inside the outline: the frozen pixels"
    );
    assert_eq!(
        px(&img, 5, 5),
        Synthetic::pixel(X0 + 105, Y0 + 105),
        "untouched elsewhere"
    );
    // The arrow's head near (380, 260) is drawn; nothing beyond it.
    let near_tip = (275..281).any(|x| (155..161).any(|y| px(&img, x, y) == RED));
    assert!(near_tip, "the arrow's head near its tip");
    assert_eq!(px(&img, 295, 190), Synthetic::pixel(X0 + 395, Y0 + 290));
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// A label typed with characters and the input method's messages becomes part of a pin; the
/// pin's annotations are edited in the layered editor (Space), committed with Enter and
/// cancelled losslessly with Escape; the pin's quick save carries the annotations.
#[test]
fn a_label_and_a_pin_edit_through_the_real_windows() {
    let mut sb = sandbox("ann-pin");
    let pid = sb.instance();
    let ov = open_overlay(&sb, pid);
    select(&sb, ov);
    let sel = Rect::new(X0 + 100, Y0 + 100, 300, 200);
    let (tx, ty) = button(
        &TOOLBAR,
        sel,
        ToolbarItem::Tool(noeul::annotation::Tool::Text),
        (X0, Y0),
    );
    click(ov, tx, ty);
    eventually(&sb, "the text tool", || {
        annotation(&status(&sb))["tool"] == "text"
    });
    click(ov, 150, 150);
    eventually(&sb, "a label is edited", || {
        annotation(&status(&sb))["editing_text"] == true
    });
    // `E` is detect.cycle, but while typing it is a character; Korean arrives as the IME's
    // result character (what DefWindowProc turns a committed composition into).
    let mode = status(&sb)["captures"]["overlay"]["detection"]["mode"].clone();
    key(ov, 0x45);
    for c in ['H', 'i', ' '] {
        assert!(post(ov, WM_CHAR, c as usize, 0));
    }
    assert!(post(ov, WM_IME_STARTCOMPOSITION, 0, 0));
    assert!(post(ov, WM_IME_COMPOSITION, 0, 0x8)); // GCS_COMPSTR (the real context is empty)
    assert!(post(ov, WM_IME_CHAR, 0xD55C, 0));
    assert!(post(ov, WM_IME_CHAR, 0xAE00, 0));
    assert!(post(ov, WM_IME_ENDCOMPOSITION, 0, 0));
    key(ov, 0x1B); // Escape ends the label
    eventually(&sb, "the label is done", || {
        annotation(&status(&sb))["editing_text"] == false
    });
    let st = status(&sb);
    assert_eq!(
        st["captures"]["overlay"]["detection"]["mode"], mode,
        "E was text"
    );
    assert_eq!(annotation(&st)["kinds"]["text"], 1);
    key(ov, 0x0D); // Enter pins with the label
    eventually(&sb, "pinned", || status(&sb)["pins"]["open"] == 1);
    let (code, doc) = sb.json(&["pin", "list"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["pins"][0]["annotations"], 1);
    let pin = of_class(pid, "NoeulPin")[0].hwnd;
    // Space on the pin: the editor opens over it, layered, off-screen, never activated.
    key(pin, 0x20);
    eventually(&sb, "the editor", || {
        !status(&sb)["pins"]["editor"].is_null()
    });
    let st = status(&sb);
    assert_eq!(st["pins"]["editor"]["mode"], "pin_edit");
    assert_eq!(st["pins"]["editor"]["pin"], "pin-1");
    assert_eq!(annotation(&st)["objects"], 1);
    let ed = of_class(pid, "NoeulOverlay")
        .into_iter()
        .next()
        .expect("the editor window");
    assert_ne!(ed.ex & WS_EX_LAYERED, 0, "layered");
    assert_eq!(ed.ex & WS_EX_TOPMOST, 0);
    assert_ne!(ed.ex & WS_EX_NOACTIVATE, 0);
    let visible = |w: HWND| {
        // SAFETY: a plain query on the instance's window.
        unsafe { IsWindowVisible(w) != 0 }
    };
    assert!(!visible(pin), "the pin hides while edited");
    let origin = (ed.rect.0, ed.rect.1);
    let (lx, ly) = button(
        &EDIT_TOOLBAR,
        sel,
        ToolbarItem::Tool(noeul::annotation::Tool::Line),
        origin,
    );
    click(ed.hwnd, lx, ly);
    eventually(&sb, "the line tool", || {
        annotation(&status(&sb))["tool"] == "line"
    });
    let c = |x: i32, y: i32| (X0 + x - origin.0, Y0 + y - origin.1);
    drag(ed.hwnd, c(120, 270), c(380, 270));
    eventually(&sb, "a line", || annotation(&status(&sb))["objects"] == 2);
    key(ed.hwnd, 0x0D); // Enter commits
    eventually(&sb, "committed", || status(&sb)["pins"]["editor"].is_null());
    assert_eq!(status(&sb)["last_action"]["result"], "committed");
    let (_, doc) = sb.json(&["pin", "list"]);
    assert_eq!(doc["data"]["pins"][0]["annotations"], 2);
    assert_eq!(doc["data"]["pins"][0]["editing"], false);
    assert!(visible(pin), "shown again");
    // A cancelled edit changes nothing.
    key(pin, 0x20);
    eventually(&sb, "the editor again", || {
        !status(&sb)["pins"]["editor"].is_null()
    });
    let ed = of_class(pid, "NoeulOverlay")[0].hwnd;
    click(ed, lx, ly);
    drag(ed, c(120, 200), c(380, 200));
    eventually(&sb, "a third line", || {
        annotation(&status(&sb))["objects"] == 3
    });
    key(ed, 0x1B); // leave the tool
    key(ed, 0x1B); // cancel
    eventually(&sb, "cancelled", || status(&sb)["pins"]["editor"].is_null());
    let (_, doc) = sb.json(&["pin", "list"]);
    assert_eq!(doc["data"]["pins"][0]["annotations"], 2, "lossless cancel");
    // The pin's quick save carries the committed annotations.
    key(pin, 0x51);
    eventually(&sb, "the pin was saved", || shots(&sb).len() == 1);
    let img = png_bgra(&shots(&sb)[0]);
    assert_eq!((img.0, img.1), (300, 200));
    assert_eq!(px(&img, 150, 170), RED, "the committed line at y 270");
    assert_eq!(
        px(&img, 150, 100),
        Synthetic::pixel(X0 + 250, Y0 + 200),
        "not the cancelled one"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// The chunk types of a saved PNG, consecutive repeats (several `IDAT`) folded.
fn png_chunks(path: &PathBuf) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut out: Vec<String> = Vec::new();
    let mut i = 8;
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        let kind = String::from_utf8_lossy(&bytes[i + 4..i + 8]).into_owned();
        if out.last() != Some(&kind) {
            out.push(kind);
        }
        i += 12 + len;
    }
    assert_eq!(i, bytes.len(), "nothing after IEND");
    out
}

/// Step 57 (NOEUL-FR-082): the privacy tools through the real overlay and a pin. A solid
/// redaction chosen on the toolbar and dragged is quick-saved as the opaque fill — not one frozen
/// pixel under it, no chunk beyond IHDR, IDAT and IEND, no temporary file; a blur next to it
/// changes its pixels; the redaction rides along into a pin, whose quick save is redacted too.
#[test]
fn a_redaction_through_the_real_windows_is_saved_without_the_original() {
    const FILL: [u8; 4] = [0x11, 0x11, 0x11, 0xFF];
    let mut sb = sandbox("ann-redact");
    let pid = sb.instance();
    let sel = Rect::new(X0 + 100, Y0 + 100, 300, 200);
    let tool = |t| button(&TOOLBAR, sel, ToolbarItem::Tool(t), (X0, Y0));
    let ov = open_overlay(&sb, pid);
    select(&sb, ov);
    let (x, y) = tool(noeul::annotation::Tool::Redact);
    click(ov, x, y);
    eventually(&sb, "the redaction tool", || {
        let st = status(&sb);
        annotation(&st)["tool"] == "redact" && annotation(&st)["color"] == "black"
    });
    drag(ov, (150, 150), (250, 220));
    let (x, y) = tool(noeul::annotation::Tool::Blur);
    click(ov, x, y);
    eventually(&sb, "the blur tool", || {
        annotation(&status(&sb))["tool"] == "blur"
    });
    drag(ov, (280, 130), (380, 260));
    eventually(&sb, "a redaction and a blur", || {
        let a = annotation(&status(&sb));
        a["kinds"]["redact"] == 1 && a["kinds"]["blur"] == 1
    });
    key(ov, 0x51); // Q: quick save
    eventually(&sb, "saved", || status(&sb)["captures"]["open"] == false);
    let files = shots(&sb);
    assert_eq!(files.len(), 1, "one file, no temporary one: {files:?}");
    assert_eq!(png_chunks(&files[0]), ["IHDR", "IDAT", "IEND"]);
    let img = png_bgra(&files[0]);
    assert_eq!((img.0, img.1), (300, 200));
    // The box (150,150)-(250,220) is (50,50)-(150,120) of the picture, both inclusive.
    for y in 50..121 {
        for x in 50..151 {
            assert_eq!(px(&img, x, y), FILL, "({x}, {y}) under the redaction");
        }
    }
    assert_eq!(
        px(&img, 49, 80),
        Synthetic::pixel(X0 + 149, Y0 + 180),
        "next to it: the frozen pixels"
    );
    let blurred = (30..161)
        .flat_map(|y| (180..281).map(move |x| (x, y)))
        .filter(|(x, y)| {
            px(&img, *x, *y) != Synthetic::pixel(X0 + 100 + *x as i32, Y0 + 100 + *y as i32)
        })
        .count();
    assert!(blurred > 1000, "the blur changed its pixels: {blurred}");
    // Into a pin: its quick save is redacted as well.
    let ov = open_overlay(&sb, pid);
    select(&sb, ov);
    let (x, y) = tool(noeul::annotation::Tool::Redact);
    click(ov, x, y);
    eventually(&sb, "the redaction tool", || {
        annotation(&status(&sb))["tool"] == "redact"
    });
    drag(ov, (150, 150), (250, 220));
    eventually(&sb, "a redaction", || {
        annotation(&status(&sb))["kinds"]["redact"] == 1
    });
    key(ov, 0x0D); // Enter pins with the redaction
    eventually(&sb, "pinned", || status(&sb)["pins"]["open"] == 1);
    let pin = of_class(pid, "NoeulPin")[0].hwnd;
    key(pin, 0x51);
    eventually(&sb, "the pin was saved", || shots(&sb).len() == 2);
    let second = shots(&sb)
        .into_iter()
        .find(|f| *f != files[0])
        .expect("the pin's file");
    assert_eq!(png_chunks(&second), ["IHDR", "IDAT", "IEND"]);
    let img = png_bgra(&second);
    for y in 50..121 {
        for x in 50..151 {
            assert_eq!(px(&img, x, y), FILL, "({x}, {y}) under the pin's redaction");
        }
    }
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
