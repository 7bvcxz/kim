//! Step 20 — the Milestone B user journeys and the Alpha resource baseline
//! (docs/MILESTONE_B_VALIDATION.md). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-011`…
//! `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-037`, `SUBAK-FR-038`, `SUBAK-FR-046`,
//! `SUBAK-NFR-001`, `SUBAK-NFR-002`.
//!
//! One journey as a user takes it, timed and measured: the two programs are copied into a fresh
//! folder ("installed"; the real per-user install by `kim install subak` needs a signed release
//! and is not exercised), the window opens with no Host (it starts one, without a shell),
//! Terminals are created, renamed, listed and ended from the window and the CLI, three tabs are
//! switched, the window is closed (a detach) and reopened (the same Terminals and processes),
//! and the idle window and Host are sampled (CPU time, private bytes, working set, threads,
//! handles). Lines starting with `EVIDENCE` are the measurements (`--nocapture`).
//!
//! By default it runs this test build's debug binaries. `SUBAK_MILESTONE_B_BIN=<folder>` runs a
//! **release** build instead (`cargo build --release -p subak --features test-hooks`, the
//! off-screen flag compiled in), the numbers docs/MILESTONE_B_VALIDATION.md reports. The windows
//! are off-screen test windows driven by their own messages; private endpoint, private
//! `APPDATA`; everything started is stopped.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::BufRead;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetProcessHandleCount, GetProcessTimes,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CHAR, WM_CLOSE, WM_COPYDATA,
};

const COPYDATA_TEST: usize = 0x5355_424B;

/// The folder the programs run from: a fresh copy of the debug (or the given release) build.
fn install(sandbox: &Path) -> PathBuf {
    let from = std::env::var_os("SUBAK_MILESTONE_B_BIN").map_or_else(
        || {
            Path::new(env!("CARGO_BIN_EXE_subak"))
                .parent()
                .unwrap()
                .to_path_buf()
        },
        PathBuf::from,
    );
    let to = sandbox.join("Programs").join("subak");
    std::fs::create_dir_all(&to).unwrap();
    for exe in ["subak.exe", "subak-gui.exe"] {
        std::fs::copy(from.join(exe), to.join(exe))
            .unwrap_or_else(|e| panic!("{}: {e}", from.join(exe).display()));
    }
    to
}

fn build_kind() -> &'static str {
    if std::env::var_os("SUBAK_MILESTONE_B_BIN").is_some() {
        "release"
    } else {
        "debug"
    }
}

/// `<installed subak> --json <args>` with the sandbox's APPDATA.
fn cli(dir: &Path, appdata: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(dir.join("subak.exe"))
        .arg("--json")
        .args(args)
        .env("APPDATA", appdata)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)));
    validate("cli-envelope-v1", &doc);
    (out.status.code().unwrap_or(-1), doc)
}

struct Gui {
    child: Child,
    pid: u32,
    hwnd: isize,
    events: mpsc::Receiver<(Instant, Value)>,
    started: Instant,
    /// When it reported `ready` (its window exists and is laid out).
    ready: Instant,
}

