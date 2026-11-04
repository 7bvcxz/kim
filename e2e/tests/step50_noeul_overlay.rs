//! E2E — Step 50: Noeul's F1 region selection overlay (docs/adr/0026-noeul-capture-overlay.md).
//! Requirements: NOEUL-FR-010, NOEUL-FR-013, NOEUL-FR-100, NOEUL-FR-110…112, NOEUL-FR-114,
//! NOEUL-SEC-003, NOEUL-SEC-005, KIM-SEC-020.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the new
//! action and command headlessly — `capture.region` and the overlay keys in `hotkeys list`,
//! `capture --interactive`/`--cancel` in `capabilities` — and the instance commands fail
//! honestly without an instance. Nothing is started or written.
//!
//! Scenario B (release `noeul`): the user starts Noeul with `capture.region` on an obscure chord;
//! the chord is registered; a dry-run overlay request checks the desktop and captures nothing.
//! A second Noeul in another profile wants the same chord: it is reported `taken` with offers,
//! and the first keeps it (never stolen).
//!
//! Scenario C (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): ten overlay
//! requests at once open exactly one overlay — on a synthetic desktop far off-screen, never
//! focused, never on top, the screen never read — and `capture --cancel` leaves nothing in
//! memory, in the profile or in TEMP.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no real overlay, nothing
//! captured from the screen, no input synthesized, and only `Ctrl+Alt+Shift+F21`/`F22` are ever
//! registered (never F1 or F3). Every process started here is ended.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CAPTURE_KEY: &str = "Ctrl+Alt+Shift+F21";
const PASTE_KEY: &str = "Ctrl+Alt+Shift+F22";

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
    /// A profile whose shortcuts are written before anything starts: `capture.region` and
    /// `paste.float` on the obscure chords, never their defaults.
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
                    {"action": "capture.region", "bindings": [CAPTURE_KEY]},
                    {"action": "paste.float", "bindings": [PASTE_KEY]}]}))
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

fn registration(st: &Value, action: &str) -> Value {
    st["hotkeys"]["registrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["action"] == action)
        .cloned()
        .unwrap_or_else(|| panic!("no registration for {action}"))
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_the_overlay_is_discoverable_headlessly() {
    let n = Noeul::new("s50a", bin("noeul"), false);
    let before = n.sb.entry_count();
    let help = String::from_utf8_lossy(&n.raw(&["--help"]).stdout).to_string();
    assert!(
        help.contains("F1 opens the region selection overlay"),
        "{help}"
    );
    let (_, caps) = n.json(&["capabilities"]);
    valid("tool-capabilities-v1", &caps["data"]);
    let capture = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "capture")
        .cloned()
        .unwrap();
    assert!(capture["usage"].as_str().unwrap().contains("--interactive"));
    assert_eq!(capture["opens_window"], true);
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let ids: Vec<&str> = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    for id in [
        "capture.region",
        "capture.pin",
        "capture.cancel",
        "selection.full",
    ] {
        assert!(ids.contains(&id), "{id} is listed: {ids:?}");
    }
    // This scenario used to assert that `selection.mark` was still planned (not listed). Step 52
    // implemented it, and Noeul reports no planned global action any more, so it must be listed
    // like the other overlay actions.
    assert!(
        ids.contains(&"selection.mark"),
        "selection.mark is listed: {ids:?}"
    );
    for args in [
        &["capture", "--interactive"][..],
        &["capture", "--interactive", "--dry-run"],
        &["capture", "--cancel"],
    ] {
        let (code, doc) = n.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    let (code, doc) = n.json(&["capture", "--interactive", "--cancel"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    assert_eq!(
        n.sb.entry_count(),
        before,
        "headless commands wrote nothing"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_the_capture_chord_registers_a_dry_run_captures_nothing_and_a_conflict_is_reported() {
    let n = Noeul::new("s50b", bin("noeul"), false);
    n.start();
    let st = n.status();
    let reg = registration(&st, "capture.region");
    assert_eq!(
        (reg["keys"].as_str(), reg["status"].as_str()),
        (Some(CAPTURE_KEY), Some("registered")),
        "{reg:#}"
    );
    assert_eq!(st["hotkeys"]["planned"], json!([]));
    let desktop = st["desktop"]["state"].as_str().unwrap().to_string();
    let (code, dry) = n.json(&["capture", "--interactive", "--dry-run"]);
    if desktop == "available" {
        assert_eq!(code, 0, "{dry:#}");
        valid("noeul-ipc-v1#capture_region_result", &dry["data"]);
        assert_eq!(
            (
                dry["data"]["dry_run"].as_bool(),
                dry["data"]["opened"].as_bool()
            ),
            (Some(true), Some(false))
        );
    } else {
        assert_eq!(
            dry["error"]["code"], "desktop_unavailable",
            "honest on {desktop}"
        );
    }
    let st = n.status();
    assert_eq!(
        (
            st["captures"]["started"].as_u64(),
            st["captures"]["open"].as_bool()
        ),
        (Some(0), Some(false)),
        "a dry run captures nothing and opens nothing"
    );
    assert_eq!(st["memory"]["bytes"], 0);

    // Another program (a second Noeul in another profile) wants the same chord.
    let other = Noeul::new("s50b2", bin("noeul"), false);
    other.start();
    let st2 = other.status();
    let reg2 = registration(&st2, "capture.region");
    assert_eq!(reg2["status"], "taken", "{reg2:#}");
    assert!(
        reg2["message"]
            .as_str()
            .unwrap()
            .contains("did not take it")
    );
    let conflict = st2["hotkeys"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "taken" && c["action"] == "capture.region")
        .cloned()
        .expect("a taken conflict with offers");
    assert!(!conflict["offers"].as_array().unwrap().is_empty());
    assert_eq!(
        registration(&n.status(), "capture.region")["status"],
        "registered",
        "the first owner keeps it"
    );
    for x in [&other, &n] {
        let (code, q) = x.json(&["quit", "--yes"]);
        assert_eq!((code, q["data"]["exited"].as_bool()), (0, Some(true)));
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn c_rapid_triggers_open_one_off_screen_overlay_and_cancel_leaves_nothing() {
    let n = Noeul::new("s50c", bin_debug("noeul"), true);
    n.start();
    let st = n.status();
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug test hook is on"
    );
    assert!(st["desktop"]["virtual_rect"]["x"].as_i64().unwrap() <= -10_000);
    let results: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..10)
            .map(|_| s.spawn(|| n.json(&["capture", "--interactive"])))
            .collect();
        hs.into_iter()
            .map(|h| {
                let (code, doc) = h.join().unwrap();
                assert_eq!(code, 0, "{doc:#}");
                doc["data"].clone()
            })
            .collect()
    });
    assert_eq!(
        results.iter().filter(|r| r["opened"] == true).count(),
        1,
        "exactly one overlay: {results:#?}"
    );
    let st = n.status();
    assert_eq!(st["captures"]["ignored"], 9);
    assert_eq!(st["captures"]["overlay"]["phase"], "hover");
    assert_eq!(st["captures"]["first_frame"]["count"], 1);
    let (code, c) = n.json(&["capture", "--cancel"]);
    assert_eq!((code, c["data"]["cancelled"].as_bool()), (0, Some(true)));
    let st = n.status();
    assert_eq!(st["captures"]["open"], false);
    assert_eq!(st["memory"]["bytes"], 0, "nothing kept");
    assert_eq!(n.files("Temp"), 0, "no temporary files");
    assert_eq!(n.files("Local"), 0, "nothing written without an output");
    let (code, _) = n.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}
