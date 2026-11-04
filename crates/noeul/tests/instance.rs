//! The real resident instance on Windows (ADR 0023; NOEUL-FR-002, NOEUL-FR-003, NOEUL-FR-100,
//! NOEUL-SEC-003, NOEUL-SEC-011, KIM-SEC-020).
//!
//! Every instance runs in an isolated profile on its own pipe endpoint with `--no-tray`, and its
//! `hotkeys.json` binds `paste.float` to an obscure `Ctrl+Alt+Shift+F2x` chord before it starts
//! (tests never register F1, F3 or a common key). Every process a test starts is ended. No test
//! here reads the screen or shows a window on the user's desktop: the only pin is a 1×1 region of
//! the debug-build test hook's synthetic desktop, far off-screen, held in memory and closed at
//! once.
#![cfg(windows)]

mod common;

use common::{Keys, Sandbox, kill_pid, validate};
use noeul::win::pipe;
use serde_json::{Value, json};
use std::io::Read;
use std::time::{Duration, Instant};

/// Tests that register a chord take turns: the obscure chords are shared by the whole machine.
static HOTKEYS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hotkey_turn() -> std::sync::MutexGuard<'static, ()> {
    HOTKEYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn assert_valid(name: &str, v: &Value) {
    let e = validate(name, v);
    assert!(e.is_empty(), "violates {name}: {e:?}\n{v:#}");
}

/// `paste.float` in a `hotkeys list` document.
fn paste_action(list: &Value) -> Value {
    list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "paste.float")
        .cloned()
        .unwrap()
}

fn status(sb: &Sandbox) -> Value {
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 0, "{doc:#}");
    doc["data"].clone()
}

#[test]
fn lifecycle_status_health_capture_checks_and_quit() {
    let mut sb = Sandbox::new("life");
    let pid = sb.instance();
    let st = status(&sb);
    assert_valid("noeul-ipc-v1#status_result", &st);
    assert_eq!(st["instance"]["pid"], pid);
    assert_eq!(st["tray"]["shown"], false, "--no-tray");
    assert_eq!(
        st["memory"]["bytes"], 0,
        "an idle instance holds no picture"
    );
    let (code, h) = sb.json(&["health"]);
    assert_eq!(
        (code, h["data"]["identity"]["image"].as_str()),
        (0, Some("noeul.exe"))
    );

    let (code, dry) = sb.json(&["capture", "--region", "0,0,1,1", "--pin", "--dry-run"]);
    let desktop = st["desktop"]["state"].as_str().unwrap().to_string();
    if desktop == "available" {
        assert_eq!(code, 0, "{dry:#}");
        assert_eq!(dry["data"]["pin"], Value::Null);
        assert_eq!(status(&sb)["pins"]["open"], 0, "a dry run pins nothing");
    } else {
        eprintln!("desktop state is {desktop}: capture is refused, as it must be");
        assert_eq!(dry["error"]["code"], "desktop_unavailable");
    }
    let (code, far) = sb.json(&["capture", "--region", "-900000,0,4,4", "--pin"]);
    assert_eq!(code, if desktop == "available" { 2 } else { 6 }, "{far:#}");

    let (code, q) = sb.json(&["quit", "--dry-run"]);
    assert_eq!((code, q["data"]["quitting"].as_bool()), (0, Some(false)));
    let (code, q) = sb.json(&["quit"]);
    assert_eq!(code, 0, "{q:#}");
    assert_eq!(q["data"]["exited"], true, "the process ended");
    assert_eq!(status(&sb)["running"], false);
}

