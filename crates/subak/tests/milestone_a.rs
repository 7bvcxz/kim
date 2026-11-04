//! Step 14 — Milestone A: persistent sessions and reconnection, validated end to end on real
//! Windows ConPTY (docs/MILESTONE_A_VALIDATION.md). `SUBAK-FR-001`, `SUBAK-FR-002`,
//! `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-013`, `SUBAK-FR-015`,
//! `SUBAK-FR-016`, `KIM-NFR-001`.
//!
//! Every test runs real `subak.exe` processes on its **own** endpoint (options always come
//! before a `--`, so the user's default Host is never touched) and plays a user or an agent:
//! Hosts start on demand from `subak new`, clients are real `subak attach` processes (or raw
//! IPC v1 clients whose control messages are schema-checked), and the programs in the panes are
//! `cmd.exe`, Windows PowerShell 5.1 and two stand-ins built into this test binary:
//!
//! - [`standin_helper`] plays the long-running interactive program (the role Claude Code has in
//!   the manual check): a full-screen program that remembers what it was told and answers every
//!   line with its turn number, its pid and the first thing it heard, so a reattached client can
//!   prove it talks to the same live process with the same state. A real interactive Claude Code
//!   session is never started by a test: it would spend tokens and use the user's account.
//! - [`devserver_helper`] plays a development server: a TCP listener on an ephemeral `127.0.0.1`
//!   port, started by typing its command into a `cmd.exe` pane (a grandchild in that pane's job),
//!   that counts the requests it answers.
//!
//! "No accidental child kill" is checked by handle, never by pid alone: a [`Witness`] holds every
//! process that must survive and asserts after each client-side event that all are alive and
//! that the Host still reports the same pane pids.
//!
//! Headless: pseudo consoles have no window, every helper process runs with `CREATE_NO_WINDOW`,
//! and "a console window closes" is a console created **hidden** (`CREATE_NEW_CONSOLE` +
//! `SW_HIDE`) that receives `WM_CLOSE`, exactly what its close button sends (Step 08's
//! technique). Each test ends what it started and checks that no pane process, pseudo console
//! host or Host survives. Lines starting with `EVIDENCE` record what was measured; they go
//! straight to stderr, so CI logs show them too. `SUBAK_MILESTONE_A_FULL=1` makes the
//! measurement test use the documented sample sizes (60 s idle windows, 100 attaches, 10 Host
//! starts).
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

/// The Host's terminal model, used here as the user's terminal behind a `subak attach`.
use subak_term::screen;

mod common;

use common::*;
use screen::{Limits, Pos, Screen};
use serde_json::{Value, json};
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::io::FromRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, HANDLE_FLAG_INHERIT, HWND, LPARAM, SetHandleInformation,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetProcessTimes, InitializeProcThreadAttributeList,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESHOWWINDOW,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SW_HIDE,
    WM_CLOSE,
};

/// Data frame flags (IPC_PROTOCOL §1).
const SNAPSHOT: u8 = 1;
const INPUT: u8 = 2;

/// A PowerShell's first answer can take long on a cold start or a busy CI runner (Step 11).
const PS_START: Duration = Duration::from_secs(120);

/// Variables for PowerShell panes in tests: no update check, no telemetry.
const PS_ENV: [&str; 4] = [
    "--env",
    "POWERSHELL_UPDATECHECK=Off",
    "--env",
    "POWERSHELL_TELEMETRY_OPTOUT=1",
];

/// The measurement test runs alone in this binary; the other tests share it with each other.
/// (Cargo runs test binaries one after another, so nothing else of this repository runs then.)
static EXCLUSIVE: RwLock<()> = RwLock::new(());

fn shared() -> RwLockReadGuard<'static, ()> {
    EXCLUSIVE.read().unwrap_or_else(PoisonError::into_inner)
}

fn exclusive() -> RwLockWriteGuard<'static, ()> {
    EXCLUSIVE.write().unwrap_or_else(PoisonError::into_inner)
}

fn me() -> PathBuf {
    std::env::current_exe().unwrap()
}

// ---------------------------------------------------------------------------------------------
// The stand-ins (this test binary, started again inside a pane).

