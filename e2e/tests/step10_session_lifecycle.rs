//! E2E — Step 10: the Terminal session lifecycle (docs/SESSION_MODEL.md, ADR 0020).
//! `SUBAK-FR-003`, `SUBAK-FR-010`…`SUBAK-FR-016`, `SUBAK-SEC-001`.
//!
//! Scenario A (release `subak`): an agent manages named Terminals headlessly on its own endpoint.
//! It creates two (one starting the Host on demand), lists and renames them, sees a duplicate
//! name and a program that cannot start refused without leftovers, attaches with piped input,
//! types and detaches without stopping anything, is refused when it does not confirm, stops one
//! (kept as `stopped`), removes it, kills the other, and finally needs `--kill-terminals` to stop
//! the Host. `tasklist` (independent of subak) confirms what runs at each point.
//!
//! Scenario B (release `subak`): six agents create a Terminal with the same name at the same
//! moment: exactly one succeeds, the others get `name_conflict`, and one pane process exists.
//!
//! Windows only. Every scenario uses its own endpoint and stops (or kills) what it started.
#![cfg(windows)]

use kim_e2e::{TempDir, bin};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn endpoint(tag: &str) -> String {
    format!("subak-e2e10-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>`: options first, because everything after `--` is the
/// command of a new Terminal.
fn json(exe: &Path, ep: &str, args: &[&str], sb: &TempDir) -> (i32, Value) {
    let out = Command::new(exe)
        .args(["--json", "--endpoint", ep])
        .args(args)
        .env("USERPROFILE", sb.path())
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

fn pane_pid(created: &Value) -> u64 {
    created["data"]["terminal"]["windows"][0]["panes"][0]["pid"]
        .as_u64()
        .unwrap()
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

/// `subak attach` with piped stdin and output and no window.
struct Client {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Client {
    fn attach(exe: &Path, ep: &str, target: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["attach", target, "--endpoint", ep, "--size", "100x30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("attach");
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut stdout = child.stdout.take().unwrap();
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Self { child, out }
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn expect(&self, needle: &str) {
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            if String::from_utf8_lossy(&self.out.lock().unwrap()).contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "`{needle}` not seen:\n{}",
            String::from_utf8_lossy(&self.out.lock().unwrap())
        );
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_manages_named_terminals_without_losing_running_work() {
    let subak = bin("subak");
    let sb = TempDir::new("step10-lifecycle");
    let ep = endpoint("lifecycle");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        pids: Vec::new(),
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);

    // No Host yet: nothing is listed and nothing is started to find out.
    let (code, doc) = run(&["ls"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["terminals"].as_array().map(Vec::len), Some(0));
    // The first create starts the Host on demand.
    let (code, build) = run(&["new", "build"]);
    assert_eq!(code, 0, "{build}");
    assert_eq!(build["data"]["host"]["started"], true);
    let host_pid = build["data"]["host"]["pid"].as_u64().unwrap();
    let build_pid = pane_pid(&build);
    guard.pids.extend([host_pid, build_pid]);
    let (code, server) = run(&["new", "서버"]);
    assert_eq!(code, 0, "{server}");
    let server_pid = pane_pid(&server);
    guard.pids.push(server_pid);
    assert_ne!(build_pid, server_pid, "each Terminal has its own process");

    // Names are unique ignoring case; a failed start leaves nothing.
    let (code, doc) = run(&["new", "BUILD"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("name_conflict"))
    );
    let (code, doc) = run(&["new", "--", r"C:\subak-e2e-missing\nothing.exe"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (1, Some("spawn_failed")),
        "{doc}"
    );
    let (_, doc) = run(&["ls"]);
    let names: Vec<&str> = doc["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["build", "서버"],
        "creation order, nothing half-created"
    );
    let (code, doc) = run(&["rename", "build", "Build-1"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["terminal_id"], build["data"]["terminal_id"]);

    // Attach, type, detach: nothing stops.
    let mut a = Client::attach(&subak, &ep, "Build-1");
    a.send(b"echo e2e-step10-mark^er\r");
    a.expect("e2e-step10-marker");
    a.send(&[0x02, b'd']);
    assert!(a.child.wait().unwrap().success(), "Ctrl+B d detaches");
    assert!(running(build_pid), "detach stops nothing (tasklist)");

    // Destructive actions are explicit: refused without consent, then done as asked.
    let (code, doc) = run(&["stop", "build-1", "--no-input"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, doc) = run(&["rm", "build-1", "--yes"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("invalid_state")),
        "rm never ends a running Terminal"
    );
    assert!(running(build_pid));
    let (code, doc) = run(&["stop", "build-1", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["terminal"]["state"], "stopped");
    assert!(wait_gone(build_pid, Duration::from_secs(15)));
    assert!(running(server_pid), "the other Terminal is untouched");
    let (code, doc) = run(&["rm", "build-1", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["removed"], true);

    // host stop with a live Terminal needs --kill-terminals (and consent).
    let (code, doc) = run(&["host", "stop", "--yes"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("terminals_running"))
    );
    assert!(running(server_pid) && running(host_pid));
    let (code, doc) = run(&["kill", "서버", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert!(wait_gone(server_pid, Duration::from_secs(15)));
    let (code, doc) = run(&["host", "stop"]);
    assert_eq!(code, 0, "no live Terminal: nothing to confirm: {doc}");
    assert!(wait_gone(host_pid, Duration::from_secs(15)));
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn six_agents_creating_one_name_at_once_get_exactly_one_terminal() {
    let subak = bin("subak");
    let sb = TempDir::new("step10-race");
    let ep = endpoint("race");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        pids: Vec::new(),
    };
    let (code, doc) = json(
        &subak,
        &ep,
        &["host", "start", "--no-shell", "--idle-exit", "60"],
        &sb,
    );
    assert_eq!(code, 0, "{doc}");
    guard
        .pids
        .push(doc["data"]["host"]["pid"].as_u64().unwrap());
    let agents: Vec<Child> = (0..6)
        .map(|_| {
            Command::new(&subak)
                .args(["--json", "--endpoint", &ep, "new", "shared"])
                .env("USERPROFILE", sb.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap()
        })
        .collect();
    let results: Vec<(i32, Value)> = agents
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().unwrap();
            (
                out.status.code().unwrap(),
                serde_json::from_slice(&out.stdout).unwrap(),
            )
        })
        .collect();
    let won: Vec<&Value> = results
        .iter()
        .filter(|(c, _)| *c == 0)
        .map(|(_, d)| d)
        .collect();
    assert_eq!(won.len(), 1, "{results:?}");
    guard.pids.push(pane_pid(won[0]));
    assert!(
        results
            .iter()
            .filter(|(c, _)| *c != 0)
            .all(|(c, d)| *c == 7 && d["error"]["code"] == "name_conflict"),
        "{results:?}"
    );
    let (_, doc) = json(&subak, &ep, &["ls"], &sb);
    assert_eq!(doc["data"]["terminals"].as_array().map(Vec::len), Some(1));
}