/// Runs on the debug-build test hook's synthetic desktop (`NOEUL_DEV_SYNTHETIC_DESKTOP=1`): the
/// 1×1 pin is cut from a synthetic picture and lies far off-screen, never topmost, never
/// activated, so nothing appears on the user's monitors and the screen is never read. (Until
/// Step 51 this test pinned a real pixel at the corner of the real primary monitor.)
#[test]
fn a_one_pixel_pin_lives_closes_and_reopens() {
    let mut sb = Sandbox::synthetic("pin", Keys::Off);
    sb.instance();
    let st = status(&sb);
    assert_eq!(st["capture_backend"], "synthetic", "{st:#}");
    let m = st["desktop"]["monitors"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["primary"] == true))
        .expect("the synthetic desktop has a primary monitor")
        .clone();
    let (x, y) = (
        m["rect"]["x"].as_i64().unwrap(),
        m["rect"]["y"].as_i64().unwrap(),
    );
    assert!(
        x <= -10_000 && y <= -10_000,
        "the synthetic monitor lies far off-screen: {m:#}"
    );
    let region = format!("{x},{y},1,1");
    let (code, cap) = sb.json(&["capture", "--region", &region, "--pin"]);
    assert_eq!(code, 0, "{cap:#}");
    assert_eq!(cap["data"]["pin"]["id"], "pin-1");
    let st = status(&sb);
    assert_eq!(
        st["memory"]["bytes"], 4,
        "1×1 BGRA, the frozen picture is gone"
    );
    assert_eq!(st["pins"]["open"], 1);
    let (_, list) = sb.json(&["pin", "list"]);
    assert_valid("noeul-ipc-v1#pin_list_result", &list["data"]);
    assert_eq!(
        list["data"]["pins"][0]["rect"],
        json!({"x": x, "y": y, "width": 1, "height": 1})
    );
    let (code, needs) = sb.json(&["pin", "close", "pin-1"]);
    assert_eq!(
        (code, needs["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, _) = sb.json(&["pin", "close", "pin-1", "--yes"]);
    assert_eq!(code, 0);
    assert_eq!(status(&sb)["pins"]["reopenable"], "pin-1");
    let (_, re) = sb.json(&["pin", "reopen"]);
    assert_eq!(re["data"]["pin"], "pin-1", "same id after reopen");
    let (code, q) = sb.json(&["quit"]);
    assert_eq!(code, 7, "open pins need --yes: {q:#}");
    let (code, q) = sb.json(&["quit", "--yes"]);
    assert_eq!((code, q["data"]["closed_pins"].as_u64()), (0, Some(1)));
}

#[test]
fn single_instance_under_concurrent_starts_and_runs() {
    let _turn = hotkey_turn();
    let mut sb = Sandbox::with_keys("race", Keys::Chord("Ctrl+Alt+Shift+F22"));
    // Four `start`s at once: one instance, every caller reports it.
    let pids: Vec<u64> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| s.spawn(|| sb.json(&["start", "--no-tray"])))
            .collect();
        hs.into_iter()
            .map(|h| {
                let (code, doc) = h.join().unwrap();
                assert_eq!(code, 0, "{doc:#}");
                doc["data"]["pid"].as_u64().unwrap()
            })
            .collect()
    });
    let first = pids[0];
    sb.adopt(u32::try_from(first).unwrap());
    assert!(pids.iter().all(|p| *p == first), "one instance: {pids:?}");
    // Further `run`s hand over and exit at once, without registering anything.
    for _ in 0..3 {
        let (code, doc) = sb.json(&["run", "--no-tray"]);
        assert_eq!(code, 0, "{doc:#}");
        assert_eq!(doc["data"]["handed_over"], true);
        assert_eq!(doc["data"]["pid"], first);
    }
    let st = status(&sb);
    assert_eq!(st["instance"]["pid"], first);
    assert_eq!(
        st["hotkeys"]["registrations"].as_array().unwrap().len(),
        1,
        "only the one instance registered its hotkey"
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
}

