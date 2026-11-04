//! E2E — Step 53: Noeul's floating pin engine (docs/adr/0029-noeul-pin-engine.md).
//! Requirements: NOEUL-FR-060, NOEUL-FR-061, NOEUL-FR-065, NOEUL-FR-067, NOEUL-FR-110…112,
//! NOEUL-FR-114, NOEUL-SEC-002, NOEUL-SEC-005, NOEUL-SEC-013.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the pin
//! commands and keys headlessly — `pin`, `pin.hide`, `pin.show` in `capabilities`, the pin zoom,
//! hide and destroy keys in `hotkeys list` (and nothing planned) — and the pin commands fail
//! honestly without an instance. Nothing is started or written.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the user's
//! scripts pin three regions, list them, hide and show one, close one while the others stay,
//! reopen it, ask to bring lost pins on screen (none are lost), and quit; a new instance has no
//! pins (pins live in memory only). Every pin lies on a synthetic desktop far off-screen, is never
//! topmost or focused, the screen and the clipboard are never read, and nothing reaches the disk.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized,
//! and no hotkey is registered at all (both global actions are disabled in the profile before
//! anything starts). Every process started here is ended.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The synthetic desktop's primary monitor starts here (`noeul::capture::OFF_SCREEN`).
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

struct Noeul {
    sb: TempDir,
    exe: PathBuf,
    endpoint: String,
    synthetic: bool,
}

impl Noeul {
    /// A profile whose global actions are disabled before anything starts: nothing is ever
    /// registered, while the pins' own keys stay on.
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

    fn list(&self) -> Value {
        let (code, l) = self.json(&["pin", "list"]);
        assert_eq!(code, 0, "{l:#}");
        valid("noeul-ipc-v1#pin_list_result", &l["data"]);
        l["data"].clone()
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
        let out = self.raw(&["status"]);
        let pid = serde_json::from_slice::<Value>(&out.stdout)
            .ok()
            .and_then(|v| v["data"]["instance"]["pid"].as_u64());
        let _ = self.raw(&["quit", "--yes"]);
        std::thread::sleep(std::time::Duration::from_millis(500));
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

fn pin<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["pins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap_or_else(|| panic!("no {id}: {list:#}"))
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_the_pin_engine_is_discoverable_headlessly() {
    let n = Noeul::new("s53a", bin("noeul"), false);
    let before = n.sb.entry_count();
    let help = String::from_utf8_lossy(&n.raw(&["pin", "--help"]).stdout).to_string();
    for word in ["--clipboard", "hide", "show", "close", "reopen"] {
        assert!(help.contains(word), "pin --help mentions {word}:\n{help}");
    }
    let (_, caps) = n.json(&["capabilities"]);
    valid("tool-capabilities-v1", &caps["data"]);
    let names: Vec<&str> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in [
        "pin",
        "pin.list",
        "pin.hide",
        "pin.show",
        "pin.close",
        "pin.reopen",
    ] {
        assert!(names.contains(&name), "{name} is listed: {names:?}");
    }
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let ids: Vec<&str> = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    for id in [
        "pin.close",
        "pin.destroy",
        "pin.zoom-in",
        "pin.zoom-out",
        "pin.zoom-reset",
        "pin.hide",
    ] {
        assert!(ids.contains(&id), "{id} is listed: {ids:?}");
    }
    // The transforms shipped in Step 54; a text card's copy-text is still planned (Step 55).
    assert!(
        !ids.contains(&"pin.copy-text"),
        "pin.copy-text is not in this build"
    );
    for args in [
        &["pin", "hide", "--all"][..],
        &["pin", "show", "--all", "--on-screen"],
        &["pin", "--clipboard", "--dry-run"],
    ] {
        let (code, doc) = n.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    let (code, doc) = n.json(&["pin"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    assert_eq!(
        n.sb.entry_count(),
        before,
        "headless commands wrote nothing"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_scripts_drive_independent_off_screen_pins_and_a_restart_restores_nothing() {
    let n = Noeul::new("s53b", bin_debug("noeul"), true);
    n.start();
    let st = n.status();
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug test hook is on"
    );
    assert_eq!(
        st["hotkeys"]["registrations"],
        json!([]),
        "nothing registered"
    );
    let regions = [
        (X0 + 10, Y0 + 10, 100, 60),
        (X0 + 150, Y0 + 10, 80, 50),
        (X0 + 300, Y0 + 10, 120, 90),
    ];
    for (x, y, w, h) in regions {
        let r = format!("{x},{y},{w},{h}");
        let (code, doc) = n.json(&["capture", "--region", &r, "--pin"]);
        assert_eq!(code, 0, "{doc:#}");
    }
    let l = n.list();
    assert_eq!(l["pins"].as_array().unwrap().len(), 3);
    for (i, (x, y, w, h)) in regions.iter().enumerate() {
        let p = pin(&l, &format!("pin-{}", i + 1));
        assert_eq!(p["rect"], json!({"x": x, "y": y, "width": w, "height": h}));
        assert_eq!(
            (p["zoom"].as_u64(), p["visible"].as_bool()),
            (Some(100), Some(true))
        );
    }
    let (code, h) = n.json(&["pin", "hide", "pin-2"]);
    assert_eq!((code, h["data"]["hidden"].clone()), (0, json!(["pin-2"])));
    assert_eq!(n.status()["pins"]["hidden"], 1);
    let (code, s) = n.json(&["pin", "show", "--all", "--on-screen"]);
    assert_eq!(code, 0, "{s:#}");
    valid("noeul-ipc-v1#pin_show_result", &s["data"]);
    assert_eq!(
        (s["data"]["shown"].clone(), s["data"]["moved"].clone()),
        (json!(["pin-2"]), json!([]))
    );
    let (code, c) = n.json(&["pin", "close", "pin-1", "--yes"]);
    assert_eq!((code, c["data"]["closed"].clone()), (0, json!(["pin-1"])));
    let l = n.list();
    assert_eq!(l["pins"].as_array().unwrap().len(), 2, "the others stay");
    assert_eq!(pin(&l, "pin-3")["rect"]["width"], 120);
    let (code, re) = n.json(&["pin", "reopen"]);
    assert_eq!((code, re["data"]["pin"].as_str()), (0, Some("pin-1")));
    let (code, dry) = n.json(&["pin", "--clipboard", "--dry-run"]);
    assert_eq!(
        (code, dry["data"]["pin"].clone()),
        (0, Value::Null),
        "nothing read"
    );
    let (code, q) = n.json(&["quit", "--yes"]);
    assert_eq!((code, q["data"]["closed_pins"].as_u64()), (0, Some(3)));

    n.start();
    assert_eq!(
        n.list(),
        json!({"pins": [], "reopenable": null, "group": "default"}),
        "pins live in memory only"
    );
    assert_eq!(n.files("Temp"), 0, "no temporary files");
    assert_eq!(n.files("Local"), 0, "no pin reached the disk");
    let (code, _) = n.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
