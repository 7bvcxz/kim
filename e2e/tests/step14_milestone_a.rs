//! E2E — Step 14: Milestone A, persistent sessions and reconnection
//! (docs/MILESTONE_A_VALIDATION.md). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`,
//! `SUBAK-FR-015`, `SUBAK-FR-016`.
//!
//! Scenario A (release `subak`): `subak new` starts the Host on demand with a cmd.exe, a Windows
//! PowerShell and a PowerShell stand-in for an interactive program that remembers what it was
//! told. Clients are killed, detached with Ctrl+B d, by the end of their input and from outside
//! (`subak detach`), 50 attach/detach cycles follow, and after every step `tasklist` (independent
//! of subak) and `subak ls` still see the same processes. Reattached clients find the stand-in's
//! memory, PowerShell's variable and cmd's environment intact. Then the Host is killed
//! (`taskkill /F`): every pane process ends with it, `ls` reports no Host, and the next `new`
//! starts a fresh Host at once with none of the old Terminals.
//!
//! Scenario B (release `subak`): three Terminals created by three concurrent `new` share one
//! Host; two clients per Terminal type at the same time and each sees only its own Terminal;
//! killing every client of one Terminal leaves the others attached and working.
//!
//! Windows only. Every scenario uses its own endpoint and stops what it started. Nothing here
//! opens a window or starts an interactive Claude Code session (that is the maintainer's manual
//! check, scripts/milestone-a-smoke.ps1).
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
const LONG: Duration = Duration::from_secs(30);
const PS_START: Duration = Duration::from_secs(120);

/// The stand-in interactive program: no double quotes, so it passes as one argument.
const STANDIN: &str = "$n = 0; $first = $null; Write-Host ('standin-ready-pid-' + $PID); \
    while ($true) { $l = Read-Host 'standin'; if (-not $l) { continue }; $n++; \
    if (-not $first) { $first = $l }; \
    Write-Host ('standin-answer-{0}-pid-{1}-first-{2}-said-{3}' -f $n, $PID, $first, $l) }";

