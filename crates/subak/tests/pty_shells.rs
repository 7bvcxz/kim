//! Step 11 — PTY and shell process management on real Windows (docs/PTY_PROCESSES.md,
//! ADR 0025). `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-016`, `SUBAK-FR-001`, `SUBAK-FR-003`,
//! `KIM-SEC-013`, `KIM-NFR-013`.
//!
//! Real `subak.exe` Hosts, each on its **own** endpoint (options always come before a `--`, so
//! a Terminal's command can never redirect a test to the user's default Host), real `cmd.exe`,
//! Windows PowerShell 5.1 and, when installed, PowerShell 7 (`pwsh`; never installed by a test).
//! Raw IPC v1 clients validate every control message against `ipc-v1.schema.json`. Every test
//! ends what it started: the [`Lab`] guard stops its Host with `--kill-terminals --yes` (or kills
//! it) and checks that no pane process and no pseudo console host survives.
//!
//! Headless: nothing here opens a window (pseudo consoles have none; every helper process is
//! started with `CREATE_NO_WINDOW`). Lines starting with `EVIDENCE` record what was measured;
//! run with `--nocapture` to see them.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// Data frame flag of client input (IPC_PROTOCOL §1).
const INPUT: u8 = 2;

/// A Host for one test; on drop it stops the Host and checks that nothing it started survives.
struct Lab {
    ep: String,
    host: Proc,
    seen: std::sync::Mutex<Vec<u32>>,
}

impl Lab {
    fn start(tag: &str) -> Self {
        Self::start_env(tag, &[])
    }

    /// `host start --no-shell` with `env` set on the launching CLI (as if an agent's terminal
    /// had those variables).
    fn start_env(tag: &str, env: &[(&str, &str)]) -> Self {
        let ep = endpoint(&format!("pty-{tag}"));
        let (code, doc) = run_env(
            &[
                "host",
                "start",
                "--endpoint",
                &ep,
                "--no-shell",
                "--idle-exit",
                "60",
            ],
            env,
        );
        assert_eq!(code, 0, "{doc}");
        Self {
            host: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
            ep,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// `subak --json --endpoint <ours> <args>`: the endpoint first, before any `--`.
    fn cli(&self, args: &[&str]) -> (i32, Value) {
        self.cli_env(args, &[])
    }

    fn cli_env(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
        let mut a = vec!["--endpoint", self.ep.as_str()];
        a.extend_from_slice(args);
        run_env(&a, env)
    }

    /// `new <name> <args…>` that must succeed; returns the created Terminal's pane.
    fn new_terminal(&self, name: &str, args: &[&str]) -> Value {
        let mut a = vec!["new", name];
        a.extend_from_slice(args);
        let (code, doc) = self.cli(&a);
        assert_eq!(code, 0, "{a:?}: {doc}");
        let mut r = doc["data"].clone();
        r.as_object_mut().unwrap().remove("host");
        ipc_def("result_terminal_create", &r);
        let pane = doc["data"]["terminal"]["windows"][0]["panes"][0].clone();
        self.seen.lock().unwrap().push(pid_of(&pane["pid"]));
        pane
    }

    /// `new` that must fail with `spawn_failed`; returns the error.
    fn new_fails(&self, args: &[&str]) -> Value {
        let mut a = vec!["new"];
        a.extend_from_slice(args);
        let (code, doc) = self.cli(&a);
        assert_eq!(code, 1, "{a:?}: {doc}");
        assert_eq!(doc["error"]["code"], "spawn_failed", "{doc}");
        doc["error"].clone()
    }

    fn info(&self, target: &str) -> Value {
        let (code, doc) = self.cli(&["info", target]);
        assert_eq!(code, 0, "{doc}");
        ipc_def("result_terminal_info", &doc["data"]);
        doc["data"]["terminal"].clone()
    }

    fn kill(&self, target: &str) -> Value {
        let (code, doc) = self.cli(&["kill", target, "--yes"]);
        assert_eq!(code, 0, "{doc}");
        doc["data"].clone()
    }

    fn stats(&self) -> Value {
        let c = Raw::handshake(&self.ep, "cli");
        let r = c.request(1, "host.stats", &json!({}));
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    }

    /// The Host's handle count: the minimum of several samples, which filters out handles the
    /// Host holds only briefly (a connection being served, a thread being joined).
    fn handles(&self) -> u64 {
        (0..5)
            .map(|i| {
                if i > 0 {
                    std::thread::sleep(Duration::from_millis(150));
                }
                self.stats()["host"]["handle_count"].as_u64().unwrap()
            })
            .min()
            .unwrap()
    }

    fn attach(&self, target: &str) -> Pane {
        Pane::attach(&self.ep, target)
    }

    fn conhosts(&self) -> Vec<u32> {
        children(self.host.pid, "conhost.exe")
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let consoles: Vec<Proc> = if self.host.alive() {
            self.conhosts()
                .into_iter()
                .filter_map(Proc::try_open)
                .collect()
        } else {
            Vec::new()
        };
        if self.host.alive() {
            let _ = run(&[
                "host",
                "stop",
                "--endpoint",
                &self.ep,
                "--kill-terminals",
                "--yes",
            ]);
            if !self.host.wait(Duration::from_secs(20)) {
                self.host.kill();
            }
        }
        let mut survivors = Vec::new();
        for pid in self.seen.lock().unwrap().iter() {
            if let Some(p) = Proc::try_open(*pid)
                && !p.wait(Duration::from_secs(10))
            {
                p.kill();
                survivors.push(*pid);
            }
        }
        for c in &consoles {
            if !c.wait(Duration::from_secs(10)) {
                c.kill();
                survivors.push(c.pid);
            }
        }
        if !std::thread::panicking() {
            assert!(
                survivors.is_empty(),
                "processes outlived the test Host: {survivors:?}"
            );
        }
    }
}

fn children(parent: u32, name: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, pp, n)| *pp == parent && n == name)
        .map(|(p, _, _)| p)
        .collect()
}

fn parent_of(pid: u32) -> Option<u32> {
    processes()
        .into_iter()
        .find(|(p, _, _)| *p == pid)
        .map(|(_, pp, _)| pp)
}

