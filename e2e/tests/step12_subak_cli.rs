//! E2E — Step 12: the Subak CLI and headless control (docs/SUBAK_CLI.md, ADR 0027).
//! `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-011`…`SUBAK-FR-015`,
//! `SUBAK-FR-060`, `KIM-FR-010`, `KIM-FR-012`, `KIM-SEC-013`.
//!
//! Scenario A (release `subak`): a user works with the tmux-familiar verbs only, on their own
//! endpoint. `status` reports no Host; `new work` starts one on demand; `ls`, `info` and
//! `status` describe it. An attach with piped input types into the shell; `subak detach work`
//! from another process ends that attach (exit 0, a notice naming the other client) while
//! `tasklist` (independent of subak) still sees the shell. A second attach is read-only and is
//! detached by its client id. `rename` keeps the process, the deprecated `terminal list` still
//! answers (canonical command name plus a warning), `kill` needs consent, and `host stop` ends
//! the Host.
//!
//! Scenario B (release `subak`): work survives its client. A `ping` runs in a Terminal, the
//! attached client is killed, and the ping keeps running in the Host (a new attach sees it).
//! A verbatim `--command-line` reaches cmd.exe exactly: quotes, `^&`, `&` and a redirection.
//!
//! Windows only. Every scenario uses its own endpoint and stops (or kills) what it started.
#![cfg(windows)]

use kim_e2e::{TempDir, bin};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);

fn endpoint(tag: &str) -> String {
    format!("subak-e2e12-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>`: options first, because everything after `--` is the
/// program of a new Terminal.
fn json(exe: &Path, ep: &str, args: &[&str], sb: &TempDir) -> (i32, Value) {
    let out = Command::new(exe)
        .args(["--json", "--endpoint", ep])
        .args(args)
        .env("USERPROFILE", sb.path())
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak");
    assert!(out.stderr.is_empty(), "{args:?}: --json keeps stderr empty");
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

fn human(exe: &Path, ep: &str, args: &[&str]) -> Output {
    Command::new(exe)
        .args(["--endpoint", ep])
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak")
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
}

impl Client {
    fn attach(exe: &Path, ep: &str, extra: &[&str]) -> Self {
        let mut child = Command::new(exe)
            .args(["--endpoint", ep, "attach", "--size", "100x30"])
            .args(extra)
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
        let c = Self { child, out };
        c.expect("Ctrl+B d detaches");
        c
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn expect(&self, needle: &str) {
        let until = Instant::now() + LONG;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < until,
                "`{needle}` not seen:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
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
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_user_manages_terminals_with_the_tmux_familiar_cli_only() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e12-cli");
    let ep = endpoint("cli");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);

    let (code, doc) = run(&["status"]);
    assert_eq!(
        (code, &doc["data"]["running"]),
        (0, &Value::Bool(false)),
        "{doc}"
    );
    let (code, doc) = run(&["new", "work", "--no-start"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("host_not_running"))
    );

    let (code, created) = run(&["new", "work", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    assert_eq!(created["command"], "new");
    assert_eq!(created["data"]["host"]["started"], true);
    let host = created["data"]["host"]["pid"].as_u64().unwrap();
    let shell = created["data"]["terminal"]["windows"][0]["panes"][0]["pid"]
        .as_u64()
        .unwrap();
    assert!(running(host) && running(shell));
    let (code, doc) = run(&["ls"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["terminals"][0]["name"], "work");
    let (code, doc) = run(&["info", "nope"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );

    // Attach, type, and be detached by another process.
    let mut a = Client::attach(&subak, &ep, &["work"]);
    a.send(b"echo mark^er-e2e12-a\r");
    a.expect("marker-e2e12-a");
    let out = human(&subak, &ep, &["detach", "work"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing was stopped"));
    assert_eq!(a.exit_code(), Some(0), "{}", a.text());
    a.expect("by another client");
    assert!(running(shell), "detach stopped nothing (tasklist)");

    // A read-only watcher, detached by its client id.
    let mut w = Client::attach(&subak, &ep, &["work", "--read-only"]);
    let (_, st) = run(&["status"]);
    let id = st["data"]["clients"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["attached"] == true)
        .expect("the watcher")["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let (code, doc) = run(&["detach", "--client", &id]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["detached"][0]["interactive"], false);
    assert_eq!(w.exit_code(), Some(0));

    // Rename keeps the process; the deprecated spelling still answers, with a warning.
    let (code, doc) = run(&["rename", "work", "api"]);
    assert_eq!(code, 0, "{doc}");
    let (code, doc) = run(&["terminal", "list"]);
    assert_eq!((code, doc["command"].as_str()), (0, Some("ls")));
    assert!(doc["warnings"][0].as_str().unwrap().contains("deprecated"));
    assert_eq!(doc["data"]["terminals"][0]["pid"].as_u64(), Some(shell));

    // Destructive: refused without consent, done with it.
    let (code, doc) = run(&["kill", "api", "--no-input"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    assert!(running(shell));
    let (code, doc) = run(&["kill", "api", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert!(wait_gone(shell), "kill ended the shell (tasklist)");
    let (code, doc) = run(&["host", "stop", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert!(wait_gone(host), "the Host exited (tasklist)");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn work_survives_its_client_and_verbatim_lines_reach_cmd() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e12-survive");
    let ep = endpoint("survive");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);

    let (code, created) = run(&["new", "busy", "--", "cmd.exe", "/d"]);
    assert_eq!(code, 0, "{created}");
    let shell = created["data"]["terminal"]["windows"][0]["panes"][0]["pid"]
        .as_u64()
        .unwrap();
    let mut a = Client::attach(&subak, &ep, &["busy"]);
    a.send(b"ping -n 60 127.0.0.1\r");
    a.expect("Reply from 127.0.0.1");
    drop(a); // killed, not detached
    let until = Instant::now() + LONG;
    loop {
        let (_, doc) = run(&["info", "busy"]);
        let t = &doc["data"]["terminal"];
        if t["clients"] == 0 {
            assert_eq!(t["state"], "running");
            assert!(
                t["windows"][0]["panes"][0]["processes"].as_u64().unwrap() >= 2,
                "{t}"
            );
            break;
        }
        assert!(Instant::now() < until, "the killed client never let go");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(running(shell));
    let mut b = Client::attach(&subak, &ep, &["busy"]);
    b.expect("Reply from 127.0.0.1");
    b.send(&[0x03]);
    b.send(&[0x02, b'd']);
    assert_eq!(b.exit_code(), Some(0));

    let out = sb.path().join("out.txt");
    let line = format!(
        r#"cmd.exe /d /s /c "(echo "a  b"&echo ^&x&echo two)>"{}"""#,
        out.display()
    );
    let (code, doc) = run(&["new", "raw", "--remain-on-exit", "--command-line", &line]);
    assert_eq!(code, 0, "{doc}");
    let until = Instant::now() + LONG;
    loop {
        let (_, doc) = run(&["info", "raw"]);
        if doc["data"]["terminal"]["state"] == "stopped" {
            assert_eq!(
                doc["data"]["terminal"]["last_exit"]["exit_code"], 0,
                "{doc}"
            );
            break;
        }
        assert!(Instant::now() < until, "cmd never finished");
        std::thread::sleep(Duration::from_millis(100));
    }
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().collect::<Vec<_>>(), [r#""a  b""#, "&x", "two"]);
}