fn endpoint(tag: &str) -> String {
    format!("subak-e2e14-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>`.
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
    let code = out.status.code().unwrap_or(-1);
    if doc["ok"] == false {
        assert_eq!(Some(i64::from(code)), doc["error"]["exit_code"].as_i64());
    }
    (code, doc)
}

fn pane_pid(created: &Value) -> u64 {
    created["data"]["terminal"]["windows"][0]["panes"][0]["pid"]
        .as_u64()
        .unwrap()
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

fn wait_gone(pid: u64) -> bool {
    let until = Instant::now() + LONG;
    while running(pid) {
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.exe)
            .args(["--json", "--endpoint", self.ep, "host", "stop"])
            .args(["--kill-terminals", "--yes"])
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

/// `subak attach` with piped stdin and output and no window.
struct Client {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
    mark: usize,
}

impl Client {
    fn attach(exe: &Path, ep: &str, target: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["--endpoint", ep, "attach", target, "--size", "100x30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("attach");
        let out = Arc::new(Mutex::new(Vec::new()));
        for mut s in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let sink = Arc::clone(&out);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        let mut c = Self {
            child,
            out,
            mark: 0,
        };
        c.expect("Ctrl+B d detaches", LONG);
        c
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()[self.mark..]).into_owned()
    }

    /// Waits for `needle` after the last match.
    fn expect(&mut self, needle: &str, limit: Duration) {
        let until = Instant::now() + limit;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < until,
                "`{needle}` not seen:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.mark = self.out.lock().unwrap().len();
    }

    fn exit_code(&mut self) -> Option<i32> {
        let until = Instant::now() + LONG;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code();
            }
            if Instant::now() > until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn detach_keys(&mut self) -> Option<i32> {
        self.send(&[0x02, b'd']);
        self.exit_code()
    }

    fn end_input(&mut self) -> Option<i32> {
        drop(self.child.stdin.take());
        self.exit_code()
    }

    fn crash(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
fn a_detached_host_keeps_shells_and_an_interactive_program_through_client_events() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e14-journey");
    let ep = endpoint("journey");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);
    let ps_env = [
        "--env",
        "POWERSHELL_UPDATECHECK=Off",
        "--env",
        "POWERSHELL_TELEMETRY_OPTOUT=1",
    ];

    let (code, cmd) = run(&["new", "cmd", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{cmd}");
    assert_eq!(cmd["data"]["host"]["started"], true, "a Host on demand");
    let host = cmd["data"]["host"]["pid"].as_u64().unwrap();
    let mut args = vec!["new", "ps"];
    args.extend(ps_env);
    args.extend(["--", "powershell", "-NoLogo", "-NoProfile"]);
    let (code, ps) = run(&args);
    assert_eq!(code, 0, "{ps}");
    let mut args = vec!["new", "agent"];
    args.extend(ps_env);
    args.extend([
        "--",
        "powershell",
        "-NoLogo",
        "-NoProfile",
        "-Command",
        STANDIN,
    ]);
    let (code, agent) = run(&args);
    assert_eq!(code, 0, "{agent}");
    let pids = [host, pane_pid(&cmd), pane_pid(&ps), pane_pid(&agent)];
    let agent_pid = pids[3];
    let survive = |event: &str| {
        for pid in pids {
            assert!(running(pid), "pid {pid} died after: {event} (tasklist)");
        }
        let (_, ls) = run(&["ls"]);
        assert_eq!(ls["data"]["host"]["pid"].as_u64(), Some(host), "{event}");
        for (t, pid) in ls["data"]["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .zip(&pids[1..])
        {
            assert_eq!(t["state"], "running", "{event}: {t}");
            assert_eq!(t["pid"].as_u64(), Some(*pid), "{event}: {t}");
        }
    };
    let answer = |n: u32, said: &str| {
        format!("standin-answer-{n}-pid-{agent_pid}-first-remember-kiwi-said-{said}")
    };

    // The interactive program remembers; its client is killed.
    let mut a = Client::attach(&subak, &ep, "agent");
    a.expect("standin-ready-pid-", PS_START);
    a.send(b"remember-kiwi\r");
    a.expect(&answer(1, "remember-kiwi"), LONG);
    a.crash();
    survive("a killed client");

    // cmd.exe: Ctrl+B d. PowerShell: the end of input.
    let mut c = Client::attach(&subak, &ep, "cmd");
    c.send(b"set KIMSTATE=e2e14-state& echo set-mark^er\r");
    c.expect("set-marker", LONG);
    assert_eq!(c.detach_keys(), Some(0));
    survive("Ctrl+B d");
    let mut p = Client::attach(&subak, &ep, "ps");
    let until = Instant::now() + PS_START;
    while !String::from_utf8_lossy(&p.out.lock().unwrap()).contains("ps-ready") {
        assert!(Instant::now() < until, "PowerShell never answered");
        p.send(b"$kim = 'ps-state-' + (6*7); 'ps-rea' + 'dy'\r");
        std::thread::sleep(Duration::from_secs(3));
    }
    assert_eq!(p.end_input(), Some(0));
    survive("the end of input");

    // `subak detach` from outside.
    let mut d = Client::attach(&subak, &ep, "agent");
    let (code, doc) = run(&["detach", "agent"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(d.exit_code(), Some(0));
    survive("subak detach");

    // 50 attach/detach cycles in three ways.
    for i in 0..50 {
        let mut c = Client::attach(&subak, &ep, "cmd");
        c.send(format!("echo cycle-{i}-mark^er\r").as_bytes());
        c.expect(&format!("cycle-{i}-marker"), LONG);
        match i % 3 {
            0 => assert_eq!(c.detach_keys(), Some(0)),
            1 => assert_eq!(c.end_input(), Some(0)),
            _ => c.crash(),
        }
    }
    survive("50 attach/detach cycles");
    let until = Instant::now() + LONG;
    while run(&["host", "status"]).1["data"]["clients"]["connected"] != 1 {
        assert!(Instant::now() < until, "client connections did not close");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Reattach: the same processes with their state.
    let mut a = Client::attach(&subak, &ep, "agent");
    a.send(b"turn-two\r");
    a.expect(&answer(2, "turn-two"), LONG);
    assert_eq!(a.detach_keys(), Some(0));
    let mut p = Client::attach(&subak, &ep, "ps");
    p.send(b"'ps-' + 'got-' + $kim\r");
    p.expect("ps-got-ps-state-42", LONG);
    assert_eq!(p.detach_keys(), Some(0));
    let mut c = Client::attach(&subak, &ep, "cmd");
    c.send(b"echo %KIMSTATE%-mark^er\r");
    c.expect("e2e14-state-marker", LONG);
    assert_eq!(c.detach_keys(), Some(0));
    survive("reattaching");

    // The Host is the boundary: killed, it ends every pane process; the next `new` is fresh.
    let killed = Command::new("taskkill")
        .args(["/PID", &host.to_string(), "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("taskkill");
    assert!(killed.status.success(), "{killed:?}");
    for pid in pids {
        assert!(
            wait_gone(pid),
            "pid {pid} outlived its killed Host (tasklist)"
        );
    }
    let (code, ls) = run(&["ls"]);
    assert_eq!(code, 0);
    assert_eq!(ls["data"]["host"], Value::Null);
    let t = Instant::now();
    let (code, fresh) = run(&["new", "again", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{fresh}");
    assert!(t.elapsed() < Duration::from_secs(15), "no stale endpoint");
    assert_eq!(fresh["data"]["host"]["started"], true);
    assert_ne!(fresh["data"]["host"]["pid"].as_u64(), Some(host));
    let old = agent["data"]["terminal_id"].as_str().unwrap();
    let (code, doc) = run(&["info", old]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );
    let (_, ls) = run(&["ls"]);
    assert_eq!(ls["data"]["terminals"].as_array().unwrap().len(), 1);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn concurrent_terminals_and_clients_stay_independent() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e14-concurrent");
    let ep = endpoint("concurrent");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let names = ["one", "two", "three"];
    let created: Vec<Value> = std::thread::scope(|s| {
        let jobs: Vec<_> = names
            .iter()
            .map(|n| {
                let (subak, ep, sb) = (&subak, &ep, &sb);
                s.spawn(move || {
                    let (code, doc) = json(subak, ep, &["new", n, "--", "cmd.exe", "/d"], sb);
                    assert_eq!(code, 0, "{doc}");
                    doc
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    });
    let hosts: Vec<_> = created
        .iter()
        .map(|d| d["data"]["host"]["pid"].as_u64())
        .collect();
    assert!(hosts.iter().all(|h| *h == hosts[0]), "one Host: {hosts:?}");
    assert_eq!(
        created
            .iter()
            .filter(|d| d["data"]["host"]["started"] == true)
            .count(),
        1
    );
    let mut clients: Vec<(&str, Client, Client)> = names
        .iter()
        .map(|n| {
            (
                *n,
                Client::attach(&subak, &ep, n),
                Client::attach(&subak, &ep, n),
            )
        })
        .collect();
    std::thread::scope(|s| {
        for (n, a, b) in &mut clients {
            s.spawn(move || {
                a.send(format!("echo {n}-a-mark^er\r").as_bytes());
                b.send(format!("echo {n}-b-mark^er\r").as_bytes());
                for c in [a, b] {
                    c.expect(&format!("{n}-a-marker"), LONG);
                    c.mark = 0;
                    c.expect(&format!("{n}-b-marker"), LONG);
                }
            });
        }
    });
    for (n, a, b) in &clients {
        for other in names.iter().filter(|o| *o != n) {
            for c in [a, b] {
                let all = String::from_utf8_lossy(&c.out.lock().unwrap()).into_owned();
                assert!(
                    !all.contains(&format!("{other}-a-marker")),
                    "{n} saw {other}"
                );
            }
        }
    }
    // Every client of `one` is killed; the others keep working.
    {
        let (_, a, b) = &mut clients[0];
        a.crash();
        b.crash();
    }
    for (n, a, _) in clients.iter_mut().skip(1) {
        a.send(format!("echo {n}-after-mark^er\r").as_bytes());
        a.expect(&format!("{n}-after-marker"), LONG);
    }
    let (_, st) = json(&subak, &ep, &["host", "status"], &sb);
    assert_eq!(st["data"]["clients"]["attached"], 4, "{st}");
    let (_, ls) = json(&subak, &ep, &["ls"], &sb);
    for t in ls["data"]["terminals"].as_array().unwrap() {
        assert_eq!(t["state"], "running", "{t}");
        assert!(running(t["pid"].as_u64().unwrap()), "{t}");
    }
}