fn alive(pid: u32) -> bool {
    Proc::try_open(pid).is_some_and(|p| p.alive())
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A scratch directory under the system temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!(
            "subak-pty-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn s(&self, name: &str) -> String {
        self.path(name).display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `icacls` (a console tool, started without a window) on a file or directory of ours.
fn icacls(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("icacls");
    assert!(out.status.success(), "icacls {args:?}: {out:?}");
}

/// An interactive raw IPC attach to one pane.
struct Pane {
    raw: Raw,
    stream: u32,
    pid: u32,
    seen: Vec<u8>,
}

impl Pane {
    fn attach(ep: &str, target: &str) -> Self {
        let raw = Raw::handshake(ep, "cli");
        let r = raw.request(
            1,
            "pane.attach",
            &json!({"target": target, "interactive": true}),
        );
        assert_eq!(r["ok"], true, "{r}");
        ipc_def("result_pane_attach", &r["result"]);
        Self {
            stream: u32::try_from(r["result"]["stream_id"].as_u64().unwrap()).unwrap(),
            pid: pid_of(&r["result"]["pane"]["pid"]),
            raw,
            seen: Vec::new(),
        }
    }

    fn send(&self, bytes: &[u8]) {
        for chunk in bytes.chunks(256 * 1024) {
            self.raw.write(&data(self.stream, 0, INPUT, chunk)).unwrap();
        }
    }

    fn text(&self) -> String {
        visible(&self.seen)
    }

    /// Reads output until `needle` is visible; returns everything seen so far.
    fn expect(&mut self, needle: &str) -> String {
        self.expect_within(needle, LONG)
    }

    fn expect_within(&mut self, needle: &str, limit: Duration) -> String {
        let until = Instant::now() + limit;
        loop {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            let left = until.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "`{needle}` not seen from pane pid {}:\n{text}",
                self.pid
            );
            match self.raw.next(left.min(Duration::from_millis(250))) {
                Got::Data(b) => self.seen.extend(b),
                Got::Eof => panic!("the Host closed the attach while waiting for `{needle}`"),
                Got::Control(_) | Got::Timeout => {}
            }
        }
    }

    fn clear(&mut self) {
        self.seen.clear();
    }
}

/// Whether `pwsh` (PowerShell 7) can start here; it is never installed by a test.
fn pwsh_installed(lab: &Lab) -> bool {
    let (code, doc) = lab.cli(&["new", "--dry-run", "--", "pwsh", "-NoLogo"]);
    if code == 0 {
        return true;
    }
    assert_eq!(
        detail(&doc["error"], "reason").as_deref(),
        Some("not_found"),
        "pwsh is either startable or not installed: {doc}"
    );
    assert!(
        doc["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("never installs"),
        "{doc}"
    );
    // The CI image (windows-2025) ships PowerShell 7, so the pwsh path is exercised there.
    assert!(
        std::env::var_os("GITHUB_ACTIONS").is_none(),
        "pwsh is expected on the CI runner: {doc}"
    );
    false
}

/// A `key: value` detail of a CLI error (the CLI lists an IPC error's details as lines).
fn detail(err: &Value, key: &str) -> Option<String> {
    let prefix = format!("{key}: ");
    err["details"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|l| l.strip_prefix(&prefix).map(String::from))
}

/// Sends an idempotent PowerShell command until its output appears. After an interrupt, keys
/// typed before PSReadLine reads again are discarded (seen on Windows Server 2025 even after the
/// prompt was drawn), as in any console.
fn ps_command(p: &mut Pane, line: &[u8], needle: &str) {
    for attempt in 0..5 {
        p.send(line);
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if p.text().contains(needle) {
                return;
            }
            match p.raw.next(Duration::from_millis(100)) {
                Got::Data(b) => p.seen.extend(b),
                Got::Eof => panic!("the Host closed the attach"),
                _ => {}
            }
        }
        eprintln!("PowerShell dropped typed input after Ctrl+C (attempt {attempt}); typing again");
    }
    panic!("`{needle}` not seen after 5 attempts:\n{}", p.text());
}

/// Waits for PowerShell's prompt after an interrupt: PowerShell discards keys typed while it
/// stops a pipeline, so the next command is sent only once the prompt is back.
fn ps_prompt(p: &mut Pane) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if p.text().trim_end().ends_with('>') {
            return;
        }
        assert!(
            Instant::now() < until,
            "no prompt after Ctrl+C:\n{}",
            p.text()
        );
        match p.raw.next(Duration::from_millis(100)) {
            Got::Data(b) => p.seen.extend(b),
            Got::Eof => panic!("the Host closed the attach"),
            _ => {}
        }
    }
}

/// How long a PowerShell's first answer may take: a cold start on a busy CI runner, with other
/// tests starting PowerShell at the same time, took longer than 20 s (Windows Server 2025).
const PS_START: Duration = Duration::from_secs(120);

/// Variables for PowerShell panes in tests: no update check, no telemetry, no logo.
const PS_ENV: [&str; 4] = [
    "--env",
    "POWERSHELL_UPDATECHECK=Off",
    "--env",
    "POWERSHELL_TELEMETRY_OPTOUT=1",
];

// ---------------------------------------------------------------------------------------------

