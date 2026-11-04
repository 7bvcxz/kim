//! Paste through the real `noeul` binary (Step 55; NOEUL-FR-070…NOEUL-FR-074, NOEUL-SEC-002,
//! NOEUL-SEC-008, NOEUL-SEC-009, NOEUL-NFR-008).
//!
//! Safety on this shared machine: every instance runs in an isolated profile on a private endpoint
//! with `--no-tray`, no global shortcut and the debug-build test hook
//! (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): pins lie far off-screen (around -20000,-20000), are never
//! topmost and never take focus. **The user's clipboard is never read**: under the hook a paste
//! reads the in-memory stand-in, which these tests fill with synthesized formats in a sandbox
//! folder (`NOEUL_DEV_CLIPBOARD_DIR`); every picture and file is generated here. Pins are driven
//! only through the CLI and messages posted to the instance's own windows; no input is
//! synthesized. The only socket is a listener on 127.0.0.1 that proves HTML makes no request.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Keys, Sandbox, validate};
use image::{ExtendedColorType, ImageEncoder};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, PostMessageW, WM_KEYDOWN, WS_EX_NOACTIVATE, WS_EX_TOPMOST,
};

const X0: i32 = -20_000;
const AT: &str = "-19990,-19990";

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

/// Polls `f` until it holds (60 s: every poll is a process; a real failure still fails, with the
/// instance's last status).
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

fn pins_of(pid: u32) -> Vec<Win> {
    windows_of(pid)
        .into_iter()
        .filter(|w| w.class == "NoeulPin")
        .collect()
}

fn ramp(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[
                (x * 30) as u8,
                (y * 50) as u8,
                180,
                if x == 0 { 0 } else { 255 },
            ]);
        }
    }
    v
}

/// A picture of `w` × `h` in one of the `image` crate's formats (JPEG drops the alpha).
fn encode(format: &str, w: u32, h: u32) -> Vec<u8> {
    let px = ramp(w, h);
    let mut out = Vec::new();
    let c = ExtendedColorType::Rgba8;
    match format {
        "png" => image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&px, w, h, c)
            .unwrap(),
        "jpg" => {
            let rgb: Vec<u8> = px.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
                .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
                .unwrap();
        }
        "bmp" => image::codecs::bmp::BmpEncoder::new(&mut out)
            .write_image(&px, w, h, c)
            .unwrap(),
        "ico" => image::codecs::ico::IcoEncoder::new(&mut out)
            .write_image(&px, w, h, c)
            .unwrap(),
        "tga" => image::codecs::tga::TgaEncoder::new(&mut out)
            .write_image(&px, w, h, c)
            .unwrap(),
        "tif" => {
            let mut cur = std::io::Cursor::new(Vec::new());
            image::codecs::tiff::TiffEncoder::new(&mut cur)
                .write_image(&px, w, h, c)
                .unwrap();
            out = cur.into_inner();
        }
        "gif" => {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            for _ in 0..3 {
                let buf =
                    image::RgbaImage::from_raw(w, h, [200u8, 10, 10, 255].repeat((w * h) as usize))
                        .unwrap();
                enc.encode_frame(image::Frame::new(buf)).unwrap();
            }
        }
        _ => unreachable!(),
    }
    out
}

/// A packed 32-bit DIB (BITMAPINFOHEADER, bottom-up) of one colour.
fn dib(w: u32, h: u32, bgra: [u8; 4]) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&(w as i32).to_le_bytes());
    d.extend_from_slice(&(h as i32).to_le_bytes());
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&32u16.to_le_bytes());
    d.extend_from_slice(&0u32.to_le_bytes());
    d.extend_from_slice(&(w * h * 4).to_le_bytes());
    d.extend_from_slice(&[0; 16]);
    for _ in 0..w * h {
        d.extend_from_slice(&bgra);
    }
    d
}

fn cf_html(fragment: &str) -> String {
    format!(
        "Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:-1\r\nEndFragment:-1\r\n<html><body><!--StartFragment-->{fragment}<!--EndFragment--></body></html>"
    )
}

/// The stand-in clipboard of a sandbox: a folder of synthesized formats.
struct Clip {
    dir: PathBuf,
}

impl Clip {
    fn new(sb: &mut Sandbox) -> Clip {
        let dir = sb.root.join("clipboard");
        std::fs::create_dir_all(&dir).unwrap();
        sb.env
            .push(("NOEUL_DEV_CLIPBOARD_DIR".into(), dir.display().to_string()));
        Clip { dir }
    }

