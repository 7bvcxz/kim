//! The capture overlay through the real `noeul` binary (ADR 0026; NOEUL-FR-010, NOEUL-FR-013,
//! NOEUL-FR-100, NOEUL-SEC-003, NOEUL-SEC-005, KIM-SEC-020).
//!
//! Safety on this shared machine: every instance runs in an isolated profile on a private
//! endpoint with `--no-tray`; its `hotkeys.json` binds `capture.region` and `paste.float` to the
//! obscure chords `Ctrl+Alt+Shift+F21`/`F22` before it starts (the sandbox guard refuses
//! anything else, so `F1` and `F3` are never registered). Overlays are opened only with the
//! debug-build test hook (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the picture is synthetic, the
//! overlay lies far off-screen, it never takes focus or stays on top, and the screen is never
//! read. No input is synthesized. Every process a test starts is ended.
#![cfg(windows)]

mod common;

use common::{Keys, Sandbox, validate};
use kim_hotkeys::Chord;
use kim_hotkeys::register::{Registrar, Status};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Tests that register a chord take turns (the obscure chords are shared by the machine).
static HOTKEYS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hotkey_turn() -> std::sync::MutexGuard<'static, ()> {
    HOTKEYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const CAPTURE_KEY: &str = "Ctrl+Alt+Shift+F21";
const PASTE_KEY: &str = "Ctrl+Alt+Shift+F22";

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    let e = validate("noeul-ipc-v1#status_result", &doc["data"]);
    assert!(e.is_empty(), "status violates the schema: {e:?}\n{doc:#}");
    doc["data"].clone()
}

fn registration(st: &Value, action: &str) -> Value {
    st["hotkeys"]["registrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["action"] == action)
        .cloned()
        .unwrap_or_else(|| panic!("no registration for {action}: {st:#}"))
}

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.extend(files_under(&e.path()));
            } else {
                out.push(e.path().display().to_string());
            }
        }
    }
    out
}