#[test]
fn powershell_and_cmd_run_interactively_in_independent_trees_under_the_host() {
    let lab = Lab::start("shells");
    let cmd = lab.new_terminal("cmd", &["--", "cmd.exe", "/d"]);
    let mut ps_args = PS_ENV.to_vec();
    ps_args.extend(["--", "powershell", "-NoLogo", "-NoProfile"]);
    let ps = lab.new_terminal("ps", &ps_args);
    let exe = |p: &Value| p["executable"].as_str().unwrap().to_lowercase();
    assert!(exe(&cmd).ends_with(r"\system32\cmd.exe"), "{cmd}");
    assert!(
        exe(&ps).ends_with(r"\windowspowershell\v1.0\powershell.exe"),
        "a bare `powershell` resolves to Windows PowerShell 5.1: {ps}"
    );
    assert_eq!(
        ps["env"],
        json!({"set": ["POWERSHELL_TELEMETRY_OPTOUT", "POWERSHELL_UPDATECHECK"], "unset": []}),
        "names only"
    );
    let mut shells = vec![("cmd", pid_of(&cmd["pid"])), ("ps", pid_of(&ps["pid"]))];
    let pwsh = pwsh_installed(&lab);
    if pwsh {
        let mut a = PS_ENV.to_vec();
        a.extend(["--", "pwsh", "-NoLogo", "-NoProfile"]);
        let p = lab.new_terminal("pwsh", &a);
        assert!(exe(&p).ends_with("pwsh.exe"), "{p}");
        shells.push(("pwsh", pid_of(&p["pid"])));
    }
    // Every shell is a child of the Host itself, with its own pseudo console host.
    for (name, pid) in &shells {
        assert_eq!(
            parent_of(*pid),
            Some(lab.host.pid),
            "{name} runs under the Host"
        );
    }
    assert_eq!(
        lab.conhosts().len(),
        shells.len(),
        "one pseudo console each"
    );

    // Interactive: commands typed into each shell run there.
    let mut c = lab.attach("cmd");
    c.send(b"echo cmd-mark^er-%SUBAK_TERMINAL_ID:~0,2% & ver\r");
    let text = c.expect("cmd-marker-t-");
    assert!(text.contains("Microsoft Windows"), "{text}");
    let mut p = lab.attach("ps");
    p.send(b"Write-Output ('ps-' + 'marker-' + $PSVersionTable.PSVersion.Major)\r");
    p.expect_within("ps-marker-5", PS_START);
    let mut seven = None;
    if pwsh {
        let mut s = lab.attach("pwsh");
        s.send(b"Write-Output ('pwsh-' + 'marker-' + $PSVersionTable.PSVersion.Major)\r");
        let text = s.expect_within("pwsh-marker-", PS_START);
        seven = text
            .split("pwsh-marker-")
            .nth(1)
            .and_then(|t| t.chars().next());
        drop(s);
    }

    // Each shell starts a grandchild; each grandchild lives in its own Terminal's job.
    c.send(b"start /b ping -n 600 127.0.0.1 >nul & echo started-mark^er\r");
    c.expect("started-marker");
    p.send(
        b"$j = Start-Process ping.exe -ArgumentList '-n','600','127.0.0.1' -NoNewWindow -PassThru; 'ping-' + 'pid=' + $j.Id + '='\r",
    );
    let text = p.expect("ping-pid=");
    let ps_ping: u32 = text
        .split("ping-pid=")
        .nth(1)
        .and_then(|t| t.split('=').next())
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or_else(|| panic!("no ping pid in:\n{text}"));
    let mut cmd_ping = Vec::new();
    wait_until("cmd's ping started", LONG, || {
        cmd_ping = children(shells[0].1, "ping.exe");
        !cmd_ping.is_empty()
    });
    let cmd_ping = cmd_ping[0];
    assert_eq!(parent_of(ps_ping), Some(shells[1].1));
    assert_eq!(lab.info("cmd")["busy"], true);
    assert_eq!(lab.info("ps")["busy"], true);
    assert!(lab.info("ps")["windows"][0]["panes"][0]["processes"].as_u64() >= Some(2));

    // Killing one Terminal ends its whole tree and nothing of the others.
    drop(c);
    let report = lab.kill("cmd");
    assert_eq!(report["affected"][0]["job_processes_left"], 0, "{report}");
    wait_until("cmd's tree ended", LONG, || {
        !alive(shells[0].1) && !alive(cmd_ping)
    });
    assert!(
        alive(shells[1].1) && alive(ps_ping),
        "PowerShell's tree is untouched"
    );
    p.send(b"'still-' + 'alive'\r");
    p.expect("still-alive");
    drop(p);
    lab.kill("ps");
    wait_until("PowerShell's tree ended", LONG, || {
        !alive(shells[1].1) && !alive(ps_ping)
    });
    eprintln!(
        "EVIDENCE shells: host={} cmd={} (ping {cmd_ping}) powershell={} (ping {ps_ping}) pwsh={}; \
         each a child of the Host with its own conhost; killing cmd ended cmd+ping only",
        lab.host.pid,
        shells[0].1,
        shells[1].1,
        if pwsh {
            format!("{} (major {})", shells[2].1, seven.unwrap_or('?'))
        } else {
            "not installed (not tested here)".into()
        }
    );
}

/// The child side of `explicit_programs_get_their_arguments_exactly_and_report_exit_codes`:
/// when started as a Terminal's program with `SUBAK_ARGV_ECHO_FILE` set, this test binary writes
/// what it received (its own MSVC-runtime argument parsing, environment and working directory).
/// In a normal test run it does nothing.
#[test]
fn argv_echo_helper() {
    let Some(file) = std::env::var_os("SUBAK_ARGV_ECHO_FILE") else {
        return;
    };
    let var = |k: &str| std::env::var(k).ok();
    // A descendant may not leave the pane's job (no JOB_OBJECT_LIMIT_BREAKAWAY_OK, ADR 0004).
    let breakaway = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "exit 0"])
        .creation_flags(
            CREATE_NO_WINDOW | windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB,
        )
        .spawn()
        .map(|mut c| {
            let _ = c.wait();
        })
        .err()
        .and_then(|e| e.raw_os_error());
    let doc = json!({
        "breakaway_error": breakaway,
        "args": std::env::args().collect::<Vec<_>>(),
        "cwd": std::env::current_dir().unwrap().display().to_string(),
        "pane_id": var("SUBAK_PANE_ID"),
        "terminal_id": var("SUBAK_TERMINAL_ID"),
        "greeting": var("SUBAK_GREETING"),
        "launcher_only": var("SUBAK_LAUNCHER_ONLY"),
        "temp": var("TEMP"),
        "userprofile": var("USERPROFILE"),
    });
    std::fs::write(file, doc.to_string()).unwrap();
}

