//! E2E — Step 54: Noeul's pin transforms and advanced interaction controls
//! (docs/adr/0044-noeul-pin-transforms-and-control.md).
//! Requirements: NOEUL-FR-061, NOEUL-FR-062, NOEUL-FR-063, NOEUL-FR-064, NOEUL-FR-068,
//! NOEUL-FR-100, NOEUL-FR-112, NOEUL-FR-114, NOEUL-NFR-007, NOEUL-SEC-013.
//!
//! Scenario A (release `noeul`, isolated profile, nothing running): an agent discovers the new
//! commands and keys headlessly — `pin.set`, `pin.group.list` and `pin.group.switch` in
//! `capabilities`; the rotate, flip, opacity, thumbnail, click-through and new-group keys in the
//! `pin` context of `hotkeys list`; the global `pins.clickable` and `pins.toggle-hidden` actions
//! with no default keys (the user binds them, HOTKEY_CONTRACT §7.5); the `[pins]` border and shadow
//! settings — and the commands fail honestly without an instance. Nothing is started or written
//! except the one setting changed on purpose.
//!
//! Scenario B (debug `noeul` with the test hook `NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the user's
//! scripts pin regions, turn, flip, fade, shrink and place them precisely and undo every change,
//! let one pin pass clicks through and bring it back from the CLI and from a second start,
//! organise pins in groups (switch, show, hide, close a group with `--yes`), hide them all, and
//! quit. Every pin lies on a synthetic desktop far off-screen, is never topmost or focused, the
//! screen and the clipboard are never read, and nothing reaches the disk.
//!
//! The machine is in use while e2e runs: no tray icon, no visible window, no input synthesized,
//! and no hotkey is registered at all (both default global actions are disabled in the profile
//! before anything starts; the new global actions have no keys). Every process started here is
//! ended.

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

    fn ok(&self, args: &[&str]) -> Value {
        let (code, doc) = self.json(args);
        assert_eq!(code, 0, "{args:?}: {doc:#}");
        doc["data"].clone()
    }

    fn list(&self) -> Value {
        let l = self.ok(&["pin", "list"]);
        valid("noeul-ipc-v1#pin_list_result", &l);
        l
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
fn a_transforms_groups_and_recovery_are_discoverable_headlessly() {
    let n = Noeul::new("s54a", bin("noeul"), false);
    let help = String::from_utf8_lossy(&n.raw(&["pin", "set", "--help"]).stdout).to_string();
    for word in [
        "--rotate",
        "--flip",
        "--opacity",
        "--thumbnail",
        "--click-through",
        "--at",
        "--move-to-group",
        "--dry-run",
    ] {
        assert!(
            help.contains(word),
            "pin set --help mentions {word}:\n{help}"
        );
    }
    let (_, caps) = n.json(&["capabilities"]);
    valid("tool-capabilities-v1", &caps["data"]);
    let names: Vec<&str> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in ["pin.set", "pin.group.list", "pin.group.switch"] {
        assert!(names.contains(&name), "{name} is listed: {names:?}");
    }
    let (_, list) = n.json(&["hotkeys", "list"]);
    valid("hotkeys-cli-v1", &list);
    let actions = list["data"]["actions"].as_array().unwrap();
    let keys_of = |id: &str| -> Vec<String> {
        let a = actions
            .iter()
            .find(|a| a["id"] == id)
            .unwrap_or_else(|| panic!("{id} is listed"));
        a["bindings"]
            .as_array()
            .map(|b| {
                b.iter()
                    .filter_map(|k| k["keys"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    for (id, key) in [
        ("pin.rotate-cw", "1"),
        ("pin.rotate-ccw", "2"),
        ("pin.flip-horizontal", "3"),
        ("pin.flip-vertical", "4"),
        ("pin.opacity-up", "Ctrl+Plus"),
        ("pin.opacity-down", "Ctrl+Minus"),
        ("pin.thumbnail", "T"),
        ("pin.click-through", "Ctrl+Shift+T"),
    ] {
        assert!(
            keys_of(id).iter().any(|k| k == key),
            "{id} on {key}: {:?}",
            keys_of(id)
        );
    }
    for id in ["pins.clickable", "pins.toggle-hidden", "pin.group-new"] {
        assert!(keys_of(id).is_empty(), "{id} ships without default keys");
    }
    for args in [
        &["pin", "set", "--all", "--click-through", "off"][..],
        &["pin", "group", "list"],
        &["pin", "group", "switch", "work"],
    ] {
        let (code, doc) = n.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    let (code, doc) = n.json(&["pin", "set", "pin-1"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (2, Some("usage")),
        "nothing to change"
    );
    let (code, doc) = n.json(&["pin", "set", "pin-1", "--opacity", "5"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    // The look is a setting: a dry run writes nothing, a set writes the one key.
    let before = n.files("Roaming");
    let dry = n.ok(&["config", "set", "pins.shadow", "off", "--dry-run"]);
    assert_eq!(dry["dry_run"], true, "{dry:#}");
    assert_eq!(n.files("Roaming"), before, "a dry run wrote nothing");
    n.ok(&["config", "set", "pins.shadow", "off"]);
    let shown = n.ok(&["config", "show"]);
    assert_eq!(
        shown["settings"]["pins"],
        json!({"border": "on", "shadow": "off"})
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn b_scripts_transform_recover_and_group_off_screen_pins() {
    let n = Noeul::new("s54b", bin_debug("noeul"), true);
    let (code, start) = n.json(&["start", "--no-tray"]);
    assert_eq!(code, 0, "{start:#}");
    let st = n.ok(&["status"]);
    valid("noeul-ipc-v1#status_result", &st);
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
        (X0 + 10, Y0 + 10, 120, 80),
        (X0 + 200, Y0 + 10, 60, 60),
        (X0 + 300, Y0 + 10, 90, 40),
    ];
    for (x, y, w, h) in regions {
        n.ok(&["capture", "--region", &format!("{x},{y},{w},{h}"), "--pin"]);
    }
    let start_list = n.list();

    // Transform pin-1 every way, then undo every change.
    let r = n.ok(&[
        "pin",
        "set",
        "pin-1",
        "--rotate",
        "cw",
        "--flip",
        "horizontal",
        "--opacity",
        "40",
        "--zoom",
        "150",
    ]);
    valid("noeul-ipc-v1#pin_set_result", &r);
    let p = &r["pins"][0];
    assert_eq!(p["orientation"], json!({"rotation": 270, "mirrored": true}));
    assert_eq!(
        (p["opacity"].as_u64(), p["zoom"].as_u64()),
        (Some(40), Some(150))
    );
    assert_eq!(
        (p["rect"]["width"].as_u64(), p["rect"]["height"].as_u64()),
        (Some(120), Some(180)),
        "turned, then 150 %"
    );
    assert_eq!(
        pin(&n.list(), "pin-2"),
        pin(&start_list, "pin-2"),
        "the other pins are untouched"
    );
    n.ok(&["pin", "set", "pin-1", "--thumbnail", "on"]);
    assert_eq!(pin(&n.list(), "pin-1")["thumbnail"], true);
    // Undo it all at once: the absolute orientation as captured, full opacity, 100 %.
    n.ok(&[
        "pin",
        "set",
        "pin-1",
        "--thumbnail",
        "off",
        "--rotation",
        "0",
        "--mirrored",
        "off",
        "--opacity",
        "100",
        "--zoom",
        "100",
    ]);
    let back = n.list();
    assert_eq!(
        pin(&back, "pin-1")["orientation"],
        json!({"rotation": 0, "mirrored": false})
    );
    // A centre-anchored turn and its inverse land exactly where the pin was; the zoom was
    // anchored at the corner, so the pin is placed back precisely.
    let r0 = &pin(&start_list, "pin-1")["rect"];
    let at = format!("{},{}", r0["x"], r0["y"]);
    n.ok(&["pin", "set", "pin-1", "--at", &at]);
    let mut now = n.list();
    let mut was = start_list.clone();
    for l in [&mut now, &mut was] {
        for p in l["pins"].as_array_mut().unwrap() {
            p["created_at"] = Value::Null;
        }
    }
    assert_eq!(now, was, "every change undone");

    // Click-through and its ways back: the CLI, then a second start.
    n.ok(&["pin", "set", "pin-2", "--click-through", "on"]);
    assert_eq!(n.ok(&["status"])["pins"]["click_through"], 1);
    n.ok(&["pin", "set", "--all", "--click-through", "off"]);
    assert_eq!(pin(&n.list(), "pin-2")["click_through"], false);
    n.ok(&["pin", "set", "pin-2", "--click-through", "on"]);
    let again = n.ok(&["start", "--no-tray"]);
    assert_eq!(again["already_running"], true, "{again:#}");
    assert_eq!(again["activate"]["clickable"], json!(["pin-2"]));
    assert_eq!(pin(&n.list(), "pin-2")["click_through"], false);

    // Groups.
    n.ok(&["pin", "set", "pin-3", "--move-to-group", "research"]);
    let g = n.ok(&["pin", "group", "list"]);
    valid("noeul-ipc-v1#group_list_result", &g);
    let s = n.ok(&["pin", "group", "switch", "research"]);
    assert_eq!(s["hidden"], json!(["pin-1", "pin-2"]));
    n.ok(&[
        "capture",
        "--region",
        &format!("{},{},30,30", X0 + 450, Y0 + 10),
        "--pin",
    ]);
    assert_eq!(pin(&n.list(), "pin-4")["group"], "research");
    let r = n.ok(&["pin", "show", "--group", "default"]);
    assert_eq!(r["shown"], json!(["pin-1", "pin-2"]));
    let (code, doc) = n.json(&["pin", "close", "--group", "research"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let r = n.ok(&["pin", "close", "--group", "research", "--yes"]);
    assert_eq!(r["closed"], json!(["pin-3", "pin-4"]));

    // Hide them all; status says so; nothing reached the disk.
    n.ok(&["pin", "hide", "--all"]);
    let st = n.ok(&["status"]);
    assert_eq!(
        (st["pins"]["open"].as_u64(), st["pins"]["hidden"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(n.files("Temp"), 0, "no temporary files");
    assert_eq!(n.files("Local"), 0, "no pin reached the disk");
    let (code, q) = n.json(&["quit", "--yes"]);
    assert_eq!((code, q["data"]["closed_pins"].as_u64()), (0, Some(2)));
}
