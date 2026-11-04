//! E2E — Step 56: Noeul's annotation shapes and text tools (NOEUL_UX_SPEC §4.9,
//! docs/adr/0038-noeul-annotation.md).
//! Requirements: NOEUL-FR-080, NOEUL-FR-081, NOEUL-FR-083, NOEUL-FR-021, NOEUL-NFR-012,
//! NOEUL-NFR-014, NOEUL-SEC-008.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the
//! annotation shortcuts headlessly — the `annotate` context (`annotate.done` `Escape`,
//! `annotate.undo` `Ctrl+Z`, `annotate.redo` `Ctrl+Y`/`Ctrl+Shift+Z`, the width keys `char:[`
//! and `char:]`, the object keys `Delete`, `N`, `PageUp`…), the seven tools without default keys,
//! and `pin.annotate` (`Space`) — in `hotkeys list`, and `capabilities` no longer says
//! "no annotation".
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): `capture
//! --interactive` opens the overlay on the synthetic desktop; a drag selects; a click on the
//! toolbar's rectangle tool and a drag draw a rectangle; the text tool and posted `WM_CHAR` /
//! `WM_IME_CHAR` messages type a label (Korean included); undo and redo (bound to `U` and `Y`
//! here, since a posted key cannot carry `Ctrl`) step back and forth; `Enter` pins the selection
//! with both annotations; `Space` on the pin opens the layered pin editor, `Escape` leaves it
//! unchanged; `Q` quick-saves the pin into the sandbox as a PNG of the selection's size. The
//! status reports counts only, never the label's text.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window (everything lies around
//! -20000,-20000 and is never activated), no input synthesized (messages are posted to Noeul's
//! own windows), the user's input method and clipboard are never touched, and no hotkey is
//! registered at all. Every process started here is ended.
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

