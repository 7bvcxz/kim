//! E2E — Step 08: Windows ConPTY and the detached Host (docs/CONPTY_PROOF.md, ADR 0016).
//!
//! Scenario A (release `subak`): a user starts a Host, works in its shell through an attached
//! client, the client dies, the Host and shell keep their process ids (checked independently with
//! `tasklist`), a new client reattaches to the same shell state, detaches with `Ctrl+B d`, and an
//! explicit, confirmed stop ends everything. The Host writes nothing into the user's data
//! directories (sandboxed).
//!
//! Scenario B (release `subak`): an agent discovers the prototype through `capabilities --json`
//! and plans a start with `--dry-run` without starting anything.
//!
//! Windows only. Every scenario uses its own endpoint and stops (or kills) what it started.
#![cfg(windows)]

use kim_e2e::{TempDir, bin, run_sandboxed};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn endpoint(tag: &str) -> String {
    format!("subak-e2e-{tag}-{}", std::process::id())
}

fn json(exe: &Path, args: &[&str], sb: &TempDir) -> (i32, Value) {
    let mut a = args.to_vec();
    a.push("--json");
    let out = run_sandboxed(exe, &a, sb);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{a:?}: not JSON ({e}): {}",
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

/// An attach client with piped stdin/stdout and no window.
struct Client {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Client {
    fn attach(exe: &Path, ep: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["host", "attach", "--endpoint", ep, "--size", "100x30"])
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

/// Stops (or kills) the Host however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: String,
    sb: &'a TempDir,
    host_pid: Option<u64>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = run_sandboxed(
            self.exe,
            &["host", "stop", "--endpoint", &self.ep, "--yes"],
            self.sb,
        );
        if let Some(pid) = self.host_pid.filter(|p| running(*p)) {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
        }
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn host_and_shell_outlive_a_killed_client_and_reattach_until_an_explicit_stop() {
    let subak = bin("subak");
    let sb = TempDir::new("step08-host");
    let ep = endpoint("survive");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        host_pid: None,
    };

    let (code, doc) = json(&subak, &["host", "start", "--endpoint", &ep], &sb);
    assert_eq!(code, 0, "{doc}");
    let host_pid = doc["data"]["host"]["pid"].as_u64().unwrap();
    let shell_pid = doc["data"]["pane"]["pid"].as_u64().unwrap();
    guard.host_pid = Some(host_pid);
    assert!(running(host_pid) && running(shell_pid));

    let mut a = Client::attach(&subak, &ep);
    a.send(b"set E2E=al^ive\r");
    a.send(b"echo mark^er-e2e-1\r");
    a.expect("marker-e2e-1");
    drop(a); // the client is killed

    let (code, st) = json(&subak, &["host", "status", "--endpoint", &ep], &sb);
    assert_eq!(code, 0, "{st}");
    assert_eq!(st["data"]["host"]["pid"].as_u64(), Some(host_pid));
    assert_eq!(st["data"]["pane"]["pid"].as_u64(), Some(shell_pid));
    assert_eq!(st["data"]["clients"]["attached"], 0);
    assert!(
        running(host_pid) && running(shell_pid),
        "tasklist still sees both"
    );

    let mut b = Client::attach(&subak, &ep);
    b.send(b"echo %E2E%-mark^er-e2e-2\r");
    b.expect("alive-marker-e2e-2");
    b.send(&[0x02, b'd']);
    let status = b.child.wait().unwrap();
    assert!(status.success(), "Ctrl+B d detaches with exit 0");

    let (code, doc) = json(
        &subak,
        &["host", "stop", "--endpoint", &ep, "--no-input"],
        &sb,
    );
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, doc) = json(&subak, &["host", "stop", "--endpoint", &ep, "--yes"], &sb);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["host"]["exited"], true);
    let (code, _) = json(&subak, &["host", "status", "--endpoint", &ep], &sb);
    assert_eq!(code, 3, "host_not_running after stop");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!running(host_pid) && !running(shell_pid));
    assert_eq!(
        sb.entry_count(),
        0,
        "the Host wrote nothing to the user's data"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn agent_discovers_the_host_prototype_and_plans_without_starting() {
    let subak = bin("subak");
    let sb = TempDir::new("step08-plan");
    let (code, caps) = json(&subak, &["capabilities"], &sb);
    assert_eq!(code, 0);
    let names: Vec<&str> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    for n in [
        "host.start",
        "host.status",
        "host.attach",
        "host.stop",
        "host.serve",
    ] {
        assert!(names.contains(&n), "{n}");
    }
    let ep = endpoint("plan");
    let (code, plan) = json(
        &subak,
        &["host", "start", "--endpoint", &ep, "--dry-run"],
        &sb,
    );
    assert_eq!(code, 0, "{plan}");
    assert!(
        plan["data"]["would_start"]["creation_flags"]
            .to_string()
            .contains("DETACHED_PROCESS")
    );
    let (code, doc) = json(&subak, &["host", "status", "--endpoint", &ep], &sb);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("host_not_running"))
    );
    assert_eq!(sb.entry_count(), 0);
}
