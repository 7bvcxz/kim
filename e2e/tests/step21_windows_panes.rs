//! E2E — Step 21: internal Windows and Panes of a Terminal (docs/SESSION_MODEL.md, ADR 0054).
//! `SUBAK-FR-010`, `SUBAK-FR-012`, `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-016`,
//! `SUBAK-FR-017`.
//!
//! Scenario A (release `subak`): an agent gives one Terminal three internal windows and five
//! panes, each running its own process (`tasklist`, independent of subak, sees all of them);
//! lists them; selects and renames windows; checks that a pane's output stays in that pane's
//! capture; is refused when it aims a window command at a whole Terminal, at another Terminal's
//! window, or at the last running window; ends one busy pane only after consent (`--yes`) and
//! one window, while every other pane and a second Terminal keep running; and finally kills the
//! Terminal, which ends every remaining pane.
//!
//! Scenario B (release `subak`): a pane whose program exits leaves its window while the other
//! panes run; stopping the Terminal keeps every window and pane as `stopped`.
//!
//! Windows only. Every scenario uses its own endpoint and stops (or kills) what it started;
//! nothing opens a window.
#![cfg(windows)]

use kim_e2e::{TempDir, bin};
use serde_json::Value;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn endpoint(tag: &str) -> String {
    format!("subak-e2e21-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>`: options first, because everything after `--` is the
/// program of a new window or pane.
fn json(exe: &Path, ep: &str, args: &[&str], sb: &TempDir) -> (i32, Value) {
    let out = Command::new(exe)
        .args(["--json", "--endpoint", ep])
        .args(args)
        .env("USERPROFILE", sb.path())
        .env("APPDATA", sb.path())
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), doc)
}

fn ok(exe: &Path, ep: &str, args: &[&str], sb: &TempDir) -> Value {
    let (code, doc) = json(exe, ep, args, sb);
    assert_eq!(code, 0, "{args:?}: {doc}");
    doc["data"].clone()
}

fn fails(exe: &Path, ep: &str, args: &[&str], sb: &TempDir, code: &str) {
    let (exit, doc) = json(exe, ep, args, sb);
    assert_ne!(exit, 0, "{args:?} must fail: {doc}");
    assert_eq!(doc["error"]["code"], code, "{args:?}: {doc}");
}

/// Whether `tasklist` (independent of subak) sees the process.
fn running(pid: u64) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
}

