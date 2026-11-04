//! E2E — Step 58: Noeul on multi-monitor, high-DPI and HDR desktops
//! (docs/adr/0050-noeul-displays-and-capture-backends.md).
//! Requirements: NOEUL-FR-016, NOEUL-FR-017, NOEUL-FR-067, NOEUL-FR-090, NOEUL-FR-091,
//! NOEUL-FR-092, NOEUL-FR-093, NOEUL-FR-112, NOEUL-FR-114, NOEUL-SEC-004.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers monitor and
//! window capture headlessly — `--monitor`, `--window active` and `--pointer` in `capture --help`
//! and `capabilities`; the global `capture.monitor` and `capture.window` actions with no default
//! keys in `hotkeys list`; the `[capture] pointer` setting; each monitor's scale, rotation and
//! HDR state in `status` — and the capture commands fail honestly without an instance. Nothing
//! is started, captured or written except the one setting changed on purpose.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1` and a layout file
//! `NOEUL_DEV_SYNTHETIC_LAYOUT`): a script captures a whole 150 % monitor at negative coordinates,
//! the 125 % portrait HDR monitor with the pointer asked for, and a region across a monitor seam
//! and a gap of the virtual desktop; then the layout file changes (a monitor is unplugged, the
//! primary goes to 150 %) and a burst of display notifications is handled once; the lost pin is
//! brought back; the next capture covers the new layout; quit. Every pin lies on a synthetic
//! desktop far off-screen, is never topmost or focused; the screen, the real displays and the
//! clipboard are never read or changed; nothing reaches the disk.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized,
//! no display setting changed, and no hotkey is registered at all (both default global actions are
//! disabled in the profile; the new global actions have no keys). Every process started here is
//! ended.
#![allow(unsafe_code)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The synthetic desktop's origin (`noeul::capture::OFF_SCREEN`).
const X0: i64 = -20_000;
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

/// The Win32 calls the scenario needs to find Noeul's own control window and post it the display
/// notifications Windows would send (never a real display change).
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
    }

    pub const WM_DISPLAYCHANGE: u32 = 0x007E;

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
}

/// A 100 % primary, a 150 % monitor to its left, a 125 % portrait HDR monitor to its right that
/// reaches higher: positions relative to the synthetic origin.
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

struct Noeul {
    sb: TempDir,
    exe: PathBuf,
    endpoint: String,
    synthetic: bool,
}

impl Noeul {
    /// A profile whose global actions are disabled before anything starts: nothing is ever
    /// registered.
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
                    {"action": "paste.float", "disabled": true}]}))
            .unwrap(),
        )
        .unwrap();
        let n = Noeul {
            endpoint: format!("noeul-e2e-{tag}-{}", std::process::id()),
            sb,
            exe,
            synthetic,
        };
        n.write_layout(&layout());
        n
    }

    fn write_layout(&self, v: &Value) {
        let tmp = self.sb.path().join("layout.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap()).unwrap();
        std::fs::rename(&tmp, self.sb.path().join("layout.json")).unwrap();
    }

    fn raw(&self, args: &[&str]) -> std::process::Output {
        let p = self.sb.path();
        let mut c = Command::new(&self.exe);
        c.args(args)
            .args(["--endpoint", &self.endpoint])
            .env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env("TEMP", p.join("Temp"))
            .env("TMP", p.join("Temp"))
            .env("KIM_NO_INPUT", "1")
            .env_remove("KIM_NO_HOTKEYS")
            .env_remove("NOEUL_DEV_ACTIVE_WINDOW")
            .env_remove("NOEUL_DEV_DETECT_PIDS")
            .stdin(Stdio::null());
        if self.synthetic {
            c.env("NOEUL_DEV_SYNTHETIC_DESKTOP", "1")
                .env("NOEUL_DEV_SYNTHETIC_LAYOUT", p.join("layout.json"));
        } else {
            c.env_remove("NOEUL_DEV_SYNTHETIC_DESKTOP")
                .env_remove("NOEUL_DEV_SYNTHETIC_LAYOUT");
        }
        c.output().unwrap()
    }

    /// `noeul <args> --json`, validated against cli-envelope-v1; the exit code matches.
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut full = args.to_vec();
        full.push("--json");
        let out = self.raw(&full);
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

    /// `capture …`, checked against the IPC schema's capture result.
    fn capture(&self, args: &[&str]) -> Value {
        let mut full = vec!["capture"];
        full.extend_from_slice(args);
        let (code, doc) = self.json(&full);
        assert_eq!(code, 0, "{args:?}: {doc:#}");
        valid("noeul-ipc-v1#capture_result", &doc["data"]);
        assert_eq!(doc["warnings"], doc["data"]["warnings"]);
        doc["data"].clone()
    }

    fn files(&self, sub: &str) -> usize {
        fn count(dir: &Path) -> usize {
            std::fs::read_dir(dir).map_or(0, |rd| {
                rd.flatten()
                    .map(|e| {
                        if e.path().is_dir() {
                            count(&e.path())
                        } else {
                            1
                        }
                    })
                    .sum()
            })
        }
        count(&self.sb.path().join(sub))
    }
}