/// The long-running interactive program: a full-screen program (alternate screen, a status bar,
/// a prompt) that keeps what it was told in memory and answers every line with
/// `standin-answer-<turn>-pid-<pid>-first-<first line>-said-<line>`. Runs only inside a pane
/// with `SUBAK_TEST_STANDIN` set; `quit` or the end of input ends it.
#[test]
fn standin_helper() {
    if std::env::var_os("SUBAK_TEST_STANDIN").is_none() {
        return;
    }
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    // SAFETY: plain console calls on this process's own standard output handle.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut m = 0;
        GetConsoleMode(h, &raw mut m);
        SetConsoleMode(h, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
    let pid = std::process::id();
    let mut memory: Vec<String> = Vec::new();
    let draw = |memory: &[String]| {
        let mut s = format!(
            "\x1b[2J\x1b[H\x1b[1;36mSTANDIN-pid-{pid}-turns-{}\x1b[0m",
            memory.len()
        );
        let from = memory.len().saturating_sub(12);
        for (i, said) in memory.iter().enumerate().skip(from) {
            s.push_str(&format!(
                "\x1b[{};1Hstandin-answer-{}-pid-{pid}-first-{}-said-{said}",
                3 + i - from,
                i + 1,
                memory[0]
            ));
        }
        s.push_str("\x1b[28;1H\x1b[7mSTANDIN-STATUS-BAR\x1b[0m\x1b[29;1H> ");
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    };
    print!("\x1b[?1049h");
    draw(&memory);
    let mut line = String::new();
    loop {
        line.clear();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let said: String = line
            .trim()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        if said == "quit" {
            break;
        }
        if said.is_empty() {
            continue;
        }
        memory.push(said);
        draw(&memory);
    }
    print!("\x1b[?1049l");
    let _ = std::io::stdout().flush();
}

/// What the stand-in prints for `turn` when its first line was `first` and this one `said`.
fn answer(turn: usize, pid: u32, first: &str, said: &str) -> String {
    format!("standin-answer-{turn}-pid-{pid}-first-{first}-said-{said}")
}

/// `new` arguments that run the stand-in as a Terminal's program.
fn standin_new(name: &str) -> Vec<String> {
    [
        "new",
        name,
        "--size",
        "100x30",
        "--env",
        "SUBAK_TEST_STANDIN=1",
        "--",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain([
        me().display().to_string(),
        "standin_helper".into(),
        "--exact".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ])
    .collect()
}

/// The development server: listens on an ephemeral `127.0.0.1` port, writes the port to the
/// file named by `SUBAK_TEST_DEVSERVER_FILE`, and answers every HTTP request with
/// `hits=<count> pid=<pid>`. Runs only when that variable is set; it never ends by itself.
#[test]
fn devserver_helper() {
    let Some(file) = std::env::var_os("SUBAK_TEST_DEVSERVER_FILE") else {
        return;
    };
    let file = PathBuf::from(file);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, port.to_string()).unwrap();
    std::fs::rename(&tmp, &file).unwrap();
    let pid = std::process::id();
    println!("devserver-listening-{port}-pid-{pid}");
    let mut hits = 0u64;
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
        let mut req = Vec::new();
        let mut buf = [0u8; 1024];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") && req.len() < 8192 {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        }
        hits += 1;
        let body = format!("hits={hits} pid={pid}\n");
        let _ = write!(
            s,
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        println!("devserver-hit-{hits}");
    }
}

/// One request to the development server: `(hits, its pid)`.
fn hit(port: u16) -> std::io::Result<(u64, u32)> {
    let mut s = TcpStream::connect_timeout(
        &SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(5),
    )?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    s.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    let field = |k: &str| -> Option<u64> {
        out.split(k)
            .nth(1)?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    };
    match (field("hits="), field("pid=")) {
        (Some(h), Some(p)) => Ok((h, u32::try_from(p).unwrap())),
        _ => Err(std::io::Error::other(format!(
            "not a dev server answer: {out}"
        ))),
    }
}

/// Starts the development server in a `cmd.exe` pane through `c` (typed, like `npm run dev`);
/// returns its port.
fn start_devserver(c: &mut Client, dir: &Scratch) -> u16 {
    let file = dir.path("port.txt");
    c.send(format!("set SUBAK_TEST_DEVSERVER_FILE={}\r", file.display()).as_bytes());
    c.send(
        format!(
            "\"{}\" devserver_helper --exact --nocapture --test-threads=1\r",
            me().display()
        )
        .as_bytes(),
    );
    c.expect("devserver-listening-");
    wait_until("the dev server wrote its port", LONG, || file.exists());
    std::fs::read_to_string(&file)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Harness.

/// A scratch directory under the system temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!(
            "subak-mila-{tag}-{}-{}",
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
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_until(what: &str, limit: Duration, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + limit;
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
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

/// The console width `mode con` printed last in `text`.
fn columns(text: &str) -> Option<u32> {
    let rest = &text[text.rfind("Columns:")? + "Columns:".len()..];
    rest.trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// One endpoint for one test. The Host behind it starts on demand (`subak new`); on drop it is
/// stopped with its Terminals (or killed), and every process the test tracked must have ended
/// with it: no pane process, grandchild or pseudo console host outlives its Host.
struct Lab {
    ep: String,
    seen: Mutex<Vec<u32>>,
}

/// A Terminal `new` created.
#[derive(Debug, Clone)]
struct Created {
    id: String,
    pid: u32,
    host: u32,
    started: bool,
}

impl Lab {
    fn new(tag: &str) -> Self {
        Self {
            ep: endpoint(&format!("mila-{tag}")),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// `subak --json --endpoint <ours> <args>`, validated against `subak-cli-v1`.
    fn cli(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--endpoint", self.ep.as_str()];
        a.extend_from_slice(args);
        let (code, doc) = run(&a);
        validate("subak-cli-v1", &doc);
        (code, doc)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (code, doc) = self.cli(args);
        assert_eq!(code, 0, "{args:?}: {doc}");
        doc["data"].clone()
    }

    fn track(&self, pid: u32) {
        self.seen.lock().unwrap().push(pid);
    }

    fn new_terminal(&self, name: &str, args: &[&str]) -> Created {
        let mut a = vec!["new", name];
        a.extend_from_slice(args);
        let d = self.ok(&a);
        let c = Created {
            id: d["terminal_id"].as_str().unwrap().to_string(),
            pid: pid_of(&d["terminal"]["windows"][0]["panes"][0]["pid"]),
            host: pid_of(&d["host"]["pid"]),
            started: d["host"]["started"] == true,
        };
        self.track(c.pid);
        c
    }

    fn new_standin(&self, name: &str) -> Created {
        let args = standin_new(name);
        let a: Vec<&str> = args[2..].iter().map(String::as_str).collect();
        self.new_terminal(name, &a)
    }

    fn host_pid(&self) -> Option<u32> {
        let d = self.ok(&["status"]);
        d["host"]["pid"].as_u64().map(|p| u32::try_from(p).unwrap())
    }

    fn terminal(&self, target: &str) -> Value {
        self.ok(&["info", target])["terminal"].clone()
    }

    fn host_status(&self) -> Value {
        self.ok(&["host", "status"])
    }

    fn attached(&self) -> u64 {
        self.host_status()["clients"]["attached"].as_u64().unwrap()
    }

    fn stats(&self) -> Value {
        let r = Raw::handshake(&self.ep, "cli").request(1, "host.stats", &json!({}));
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    }

    /// The minimum of several samples of a Host counter: filters out what the Host holds only
    /// briefly (the connection serving the sample, a thread being joined).
    fn min_sample(&self, f: impl Fn(&Value) -> u64) -> u64 {
        (0..5)
            .map(|i| {
                if i > 0 {
                    std::thread::sleep(Duration::from_millis(150));
                }
                f(&self.stats())
            })
            .min()
            .unwrap()
    }

    fn handles(&self) -> u64 {
        self.min_sample(|s| s["host"]["handle_count"].as_u64().unwrap())
    }

    fn private_bytes(&self) -> u64 {
        self.min_sample(|s| s["host"]["memory"]["private_bytes"].as_u64().unwrap())
    }

    /// Waits until the Host has no connection but the one asking.
    fn wait_no_clients(&self) {
        wait_until("every client connection to close", LONG, || {
            self.host_status()["clients"]["connected"] == 1
        });
    }

    /// The Host's own text capture of a pane: every history line and the screen.
    fn capture(&self, target: &str) -> Vec<String> {
        let r = Raw::handshake(&self.ep, "cli").request(
            1,
            "pane.capture",
            &json!({"target": target, "start": "-", "end": "-", "redact": false}),
        );
        assert_eq!(r["ok"], true, "{r}");
        ipc_def("result_pane_capture", &r["result"]);
        assert_eq!(r["result"]["truncated"], false);
        r["result"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect()
    }

    fn output_bytes(&self, target: &str) -> u64 {
        self.terminal(target)["windows"][0]["panes"][0]["output_bytes"]
            .as_u64()
            .unwrap()
    }

    /// Waits until a pane has written nothing for 600 ms.
    fn quiet(&self, target: &str) {
        let until = Instant::now() + Duration::from_secs(60);
        let mut last = self.output_bytes(target);
        loop {
            std::thread::sleep(Duration::from_millis(600));
            let now = self.output_bytes(target);
            if now == last {
                return;
            }
            assert!(Instant::now() < until, "pane `{target}` never went quiet");
            last = now;
        }
    }

    /// Stops the Host with its Terminals and checks that everything tracked ended.
    fn shutdown(&self) -> Vec<u32> {
        let host = self.host_pid().and_then(Proc::try_open);
        let consoles: Vec<Proc> = host.as_ref().map_or_else(Vec::new, |h| {
            children(h.pid, "conhost.exe")
                .into_iter()
                .filter_map(Proc::try_open)
                .collect()
        });
        if let Some(h) = &host {
            let _ = self.cli(&["host", "stop", "--kill-terminals", "--yes"]);
            if !h.wait(Duration::from_secs(20)) {
                h.kill();
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
        survivors
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let survivors = self.shutdown();
        if !std::thread::panicking() {
            assert!(
                survivors.is_empty(),
                "processes outlived the test Host: {survivors:?}"
            );
        }
    }
}

/// Every process that must survive the client-side events of a test, held by handle (a pid alone
/// could be reused), and the Terminals whose pane pid the Host must keep reporting.
struct Witness {
    host: Proc,
    procs: Vec<(String, Proc)>,
    terminals: Vec<(String, u32)>,
    events: Vec<String>,
}

impl Witness {
    fn new(host: u32) -> Self {
        Self {
            host: Proc::open(host),
            procs: Vec::new(),
            terminals: Vec::new(),
            events: Vec::new(),
        }
    }

    fn process(&mut self, lab: &Lab, what: &str, pid: u32) {
        lab.track(pid);
        self.procs.push((what.to_string(), Proc::open(pid)));
    }

    fn terminal(&mut self, lab: &Lab, name: &str, pid: u32) {
        self.process(lab, &format!("the program of Terminal `{name}`"), pid);
        self.terminals.push((name.to_string(), pid));
    }

    /// Asserts that every witnessed process is the same live process, and that the Host lists
    /// every witnessed Terminal as running with the same pane pid.
    fn check(&mut self, lab: &Lab, event: &str) {
        assert!(
            self.host.alive(),
            "the Host (pid {}) died after: {event}",
            self.host.pid
        );
        for (what, p) in &self.procs {
            assert!(p.alive(), "{what} (pid {}) died after: {event}", p.pid);
        }
        let ls = lab.ok(&["ls"]);
        assert_eq!(
            pid_of(&ls["host"]["pid"]),
            self.host.pid,
            "the same Host after: {event}"
        );
        for (name, pid) in &self.terminals {
            let t = ls["terminals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name.as_str())
                .unwrap_or_else(|| panic!("Terminal `{name}` is gone after: {event}"));
            assert_eq!(t["state"], "running", "`{name}` after: {event}");
            assert_eq!(pid_of(&t["pid"]), *pid, "`{name}` pid after: {event}");
        }
        self.events.push(event.to_string());
    }

    fn pids(&self) -> String {
        std::iter::once(format!("host={}", self.host.pid))
            .chain(self.procs.iter().map(|(w, p)| format!("{w}={}", p.pid)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Where a reader thread collects what a child process writes.
type Sink = Arc<Mutex<Vec<u8>>>;

/// `subak attach <target>` as a user's terminal runs it, with piped stdin, stdout and stderr and
/// no window (redirected input: its bytes go to the pane, the end of input detaches).
struct Client {
    child: Child,
    out: Sink,
    err: Sink,
    mark: usize,
}

impl Client {
    fn attach(ep: &str, target: &str, extra: &[&str]) -> Self {
        let c = Self::spawn(ep, target, extra, true);
        c.wait_notice("Ctrl+B d detaches");
        c
    }

    fn spawn(ep: &str, target: &str, extra: &[&str], read_stdout: bool) -> Self {
        let mut args = vec!["--endpoint", ep, "attach", target, "--size", "100x30"];
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
        let err = Arc::new(Mutex::new(Vec::new()));
        let stdout = child.stdout.take().unwrap();
        let mut streams: Vec<(Box<dyn Read + Send>, Sink)> =
            vec![(Box::new(child.stderr.take().unwrap()), Arc::clone(&err))];
        if read_stdout {
            streams.push((Box::new(stdout), Arc::clone(&out)));
        } else {
            drop(stdout); // the reader of this client's output goes away
        }
        for (mut stream, sink) in streams {
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
        Self {
            child,
            out,
            err,
            mark: 0,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    /// Everything this client wrote to its stdout: the pane's bytes as a terminal gets them.
    fn bytes(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }

    fn text(&self) -> String {
        visible(&self.out.lock().unwrap()[self.mark..])
    }

    fn notices(&self) -> String {
        String::from_utf8_lossy(&self.err.lock().unwrap()).into_owned()
    }

    fn wait_notice(&self, needle: &str) {
        let until = Instant::now() + LONG;
        while !self.notices().contains(needle) {
            assert!(
                Instant::now() < until,
                "`{needle}` not on the stderr of client {}:\n{}",
                self.pid(),
                self.notices()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn expect(&mut self, needle: &str) -> String {
        self.expect_within(needle, LONG)
    }

    /// Waits until `needle` appears in the output after the last mark; moves the mark past it.
    fn expect_within(&mut self, needle: &str, limit: Duration) -> String {
        let until = Instant::now() + limit;
        loop {
            let text = self.text();
            if text.contains(needle) {
                self.mark = self.out.lock().unwrap().len();
                return text;
            }
            assert!(
                Instant::now() < until,
                "`{needle}` not seen by client {}:\n{text}\nstderr: {}",
                self.pid(),
                self.notices()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn exits(&mut self, limit: Duration) -> Option<i32> {
        let end = Instant::now() + limit;
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code();
            }
            if Instant::now() > end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// `Ctrl+B d`; returns the exit code.
    fn detach_keys(&mut self) -> Option<i32> {
        self.send(&[0x02, b'd']);
        self.exits(LONG)
    }

    /// Closes stdin (the end of piped input detaches); returns the exit code.
    fn end_input(&mut self) -> Option<i32> {
        drop(self.child.stdin.take());
        self.exits(LONG)
    }

    /// `TerminateProcess`: no detach, no goodbye.
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

/// A raw IPC v1 client attached to a pane (kind `gui`); every control message it receives is
/// schema-checked by [`Raw`].
struct Watch {
    raw: Raw,
    stream: u32,
    seen: Vec<u8>,
    events: Vec<Value>,
}

impl Watch {
    fn attach(ep: &str, target: &str, interactive: bool) -> Self {
        let raw = Raw::handshake(ep, "gui");
        // Subscribed to `panes`, as a GUI is: it learns of `pane.detached` and `pane.resync`.
        let r = raw.request(1, "events.subscribe", &json!({"topics": ["panes"]}));
        assert_eq!(r["ok"], true, "{r}");
        let r = raw.request(
            2,
            "pane.attach",
            &json!({"target": target, "interactive": interactive}),
        );
        assert_eq!(r["ok"], true, "{r}");
        ipc_def("result_pane_attach", &r["result"]);
        Self {
            stream: u32::try_from(r["result"]["stream_id"].as_u64().unwrap()).unwrap(),
            raw,
            seen: Vec::new(),
            events: Vec::new(),
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.raw.write(&data(self.stream, 0, INPUT, bytes)).unwrap();
    }

    fn text(&self) -> String {
        visible(&self.seen)
    }

    /// Reads what arrives within `wait`; false once the Host closed the connection.
    fn pump(&mut self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            match self.raw.next(left) {
                Got::Data(b) => self.seen.extend(b),
                Got::Control(m) => self.events.push(m),
                Got::Timeout => return true,
                Got::Eof => return false,
            }
        }
    }

    fn expect(&mut self, needle: &str, limit: Duration) -> String {
        let until = Instant::now() + limit;
        loop {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            assert!(Instant::now() < until, "`{needle}` not seen:\n{text}");
            assert!(
                self.pump(Duration::from_millis(100)),
                "the Host closed the connection while waiting for `{needle}`"
            );
        }
    }
}

fn pipe_pair() -> (HANDLE, HANDLE) {
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: writable out-pointers; the ends are not inheritable by default.
    let ok = unsafe { CreatePipe(&raw mut r, &raw mut w, std::ptr::null(), 0) };
    assert_ne!(ok, 0);
    (r, w)
}

/// A program in a **new console whose window is created hidden** (never visible on the
/// desktop), with piped stdin and stdout: a user's terminal window, closed with `WM_CLOSE`.
struct Term {
    proc: Proc,
    stdin: Option<File>,
    out: Arc<Mutex<Vec<u8>>>,
    mark: usize,
}

impl Term {
    fn spawn(cmdline: &str) -> Self {
        let (in_r, in_w) = pipe_pair();
        let (out_r, out_w) = pipe_pair();
        // SAFETY: valid handles; only these two become inheritable, and the attribute list
        // below restricts inheritance to exactly them.
        unsafe {
            SetHandleInformation(in_r, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
            SetHandleInformation(out_w, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
        let handles = [in_r, out_w];
        let mut size = 0usize;
        // SAFETY: size query.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size) };
        let mut attr = vec![0u64; size.div_ceil(8)];
        // SAFETY: a buffer of `size` bytes; `handles` outlives CreateProcessW.
        unsafe {
            assert_ne!(
                InitializeProcThreadAttributeList(attr.as_mut_ptr().cast(), 1, 0, &raw mut size),
                0
            );
            assert_ne!(
                UpdateProcThreadAttribute(
                    attr.as_mut_ptr().cast(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast::<c_void>(),
                    size_of_val(&handles),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ),
                0
            );
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
        si.StartupInfo.wShowWindow = u16::try_from(SW_HIDE).unwrap();
        si.StartupInfo.hStdInput = in_r;
        si.StartupInfo.hStdOutput = out_w;
        si.StartupInfo.hStdError = out_w;
        si.lpAttributeList = attr.as_mut_ptr().cast();
        let mut cmd = wide(cmdline);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: a writable command line, live startup info and attribute list.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_NEW_CONSOLE,
                std::ptr::null(),
                std::ptr::null(),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        // SAFETY: the child's ends are ours to close; the attribute list is done.
        unsafe {
            DeleteProcThreadAttributeList(attr.as_mut_ptr().cast());
            CloseHandle(in_r);
            CloseHandle(out_w);
        }
        assert_ne!(
            ok,
            0,
            "CreateProcessW({cmdline}): {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: fresh handles from CreatePipe/CreateProcessW, owned from here on.
        let (stdin, mut stdout, process) = unsafe {
            CloseHandle(pi.hThread);
            (
                File::from_raw_handle(in_w),
                File::from_raw_handle(out_r),
                std::os::windows::io::OwnedHandle::from_raw_handle(pi.hProcess),
            )
        };
        let out = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let out = Arc::clone(&out);
            move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = stdout.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    out.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        Self {
            proc: Proc {
                pid: pi.dwProcessId,
                h: process,
            },
            stdin: Some(stdin),
            out,
            mark: 0,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let w = self.stdin.as_mut().expect("stdin open");
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    fn expect(&mut self, needle: &str) -> String {
        let until = Instant::now() + LONG;
        loop {
            let text = visible(&self.out.lock().unwrap()[self.mark..]);
            if text.contains(needle) {
                self.mark = self.out.lock().unwrap().len();
                return text;
            }
            assert!(
                Instant::now() < until,
                "`{needle}` not seen from pid {}:\n{text}",
                self.proc.pid
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// The console host of this program's hidden console and its window.
    fn console(&self) -> (Proc, HWND) {
        let mut found = Vec::new();
        wait_until("the console host", LONG, || {
            found = children(self.proc.pid, "conhost.exe");
            !found.is_empty()
        });
        let conhost = Proc::open(found[0]);
        let mut hwnd = None;
        wait_until("the console window", LONG, || {
            hwnd = console_window(&[self.proc.pid, conhost.pid]);
            hwnd.is_some()
        });
        let hwnd = hwnd.unwrap();
        // SAFETY: a valid window handle from the enumeration.
        let shown = unsafe { IsWindowVisible(hwnd) };
        assert_eq!(shown, 0, "the console window stays hidden");
        (conhost, hwnd)
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if self.proc.alive() {
            self.proc.kill();
        }
    }
}

/// The console window hosted for any of `pids` (console windows belong to conhost).
fn console_window(pids: &[u32]) -> Option<HWND> {
    struct Find<'a> {
        pids: &'a [u32],
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> i32 {
        // SAFETY: `lp` is the &mut Find passed to EnumWindows below, alive for the call.
        let f = unsafe { &mut *(lp as *mut Find<'_>) };
        let mut pid = 0u32;
        let mut class = [0u16; 64];
        // SAFETY: a valid window from the enumeration; writable buffers.
        let n = unsafe {
            GetWindowThreadProcessId(hwnd, &raw mut pid);
            GetClassNameW(hwnd, class.as_mut_ptr(), 64)
        };
        let class = String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)]);
        if f.pids.contains(&pid) && class == "ConsoleWindowClass" {
            f.found = Some(hwnd);
            return 0;
        }
        1
    }
    let mut f = Find { pids, found: None };
    // SAFETY: the callback only dereferences `f`, which outlives the call.
    unsafe { EnumWindows(Some(each), (&raw mut f) as LPARAM) };
    f.found
}

fn threads_of(pid: u32) -> usize {
    // SAFETY: Toolhelp snapshot, closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let mut e = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
        ..Default::default()
    };
    let mut n = 0;
    // SAFETY: `e.dwSize` is set.
    let mut more = unsafe { Thread32First(snap, &raw mut e) } != 0;
    while more {
        if e.th32OwnerProcessID == pid {
            n += 1;
        }
        // SAFETY: same snapshot and entry.
        more = unsafe { Thread32Next(snap, &raw mut e) } != 0;
    }
    // SAFETY: a valid snapshot handle.
    unsafe { CloseHandle(snap) };
    n
}

/// User plus kernel CPU time of a process, in milliseconds.
fn cpu_ms(p: &Proc) -> u64 {
    let mut t = [FILETIME::default(); 4];
    // SAFETY: a valid process handle with query access; four out-parameters of the right type.
    let ok = unsafe {
        GetProcessTimes(
            std::os::windows::io::AsRawHandle::as_raw_handle(&p.h),
            &raw mut t[0],
            &raw mut t[1],
            &raw mut t[2],
            &raw mut t[3],
        )
    };
    assert_ne!(ok, 0);
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    (ft(t[2]) + ft(t[3])) / 10_000
}

/// Waits for PowerShell's prompt, then sends an idempotent command until its output appears
/// (PowerShell may discard keys typed while it starts or stops a pipeline).
fn ps_command(c: &mut Client, line: &[u8], needle: &str) -> String {
    let until = Instant::now() + PS_START;
    while !c.text().trim_end().ends_with('>') {
        assert!(
            Instant::now() < until,
            "no PowerShell prompt:\n{}",
            c.text()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    for attempt in 0..5 {
        c.send(line);
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            let text = c.text();
            if text.contains(needle) {
                c.mark = c.out.lock().unwrap().len();
                return text;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        eprintln!("PowerShell did not answer `{needle}` (attempt {attempt}); typing again");
    }
    panic!("`{needle}` not seen after 5 attempts:\n{}", c.text());
}

/// The id `subak detach --client` takes for the client process `pid` (`host status` lists it).
fn client_id(lab: &Lab, pid: u32) -> String {
    let st = lab.host_status();
    st["clients"]["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["pid"] == pid)
        .unwrap_or_else(|| panic!("client {pid} is not listed: {st}"))["id"]
        .to_string()
}

/// Prints an `EVIDENCE` line straight to stderr. The test harness captures only `print!` and
/// `eprint!` output, so these lines also reach the log of a passing CI run (CI runs `cargo test`
/// without `--nocapture`), which is where the CI numbers of docs/MILESTONE_A_VALIDATION.md come
/// from.
fn evidence(line: &str) {
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{line}");
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// p50, p95 and max of samples, in milliseconds.
fn percentiles(samples: &mut [Duration]) -> (f64, f64, f64) {
    samples.sort();
    let at = |q: f64| {
        let i = ((samples.len() as f64 - 1.0) * q).round() as usize;
        ms(samples[i])
    };
    (at(0.5), at(0.95), ms(*samples.last().unwrap()))
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

// ---------------------------------------------------------------------------------------------
// 1. The Milestone A journey.

/// Acceptance (issue #14, automated part): a detached Host keeps a PowerShell, a `cmd.exe` with a
/// development server and a long-running interactive program running through every client-side
/// event — the window that launched the Host closing, clients killed, detached in every way,
/// their console host killed, their output reader vanishing — and a new client reattaches to the
/// same live processes with their state.
#[test]
fn a_detached_host_keeps_shells_a_dev_server_and_an_interactive_program_through_every_client_event()
{
    let _g = shared();
    let lab = Lab::new("journey");
    let ep = lab.ep.clone();
    let dir = Scratch::new("journey");

    // 1. The user's terminal window (a console, hidden here) starts the Host on demand with
    //    `subak new agent -- <program>` and attaches to the program.
    let mut term = Term::spawn("cmd.exe /d /q /k");
    let standin = standin_new("agent").join(" ").replace(
        &me().display().to_string(),
        &format!("\"{}\"", me().display()),
    );
    term.send(format!("\"{}\" --endpoint {ep} --json {standin}\r\n", subak()).as_bytes());
    term.expect("\"ok\":true");
    let agent = lab.terminal("agent");
    let agent_pid = pid_of(&agent["windows"][0]["panes"][0]["pid"]);
    let host_pid = lab.host_pid().expect("the Host started on demand");
    lab.track(agent_pid);
    assert_ne!(
        parent_of(host_pid),
        Some(std::process::id()),
        "the Host was started from the terminal window, not by the test"
    );
    let mut w = Witness::new(host_pid);
    w.terminal(&lab, "agent", agent_pid);
    term.send(
        format!(
            "\"{}\" --endpoint {ep} attach agent --size 100x30\r\n",
            subak()
        )
        .as_bytes(),
    );
    term.expect("Ctrl+B d detaches");
    term.send(b"remember-kiwi\r");
    term.expect(&answer(1, agent_pid, "remember-kiwi", "remember-kiwi"));
    let in_window = children(term.proc.pid, "subak.exe");
    assert_eq!(in_window.len(), 1, "the attach client runs in the window");
    let window_client = Proc::open(in_window[0]);
    let (conhost, hwnd) = term.console();
    evidence(&format!(
        "EVIDENCE journey: window shell={} attach-client={} console-host={}; host={host_pid} \
         (started on demand from the window), agent={agent_pid}",
        term.proc.pid, window_client.pid, conhost.pid
    ));
    // The user closes the window (its close button sends WM_CLOSE): every process attached to
    // that console gets CTRL_CLOSE_EVENT and ends.
    // SAFETY: posting a message to a window; no pointers involved.
    assert_ne!(unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) }, 0);
    assert!(
        window_client.wait(LONG),
        "the attach client ended with its window"
    );
    assert!(term.proc.wait(LONG), "the window's shell ended");
    assert!(conhost.wait(LONG), "the window's console host ended");
    w.check(
        &lab,
        "the window that started the Host and ran the attach closed (WM_CLOSE)",
    );
    wait_until("the closed window's client to be dropped", LONG, || {
        lab.attached() == 0
    });

    // 2. More Terminals from the CLI: a cmd.exe and a Windows PowerShell.
    let cmd = lab.new_terminal("cmd", &["--size", "100x30", "--", "cmd.exe", "/d"]);
    let mut ps_args = vec!["--size", "100x30"];
    ps_args.extend(PS_ENV);
    ps_args.extend(["--", "powershell", "-NoLogo", "-NoProfile"]);
    let ps = lab.new_terminal("ps", &ps_args);
    assert!(!cmd.started && !ps.started, "one Host for every Terminal");
    assert_eq!((cmd.host, ps.host), (host_pid, host_pid));
    w.terminal(&lab, "cmd", cmd.pid);
    w.terminal(&lab, "ps", ps.pid);

    // 3. A development server typed into the cmd.exe pane: a grandchild in its job.
    let mut c = Client::attach(&ep, "cmd", &[]);
    c.expect(">");
    let port = start_devserver(&mut c, &dir);
    let (hits, dev_pid) = hit(port).unwrap();
    assert_eq!(hits, 1);
    assert_eq!(
        parent_of(dev_pid),
        Some(cmd.pid),
        "a child of the pane's shell"
    );
    w.process(&lab, "the dev server (grandchild of `cmd`)", dev_pid);
    c.expect("devserver-hit-1");
    c.crash();
    w.check(
        &lab,
        "a client attached to `cmd` was killed (TerminateProcess)",
    );
    assert_eq!(
        hit(port).unwrap(),
        (2, dev_pid),
        "it kept serving with no client"
    );

    // 4. PowerShell keeps a variable; the client detaches by the end of its input.
    let mut p = Client::attach(&ep, "ps", &[]);
    ps_command(
        &mut p,
        b"$kim = 'ps-state-' + (6*7); 'ps-rea' + 'dy'\r",
        "ps-ready",
    );
    assert_eq!(p.end_input(), Some(0), "{}", p.notices());
    w.check(&lab, "a client of `ps` detached by the end of its input");

    // 5. The interactive program: Ctrl+B d.
    let mut a = Client::attach(&ep, "agent", &[]);
    a.expect(&answer(1, agent_pid, "remember-kiwi", "remember-kiwi"));
    a.send(b"turn-two\r");
    a.expect(&answer(2, agent_pid, "remember-kiwi", "turn-two"));
    assert_eq!(a.detach_keys(), Some(0), "{}", a.notices());
    assert!(a.notices().contains("detached; the shell keeps running"));
    w.check(&lab, "a client of `agent` detached with Ctrl+B d");

    // 6. A client in a hidden console whose console host is killed.
    let mut k = Term::spawn(&format!(
        "\"{}\" --endpoint {ep} attach agent --size 100x30",
        subak()
    ));
    k.expect("STANDIN-STATUS-BAR");
    let (k_conhost, _) = k.console();
    k_conhost.kill();
    let ended_by_itself = k.proc.wait(Duration::from_secs(5));
    k.proc.kill();
    w.check(&lab, "the console host of a client of `agent` was killed");

    // 7. A read-only client whose output reader is gone: it detaches at its first write.
    let mut r = Client::spawn(&ep, "cmd", &["--read-only"], false);
    assert_eq!(r.exits(LONG), Some(0), "{}", r.notices());
    assert!(r.notices().contains("read-only"), "{}", r.notices());
    w.check(&lab, "a read-only client of `cmd` lost its output reader");

    // 8. `subak detach agent` from elsewhere while a client is attached.
    let mut d = Client::attach(&ep, "agent", &[]);
    let detached = lab.ok(&["detach", "agent"]);
    assert_eq!(
        detached["detached"].as_array().unwrap().len(),
        1,
        "{detached}"
    );
    assert_eq!(d.exits(LONG), Some(0), "{}", d.notices());
    assert!(d.notices().contains("by another client"), "{}", d.notices());
    w.check(&lab, "`subak detach agent` from another terminal");

    // 9. Several clients killed at the same moment.
    let mut many = vec![
        Client::attach(&ep, "agent", &[]),
        Client::attach(&ep, "cmd", &["--read-only"]),
        Client::attach(&ep, "ps", &["--read-only"]),
    ];
    wait_until("three clients attached", LONG, || lab.attached() == 3);
    for m in &mut many {
        let _ = m.child.kill();
    }
    for m in &mut many {
        let _ = m.child.wait();
    }
    wait_until("the killed clients to be dropped", LONG, || {
        lab.attached() == 0
    });
    w.check(&lab, "three clients of three Terminals killed at once");

    // 10. Nobody attached: the dev server still serves and counts.
    assert_eq!(hit(port).unwrap(), (3, dev_pid));

    // 11. Reattach: the same live processes, with their state.
    let mut a = Client::attach(&ep, "agent", &[]);
    let screen = a.expect(&answer(2, agent_pid, "remember-kiwi", "turn-two"));
    assert!(
        screen.contains("STANDIN-STATUS-BAR"),
        "the program's screen is back"
    );
    a.send(b"after-everything\r");
    a.expect(&answer(3, agent_pid, "remember-kiwi", "after-everything"));
    assert_eq!(a.detach_keys(), Some(0));
    let mut p = Client::attach(&ep, "ps", &[]);
    ps_command(&mut p, b"'ps-' + 'got-' + $kim\r", "ps-got-ps-state-42");
    assert_eq!(p.detach_keys(), Some(0));
    w.check(&lab, "reattached to `agent` and `ps`");
    let ls = lab.ok(&["ls"]);
    assert_eq!(ls["terminals"].as_array().unwrap().len(), 3);
    evidence(&format!(
        "EVIDENCE journey: survived {} events with the same pids ({}); console-host-killed client \
         ended by itself: {ended_by_itself}; agent answered turn 3 remembering turn 1; \
         PowerShell kept $kim; dev server on 127.0.0.1:{port} answered 3 requests",
        w.events.len(),
        w.pids()
    ));

    // 12. An explicit Host stop ends everything; nothing listens any more.
    let survivors = lab.shutdown();
    assert!(survivors.is_empty(), "{survivors:?}");
    for (what, p) in &w.procs {
        assert!(!p.alive(), "{what} outlived `host stop`");
    }
    assert!(hit(port).is_err(), "the dev server's port is closed");
}

// ---------------------------------------------------------------------------------------------
// 2. Concurrency.

/// Several Terminals with several clients each, all at the same time: every client sees its own
/// Terminal's output and no other; two clients typing into one program see one consistent state;
/// ending the clients of one Terminal in different ways touches no other client or process.
#[test]
fn concurrent_sessions_and_clients_stay_independent_and_consistent() {
    let _g = shared();
    let lab = Lab::new("conc");
    let ep = lab.ep.clone();

    // Four `new` at once: one Host, four Terminals.
    let created: Vec<(String, Created)> = std::thread::scope(|s| {
        let jobs: Vec<_> = ["c1", "c2", "s1", "s2"]
            .iter()
            .map(|name| {
                let lab = &lab;
                s.spawn(move || {
                    let c = if name.starts_with('c') {
                        lab.new_terminal(name, &["--size", "100x30", "--", "cmd.exe", "/d"])
                    } else {
                        lab.new_standin(name)
                    };
                    ((*name).to_string(), c)
                })
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap()).collect()
    });
    let host = created[0].1.host;
    assert!(created.iter().all(|(_, c)| c.host == host), "one Host");
    assert_eq!(
        created.iter().filter(|(_, c)| c.started).count(),
        1,
        "exactly one `new` started it"
    );
    let mut w = Witness::new(host);
    for (name, c) in &created {
        w.terminal(&lab, name, c.pid);
    }
    let pid = |n: &str| created.iter().find(|(m, _)| m == n).unwrap().1.pid;

    // Per Terminal: two interactive CLI clients, one read-only CLI client, one raw viewer.
    struct Group {
        name: &'static str,
        a: Client,
        b: Client,
        ro: Client,
        view: Watch,
    }
    let mut groups: Vec<Group> = ["c1", "c2", "s1", "s2"]
        .iter()
        .map(|name| Group {
            name,
            a: Client::attach(&ep, name, &[]),
            b: Client::attach(&ep, name, &[]),
            ro: Client::attach(&ep, name, &["--read-only"]),
            view: Watch::attach(&ep, name, false),
        })
        .collect();
    wait_until("16 clients attached", LONG, || lab.attached() == 16);

    // Everyone types at the same time.
    std::thread::scope(|s| {
        for g in &mut groups {
            s.spawn(move || {
                let n = g.name;
                if n.starts_with('c') {
                    g.a.send(format!("echo {n}-from-a-mark^er\r").as_bytes());
                    g.b.send(format!("echo {n}-from-b-mark^er\r").as_bytes());
                    for c in [&mut g.a, &mut g.b, &mut g.ro] {
                        c.expect(&format!("{n}-from-a-marker"));
                    }
                    for c in [&mut g.a, &mut g.b, &mut g.ro] {
                        c.mark = 0;
                        c.expect(&format!("{n}-from-b-marker"));
                    }
                    g.view.expect(&format!("{n}-from-a-marker"), LONG);
                    g.view.expect(&format!("{n}-from-b-marker"), LONG);
                } else {
                    // Two writers, one program: ten lines in turn reach one counter.
                    for i in 1..=10 {
                        let c = if i % 2 == 1 { &mut g.a } else { &mut g.b };
                        c.send(format!("{n}-line-{i}\r").as_bytes());
                        c.expect(&format!("standin-answer-{i}-"));
                    }
                    for c in [&mut g.a, &mut g.b, &mut g.ro] {
                        c.mark = 0;
                        c.expect("standin-answer-10-pid-");
                    }
                    g.view.expect("standin-answer-10-pid-", LONG);
                }
            });
        }
    });
    // No client saw another Terminal's output.
    for g in &groups {
        for other in ["c1", "c2", "s1", "s2"].iter().filter(|o| **o != g.name) {
            let foreign = format!("{other}-from-");
            let foreign_line = format!("{other}-line-");
            for (who, text) in [
                ("a", visible(&g.a.bytes())),
                ("b", visible(&g.b.bytes())),
                ("read-only", visible(&g.ro.bytes())),
                ("viewer", g.view.text()),
            ] {
                assert!(
                    !text.contains(&foreign) && !text.contains(&foreign_line),
                    "client {who} of `{}` saw output of `{other}`",
                    g.name
                );
            }
        }
    }
    let s1_answer = answer(10, pid("s1"), "s1-line-1", "s1-line-10");
    assert!(
        visible(&groups[2].view.seen).contains(&s1_answer),
        "one counter, one memory for both writers"
    );
    w.check(
        &lab,
        "16 clients of 4 Terminals typed and read at the same time",
    );

    // End the clients of each Terminal differently, at the same time.
    let mut g = groups.into_iter();
    let (mut c1, mut c2, mut s1, mut s2) = (
        g.next().unwrap(),
        g.next().unwrap(),
        g.next().unwrap(),
        g.next().unwrap(),
    );
    std::thread::scope(|s| {
        s.spawn(|| {
            for c in [&mut c1.a, &mut c1.b, &mut c1.ro] {
                let _ = c.child.kill();
            }
        });
        s.spawn(|| {
            assert_eq!(c2.a.detach_keys(), Some(0));
            assert_eq!(c2.b.end_input(), Some(0));
        });
        s.spawn(|| {
            let d = lab.ok(&["detach", "s1"]);
            assert_eq!(d["detached"].as_array().unwrap().len(), 4, "{d}");
        });
    });
    drop(c1);
    for c in [&mut s1.a, &mut s1.b, &mut s1.ro] {
        assert_eq!(c.exits(LONG), Some(0), "{}", c.notices());
    }
    // The raw viewer of s1 learns it was detached; its connection stays open.
    let until = Instant::now() + LONG;
    while !s1.view.events.iter().any(|e| e["event"] == "pane.detached") {
        assert!(
            Instant::now() < until,
            "no pane.detached: {:?}",
            s1.view.events
        );
        assert!(s1.view.pump(Duration::from_millis(100)));
    }
    // s2's clients never noticed.
    s2.a.send(b"s2-after\r");
    for c in [&mut s2.a, &mut s2.b, &mut s2.ro] {
        c.expect(&answer(11, pid("s2"), "s2-line-1", "s2-after"));
    }
    s2.view.expect("s2-after", LONG);
    assert_eq!(
        lab.attached(),
        6,
        "s2's four clients and c2's read-only client and viewer remain"
    );
    w.check(
        &lab,
        "c1's clients killed, c2's detached (Ctrl+B d, end of input), s1's detached from outside, \
         all at once",
    );
    // State stayed per Terminal.
    let mut again = Client::attach(&ep, "s1", &[]);
    again.send(b"s1-again\r");
    again.expect(&answer(11, pid("s1"), "s1-line-1", "s1-again"));
    drop((again, c2, s1, s2));
    lab.wait_no_clients();
    w.check(&lab, "every client gone");
    evidence(&format!(
        "EVIDENCE concurrent: 4 Terminals created at once by 4 `new` -> one Host ({host}); 16 \
         clients (2 interactive, 1 read-only CLI, 1 raw viewer per Terminal) saw only their own \
         Terminal; two writers drove one program to turn 10; killing, detaching and detaching \
         from outside at once left {} processes alive ({})",
        w.procs.len(),
        w.pids()
    ));
}

// ---------------------------------------------------------------------------------------------
// 3. Repeated attach and detach.

/// 60 attach/detach cycles in six ways (Ctrl+B d, end of input, TerminateProcess, `subak
/// detach`, a raw connection dropped without detaching, a client of another Terminal killed)
/// keep the program and its state, and leave no handles, threads or memory behind.
#[test]
fn sixty_attach_detach_cycles_keep_the_program_and_leak_nothing() {
    let _g = shared();
    let lab = Lab::new("cycles");
    let ep = lab.ep.clone();
    let st = lab.new_standin("st");
    let sh = lab.new_terminal("sh", &["--size", "100x30", "--", "cmd.exe", "/d", "/q"]);
    let mut w = Witness::new(st.host);
    w.terminal(&lab, "st", st.pid);
    w.terminal(&lab, "sh", sh.pid);
    let mut turn = 0usize;
    let cycle = |i: usize, turn: &mut usize| {
        let said = format!("cycle-{i}");
        let method = i % 6;
        if method == 4 {
            let mut v = Watch::attach(&ep, "st", true);
            *turn += 1;
            v.send(format!("{said}\r").as_bytes());
            v.expect(&answer(*turn, st.pid, "cycle-0", &said), LONG);
            return; // the connection is dropped without a detach
        }
        if method == 5 {
            let mut c = Client::attach(&ep, "sh", &[]);
            c.send(format!("echo {said}-mark^er\r").as_bytes());
            c.expect(&format!("{said}-marker"));
            c.crash();
            return;
        }
        let mut c = Client::attach(&ep, "st", &[]);
        *turn += 1;
        c.send(format!("{said}\r").as_bytes());
        c.expect(&answer(*turn, st.pid, "cycle-0", &said));
        match method {
            0 => assert_eq!(c.detach_keys(), Some(0), "{}", c.notices()),
            1 => assert_eq!(c.end_input(), Some(0), "{}", c.notices()),
            2 => c.crash(),
            _ => {
                lab.ok(&["detach", "st"]);
                assert_eq!(c.exits(LONG), Some(0), "{}", c.notices());
            }
        }
    };
    for i in 0..6 {
        cycle(i, &mut turn);
    }
    lab.wait_no_clients();
    let pid = st.host;
    let (h0, m0, t0) = (lab.handles(), lab.private_bytes(), threads_of(pid));
    let started = Instant::now();
    for i in 6..66 {
        cycle(i, &mut turn);
        if i % 10 == 0 {
            w.check(&lab, &format!("attach/detach cycle {i}"));
        }
    }
    let took = started.elapsed();
    lab.wait_no_clients();
    wait_until("the Host's threads to settle", LONG, || {
        threads_of(pid) <= t0
    });
    let (h1, m1, t1) = (lab.handles(), lab.private_bytes(), threads_of(pid));
    w.check(&lab, "60 attach/detach cycles");
    // The program kept every turn: one process, 60 + 6 lines minus the cmd cycles.
    let mut c = Client::attach(&ep, "st", &[]);
    c.send(b"final\r");
    c.expect(&answer(turn + 1, st.pid, "cycle-0", "final"));
    drop(c);
    let grew = i64::try_from(m1).unwrap() - i64::try_from(m0).unwrap();
    evidence(&format!(
        "EVIDENCE cycles: 60 cycles in {:.1} s ({:.0} ms each); Host handles {h0} -> {h1}, threads \
         {t0} -> {t1}, private bytes {:.2} -> {:.2} MiB ({:+.1} KiB, min of 5 samples each); the \
         stand-in answered all {} turns as pid {}",
        took.as_secs_f64(),
        ms(took) / 60.0,
        mib(m0),
        mib(m1),
        grew as f64 / 1024.0,
        turn + 1,
        st.pid
    ));
    // Leak detectors, not performance budgets: one handle or one snapshot leaked per cycle would
    // add at least 60 handles or several MiB.
    assert!(h1 <= h0 + 20, "Host handles grew from {h0} to {h1}");
    assert!(t1 <= t0, "Host threads grew from {t0} to {t1}");
    assert!(
        grew < 4 << 20,
        "Host private bytes grew by {grew} bytes over 60 cycles"
    );
}

// ---------------------------------------------------------------------------------------------
// 4. Scrollback after reattach.

/// After output with clients detached, a client killed in the middle of a burst and a reattach
/// through the real `subak attach`, the reattached terminal holds exactly the Host's history and
/// screen: every line once, in order (Step 13 through the CLI client). A full-screen program's
/// screen comes back the same way.
#[test]
fn scrollback_after_reattach_is_complete_ordered_and_never_repeated() {
    let _g = shared();
    let lab = Lab::new("scroll");
    let ep = lab.ep.clone();
    let sb = lab.new_terminal("sb", &["--size", "100x30", "--", "cmd.exe", "/d", "/q"]);
    let st = lab.new_standin("st");
    let mut w = Witness::new(sb.host);
    w.terminal(&lab, "sb", sb.pid);
    w.terminal(&lab, "st", st.pid);
    let mut a = Client::attach(&ep, "sb", &[]);
    a.expect(">");
    a.send(b"for /L %i in (1,1,300) do @echo sb-line-%i\r");
    a.expect("sb-line-300");
    assert_eq!(a.detach_keys(), Some(0));
    // Output while nobody is attached through the CLI (a raw client types it, then leaves).
    let mut v = Watch::attach(&ep, "sb", true);
    v.send(b"for /L %i in (301,1,500) do @echo sb-line-%i\r");
    v.expect("sb-line-500", LONG);
    drop(v);
    // A client killed in the middle of a burst.
    let mut k = Client::attach(&ep, "sb", &[]);
    k.send(b"for /L %i in (501,1,1500) do @echo sb-line-%i\r");
    k.expect("sb-line-700");
    k.crash();
    w.check(&lab, "a client was killed during a 1000-line burst");
    let until = Instant::now() + Duration::from_secs(60);
    while !lab.capture("sb").iter().any(|l| l == "sb-line-1500") {
        assert!(Instant::now() < until, "the burst never finished");
        std::thread::sleep(Duration::from_millis(200));
    }
    lab.quiet("sb");

    // Reattach through the CLI; its stdout feeds a terminal model, as the user's terminal would.
    let mut b = Client::attach(&ep, "sb", &[]);
    b.send(b"echo after-reattach-mark^er\r");
    b.expect("after-reattach-marker");
    lab.quiet("sb");
    std::thread::sleep(Duration::from_millis(300));
    let mut term = Screen::new(
        100,
        30,
        Limits {
            lines: 100_000,
            bytes: usize::MAX / 4,
        },
    );
    term.feed(&b.bytes());
    let mine = term.capture(Pos::Edge, Pos::Edge, false, false).lines;
    let host = lab.capture("sb");
    assert_eq!(
        mine, host,
        "the reattached terminal == the Host's history and screen"
    );
    let numbers: Vec<u32> = mine
        .iter()
        .filter_map(|l| l.strip_prefix("sb-line-")?.parse().ok())
        .collect();
    assert_eq!(
        numbers,
        (1..=1500).collect::<Vec<_>>(),
        "every line once and in order"
    );
    let history = lab.ok(&["history", "sb"])["history"].clone();
    assert_eq!(history["evicted_lines"], 0, "{history}");
    assert_eq!(b.detach_keys(), Some(0));

    // A full-screen program: the screen it drew before its client was killed comes back.
    let mut c = Client::attach(&ep, "st", &[]);
    for i in 1..=3 {
        c.send(format!("tui-{i}\r").as_bytes());
        c.expect(&answer(i, st.pid, "tui-1", &format!("tui-{i}")));
    }
    c.crash();
    lab.quiet("st");
    let mut d = Client::attach(&ep, "st", &[]);
    d.expect("STANDIN-STATUS-BAR");
    lab.quiet("st");
    std::thread::sleep(Duration::from_millis(300));
    let mut term = Screen::new(
        100,
        30,
        Limits {
            lines: 100_000,
            bytes: usize::MAX / 4,
        },
    );
    term.feed(&d.bytes());
    let screen = term.capture(Pos::Line(0), Pos::Edge, false, false).lines;
    let host = lab.capture("st");
    assert_eq!(
        screen,
        host[host.len() - 30..],
        "the reattached screen == the Host's screen"
    );
    let last = answer(3, st.pid, "tui-1", "tui-3");
    assert!(screen.iter().any(|l| l.contains(&last)), "{screen:#?}");
    assert!(screen.iter().any(|l| l.starts_with("STANDIN-STATUS-BAR")));
    drop(d);
    w.check(
        &lab,
        "reattached after scrollback and a full-screen program",
    );
    evidence(&format!(
        "EVIDENCE scrollback: 1500 lines (300 attached, 200 with no CLI client, 1000 while a \
         client was killed mid-burst): the reattached `subak attach` output, fed to a terminal \
         model, equals the Host's {} history+screen lines with sb-line-1..1500 once each and in \
         order; the stand-in's full-screen view came back after a killed client",
        mine.len()
    ));
}

// ---------------------------------------------------------------------------------------------
// 5. Output bursts and resize storms.

/// A burst of output while clients come and go, one never reads, and the pane is resized 120
/// times: nothing blocks, nothing dies, another Terminal answers promptly, and the last size
/// reaches the shell.
#[test]
fn output_bursts_and_resize_storms_never_block_or_end_anything() {
    let _g = shared();
    let lab = Lab::new("burst");
    let ep = lab.ep.clone();
    let burst = lab.new_terminal("burst", &["--size", "120x30", "--", "cmd.exe", "/d", "/q"]);
    let st = lab.new_standin("st");
    let mut w = Witness::new(burst.host);
    w.terminal(&lab, "burst", burst.pid);
    w.terminal(&lab, "st", st.pid);
    let mut typer = Watch::attach(&ep, "burst", true);
    typer.expect(">", LONG);
    let stalled = Watch::attach(&ep, "burst", false); // never reads
    let mut answers = Watch::attach(&ep, "st", true);
    answers.expect("STANDIN-STATUS-BAR", LONG);
    let base = lab.stats();
    typer
        .send(b"for /L %i in (1,1,20000) do @echo burst-%i-0123456789abcdefghijklmnopqrstuvwxyz\r");
    typer.expect("burst-200-", LONG);
    let (mut health_worst, mut answer_worst) = (Duration::ZERO, Duration::ZERO);
    let resizes = std::thread::scope(|s| {
        // 120 resizes from an interactive client of its own, during the burst.
        let resizer = s.spawn(|| {
            let r = Watch::attach(&ep, "burst", true);
            let n = 120u64;
            for i in 0..n {
                let (c, rows) = (40 + (i * 7) % 160, 10 + (i * 3) % 50);
                r.raw.send(
                    &json!({"type": "request", "id": 100 + i, "method": "pane.resize",
                                   "params": {"cols": c, "rows": rows}}),
                );
                std::thread::sleep(Duration::from_millis(15));
            }
            r.raw.send(
                &json!({"type": "request", "id": 100 + n, "method": "pane.resize",
                               "params": {"cols": 97, "rows": 33}}),
            );
            let mut answered = 0;
            let deadline = Instant::now() + Duration::from_secs(60);
            while answered <= n {
                assert!(Instant::now() < deadline, "{answered} resizes answered");
                match r.raw.next(Duration::from_millis(500)) {
                    Got::Control(m) if m["type"] == "response" => {
                        assert_eq!(m["ok"], true, "{m}");
                        answered += 1;
                    }
                    Got::Eof => panic!("the Host closed the resizing connection"),
                    _ => {}
                }
            }
            answered
        });
        // CLI clients come and go during the burst; every other one is killed.
        let churn = s.spawn(|| {
            for i in 0..10 {
                let mut c = Client::attach(&ep, "burst", &["--read-only"]);
                std::thread::sleep(Duration::from_millis(150));
                if i % 2 == 0 {
                    c.crash();
                } else {
                    let id = client_id(&lab, c.pid());
                    let d = lab.ok(&["detach", "--client", &id]);
                    assert_eq!(d["detached"].as_array().unwrap().len(), 1, "{d}");
                    assert_eq!(c.exits(LONG), Some(0), "{}", c.notices());
                }
            }
        });
        // The Host and another Terminal stay responsive.
        for i in 0..8 {
            let t = Instant::now();
            let h = Raw::handshake(&ep, "cli").request(1, "host.health", &json!({}));
            assert_eq!(h["result"]["healthy"], true, "{h}");
            health_worst = health_worst.max(t.elapsed());
            let t = Instant::now();
            answers.send(format!("during-{i}\r").as_bytes());
            answers.expect(&format!("said-during-{i}"), LONG);
            answer_worst = answer_worst.max(t.elapsed());
            std::thread::sleep(Duration::from_millis(250));
        }
        churn.join().unwrap();
        resizer.join().unwrap()
    });
    typer.expect("burst-20000-", Duration::from_secs(180));
    typer.send(b"echo burst-done-mark^er\r");
    typer.expect("burst-done-marker", LONG);
    lab.quiet("burst");
    w.check(
        &lab,
        "a 20000-line burst with 121 resizes and 10 clients coming and going",
    );
    // The last size reached the shell; the shell answers.
    typer.seen.clear();
    typer.send(b"mode con & echo cols-mark^er\r");
    let text = typer.expect("cols-marker", LONG);
    assert_eq!(columns(&text), Some(97), "{text}");
    let size = &lab.terminal("burst")["windows"][0]["panes"][0]["size"];
    assert_eq!(*size, json!({"cols": 97, "rows": 33}));
    let s = lab.stats();
    assert_eq!(s["ipc"]["panics"], 0, "{s}");
    assert_eq!(s["ipc"]["lagging_clients_dropped"], 0, "{s}");
    // The client that never read is still connected and can read now.
    drop(stalled);
    let out = s["pane"]["output_bytes_total"].as_u64().unwrap()
        - base["pane"]["output_bytes_total"].as_u64().unwrap();
    evidence(&format!(
        "EVIDENCE burst: {:.1} MiB of output with {resizes} resizes answered, 10 CLI clients \
         attached and killed or detached, a client that never read kept (resyncs {}, skipped \
         frames {}); worst host.health {:.0} ms, worst answer from another Terminal {:.0} ms; \
         final 97x33 reached the shell",
        mib(out),
        s["ipc"]["lagging_resyncs"],
        s["ipc"]["output_frames_skipped"],
        ms(health_worst),
        ms(answer_worst)
    ));
}

// ---------------------------------------------------------------------------------------------
// 6. Host failure boundaries.

/// The Host is the boundary: killed, it ends every pane process (kill-on-close jobs; no orphan
/// keeps a port open) and its clients report `host_disconnected`; the next `new` starts a fresh
/// Host at once on the same endpoint with no stale Terminal; a hung Host is reported as
/// `host_unresponsive` and neither killed nor replaced.
#[test]
fn host_failure_ends_its_panes_as_documented_and_leaves_no_stale_endpoint() {
    let _g = shared();
    let lab = Lab::new("fail");
    let ep = lab.ep.clone();
    let dir = Scratch::new("fail");
    let cmd = lab.new_terminal("cmd", &["--size", "100x30", "--", "cmd.exe", "/d"]);
    let agent = lab.new_standin("agent");
    let host = Proc::open(cmd.host);
    let mut c = Client::attach(&ep, "cmd", &[]);
    c.expect(">");
    let port = start_devserver(&mut c, &dir);
    let (_, dev_pid) = hit(port).unwrap();
    lab.track(dev_pid);
    let mut a = Client::attach(&ep, "agent", &[]);
    a.send(b"before-crash\r");
    a.expect(&answer(1, agent.pid, "before-crash", "before-crash"));
    let watcher = Raw::handshake(&ep, "gui");
    assert_eq!(
        watcher.request(1, "events.subscribe", &json!({"topics": ["host", "panes"]}))["ok"],
        true
    );
    let consoles: Vec<Proc> = children(host.pid, "conhost.exe")
        .into_iter()
        .map(Proc::open)
        .collect();
    assert_eq!(consoles.len(), 2, "one pseudo console host per pane");
    let tree: Vec<Proc> = [cmd.pid, agent.pid, dev_pid]
        .into_iter()
        .map(Proc::open)
        .collect();

    // The Host crashes.
    let t = Instant::now();
    host.kill();
    for p in tree.iter().chain(&consoles) {
        assert!(p.wait(LONG), "pid {} outlived its killed Host", p.pid);
    }
    let tree_gone = t.elapsed();
    assert_eq!(
        watcher.next(LONG),
        Got::Eof,
        "the raw client sees the Host go"
    );
    for cl in [&mut c, &mut a] {
        assert_eq!(cl.exits(LONG), Some(6), "{}", cl.notices());
        assert!(
            cl.notices().contains("the Host closed the connection"),
            "{}",
            cl.notices()
        );
    }
    assert!(hit(port).is_err(), "no orphan keeps the dev server's port");
    let ls = lab.ok(&["ls"]);
    assert_eq!(
        (ls["host"].clone(), ls["terminals"].clone()),
        (Value::Null, json!([]))
    );
    let (code, doc) = lab.cli(&["info", "agent"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("host_not_running"))
    );

    // A new Host on the same endpoint: at once, fresh, no stale Terminal or id.
    let t = Instant::now();
    let fresh = lab.new_standin("agent");
    let restart = t.elapsed();
    assert!(
        fresh.started && fresh.host != host.pid,
        "a new Host was started"
    );
    assert_ne!(fresh.id, agent.id, "a new Terminal id");
    let (code, doc) = lab.cli(&["info", &agent.id]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );
    let ls = lab.ok(&["ls"]);
    assert_eq!(ls["terminals"].as_array().unwrap().len(), 1, "{ls}");
    let mut a = Client::attach(&ep, "agent", &[]);
    a.send(b"fresh-start\r");
    a.expect(&answer(1, fresh.pid, "fresh-start", "fresh-start"));
    drop(a);

    // A hung Host (every thread suspended) is reported, never killed or replaced.
    let new_host = Proc::open(fresh.host);
    let program = Proc::open(fresh.pid);
    let frozen = new_host.suspend();
    let t = Instant::now();
    let (code, doc) = lab.cli(&["ls"]);
    let hung_after = t.elapsed();
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("host_unresponsive")),
        "{doc}"
    );
    let (code, doc) = lab.cli(&["new", "second"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("host_unresponsive")),
        "{doc}"
    );
    assert!(new_host.alive() && program.alive(), "nothing was killed");
    drop(frozen);
    let ls = lab.ok(&["ls"]);
    assert_eq!(
        pid_of(&ls["host"]["pid"]),
        fresh.host,
        "the same Host after it resumed"
    );
    assert_eq!(
        ls["terminals"].as_array().unwrap().len(),
        1,
        "no second Host, no extra Terminal"
    );
    let mut a = Client::attach(&ep, "agent", &[]);
    a.send(b"after-resume\r");
    a.expect(&answer(2, fresh.pid, "fresh-start", "after-resume"));
    drop(a);
    evidence(&format!(
        "EVIDENCE host-failure: host={} killed -> cmd={} agent={} dev-server={} and 2 conhosts \
         ended within {:.0} ms, 2 CLI clients exited 6 (host_disconnected), port {port} closed; \
         new host={} on the same endpoint in {:.0} ms with only the new Terminal; suspended host \
         -> host_unresponsive after {:.0} ms, not killed or replaced, intact after resume",
        host.pid,
        cmd.pid,
        agent.pid,
        dev_pid,
        ms(tree_gone),
        fresh.host,
        ms(restart),
        ms(hung_after)
    ));
}

// ---------------------------------------------------------------------------------------------
// 7. Measurements.

/// The OS build string (`ver`), for the record.
fn os_version() -> String {
    let out = Command::new("cmd.exe")
        .args(["/d", "/c", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Baseline numbers of the Host on this machine (docs/MILESTONE_A_VALIDATION.md §5): Host start
/// latency (explicit and on demand), the Host's memory with 0, 1 and 4 Terminals, idle CPU with
/// no client and with attached clients, and attach latency through IPC and through the CLI.
/// The only requirement with a budget here is `KIM-NFR-001` (idle CPU < 0.1 % of one core);
/// everything else is reported, not judged.
#[test]
fn milestone_a_baseline_measurements() {
    let _g = exclusive();
    let full = std::env::var_os("SUBAK_MILESTONE_A_FULL").is_some();
    let (starts, attaches, idle_s, cli_attaches) = if full {
        (10, 100, 60, 20)
    } else {
        (5, 30, 15, 10)
    };
    evidence(&format!(
        "EVIDENCE measure-env: {}; {} logical processors ({}); {} build of subak; samples: {} \
         starts, {} attaches, {} s idle windows",
        os_version(),
        std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
        std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        starts,
        attaches,
        idle_s
    ));

    // Host start: `host start --no-shell` and `new` with the Host started on demand.
    let mut start = Vec::new();
    let mut health = Vec::new();
    let mut empty_private = Vec::new();
    let mut empty_ws = Vec::new();
    for _ in 0..starts {
        let lab = Lab::new("m-start");
        let t = Instant::now();
        lab.ok(&["host", "start", "--no-shell", "--idle-exit", "60"]);
        start.push(t.elapsed());
        let t = Instant::now();
        lab.ok(&["host", "health"]);
        health.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(300));
        let s = lab.stats();
        empty_private.push(s["host"]["memory"]["private_bytes"].as_u64().unwrap());
        empty_ws.push(s["host"]["memory"]["working_set_bytes"].as_u64().unwrap());
    }
    let mut on_demand = Vec::new();
    for _ in 0..starts {
        let lab = Lab::new("m-new");
        let t = Instant::now();
        let c = lab.new_terminal("m", &["--", "cmd.exe", "/d"]);
        on_demand.push(t.elapsed());
        assert!(c.started);
    }
    let (s50, s95, smax) = percentiles(&mut start);
    let (h50, _, hmax) = percentiles(&mut health);
    let (n50, n95, nmax) = percentiles(&mut on_demand);
    empty_private.sort_unstable();
    empty_ws.sort_unstable();
    evidence(&format!(
        "EVIDENCE measure-start: `host start` p50 {s50:.0} ms, p95 {s95:.0} ms, max {smax:.0} ms; \
         first `host health` p50 {h50:.0} ms, max {hmax:.0} ms; `new` with a Host on demand \
         p50 {n50:.0} ms, p95 {n95:.0} ms, max {nmax:.0} ms; empty Host private {:.2} MiB, \
         working set {:.2} MiB (median)",
        mib(empty_private[empty_private.len() / 2]),
        mib(empty_ws[empty_ws.len() / 2])
    ));

    // Memory with Terminals, then idle CPU.
    let lab = Lab::new("m-mem");
    let ep = lab.ep.clone();
    let first = lab.new_terminal("cmd1", &["--size", "120x30", "--", "cmd.exe", "/d", "/q"]);
    let host = Proc::open(first.host);
    std::thread::sleep(Duration::from_millis(500));
    let one = lab.stats()["host"]["memory"].clone();
    lab.new_terminal("cmd2", &["--size", "120x30", "--", "cmd.exe", "/d", "/q"]);
    let mut ps_args = vec!["--size", "120x30"];
    ps_args.extend(PS_ENV);
    ps_args.extend(["--", "powershell", "-NoLogo", "-NoProfile"]);
    lab.new_terminal("ps", &ps_args);
    lab.new_standin("st");
    std::thread::sleep(Duration::from_millis(500));
    let four = lab.stats()["host"]["memory"].clone();
    // Fill both cmd panes' history to the default limit (2 000 rows).
    for name in ["cmd1", "cmd2"] {
        let mut v = Watch::attach(&ep, name, true);
        v.expect(">", LONG);
        v.send(
            format!(
                "for /L %i in (1,1,2100) do @echo {name}-history-line-%i-0123456789abcdefghij\r"
            )
            .as_bytes(),
        );
        v.expect(
            &format!("{name}-history-line-2100-"),
            Duration::from_secs(120),
        );
    }
    let mut p = Client::attach(&ep, "ps", &[]);
    ps_command(&mut p, b"'ps-' + 'idle-ready'\r", "ps-idle-ready");
    drop(p);
    lab.quiet("cmd1");
    lab.quiet("cmd2");
    lab.wait_no_clients();
    // Threads of the connections that just closed may still be exiting (CI saw 14 -> 13).
    let mut threads0 = threads_of(host.pid);
    let until = Instant::now() + LONG;
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let now = threads_of(host.pid);
        if now == threads0 || Instant::now() > until {
            break;
        }
        threads0 = now;
    }
    let full_mem = lab.stats();
    let cpu0 = cpu_ms(&host);
    std::thread::sleep(Duration::from_secs(idle_s));
    let idle_cpu = cpu_ms(&host) - cpu0;
    let threads1 = threads_of(host.pid);
    // Idle with two attached clients (one interactive, one read-only).
    let watch_a = Watch::attach(&ep, "cmd1", true);
    let watch_b = Watch::attach(&ep, "st", false);
    std::thread::sleep(Duration::from_secs(1));
    let cpu0 = cpu_ms(&host);
    std::thread::sleep(Duration::from_secs(idle_s));
    let idle_cpu_attached = cpu_ms(&host) - cpu0;
    drop((watch_a, watch_b));
    let pct = |cpu: u64| cpu as f64 / (idle_s as f64 * 1000.0) * 100.0;
    evidence(&format!(
        "EVIDENCE measure-memory: Host private/working set: 1 Terminal {:.2}/{:.2} MiB; 4 \
         Terminals (2 cmd, PowerShell, stand-in) {:.2}/{:.2} MiB; with 2 x 2000 history rows \
         {:.2}/{:.2} MiB (scrollback {:.0} KiB in the model); peak private {:.2} MiB",
        mib(one["private_bytes"].as_u64().unwrap()),
        mib(one["working_set_bytes"].as_u64().unwrap()),
        mib(four["private_bytes"].as_u64().unwrap()),
        mib(four["working_set_bytes"].as_u64().unwrap()),
        mib(full_mem["host"]["memory"]["private_bytes"]
            .as_u64()
            .unwrap()),
        mib(full_mem["host"]["memory"]["working_set_bytes"]
            .as_u64()
            .unwrap()),
        full_mem["scrollback"]["memory_bytes"].as_u64().unwrap() as f64 / 1024.0,
        mib(full_mem["host"]["memory"]["peak_private_bytes"]
            .as_u64()
            .unwrap())
    ));
    evidence(&format!(
        "EVIDENCE measure-idle: 4 idle Terminals, no client: Host CPU {idle_cpu} ms over {idle_s} \
         s ({:.3} % of one core), threads {threads0} -> {threads1}; with 2 attached clients: \
         {idle_cpu_attached} ms ({:.3} %)",
        pct(idle_cpu),
        pct(idle_cpu_attached)
    ));
    // KIM-NFR-001: a long-running process with no activity averages < 0.1 % of one core.
    assert!(
        pct(idle_cpu) < 0.1,
        "idle CPU {idle_cpu} ms over {idle_s} s"
    );
    assert!(
        pct(idle_cpu_attached) < 0.1,
        "idle CPU with clients {idle_cpu_attached} ms over {idle_s} s"
    );
    assert!(
        threads1 <= threads0,
        "no thread appeared while idle: {threads0} -> {threads1}"
    );

    // Attach latency: `pane.attach` sent -> the whole snapshot received, on a pane with 2 000
    // history rows and on the full-screen stand-in.
    let lat = |target: &str| {
        let mut samples = Vec::new();
        let mut bytes = 0;
        for _ in 0..attaches {
            let raw = Raw::handshake(&ep, "gui");
            let t = Instant::now();
            let r = raw.request(
                1,
                "pane.attach",
                &json!({"target": target, "interactive": false}),
            );
            assert_eq!(r["ok"], true, "{r}");
            let want = r["result"]["snapshot"]["bytes"].as_u64().unwrap();
            let mut got = 0;
            while got < want {
                match raw.next(LONG) {
                    Got::Data(b) => {
                        assert_eq!(raw.last_data.get().unwrap().2, SNAPSHOT);
                        got += b.len() as u64;
                    }
                    other => panic!("expected the snapshot, got {other:?}"),
                }
            }
            samples.push(t.elapsed());
            bytes = want;
        }
        let (p50, p95, max) = percentiles(&mut samples);
        (p50, p95, max, bytes)
    };
    let hist = lat("cmd1");
    let tui = lat("st");
    // The CLI: process start -> attached and the screen shown.
    let mut cli = Vec::new();
    for _ in 0..cli_attaches {
        let t = Instant::now();
        let mut c = Client::attach(&ep, "st", &[]);
        c.expect("STANDIN-STATUS-BAR");
        cli.push(t.elapsed());
        assert_eq!(c.detach_keys(), Some(0));
    }
    let (c50, c95, cmax) = percentiles(&mut cli);
    evidence(&format!(
        "EVIDENCE measure-attach: pane.attach -> snapshot received: pane with 2000 history rows \
         ({} bytes) p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms; full-screen stand-in ({} bytes) p50 \
         {:.1} ms, p95 {:.1} ms, max {:.1} ms; `subak attach` process start -> screen shown p50 \
         {c50:.0} ms, p95 {c95:.0} ms, max {cmax:.0} ms",
        hist.3, hist.0, hist.1, hist.2, tui.3, tui.0, tui.1, tui.2
    ));
}