    /// Replaces the stand-in's content with these formats.
    fn set(&self, formats: &[(&str, &[u8])]) {
        for e in std::fs::read_dir(&self.dir).unwrap().flatten() {
            std::fs::remove_file(e.path()).unwrap();
        }
        for (name, bytes) in formats {
            std::fs::write(self.dir.join(name), bytes).unwrap();
        }
    }
}

/// `noeul pin --clipboard` (+ args) with its result checked against the schema.
fn paste(sb: &Sandbox, extra: &[&str]) -> (i32, Value) {
    let mut args = vec!["pin", "--clipboard", "--at", AT];
    args.extend_from_slice(extra);
    let (code, doc) = sb.json(&args);
    if code == 0 {
        assert_valid("noeul-ipc-v1#pin_create_result", &doc["data"]);
    }
    (code, doc)
}

fn files_dir(sb: &Sandbox) -> PathBuf {
    let d = sb.root.join("files");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Every kind of clipboard content becomes a pin of its kind, off-screen and never activated;
/// HTML before text, a colour before HTML, `--as` picks another kind, image files (several,
/// every format) and a copied path pin their files.
#[test]
fn every_kind_pins_from_the_stand_in_clipboard() {
    let mut sb = Sandbox::synthetic("paste-kinds", Keys::NoGlobals);
    let clip = Clip::new(&mut sb);
    let pid = sb.instance();
    let mut want_pins = 0;
    let mut check = |sb: &Sandbox, doc: &Value, kind: &str, content: &str, pins: usize| {
        assert_eq!(doc["data"]["kind"], kind, "{doc:#}");
        want_pins += pins;
        let l = list(sb);
        assert_eq!(l["pins"].as_array().unwrap().len(), want_pins, "{l:#}");
        let last = l["pins"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["content"], content, "{last:#}");
        let wins = pins_of(pid);
        assert_eq!(wins.len(), want_pins, "one window per pin");
        for w in &wins {
            assert!(w.rect.0 < X0 + 2000, "far off-screen: {:?}", w.rect);
            assert_eq!(w.ex & WS_EX_TOPMOST, 0);
            assert_ne!(w.ex & WS_EX_NOACTIVATE, 0);
        }
    };
    // A PNG next to a DIB: the PNG wins.
    clip.set(&[
        ("PNG", &encode("png", 24, 16)),
        ("CF_DIB", &dib(8, 8, [1, 2, 3, 255])),
    ]);
    let (code, doc) = paste(&sb, &[]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(doc["data"]["offered"], json!(["image"]));
    assert_eq!(doc["data"]["pin"]["rect"]["width"], 24);
    check(&sb, &doc, "image", "image", 1);
    // A DIB alone (what most programs offer).
    clip.set(&[("CF_DIBV5", &dib(10, 6, [9, 8, 7, 255]))]);
    let (_, doc) = paste(&sb, &[]);
    assert_eq!(doc["data"]["pin"]["rect"]["width"], 10);
    check(&sb, &doc, "image", "image", 1);
    // Text with HTML: the HTML card; `--as text` the text card.
    let html =
        cf_html("<h2>Release notes</h2><p>Fixed <b>three</b> bugs.</p><ul><li>one</li></ul>");
    clip.set(&[
        ("HTML Format", html.as_bytes()),
        ("CF_UNICODETEXT", b"Release notes\nFixed three bugs.\none"),
    ]);
    let (_, doc) = paste(&sb, &[]);
    assert_eq!(doc["data"]["offered"], json!(["html", "text"]));
    check(&sb, &doc, "html", "html", 1);
    let (_, doc) = paste(&sb, &["--as", "text"]);
    check(&sb, &doc, "text", "text", 1);
    // A colour value.
    clip.set(&[("CF_UNICODETEXT", b"#1E90FF")]);
    let (_, doc) = paste(&sb, &[]);
    check(&sb, &doc, "color", "color", 1);
    // Korean text.
    clip.set(&[("CF_UNICODETEXT", "노을 카드 — 한글 텍스트".as_bytes())]);
    let (_, doc) = paste(&sb, &[]);
    check(&sb, &doc, "text", "text", 1);
    // Image files copied in Explorer: every format at once, a document skipped.
    let dir = files_dir(&sb);
    let mut paths = Vec::new();
    for f in ["png", "jpg", "bmp", "ico", "tga", "tif", "gif"] {
        let p = dir.join(format!("shot.{f}"));
        std::fs::write(&p, encode(f, 12, 9)).unwrap();
        paths.push(p.display().to_string());
    }
    let doc_path = dir.join("readme.txt");
    std::fs::write(&doc_path, "words").unwrap();
    paths.push(doc_path.display().to_string());
    clip.set(&[("CF_HDROP", paths.join("\r\n").as_bytes())]);
    let (code, doc) = paste(&sb, &[]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(doc["data"]["pins"].as_array().unwrap().len(), 7, "{doc:#}");
    assert_eq!(
        (
            doc["data"]["skipped"].as_u64(),
            doc["data"]["failed"].as_u64()
        ),
        (Some(1), Some(0))
    );
    check(&sb, &doc, "image_file", "image", 7);
    let l = list(&sb);
    let gif = l["pins"].as_array().unwrap().last().unwrap();
    assert_eq!(
        gif["animation"]["frames"], 3,
        "the GIF says it shows its first frame"
    );
    // Nothing reported names a path or a file.
    let all = format!("{doc}{l}{}", status(&sb));
    assert!(!all.contains("shot.") && !all.contains("readme"), "{all}");
    // A copied path ("Copy as path" quotes it).
    let one = dir.join("shot.png");
    clip.set(&[(
        "CF_UNICODETEXT",
        format!("\"{}\"", one.display()).as_bytes(),
    )]);
    let (_, doc) = paste(&sb, &[]);
    check(&sb, &doc, "image_file", "image", 1);
    // `noeul pin <file>` reads the file itself.
    let (code, doc) = sb.json(&["pin", one.to_str().unwrap(), "--at", AT]);
    assert_eq!(code, 0, "{doc:#}");
    assert_valid("noeul-ipc-v1#pin_create_result", &doc["data"]);
    assert_eq!(doc["data"]["pin"]["source"], "file");
    check(&sb, &doc, "image_file", "image", 1);
    let human =
        String::from_utf8_lossy(&sb.run(&["pin", one.to_str().unwrap(), "--at", AT]).stdout)
            .to_string();
    assert!(
        human.starts_with("Pinned the image file as pin-"),
        "{human}"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

/// An empty, private, held, malformed, oversized or hostile source pins nothing and says why
/// with its documented code; nothing is left behind.
#[test]
fn bad_sources_fail_with_their_codes_and_leave_nothing() {
    let mut sb = Sandbox::synthetic("paste-bad", Keys::NoGlobals);
    let clip = Clip::new(&mut sb);
    let _pid = sb.instance();
    let fail = |sb: &Sandbox, args: &[&str], code: i32, error: &str| {
        let (c, doc) = sb.json(args);
        assert_eq!(
            (c, doc["error"]["code"].as_str()),
            (code, Some(error)),
            "{args:?}: {doc:#}"
        );
        doc
    };
    let pin_clip = ["pin", "--clipboard", "--at", AT];
    clip.set(&[]);
    fail(&sb, &pin_clip, 3, "nothing_to_pin");
    clip.set(&[
        ("CF_UNICODETEXT", b"hunter2-secret"),
        ("ExcludeClipboardContentFromMonitorProcessing", b""),
    ]);
    let doc = fail(&sb, &pin_clip, 3, "nothing_to_pin");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("private")
    );
    assert!(!doc.to_string().contains("hunter2"), "never the content");
    // A PNG header claiming 30000 × 30000 pixels (a decompression bomb).
    let mut bomb = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    bomb.extend_from_slice(&30_000u32.to_be_bytes());
    bomb.extend_from_slice(&30_000u32.to_be_bytes());
    bomb.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    clip.set(&[("PNG", &bomb)]);
    fail(&sb, &pin_clip, 4, "image_invalid");
    clip.set(&[("CF_DIB", &[40, 0, 0, 0, 255, 255, 255, 127][..])]);
    fail(&sb, &pin_clip, 4, "image_invalid");
    clip.set(&[("PNG", b"<script>not a png</script>")]);
    fail(&sb, &pin_clip, 4, "image_invalid");
    // Files: malformed, not an image, missing, a network path, a folder.
    let dir = files_dir(&sb);
    let mut jpg = encode("jpg", 16, 16);
    jpg.truncate(40);
    std::fs::write(dir.join("cut.jpg"), &jpg).unwrap();
    std::fs::write(dir.join("fake.png"), b"just text").unwrap();
    let path = |n: &str| dir.join(n).display().to_string();
    fail(
        &sb,
        &["pin", &path("cut.jpg"), "--at", AT],
        4,
        "image_invalid",
    );
    fail(
        &sb,
        &["pin", &path("fake.png"), "--at", AT],
        3,
        "nothing_to_pin",
    );
    fail(
        &sb,
        &["pin", &path("missing.png"), "--at", AT],
        3,
        "nothing_to_pin",
    );
    fail(&sb, &["pin", r"\\127.0.0.1\share\x.png"], 2, "usage");
    fail(
        &sb,
        &["pin", &dir.display().to_string(), "--at", AT],
        3,
        "nothing_to_pin",
    );
    fail(&sb, &["pin", "--clipboard", "--as", "pdf"], 2, "usage");
    let st = status(&sb);
    assert_eq!(st["pins"]["open"], 0, "nothing was pinned");
    assert_eq!(
        st["memory"]["frames"], 0,
        "no partial picture: {:#}",
        st["memory"]
    );
    // A dry run reads nothing at all.
    let (code, doc) = sb.json(&["pin", &path("cut.jpg"), "--dry-run"]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(doc["data"]["pin"], Value::Null);
    drop(sb);
    // A clipboard another program holds (the stand-in says so): retried, then reported.
    let mut busy = Sandbox::synthetic("paste-busy", Keys::NoGlobals);
    busy.env.push(("NOEUL_DEV_CLIPBOARD".into(), "busy".into()));
    let _ = Clip::new(&mut busy);
    busy.instance();
    fail(&busy, &pin_clip, 6, "clipboard_unavailable");
}

/// HTML that names remote images, styles, scripts and frames on a local listener: the card is
/// made and the listener never sees a connection (NOEUL-NFR-008, NOEUL-SEC-009).
#[test]
fn html_with_remote_resources_makes_no_request() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let u = |p: &str| format!("http://127.0.0.1:{port}/{p}");
    let html = cf_html(&format!(
        "<link rel=stylesheet href=\"{}\"><style>@import url({});</style>\
         <script src=\"{}\"></script><p style=\"background:url({})\">Text \
         <img src=\"{}\" alt=\"remote\"> <img srcset=\"{} 2x\"></p>\
         <iframe src=\"{}\"></iframe><object data=\"{}\"></object><video src=\"{}\"></video>",
        u("a.css"),
        u("b.css"),
        u("c.js"),
        u("d.png"),
        u("e.png"),
        u("f.png"),
        u("g.html"),
        u("h.swf"),
        u("i.mp4"),
    ));
    let mut sb = Sandbox::synthetic("paste-remote", Keys::NoGlobals);
    let clip = Clip::new(&mut sb);
    sb.instance();
    clip.set(&[("HTML Format", html.as_bytes())]);
    let (code, doc) = paste(&sb, &[]);
    assert_eq!(code, 0, "{doc:#}");
    assert_eq!(list(&sb)["pins"][0]["content"], "html");
    // Give a stray request every chance to arrive.
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        match listener.accept() {
            Ok((_, from)) => panic!("Noeul made a request from {from}"),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("listener: {e}"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn key(hwnd: HWND, vk: usize) {
    // SAFETY: a pointer-free message to a window this test's own child process owns.
    assert!(unsafe { PostMessageW(hwnd, WM_KEYDOWN, vk, 0) } != 0);
}

/// A card takes the pin keys: it turns, copies its text back (on the test hook's stand-in), closes
/// and reopens as the same card.
#[test]
fn a_card_turns_copies_its_text_and_reopens() {
    let mut sb = Sandbox::synthetic("paste-card", Keys::NoGlobals);
    std::fs::write(
        sb.hotkeys_file(),
        serde_json::to_vec_pretty(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "capture.region", "disabled": true},
                {"action": "paste.float", "disabled": true},
                {"action": "pin.copy-text", "bindings": ["Ctrl+Shift+C", "X"]}]}))
        .unwrap(),
    )
    .unwrap();
    let clip = Clip::new(&mut sb);
    let pid = sb.instance();
    clip.set(&[("CF_UNICODETEXT", b"a card of text\nsecond line")]);
    let (code, doc) = paste(&sb, &[]);
    assert_eq!(code, 0, "{doc:#}");
    let w = doc["data"]["pin"]["rect"]["width"].as_u64().unwrap();
    let h = doc["data"]["pin"]["rect"]["height"].as_u64().unwrap();
    let hwnd = pins_of(pid)[0].hwnd;
    key(hwnd, 0x31); // `1`: rotate right
    eventually(&sb, "the card turned", || {
        list(&sb)["pins"][0]["orientation"]["rotation"] == 90
    });
    let r = &list(&sb)["pins"][0]["rect"];
    assert_eq!(
        (r["width"].as_u64(), r["height"].as_u64()),
        (Some(h), Some(w))
    );
    key(hwnd, 0x58); // X: pin.copy-text
    eventually(&sb, "the text was copied", || {
        status(&sb)["last_action"] == json!({"action": "pin.copy-text", "result": "copied"})
    });
    key(hwnd, 0x1B); // Escape: close
    eventually(&sb, "closed", || status(&sb)["pins"]["open"] == 0);
    let (code, _) = sb.json(&["pin", "reopen"]);
    assert_eq!(code, 0);
    assert_eq!(list(&sb)["pins"][0]["content"], "text");
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
