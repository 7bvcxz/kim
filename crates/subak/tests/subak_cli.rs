//! Step 12 — the Subak CLI and headless control on real Windows (docs/SUBAK_CLI.md, ADR 0027,
//! docs/CLI_CONTRACT.md). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-011`…
//! `SUBAK-FR-015`, `SUBAK-FR-060`, `SUBAK-SEC-010`, `KIM-FR-010`, `KIM-FR-012`, `KIM-SEC-013`.
//!
//! Real `subak.exe` processes, each test on its **own** endpoint (the user's Host is never
//! touched), with every option before `--`. Every `--json` document is validated against
//! `cli-envelope-v1` and `subak-cli-v1`; stable ones are compared with the golden files in
//! `docs/contracts/examples/subak-cli-v1/` after volatile values (pids, ids, times, the
//! endpoint) are replaced. Refresh them with `SUBAK_BLESS=1 cargo test -p subak --test
//! subak_cli`, review the diff, and say whether the change is additive (CLI_CONTRACT §8).
//!
//! Attach tests run inside an **outer pseudo console** owned by the test (as Step 08 did): a real,
//! invisible console whose keyboard and output the test controls. A probe — this test binary
//! started again as a detached process that attaches to that console — reads its input and
//! output modes, code page and cursor, or raises Ctrl+Break in it. Nothing opens a window, and
//! every test ends what it started; lines starting with `EVIDENCE` record what was measured
//! (run with `--nocapture`).
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HWND, LPARAM,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, CONSOLE_CURSOR_INFO, COORD, CTRL_BREAK_EVENT, ClosePseudoConsole,
    CreatePseudoConsole, GenerateConsoleCtrlEvent, GetConsoleCursorInfo, GetConsoleMode,
    GetConsoleOutputCP, HPCON, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateProcessW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
};

/// PowerShell's first start on a cold machine can take long (Step 11).
const SLOW: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------------------------
// CLI runs, schema validation and golden files.

/// `subak --json --endpoint <ep> <args>`: validated against cli-envelope-v1 (by `run`) and
/// subak-cli-v1; stderr must stay empty.
fn sub(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    let (code, doc) = run(&a);
    validate("subak-cli-v1", &doc);
    (code, doc)
}

/// `subak <args> --endpoint <ep>` in human mode.
fn human(ep: &str, args: &[&str]) -> Output {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    Command::new(subak())
        .args(&a)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak")
}

fn text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Replaces what differs between runs and machines: ids (stable placeholders in order of first
/// appearance), pids, times, counters, the endpoint and the user profile.
fn normalize(doc: &Value, ep: &str) -> Value {
    fn walk(v: &mut Value, key: &str, ids: &mut Vec<String>, ep: &str, home: &str) {
        match v {
            Value::Object(m) => {
                for (k, child) in m.iter_mut() {
                    walk(child, k, ids, ep, home);
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, key, ids, ep, home);
                }
            }
            Value::Number(_) => {
                let fixed: Option<u64> = match key {
                    "pid" => Some(4242),
                    "uptime_s" => Some(7),
                    "latency_ms" => Some(3),
                    "output_bytes" => Some(1024),
                    "stream_id" => Some(1),
                    // Step 13: history counters and the cursor depend on how far the shell got.
                    "lines" | "bytes" | "total_lines" | "evicted_lines" | "x" | "y" => Some(0),
                    "id" => Some(1), // client ids
                    _ => None,
                };
                if let Some(f) = fixed {
                    *v = f.into();
                }
            }
            Value::String(s) => {
                let mut t = s.replace(ep, "subak-example");
                // Process ids inside messages ("root pid 1234").
                t = regex::Regex::new(r"\bpid \d+")
                    .unwrap()
                    .replace_all(&t, "pid 4242")
                    .into_owned();
                if !home.is_empty() {
                    t = t.replace(home, r"C:\Users\me");
                }
                let original = t.clone();
                for (i, _) in original.match_indices(['t', 'w', 'p']) {
                    // Replacements keep the length, so positions in `original` stay valid.
                    let cand = t.get(i..i + 14).unwrap_or("").to_string();
                    let cand = cand.as_str();
                    if cand.len() == 14
                        && cand.as_bytes()[1] == b'-'
                        && cand[2..].bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        let n = match ids.iter().position(|x| x == cand) {
                            Some(n) => n,
                            None => {
                                ids.push(cand.to_string());
                                ids.len() - 1
                            }
                        };
                        let fake = format!("{}-{:012x}", &cand[..1], n + 1);
                        t = t.replace(cand, &fake);
                    }
                }
                if matches!(key, "created_at" | "started_at") {
                    t = "2026-11-04T01:02:03Z".into();
                }
                *s = t;
            }
            _ => {}
        }
    }
    let mut out = doc.clone();
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    walk(&mut out, "", &mut Vec::new(), ep, &home);
    out
}

fn golden(name: &str, doc: &Value, ep: &str) {
    let actual = normalize(doc, ep);
    let path = repo().join(format!("docs/contracts/examples/subak-cli-v1/{name}.json"));
    let rendered = format!("{}\n", serde_json::to_string_pretty(&actual).unwrap());
    if std::env::var_os("SUBAK_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    assert_eq!(
        expected,
        actual,
        "{} differs from the real output. If intended, run `SUBAK_BLESS=1 cargo test -p subak \
         --test subak_cli`, review the diff and note whether it is additive (CLI_CONTRACT §8).\n\
         actual:\n{rendered}",
        path.display()
    );
}

/// The ids of every visible top-level window owned by one of `pids`.
fn visible_windows(pids: &[u32]) -> Vec<u32> {
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: `lparam` is the `&mut Vec<(HWND, u32)>` passed below, alive during the call.
        let out = unsafe { &mut *(lparam as *mut Vec<(HWND, u32)>) };
        let mut pid = 0u32;
        // SAFETY: valid window handle from EnumWindows; `pid` is writable.
        unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
        out.push((hwnd, pid));
        1
    }
    let mut all: Vec<(HWND, u32)> = Vec::new();
    // SAFETY: the callback only touches `all` through `lparam` while EnumWindows runs.
    unsafe { EnumWindows(Some(each), (&raw mut all) as LPARAM) };
    all.into_iter()
        // SAFETY: plain query of a window handle that may already be gone (then false).
        .filter(|(h, p)| pids.contains(p) && unsafe { IsWindowVisible(*h) } != 0)
        .map(|(_, p)| p)
        .collect()
}

fn pane_pid(created: &Value) -> u32 {
    pid_of(&created["data"]["terminal"]["windows"][0]["panes"][0]["pid"])
}

/// A fresh endpoint and, on drop, the Host behind it stopped with its Terminals (or killed).
struct Ep {
    ep: String,
}

impl Ep {
    fn new(tag: &str) -> Self {
        Self {
            ep: endpoint(&format!("cli12-{tag}")),
        }
    }
}

