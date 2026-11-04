//! E2E — Step 59: Noeul's pixel colour picker and the colour clipboard workflow (NOEUL_UX_SPEC
//! §4.14, docs/adr/0053-noeul-color-picker.md).
//! Requirements: NOEUL-FR-041, NOEUL-FR-044, NOEUL-FR-071, NOEUL-FR-092, NOEUL-NFR-013,
//! NOEUL-NFR-014, NOEUL-SEC-002, NOEUL-SEC-008.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the picker
//! headlessly — `color.copy` (`C`) and `color.format-next` (`Shift+C`) in `hotkeys list`, the
//! `[color]` settings in `config show` — a dry run of `color.format` writes nothing, an invalid
//! value is refused, `color.hex_case lower` is written and `config reset color --yes` undoes it,
//! and the keys can be rebound with a dry run.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the user opens
//! the overlay (`capture --interactive`), hovers known pixels of the synthetic desktop on the
//! 100 % and the 150 % monitor, presses `C` and gets the exact HEX on the clipboard; `K` (bound
//! here next to `Shift+C`, which cannot be posted without a real Shift key) switches to RGB and
//! HSL; each copied text is pinned back as a colour swatch with `noeul pin --clipboard` (F3's
//! command-line twin); next to a selection `C` copies and the selection stays. Under the hook the
//! clipboard is the in-memory stand-in folder of this scenario (`NOEUL_DEV_CLIPBOARD_DIR`),
//! **never the user's clipboard**, and the overlay only ever shows the synthetic picture far
//! off-screen.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized
//! (messages are posted to Noeul's own overlay window), the real cursor never moves, no hotkey is
//! registered (both global actions are disabled in the profile before anything starts) and the
//! clipboard is never touched. Every process started here is ended.
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

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
#[cfg(windows)]
const X0: i32 = -20_000;
#[cfg(windows)]
const Y0: i32 = -20_000;

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

struct Noeul {
    sb: TempDir,
    exe: PathBuf,
    endpoint: String,
    synthetic: bool,
}

