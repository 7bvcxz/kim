//! Step 09 — the Host supervisor and IPC v1 on real Windows (docs/contracts/IPC_PROTOCOL.md,
//! ADR 0005, ADR 0019). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-SEC-010`,
//! `SUBAK-SEC-012`, `KIM-NFR-014`.
//!
//! Real `subak.exe` Hosts, each on its **own** endpoint (a user's Host is never touched), driven
//! by real CLI clients and by raw IPC clients written here (overlapped pipe I/O, as the protocol
//! requires). Every control message a raw client receives is validated against
//! `docs/contracts/schemas/ipc-v1.schema.json`. Every test ends what it started: Hosts are
//! stopped or killed by a guard, child processes are killed on drop.
//!
//! Headless: nothing here opens a window. Lines starting with `EVIDENCE` record what was
//! measured; run with `--nocapture` to see them.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::{
    DuplicateTokenEx, GetLengthSid, PSID, RevertToSelf, SID_AND_ATTRIBUTES, SecurityImpersonation,
    SetTokenInformation, TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenImpersonation,
    TokenIntegrityLevel,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX, ReadFile,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateMutexW, GetCurrentProcess, OpenProcess,
    OpenProcessToken, PROCESS_SYNCHRONIZE, ResumeThread, SetThreadToken, WaitForSingleObject,
};

// ---------------------------------------------------------------------------------------------

#[test]
fn two_clients_share_one_host_and_a_killed_client_stops_nothing() {
    let h = Host::start("two", &[]);
    let shell = pid_of(&h.status()["pane"]["pid"]);
    let mut a = Attach::spawn(&h.ep);
    let mut b = Attach::spawn(&h.ep);
    let st = h.status();
    assert_eq!(st["clients"]["attached"], 2, "{st}");
    let kinds: Vec<&Value> = st["clients"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["attached"] == true)
        .map(|c| &c["kind"])
        .collect();
    assert_eq!(kinds, [&json!("cli"), &json!("cli")]);
    // Both clients see the same pane: what one types, the other sees.
    a.send(b"echo from-a-mark^er\r");
    a.expect("from-a-marker");
    b.expect("from-a-marker");
    b.send(b"echo from-b-mark^er\r");
    a.expect("from-b-marker");
    b.expect("from-b-marker");
    // One client is killed (TerminateProcess): the Host drops it and nothing else changes.
    let a_pid = a.child.id();
    drop(a);
    let until = Instant::now() + LONG;
    while h.status()["clients"]["attached"] != 1 {
        assert!(Instant::now() < until, "the killed client was not dropped");
        std::thread::sleep(Duration::from_millis(50));
    }
    let st = h.status();
    assert_eq!(pid_of(&st["pane"]["pid"]), shell, "same shell");
    assert_eq!(st["pane"]["running"], true);
    b.send(b"echo after-kill-mark^er\r");
    b.expect("after-kill-marker");
    b.send(&[0x02, b'd']);
    assert!(b.child.wait().unwrap().success(), "Ctrl+B d detaches");
    assert_eq!(h.status()["pane"]["running"], true, "detach stops nothing");
    eprintln!(
        "EVIDENCE two-clients: host={} shell={} client {a_pid} killed, the other kept working",
        h.proc.pid, shell
    );
}

#[test]
fn handshake_violations_are_refused_with_structured_errors() {
    let h = Host::start("hello", &["--no-shell", "--idle-exit", "60"]);
    // A well-behaved client stays connected through all of it.
    let good = Raw::handshake(&h.ep, "gui");
    let refuse = |frames: &[Vec<u8>], code: &str| {
        let c = Raw::open(&h.ep);
        for f in frames {
            let _ = c.write(f);
        }
        c.expect_closed_with(code, LONG)
    };
    refuse(
        &[b"\x09\x00\x00\x00\x01{not json".to_vec()],
        "protocol_error",
    );
    refuse(&[control(&json!([1, 2]))[..].to_vec()], "protocol_error");
    refuse(
        &[control(
            &json!({"type": "shell", "command": "del /s /q C:\\"}),
        )],
        "protocol_error",
    );
    refuse(
        &[control(
            &json!({"type": "request", "id": 1, "method": "host.stop"}),
        )],
        "protocol_error",
    );
    let mut root = hello("cli");
    root["client"]["kind"] = json!("root");
    refuse(&[control(&root)], "protocol_error");
    refuse(&[data(1, 0, 2, b"dir\r")], "protocol_error");
    refuse(&[vec![0, 0, 0, 0, 1]], "protocol_error");
    refuse(&[vec![2, 0, 0, 0, 7, b'x']], "protocol_error");
    // Version negotiation: no common major → protocol_unsupported with the supported range.
    for (min, max) in [(2, 3), (0, 0)] {
        let mut v = hello("cli");
        v["protocol"] = json!({"min": min, "max": max});
        let m = refuse(&[control(&v)], "protocol_unsupported");
        assert_eq!(
            m["error"]["details"]["supported"],
            json!({"min": 1, "max": 1})
        );
    }
    // An oversized frame is refused from its header, before any payload is sent or allocated.
    let t = Instant::now();
    let mut huge = u32::try_from(256 * 1024 + 2)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    huge.push(1);
    refuse(&[huge], "protocol_error");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    // Silence and a hello that never completes: closed when the 5 s handshake ends.
    let t = Instant::now();
    refuse(&[], "timeout");
    let silent = t.elapsed();
    let t = Instant::now();
    refuse(&[control(&hello("cli"))[..9].to_vec()], "protocol_error");
    let partial = t.elapsed();
    for d in [silent, partial] {
        assert!(
            d >= Duration::from_millis(4500) && d < Duration::from_secs(12),
            "{d:?}"
        );
    }
    let r = good.request(1, "host.health", &json!({}));
    assert_eq!(
        r["result"]["healthy"], true,
        "the good client was never affected"
    );
    let st = h.stats();
    assert!(
        st["ipc"]["rejected"]["handshake"].as_u64().unwrap() >= 13,
        "{st}"
    );
    assert_eq!(st["ipc"]["panics"], 0);
    eprintln!(
        "EVIDENCE handshake: 13 malformed handshakes refused; silent connection closed after {silent:?}, partial hello after {partial:?}"
    );
}