impl Drop for Ep {
    fn drop(&mut self) {
        let (code, doc) = run(&["host", "status", "--endpoint", &self.ep]);
        if code != 0 {
            return;
        }
        let host = Proc::try_open(pid_of(&doc["data"]["host"]["pid"]));
        let _ = run(&[
            "host",
            "stop",
            "--endpoint",
            &self.ep,
            "--kill-terminals",
            "--yes",
        ]);
        if let Some(h) = host
            && !h.wait(Duration::from_secs(15))
        {
            h.kill();
        }
    }
}

/// Waits until `f` holds.
fn until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + limit;
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------------------------
// The JSON contract.

/// The shared definitions of subak-cli-v1 are those of ipc-v1 and tool-capabilities-v1, copied
/// verbatim; the deprecated spelling cannot appear as a command name.
#[test]
fn the_cli_schema_embeds_the_ipc_and_capabilities_definitions_unchanged() {
    let cli = schema("subak-cli-v1");
    let ipc = schema("ipc-v1");
    let caps = schema("tool-capabilities-v1");
    for name in [
        "u64",
        "version_string",
        "timestamp",
        "terminal_id",
        "window_id",
        "pane_id",
        "terminal_name",
        "terminal_state",
        "end_cause",
        "size",
        "command",
        "env_names",
        "pane",
        "window",
        "terminal",
        "terminal_summary",
        "pane_end_report",
        "end_plan",
        "result_terminal_info",
        "result_terminal_rename",
        "result_terminal_end",
        "client_attachment",
        "detach_terminal",
        // Step 13.
        "history",
        "screen_info",
        "result_pane_capture",
        "result_pane_clear_history",
        "scrollback",
    ] {
        assert_eq!(cli["$defs"][name], ipc["$defs"][name], "$defs.{name}");
    }
    assert_eq!(
        cli["$defs"]["would_create"],
        ipc["$defs"]["result_terminal_create"]["oneOf"][1]["properties"]["would_create"]
    );
    for name in ["tool_id", "semver", "command_name", "capability"] {
        assert_eq!(cli["$defs"][name], caps["$defs"][name], "$defs.{name}");
    }
    let mut doc = caps.clone();
    for k in ["$schema", "$id", "title", "$defs"] {
        doc.as_object_mut().unwrap().remove(k);
    }
    assert_eq!(cli["$defs"]["capabilities"], doc);
    // `subak capabilities --json` names exactly the commands the schema types.
    let (code, d) = run(&["capabilities"]);
    assert_eq!(code, 0);
    validate("subak-cli-v1", &d);
    let typed: Vec<String> = cli["$defs"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.starts_with("ok_"))
        .flat_map(|(_, v)| {
            let c = &v["allOf"][1]["properties"]["command"];
            c["enum"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| vec![c["const"].clone()])
        })
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for c in d["data"]["commands"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        assert!(
            typed.iter().any(|t| t == name),
            "{name} has no ok_ definition"
        );
    }
}

