//! E2E — Step 51: Noeul's double-click-to-pin signature flow (NOEUL_UX_SPEC §4,
//! docs/adr/0032-noeul-double-click-pin-and-outputs.md).
//! Requirements: NOEUL-FR-020…026, NOEUL-FR-030…032, NOEUL-FR-102, NOEUL-NFR-012,
//! NOEUL-SEC-005.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the flow
//! headlessly — `config.set` and `config.reset` in `capabilities`, the overlay's copy, save and
//! quick-save keys in `hotkeys list`, the gestures in `config show` — changes the double-click
//! gesture with a dry run (nothing written), is refused an invalid value, and turns the
//! Snipaste-style preset on and off (config and shortcuts written, then undone).
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the user's
//! signature flow on the synthetic desktop. `capture --interactive` opens the overlay (off-screen,
//! never focused or on top); a drag selects; a double-click outside and a slow second click pin
//! nothing; a quick double-click inside pins exactly the selection and closes the overlay; then
//! Tab, Tab, Enter copies (to the hook's stand-in clipboard) and a quick save writes a PNG into
//! the sandbox only. Five rapid captures make five independent pins. The overlay is driven only
//! by messages posted to Noeul's own window.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized,
//! the user's clipboard and Pictures folder are never touched, and no hotkey is registered at
//! all (both global actions are disabled in the profile before anything starts). Every process
//! started here is ended.
#![allow(unsafe_code)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
#[cfg(windows)]
const X0: i64 = -20_000;
#[cfg(windows)]
const Y0: i64 = -20_000;

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