#[test]
fn requests_are_validated_and_violations_close_only_that_connection() {
    let h = Host::start("requests", &["--no-shell", "--idle-exit", "60"]);
    let c = Raw::handshake(&h.ep, "cli");
    let cases = [
        (
            "shell.exec",
            json!({"command": "calc.exe"}),
            "unknown_request",
        ),
        (
            "terminal.restore",
            json!({"target": "t-000000000001", "claude_resume": true}),
            "unknown_request",
        ),
        (
            "terminal.create",
            json!({"command": "cmd.exe /c echo pwned"}),
            "invalid_params",
        ),
        (
            "terminal.create",
            // Environment overrides exist since Step 11; a malformed one is refused.
            json!({"env": {"A=B": "1"}}),
            "invalid_params",
        ),
        (
            "terminal.create",
            json!({"profile": "work"}),
            "invalid_params",
        ),
        ("Host.Status", json!({}), "unknown_request"),
        ("host.stop", json!({"dry_run": "yes"}), "invalid_params"),
        (
            "events.subscribe",
            json!({"topics": ["everything"]}),
            "invalid_params",
        ),
        ("pane.attach", json!({}), "target_not_found"),
        (
            "pane.resize",
            json!({"cols": 80, "rows": 25}),
            "target_not_found",
        ),
    ];
    for (i, (method, params, code)) in cases.iter().enumerate() {
        let r = c.request(i as u64 + 1, method, params);
        assert_eq!(error_code(&r), *code, "{method}: {r}");
    }
    let r = {
        c.send(&json!({"type": "request", "id": 50, "method": "host.status", "params": [1]}));
        match c.next_control(LONG) {
            Got::Control(m) => m,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(error_code(&r), "invalid_params");
    assert_eq!(r["error"]["details"]["field"], "params");
    // Nothing was started by any of these.
    let children: Vec<_> = processes()
        .into_iter()
        .filter(|(_, parent, _)| *parent == h.proc.pid)
        .collect();
    assert!(children.is_empty(), "the Host started {children:?}");
    let r = c.request(60, "host.status", &json!({}));
    assert_eq!(r["result"]["terminals"], 0);

    // Fatal violations close only the offending connection.
    let fatal = |frames: Vec<Vec<u8>>| {
        let v = Raw::handshake(&h.ep, "cli");
        for f in frames {
            let _ = v.write(&f);
        }
        v.expect_closed_with("protocol_error", LONG);
    };
    fatal(vec![control(
        &json!({"type": "request", "id": -1, "method": "host.status"}),
    )]);
    fatal(vec![control(&hello("cli"))]);
    fatal(vec![control(
        &json!({"type": "response", "id": 1, "ok": true, "result": {}}),
    )]);
    fatal(vec![data(1, 0, 0, b"x")]);
    fatal(vec![data(1, 0, 3, b"x")]);
    let mut big = u32::try_from((1 << 20) + 15)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    big.push(2);
    fatal(vec![big]);
    // A request frame that stops half-way is closed when the frame timeout (10 s) ends.
    let t = Instant::now();
    fatal(vec![
        control(&json!({"type": "request", "id": 9, "method": "host.status"}))[..12].to_vec(),
    ]);
    let stalled = t.elapsed();
    assert!(
        stalled >= Duration::from_secs(9) && stalled < Duration::from_secs(20),
        "{stalled:?}"
    );
    let r = c.request(61, "host.health", &json!({}));
    assert_eq!(
        r["result"]["healthy"], true,
        "the first connection is unaffected"
    );
    let st = h.stats();
    assert!(st["ipc"]["protocol_errors"].as_u64().unwrap() >= 7, "{st}");
    assert!(
        st["ipc"]["errors_total"]["unknown_request"]
            .as_u64()
            .unwrap()
            >= 3
    );
    assert_eq!(st["ipc"]["panics"], 0);
    eprintln!(
        "EVIDENCE requests: 8 refused requests answered, 7 fatal violations closed only their connection; a stalled frame was closed after {stalled:?}"
    );
}

#[test]
fn unauthorized_clients_cannot_type_into_or_read_a_pane() {
    let h = Host::start("authz", &[]);
    let hook = Raw::handshake(&h.ep, "hook");
    assert_eq!(hook.request(1, "host.status", &json!({}))["ok"], true);
    assert_eq!(hook.request(2, "host.health", &json!({}))["ok"], true);
    for (id, method, code) in [
        (3, "pane.attach", "unauthorized"),
        (4, "host.stop", "unauthorized"),
        (5, "host.stats", "unauthorized"),
        (6, "terminal.create", "unauthorized"),
        (7, "events.subscribe", "unauthorized"),
        (8, "claude.report", "unknown_request"), // the hook's own method: Step 27
    ] {
        let r = hook.request(id, method, &json!({"kill_terminals": true}));
        assert_eq!(error_code(&r), code, "hook {method}");
    }
    let mcp = Raw::handshake(&h.ep, "mcp");
    for (id, method, code) in [
        (1, "pane.attach", "unauthorized"),
        (2, "pane.resize", "unauthorized"),
        (3, "host.stop", "unauthorized"),
    ] {
        assert_eq!(
            error_code(&mcp.request(id, method, &json!({}))),
            code,
            "mcp {method}"
        );
    }
    assert_eq!(mcp.request(4, "host.stats", &json!({}))["ok"], true);
    // Typing without an interactive attach reaches nothing.
    hook.write(&data(1, 0, 2, b"echo pwn^ed-by-hook\r"))
        .unwrap();
    mcp.write(&data(1, 0, 2, b"echo pwn^ed-by-mcp\r")).unwrap();
    let cli = Raw::handshake(&h.ep, "cli");
    cli.write(&data(1, 0, 2, b"echo pwn^ed-unattached\r"))
        .unwrap();
    let viewer = Raw::handshake(&h.ep, "cli");
    let r = viewer.request(1, "pane.attach", &json!({"interactive": false}));
    assert_eq!(r["result"]["interactive"], false, "{r}");
    viewer
        .write(&data(1, 0, 2, b"echo pwn^ed-read-only\r"))
        .unwrap();
    let r = viewer.request(2, "pane.resize", &json!({"cols": 50, "rows": 10}));
    assert_eq!(error_code(&r), "invalid_state", "a viewer cannot resize");
    // A real user attaches and types: only that reaches the shell.
    let mut user = Attach::spawn(&h.ep);
    user.send(b"echo legit-mark^er\r");
    user.expect("legit-marker");
    let seen = user.text();
    assert!(
        !seen.contains("pwn"),
        "unauthorized input reached the shell:\n{seen}"
    );
    // …and the viewer saw it, but the hook and the mcp client got no pane output at all.
    let until = Instant::now() + LONG;
    let mut viewer_out = Vec::new();
    while !String::from_utf8_lossy(&viewer_out).contains("legit-marker") {
        assert!(Instant::now() < until, "the read-only viewer saw no output");
        if let Got::Data(b) = viewer.next(Duration::from_millis(200)) {
            viewer_out.extend(b);
        }
    }
    for (who, c) in [("hook", &hook), ("mcp", &mcp), ("unattached cli", &cli)] {
        assert_eq!(
            c.next(Duration::from_millis(500)),
            Got::Timeout,
            "{who} got data"
        );
    }
    let st = h.stats();
    assert!(
        st["ipc"]["input_frames_dropped"].as_u64().unwrap() >= 4,
        "{st}"
    );
    eprintln!(
        "EVIDENCE authz: hook/mcp refused pane.attach, host.stop; 4 unauthorized input frames dropped ({}); pane shell={} only ran the attached user's command",
        st["ipc"]["input_frames_dropped"],
        h.status()["pane"]["pid"]
    );
}

/// Not a test of its own: the misbehaving client process for
/// [`a_client_killed_mid_request_leaves_the_host_and_its_pane_running`]. Inert unless
/// `SUBAK_IPC_CHILD=<mode>:<endpoint>` is set.
#[test]
fn helper_client_process() {
    let Ok(spec) = std::env::var("SUBAK_IPC_CHILD") else {
        return;
    };
    let (mode, ep) = spec.split_once(':').unwrap();
    let c = Raw::handshake(ep, "cli");
    match mode {
        // Half of a request frame, then nothing.
        "half-frame" => c
            .write(&control(&json!({"type": "request", "id": 1, "method": "host.status"}))[..10])
            .unwrap(),
        // An interactive attach whose output is never read.
        "unread" => {
            c.send(&json!({"type": "request", "id": 1, "method": "pane.attach", "params": {}}));
        }
        other => panic!("unknown mode {other}"),
    }
    println!("CHILD-READY");
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn a_client_killed_mid_request_leaves_the_host_and_its_pane_running() {
    let h = Host::start("crash-client", &[]);
    let shell = pid_of(&h.status()["pane"]["pid"]);
    let mut pids = Vec::new();
    for mode in ["half-frame", "unread", "half-frame"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "helper_client_process",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("SUBAK_IPC_CHILD", format!("{mode}:{}", h.ep))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        let mut out = child.stdout.take().unwrap();
        let mut seen = Vec::new();
        let until = Instant::now() + LONG;
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("CHILD-READY") {
            assert!(Instant::now() < until, "helper not ready");
            let n = out.read(&mut buf).unwrap();
            assert!(n > 0, "helper ended: {}", String::from_utf8_lossy(&seen));
            seen.extend_from_slice(&buf[..n]);
        }
        // The client process crashes in the middle of its request.
        child.kill().unwrap();
        child.wait().unwrap();
        pids.push(child.id());
    }
    h.wait_connected(1);
    let st = h.status();
    assert_eq!(pid_of(&st["pane"]["pid"]), shell);
    assert_eq!(st["pane"]["running"], true);
    let mut user = Attach::spawn(&h.ep);
    user.send(b"echo still-alive-mark^er\r");
    user.expect("still-alive-marker");
    assert_eq!(h.stats()["ipc"]["panics"], 0);
    eprintln!(
        "EVIDENCE client-crash: clients {pids:?} killed mid-request; host={} shell={shell} unaffected",
        h.proc.pid
    );
}

/// xorshift64*: deterministic pseudo-random input (no extra crate).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[test]
fn fuzzed_frames_never_crash_the_host() {
    let h = Host::start("fuzz", &["--no-shell", "--idle-exit", "60"]);
    let before = h.stats();
    let mut rng = Rng(0x0009_f022_5eed);
    // Seeds only use kinds that may not stop the Host (hook, mcp), so no mutation can.
    let seeds: Vec<Vec<u8>> = vec![
        control(&hello("hook")),
        control(&hello("mcp")),
        control(&json!({"type": "request", "id": 1, "method": "host.status", "params": {}})),
        control(
            &json!({"type": "request", "id": 2, "method": "pane.attach", "params": {"size": {"cols": 80, "rows": 25}}}),
        ),
        control(
            &json!({"type": "request", "id": 3, "method": "events.subscribe", "params": {"topics": ["host"]}}),
        ),
        data(1, 0, 2, b"dir\r"),
    ];
    let rounds = 300;
    let mut replies = 0;
    for i in 0..rounds {
        let c = Raw::open(&h.ep);
        let mut wire = Vec::new();
        if i % 2 == 0 {
            wire.extend(&seeds[rng.below(2) as usize]); // a valid hook/mcp hello
        }
        for _ in 0..=rng.below(4) {
            match rng.below(4) {
                0 => wire.extend((0..rng.below(48)).map(|_| rng.next() as u8)),
                1 => {
                    // A header announcing a random length, then a few bytes.
                    let len = (rng.next() % 600_000) as u32;
                    wire.extend(len.to_le_bytes());
                    wire.push((rng.below(4)) as u8);
                    wire.extend((0..rng.below(16)).map(|_| rng.next() as u8));
                }
                _ => {
                    let mut f = seeds[rng.below(seeds.len() as u64) as usize].clone();
                    for _ in 0..=rng.below(3) {
                        let at = rng.below(f.len() as u64) as usize;
                        f[at] ^= 1 << rng.below(8);
                    }
                    if rng.below(3) == 0 {
                        f.truncate(rng.below(f.len() as u64) as usize);
                    }
                    wire.extend(f);
                }
            }
        }
        let _ = c.write(&wire); // the Host may already have closed the connection
        if i % 10 == 0 {
            // Read what the Host answers; every control message must be valid IPC v1.
            while let Got::Control(_) | Got::Data(_) = c.next(Duration::from_millis(300)) {
                replies += 1;
            }
        }
    }
    h.wait_connected(1);
    let (code, health) = run(&["host", "health", "--endpoint", &h.ep]);
    assert_eq!(code, 0, "{health}");
    assert_eq!(
        pid_of(&health["data"]["host"]["pid"]),
        h.proc.pid,
        "the same Host"
    );
    let after = h.stats();
    assert_eq!(after["ipc"]["panics"], 0, "{after}");
    // A client that closes before the Host's accept completes is never served (ERROR_NO_DATA),
    // so not every round reaches a handshake; most must.
    let total = |s: &Value| s["ipc"]["connections_total"].as_u64().unwrap();
    let served = total(&after) - total(&before);
    assert!(served >= rounds / 2, "{after}");
    let handles = |s: &Value| s["host"]["handle_count"].as_u64().unwrap();
    assert!(
        handles(&after) <= handles(&before) + 16,
        "handles {} -> {}",
        handles(&before),
        handles(&after)
    );
    eprintln!(
        "EVIDENCE fuzz: {rounds} connections of random frames ({served} reached the handshake), {replies} valid replies read; host={} alive, panics 0, protocol errors {}, handshakes refused {}, handles {} -> {}",
        h.proc.pid,
        after["ipc"]["protocol_errors"],
        after["ipc"]["rejected"]["handshake"],
        handles(&before),
        handles(&after)
    );
}

#[test]
fn the_connection_limit_refuses_extra_clients_with_busy() {
    let h = Host::start("limit", &["--no-shell", "--idle-exit", "60"]);
    // 64 connections that never say hello occupy every slot until their handshake times out.
    let idle: Vec<Raw> = (0..64).map(|_| Raw::open(&h.ep)).collect();
    std::thread::sleep(Duration::from_millis(500));
    let extra = Raw::open(&h.ep);
    extra.send(&hello("cli"));
    extra.expect_closed_with("busy", LONG);
    drop(idle);
    let good = Raw::handshake(&h.ep, "cli");
    let st = good.request(1, "host.stats", &json!({}));
    assert!(
        st["result"]["ipc"]["rejected"]["limit"].as_u64().unwrap() >= 1,
        "{st}"
    );
}

#[test]
fn concurrent_starts_give_exactly_one_host() {
    let ep = endpoint("race");
    let starters: Vec<Child> = (0..6)
        .map(|_| {
            Command::new(subak())
                .args([
                    "host",
                    "start",
                    "--endpoint",
                    &ep,
                    "--no-shell",
                    "--idle-exit",
                    "60",
                    "--json",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap()
        })
        .collect();
    let ids: Vec<u32> = starters.iter().map(Child::id).collect();
    let results: Vec<(i32, Value)> = starters
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().unwrap();
            let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
            validate("cli-envelope-v1", &doc);
            (out.status.code().unwrap(), doc)
        })
        .collect();
    let started: Vec<&Value> = results
        .iter()
        .filter(|(c, _)| *c == 0)
        .map(|(_, d)| d)
        .collect();
    assert_eq!(
        started.len(),
        1,
        "exactly one starter started a Host: {results:?}"
    );
    let pid = pid_of(&started[0]["data"]["host"]["pid"]);
    let h = Host {
        ep,
        proc: Proc::open(pid),
    };
    for (code, doc) in results.iter().filter(|(c, _)| *c != 0) {
        assert_eq!(
            (*code, doc["error"]["code"].as_str()),
            (7, Some("host_already_running")),
            "{doc}"
        );
        assert!(
            doc["error"]["details"]
                .to_string()
                .contains(&format!("host pid: {pid}")),
            "every loser found the winner's Host: {doc}"
        );
    }
    let hosts: Vec<u32> = processes()
        .into_iter()
        .filter(|(p, parent, name)| {
            name == "subak.exe" && ids.contains(parent) && Proc::try_alive(*p)
        })
        .map(|(p, _, _)| p)
        .collect();
    assert_eq!(hosts, [pid], "one Host process for the endpoint");
    assert_eq!(pid_of(&h.status()["host"]["pid"]), pid);
    eprintln!(
        "EVIDENCE start-race: 6 concurrent `host start`, starters {ids:?}: one Host {pid}, 5 x host_already_running"
    );

    // A starter that crashed while holding the start lock does not block the next one.
    let ep2 = endpoint("abandoned");
    let lock = wide(&format!(r"Local\mouv-subak-start-{ep2}"));
    let holder = std::thread::spawn(move || {
        // SAFETY: named mutex; taken and the thread ends without releasing it (abandoned). The
        // handle is returned so the mutex object outlives the thread.
        unsafe {
            let m = CreateMutexW(std::ptr::null(), 1, lock.as_ptr());
            assert!(!m.is_null());
            m as usize
        }
    });
    let mutex = holder.join().unwrap();
    let (code, doc) = run(&["host", "start", "--endpoint", &ep2, "--no-shell"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["start_lock_abandoned"], true);
    let h2 = Host {
        ep: ep2,
        proc: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
    };
    // SAFETY: the mutex handle from the holder thread; closed once.
    unsafe { CloseHandle(mutex as HANDLE) };
    drop(h2);
    drop(h);
}

impl Proc {
    fn try_alive(pid: u32) -> bool {
        // SAFETY: plain open; closed below.
        let h = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if h.is_null() {
            return false;
        }
        // SAFETY: valid handle; closed right after.
        let alive = unsafe { WaitForSingleObject(h, 0) } != WAIT_OBJECT_0;
        // SAFETY: as above.
        unsafe { CloseHandle(h) };
        alive
    }
}

#[test]
fn a_crashed_host_leaves_no_stale_endpoint_and_a_new_one_starts() {
    let h = Host::start("stale", &[]);
    let shell = Proc::open(pid_of(&h.status()["pane"]["pid"]));
    let watcher = Raw::handshake(&h.ep, "gui");
    assert_eq!(
        watcher.request(1, "events.subscribe", &json!({"topics": ["host", "panes"]}))["ok"],
        true
    );
    h.proc.kill(); // a Host crash
    assert_eq!(
        watcher.next(LONG),
        Got::Eof,
        "the client sees the Host go away"
    );
    assert!(
        shell.wait(LONG),
        "the pane ends with its Host (kill-on-close job)"
    );
    let (code, doc) = run(&["host", "status", "--endpoint", &h.ep]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("host_not_running"))
    );
    // Nothing stale is left: the same endpoint starts a new Host at once.
    let t = Instant::now();
    let (code, doc) = run(&["host", "start", "--endpoint", &h.ep, "--no-shell"]);
    assert_eq!(code, 0, "{doc}");
    let new = Proc::open(pid_of(&doc["data"]["host"]["pid"]));
    assert_ne!(new.pid, h.proc.pid);
    eprintln!(
        "EVIDENCE stale: host={} killed; shell={} ended; new host={} on the same endpoint after {:?}",
        h.proc.pid,
        shell.pid,
        new.pid,
        t.elapsed()
    );
    let _h2 = Host {
        ep: h.ep.clone(),
        proc: new,
    };
}

#[test]
fn an_unresponsive_host_is_reported_but_never_killed_or_replaced() {
    let h = Host::start("hung", &[]);
    let shell = Proc::open(pid_of(&h.status()["pane"]["pid"]));
    let frozen = h.proc.suspend();
    let t = Instant::now();
    let (code, doc) = run(&["host", "health", "--endpoint", &h.ep]);
    let waited = t.elapsed();
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("host_unresponsive")),
        "{doc}"
    );
    assert!(waited < Duration::from_secs(15), "{waited:?}");
    // start-on-demand must neither kill it nor start a second Host next to it.
    let (code, doc) = run(&["host", "start", "--endpoint", &h.ep, "--no-shell"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("host_unresponsive")),
        "{doc}"
    );
    assert!(h.proc.alive() && shell.alive(), "nothing was killed");
    drop(frozen);
    let (code, doc) = run(&["host", "health", "--endpoint", &h.ep]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(pid_of(&doc["data"]["host"]["pid"]), h.proc.pid);
    assert_eq!(
        pid_of(&h.status()["pane"]["pid"]),
        shell.pid,
        "the session is intact"
    );
    eprintln!(
        "EVIDENCE hung-host: host={} suspended: health -> host_unresponsive after {waited:?}, start refused; resumed with shell={} intact",
        h.proc.pid, shell.pid
    );
}

/// A same-user process that creates the Host's pipe name first (a squatter): clients refuse it
/// before sending a single byte, and `host serve` reports it instead of claiming a Host runs.
#[test]
fn a_pipe_squatter_is_refused_before_any_data_is_sent() {
    let ep = endpoint("squat");
    let name = pipe_name(&ep);
    // SAFETY: NUL-terminated name; default security (this user); synchronous server handle.
    let server = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(server, INVALID_HANDLE_VALUE);
    let server = server as usize;
    let bytes = Arc::new(AtomicUsize::new(0));
    let conns = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let squatter = std::thread::spawn({
        let (bytes, conns, stop) = (Arc::clone(&bytes), Arc::clone(&conns), Arc::clone(&stop));
        move || {
            let h = server as HANDLE;
            while !stop.load(Ordering::SeqCst) {
                // SAFETY: valid synchronous server handle owned by this test.
                unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
                conns.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 4096];
                loop {
                    let mut n = 0u32;
                    // SAFETY: `buf` is writable; synchronous read by this thread only.
                    let ok = unsafe {
                        ReadFile(h, buf.as_mut_ptr(), 4096, &raw mut n, std::ptr::null_mut())
                    };
                    if ok == 0 || n == 0 {
                        break;
                    }
                    bytes.fetch_add(n as usize, Ordering::SeqCst);
                }
                // SAFETY: as above.
                unsafe { DisconnectNamedPipe(h) };
            }
        }
    });
    for args in [
        &["host", "status", "--endpoint", &ep][..],
        &["host", "health", "--endpoint", &ep],
        &["host", "attach", "--endpoint", &ep, "--dry-run"],
        &["host", "stop", "--endpoint", &ep, "--yes"],
        &["host", "start", "--endpoint", &ep, "--no-shell"],
        &["host", "serve", "--endpoint", &ep, "--no-shell"],
    ] {
        let (code, doc) = run(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (4, Some("host_identity_mismatch")),
            "{args:?}: {doc}"
        );
        let text = doc["error"].to_string();
        assert!(
            text.contains(&format!("server process id: {}", std::process::id())),
            "{text}"
        );
    }
    stop.store(true, Ordering::SeqCst);
    // Unblock the squatter's ConnectNamedPipe with one last, silent connection.
    drop(Raw::try_open(&ep));
    squatter.join().unwrap();
    // SAFETY: the server handle, closed once.
    unsafe { CloseHandle(server as HANDLE) };
    assert!(conns.load(Ordering::SeqCst) >= 6, "every client connected");
    assert_eq!(
        bytes.load(Ordering::SeqCst),
        0,
        "no client sent anything to the squatter (not even hello)"
    );
    eprintln!(
        "EVIDENCE squatter: pipe held by test pid {}: 6 subak commands refused it with host_identity_mismatch, {} connections, 0 bytes sent",
        std::process::id(),
        conns.load(Ordering::SeqCst)
    );
}

/// A sandboxed (low-integrity) process of the same user cannot even open the Host's pipe.
#[test]
fn a_low_integrity_process_of_the_same_user_cannot_open_the_pipe() {
    let h = Host::start("lowil", &["--no-shell", "--idle-exit", "60"]);
    let open_as = |sid: &'static str| {
        let ep = h.ep.clone();
        std::thread::spawn(move || {
            let mut token = std::ptr::null_mut();
            let mut imp = std::ptr::null_mut();
            let mut psid: PSID = std::ptr::null_mut();
            let s = wide(sid);
            // SAFETY: standard token duplication; every out-pointer is writable; the
            // impersonation applies to this short-lived thread only and is reverted.
            unsafe {
                assert_ne!(
                    OpenProcessToken(
                        GetCurrentProcess(),
                        TOKEN_DUPLICATE | TOKEN_QUERY,
                        &raw mut token
                    ),
                    0
                );
                assert_ne!(
                    DuplicateTokenEx(
                        token,
                        0x0200_0000,
                        std::ptr::null(),
                        SecurityImpersonation,
                        TokenImpersonation,
                        &raw mut imp
                    ),
                    0
                );
                assert_ne!(ConvertStringSidToSidW(s.as_ptr(), &raw mut psid), 0);
                let label = TOKEN_MANDATORY_LABEL {
                    Label: SID_AND_ATTRIBUTES {
                        Sid: psid,
                        Attributes: 0x20,
                    }, // SE_GROUP_INTEGRITY
                };
                let size =
                    u32::try_from(size_of::<TOKEN_MANDATORY_LABEL>()).unwrap() + GetLengthSid(psid);
                assert_ne!(
                    SetTokenInformation(imp, TokenIntegrityLevel, (&raw const label).cast(), size),
                    0
                );
                assert_ne!(SetThreadToken(std::ptr::null(), imp), 0);
                let r = Raw::try_open(&ep).map(|_| ());
                RevertToSelf();
                CloseHandle(imp);
                CloseHandle(token);
                r
            }
        })
        .join()
        .unwrap()
    };
    let medium = open_as("S-1-16-8192");
    assert!(
        medium.is_ok(),
        "the test mechanics work at medium integrity: {medium:?}"
    );
    let low = open_as("S-1-16-4096");
    assert_eq!(
        low.as_ref().map_err(std::io::Error::raw_os_error),
        Err(Some(5)),
        "low integrity is denied (ERROR_ACCESS_DENIED): {low:?}"
    );
    eprintln!("EVIDENCE low-integrity: same user at low integrity -> {low:?}; at medium -> ok");
}