#[test]
fn explicit_programs_get_their_arguments_exactly_and_report_exit_codes() {
    let lab = Lab::start_env("argv", &[("SUBAK_LAUNCHER_ONLY", "launcher")]);
    let watcher = Raw::handshake(&lab.ep, "gui");
    assert_eq!(
        watcher.request(
            1,
            "events.subscribe",
            &json!({"topics": ["panes", "terminals"]})
        )["ok"],
        true
    );
    let dir = Scratch::new("argv-서버");
    let out = dir.path("argv.json");
    let me = std::env::current_exe().unwrap();
    let tricky = [
        "",
        "a b",
        "\"",
        "say \"hi\"",
        "C:\\Program Files\\x\\",
        "a\\\\\"b",
        "100% ^&|<>()",
        "한글 인자",
        "--flag=\"quoted value\"",
        "trailing\\",
    ];
    let mut args: Vec<String> = vec![
        "terminal".into(),
        "new".into(),
        "argv".into(),
        "--remain-on-exit".into(),
        "--cwd".into(),
        dir.0.display().to_string(),
        "--env".into(),
        format!("SUBAK_ARGV_ECHO_FILE={}", out.display()),
        "--env".into(),
        "SUBAK_GREETING=안녕하세요 =x".into(),
        "--unset-env".into(),
        "TEMP".into(),
        "--".into(),
        me.display().to_string(),
        "argv_echo_helper".into(),
        "--exact".into(),
        "--test-threads=1".into(),
        "--".into(),
    ];
    args.extend(tricky.iter().map(|s| (*s).to_string()));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, doc) = lab.cli(&a);
    assert_eq!(code, 0, "{doc}");
    let tid = doc["data"]["terminal_id"].as_str().unwrap().to_string();
    let pane_id = doc["data"]["pane_id"].as_str().unwrap().to_string();
    wait_until("the helper wrote its argv", LONG, || out.exists());
    wait_until("the helper exited", LONG, || {
        lab.info(&tid)["state"] == "stopped"
    });
    let got: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let argv: Vec<String> = got["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        argv[5..],
        tricky.map(String::from),
        "every argument arrives exactly: {argv:?}"
    );
    assert_eq!(
        got["cwd"],
        dir.0.display().to_string(),
        "the given working directory"
    );
    assert_eq!(
        got["breakaway_error"], 5,
        "CREATE_BREAKAWAY_FROM_JOB from inside a pane is refused (access denied)"
    );
    assert_eq!(got["pane_id"], pane_id.as_str(), "SUBAK_PANE_ID (ADR 0010)");
    assert_eq!(got["terminal_id"], tid.as_str());
    assert_eq!(got["greeting"], "안녕하세요 =x", "a Unicode value with `=`");
    assert_eq!(got["temp"], Value::Null, "--unset-env removed it");
    assert_eq!(
        got["launcher_only"],
        Value::Null,
        "the launcher's variables never reach a pane"
    );
    assert_eq!(
        got["userprofile"].as_str(),
        std::env::var("USERPROFILE").ok().as_deref()
    );
    let t = lab.info(&tid);
    assert_eq!(t["last_exit"]["exit_code"], 0, "{t}");
    assert_eq!(t["last_exit"]["cause"], "exited");
    let shown = t.to_string();
    assert!(
        !shown.contains("안녕하세요") && !shown.contains("argv.json"),
        "env values are never shown: {shown}"
    );
    assert_eq!(
        t["windows"][0]["panes"][0]["env"],
        json!({"set": ["SUBAK_ARGV_ECHO_FILE", "SUBAK_GREETING"], "unset": ["TEMP"]})
    );

    // Exit statuses: of cmd, of PowerShell, an NTSTATUS, and of a program without
    // remain-on-exit (removed; its status is in `pane.exited`).
    let cases: [(&str, Vec<&str>, u64); 4] = [
        ("exit42", vec!["cmd.exe", "/d", "/c", "exit 42"], 42),
        (
            "ps9",
            vec![
                "powershell",
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "exit 9",
            ],
            9,
        ),
        (
            "ntstatus",
            vec!["cmd.exe", "/d", "/c", "exit -1073741510"],
            0xC000_013A,
        ),
        ("gone", vec!["cmd.exe", "/d", "/c", "exit 3"], 3),
    ];
    let mut ids = Vec::new();
    for (name, command, want) in &cases {
        let mut a = vec!["new", name];
        if *name != "gone" {
            a.push("--remain-on-exit");
        }
        a.push("--");
        a.extend(command.iter().copied());
        let (code, doc) = lab.cli(&a);
        assert_eq!(code, 0, "{doc}");
        ids.push((
            doc["data"]["terminal_id"].as_str().unwrap().to_string(),
            doc["data"]["pane_id"].as_str().unwrap().to_string(),
            *want,
            *name,
        ));
    }
    let deadline = Instant::now() + PS_START;
    let mut exits = std::collections::BTreeMap::new();
    let mut removed = false;
    while exits.len() < ids.len() || !removed {
        assert!(Instant::now() < deadline, "exits so far: {exits:?}");
        if let Got::Control(m) = watcher.next_control(Duration::from_millis(200)) {
            let mine = ids
                .iter()
                .any(|(_, p, _, _)| m["data"]["pane_id"] == p.as_str());
            if m["event"] == "pane.exited" && mine {
                exits.insert(
                    m["data"]["pane_id"].as_str().unwrap().to_string(),
                    m["data"]["exit_code"].as_u64(),
                );
            }
            if m["event"] == "terminal.removed" && m["data"]["terminal_id"] == ids[3].0.as_str() {
                assert_eq!(m["data"]["reason"], "exited");
                removed = true;
            }
        }
    }
    for (tid, pid, want, name) in &ids {
        assert_eq!(exits[pid], Some(*want), "{name}: pane.exited");
        if *name != "gone" {
            let t = lab.info(tid);
            assert_eq!(t["state"], "stopped", "{name}");
            assert_eq!(t["last_exit"]["exit_code"], *want, "{name}: info");
            assert_eq!(t["last_exit"]["forced"], false, "{name}");
        }
    }
    let (code, _) = lab.cli(&["info", &ids[3].0]);
    assert_eq!(code, 3, "without --remain-on-exit the Terminal is removed");
    eprintln!(
        "EVIDENCE argv: {} tricky arguments round-tripped through quoting and CreateProcessW into the \
         MSVC runtime of a real child; exit codes 42 (cmd), 9 (powershell), 0xC000013A, 3 (removed) \
         reported in terminal.info and pane.exited",
        tricky.len()
    );
}