/// The few Win32 calls the scenario needs to find Noeul's own overlay window and post it the
/// messages a mouse and keyboard would produce (never `SendInput`).
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
        fn GetDoubleClickTime() -> u32;
    }

    pub const WM_KEYDOWN: u32 = 0x0100;
    pub const WM_MOUSEMOVE: u32 = 0x0200;
    pub const WM_LBUTTONDOWN: u32 = 0x0201;
    pub const WM_LBUTTONUP: u32 = 0x0202;

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
                // SAFETY: plain queries on a window handle (a stale one fails harmlessly).
                let n = unsafe {
                    GetWindowThreadProcessId(h, &raw mut owner);
                    GetClassNameW(h, name.as_mut_ptr(), 64)
                };
                owner == pid
                    && String::from_utf16_lossy(&name[..usize::try_from(n).unwrap_or(0)]) == class
            })
            .collect()
    }

    /// Posts a pointer-free message to a window of the instance this scenario started.
    pub fn post(h: Hwnd, msg: u32, wp: usize, lp: isize) -> bool {
        // SAFETY: a pointer-free message to a window this scenario's own child process owns.
        unsafe { PostMessageW(h, msg, wp, lp) != 0 }
    }

    pub fn double_click_ms() -> u64 {
        // SAFETY: a plain query.
        u64::from(unsafe { GetDoubleClickTime() })
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
    /// registered); the overlay keys stay on, with `Q` as an extra quick-save key (a posted
    /// key message cannot carry `Ctrl`), and quick save goes to `<sandbox>/shots`.
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
                    {"action": "capture.quick-save", "bindings": ["Ctrl+Shift+S", "Q"]}]}))
            .unwrap(),
        )
        .unwrap();
        let shots = sb
            .path()
            .join("shots")
            .display()
            .to_string()
            .replace('\\', "/");
        std::fs::write(
            cfg.join("config.toml"),
            format!("schema_version = 1\n\n[capture]\nquick_save_folder = \"{shots}\"\n"),
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

    fn start(&self) -> u64 {
        let (code, start) = self.json(&["start", "--no-tray"]);
        assert_eq!(code, 0, "{start:#}");
        start["data"]["pid"].as_u64().unwrap()
    }

    fn status(&self) -> Value {
        let (code, st) = self.json(&["status"]);
        assert_eq!(code, 0, "{st:#}");
        valid("noeul-ipc-v1#status_result", &st["data"]);
        st["data"].clone()
    }

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
fn a_the_flow_and_its_settings_are_discoverable_headlessly() {
    let n = Noeul::new("s51a", bin("noeul"), false);
    let (_, caps) = n.json(&["capabilities"]);
    valid("tool-capabilities-v1", &caps["data"]);
    let names: Vec<&str> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in ["config.show", "config.set", "config.reset"] {
        assert!(names.contains(&name), "{name} is listed: {names:?}");
    }
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
    assert_eq!(keys("capture.pin"), ["Enter"]);
    assert_eq!(keys("capture.copy"), ["Ctrl+C"]);
    assert_eq!(keys("capture.save"), ["Ctrl+S"]);
    let (_, show) = n.json(&["config", "show"]);
    assert_eq!(
        show["data"]["settings"]["gestures"],
        json!({"double_click": "pin", "middle_click": "pin", "preset": "noeul"})
    );
    let config = n.sb.path().join("Roaming/mouv/noeul/config.toml");
    let before = std::fs::read_to_string(&config).unwrap();
    let (code, dry) = n.json(&[
        "config",
        "set",
        "gestures.double_click",
        "copy",
        "--dry-run",
    ]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "a dry run writes nothing"
    );
    let (code, doc) = n.json(&["config", "set", "gestures.double_click", "close"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, on) = n.json(&["config", "set", "gestures.preset", "snipaste"]);
    assert_eq!(code, 0, "{on:#}");
    assert_eq!(on["data"]["instance"], "not_running", "nothing was started");
    let (_, show) = n.json(&["config", "show"]);
    assert_eq!(show["data"]["settings"]["gestures"]["double_click"], "copy");
    let (code, off) = n.json(&["config", "reset", "gestures", "--yes"]);
    assert_eq!(code, 0, "{off:#}");
    let (_, list) = n.json(&["hotkeys", "list"]);
    let pin_keys: Vec<&str> = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "capture.pin")
        .unwrap()["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["keys"].as_str().unwrap())
        .collect();
    assert_eq!(pin_keys, ["Enter"], "reset undid the preset's keys");
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("quick_save_folder"),
        "the other settings stay"
    );
}

#[cfg(windows)]
fn lp(x: i64, y: i64) -> isize {
    ((y as u16 as u32) << 16 | (x as u16 as u32)) as isize
}

#[cfg(windows)]
#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_f1_drag_double_click_pins_on_the_synthetic_desktop() {
    use win::*;
    let n = Noeul::new("s51b", kim_e2e::bin_debug("noeul"), true);
    let pid = u32::try_from(n.start()).unwrap();
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
        assert_eq!(
            (code, doc["data"]["opened"].as_bool()),
            (0, Some(true)),
            "{doc:#}"
        );
        windows(pid, "NoeulOverlay")[0]
    };
    let select = |ov: Hwnd, a: (i64, i64), b: (i64, i64)| -> Value {
        for (msg, wp, p) in [
            (WM_MOUSEMOVE, 0, a),
            (WM_LBUTTONDOWN, 1, a),
            (WM_MOUSEMOVE, 1, ((a.0 + b.0) / 2, (a.1 + b.1) / 2)),
            (WM_MOUSEMOVE, 1, b),
            (WM_LBUTTONUP, 0, b),
        ] {
            assert!(post(ov, msg, wp, lp(p.0, p.1)));
        }
        let want =
            json!({"x": X0 + a.0, "y": Y0 + a.1, "width": b.0 - a.0 + 1, "height": b.1 - a.1 + 1});
        n.eventually("the selection is confirmed", |st| {
            st["captures"]["overlay"]["selection"] == want
        });
        want
    };
    let click = |ov: Hwnd, x: i64, y: i64| {
        assert!(post(ov, WM_LBUTTONDOWN, 1, lp(x, y)));
        let _ = post(ov, WM_LBUTTONUP, 0, lp(x, y));
    };
    let slow = Duration::from_millis(double_click_ms() + 200);

    // F1 (here `capture --interactive`), drag, double-click outside, a slow second click.
    let ov = open();
    let sel = select(ov, (100, 100), (399, 299));
    click(ov, 500, 350);
    click(ov, 500, 350);
    std::thread::sleep(slow);
    click(ov, 250, 200);
    std::thread::sleep(slow);
    click(ov, 250, 200);
    std::thread::sleep(slow);
    let st = n.eventually("the posts were handled", |st| {
        st["captures"]["open"] == true
    });
    assert_eq!(st["pins"]["open"], 0, "nothing was pinned");
    // The signature double-click.
    click(ov, 250, 200);
    click(ov, 250, 200);
    let st = n.eventually("the pin appeared", |st| st["pins"]["open"] == 1);
    assert_eq!(st["captures"]["open"], false, "the overlay closed");
    assert_eq!(st["captures"]["copied"], 0, "the clipboard was not used");
    let (_, l) = n.json(&["pin", "list"]);
    assert_eq!(
        l["data"]["pins"][0]["rect"], sel,
        "exactly the selection, in place"
    );
    assert_eq!(windows(pid, "NoeulPin").len(), 1);

    // Tab, Tab, Enter: Enter on the focused Copy button copies (never pins).
    let ov = open();
    select(ov, (100, 100), (399, 299));
    for vk in [0x09, 0x09, 0x0D] {
        assert!(post(ov, WM_KEYDOWN, vk, 0));
    }
    let st = n.eventually("copied", |st| st["captures"]["copied"] == 1);
    assert_eq!(st["pins"]["open"], 1);
    // Quick save (`Q` here): one PNG in the sandbox's folder only.
    let ov = open();
    select(ov, (100, 100), (399, 299));
    assert!(post(ov, WM_KEYDOWN, 0x51, 0));
    n.eventually("saved", |st| st["captures"]["saved"] == 1);
    let shots: Vec<_> = std::fs::read_dir(n.sb.path().join("shots"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(shots.len(), 1);
    let png = std::fs::read(shots[0].path()).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

    // Five rapid captures: five more independent pins.
    for i in 0..5i64 {
        let ov = open();
        let x = 20 + i * 100;
        select(ov, (x, 120), (x + 59, 179));
        click(ov, x + 30, 160);
        click(ov, x + 30, 160);
        n.eventually("the pin appeared", |st| st["pins"]["open"] == 2 + i);
    }
    assert_eq!(windows(pid, "NoeulPin").len(), 6);
    let (code, q) = n.json(&["quit", "--yes"]);
    assert_eq!((code, q["data"]["closed_pins"].as_u64()), (0, Some(6)));
    assert_eq!(
        std::fs::read_dir(n.sb.path().join("Local")).map_or(0, |d| d
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .count()),
        0,
        "no picture in Noeul's data folder"
    );
}