#[test]
fn an_idle_host_exits_after_its_grace_period_and_not_before() {
    // No pane, no client: exits after the grace period.
    let h = Host::start("idle", &["--no-shell", "--idle-exit", "2"]);
    let t = Instant::now();
    std::thread::sleep(Duration::from_millis(800));
    assert!(h.proc.alive(), "not before the grace period");
    assert!(h.proc.wait(Duration::from_secs(10)), "exits when idle");
    let idle_exit = t.elapsed();
    // A connected client keeps it alive past the grace period; its disconnect starts the grace.
    let h = Host::start("idle-client", &["--no-shell", "--idle-exit", "2"]);
    let c = Raw::handshake(&h.ep, "gui");
    std::thread::sleep(Duration::from_secs(4));
    assert!(h.proc.alive(), "a client keeps the Host");
    drop(c);
    let t2 = Instant::now();
    assert!(
        h.proc.wait(Duration::from_secs(10)),
        "exits after the last client left"
    );
    let after_client = t2.elapsed();
    // A live pane keeps it too; when its shell exits, the Host idles out.
    let h = Host::start("idle-pane", &["--idle-exit", "1"]);
    std::thread::sleep(Duration::from_secs(3));
    assert!(h.proc.alive(), "a live pane keeps the Host");
    let mut a = Attach::spawn(&h.ep);
    a.send(b"exit 3\r");
    // (Child::wait closes stdin first, which would detach; wait for the pane's end instead.)
    a.expect("exit code 3");
    assert!(a.child.wait().unwrap().success());
    assert!(
        h.proc.wait(Duration::from_secs(10)),
        "exits once the pane ended and nobody is connected"
    );
    let (code, _) = run(&["host", "status", "--endpoint", &h.ep]);
    assert_eq!(code, 3);
    eprintln!(
        "EVIDENCE idle-exit: grace 2 s: no client -> exit after {idle_exit:?}; last client left -> exit after {after_client:?}; pane kept the Host 3 s past a 1 s grace"
    );
}