impl Drop for Noeul {
    fn drop(&mut self) {
        let out = self.raw(&["status", "--json"]);
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

fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_monitor_and_window_capture_are_discoverable_headlessly() {
    let n = Noeul::new("s58a", bin("noeul"), false);
    let help = String::from_utf8_lossy(&n.raw(&["capture", "--help"]).stdout).to_string();
    for word in [
        "--monitor",
        "--window",
        "--pointer",
        "--region",
        "--pin",
        "--dry-run",
    ] {
        assert!(
            help.contains(word),
            "capture --help mentions {word}:\n{help}"
        );
    }
    let caps = n.ok(&["capabilities"]);
    valid("tool-capabilities-v1", &caps);
    let capture = caps["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "capture")
        .unwrap();
    for word in ["--monitor", "--window active", "--pointer"] {
        assert!(
            capture["usage"].as_str().unwrap().contains(word),
            "{capture:#}"
        );
    }
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    for id in ["capture.monitor", "capture.window"] {
        let a = list["data"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed"));
        assert!(
            a["bindings"].as_array().is_none_or(Vec::is_empty),
            "{id} ships without a default key: {a:#}"
        );
    }
    // The headless status reports the real monitors' scale, rotation and HDR state (geometry
    // only), and starts nothing.
    let st = n.ok(&["status"]);
    assert_eq!(st["running"], false);
    for m in st["desktop"]["monitors"].as_array().into_iter().flatten() {
        assert!(
            [0, 90, 180, 270].contains(&m["rotation"].as_u64().unwrap()),
            "{m:#}"
        );
        assert!(m["hdr"].is_boolean() || m["hdr"].is_null(), "{m:#}");
    }
    // Without an instance every capture fails honestly and changes nothing.
    for args in [
        &["capture", "--monitor", "1", "--pin"][..],
        &["capture", "--window", "active", "--pin"],
        &[
            "capture",
            "--monitor",
            "primary",
            "--pin",
            "--pointer",
            "--dry-run",
        ],
    ] {
        let (code, doc) = n.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}: {doc:#}"
        );
    }
    let (code, doc) = n.json(&["capture", "--monitor", "first", "--pin"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    // The pointer setting for the keys and F1.
    let set = n.ok(&["config", "set", "capture.pointer", "on"]);
    assert_eq!(set["written"], true, "{set:#}");
    let shown = n.ok(&["config", "show"]);
    assert_eq!(shown["settings"]["pointer"], "on", "{shown:#}");
    assert_eq!(n.files("Local"), 0, "nothing in the data folder");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_monitors_attach_detach_and_rescale_under_a_running_noeul() {
    let n = Noeul::new("s58b", bin_debug("noeul"), true);
    let (code, start) = n.json(&["start", "--no-tray"]);
    assert_eq!(code, 0, "{start:#}");
    let st = n.ok(&["status"]);
    valid("noeul-ipc-v1#status_result", &st);
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug test hook is on"
    );
    assert_eq!(st["desktop"]["monitors"].as_array().unwrap().len(), 3);
    let pid = u32::try_from(st["instance"]["pid"].as_u64().unwrap()).unwrap();

    // A whole 150 % monitor at negative coordinates: native pixels, its logical size.
    let m = n.capture(&["--monitor", "2", "--pin"]);
    assert_eq!(
        m["region"],
        json!({"x": X0 - 480, "y": Y0 + 50, "width": 480, "height": 300})
    );
    assert_eq!(
        m["mapping"]["logical"],
        json!({"x": 0, "y": 0, "width": 320, "height": 200})
    );
    // The portrait HDR monitor with the pointer asked for (it is on the primary): both said.
    let p = n.capture(&["--monitor", "3", "--pin", "--pointer"]);
    assert_eq!(p["monitor"]["rotation"], 90);
    assert_eq!(p["pointer"]["reason"], "outside");
    assert!(
        p["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("HDR")),
        "{p:#}"
    );
    // A region across the seam and a gap, with the pointer drawn in.
    let r = n.capture(&[
        "--region",
        &format!("{},{},100,60", X0 - 40, Y0 + 10),
        "--pin",
        "--pointer",
    ]);
    assert_eq!(r["pointer"]["drawn"], true);
    assert_eq!(r["mapping"]["mixed_dpi"], true);
    // The active window: the test hook never looks at the user's; none is named here.
    let (code, doc) = n.json(&["capture", "--window", "active", "--pin"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("window_not_found"))
    );

    // The left monitor is unplugged and the primary goes to 150 %: a burst of notifications is
    // handled once, the pins stay where they were.
    let mut next = layout();
    next["monitors"] = json!([
        {"x": 0, "y": 0, "width": 640, "height": 400, "primary": true, "dpi": 144},
        layout()["monitors"][2].clone()
    ]);
    n.write_layout(&next);
    #[cfg(windows)]
    {
        let control = win::windows(pid, "NoeulControl")[0];
        for _ in 0..5 {
            assert!(win::post(control, win::WM_DISPLAYCHANGE, 32, 0));
        }
    }
    eventually("the display change was handled", || {
        n.ok(&["status"])["displays"]["handled"] == 1
    });
    let st = n.ok(&["status"]);
    assert_eq!(
        st["displays"],
        json!({"changes": 5, "handled": 1, "pending": false})
    );
    assert_eq!(st["pins"]["off_screen"], 1);
    let shown = n.ok(&["pin", "show", "--all", "--on-screen"]);
    assert_eq!(shown["moved"], json!(["pin-1"]));
    assert_eq!(n.ok(&["status"])["pins"]["off_screen"], 0);
    // The next capture covers the new layout.
    let m = n.capture(&["--monitor", "primary", "--pin", "--dry-run"]);
    assert_eq!(m["mapping"]["scale_percent"], 150);
    assert_eq!(
        m["mapping"]["logical"],
        json!({"x": 0, "y": 0, "width": 427, "height": 267})
    );
    let quit = n.ok(&["quit", "--yes"]);
    assert_eq!(quit["closed_pins"], 3, "{quit:#}");
    assert_eq!(n.files("Local"), 0, "nothing reached the disk");
}