impl Gui {
    fn start(dir: &Path, ep: &str, appdata: &Path) -> Self {
        let started = Instant::now();
        let mut child = Command::new(dir.join("subak-gui.exe"))
            .args([
                "--test-offscreen",
                "--test-max-seconds",
                "180",
                "--endpoint",
                ep,
            ])
            .env("APPDATA", appdata)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start subak-gui");
        let (tx, events) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send((Instant::now(), v));
                }
            }
        });
        let pid = child.id();
        let mut g = Self {
            child,
            pid,
            hwnd: 0,
            events,
            started,
            ready: started,
        };
        let (at, ready) = g.wait("ready", |_| true);
        g.hwnd = ready["hwnd"].as_u64().unwrap() as isize;
        g.ready = at;
        g
    }

    fn wait(&self, name: &str, pred: impl Fn(&Value) -> bool) -> (Instant, Value) {
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok((t, v)) if v["event"] == name && pred(&v) => return (t, v),
                Ok(_) => {}
                Err(_) => panic!("subak-gui: no `{name}` event"),
            }
        }
    }

    fn send(&self, c: &Value) {
        let body = c.to_string();
        let cds = COPYDATASTRUCT {
            dwData: COPYDATA_TEST,
            cbData: body.len() as u32,
            lpData: body.as_ptr() as *mut _,
        };
        let mut r = 0usize;
        // SAFETY: a synchronous WM_COPYDATA to the test's own window; `cds` and `body` outlive it.
        let ok = unsafe {
            SendMessageTimeoutW(
                self.hwnd as _,
                WM_COPYDATA,
                0,
                (&raw const cds) as isize,
                SMTO_ABORTIFHUNG,
                10_000,
                &raw mut r,
            )
        };
        assert_ne!(ok, 0);
    }

    fn dump(&self) -> Value {
        self.send(&json!({"cmd": "dump"}));
        self.wait("dump", |_| true).1
    }

    fn until(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let d = self.dump();
            if pred(&d) {
                return d;
            }
            assert!(Instant::now() < until, "{what}: {d:#}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn post(&self, msg: u32, wp: usize, lp: isize) {
        // SAFETY: posting to the test's own off-screen window.
        assert_ne!(unsafe { PostMessageW(self.hwnd as _, msg, wp, lp) }, 0);
    }

    fn close(&mut self) {
        self.post(WM_CLOSE, 0, 0);
        let until = Instant::now() + Duration::from_secs(30);
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "the window did not close");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// CPU time (ms), private bytes, working set and handles of a process.
fn sample(p: &Proc) -> (f64, usize, usize, u32) {
    let ft = |f: FILETIME| {
        ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 10_000.0
    };
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    let mut pm = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let mut handles = 0;
    let h = p.h.as_raw_handle();
    // SAFETY: queries on a process handle we own; out-structures sized correctly.
    unsafe {
        GetProcessTimes(h, &raw mut c, &raw mut e, &raw mut k, &raw mut u);
        GetProcessMemoryInfo(h, (&raw mut pm).cast(), pm.cb);
        GetProcessHandleCount(h, &raw mut handles);
    }
    (ft(k) + ft(u), pm.PrivateUsage, pm.WorkingSetSize, handles)
}

fn threads_of(pid: u32) -> usize {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    // SAFETY: a Toolhelp snapshot, closed below.
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
    unsafe { windows_sys::Win32::Foundation::CloseHandle(snap) };
    n
}

fn mib(b: usize) -> f64 {
    b as f64 / 1_048_576.0
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn screen(d: &Value) -> String {
    d["screen"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn tab_count(d: &Value) -> usize {
    d["tabs"].as_array().map_or(0, Vec::len)
}

#[test]
fn milestone_b_journey_and_resource_baseline() {
    let sandbox = std::env::temp_dir().join(format!(
        "subak-milestone-b-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let appdata = sandbox.join("AppData");
    std::fs::create_dir_all(&appdata).unwrap();
    let dir = install(&sandbox);
    let kind = build_kind();
    let ep = endpoint("milestone-b");
    struct Stop(PathBuf, PathBuf, String);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = cli(
                &self.0,
                &self.1,
                &[
                    "host",
                    "stop",
                    "--endpoint",
                    &self.2,
                    "--kill-terminals",
                    "--yes",
                ],
            );
        }
    }
    let _stop = Stop(dir.clone(), appdata.clone(), ep.clone());
    // 1. Installed programs answer.
    let (code, doc) = cli(&dir, &appdata, &["capabilities"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["tool"], "subak");
    // 2. Open with no Host: the window starts one (no shell) and shows the empty state.
    let (code, doc) = cli(&dir, &appdata, &["status", "--endpoint", &ep]);
    assert_eq!((code, &doc["data"]["running"]), (0, &json!(false)));
    let mut gui = Gui::start(&dir, &ep, &appdata);
    let t_ready = ms(gui.ready.duration_since(gui.started));
    let (t, _) = gui.wait("state", |v| v["phase"] == "empty");
    let t_empty = ms(t.duration_since(gui.started));
    let (_, st) = cli(&dir, &appdata, &["status", "--endpoint", &ep]);
    assert_eq!(st["data"]["running"], true);
    assert_eq!(
        st["data"]["terminals"].as_array().unwrap().len(),
        0,
        "no shell"
    );
    let host_pid = pid_of(&st["data"]["host"]["pid"]);
    // 3. Create (Ctrl+T in the window, `subak new`), rename, list, type.
    let t0 = Instant::now();
    gui.send(&json!({"cmd": "key", "vk": u32::from(b'T'), "ctrl": true}));
    let (t, _) = gui.wait("state", |v| v["phase"] == "attached");
    let t_create = ms(t.duration_since(t0));
    for name in ["build", "logs"] {
        let (code, doc) = cli(&dir, &appdata, &["new", name, "--endpoint", &ep]);
        assert_eq!(code, 0, "{doc}");
    }
    let (code, _) = cli(
        &dir,
        &appdata,
        &["rename", "logs", "로그", "--endpoint", &ep],
    );
    assert_eq!(code, 0);
    let d = gui.until("three tabs", |d| {
        tab_count(d) == 3 && d["tabs"].to_string().contains("로그")
    });
    let first = d["terminal"]["name"].as_str().unwrap().to_string();
    for u in "echo milestone-b-ok\r".encode_utf16() {
        gui.post(WM_CHAR, usize::from(u), 1);
    }
    gui.until("typed output", |d| screen(d).contains("milestone-b-ok"));
    // 4. Multi-tab: Ctrl+Tab three times; each switch attaches the next tab's pane.
    let mut switches = Vec::new();
    for _ in 0..3 {
        let t0 = Instant::now();
        gui.send(&json!({"cmd": "key", "vk": 0x09, "ctrl": true}));
        let (t, _) = gui.wait("state", |v| v["phase"] == "attached");
        switches.push(ms(t.duration_since(t0)));
    }
    let d = gui.until("back on the first tab", |d| {
        d["terminal"]["name"] == first.as_str()
    });
    assert!(
        screen(&d).contains("milestone-b-ok"),
        "the screen came back"
    );
    let (_, ls) = cli(&dir, &appdata, &["ls", "--endpoint", &ep]);
    let pids: Vec<Value> = ls["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["pid"].clone())
        .collect();
    assert_eq!(pids.len(), 3);
    // 5. Idle: the window attached to a quiet Terminal, and the Host with three.
    gui.until("settled", |d| {
        d["motion"]["active"].as_array().is_some_and(Vec::is_empty)
    });
    let wp = Proc::open(gui.pid);
    let hp = Proc::open(host_pid);
    std::thread::sleep(Duration::from_secs(2));
    let secs = if kind == "release" { 30 } else { 10 };
    let (w0, ..) = sample(&wp);
    let (h0, ..) = sample(&hp);
    std::thread::sleep(Duration::from_secs(secs));
    let (w1, wpriv, wws, wh) = sample(&wp);
    let (h1, hpriv, hws, hh) = sample(&hp);
    let (wt, ht) = (threads_of(gui.pid), threads_of(host_pid));
    // 6. Close (a detach) and reopen: the same Terminals and processes, the screen restored.
    let t0 = Instant::now();
    gui.close();
    let t_close = ms(t0.elapsed());
    let (_, ls2) = cli(&dir, &appdata, &["ls", "--endpoint", &ep]);
    let after: Vec<Value> = ls2["data"]["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["pid"].clone())
        .collect();
    assert_eq!(pids, after, "closing the window ended nothing");
    let gui = Gui::start(&dir, &ep, &appdata);
    let (t, _) = gui.wait("state", |v| v["phase"] == "attached");
    let t_reopen = ms(t.duration_since(gui.started));
    let d = gui.until("the earlier output", |d| {
        screen(d).contains("milestone-b-ok")
    });
    assert_eq!(tab_count(&d), 3);
    // 7. End Terminals: `subak kill` (the tab goes) and Ctrl+W on an idle shell.
    let (code, _) = cli(
        &dir,
        &appdata,
        &["kill", "build", "--yes", "--endpoint", &ep],
    );
    assert_eq!(code, 0);
    gui.until("two tabs", |d| tab_count(d) == 2);
    gui.send(&json!({"cmd": "key", "vk": u32::from(b'W'), "ctrl": true}));
    gui.until("one tab", |d| tab_count(d) == 1);
    let exe_kb = |n: &str| std::fs::metadata(dir.join(n)).unwrap().len() / 1024;
    println!(
        "EVIDENCE milestone B ({kind} build, off-screen): window ready {t_ready:.0} ms, Host started and empty state {t_empty:.0} ms, Ctrl+T to an attached shell {t_create:.0} ms, tab switches {:.0}/{:.0}/{:.0} ms, close {t_close:.0} ms, reopen to attached {t_reopen:.0} ms",
        switches[0], switches[1], switches[2]
    );
    println!(
        "EVIDENCE idle {secs} s ({kind}): window CPU {:.1} ms, private {:.2} MiB, working set {:.2} MiB, {wt} threads, {wh} handles; Host (3 Terminals) CPU {:.1} ms, private {:.2} MiB, working set {:.2} MiB, {ht} threads, {hh} handles",
        w1 - w0,
        mib(wpriv),
        mib(wws),
        h1 - h0,
        mib(hpriv),
        mib(hws)
    );
    println!(
        "EVIDENCE binaries ({kind}): subak.exe {} KiB, subak-gui.exe {} KiB",
        exe_kb("subak.exe"),
        exe_kb("subak-gui.exe")
    );
    // The budgets of SUBAK-NFR-002 (idle at 0 CPU: below the timer tick; a few MiB private).
    assert!(w1 - w0 <= 47.0, "window idle CPU {:.1} ms", w1 - w0);
    assert!(h1 - h0 <= 47.0, "Host idle CPU {:.1} ms", h1 - h0);
    assert!(mib(wpriv) < 48.0, "window private {:.1} MiB", mib(wpriv));
    drop(gui);
    let _ = std::fs::remove_dir_all(&sandbox);
}