#[test]
fn the_host_environment_is_the_users_not_the_launchers() {
    let real_profile = std::env::var("USERPROFILE").unwrap();
    let ep = endpoint("env");
    let (code, doc) = run_env(
        &["host", "start", "--endpoint", &ep],
        &[
            ("USERPROFILE", r"C:\bogus-profile-kim"),
            ("HOME", r"C:\bogus-home-kim"),
            ("SUBAK_LEAK_TEST", "secret-value"),
            ("SSH_AUTH_SOCK", r"C:\bogus\agent.sock"),
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let h = Host {
        proc: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
        ep,
    };
    assert_eq!(h.status()["host"]["environment"], "user");
    let mut a = Attach::spawn(&h.ep);
    a.send(
        b"echo [%USERPROFILE%] [%SUBAK_LEAK_TEST%] [%HOME%] [%SSH_AUTH_SOCK%] & echo env-mark^er\r",
    );
    a.expect("env-marker");
    let text = a.text();
    assert!(
        text.contains(&format!("[{real_profile}]")),
        "USERPROFILE is the user's:\n{text}"
    );
    assert!(
        text.contains("[%SUBAK_LEAK_TEST%]"),
        "the launcher's variable is gone:\n{text}"
    );
    assert!(
        text.contains("[%SSH_AUTH_SOCK%]"),
        "a terminal-scoped agent socket is gone:\n{text}"
    );
    assert!(
        !text.contains("secret-value") && !text.contains("bogus"),
        "{text}"
    );
    // cmd's prompt shows its working directory: the user profile, not the launcher's.
    assert!(
        text.to_lowercase()
            .contains(&format!("{}>", real_profile.to_lowercase())),
        "the pane starts in the user profile:\n{text}"
    );
    eprintln!(
        "EVIDENCE environment: launcher had USERPROFILE/HOME/SSH_AUTH_SOCK/SUBAK_LEAK_TEST poisoned; the pane saw USERPROFILE={real_profile}, none of the rest"
    );
}

/// A job object owned by the test: kill-on-close plus `extra` limit flags.
struct Job(HANDLE);

impl Job {
    fn new(extra: u32) -> Self {
        // SAFETY: anonymous job; checked.
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!h.is_null());
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | extra;
        // SAFETY: valid struct of the size passed.
        let ok = unsafe {
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            )
        };
        assert_ne!(ok, 0);
        Self(h)
    }

    fn assign(&self, process: HANDLE) {
        // SAFETY: valid job and process handles.
        assert_ne!(unsafe { AssignProcessToJobObject(self.0, process) }, 0);
    }

    fn contains(&self, p: &Proc) -> bool {
        let mut r = 0;
        // SAFETY: valid handles; `r` is writable.
        unsafe { IsProcessInJob(p.h.as_raw_handle(), self.0, &raw mut r) };
        r != 0
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: owned handle; kill-on-close ends every process still in the job.
        unsafe { CloseHandle(self.0) };
    }
}