#[test]
fn startup_failures_are_precise_and_leave_nothing_behind() {
    let lab = Lab::start("fail");
    let dir = Scratch::new("fail");
    let sys = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
    std::fs::write(dir.path("build.cmd"), "@echo off\r\necho built\r\n").unwrap();
    std::fs::write(dir.path("setup.ps1"), "'x'\r\n").unwrap();
    std::fs::write(dir.path("fake.exe"), "this is not a program").unwrap();
    std::fs::copy(sys.join("whoami.exe"), dir.path("denied.exe")).unwrap();
    std::fs::create_dir(dir.path("locked")).unwrap();
    // Deny execute on one program and traverse on one directory, for everyone; undone below.
    icacls(&dir.path("denied.exe"), &["/deny", "*S-1-1-0:(X)"]);
    icacls(&dir.path("locked"), &["/deny", "*S-1-1-0:(X)"]);
    let dir_path = dir.0.display().to_string();
    let path_override = format!("PATH={dir_path}");
    let cases: Vec<(Vec<String>, &str, &str)> = vec![
        (
            vec!["--".into(), r"C:\subak-nope\missing.exe".into()],
            "not_found",
            "check the path",
        ),
        (
            vec!["--".into(), "definitely-not-a-program-subak".into()],
            "not_found",
            "PATH",
        ),
        (
            vec!["--".into(), dir.s("build.cmd")],
            "not_executable",
            "cmd.exe /d /c",
        ),
        (
            // A bare name that resolves to a batch script on the pane's PATH (like `npm`).
            vec!["--env".into(), path_override, "--".into(), "build".into()],
            "not_executable",
            "cmd.exe /d /c",
        ),
        (
            vec!["--".into(), dir.s("setup.ps1")],
            "not_executable",
            "powershell",
        ),
        (
            vec!["--".into(), dir.s("fake.exe")],
            "not_executable",
            "not a program Windows can start",
        ),
        (vec!["--".into(), dir.s("denied.exe")], "access_denied", ""),
        (
            vec![
                "--cwd".into(),
                dir.s("missing"),
                "--".into(),
                "cmd.exe".into(),
            ],
            "bad_cwd",
            "",
        ),
        (
            vec![
                "--cwd".into(),
                dir.s("build.cmd"),
                "--".into(),
                "cmd.exe".into(),
            ],
            "bad_cwd",
            "",
        ),
        (
            vec![
                "--cwd".into(),
                dir.s("locked"),
                "--".into(),
                "cmd.exe".into(),
            ],
            "bad_cwd",
            "",
        ),
    ];
    // Warm up, then measure the Host's handles over three rounds of every failure.
    let _ = lab.cli(&["new", "--", r"C:\subak-nope\missing.exe"]);
    let before = lab.handles();
    let mut failures = 0;
    let mut messages = Vec::new();
    for round in 0..3 {
        for (args, reason, hint) in &cases {
            let a: Vec<&str> = args.iter().map(String::as_str).collect();
            let e = lab.new_fails(&a);
            assert_eq!(
                detail(&e, "reason").as_deref(),
                Some(*reason),
                "round {round} {a:?}: {e}"
            );
            if !hint.is_empty() {
                let all = format!("{} {}", e["message"], e["hint"]);
                assert!(all.contains(hint), "{a:?}: {e}");
            }
            if round == 0 {
                messages.push(format!(
                    "{reason}: {}",
                    e["message"].as_str().unwrap().replace(&dir_path, "<dir>")
                ));
            }
            failures += 1;
        }
    }
    let after = lab.handles();
    // Dry runs check the same things and start nothing.
    let (code, doc) = lab.cli(&["new", "--dry-run", "--", dir.s("build.cmd").as_str()]);
    assert_eq!(
        (code, detail(&doc["error"], "reason").as_deref()),
        (1, Some("not_executable"))
    );
    let (code, doc) = lab.cli(&["new", "--dry-run", "--", "cmd"]);
    assert_eq!(code, 0, "{doc}");
    assert!(
        doc["data"]["would_create"]["executable"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .ends_with(r"\system32\cmd.exe")
    );
    // Malformed overrides never reach the Host's spawn path.
    let (code, doc) = lab.cli(&["new", "--env", "=x"]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    let (code, doc) = lab.cli(&["new", "--env", "SUBAK_PANE_ID=p-forged"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (2, Some("usage")),
        "{doc}"
    );
    let raw = Raw::handshake(&lab.ep, "cli");
    for bad in [
        json!({"env": {"SUBAK_TERMINAL_ID": "t-forged"}}),
        json!({"env": {"X": 5}}),
        json!({"env": "X=1"}),
    ] {
        let r = raw.request(1, "terminal.create", &bad);
        assert_eq!(error_code(&r), "invalid_params", "{bad}: {r}");
        assert_eq!(r["error"]["details"]["field"], "env");
    }
    // keep_on_failure records the precise reason.
    let (code, doc) = lab.cli(&[
        "new",
        "kept",
        "--keep-on-failure",
        "--",
        dir.s("fake.exe").as_str(),
    ]);
    assert_eq!(code, 1, "{doc}");
    assert_eq!(lab.info("kept")["failure"]["reason"], "not_executable");
    let (code, _) = lab.cli(&["rm", "kept", "--yes"]);
    assert_eq!(code, 0);

    let (code, doc) = lab.cli(&["ls"]);
    assert_eq!(code, 0);
    assert_eq!(
        doc["data"]["terminals"],
        json!([]),
        "no half-created Terminal"
    );
    assert!(lab.conhosts().is_empty(), "no pseudo console remains");
    let kids: Vec<_> = processes()
        .into_iter()
        .filter(|(_, parent, _)| *parent == lab.host.pid)
        .collect();
    assert!(kids.is_empty(), "the Host has no child process: {kids:?}");
    let s = lab.stats();
    assert_eq!(
        s["sessions"]["spawn_failures_total"].as_u64(),
        Some(failures + 2),
        "{s}"
    );
    icacls(&dir.path("denied.exe"), &["/remove:d", "*S-1-1-0"]);
    icacls(&dir.path("locked"), &["/remove:d", "*S-1-1-0"]);
    // Every case is refused before a pseudo console exists, so not even the handle Windows 10's
    // ConPTY keeps per pseudo console accumulates.
    assert!(
        after <= before + 8,
        "Host handles grew from {before} to {after} over {failures} failed starts"
    );
    eprintln!(
        "EVIDENCE failures: {failures} failed starts, Host handles {before} -> {after} (min of 5 samples), \
         no Terminal/conhost/child left; messages: {messages:#?}"
    );
}

#[test]
fn ctrl_c_and_unicode_reach_programs_in_cmd_and_powershell() {
    let lab = Lab::start("ctrlc");
    lab.new_terminal("cmd", &["--", "cmd.exe", "/d"]);
    let mut ps_args = PS_ENV.to_vec();
    ps_args.extend(["--", "powershell", "-NoLogo", "-NoProfile"]);
    lab.new_terminal("ps", &ps_args);

    // cmd: Ctrl+C (0x03) interrupts a running ping; Korean typed in comes back.
    let mut c = lab.attach("cmd");
    c.send("echo 한글-입력-마커 & echo ready-mark^er\r".as_bytes());
    let text = c.expect("ready-marker");
    assert!(text.contains("한글-입력-마커"), "{text}");
    // A UTF-8 sequence split across input frames is still one character.
    let split = "echo 분할-서버-마커\r".as_bytes().to_vec();
    for b in split.chunks(1) {
        c.send(b);
    }
    c.expect("분할-서버-마커");
    c.send(b"ping -n 60 127.0.0.1\r");
    c.expect("Reply from 127.0.0.1");
    let t0 = Instant::now();
    c.send(b"\x03");
    c.send(b"echo after-ctrl-c-mark^er\r");
    c.expect_within("after-ctrl-c-marker", Duration::from_secs(15));
    let cmd_ms = t0.elapsed().as_millis();
    assert!(
        children(c.pid, "ping.exe").is_empty(),
        "ping was interrupted"
    );

    // PowerShell: Ctrl+C stops a running cmdlet and a native child; Unicode round-trips.
    let mut p = lab.attach("ps");
    p.send("Write-Output ('유니코드-' + '확인')\r".as_bytes());
    p.expect_within("유니코드-확인", PS_START);
    p.send(b"Start-Sleep -Seconds 60; 'not-' + 'interrupted'\r");
    std::thread::sleep(Duration::from_millis(800));
    p.clear();
    let t1 = Instant::now();
    p.send(b"\x03");
    ps_prompt(&mut p);
    let ps_ms = t1.elapsed().as_millis();
    ps_command(&mut p, b"'after-' + 'ps-ctrl-c'\r", "after-ps-ctrl-c");
    assert!(!p.text().contains("not-interrupted"), "{}", p.text());
    p.send(b"ping.exe -n 60 127.0.0.1\r");
    p.expect("Reply from 127.0.0.1");
    p.clear();
    p.send(b"\x03");
    ps_prompt(&mut p);
    ps_command(
        &mut p,
        b"'after-' + 'native-ctrl-c'\r",
        "after-native-ctrl-c",
    );
    assert!(
        children(p.pid, "ping.exe").is_empty(),
        "native ping was interrupted"
    );
    // Both shells survive Ctrl+C.
    assert!(alive(c.pid) && alive(p.pid));
    eprintln!(
        "EVIDENCE ctrl-c: cmd ping interrupted, next command after {cmd_ms} ms; PowerShell Start-Sleep \
         interrupted after {ps_ms} ms, native ping interrupted; Korean input/output round-tripped \
         in both, a UTF-8 character split across 1-byte input frames arrived whole"
    );
}

#[test]
fn resize_storms_and_input_floods_never_block_the_host() {
    let lab = Lab::start("storm");
    lab.new_terminal("storm", &["--", "cmd.exe", "/d", "/q"]);
    lab.new_terminal("flood", &["--", "cmd.exe", "/d", "/q"]);
    lab.new_terminal("other", &["--", "cmd.exe", "/d", "/q"]);
    let before = lab.handles();

    // A resize storm while the pane produces output: 400 resizes pipelined on one connection.
    let mut s = lab.attach("storm");
    s.send(b"ping -t 127.0.0.1\r");
    s.expect("Reply from 127.0.0.1");
    let t0 = Instant::now();
    let n = 400u64;
    for i in 0..n {
        let (c, r) = (40 + (i * 7) % 160, 10 + (i * 3) % 50);
        s.raw.send(
            &json!({"type": "request", "id": 100 + i, "method": "pane.resize",
                           "params": {"cols": c, "rows": r}}),
        );
    }
    s.raw.send(
        &json!({"type": "request", "id": 100 + n, "method": "pane.resize",
                       "params": {"cols": 97, "rows": 33}}),
    );
    let mut answered = 0;
    let deadline = Instant::now() + LONG;
    while answered <= n {
        assert!(
            Instant::now() < deadline,
            "{answered} of {} resizes answered",
            n + 1
        );
        match s.raw.next(Duration::from_millis(500)) {
            Got::Control(m) if m["type"] == "response" => {
                assert_eq!(m["ok"], true, "{m}");
                answered += 1;
            }
            Got::Eof => panic!("the Host closed the connection during the storm"),
            _ => {}
        }
    }
    let storm_ms = t0.elapsed().as_millis();
    s.send(b"\x03");
    s.clear();
    s.send(b"mode con\r");
    s.expect("Columns:");
    let text = s.expect("97");
    assert!(text.contains("Lines:"), "{text}");
    let size = &lab.info("storm")["windows"][0]["panes"][0]["size"];
    assert_eq!(*size, json!({"cols": 97, "rows": 33}));

    // An input flood into a pane whose program does not read: the flooding client waits, nobody
    // else does, and the flooded Terminal can still be killed at once.
    let f = lab.attach("flood");
    f.send(b"ping -n 600 127.0.0.1 >nul\r");
    std::thread::sleep(Duration::from_millis(500));
    let sent = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let flooder = {
        let (sent, stop) = (Arc::clone(&sent), Arc::clone(&stop));
        let (h, stream) = (f.raw, f.stream);
        std::thread::spawn(move || {
            let chunk = vec![b'a'; 256 * 1024];
            while !stop.load(Ordering::SeqCst) && sent.load(Ordering::SeqCst) < 64 << 20 {
                if h.write(&data(stream, 0, INPUT, &chunk)).is_err() {
                    break;
                }
                sent.fetch_add(chunk.len(), Ordering::SeqCst);
            }
        })
    };
    let mut o = lab.attach("other");
    let mut worst = Duration::ZERO;
    for i in 0..10 {
        let t = Instant::now();
        let h = Raw::handshake(&lab.ep, "cli").request(1, "host.health", &json!({}));
        assert_eq!(h["result"]["healthy"], true);
        o.send(format!("echo other-{i}-mark^er\r").as_bytes());
        o.expect(&format!("other-{i}-marker"));
        worst = worst.max(t.elapsed());
        std::thread::sleep(Duration::from_millis(200));
    }
    let flooded = sent.load(Ordering::SeqCst);
    let t1 = Instant::now();
    let report = lab.kill("flood");
    let kill_ms = t1.elapsed().as_millis();
    stop.store(true, Ordering::SeqCst);
    assert!(kill_ms < 15_000, "kill took {kill_ms} ms");
    assert_eq!(report["removed"], true, "{report}");
    let _ = flooder.join();
    assert!(
        worst < Duration::from_secs(3),
        "another client waited {worst:?} during the flood"
    );
    drop((s, o));
    lab.kill("storm");
    lab.kill("other");
    let mem = lab.stats()["host"]["memory"].clone();
    let after = lab.handles();
    assert!(
        after <= before + 24,
        "Host handles {before} -> {after} after the storm and the flood"
    );
    assert!(
        mem["private_bytes"].as_u64().unwrap() < 256 << 20,
        "the Host's memory stays bounded: {mem}"
    );
    eprintln!(
        "EVIDENCE storm: {} resizes answered in {storm_ms} ms during `ping -t`, final 97x33 reached \
         the shell; flood: {} KiB of input accepted by a pane that does not read, other clients' \
         worst round trip {worst:?}, kill of the flooded Terminal {kill_ms} ms; Host memory {mem}; \
         handles {before} -> {after}",
        n + 1,
        flooded / 1024
    );
}

#[test]
fn large_output_streams_to_fast_clients_while_stalled_ones_are_dropped() {
    let lab = Lab::start("big");
    let dir = Scratch::new("big");
    let file = dir.path("big.txt");
    let line =
        "0123456789abcdefghijklmnopqrstuvwxyz 한글 ABCDEFGHIJKLMNOPQRSTUVWXYZ-0123456789\r\n";
    let mib = 20usize;
    std::fs::write(&file, line.repeat(mib * 1024 * 1024 / line.len())).unwrap();
    let size = std::fs::metadata(&file).unwrap().len();
    lab.new_terminal("big", &["--", "cmd.exe", "/d", "/q"]);
    let base = lab.stats();
    let mut fast = lab.attach("big");
    fast.send(b"echo ready-mark^er\r");
    fast.expect("ready-marker");
    // Two clients that attach and then never read again.
    let stalled: Vec<Raw> = (0..2)
        .map(|_| {
            let r = Raw::handshake(&lab.ep, "gui");
            let a = r.request(
                1,
                "pane.attach",
                &json!({"target": "big", "interactive": false}),
            );
            assert_eq!(a["ok"], true, "{a}");
            r
        })
        .collect();
    fast.clear();
    let t0 = Instant::now();
    fast.send(format!("type \"{}\" & echo done-mark^er\r", file.display()).as_bytes());
    // Read only the tail, so the test itself stays small.
    let mut total = 0usize;
    let mut frames = 0usize;
    let mut tail: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut health_worst = Duration::ZERO;
    let mut last_health = Instant::now();
    loop {
        assert!(
            Instant::now() < deadline,
            "done not seen after {total} bytes"
        );
        match fast.raw.next(Duration::from_millis(250)) {
            Got::Data(b) => {
                total += b.len();
                frames += 1;
                let n = b.len();
                tail.extend(b);
                if tail.len() > 8192 {
                    tail.drain(..tail.len() - 8192);
                }
                // Search only the new bytes (and a marker's length before them): the client
                // must keep up with the pane, or the Host rightly drops it as lagging.
                let from = tail.len().saturating_sub(n + 16);
                if tail[from..].windows(11).any(|w| w == b"done-marker") {
                    break;
                }
            }
            Got::Eof => panic!("the fast client was dropped after {total} bytes"),
            _ => {}
        }
        if last_health.elapsed() > Duration::from_secs(2) {
            let t = Instant::now();
            let h = Raw::handshake(&lab.ep, "cli").request(1, "host.health", &json!({}));
            assert_eq!(h["result"]["healthy"], true);
            health_worst = health_worst.max(t.elapsed());
            last_health = Instant::now();
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let s = lab.stats();
    let dropped = s["ipc"]["lagging_clients_dropped"].as_u64().unwrap();
    let peak = s["host"]["memory"]["peak_private_bytes"].as_u64().unwrap();
    let peak0 = base["host"]["memory"]["peak_private_bytes"]
        .as_u64()
        .unwrap();
    // The stalled clients were disconnected; their pipes report the end once drained.
    assert_eq!(dropped, 2, "{s}");
    for r in &stalled {
        let until = Instant::now() + LONG;
        loop {
            match r.next(Duration::from_millis(500)) {
                Got::Eof => break,
                _ => assert!(Instant::now() < until, "a stalled client was not dropped"),
            }
        }
    }
    let grew = peak.saturating_sub(peak0);
    assert!(
        grew < 64 << 20,
        "two stalled clients share one bounded queue of output (peak grew by {grew} bytes)"
    );
    assert!(health_worst < Duration::from_secs(2), "{health_worst:?}");
    assert!(total as u64 >= size / 2, "received {total} of {size} bytes");
    eprintln!(
        "EVIDENCE output: {size} bytes typed through ConPTY, {total} bytes of VT output received by \
         the fast client in {secs:.1} s ({:.2} MiB/s) as {frames} data frames (avg {} bytes); \
         2 stalled clients dropped (lagging_clients_dropped {dropped}); Host peak private bytes \
         grew by {:.1} MiB; host.health worst {health_worst:?}",
        total as f64 / secs / 1_048_576.0,
        total / frames.max(1),
        grew as f64 / 1_048_576.0
    );
}

#[test]
fn the_pane_environment_keeps_the_users_locations_and_secrets_never_leak() {
    let secret = "s3cr3t-token-for-the-subak-step11-test";
    let copied = "c0pied-value-for-the-subak-step11-test";
    let real_profile = std::env::var("USERPROFILE").unwrap();
    // The launcher looks like an agent's terminal: a Git Bash HOME, a token, a Claude session.
    let lab = Lab::start_env(
        "env",
        &[
            ("HOME", r"C:\launcher-home"),
            ("GITHUB_TOKEN", secret),
            ("CLAUDE_CODE_SESSION_ID", secret),
        ],
    );
    let (code, doc) = lab.cli_env(
        &[
            "new",
            "envs",
            "--env",
            &format!("SUBAK_SECRET_TEST={secret}"),
            "--env",
            "SUBAK_COPIED_TEST",
            "--",
            "cmd.exe",
            "/d",
        ],
        &[("SUBAK_COPIED_TEST", copied)],
    );
    assert_eq!(code, 0, "{doc}");
    let tid = doc["data"]["terminal_id"].as_str().unwrap().to_string();
    let pid = doc["data"]["pane_id"].as_str().unwrap().to_string();
    let mut outputs = vec![doc.to_string()];
    let mut c = lab.attach("envs");
    let vars = [
        "%USERPROFILE%",
        "%HOMEDRIVE%%HOMEPATH%",
        "%APPDATA%",
        "%LOCALAPPDATA%",
        "%HOME%",
        "%GITHUB_TOKEN%",
        "%CLAUDE_CODE_SESSION_ID%",
        "%SUBAK_SECRET_TEST%",
        "%SUBAK_COPIED_TEST%",
        "%SUBAK_PANE_ID%",
        "%SUBAK_TERMINAL_ID%",
    ];
    // One short line per variable; `f^N` is what the console echoes as typed, `fN` only appears
    // in the output.
    for (i, v) in vars.iter().enumerate() {
        c.send(format!("echo f^{i}=[{v}]\r").as_bytes());
    }
    c.send(b"echo env-done-mark^er\r");
    let text = c.expect("env-done-marker");
    let fields: Vec<String> = (0..vars.len())
        .map(|i| {
            let start = format!("f{i}=[");
            let at = text
                .find(&start)
                .unwrap_or_else(|| panic!("no `{start}` in:\n{text}"))
                + start.len();
            let end = text[at..].find(']').unwrap() + at;
            text[at..end].to_string()
        })
        .collect();
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    assert_eq!(
        fields[0], real_profile,
        "USERPROFILE (Git, OpenSSH, Claude Code)"
    );
    assert_eq!(
        fields[1],
        format!("{}{}", var("HOMEDRIVE"), var("HOMEPATH")),
        "HOMEDRIVE+HOMEPATH"
    );
    assert_eq!(fields[2], var("APPDATA"));
    assert_eq!(fields[3], var("LOCALAPPDATA"));
    assert_ne!(
        fields[4], r"C:\launcher-home",
        "the launcher's HOME is not inherited"
    );
    assert_eq!(
        fields[5], "%GITHUB_TOKEN%",
        "the launcher's token is not inherited"
    );
    assert_eq!(fields[6], "%CLAUDE_CODE_SESSION_ID%");
    assert_eq!(fields[7], secret, "an explicit override reaches the pane");
    assert_eq!(fields[8], copied, "--env NAME copies the CLI's own value");
    assert_eq!(fields[9], pid);
    assert_eq!(fields[10], tid);
    drop(c);

    // Nothing a client can read shows a value: create, info, list, status, stats, dry runs,
    // events, and the CLI's diagnostics with --verbose.
    let watcher = Raw::handshake(&lab.ep, "gui");
    watcher.request(
        1,
        "events.subscribe",
        &json!({"topics": ["terminals", "panes"]}),
    );
    for args in [vec!["info", "envs"], vec!["ls"], vec!["host", "status"]] {
        outputs.push(lab.cli(&args).1.to_string());
    }
    let full =
        Raw::handshake(&lab.ep, "cli").request(1, "terminal.list", &json!({"detail": "full"}));
    outputs.push(full.to_string());
    outputs.push(lab.stats().to_string());
    outputs.push(
        lab.cli(&[
            "new",
            "--dry-run",
            "--env",
            &format!("SUBAK_SECRET_TEST={secret}"),
        ])
        .1
        .to_string(),
    );
    let (code, doc) = lab.cli(&[
        "new",
        "second",
        "--env",
        &format!("TOKEN={secret}"),
        "--",
        "cmd.exe",
        "/d",
    ]);
    assert_eq!(code, 0);
    outputs.push(doc.to_string());
    let verbose = std::process::Command::new(subak())
        .args(["--endpoint", &lab.ep, "-v", "new", "third", "--env"])
        .arg(format!("TOKEN={secret}"))
        .args(["--", "cmd.exe", "/d"])
        .stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(verbose.status.success(), "{verbose:?}");
    outputs.push(String::from_utf8_lossy(&verbose.stdout).into_owned());
    outputs.push(String::from_utf8_lossy(&verbose.stderr).into_owned());
    let failed = lab.cli(&[
        "new",
        "--env",
        &format!("TOKEN={secret}"),
        "--",
        "C:\\nope.exe",
    ]);
    outputs.push(failed.1.to_string());
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Got::Control(m) = watcher.next_control(Duration::from_millis(100)) {
            outputs.push(m.to_string());
        }
    }
    for o in &outputs {
        assert!(
            !o.contains(secret) && !o.contains(copied),
            "a value leaked: {o}"
        );
    }
    for t in ["second", "third"] {
        assert_eq!(
            lab.info(t)["windows"][0]["panes"][0]["env"],
            json!({"set": ["TOKEN"], "unset": []})
        );
    }
    eprintln!(
        "EVIDENCE env: pane saw USERPROFILE/HOMEDRIVE+HOMEPATH/APPDATA/LOCALAPPDATA of the user, not \
         the launcher's HOME/GITHUB_TOKEN/CLAUDE_CODE_SESSION_ID; overrides and SUBAK_PANE_ID/ \
         SUBAK_TERMINAL_ID arrived; {} documents and streams checked for leaked values: none",
        outputs.len()
    );
}

/// Removes the user environment variable this test sets, even when it fails.
struct UserVar(&'static str);

impl UserVar {
    fn reg(args: &[&str]) -> bool {
        std::process::Command::new("reg")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn set(name: &'static str, value: &str) -> Self {
        assert!(Self::reg(&[
            "add",
            r"HKCU\Environment",
            "/v",
            name,
            "/t",
            "REG_SZ",
            "/d",
            value,
            "/f"
        ]));
        Self(name)
    }
}

impl Drop for UserVar {
    fn drop(&mut self) {
        Self::reg(&["delete", r"HKCU\Environment", "/v", self.0, "/f"]);
        assert!(
            !Self::reg(&["query", r"HKCU\Environment", "/v", self.0]),
            "the test variable was removed again"
        );
    }
}

/// Manual: briefly writes a variable to the user's persistent environment (`HKCU\Environment`,
/// like `setx`), then removes it. It is not broadcast, so no other program notices. Run with
/// `cargo test -p subak --test pty_shells -- --ignored --nocapture`.
#[test]
#[ignore = "manual: briefly sets a user environment variable in HKCU\\Environment"]
fn each_new_pane_gets_a_fresh_logon_environment() {
    let lab = Lab::start("fresh");
    let dir = Scratch::new("fresh");
    let probe = |name: &str, out: &Path| {
        // No quotes inside: cmd.exe parses its own /c line and does not understand the `\"`
        // escapes of the MSVC rules (docs/PTY_PROCESSES.md §2). The temp path has no spaces.
        let line = format!("set SUBAK_STEP11_FRESH > {} 2>&1", out.display());
        let (code, doc) = lab.cli(&["new", name, "--", "cmd.exe", "/d", "/c", &line]);
        assert_eq!(code, 0, "{doc}");
        wait_until("the probe wrote its file", LONG, || {
            std::fs::read_to_string(out).is_ok_and(|s| !s.is_empty())
        });
        std::fs::read_to_string(out).unwrap()
    };
    let before = probe("before", &dir.path("before.txt"));
    let set = UserVar::set("SUBAK_STEP11_FRESH", "fresh-value-11");
    let during = probe("during", &dir.path("during.txt"));
    drop(set);
    let after = probe("after", &dir.path("after.txt"));
    assert!(!before.contains("fresh-value-11"), "{before}");
    assert!(
        during.contains("SUBAK_STEP11_FRESH=fresh-value-11"),
        "a variable set after the Host started reaches the next new pane: {during}"
    );
    assert!(!after.contains("fresh-value-11"), "{after}");
    eprintln!(
        "EVIDENCE fresh env: Host {} started before the variable existed; the pane created while \
         it was set saw it, the panes before and after did not",
        lab.host.pid
    );
}

#[test]
fn many_shell_runs_and_failed_launches_do_not_leak_host_handles() {
    const CYCLES: usize = 20;
    let lab = Lab::start("leak");
    let cycle = |i: usize| {
        // A program that exits by itself (removed), a failed launch, a shell that is used,
        // resized and killed.
        let (code, doc) = lab.cli(&["new", "--", "cmd.exe", "/d", "/c", "exit 3"]);
        assert_eq!(code, 0, "{doc}");
        let e = lab.new_fails(&["--", r"C:\subak-nope\missing.exe"]);
        assert_eq!(detail(&e, "reason").as_deref(), Some("not_found"));
        let name = format!("c{i}");
        lab.new_terminal(&name, &["--", "cmd.exe", "/d", "/q"]);
        let mut p = lab.attach(&name);
        p.send(format!("echo cyc^le-{i}\r").as_bytes());
        p.expect(&format!("cycle-{i}"));
        let r = p
            .raw
            .request(2, "pane.resize", &json!({"cols": 90, "rows": 20}));
        assert_eq!(r["ok"], true, "{r}");
        drop(p);
        lab.kill(&name);
    };
    cycle(0);
    cycle(1);
    wait_until("the exited Terminals are gone", LONG, || {
        lab.cli(&["ls"]).1["data"]["terminals"] == json!([])
    });
    // Failed launches that are refused before any OS object exists hold nothing.
    let start = lab.handles();
    for _ in 0..CYCLES {
        let e = lab.new_fails(&["--", "definitely-not-a-program-subak"]);
        assert_eq!(detail(&e, "reason").as_deref(), Some("not_found"));
    }
    let before = lab.handles();
    assert!(
        before <= start + 8,
        "{CYCLES} refused launches: Host handles {start} -> {before}"
    );
    for i in 2..CYCLES + 2 {
        cycle(i);
    }
    wait_until("the exited Terminals are gone", LONG, || {
        lab.cli(&["ls"]).1["data"]["terminals"] == json!([])
    });
    let after = lab.handles();
    assert!(lab.conhosts().is_empty());
    // Each cycle starts two pseudo consoles; Windows 10's inbox ConPTY keeps one handle per
    // pseudo console after ClosePseudoConsole (measured in win::conpty's unit tests), so the
    // bound is that plus slack. A leak of our own (pipes, job, process, thread: about 8 per
    // pane) would add hundreds.
    let consoles = 2 * CYCLES as u64;
    assert!(
        after <= before + consoles + 16,
        "Host handles grew from {before} to {after} over {CYCLES} cycles ({consoles} pseudo consoles)"
    );
    eprintln!(
        "EVIDENCE handles: {CYCLES} refused launches: {start} -> {before}; {CYCLES} cycles (exiting \
         program, failed launch, shell used/resized/killed; {consoles} pseudo consoles): \
         {before} -> {after} (min of 5 samples)"
    );
}