/// Finding Noeul's windows and posting them the messages a mouse and keyboard would produce
/// (never `SendInput`).
#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub type Hwnd = *mut c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(cb: Option<unsafe extern "system" fn(Hwnd, isize) -> i32>, lp: isize)
        -> i32;
        fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
        fn GetClassNameW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
        fn PostMessageW(h: Hwnd, msg: u32, wp: usize, lp: isize) -> i32;
        fn GetWindowLongW(h: Hwnd, index: i32) -> i32;
    }

    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_CHAR: u32 = 0x0102;
    pub const WM_IME_CHAR: u32 = 0x0286;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;
    pub const WS_EX_LAYERED: u32 = 0x0008_0000;
    pub const WS_EX_TOPMOST: u32 = 0x0000_0008;

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

    pub fn ex_style(h: Hwnd) -> u32 {
        // SAFETY: a plain query on a window of the instance this scenario started.
        unsafe { GetWindowLongW(h, -20) as u32 }
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
    /// registered), with a few window keys a posted message can carry.
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
                    {"action": "paste.float", "disabled": true},
                    {"action": "annotate.undo", "bindings": ["Ctrl+Z", "U"]},
                    {"action": "annotate.redo", "bindings": ["Ctrl+Y", "Y"]},
                    {"action": "pin.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]}]}))
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
            .env_remove("NOEUL_DEV_DETECT_PIDS")
            .stdin(Stdio::null());
        if self.synthetic {
            c.env("NOEUL_DEV_SYNTHETIC_DESKTOP", "1");
        } else {
            c.env_remove("NOEUL_DEV_SYNTHETIC_DESKTOP");
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

    #[cfg(windows)]
    fn eventually(&self, what: &str, mut f: impl FnMut(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(15);
        loop {
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
fn a_the_annotation_shortcuts_are_discoverable_headlessly() {
    let n = Noeul::new("s56a", bin("noeul"), false);
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let action = |id: &str| -> Value {
        list["data"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed"))
            .clone()
    };
    let keys = |id: &str| -> Vec<String> {
        action(id)["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["keys"].as_str().unwrap().to_string())
            .collect()
    };
    for (id, want) in [
        ("annotate.done", vec!["Escape"]),
        ("annotate.width-down", vec!["char:["]),
        ("annotate.width-up", vec!["char:]"]),
        ("annotate.delete", vec!["Delete"]),
        ("annotate.select-next", vec!["N"]),
        ("annotate.bring-forward", vec!["PageUp"]),
        ("pin.annotate", vec!["Space"]),
        ("annotate.undo", vec!["Ctrl+Z", "U"]),
    ] {
        assert_eq!(keys(id), want, "{id}");
    }
    for tool in [
        "rectangle",
        "ellipse",
        "line",
        "arrow",
        "pen",
        "marker",
        "text",
    ] {
        assert!(
            keys(&format!("annotate.{tool}")).is_empty(),
            "{tool}: bindable, no default"
        );
    }
    let (_, caps) = n.json(&["capabilities"]);
    let text = caps.to_string();
    assert!(!text.contains("no annotation"), "{text}");
}

#[cfg(windows)]
fn lp(x: i32, y: i32) -> isize {
    ((y as u16 as u32) << 16 | (x as u16 as u32)) as isize
}

#[cfg(windows)]
#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_draw_type_undo_pin_edit_and_save_on_the_synthetic_desktop() {
    use win::*;
    let n = Noeul::new("s56b", kim_e2e::bin_debug("noeul"), true);
    let shots = n.sb.path().join("shots");
    let (code, _) = n.json(&[
        "config",
        "set",
        "capture.quick_save_folder",
        &shots.display().to_string(),
    ]);
    assert_eq!(code, 0);
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
    let (code, doc) = n.json(&["capture", "--interactive"]);
    assert_eq!((code, doc["data"]["opened"].as_bool()), (0, Some(true)));
    let ov = windows(pid, "NoeulOverlay")[0];
    assert_eq!(ex_style(ov) & WS_EX_TOPMOST, 0);
    let drag = |w: Hwnd, a: (i32, i32), b: (i32, i32)| {
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
    };
    let click = |w: Hwnd, x: i32, y: i32| {
        assert!(post(w, WM_LBUTTONDOWN, 1, lp(x, y)) && post(w, WM_LBUTTONUP, 0, lp(x, y)));
    };
    let key = |w: Hwnd, vk: usize| assert!(post(w, WM_KEYDOWN, vk, 0));
    let ann = |st: &Value| st["captures"]["overlay"]["annotation"].clone();
    drag(ov, (100, 100), (399, 299));
    n.eventually("selected", |st| {
        st["captures"]["overlay"]["phase"] == "selected"
    });
    // The toolbar sits below the selection from the 640-pixel monitor's left edge: the
    // rectangle tool's button is centred at (192, 324), the text tool's at (384, 324).
    click(ov, 192, 324);
    n.eventually("the rectangle tool", |st| ann(st)["tool"] == "rectangle");
    drag(ov, (150, 150), (250, 220));
    click(ov, 384, 324);
    n.eventually("the text tool", |st| ann(st)["tool"] == "text");
    click(ov, 160, 240);
    n.eventually("typing", |st| ann(st)["editing_text"] == true);
    for c in ['N', 'o', 't', 'e', ' '] {
        assert!(post(ov, WM_CHAR, c as usize, 0));
    }
    for c in ['\u{d55c}', '\u{ae00}'] {
        assert!(post(ov, WM_IME_CHAR, c as usize, 0));
    }
    key(ov, 0x1B); // Escape ends the label
    let st = n.eventually("two annotations", |st| {
        ann(st)["objects"] == 2 && ann(st)["editing_text"] == false
    });
    assert!(
        !st.to_string().contains('\u{d55c}'),
        "never the label's text"
    );
    key(ov, 0x55); // U: undo the label
    n.eventually("undone", |st| ann(st)["objects"] == 1);
    key(ov, 0x59); // Y: redo
    n.eventually("redone", |st| ann(st)["objects"] == 2);
    key(ov, 0x0D); // Enter pins with both
    n.eventually("pinned", |st| st["pins"]["open"] == 1);
    let (_, pins) = n.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][0]["annotations"], 2);
    // Space on the pin: the layered editor; Escape (no tool active) leaves the pin as it was.
    let pin = windows(pid, "NoeulPin")[0];
    key(pin, 0x20);
    n.eventually("the pin editor", |st| {
        st["pins"]["editor"]["mode"] == "pin_edit"
    });
    let ed = windows(pid, "NoeulOverlay")[0];
    assert_ne!(ex_style(ed) & WS_EX_LAYERED, 0);
    assert_eq!(ex_style(ed) & WS_EX_TOPMOST, 0);
    key(ed, 0x1B);
    n.eventually("closed unchanged", |st| st["pins"]["editor"].is_null());
    let (_, pins) = n.json(&["pin", "list"]);
    assert_eq!(pins["data"]["pins"][0]["annotations"], 2);
    // Q quick-saves the pin with its annotations into the sandbox.
    key(pin, 0x51);
    let until = Instant::now() + Duration::from_secs(10);
    let file = loop {
        let found: Vec<PathBuf> = std::fs::read_dir(&shots)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        if let Some(f) = found.into_iter().next() {
            break f;
        }
        assert!(Instant::now() < until, "the pin was saved");
        std::thread::sleep(Duration::from_millis(50));
    };
    let png = std::fs::read(&file).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // IHDR: width and height (big-endian) at bytes 16..24.
    let w = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((w, h), (300, 200), "the selection's size");
    drop(n);
}