/// Runs `subak host start … --json` as a launcher that is placed in `jobs` (outermost first)
/// before it runs a single instruction; returns its envelope.
fn start_in_jobs(ep: &str, jobs: &[&Job]) -> Value {
    let child = Command::new(subak())
        .args([
            "host",
            "start",
            "--endpoint",
            ep,
            "--no-shell",
            "--idle-exit",
            "60",
            "--json",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
        .spawn()
        .unwrap();
    for j in jobs {
        j.assign(child.as_raw_handle());
    }
    // Resume the suspended main thread: suspend once more, resume twice.
    let threads = Proc::open(child.id()).suspend();
    for t in &threads.0 {
        // SAFETY: valid thread handle with suspend/resume access.
        unsafe { ResumeThread(t.as_raw_handle()) };
    }
    drop(threads);
    let out = child.wait_with_output().unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    validate("cli-envelope-v1", &doc);
    assert_eq!(out.status.code(), Some(0), "{doc}");
    doc
}

/// Risk R-03, measured headlessly: breakaway is decided per job level (ADR 0016 §4, ADR 0019).
#[test]
fn breakaway_is_decided_per_job_level() {
    // Silent breakaway: the Host lands outside the launcher's job and survives its close.
    let ep = endpoint("job-silent");
    let job = Job::new(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK);
    let doc = start_in_jobs(&ep, &[&job]);
    assert_eq!(
        doc["data"]["launcher_job"]["silent_breakaway_ok"], true,
        "{doc}"
    );
    let h = Host {
        proc: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
        ep,
    };
    assert!(!job.contains(&h.proc));
    drop(job);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        h.proc.alive(),
        "silent breakaway: the Host outlives the launcher's job"
    );
    let silent_pid = h.proc.pid;
    drop(h);

    // Nested jobs: the inner one allows breakaway, the outer one does not (a terminal whose own
    // job allows it, inside an IDE or a service whose job does not).
    let ep = endpoint("job-nested");
    let outer = Job::new(0);
    let inner = Job::new(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
    let doc = start_in_jobs(&ep, &[&outer, &inner]);
    let d = &doc["data"];
    assert_eq!(
        d["launcher_job"]["breakaway_ok"], true,
        "the launcher sees its immediate job: {doc}"
    );
    assert_eq!(d["host"]["job_breakaway"], true, "{doc}");
    assert_eq!(d["host"]["in_job"], true, "…but the Host is still in a job");
    assert_eq!(
        d["host"]["job"]["breakaway_ok"], false,
        "…which forbids breakaway"
    );
    assert!(
        doc["warnings"][0]
            .as_str()
            .is_some_and(|w| w.contains("R-03")),
        "the user is warned: {doc}"
    );
    let h = Host {
        proc: Proc::open(pid_of(&d["host"]["pid"])),
        ep,
    };
    assert!(!inner.contains(&h.proc) && outer.contains(&h.proc));
    drop(inner);
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.proc.alive(), "closing the inner job is safe");
    drop(outer);
    assert!(
        h.proc.wait(LONG),
        "closing the outer kill-on-close job ends the Host (R-03)"
    );
    eprintln!(
        "EVIDENCE R-03: silent-breakaway job closed, Host {silent_pid} survived; nested jobs: Host {} left the inner job (breakaway ok), stayed in the outer one and died with it",
        h.proc.pid
    );
}
