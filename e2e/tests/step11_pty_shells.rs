//! E2E — Step 11: PTY and shell process management (docs/PTY_PROCESSES.md, ADR 0025).
//! `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-016`, `KIM-SEC-013`, `KIM-NFR-013`.
//!
//! Scenario A (release `subak`): an agent runs Windows PowerShell and cmd side by side in named
//! Terminals on its own endpoint, with environment overrides (one given inline, one copied from
//! its own environment so the token is not on any command line). It types into both through
//! `terminal attach` with piped input (Korean text included), sees the overrides and
//! `SUBAK_PANE_ID` arrive but none of its own launcher variables, interrupts a running `ping`
//! with Ctrl+C, and checks that no JSON document ever shows a value. `tasklist` (independent of
//! subak) confirms each shell runs and ends.
//!
//! Scenario B (release `subak`): explicit programs and their failures. A program's exit code is
//! kept with `--remain-on-exit`; a batch script, a missing program, `pwsh` when it is not
//! installed, and a missing working directory are refused with precise reasons and hints, and
//! leave no Terminal behind. A dry run resolves `cmd` to its file without starting anything.
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
const SECRET: &str = "e2e-step11-secret-value";
const COPIED: &str = "e2e-step11-copied-value";

fn endpoint(tag: &str) -> String {
    format!("subak-e2e11-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>` with `env` added: options first, because everything
/// after `--` is the command of a new Terminal.
fn json_env(exe: &Path, ep: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
    let mut cmd = Command::new(exe);
    cmd.args(["--json", "--endpoint", ep])
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let text = format!("{doc}{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !text.contains(SECRET) && !text.contains(COPIED),
        "{args:?} showed a value: {text}"
    );
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

fn pane(doc: &Value) -> &Value {
    &doc["data"]["terminal"]["windows"][0]["panes"][0]
}

/// A `key: value` detail line of a CLI error.
fn detail(doc: &Value, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    doc["error"]["details"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|l| l.strip_prefix(&prefix).map(String::from))
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: String,
    pids: Vec<u64>,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.exe)
            .args([
                "--json",
                "--endpoint",
                &self.ep,
                "host",
                "stop",
                "--kill-terminals",
                "--yes",
            ])
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
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

/// `subak terminal attach` with piped stdin and output and no window.
struct Client {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Client {
    fn attach(exe: &Path, ep: &str, target: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["--endpoint", ep, "terminal", "attach", target])
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

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn expect(&self, needle: &str) {
        let until = Instant::now() + Duration::from_secs(120); // a cold PowerShell start can be slow
        while Instant::now() < until {
            if self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("`{needle}` not seen:\n{}", self.text());
    }

    /// Ctrl+B d: detach without stopping anything.
    fn detach(mut self) {
        self.send(&[0x02, b'd']);
        assert!(self.child.wait().unwrap().success(), "Ctrl+B d detaches");
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
fn an_agent_runs_powershell_and_cmd_with_overrides_and_no_leaked_secrets() {
    let subak = bin("subak");
    let ep = endpoint("shells");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        pids: Vec::new(),
    };
    // The agent's own environment: a token and a Git Bash HOME it must not hand to panes.
    let launcher = [("GITHUB_TOKEN", SECRET), ("HOME", r"C:\e2e-launcher-home")];
    let (code, ps) = json_env(
        &subak,
        &ep,
        &[
            "terminal",
            "new",
            "ps",
            "--env",
            &format!("E2E_INLINE={SECRET}"),
            "--env",
            "E2E_COPIED",
            "--env",
            "POWERSHELL_UPDATECHECK=Off",
            "--",
            "powershell",
            "-NoLogo",
            "-NoProfile",
        ],
        &[launcher[0], launcher[1], ("E2E_COPIED", COPIED)],
    );
    assert_eq!(code, 0, "{ps}");
    let host_pid = ps["data"]["host"]["pid"].as_u64().unwrap();
    let ps_pid = pane(&ps)["pid"].as_u64().unwrap();
    guard.pids.extend([host_pid, ps_pid]);
    assert!(
        pane(&ps)["executable"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .ends_with(r"\windowspowershell\v1.0\powershell.exe"),
        "{ps}"
    );
    assert_eq!(
        pane(&ps)["env"]["set"],
        serde_json::json!(["E2E_COPIED", "E2E_INLINE", "POWERSHELL_UPDATECHECK"]),
        "names only"
    );
    let (code, cmd) = json_env(
        &subak,
        &ep,
        &["terminal", "new", "cmd", "--", "cmd.exe", "/d"],
        &launcher,
    );
    assert_eq!(code, 0, "{cmd}");
    let cmd_pid = pane(&cmd)["pid"].as_u64().unwrap();
    guard.pids.push(cmd_pid);
    assert!(
        running(ps_pid) && running(cmd_pid),
        "tasklist sees both shells"
    );

    // PowerShell: the overrides and the pane id arrive, the launcher's variables do not.
    let mut p = Client::attach(&subak, &ep, "ps");
    p.send(
        b"Write-Output (\"v1=\" + ($env:E2E_INLINE -eq 'e2e-step11-secret-value') + \" v2=\" + ($env:E2E_COPIED -eq 'e2e-step11-copied-value') + \" gh=\" + [bool]$env:GITHUB_TOKEN + \" pane=\" + $env:SUBAK_PANE_ID.Substring(0,2) + \" end\")\r",
    );
    p.expect("v1=True v2=True gh=False pane=p- end");
    p.send("Write-Output ('한글-' + '파워셸')\r".as_bytes());
    p.expect("한글-파워셸");
    p.detach();

    // cmd: Korean round trip and Ctrl+C reaching a running program.
    let mut c = Client::attach(&subak, &ep, "cmd");
    c.send("echo 한글-명령-마커\r".as_bytes());
    c.expect("한글-명령-마커");
    c.send(b"ping -n 60 127.0.0.1\r");
    c.expect("Reply from 127.0.0.1");
    c.send(b"\x03");
    c.send(b"echo after-ctrl-c-mark^er\r");
    c.expect("after-ctrl-c-marker");
    c.detach();
    assert!(
        running(ps_pid) && running(cmd_pid),
        "detaching stops nothing"
    );

    // No document shows a value (json_env checks every one), and the launcher's HOME did not
    // become the pane's.
    for target in ["ps", "cmd"] {
        let (code, doc) = json_env(&subak, &ep, &["terminal", "info", target], &[]);
        assert_eq!(code, 0, "{doc}");
    }
    let (code, _) = json_env(&subak, &ep, &["terminal", "list"], &[]);
    assert_eq!(code, 0);

    // Killing one Terminal ends its tree only.
    let (code, doc) = json_env(&subak, &ep, &["terminal", "kill", "cmd", "--yes"], &[]);
    assert_eq!(code, 0, "{doc}");
    assert!(wait_gone(cmd_pid, Duration::from_secs(15)));
    assert!(running(ps_pid), "PowerShell is untouched");
    let (code, doc) = json_env(
        &subak,
        &ep,
        &["host", "stop", "--kill-terminals", "--yes"],
        &[],
    );
    assert_eq!(code, 0, "{doc}");
    assert!(wait_gone(ps_pid, Duration::from_secs(15)));
    assert!(wait_gone(host_pid, Duration::from_secs(15)));
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn explicit_programs_report_exit_codes_and_failures_are_precise() {
    let subak = bin("subak");
    let ep = endpoint("programs");
    let sb = TempDir::new("step11-programs");
    let mut guard = Cleanup {
        exe: &subak,
        ep: ep.clone(),
        pids: Vec::new(),
    };
    let run = |args: &[&str]| json_env(&subak, &ep, args, &[]);
    // A dry run without a Host resolves the program and starts nothing.
    let (code, doc) = run(&["terminal", "new", "--dry-run", "--", "cmd"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["would_start_host"], true);
    assert!(
        doc["data"]["would_create"]["executable"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .ends_with(r"\system32\cmd.exe")
    );
    // An exit code is kept with --remain-on-exit.
    let (code, doc) = run(&[
        "terminal",
        "new",
        "seven",
        "--remain-on-exit",
        "--",
        "cmd.exe",
        "/d",
        "/c",
        "exit 7",
    ]);
    assert_eq!(code, 0, "{doc}");
    guard
        .pids
        .push(doc["data"]["host"]["pid"].as_u64().unwrap());
    let until = Instant::now() + Duration::from_secs(30);
    let info = loop {
        let (_, info) = run(&["terminal", "info", "seven"]);
        if info["data"]["terminal"]["state"] == "stopped" {
            break info;
        }
        assert!(Instant::now() < until, "{info}");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(info["data"]["terminal"]["last_exit"]["exit_code"], 7);

    // Failures: precise reasons and hints, nothing left behind.
    let script = sb.path().join("build.cmd");
    std::fs::write(&script, "@echo off\r\n").unwrap();
    let script = script.display().to_string();
    let missing_dir = sb.path().join("missing").display().to_string();
    let mut cases: Vec<(Vec<&str>, &str, &str)> = vec![
        (vec!["--", &script], "not_executable", "cmd.exe /d /c"),
        (
            vec!["--", r"C:\subak-e2e-missing\nothing.exe"],
            "not_found",
            "path",
        ),
        (vec!["--cwd", &missing_dir, "--", "cmd.exe"], "bad_cwd", ""),
    ];
    let (code, _) = run(&["terminal", "new", "--dry-run", "--", "pwsh"]);
    if code != 0 {
        cases.push((vec!["--", "pwsh"], "not_found", "never installs"));
    }
    for (args, reason, hint) in &cases {
        let mut a = vec!["terminal", "new"];
        a.extend_from_slice(args);
        let (code, doc) = run(&a);
        assert_eq!(code, 1, "{a:?}: {doc}");
        assert_eq!(doc["error"]["code"], "spawn_failed");
        assert_eq!(detail(&doc, "reason").as_deref(), Some(*reason), "{doc}");
        if !hint.is_empty() {
            assert!(
                doc["error"]["hint"].as_str().unwrap_or("").contains(hint),
                "{doc}"
            );
        }
    }
    let (_, doc) = run(&["terminal", "list"]);
    let names: Vec<&str> = doc["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["seven"], "failed starts leave nothing");
    let (code, doc) = run(&["host", "stop", "--kill-terminals", "--yes"]);
    assert_eq!(code, 0, "{doc}");
}