impl Noeul {
    /// A profile whose global actions are disabled before anything starts (nothing is ever
    /// registered); `color.format-next` also answers to `K`.
    fn new(tag: &str, exe: PathBuf, synthetic: bool) -> Noeul {
        let sb = TempDir::new(tag);
        let cfg = sb.path().join("Roaming").join("mouv").join("noeul");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(sb.path().join("Local")).unwrap();
        std::fs::create_dir_all(sb.path().join("Temp")).unwrap();
        std::fs::create_dir_all(sb.path().join("clipboard")).unwrap();
        std::fs::write(
            cfg.join("hotkeys.json"),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.region", "disabled": true},
                    {"action": "paste.float", "disabled": true},
                    {"action": "color.format-next", "bindings": ["Shift+C", "K"]}]}))
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

    fn clipboard_dir(&self) -> PathBuf {
        self.sb.path().join("clipboard")
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
            .env("NOEUL_DEV_CLIPBOARD_DIR", self.clipboard_dir())
            .env_remove("NOEUL_DEV_CLIPBOARD")
            .env_remove("NOEUL_DEV_DETECT_PIDS")
            .env_remove("KIM_NO_HOTKEYS")
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

    fn ok(&self, args: &[&str]) -> Value {
        let (code, doc) = self.json(args);
        assert_eq!(code, 0, "{args:?}: {doc:#}");
        doc["data"].clone()
    }

    #[cfg(windows)]
    fn status(&self) -> Value {
        let st = self.ok(&["status"]);
        valid("noeul-ipc-v1#status_result", &st);
        st
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

    /// What Noeul copied last to the stand-in clipboard.
    #[cfg(windows)]
    fn copied(&self) -> String {
        std::fs::read_to_string(self.clipboard_dir().join("CF_UNICODETEXT"))
            .expect("the stand-in clipboard holds text")
    }

    /// `noeul pin --clipboard` (F3's twin): the kind it pinned.
    #[cfg(windows)]
    fn paste(&self) -> Value {
        let r = self.ok(&["pin", "--clipboard", "--at", "-19990,-19990"]);
        valid("noeul-ipc-v1#pin_create_result", &r);
        r["kind"].clone()
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
fn a_the_picker_keys_and_settings_are_discoverable_headlessly() {
    let n = Noeul::new("s59a", bin("noeul"), false);
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
    assert_eq!(keys("color.copy"), ["C"]);
    assert_eq!(keys("color.format-next"), ["Shift+C", "K"]);
    let show = n.ok(&["config", "show"]);
    assert_eq!(
        show["settings"]["color"],
        json!({"format": "hex", "hex_case": "upper", "alpha": "off"})
    );
    let config = n.sb.path().join("Roaming/mouv/noeul/config.toml");
    let (code, dry) = n.json(&["config", "set", "color.format", "hsl", "--dry-run"]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
    assert!(!config.exists(), "a dry run writes nothing");
    let (code, doc) = n.json(&["config", "set", "color.format", "cmyk"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    n.ok(&["config", "set", "color.hex_case", "lower"]);
    assert_eq!(
        n.ok(&["config", "show"])["settings"]["color"]["hex_case"],
        "lower"
    );
    n.ok(&["config", "reset", "color", "--yes"]);
    assert_eq!(
        n.ok(&["config", "show"])["settings"]["color"]["hex_case"],
        "upper"
    );
    let (code, dry) = n.json(&["hotkeys", "set", "color.copy", "X", "--dry-run"]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
}

#[cfg(windows)]
mod win {
    pub use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowThreadProcessId, PostMessageW,
    };
    pub use windows_sys::Win32::UI::WindowsAndMessaging::{
        WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    };

    /// The top-level windows of `pid` with window class `class`.
    pub fn windows(pid: u32, class: &str) -> Vec<HWND> {
        unsafe extern "system" fn each(h: HWND, data: isize) -> i32 {
            // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
            unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
            1
        }
        let mut all: Vec<isize> = Vec::new();
        // SAFETY: the callback only touches `all`, passed by pointer for the duration.
        unsafe { EnumWindows(Some(each), (&raw mut all) as isize) };
        all.into_iter()
            .map(|h| h as HWND)
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

    /// Posts a pointer-free message to a window of the instance this scenario started.
    pub fn post(h: HWND, msg: u32, wp: usize, lp: isize) -> bool {
        // SAFETY: a pointer-free message to a window this scenario's own child process owns.
        unsafe { PostMessageW(h, msg, wp, lp) != 0 }
    }
}

#[cfg(windows)]
fn lp(x: i32, y: i32) -> isize {
    (((y - Y0) as u16 as u32) << 16 | ((x - X0) as u16 as u32)) as isize
}

/// The synthetic desktop's pattern (`noeul::capture::Synthetic::pixel`) as `(r, g, b)`.
#[cfg(windows)]
fn known(x: i32, y: i32) -> (u8, u8, u8) {
    (((x >> 8) ^ (y >> 8)) as u8 | 0x40, y as u8, x as u8)
}

#[cfg(windows)]
#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_the_user_picks_known_pixels_and_pins_them_as_swatches_off_screen() {
    use win::*;
    let n = Noeul::new("s59b", kim_e2e::bin_debug("noeul"), true);
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
    let open = || -> HWND {
        let (code, doc) = n.json(&["capture", "--interactive"]);
        assert_eq!((code, doc["data"]["opened"].as_bool()), (0, Some(true)));
        windows(pid, "NoeulOverlay")[0]
    };
    let key = |ov: HWND, vk: usize, scancode: isize| {
        assert!(post(ov, WM_KEYDOWN, vk, (scancode << 16) | 1));
    };
    let hover = |ov: HWND, x: i32, y: i32| {
        assert!(post(ov, WM_MOUSEMOVE, 0, lp(x, y)));
        n.eventually("the magnifier is over the point", |s| {
            let o = &s["captures"]["overlay"];
            o["pointer"] == json!({"x": x, "y": y}) && o["magnifier"]["shown"] == true
        });
    };
    let copy_and_end = |ov: HWND| -> String {
        key(ov, 0x43, 0x2E); // C
        n.eventually("the colour was copied and the capture ended", |s| {
            s["captures"]["overlay"].is_null()
                && s["last_action"] == json!({"action": "color.copy", "result": "copied"})
        });
        n.copied()
    };
    // HEX of a known pixel on the 100 % primary, pinned back as a swatch.
    let ov = open();
    let (x, y) = (X0 + 200, Y0 + 100);
    hover(ov, x, y);
    let (r, g, b) = known(x, y);
    let hex = format!("#{r:02X}{g:02X}{b:02X}");
    assert_eq!(copy_and_end(ov), hex);
    let st = n.status();
    assert!(!st.to_string().contains(&hex[1..]), "status never names it");
    assert_eq!(st["captures"]["colors"], 1);
    assert_eq!(n.paste(), "color");
    // RGB and HSL on the 150 % monitor (K = color.format-next).
    let ov = open();
    let (x, y) = (X0 + 700, Y0 + 60);
    hover(ov, x, y);
    key(ov, 0x4B, 0x25); // K
    n.eventually("RGB", |s| {
        s["captures"]["overlay"]["color"]["format"] == "rgb"
    });
    let (r, g, b) = known(x, y);
    assert_eq!(copy_and_end(ov), format!("rgb({r}, {g}, {b})"));
    assert_eq!(n.paste(), "color");
    let ov = open();
    hover(ov, x, y);
    key(ov, 0x4B, 0x25); // K: RGB (remembered) → HSL
    n.eventually("HSL", |s| {
        s["captures"]["overlay"]["color"]["format"] == "hsl"
    });
    let hsl = copy_and_end(ov);
    assert!(hsl.starts_with("hsl("), "{hsl}");
    assert_eq!(n.paste(), "color");
    // Next to a selection, C copies and the selection stays; Esc cancels.
    let ov = open();
    assert!(post(ov, WM_MOUSEMOVE, 0, lp(X0 + 20, Y0 + 20)));
    assert!(post(ov, WM_LBUTTONDOWN, 1, lp(X0 + 20, Y0 + 20)));
    assert!(post(ov, WM_MOUSEMOVE, 1, lp(X0 + 120, Y0 + 90)));
    assert!(post(ov, WM_LBUTTONUP, 0, lp(X0 + 120, Y0 + 90)));
    let sel = json!({"x": X0 + 20, "y": Y0 + 20, "width": 101, "height": 71});
    n.eventually("selected", |s| s["captures"]["overlay"]["selection"] == sel);
    hover(ov, X0 + 300, Y0 + 300);
    key(ov, 0x43, 0x2E); // C
    n.eventually("copied, the selection kept", |s| {
        s["last_action"] == json!({"action": "color.copy", "result": "copied"})
            && s["captures"]["overlay"]["selection"] == sel
    });
    key(ov, 0x1B, 0x01); // Esc
    n.eventually("cancelled", |s| s["captures"]["overlay"].is_null());
    let list = n.ok(&["pin", "list"]);
    assert_eq!(list["pins"].as_array().unwrap().len(), 3);
    assert!(
        std::fs::read_dir(n.sb.path().join("Temp")).unwrap().count() == 0,
        "no temporary file"
    );
}