#[test]
fn concurrent_runs_from_nothing_leave_exactly_one() {
    let mut sb = Sandbox::with_keys("runrace", Keys::Off);
    let mut children: Vec<std::process::Child> = (0..4)
        .map(|_| {
            sb.cmd(&["run", "--no-tray", "--json"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    // Losers hand over and exit; the winner keeps running.
    let until = Instant::now() + Duration::from_secs(20);
    let mut exited = Vec::new();
    while exited.len() < 3 && Instant::now() < until {
        for (i, c) in children.iter_mut().enumerate() {
            if !exited.contains(&i) && c.try_wait().unwrap().is_some() {
                exited.push(i);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(exited.len(), 3, "exactly three hand over");
    let st = status(&sb);
    let winner = st["instance"]["pid"].as_u64().unwrap();
    for i in &exited {
        let mut out = String::new();
        children[*i]
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        let doc: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(doc["data"]["handed_over"], true, "{doc:#}");
        assert_eq!(doc["data"]["pid"], winner);
    }
    sb.adopt(u32::try_from(winner).unwrap());
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    for mut c in children {
        let _ = c.wait();
    }
}

#[test]
fn restart_after_a_crash_and_graceful_close() {
    let _turn = hotkey_turn();
    let mut sb = Sandbox::with_keys("crash", Keys::Chord("Ctrl+Alt+Shift+F22"));
    let pid = sb.instance();
    kill_pid(pid); // a crash: no clean-up at all
    assert!(noeul::win::wait_exit(pid, Duration::from_secs(5)));
    assert_eq!(
        status(&sb)["running"],
        false,
        "no stale endpoint survives the process"
    );
    let pid2 = sb.instance();
    assert_ne!(pid, pid2);
    let st = status(&sb);
    assert_eq!(
        st["hotkeys"]["registrations"][0]["status"], "registered",
        "the OS released the crashed instance's hotkey"
    );
    // `taskkill` without /F asks politely (WM_CLOSE): the instance cleans up and exits.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid2.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        noeul::win::wait_exit(pid2, Duration::from_secs(5)),
        "closed on WM_CLOSE"
    );
}

#[test]
fn a_squatter_gets_nothing_and_is_reported() {
    let sb = Sandbox::new("squat");
    let mut squatter = pipe::Listener::bind(&sb.endpoint).unwrap();
    let got = std::thread::spawn(move || {
        let p = squatter.accept().unwrap().unwrap();
        let mut buf = [0u8; 64];
        // The client checks who serves the pipe before sending anything and then hangs up.
        let n = (&p).read(&mut buf).unwrap_or(0);
        (n, squatter)
    });
    let (code, doc) = sb.json(&["status"]);
    assert_eq!(code, 4, "{doc:#}");
    assert_eq!(doc["error"]["code"], "noeul_identity_mismatch");
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not noeul.exe")
    );
    let (n, squatter) = got.join().unwrap();
    assert_eq!(n, 0, "not one byte was sent to the squatter");
    // A start on a squatted endpoint refuses instead of fighting over it.
    let (code, doc) = sb.json(&["run", "--no-tray"]);
    assert_eq!(code, 4, "{doc:#}");
    drop(squatter);
}

fn raw(sb: &Sandbox) -> pipe::Pipe {
    pipe::connect(&sb.endpoint, Duration::from_secs(2)).unwrap()
}

fn read_msg(p: &pipe::Pipe) -> Option<Value> {
    let mut r = pipe::Until {
        pipe: p,
        deadline: Some(Instant::now() + Duration::from_secs(10)),
    };
    noeul::ipc::read_frame(&mut r).unwrap_or(None)
}

#[test]
fn malformed_frames_bad_handshakes_and_two_clients() {
    let mut sb = Sandbox::new("frames");
    let pid = sb.instance();
    // An oversized length prefix.
    let p = raw(&sb);
    p.write_all_bytes(&u32::MAX.to_le_bytes()).unwrap();
    let e = read_msg(&p).expect("a connection error");
    assert_valid("noeul-ipc-v1", &e);
    assert_eq!(e["error"]["code"], "protocol_error");
    assert!(read_msg(&p).is_none(), "then the connection is closed");
    // Not JSON.
    let p = raw(&sb);
    let mut junk = 4u32.to_le_bytes().to_vec();
    junk.extend_from_slice(b"\xff\xfe!!");
    p.write_all_bytes(&junk).unwrap();
    assert_eq!(read_msg(&p).unwrap()["error"]["code"], "protocol_error");
    // A newer-only client.
    let p = raw(&sb);
    let hello = json!({"type": "hello", "protocol": {"min": 2, "max": 3},
                       "client": {"name": "future", "version": "9.0.0"}});
    p.write_all_bytes(&noeul::ipc::encode(&hello)).unwrap();
    assert_eq!(read_msg(&p).unwrap()["error"]["code"], "noeul_incompatible");
    // A request before the handshake.
    let p = raw(&sb);
    p.write_all_bytes(&noeul::ipc::encode(&noeul::ipc::request(
        1,
        "quit",
        json!({"force": true}),
    )))
    .unwrap();
    assert_eq!(read_msg(&p).unwrap()["error"]["code"], "protocol_error");
    // A proper client, then a bad request and an unknown method on the same connection.
    let p = raw(&sb);
    p.write_all_bytes(&noeul::ipc::encode(&noeul::ipc::hello("test")))
        .unwrap();
    let w = read_msg(&p).unwrap();
    assert_valid("noeul-ipc-v1", &w);
    assert_eq!(w["instance"]["pid"], pid);
    p.write_all_bytes(&noeul::ipc::encode(&noeul::ipc::request(
        5,
        "format.disk",
        json!({}),
    )))
    .unwrap();
    let r = read_msg(&p).unwrap();
    assert_eq!(
        (r["id"].as_u64(), r["error"]["code"].as_str()),
        (Some(5), Some("usage"))
    );
    p.write_all_bytes(&noeul::ipc::encode(&noeul::ipc::request(
        6,
        "pin.list",
        json!({}),
    )))
    .unwrap();
    let r = read_msg(&p).unwrap();
    assert_valid("noeul-ipc-v1", &r);
    assert_eq!(r["id"], 6, "the connection survived the bad request");

    // Two clients at once, many requests each.
    std::thread::scope(|s| {
        for _ in 0..2 {
            s.spawn(|| {
                for _ in 0..10 {
                    let (code, doc) = sb.json(&["pin", "list"]);
                    assert_eq!(code, 0, "{doc:#}");
                }
            });
        }
    });
    // A silent client is dropped after the handshake timeout, and the instance stays healthy.
    let silent = raw(&sb);
    let t = Instant::now();
    let e = read_msg(&silent).expect("a timeout error");
    assert_eq!(e["error"]["code"], "timeout");
    assert!(t.elapsed() >= Duration::from_secs(4));
    let (code, _) = sb.json(&["health"]);
    assert_eq!(code, 0, "the instance survived every bad client");
}

#[test]
fn hotkeys_register_report_taken_and_reload_without_stealing() {
    use kim_hotkeys::Chord;
    use kim_hotkeys::register::{Registrar, Status};
    let _turn = hotkey_turn();
    let mut sb = Sandbox::with_keys("keys", Keys::Chord("Ctrl+Alt+Shift+F22"));
    sb.instance();
    let reg = |sb: &Sandbox| status(sb)["hotkeys"]["registrations"][0].clone();
    assert_eq!(reg(&sb)["status"], "registered", "{:#}", reg(&sb));
    assert_eq!(reg(&sb)["keys"], "Ctrl+Alt+Shift+F22");
    let (_, list) = sb.json(&["hotkeys", "list"]);
    assert_valid("hotkeys-cli-v1", &list);
    assert_eq!(
        paste_action(&list)["bindings"][0]["registration"],
        "registered"
    );

    // Another program (this test) owns F21; Noeul is asked to move there.
    let other = Registrar::start().unwrap();
    let (id, st) = other
        .register(&Chord::parse("Ctrl+Alt+Shift+F21").unwrap())
        .unwrap();
    assert_eq!(
        st,
        Status::Registered,
        "F21 must be free on this machine for the test"
    );
    let (code, set) = sb.json(&["hotkeys", "set", "paste.float", "Ctrl+Alt+Shift+F21"]);
    assert_eq!((code, set["data"]["written"].as_bool()), (0, Some(true)));
    let r = reg(&sb);
    assert_eq!(r["status"], "taken", "{r:#}");
    assert!(r["message"].as_str().unwrap().contains("did not take it"));
    let st = status(&sb);
    let c = st["hotkeys"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "taken")
        .expect("a taken conflict with offers");
    // Ctrl+Alt+Shift+F21 has no free modifier left to suggest, so the offers are disable/keep.
    assert!(
        c["offers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "disable"),
        "{c:#}"
    );
    let human = String::from_utf8_lossy(&sb.run(&["status"]).stdout).to_string();
    assert!(
        human.contains(
            "fix: `noeul hotkeys set paste.float <keys>` or `noeul hotkeys disable paste.float`"
        ),
        "{human}"
    );
    let (_, list) = sb.json(&["hotkeys", "list"]);
    let b = &paste_action(&list)["bindings"][0];
    assert_eq!(
        (b["registration"].as_str(), b["active"].as_bool()),
        (Some("taken"), Some(false))
    );
    // Never stolen: the other program still owns F21.
    let again = Registrar::start().unwrap();
    assert_eq!(
        again
            .register(&Chord::parse("Ctrl+Alt+Shift+F21").unwrap())
            .unwrap()
            .1,
        Status::Taken
    );
    drop(again);
    other.unregister(id.unwrap()).unwrap();

    // The file watcher: a direct edit of hotkeys.json is applied without the CLI.
    sb.write_keys(Keys::Chord("Ctrl+Alt+Shift+F22"));
    let until = Instant::now() + Duration::from_secs(10);
    while reg(&sb)["keys"] != "Ctrl+Alt+Shift+F22" {
        assert!(
            Instant::now() < until,
            "the change notification reloaded the file"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(reg(&sb)["status"], "registered");
    // A broken file keeps the previous bindings and says why.
    std::fs::write(sb.hotkeys_file(), b"{ not json").unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while status(&sb)["hotkeys"]["problem"].is_null() {
        assert!(Instant::now() < until, "the broken file was noticed");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(reg(&sb)["status"], "registered", "previous bindings stay");
    sb.write_keys(Keys::Chord("Ctrl+Alt+Shift+F22"));

    // Quit releases the registration (the test can take the chord afterwards).
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    let after = Registrar::start().unwrap();
    let (id, st) = after
        .register(&Chord::parse("Ctrl+Alt+Shift+F22").unwrap())
        .unwrap();
    assert_eq!(st, Status::Registered, "quit released the hotkey");
    after.unregister(id.unwrap()).unwrap();
}

#[test]
fn no_hotkeys_flag_registers_nothing() {
    let mut sb = Sandbox::new("nohk");
    let child = sb
        .cmd(&["run", "--no-tray", "--no-hotkeys"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    sb.adopt(pid);
    sb.wait_healthy(pid);
    let st = status(&sb);
    assert_eq!(st["hotkeys"]["off_reason"], "--no-hotkeys");
    assert!(
        st["hotkeys"]["registrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (code, _) = sb.json(&["quit", "--yes"]);
    assert_eq!(code, 0);
    drop(child);
}
