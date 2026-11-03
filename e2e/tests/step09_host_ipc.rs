//! E2E — Step 09: the secure Host supervisor and IPC v1 (docs/contracts/IPC_PROTOCOL.md,
//! ADR 0019). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-SEC-010`, `SUBAK-SEC-012`.
//!
//! Scenario A (release `subak`): an agent starts a Host on demand, checks it with `host health`,
//! and finds that six concurrent starts gave one Host. It confirms the Host runs with the
//! user's logon environment even though the agent's own environment is sandboxed, and that the
//! Host exits by itself after its idle grace period.
//!
//! Scenario B (release `subak`): two terminals share one pane; one is killed and the other keeps
//! working; an explicit, confirmed stop ends the pane and the Host.
//!
//! Scenario C (release `subak`): another program (a PowerShell process) holds the pipe name
//! first; every `subak` command refuses it with `host_identity_mismatch` and sends it nothing.
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
    format!("subak-e2e09-{tag}-{}", std::process::id())
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

/// An attach client with piped stdin and output and no window.
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

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_starts_one_healthy_host_on_demand_that_idles_out() {
    let subak = bin("subak");
    let sb = TempDir::new("step09-ondemand");
    let ep = endpoint("ondemand");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        sb: &sb,
        host_pid: None,
    };
    // Six agents start the Host at the same moment: exactly one Host results.
    let starters: Vec<Child> = (0..6)
        .map(|_| {
            Command::new(&subak)
                .args([
                    "host",
                    "start",
                    "--endpoint",
                    &ep,
                    "--no-shell",
                    "--idle-exit",
                    "3",
                    "--json",
                ])
                .env("USERPROFILE", sb.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap()
        })
        .collect();
    let docs: Vec<(i32, Value)> = starters
        .into_iter()
        .map(|c| {
            let out = c.wait_with_output().unwrap();
            (
                out.status.code().unwrap(),
                serde_json::from_slice(&out.stdout).unwrap(),
            )
        })
        .collect();
    let winners: Vec<&Value> = docs
        .iter()
        .filter(|(c, _)| *c == 0)
        .map(|(_, d)| d)
        .collect();
    assert_eq!(winners.len(), 1, "{docs:?}");
    let pid = winners[0]["data"]["host"]["pid"].as_u64().unwrap();
    guard.host_pid = Some(pid);
    assert!(
        docs.iter()
            .filter(|(c, _)| *c != 0)
            .all(|(c, d)| *c == 7 && d["error"]["code"] == "host_already_running"),
        "{docs:?}"
    );
    // A heartbeat: the endpoint is verified to be subak.exe of this user, and it answers.
    let (code, h) = json(&subak, &["host", "health", "--endpoint", &ep], &sb);
    assert_eq!(code, 0, "{h}");
    assert_eq!(h["data"]["healthy"], true);
    assert_eq!(h["data"]["host"]["pid"].as_u64(), Some(pid));
    assert_eq!(h["data"]["identity"]["image"], "subak.exe");
    let (code, st) = json(&subak, &["host", "status", "--endpoint", &ep], &sb);
    assert_eq!(code, 0, "{st}");
    assert_eq!(st["data"]["protocol"], 1);
    assert_eq!(
        st["data"]["host"]["environment"], "user",
        "the Host uses the user's logon environment, not the agent's sandbox"
    );
    // With no pane and no client, the Host exits by itself after its 3 s grace period.
    assert!(
        wait_gone(pid, Duration::from_secs(20)),
        "the idle Host exits"
    );
    let (code, _) = json(&subak, &["host", "status", "--endpoint", &ep], &sb);
    assert_eq!(code, 3, "host_not_running afterwards");
    assert_eq!(sb.entry_count(), 0, "nothing was written to the sandbox");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn two_terminals_share_a_pane_and_a_killed_one_changes_nothing() {
    let subak = bin("subak");
    let sb = TempDir::new("step09-share");
    let ep = endpoint("share");
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
    let mut a = Client::attach(&subak, &ep);
    let mut b = Client::attach(&subak, &ep);
    a.send(b"echo e2e-from-a-mark^er\r");
    a.expect("e2e-from-a-marker");
    b.expect("e2e-from-a-marker");
    drop(a); // one terminal is killed
    b.send(b"echo e2e-from-b-mark^er\r");
    b.expect("e2e-from-b-marker");
    assert!(running(host_pid) && running(shell_pid));
    b.send(&[0x02, b'd']);
    assert!(b.child.wait().unwrap().success(), "Ctrl+B d detaches");
    let (code, doc) = json(
        &subak,
        &["host", "stop", "--endpoint", &ep, "--no-input"],
        &sb,
    );
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required")),
        "ending a live pane needs consent"
    );
    let (code, doc) = json(&subak, &["host", "stop", "--endpoint", &ep, "--yes"], &sb);
    assert_eq!(code, 0, "{doc}");
    assert!(
        wait_gone(host_pid, Duration::from_secs(15))
            && wait_gone(shell_pid, Duration::from_secs(15))
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_program_holding_the_pipe_name_is_refused_and_sent_nothing() {
    let subak = bin("subak");
    let sb = TempDir::new("step09-squat");
    let ep = endpoint("squat");
    // A same-user squatter in another process: it creates the pipe first and reports how many
    // bytes each client sent before disconnecting.
    let script = format!(
        "$s = New-Object System.IO.Pipes.NamedPipeServerStream('{ep}', 'InOut', 1); \
         [Console]::Out.WriteLine('READY'); [Console]::Out.Flush(); \
         for ($i = 0; $i -lt 3; $i++) {{ $s.WaitForConnection(); $n = 0; $b = New-Object byte[] 4096; \
         while (($r = $s.Read($b, 0, 4096)) -gt 0) {{ $n += $r }}; \
         [Console]::Out.WriteLine(\"BYTES $n\"); [Console]::Out.Flush(); $s.Disconnect() }}"
    );
    let mut squatter = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("powershell");
    let mut out = squatter.stdout.take().unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 256];
    while !String::from_utf8_lossy(&seen).contains("READY") {
        let n = out.read(&mut buf).unwrap();
        assert!(n > 0, "the squatter did not start");
        seen.extend_from_slice(&buf[..n]);
    }
    for args in [
        &["host", "status", "--endpoint", &ep][..],
        &["host", "health", "--endpoint", &ep],
        &["host", "start", "--endpoint", &ep, "--no-shell"],
    ] {
        let (code, doc) = json(&subak, args, &sb);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (4, Some("host_identity_mismatch")),
            "{args:?}: {doc}"
        );
    }
    let mut rest = String::new();
    let _ = out.read_to_string(&mut rest);
    let _ = squatter.kill();
    let _ = squatter.wait();
    let counts: Vec<&str> = rest.lines().filter(|l| l.starts_with("BYTES")).collect();
    assert_eq!(counts, ["BYTES 0", "BYTES 0", "BYTES 0"], "{rest}");
}