#[test]
fn capture_region_is_registered_on_its_chord_and_a_taken_chord_is_reported_not_stolen() {
    let _turn = hotkey_turn();
    // Another program (this test) owns the capture chord before Noeul starts.
    let other = Registrar::start().unwrap();
    let (id, st) = other.register(&Chord::parse(CAPTURE_KEY).unwrap()).unwrap();
    assert_eq!(
        st,
        Status::Registered,
        "{CAPTURE_KEY} must be free on this machine for the test"
    );
    let mut sb = Sandbox::with_keys(
        "ov-taken",
        Keys::Both {
            capture: CAPTURE_KEY,
            paste: PASTE_KEY,
        },
    );
    sb.instance();
    let st = status(&sb);
    let reg = registration(&st, "capture.region");
    assert_eq!(
        (reg["keys"].as_str(), reg["status"].as_str()),
        (Some(CAPTURE_KEY), Some("taken")),
        "{reg:#}"
    );
    assert!(reg["message"].as_str().unwrap().contains("did not take it"));
    assert_eq!(registration(&st, "paste.float")["status"], "registered");
    let conflict = st["hotkeys"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "taken" && c["action"] == "capture.region")
        .cloned()
        .expect("a taken conflict for capture.region");
    assert!(
        conflict["offers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "disable"),
        "{conflict:#}"
    );
    let human = String::from_utf8_lossy(&sb.run(&["status"]).stdout).to_string();
    assert!(human.contains("capture.region"), "{human}");
    assert!(human.contains("is never taken over"), "{human}");
    // Never stolen, never retried in the background: the other program still owns it.
    let probe = Registrar::start().unwrap();
    assert_eq!(
        probe
            .register(&Chord::parse(CAPTURE_KEY).unwrap())
            .unwrap()
            .1,
        Status::Taken
    );
    drop(probe);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        registration(&status(&sb), "capture.region")["status"],
        "taken",
        "no background retry"
    );
    // Once it is free, an explicit reload (here: the file changed) registers it.
    other.unregister(id.unwrap()).unwrap();
    sb.write_keys(Keys::Both {
        capture: CAPTURE_KEY,
        paste: PASTE_KEY,
    });
    let until = Instant::now() + Duration::from_secs(10);
    while registration(&status(&sb), "capture.region")["status"] != "registered" {
        assert!(
            Instant::now() < until,
            "the reload registered the free chord"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    // Quit released both chords.
    let after = Registrar::start().unwrap();
    for k in [CAPTURE_KEY, PASTE_KEY] {
        let (id, st) = after.register(&Chord::parse(k).unwrap()).unwrap();
        assert_eq!(st, Status::Registered, "{k} was released at quit");
        after.unregister(id.unwrap()).unwrap();
    }
}

#[test]
fn rapid_triggers_open_exactly_one_overlay_and_cancel_leaves_nothing() {
    let _turn = hotkey_turn();
    let mut sb = Sandbox::synthetic(
        "ov-rapid",
        Keys::Both {
            capture: CAPTURE_KEY,
            paste: PASTE_KEY,
        },
    );
    let child = sb
        .cmd(&["run", "--no-tray"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    sb.adopt(pid); // ended by the sandbox even if an assertion fails
    sb.wait_healthy(pid);
    let st = status(&sb);
    assert_eq!(
        st["capture_backend"], "synthetic",
        "the debug-build test hook is on: nothing reads the screen"
    );
    assert_eq!(registration(&st, "capture.region")["status"], "registered");
    let picture = st["desktop"]["virtual_rect"].clone();
    assert!(
        picture["x"].as_i64().unwrap() <= -10_000,
        "the synthetic desktop lies far off-screen: {picture}"
    );
    // A dry run checks and captures nothing.
    let (code, dry) = sb.json(&["capture", "--interactive", "--dry-run"]);
    assert_eq!(code, 0, "{dry:#}");
    assert_eq!(
        (
            dry["data"]["opened"].as_bool(),
            dry["data"]["dry_run"].as_bool()
        ),
        (Some(false), Some(true))
    );
    assert_eq!(dry["data"]["picture"], picture);
    assert_eq!(status(&sb)["captures"]["started"], 0, "nothing captured");
    // Ten requests at once: one overlay.
    let results: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..10)
            .map(|_| s.spawn(|| sb.json(&["capture", "--interactive"])))
            .collect();
        hs.into_iter()
            .map(|h| {
                let (code, doc) = h.join().unwrap();
                assert_eq!(code, 0, "{doc:#}");
                let e = validate("noeul-ipc-v1#capture_region_result", &doc["data"]);
                assert!(e.is_empty(), "{e:?}");
                doc["data"].clone()
            })
            .collect()
    });
    let opened = results.iter().filter(|r| r["opened"] == true).count();
    assert_eq!(opened, 1, "exactly one overlay: {results:#?}");
    assert!(
        results
            .iter()
            .filter(|r| r["opened"] == false)
            .all(|r| r["reason"] == "already_open")
    );
    let st = status(&sb);
    let caps = &st["captures"];
    assert_eq!(
        (
            caps["open"].as_bool(),
            caps["started"].as_u64(),
            caps["ignored"].as_u64()
        ),
        (Some(true), Some(1), Some(9)),
        "{caps:#}"
    );
    assert_eq!(caps["overlay"]["phase"], "hover");
    assert_eq!(caps["overlay"]["picture"], picture);
    assert_eq!(caps["overlay"]["monitors"], 2);
    let w = picture["width"].as_u64().unwrap();
    let h = picture["height"].as_u64().unwrap();
    assert_eq!(
        caps["overlay"]["buffer_bytes"],
        w * h * 4,
        "one back buffer"
    );
    assert_eq!(st["memory"]["bytes"], w * h * 4, "one frozen picture");
    assert_eq!(caps["first_frame"]["count"], 1);
    let first = caps["first_frame"]["last"]["total_us"].as_u64().unwrap();
    assert!(first > 0 && first < 5_000_000, "first frame in {first} µs");
    // Cancel: the overlay and its picture are gone; nothing reached the disk or TEMP.
    let (code, c) = sb.json(&["capture", "--cancel", "--dry-run"]);
    assert_eq!((code, c["data"]["cancelled"].as_bool()), (0, Some(false)));
    assert_eq!(
        status(&sb)["captures"]["open"],
        true,
        "a dry run cancels nothing"
    );
    let (code, c) = sb.json(&["capture", "--cancel"]);
    assert_eq!((code, c["data"]["cancelled"].as_bool()), (0, Some(true)));
    let st = status(&sb);
    assert_eq!(st["captures"]["open"], false);
    assert_eq!(st["captures"]["overlay"], Value::Null);
    assert_eq!(st["memory"]["bytes"], 0, "nothing is kept after cancel");
    assert_eq!(st["captures"]["cancelled"], 1);
    let (code, c) = sb.json(&["capture", "--cancel"]);
    assert_eq!((code, c["data"]["session"].clone()), (0, Value::Null));
    assert!(
        files_under(&sb.temp_dir()).is_empty(),
        "no temporary files: {:?}",
        files_under(&sb.temp_dir())
    );
    assert!(
        files_under(&sb.root.join("Local")).is_empty(),
        "no data files without an output: {:?}",
        files_under(&sb.root.join("Local"))
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.len() < 4096 && out.stdout.len() < 4096,
        "the instance printed no picture"
    );
}

#[test]
fn the_overlay_needs_the_instance_and_reports_honestly() {
    let sb = Sandbox::synthetic("ov-absent", Keys::Off);
    for args in [
        &["capture", "--interactive"][..],
        &["capture", "--interactive", "--dry-run"],
        &["capture", "--cancel"],
    ] {
        let (code, doc) = sb.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("noeul_not_running")),
            "{args:?}"
        );
    }
    let (code, doc) = sb.json(&["capture", "--interactive", "--region", "0,0,1,1"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, doc) = sb.json(&["capture"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, doc) = sb.json(&["capture", "--pin"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (2, Some("usage")),
        "--pin needs --region"
    );
}