/// Every command's `--json` output on a live Host, validated and compared with golden files;
/// exit codes equal `error.exit_code`; a secret never reaches stdout or stderr.
#[test]
fn every_command_answers_one_schema_valid_document_with_stable_exit_codes() {
    let e = Ep::new("json");
    let ep = e.ep.as_str();
    let secret = "s3cr3t-step12-never-printed";
    let windows = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());

    let (code, doc) = sub(ep, &["status"]);
    assert_eq!(code, 0, "{doc}");
    golden("status-no-host", &doc, ep);
    let (code, doc) = sub(ep, &["new", "x", "--no-start"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("host_not_running"))
    );
    golden("error-host-not-running-no-start", &doc, ep);

    // The first `new` starts the Host on demand (under the start lock, ADR 0019 §5).
    let (code, created) = sub(
        ep,
        &["new", "work", "--cwd", &windows, "--", "cmd.exe", "/d"],
    );
    assert_eq!(code, 0, "{created}");
    assert_eq!(created["data"]["host"]["started"], true);
    golden("new", &created, ep);
    let host = Proc::open(pid_of(&created["data"]["host"]["pid"]));
    let pane = Proc::open(pane_pid(&created));

    let (code, doc) = sub(ep, &["new", "WORK"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("name_conflict"))
    );
    golden("error-name-conflict", &doc, ep);
    let (code, doc) = sub(ep, &["ls"]);
    assert_eq!(code, 0);
    golden("ls", &doc, ep);
    let (code, doc) = sub(ep, &["info", "work"]);
    assert_eq!(code, 0);
    golden("info", &doc, ep);
    let (code, doc) = sub(ep, &["info", "nope"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );
    golden("error-target-not-found", &doc, ep);

    // Step 13: history counters (never content), a dry-run clear, and a capture of a pane
    // with known content (kept after its program exited), redacted by default.
    let (code, doc) = sub(ep, &["history", "work"]);
    assert_eq!(code, 0, "{doc}");
    golden("history", &doc, ep);
    let (code, doc) = sub(ep, &["clear-history", "work", "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    golden("clear-history-dry-run", &doc, ep);
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "echo",
            "--remain-on-exit",
            "--size",
            "40x4",
            "--cwd",
            &windows,
            "--command-line",
            &format!("cmd.exe /d /c \"echo golden-1& echo token={secret}\""),
        ],
    );
    assert_eq!(code, 0, "{doc}");
    wait_state(ep, "echo", "stopped");
    let (code, doc) = sub(ep, &["capture", "echo"]);
    assert_eq!(code, 0, "{doc}");
    assert!(!doc.to_string().contains(secret), "capture redacts: {doc}");
    golden("capture", &doc, ep);
    let (code, doc) = sub(ep, &["capture", "echo", "--no-redact"]);
    assert_eq!(code, 0, "{doc}");
    assert!(doc.to_string().contains(secret), "--no-redact shows it");
    let (code, doc) = sub(ep, &["rm", "echo", "--yes"]);
    assert_eq!(code, 0, "{doc}");

    for args in [
        &["status"][..],
        &["host", "status"],
        &["host", "health"],
        &["attach", "work", "--dry-run"],
        &["attach", "--dry-run"],
        &["host", "attach", "--dry-run"],
        &["attach", "work", "--dry-run", "--read-only"],
    ] {
        let (code, doc) = sub(ep, args);
        assert_eq!(code, 0, "{args:?}: {doc}");
    }
    let (_, doc) = sub(ep, &["status"]);
    assert_eq!(doc["data"]["running"], true);
    assert_eq!(doc["data"]["host"]["pid"], u64::from(host.pid));
    assert!(
        doc["data"]["clients"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["id"].as_u64().is_some_and(|i| i > 0))
    );

    let (code, doc) = sub(ep, &["rename", "work", "build", "--dry-run"]);
    assert_eq!(code, 0);
    golden("rename-dry-run", &doc, ep);
    let (code, doc) = sub(ep, &["rename", "work", "build"]);
    assert_eq!(code, 0);
    golden("rename", &doc, ep);
    let (code, doc) = sub(ep, &["detach", "build", "--dry-run"]);
    assert_eq!(code, 0);
    golden("detach-dry-run", &doc, ep);
    let (code, doc) = sub(ep, &["detach", "build"]);
    assert_eq!(code, 0);
    golden("detach", &doc, ep);
    let (code, doc) = sub(ep, &["detach", "--client", "999999"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );

    // The deprecated spelling: the same command, its canonical name, and a warning.
    let (code, doc) = sub(ep, &["terminal", "list"]);
    assert_eq!((code, doc["command"].as_str()), (0, Some("ls")));
    assert!(doc["warnings"][0].as_str().unwrap().contains("deprecated"));
    golden("ls-deprecated-alias", &doc, ep);

    // A verbatim command line: shown with secret-looking words hidden, everywhere.
    let line = format!(r#"cmd.exe /d /s /c "echo "a  b" & echo --token={secret}""#);
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "raw",
            "--dry-run",
            "--cwd",
            &windows,
            "--command-line",
            &line,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    golden("new-command-line-dry-run", &doc, ep);
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "keep",
            "--cwd",
            &windows,
            "--command-line",
            &format!("cmd.exe /d /k rem --token={secret}"),
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let shown = &doc["data"]["terminal"]["windows"][0]["panes"][0];
    assert_eq!(shown["command_line"], "cmd.exe /d /k rem --token=***");
    for args in [&["info", "keep"][..], &["ls"], &["status"]] {
        let out = human(ep, args);
        let (o, er) = text(&out);
        assert!(
            !o.contains(secret) && !er.contains(secret),
            "{args:?} leaked: {o}{er}"
        );
        let (_, doc) = sub(ep, args);
        assert!(!doc.to_string().contains(secret), "{args:?} leaked in JSON");
    }
    assert!(
        text(&human(ep, &["info", "keep"]))
            .0
            .contains("--token=*** (verbatim)")
    );

    // Destructive: refused without consent (exit 7), planned with --dry-run, done with --yes.
    let (code, doc) = sub(ep, &["kill", "build", "--dry-run"]);
    assert_eq!(code, 0);
    golden("kill-dry-run", &doc, ep);
    let (code, doc) = sub(ep, &["kill", "build", "--no-input"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    golden("error-confirmation-required", &doc, ep);
    let out = Command::new(subak())
        .args(["--endpoint", ep, "kill", "build"])
        .env("KIM_NO_INPUT", "1")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7), "KIM_NO_INPUT never asks");
    assert!(pane.alive(), "nothing was killed without consent");
    let (code, doc) = sub(ep, &["kill", "build", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    golden("kill", &doc, ep);
    assert!(pane.wait(Duration::from_secs(10)));
    let (code, doc) = sub(ep, &["kill", "keep", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    for args in [
        &["host", "stop", "--dry-run"][..],
        &["host", "stop", "--yes"],
    ] {
        let (code, doc) = sub(ep, args);
        assert_eq!(code, 0, "{args:?}: {doc}");
    }
    assert!(host.wait(Duration::from_secs(15)), "the Host exited");
    eprintln!(
        "EVIDENCE json: every command validated against subak-cli-v1; host={} pane={}",
        host.pid, pane.pid
    );
}

// ---------------------------------------------------------------------------------------------
// Acceptance: create, attach, detach, list, rename and kill with the CLI only.

/// Spawns `subak <args>` with piped stdin and output (the Attach harness shape) and waits for
/// the attach banner.
fn attach_with(ep: &str, extra: &[&str]) -> Attach {
    let mut args = vec!["--endpoint", ep, "attach", "--size", "100x30"];
    args.extend_from_slice(extra);
    let mut child = Command::new(subak())
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("attach");
    let out = Arc::new(Mutex::new(Vec::new()));
    for mut stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().unwrap()),
    ] {
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }
    let a = Attach { child, out };
    a.expect("Ctrl+B d detaches");
    a
}

fn exits_with(a: &mut Attach, limit: Duration) -> Option<i32> {
    let end = Instant::now() + limit;
    loop {
        if let Some(s) = a.child.try_wait().unwrap() {
            return s.code();
        }
        if Instant::now() > end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_user_creates_attaches_detaches_lists_renames_and_kills_with_the_cli_only() {
    let e = Ep::new("journey");
    let ep = e.ep.as_str();
    // Human mode, as typed: `new` starts the Host on demand and prints how to attach.
    let out = human(ep, &["new", "work"]);
    let (o, er) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{o}{er}");
    assert!(
        o.contains("Terminal `work`") && o.contains("started Host pid"),
        "{o}"
    );
    assert!(o.contains("attach with: subak attach"), "{o}");
    let (_, info) = sub(ep, &["info", "work"]);
    let shell = Proc::open(pid_of(
        &info["data"]["terminal"]["windows"][0]["panes"][0]["pid"],
    ));
    let (_, st) = sub(ep, &["status"]);
    let host = Proc::open(pid_of(&st["data"]["host"]["pid"]));
    assert!(
        visible_windows(&[host.pid, shell.pid]).is_empty(),
        "no window, no GUI process"
    );

    // Attach (piped input, as an agent would), type, and get detached from another terminal.
    let mut a = attach_with(ep, &["work"]);
    a.send(b"echo mark^er-journey-1\r");
    a.expect("marker-journey-1");
    let (code, doc) = sub(ep, &["status"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["clients"]["attached"], 1);
    let out = human(ep, &["detach", "work"]);
    let (o, er) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{o}{er}");
    assert!(
        o.contains("detached 1 attachment(s) of `work`") && o.contains("nothing was stopped"),
        "{o}"
    );
    assert_eq!(exits_with(&mut a, LONG), Some(0), "{}", a.text());
    assert!(
        a.text().contains("detached from `work` by another client"),
        "{}",
        a.text()
    );
    assert!(shell.alive() && host.alive(), "detach stopped nothing");
    let (_, doc) = sub(ep, &["info", "work"]);
    assert_eq!(doc["data"]["terminal"]["state"], "running");
    assert_eq!(doc["data"]["terminal"]["clients"], 0);

    // Two clients; detach exactly one of them by its id.
    let mut a1 = attach_with(ep, &["work"]);
    let mut a2 = attach_with(ep, &["work", "--read-only"]);
    until("two attached clients", LONG, || {
        sub(ep, &["status"]).1["data"]["clients"]["attached"] == 2
    });
    let (_, st) = sub(ep, &["status"]);
    let viewer = st["data"]["clients"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["attached"] == true && c["interactive"] == false)
        .expect("the read-only client")
        .clone();
    a2.send(b"echo viewer-typed\r");
    a1.send(b"echo mark^er-journey-1b\r");
    a1.expect("marker-journey-1b");
    a2.expect("marker-journey-1b"); // the viewer sees the output
    let id = viewer["id"].as_u64().unwrap().to_string();
    let (code, doc) = sub(ep, &["detach", "--client", &id]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["detached"].as_array().unwrap().len(), 1);
    assert_eq!(doc["data"]["detached"][0]["interactive"], false);
    assert_eq!(exits_with(&mut a2, LONG), Some(0));
    assert!(
        a1.child.try_wait().unwrap().is_none(),
        "the other client stays attached"
    );
    a1.send(b"echo mark^er-journey-2\r");
    a1.expect("marker-journey-2");
    // Read-only means read-only: what the viewer typed never reached the shell.
    assert!(!a1.text().contains("viewer-typed"));

    // Rename keeps the attachment and the process; then detach with the keys.
    let (code, doc) = sub(ep, &["rename", "work", "build"]);
    assert_eq!(code, 0, "{doc}");
    a1.send(b"echo mark^er-journey-3\r");
    a1.expect("marker-journey-3");
    a1.send(&[0x02, b'd']);
    assert_eq!(exits_with(&mut a1, LONG), Some(0));
    assert!(
        a1.text().contains("detached; the shell keeps running"),
        "{}",
        a1.text()
    );
    let out = human(ep, &["ls"]);
    let (o, _) = text(&out);
    assert!(
        o.contains("build") && o.contains("running") && !o.contains("work"),
        "{o}"
    );

    // Kill: consent, then the shell is gone and the Terminal removed.
    let out = human(ep, &["kill", "build", "--yes"]);
    let (o, er) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{o}{er}");
    assert!(o.contains("Terminal `build` removed"), "{o}");
    assert!(shell.wait(Duration::from_secs(10)), "the shell ended");
    let (_, doc) = sub(ep, &["ls"]);
    assert_eq!(doc["data"]["terminals"], json!([]));
    assert!(visible_windows(&[host.pid]).is_empty());
    eprintln!(
        "EVIDENCE journey: host={} shell={} new/attach/detach(target, client id)/rename/Ctrl+B d/kill, no window",
        host.pid, shell.pid
    );
}

/// Acceptance: an attached client that is killed (or whose output goes away) leaves the work in
/// the Host running; a new client sees it continue.
#[test]
fn client_exit_during_attach_leaves_host_work_running() {
    let e = Ep::new("survive");
    let ep = e.ep.as_str();
    let (code, created) = sub(ep, &["new", "busy", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    let shell = Proc::open(pane_pid(&created));
    let mut a = attach_with(ep, &["busy"]);
    a.send(b"ping -n 120 127.0.0.1\r");
    a.expect("Reply from 127.0.0.1");
    // The client dies abruptly (TerminateProcess): no detach, no goodbye.
    a.child.kill().unwrap();
    let _ = a.child.wait();
    until("the killed client to be gone", LONG, || {
        sub(ep, &["status"]).1["data"]["clients"]["attached"] == 0
    });
    let (_, doc) = sub(ep, &["info", "busy"]);
    let p = &doc["data"]["terminal"]["windows"][0]["panes"][0];
    assert_eq!(doc["data"]["terminal"]["state"], "running");
    assert!(
        p["processes"].as_u64().unwrap() >= 2,
        "ping still runs under the shell: {p}"
    );
    assert_eq!(doc["data"]["terminal"]["busy"], true);
    let before = p["output_bytes"].as_u64().unwrap();
    until("output to keep growing", LONG, || {
        sub(ep, &["info", "busy"]).1["data"]["terminal"]["windows"][0]["panes"][0]["output_bytes"]
            .as_u64()
            .unwrap()
            > before
    });
    assert!(shell.alive());
    // A client whose own output goes away (its reader closed) detaches; nothing stops either.
    let mut child = Command::new(subak())
        .args(["--endpoint", ep, "attach", "busy"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let end = Instant::now() + LONG;
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < end,
            "a client without a reader did not end"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // A new client sees the same work go on (replay, then live output).
    let mut b = attach_with(ep, &["busy"]);
    b.expect("Reply from 127.0.0.1");
    b.send(&[0x03]);
    b.send(b"echo mark^er-after-^C\r");
    b.expect("marker-after-C");
    b.send(&[0x02, b'd']);
    assert_eq!(exits_with(&mut b, LONG), Some(0));
    assert!(shell.alive(), "still running after every client went");
    eprintln!(
        "EVIDENCE survive: shell={} kept running ping after a killed client",
        shell.pid
    );
}

/// Concurrent commands: one Host for concurrent `new` calls, one holder of a name, and reads
/// that never fail while Terminals are created and removed.
#[test]
fn concurrent_commands_share_one_host_and_never_duplicate_a_name() {
    let e = Ep::new("race");
    let ep = e.ep.clone();
    let n = 6;
    let gate = Arc::new(Barrier::new(n));
    let results: Vec<(i32, Value)> = (0..n)
        .map(|_| {
            let (ep, gate) = (ep.clone(), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                sub(&ep, &["new", "same", "--", "cmd.exe", "/d"])
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    let ok: Vec<&Value> = results
        .iter()
        .filter(|(c, _)| *c == 0)
        .map(|(_, d)| d)
        .collect();
    assert_eq!(ok.len(), 1, "{results:?}");
    for (code, doc) in results.iter().filter(|(c, _)| *c != 0) {
        assert_eq!(
            (*code, doc["error"]["code"].as_str()),
            (7, Some("name_conflict"))
        );
    }
    let host_pid = ok[0]["data"]["host"]["pid"].clone();
    let (_, st) = sub(&ep, &["status"]);
    assert_eq!(
        st["data"]["host"]["pid"], host_pid,
        "one Host for every starter"
    );

    // Unnamed creates, listings and status calls at once.
    let gate = Arc::new(Barrier::new(2 * n));
    let threads: Vec<JoinHandle<(i32, Value)>> = (0..2 * n)
        .map(|i| {
            let (ep, gate) = (ep.clone(), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                match i % 3 {
                    0 => sub(&ep, &["new", "--", "cmd.exe", "/d"]),
                    1 => sub(&ep, &["ls"]),
                    _ => sub(&ep, &["status"]),
                }
            })
        })
        .collect();
    for t in threads {
        let (code, doc) = t.join().unwrap();
        assert_eq!(code, 0, "{doc}");
    }
    let (_, doc) = sub(&ep, &["ls"]);
    let names: Vec<String> = doc["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    let mut unique = names.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "no duplicate names: {names:?}");
    assert_eq!(names.len(), 1 + 4, "{names:?}");

    // Three kills of one Terminal at once: it ends once; the others find it gone or share it.
    let gate = Arc::new(Barrier::new(3));
    let kills: Vec<(i32, Value)> = (0..3)
        .map(|_| {
            let (ep, gate) = (ep.clone(), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                sub(&ep, &["kill", "same", "--yes"])
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    assert!(kills.iter().any(|(c, _)| *c == 0), "{kills:?}");
    for (code, doc) in &kills {
        assert!(
            *code == 0 || doc["error"]["code"] == "target_not_found",
            "{doc}"
        );
    }
    let (code, _) = sub(&ep, &["info", "same"]);
    assert_eq!(code, 3);
}

/// IPC `client.detach` (ADR 0027 §3) with raw clients: every message the Host sends is valid
/// ipc-v1 (the raw client validates it), results match `result_client_detach`, the detached
/// connection is told with `pane.detached`, and nothing but attachments changes.
#[test]
fn client_detach_over_ipc_ends_only_attachments_and_tells_the_detached_client() {
    let e = Ep::new("ipcdetach");
    let ep = e.ep.as_str();
    let (code, created) = sub(ep, &["new", "work", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    let shell = Proc::open(pane_pid(&created));
    // A GUI-like client that subscribed to `panes`, and a read-only viewer that did not.
    let gui = Raw::handshake(ep, "gui");
    assert_eq!(
        gui.request(1, "events.subscribe", &json!({"topics": ["panes"]}))["ok"],
        true
    );
    let r = gui.request(
        2,
        "pane.attach",
        &json!({"target": "work", "interactive": true}),
    );
    ipc_def("result_pane_attach", &r["result"]);
    let stream = r["result"]["stream_id"].clone();
    let viewer = Raw::handshake(ep, "cli");
    let r = viewer.request(
        1,
        "pane.attach",
        &json!({"target": "work", "interactive": false}),
    );
    assert_eq!(r["ok"], true, "{r}");

    let ctl = Raw::handshake(ep, "cli");
    let r = ctl.request(
        1,
        "client.detach",
        &json!({"target": "work", "dry_run": true}),
    );
    ipc_def("result_client_detach", &r["result"]);
    assert_eq!(
        r["result"]["would_detach"].as_array().unwrap().len(),
        2,
        "{r}"
    );
    let st = ctl.request(2, "host.status", &json!({}));
    let clients = st["result"]["clients"]["list"].as_array().unwrap().clone();
    let ids: Vec<u64> = clients.iter().map(|c| c["id"].as_u64().unwrap()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "ordered by id: {ids:?}"
    );
    let viewer_id = clients
        .iter()
        .find(|c| c["attached"] == true && c["interactive"] == false)
        .unwrap()["id"]
        .clone();
    // One client by id: only the viewer goes.
    let r = ctl.request(3, "client.detach", &json!({"client_id": viewer_id}));
    ipc_def("result_client_detach", &r["result"]);
    let gone = r["result"]["detached"].as_array().unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(gone[0]["client"]["id"], viewer_id);
    assert!(r["result"]["terminal"].is_null());
    // Every client of the Terminal: the GUI client goes and is told so, before its output stops.
    let r = ctl.request(4, "client.detach", &json!({"target": "work"}));
    ipc_def("result_client_detach", &r["result"]);
    assert_eq!(r["result"]["detached"].as_array().unwrap().len(), 1);
    assert_eq!(r["result"]["terminal"]["name"], "work");
    let end = Instant::now() + LONG;
    let event = loop {
        match gui.next_control(end.saturating_duration_since(Instant::now())) {
            Got::Control(m) if m["event"] == "pane.detached" => break m,
            Got::Control(_) => {}
            other => panic!("no pane.detached: {other:?}"),
        }
    };
    assert_eq!(event["data"]["stream_id"], stream);
    assert_eq!(event["data"]["by"]["kind"], "cli");
    // Nothing else changed: both connections are still served, the shell runs.
    assert_eq!(gui.request(9, "host.health", &json!({}))["ok"], true);
    assert_eq!(viewer.request(9, "host.health", &json!({}))["ok"], true);
    let st = ctl.request(5, "host.status", &json!({}));
    assert_eq!(st["result"]["clients"]["attached"], 0);
    assert!(shell.alive());
    let info = ctl.request(6, "terminal.info", &json!({"target": "work"}));
    assert_eq!(info["result"]["terminal"]["state"], "running");
    // Refusals: nothing named, bad ids, unknown targets, and MCP without the Step 37 grant.
    for (id, params, code) in [
        (7, json!({}), "invalid_params"),
        (8, json!({"client_id": 0}), "invalid_params"),
        (10, json!({"client_id": "1"}), "invalid_params"),
        (11, json!({"client_id": 999_999}), "target_not_found"),
        (12, json!({"target": "nope"}), "target_not_found"),
        (
            13,
            json!({"target": "work", "dry_run": "yes"}),
            "invalid_params",
        ),
    ] {
        let r = ctl.request(id, "client.detach", &params);
        assert_eq!(error_code(&r), code, "{params}");
    }
    let mcp = Raw::handshake(ep, "mcp");
    let r = mcp.request(1, "client.detach", &json!({"target": "work"}));
    assert_eq!(error_code(&r), "unauthorized");
}

// ---------------------------------------------------------------------------------------------
// The verbatim command line (ADR 0027 §4) against cmd.exe's real parsing.

fn wait_state(ep: &str, target: &str, state: &str) -> Value {
    let end = Instant::now() + LONG;
    loop {
        let (_, doc) = sub(ep, &["info", target]);
        if doc["data"]["terminal"]["state"] == state {
            return doc;
        }
        assert!(Instant::now() < end, "{target} never became {state}: {doc}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn verbatim_command_lines_reach_cmd_exe_exactly_as_written() {
    let e = Ep::new("rawline");
    let ep = e.ep.as_str();
    let dir = std::env::temp_dir().join(format!("subak-step12-raw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.txt");
    // cmd's own rules: /s strips the outer quotes, inner quotes group "a  b" (two blanks kept),
    // ^& is a literal &, & separates commands, and the redirection target is quoted.
    let inner = format!(r#"(echo "a  b"&echo ^&x&echo two)>"{}""#, out.display());
    let line = format!(r#"cmd.exe /d /s /c "{inner}""#);
    let (code, doc) = sub(
        ep,
        &["new", "raw", "--remain-on-exit", "--command-line", &line],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(
        doc["data"]["terminal"]["windows"][0]["panes"][0]["command_line"],
        line.as_str()
    );
    let done = wait_state(ep, "raw", "stopped");
    assert_eq!(
        done["data"]["terminal"]["last_exit"]["exit_code"], 0,
        "{done}"
    );
    let written = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines, [r#""a  b""#, "&x", "two"], "{written:?}");

    // The same text as separate words is quoted by the MSVC rules (`\"`), which cmd does not
    // understand: the redirection target is mangled and nothing is written. This is why the
    // verbatim mode exists.
    let out2 = dir.join("out2.txt");
    let inner2 = format!(r#"(echo "a  b"&echo ^&x&echo two)>"{}""#, out2.display());
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "words",
            "--remain-on-exit",
            "--",
            "cmd.exe",
            "/d",
            "/s",
            "/c",
            &inner2,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let done = wait_state(ep, "words", "stopped");
    assert_ne!(
        done["data"]["terminal"]["last_exit"]["exit_code"], 0,
        "{done}"
    );
    assert!(!out2.exists(), "argv quoting cannot express this cmd line");

    // Only the first word is looked up, as for every program: a script is still refused, and a
    // missing program fails before anything starts.
    let script = dir.join("s.cmd");
    std::fs::write(&script, "@echo hi\r\n").unwrap();
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "--command-line",
            &format!("\"{}\" arg", script.display()),
        ],
    );
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (1, Some("spawn_failed")),
        "{doc}"
    );
    assert!(
        doc["error"]["details"]
            .to_string()
            .contains("not_executable")
    );
    let (code, doc) = sub(ep, &["new", "--command-line", r"C:\subak-missing\x.exe /c"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (1, Some("spawn_failed"))
    );
    // The dry run checks the same and starts nothing.
    let (code, doc) = sub(ep, &["new", "--dry-run", "--command-line", &line]);
    assert_eq!(code, 0, "{doc}");
    assert!(
        doc["data"]["would_create"]["executable"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .ends_with(r"\cmd.exe")
    );
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("EVIDENCE rawline: cmd.exe got {line:?} verbatim and wrote {lines:?}");
}

/// The e2e run 2 finding (Step 13 scenario, fixed in Step 16): a token in a command line was
/// shown verbatim by `new` and `info` while `capture` hid it, because commands had a weaker rule
/// set. Every place that shows a command now uses the capture's detector (`redact.rs`).
#[test]
fn secrets_in_commands_are_hidden_everywhere_a_command_is_shown() {
    let e = Ep::new("cmdsecret");
    let ep = e.ep.as_str();
    let gh = "ghp_e2e13e2e13e2e13e2e13e2e13e2e13e2e13e2e13";
    let sk = format!("sk-ant-api03-{}", "Q".repeat(40));
    let line = format!(r#"cmd.exe /d /c "echo {gh}& echo password=x""#);
    let leaks = |doc: &Value, what: &str| {
        let s = doc.to_string();
        assert!(
            !s.contains("e2e13e2e13") && !s.contains("QQQQQQQQ") && !s.contains("password=x"),
            "{what} shows a secret: {s}"
        );
    };
    // The exact e2e case: `new` and `info` with the verbatim command line.
    let (code, created) = sub(
        ep,
        &["new", "sec", "--remain-on-exit", "--command-line", &line],
    );
    assert_eq!(code, 0, "{created}");
    let p = &created["data"]["terminal"]["windows"][0]["panes"][0];
    assert_eq!(
        p["command_line"],
        r#"cmd.exe /d /c "echo ghp_***& echo password=***""#
    );
    assert_eq!(p["command"]["args"][2], "echo ghp_***& echo password=***");
    leaks(&created, "new");
    let (_, info) = sub(ep, &["info", "sec"]);
    leaks(&info, "info");
    let p = &info["data"]["terminal"]["windows"][0]["panes"][0];
    assert_eq!(p["command"]["args"][2], "echo ghp_***& echo password=***");
    // Word-by-word commands, a dry run, and the lists.
    let (code, doc) = sub(
        ep,
        &[
            "new",
            "words",
            "--",
            "cmd.exe",
            "/d",
            "/k",
            "rem",
            &sk,
            "--token",
            "abc123",
            "-H",
            "Authorization: Bearer abcdefghijkl",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    leaks(&doc, "new (words)");
    assert!(!doc.to_string().contains("abcdefghijkl"), "{doc}");
    let args = &doc["data"]["terminal"]["windows"][0]["panes"][0]["command"]["args"];
    assert_eq!(args[3], "sk-***");
    assert_eq!(args[5], "***");
    assert_eq!(args[7], "Authorization: Bearer ***");
    let (code, dry) = sub(ep, &["new", "--dry-run", "--command-line", &line]);
    assert_eq!(code, 0, "{dry}");
    leaks(&dry, "new --dry-run");
    for args in [&["ls"][..], &["status"], &["host", "status"]] {
        let (code, doc) = sub(ep, args);
        assert_eq!(code, 0, "{args:?}: {doc}");
        leaks(&doc, &args.join(" "));
    }
    // The capture hides the same token the same way (one detector).
    wait_state(ep, "sec", "stopped");
    let (code, cap) = sub(ep, &["capture", "sec"]);
    assert_eq!(code, 0, "{cap}");
    assert!(cap.to_string().contains("ghp_***"), "{cap}");
    leaks(&cap, "capture");
}

// ---------------------------------------------------------------------------------------------
// TTY attach inside an outer pseudo console, and what the console looks like afterwards.

/// A test-owned pseudo console with its own input and output pipes; several processes can be
/// started in it, as in one terminal window.
struct OuterConsole {
    hpc: HPCON,
    input: File,
    out: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    procs: Vec<Proc>,
    mark: usize,
}

fn pipe_pair() -> (HANDLE, HANDLE) {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: both out-pointers are valid; no inheritance.
    let ok = unsafe { CreatePipe(&raw mut r, &raw mut w, std::ptr::null(), 0) };
    assert_ne!(ok, 0);
    (r, w)
}

impl OuterConsole {
    fn new(cols: i16, rows: i16) -> Self {
        let (in_r, in_w) = pipe_pair();
        let (out_r, out_w) = pipe_pair();
        let mut hpc: HPCON = 0;
        // SAFETY: valid pipe ends; `hpc` receives the pseudo console.
        let hr = unsafe {
            CreatePseudoConsole(COORD { X: cols, Y: rows }, in_r, out_w, 0, &raw mut hpc)
        };
        assert!(hr >= 0, "CreatePseudoConsole: 0x{hr:08X}");
        // SAFETY: conhost duplicated both; ours are no longer needed.
        unsafe {
            CloseHandle(in_r);
            CloseHandle(out_w);
        }
        // SAFETY: fresh pipe handles, owned from here on.
        let (input, mut output) =
            unsafe { (File::from_raw_handle(in_w), File::from_raw_handle(out_r)) };
        let out = Arc::new(Mutex::new(Vec::new()));
        let reader = std::thread::spawn({
            let out = Arc::clone(&out);
            move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = output.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    out.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        Self {
            hpc,
            input,
            out,
            reader: Some(reader),
            procs: Vec::new(),
            mark: 0,
        }
    }

    /// Starts `cmdline` attached to this pseudo console (with `flags`, e.g. a new process group).
    fn start(&mut self, cmdline: &str, flags: u32) -> u32 {
        let mut size = 0usize;
        // SAFETY: size query.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size) };
        let mut attr = vec![0u64; size.div_ceil(8)];
        // SAFETY: buffer of `size` bytes; for this attribute the value is the HPCON itself.
        let ok = unsafe {
            InitializeProcThreadAttributeList(attr.as_mut_ptr().cast(), 1, 0, &raw mut size) != 0
                && UpdateProcThreadAttribute(
                    attr.as_mut_ptr().cast(),
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    self.hpc as *const c_void,
                    size_of::<HPCON>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ) != 0
        };
        assert!(ok);
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES; // null std handles: use the console
        si.lpAttributeList = attr.as_mut_ptr().cast();
        let mut cmd = wide(cmdline);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: writable command line; live startup info; no handle inheritance.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | flags,
                std::ptr::null(),
                std::ptr::null(),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        // SAFETY: the list was initialized above.
        unsafe { DeleteProcThreadAttributeList(attr.as_mut_ptr().cast()) };
        assert_ne!(ok, 0, "CreateProcessW {cmdline}");
        // SAFETY: fresh handles; the thread handle is not needed, the process handle is owned.
        let h = unsafe {
            CloseHandle(pi.hThread);
            OwnedHandle::from_raw_handle(pi.hProcess)
        };
        self.procs.push(Proc {
            pid: pi.dwProcessId,
            h,
        });
        pi.dwProcessId
    }

    fn proc(&self, pid: u32) -> &Proc {
        self.procs.iter().find(|p| p.pid == pid).unwrap()
    }

    /// Types `bytes` on the console's keyboard.
    fn keys(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }

    /// Waits for `needle` in what appeared since the last match.
    fn expect_within(&mut self, needle: &str, limit: Duration) -> String {
        let end = Instant::now() + limit;
        loop {
            let text = visible(&self.out.lock().unwrap()[self.mark..]);
            if text.contains(needle) {
                self.mark = self.out.lock().unwrap().len();
                return text;
            }
            assert!(Instant::now() < end, "`{needle}` not seen:\n{text}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn expect(&mut self, needle: &str) -> String {
        self.expect_within(needle, LONG)
    }
}

impl Drop for OuterConsole {
    fn drop(&mut self) {
        for p in &self.procs {
            if p.alive() {
                p.kill();
            }
        }
        // SAFETY: created in `new`, closed once; the reader keeps draining meanwhile.
        unsafe { ClosePseudoConsole(self.hpc) };
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

/// What the probe saw in a console.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modes {
    input: u32,
    output: u32,
    cp: u32,
    cursor: bool,
}

/// Runs this test binary as the probe ([`probe_helper`]) attached to the console of `pid`.
fn probe(pid: u32, action: &str) -> Option<Modes> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["probe_helper", "--exact", "--nocapture", "--test-threads=1"])
        .env("SUBAK_TEST_PROBE_PID", pid.to_string())
        .env("SUBAK_TEST_PROBE_ACTION", action)
        .stdin(Stdio::null())
        .creation_flags(DETACHED_PROCESS)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // libtest prints `test probe_helper ... ` first, on the same line.
    let line = text
        .lines()
        .find_map(|l| l.find("PROBE ").map(|i| &l[i..]))
        .unwrap_or_else(|| {
            panic!(
                "the probe of {pid} printed nothing usable:\n{text}\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let field = |k: &str| {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(k))
            .unwrap_or_else(|| panic!("probe output: {line}"))
            .to_string()
    };
    if action != "modes" {
        return None;
    }
    let hex = |k: &str| u32::from_str_radix(field(k).trim_start_matches("0x"), 16).unwrap();
    Some(Modes {
        input: hex("in="),
        output: hex("out="),
        cp: field("cp=").parse().unwrap(),
        cursor: field("cursor=") == "true",
    })
}

/// Not a test of its own: the probe that [`probe`] starts. Without its environment variables it
/// does nothing (as in every normal run).
#[test]
fn probe_helper() {
    let Ok(pid) = std::env::var("SUBAK_TEST_PROBE_PID") else {
        return;
    };
    let pid: u32 = pid.parse().unwrap();
    let action = std::env::var("SUBAK_TEST_PROBE_ACTION").unwrap_or_default();
    // This process was started detached (no console); attach to the target's (retried briefly,
    // because a process that just started may not have its console yet).
    let mut tries = 0;
    loop {
        // SAFETY: plain call; on failure the thread's last error says why.
        if unsafe { AttachConsole(pid) } != 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        tries += 1;
        assert!(tries < 50, "AttachConsole({pid}): {err}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let open = |name: &str| {
        // SAFETY: opens the attached console's buffer by its reserved name.
        unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        }
    };
    if let Some(group) = action.strip_prefix("break:") {
        unsafe extern "system" fn ignore(_: u32) -> windows_sys::core::BOOL {
            1
        }
        // SAFETY: a handler that ignores the event for this probe; then the event is raised in
        // the console for one process group (the client's own).
        unsafe {
            SetConsoleCtrlHandler(Some(ignore), 1);
            assert_ne!(
                GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, group.parse().unwrap()),
                0
            );
        }
        std::thread::sleep(Duration::from_millis(500));
        println!("PROBE break sent");
        return;
    }
    let (hin, hout) = (open("CONIN$"), open("CONOUT$"));
    let (mut i, mut o) = (0u32, 0u32);
    let mut cursor = CONSOLE_CURSOR_INFO::default();
    // SAFETY: handles of the attached console; out-pointers are writable.
    let cp = unsafe {
        GetConsoleMode(hin, &raw mut i);
        GetConsoleMode(hout, &raw mut o);
        GetConsoleCursorInfo(hout, &raw mut cursor);
        GetConsoleOutputCP()
    };
    println!(
        "PROBE in=0x{i:x} out=0x{o:x} cp={cp} cursor={}",
        cursor.bVisible != 0
    );
}

/// The input-mode bits raw mode clears (line input, echo, processed input).
const COOKED: u32 = 0x0002 | 0x0004 | 0x0001;

#[test]
fn console_attach_restores_the_console_on_every_exit_path_it_survives() {
    let e = Ep::new("tty");
    let ep = e.ep.as_str();
    let (code, created) = sub(ep, &["new", "work", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    let shell = Proc::open(pane_pid(&created));
    let mut con = OuterConsole::new(100, 30);
    // A process that keeps the console alive and never reads its input.
    let keeper = con.start("cmd.exe /d /c ping -n 900 127.0.0.1 >NUL", 0);
    // cmd changes the console mode while it starts (measured: ENABLE_MOUSE_INPUT goes off and
    // on again); the baseline is taken once its ping runs and the mode is settled.
    until("the keeper's ping", LONG, || {
        processes()
            .iter()
            .any(|(_, parent, exe)| *parent == keeper && exe.eq_ignore_ascii_case("PING.EXE"))
    });
    let mut before = probe(keeper, "modes").expect("baseline");
    until("a settled console mode", LONG, || {
        let again = probe(keeper, "modes").unwrap();
        std::mem::replace(&mut before, again) == again
    });
    eprintln!("EVIDENCE tty-baseline: {before:?}");
    assert_eq!(before.input & COOKED, COOKED, "a new console is cooked");
    let attach = format!("\"{}\" attach work --endpoint {ep}", subak());

    // 1. Ctrl+B d.
    let client = con.start(&attach, CREATE_NEW_PROCESS_GROUP);
    con.expect("Ctrl+B d detaches");
    let during = probe(keeper, "modes").unwrap();
    eprintln!("EVIDENCE tty-attached: {during:?}");
    assert_eq!(during.input & COOKED, 0, "raw while attached");
    assert_eq!(during.cp, 65001);
    // The pane hides the cursor (as full-screen programs do) and changes its prompt.
    con.keys(b"prompt $E[?25l$P$G\r");
    con.keys(b"echo mark^er-tty-1\r");
    con.expect("marker-tty-1");
    until("the hidden cursor to reach this console", LONG, || {
        !probe(keeper, "modes").unwrap().cursor
    });
    // Ctrl+C is a key for the pane: ping is interrupted, the client lives on.
    con.keys(b"ping -n 60 127.0.0.1\r");
    con.expect("Pinging 127.0.0.1");
    con.keys(&[0x03]);
    con.keys(b"echo mark^er-tty-2\r");
    con.expect("marker-tty-2");
    assert!(con.proc(client).alive(), "Ctrl+C did not end the client");
    con.keys(&[0x02, b'd']);
    assert!(con.proc(client).wait(LONG), "Ctrl+B d ends the client");
    assert_eq!(con.proc(client).exit_code(), Some(0));
    let after = probe(keeper, "modes").unwrap();
    eprintln!("EVIDENCE tty-after-ctrl-b-d: {after:?}");
    assert_eq!(
        after,
        Modes {
            cursor: true,
            ..before
        },
        "everything put back"
    );

    // 2. Detached by another client (`subak detach`).
    let client = con.start(&attach, CREATE_NEW_PROCESS_GROUP);
    con.expect("Ctrl+B d detaches");
    let (code, doc) = sub(ep, &["detach", "work"]);
    assert_eq!(code, 0, "{doc}");
    assert!(con.proc(client).wait(LONG));
    assert_eq!(con.proc(client).exit_code(), Some(0));
    con.expect("by another client");
    let after = probe(keeper, "modes").unwrap();
    eprintln!("EVIDENCE tty-after-remote-detach: {after:?}");
    assert_eq!(
        after,
        Modes {
            cursor: true,
            ..before
        }
    );

    // 3. Ctrl+Break: a signal, not a key; the client detaches cleanly.
    let client = con.start(&attach, CREATE_NEW_PROCESS_GROUP);
    con.expect("Ctrl+B d detaches");
    probe(keeper, &format!("break:{client}"));
    assert!(con.proc(client).wait(LONG), "Ctrl+Break detaches");
    assert_eq!(con.proc(client).exit_code(), Some(0));
    let after = probe(keeper, "modes").unwrap();
    eprintln!("EVIDENCE tty-after-ctrl-break: {after:?}");
    assert_eq!(
        after,
        Modes {
            cursor: true,
            ..before
        }
    );

    // 4. Killed (TerminateProcess): no code of the client runs, so whatever it set stays until
    //    the shell around it resets it (see the shell test below). Recorded, not hidden.
    let client = con.start(&attach, CREATE_NEW_PROCESS_GROUP);
    con.expect("Ctrl+B d detaches");
    con.proc(client).kill();
    let after_kill = probe(keeper, "modes").unwrap();
    eprintln!(
        "EVIDENCE tty-after-kill: {after_kill:?} (raw input stays: {})",
        after_kill.input & COOKED == 0
    );

    assert!(shell.alive(), "no exit path stopped the Terminal");
    let (_, doc) = sub(ep, &["info", "work"]);
    assert_eq!(doc["data"]["terminal"]["state"], "running");
    assert!(visible_windows(&[keeper]).is_empty());
}

/// From an interactive cmd.exe and Windows PowerShell 5.1: attach, type, detach, and the shell
/// takes the next command normally, also after the client was killed.
#[test]
fn cmd_and_windows_powershell_keep_working_after_an_attach_ends_or_is_killed() {
    let e = Ep::new("shells");
    let ep = e.ep.as_str();
    let (code, created) = sub(ep, &["new", "work", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    let shell = Proc::open(pane_pid(&created));
    let exe = subak();
    for (name, root, launch, alive) in [
        (
            "cmd",
            "cmd.exe /d /q".to_string(),
            format!("\"{exe}\" attach work --endpoint {ep}\r"),
            "echo outer-^alive-",
        ),
        (
            "powershell",
            "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass".to_string(),
            format!("& '{exe}' attach work --endpoint {ep}\r"),
            "'outer-' + 'alive-' + ",
        ),
    ] {
        let mut con = OuterConsole::new(110, 30);
        let root_pid = con.start(&root, 0);
        // Wait for the shell to take input.
        con.keys(format!("{alive}0\r").as_bytes());
        con.expect_within("outer-alive-0", SLOW);
        con.keys(launch.as_bytes());
        con.expect("Ctrl+B d detaches");
        con.keys(format!("echo mark^er-{name}-in\r").as_bytes());
        con.expect(&format!("marker-{name}-in"));
        con.keys(&[0x02, b'd']);
        con.expect("detached; the shell keeps running");
        con.keys(format!("{alive}1\r").as_bytes());
        con.expect("outer-alive-1");
        let detached = probe(root_pid, "modes").unwrap();

        // Killed while attached: the next command still works.
        con.keys(launch.as_bytes());
        con.expect("Ctrl+B d detaches");
        let client = processes()
            .into_iter()
            .find(|(_, parent, exe)| *parent == root_pid && exe.eq_ignore_ascii_case("subak.exe"))
            .map(|(pid, ..)| Proc::open(pid))
            .expect("the attached client");
        client.kill();
        con.keys(format!("{alive}2\r").as_bytes());
        con.expect_within("outer-alive-2", SLOW);
        let killed = probe(root_pid, "modes").unwrap();
        eprintln!(
            "EVIDENCE shell-{name}: after detach {detached:?}; after kill and the next command {killed:?} (cooked: {})",
            killed.input & COOKED == COOKED
        );
        // The probe sees the mode the shell set for its own prompt: cmd reads cooked lines
        // (and resets the console after a killed child), PSReadLine reads raw keys itself. What
        // matters, and was checked above, is that both took the next command.
        if name == "cmd" {
            assert_eq!(
                detached.input & COOKED,
                COOKED,
                "cmd: cooked after a detach"
            );
            assert_eq!(
                killed.input & COOKED,
                COOKED,
                "cmd resets a killed child's mode"
            );
        }
        assert!(visible_windows(&[root_pid, client.pid]).is_empty());
    }
    assert!(shell.alive(), "the Terminal survived every client");
}

trait ExitCode {
    fn exit_code(&self) -> Option<u32>;
}

impl ExitCode for Proc {
    fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: valid process handle; `code` is writable.
        let ok = unsafe {
            windows_sys::Win32::System::Threading::GetExitCodeProcess(
                std::os::windows::io::AsRawHandle::as_raw_handle(&self.h),
                &raw mut code,
            )
        };
        (ok != 0 && code != 259).then_some(code)
    }
}