fn wait_gone(pid: u64, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while running(pid) {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// Every pane of a Terminal as (`window.pane`, id, pid).
fn panes(exe: &Path, ep: &str, t: &str, sb: &TempDir) -> Vec<(String, String, u64)> {
    let d = ok(exe, ep, &["pane", "ls", t], sb);
    let mut out = Vec::new();
    for w in d["windows"].as_array().unwrap() {
        for p in w["panes"].as_array().unwrap() {
            out.push((
                format!("{}.{}", w["index"], p["index"]),
                p["id"].as_str().unwrap().to_string(),
                p["pid"].as_u64().unwrap_or(0),
            ));
        }
    }
    out
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: String,
    sb: &'a TempDir,
    pids: Vec<u64>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = json(
            self.exe,
            &self.ep,
            &["host", "stop", "--kill-terminals", "--yes"],
            self.sb,
        );
        for pid in self
            .pids
            .iter()
            .filter(|p| !wait_gone(**p, Duration::from_secs(10)))
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_organizes_one_terminal_into_windows_and_panes_and_ends_only_what_it_means() {
    let subak = bin("subak");
    let sb = TempDir::new("step21-windows");
    let ep = endpoint("windows");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        pids: Vec::new(),
    };
    let (exe, ep) = (subak.as_path(), ep.as_str());
    ok(exe, ep, &["new", "work"], &sb);
    let other = ok(exe, ep, &["new", "other"], &sb);
    ok(exe, ep, &["window", "new", "work", "--name", "build"], &sb);
    ok(
        exe,
        ep,
        &[
            "pane",
            "new",
            "work:build",
            "--command-line",
            "cmd.exe /d /k echo E2E-PANE-MARK",
        ],
        &sb,
    );
    ok(
        exe,
        ep,
        &["window", "new", "work", "--name", "srv", "--no-select"],
        &sb,
    );
    let busy = ok(
        exe,
        ep,
        &[
            "pane",
            "new",
            "work:srv",
            "--",
            "ping.exe",
            "-n",
            "600",
            "127.0.0.1",
        ],
        &sb,
    );
    let all = panes(exe, ep, "work", &sb);
    let places: Vec<&str> = all.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(places, ["0.0", "1.0", "1.1", "2.0", "2.1"]);
    guard.pids.extend(all.iter().map(|(_, _, pid)| *pid));
    for (_, _, pid) in &all {
        assert!(running(*pid), "tasklist sees pane pid {pid}");
    }
    let other_pid = other["terminal"]["windows"][0]["panes"][0]["pid"]
        .as_u64()
        .unwrap();
    guard.pids.push(other_pid);
    // Listing, selection, renaming.
    let w = ok(exe, ep, &["window", "ls", "work"], &sb);
    assert_eq!(w["windows"].as_array().unwrap().len(), 3);
    ok(exe, ep, &["window", "select", "work:srv"], &sb);
    assert_eq!(
        ok(exe, ep, &["info", "work"], &sb)["terminal"]["active_window"],
        w["windows"][2]["id"]
    );
    ok(exe, ep, &["window", "rename", "work:srv", "servers"], &sb);
    // Each pane keeps its own output.
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let c = ok(exe, ep, &["capture", "work:build.1"], &sb);
        if c["lines"].to_string().contains("E2E-PANE-MARK") {
            break;
        }
        assert!(Instant::now() < until, "the marker in its pane");
        std::thread::sleep(Duration::from_millis(200));
    }
    let c = ok(exe, ep, &["capture", "work:0.0"], &sb);
    assert!(!c["lines"].to_string().contains("E2E-PANE-MARK"));
    // The hierarchy holds: no window command ends a Terminal or reaches another Terminal.
    fails(exe, ep, &["window", "kill", "work", "--yes"], &sb, "usage");
    let ow = other["window_id"].as_str().unwrap();
    fails(
        exe,
        ep,
        &["window", "kill", &format!("work:{ow}"), "--yes"],
        &sb,
        "target_not_found",
    );
    fails(
        exe,
        ep,
        &["window", "kill", "other:0", "--yes"],
        &sb,
        "invalid_state",
    );
    // The busy pane needs consent; then only it ends.
    let busy_pid = all
        .iter()
        .find(|(_, id, _)| id == busy["pane_id"].as_str().unwrap())
        .unwrap()
        .2;
    fails(
        exe,
        ep,
        &["pane", "kill", "work:servers.1"],
        &sb,
        "confirmation_required",
    );
    assert!(running(busy_pid));
    ok(exe, ep, &["pane", "kill", "work:servers.1", "--yes"], &sb);
    assert!(wait_gone(busy_pid, Duration::from_secs(15)));
    // One window ends with its panes only.
    let build: Vec<u64> = all
        .iter()
        .filter(|(p, _, _)| p.starts_with("1."))
        .map(|(_, _, pid)| *pid)
        .collect();
    ok(exe, ep, &["window", "kill", "work:build", "--yes"], &sb);
    for pid in &build {
        assert!(wait_gone(*pid, Duration::from_secs(15)), "build pane {pid}");
    }
    let left = panes(exe, ep, "work", &sb);
    assert_eq!(left.len(), 2);
    for (_, _, pid) in &left {
        assert!(running(*pid), "pane {pid} kept running");
    }
    assert!(running(other_pid), "the other Terminal kept running");
    // Killing the Terminal ends every remaining pane, and only its own.
    ok(exe, ep, &["kill", "work", "--yes"], &sb);
    for (_, _, pid) in &left {
        assert!(wait_gone(*pid, Duration::from_secs(15)));
    }
    assert!(running(other_pid));
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_pane_that_exits_leaves_alone_and_stop_keeps_the_topology() {
    let subak = bin("subak");
    let sb = TempDir::new("step21-exit");
    let ep = endpoint("exit");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        pids: Vec::new(),
    };
    let (exe, ep) = (subak.as_path(), ep.as_str());
    ok(exe, ep, &["new", "work"], &sb);
    ok(exe, ep, &["window", "new", "work"], &sb);
    ok(exe, ep, &["pane", "new", "work:1"], &sb);
    let quick = ok(
        exe,
        ep,
        &[
            "pane",
            "new",
            "work:1",
            "--command-line",
            "cmd.exe /d /c exit 4",
        ],
        &sb,
    );
    let gone = quick["pane_id"].as_str().unwrap().to_string();
    let until = Instant::now() + Duration::from_secs(30);
    while panes(exe, ep, "work", &sb)
        .iter()
        .any(|(_, id, _)| *id == gone)
    {
        assert!(Instant::now() < until, "the exited pane left its window");
        std::thread::sleep(Duration::from_millis(200));
    }
    let all = panes(exe, ep, "work", &sb);
    assert_eq!(all.len(), 3);
    guard.pids.extend(all.iter().map(|(_, _, pid)| *pid));
    for (_, _, pid) in &all {
        assert!(running(*pid));
    }
    ok(exe, ep, &["stop", "work", "--yes"], &sb);
    for (_, _, pid) in &all {
        assert!(wait_gone(*pid, Duration::from_secs(15)));
    }
    let t = ok(exe, ep, &["info", "work"], &sb)["terminal"].clone();
    assert_eq!(t["state"], "stopped");
    assert_eq!(t["windows"].as_array().unwrap().len(), 2);
}
