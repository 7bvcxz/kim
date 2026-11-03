//! Step 08 — the detached Host and its ConPTY shell on real Windows (docs/CONPTY_PROOF.md,
//! ADR 0016). `SUBAK-FR-001`, `SUBAK-FR-003`, `SUBAK-FR-015`, `SUBAK-FR-016`.
//!
//! Every test runs real `subak.exe` processes against its **own** pipe endpoint, so a user's Host
//! is never touched, and every test ends what it started: Hosts are stopped (or killed), clients
//! killed, jobs closed — then the test checks that no Host, shell or console host survived.
//!
//! Headless: no window is ever shown. "A console window closes" is reproduced with a console
//! created hidden (`CREATE_NEW_CONSOLE` + `SW_HIDE`) that receives `WM_CLOSE`, which is what
//! clicking its close button does. Lines starting with `EVIDENCE` record process ids; run with
//! `--nocapture` to see them.
#![cfg(windows)]
#![allow(unsafe_code)]

use serde_json::Value;
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HWND, LPARAM, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
    GetProcessHandleCount, InitializeProcThreadAttributeList, OpenProcess,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread,
    STARTF_USESHOWWINDOW, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, SW_HIDE,
    WM_CLOSE,
};

const LONG: Duration = Duration::from_secs(20);

fn subak() -> &'static str {
    env!("CARGO_BIN_EXE_subak")
}

fn endpoint(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("subak-test-{tag}-{}-{nanos}", std::process::id())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `subak <args> --json` without a window; returns (exit code, envelope).
fn run(args: &[&str]) -> (i32, Value) {
    let out = Command::new(subak())
        .args(args)
        .arg("--json")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "`subak {}`: not one JSON document ({e}): {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let code = out.status.code().unwrap_or(-1);
    if doc["ok"] == false {
        assert_eq!(
            Some(i64::from(code)),
            doc["error"]["exit_code"].as_i64(),
            "{doc}"
        );
    }
    (code, doc)
}

fn status(ep: &str) -> Value {
    let (code, doc) = run(&["host", "status", "--endpoint", ep]);
    assert_eq!(code, 0, "{doc}");
    doc["data"].clone()
}

fn pid_of(v: &Value) -> u32 {
    u32::try_from(v.as_u64().unwrap_or_else(|| panic!("not a pid: {v}"))).unwrap()
}

/// Strips CSI/OSC escape sequences.
fn visible(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && it.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A process we can watch and end, opened by pid (or adopted from CreateProcess).
#[derive(Debug)]
struct Proc {
    pid: u32,
    h: OwnedHandle,
}

impl Proc {
    fn open(pid: u32) -> Self {
        // SAFETY: plain open; null is checked.
        let h = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(!h.is_null(), "process {pid} is not running");
        // SAFETY: a fresh handle that only this value owns from here on.
        let h = unsafe { OwnedHandle::from_raw_handle(h) };
        Self { pid, h }
    }

    fn raw(&self) -> HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.h.as_raw_handle()
    }

    fn wait(&self, t: Duration) -> bool {
        // SAFETY: valid process handle.
        unsafe {
            WaitForSingleObject(self.raw(), u32::try_from(t.as_millis()).unwrap()) == WAIT_OBJECT_0
        }
    }

    fn alive(&self) -> bool {
        !self.wait(Duration::ZERO)
    }

    fn exit_code(&self) -> Option<u32> {
        let mut c = 0u32;
        // SAFETY: valid handle; `c` is writable.
        (!self.alive() && unsafe { GetExitCodeProcess(self.raw(), &raw mut c) } != 0).then_some(c)
    }

    fn kill(&self) {
        // SAFETY: valid handle with terminate access.
        unsafe { TerminateProcess(self.raw(), 1) };
        self.wait(Duration::from_secs(10));
    }

    fn handle_count(&self) -> u32 {
        let mut n = 0u32;
        // SAFETY: valid handle with query access.
        unsafe { GetProcessHandleCount(self.raw(), &raw mut n) };
        n
    }
}

/// (pid, parent pid, exe name) of every process.
fn processes() -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp snapshot, closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let mut e = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap(),
        ..Default::default()
    };
    // SAFETY: `e.dwSize` is set as required.
    let mut ok = unsafe { Process32FirstW(snap, &raw mut e) } != 0;
    while ok {
        let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
        out.push((
            e.th32ProcessID,
            e.th32ParentProcessID,
            String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase(),
        ));
        // SAFETY: same snapshot and entry.
        ok = unsafe { Process32NextW(snap, &raw mut e) } != 0;
    }
    // SAFETY: valid snapshot handle.
    unsafe { CloseHandle(snap) };
    out
}

fn children(parent: u32, name: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, pp, n)| *pp == parent && n == name)
        .map(|(p, _, _)| p)
        .collect()
}

/// A job object owned by the test.
struct Job(HANDLE);

impl Job {
    fn new(breakaway_ok: bool) -> Self {
        // SAFETY: anonymous job; checked.
        let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!h.is_null());
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | if breakaway_ok {
                JOB_OBJECT_LIMIT_BREAKAWAY_OK
            } else {
                0
            };
        // SAFETY: valid struct of the size passed.
        let ok = unsafe {
            SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            )
        };
        assert_ne!(ok, 0);
        Self(h)
    }

    /// Puts a (suspended) process into the job.
    fn assign(&self, p: &Proc) {
        // SAFETY: valid job and process handles; the process has not run yet.
        let ok = unsafe { AssignProcessToJobObject(self.0, p.raw()) };
        assert_ne!(ok, 0, "AssignProcessToJobObject");
    }

    fn contains(&self, p: &Proc) -> bool {
        let mut r = 0;
        // SAFETY: valid handles; `r` writable.
        unsafe { IsProcessInJob(p.raw(), self.0, &raw mut r) };
        r != 0
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: owned handle; kill-on-close ends every process still in the job.
        unsafe { CloseHandle(self.0) };
    }
}

/// Whether this test process sits in a job that forbids breakaway (some CI runners).
fn outer_job_forbids_breakaway() -> bool {
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: null job = the job of the calling process; fails when there is none.
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut info).cast(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap(),
            std::ptr::null_mut(),
        )
    };
    ok != 0 && info.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_BREAKAWAY_OK == 0
}

/// A child with piped stdin and stdout (stderr into stdout), optionally in a hidden console,
/// optionally suspended until [`Child::resume`] (so it can be put in a job first).
struct Child {
    proc: Proc,
    thread: OwnedHandle,
    stdin: Option<File>,
    out: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    mark: usize,
}

fn pipe_pair() -> (HANDLE, HANDLE) {
    let (mut r, mut w) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: writable out-pointers; non-inheritable by default.
    let ok = unsafe { CreatePipe(&raw mut r, &raw mut w, std::ptr::null(), 0) };
    assert_ne!(ok, 0);
    (r, w)
}

impl Child {
    fn spawn(cmdline: &str, hidden_console: bool, suspended: bool) -> Self {
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
        // SAFETY: buffer of `size` bytes; `handles` outlives CreateProcessW.
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
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = in_r;
        si.StartupInfo.hStdOutput = out_w;
        si.StartupInfo.hStdError = out_w;
        si.lpAttributeList = attr.as_mut_ptr().cast();
        let mut flags = EXTENDED_STARTUPINFO_PRESENT;
        if hidden_console {
            // A new console whose window is created hidden: never visible on the desktop.
            si.StartupInfo.dwFlags |= STARTF_USESHOWWINDOW;
            si.StartupInfo.wShowWindow = u16::try_from(SW_HIDE).unwrap();
            flags |= CREATE_NEW_CONSOLE;
        } else {
            flags |= CREATE_NO_WINDOW;
        }
        if suspended {
            flags |= CREATE_SUSPENDED;
        }
        let mut cmd = wide(cmdline);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: writable command line, live startup info and attribute list.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                flags,
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
        // SAFETY: fresh owned handles.
        let (stdin, stdout) =
            unsafe { (File::from_raw_handle(in_w), File::from_raw_handle(out_r)) };
        let out = Arc::new(Mutex::new(Vec::new()));
        let reader = std::thread::spawn({
            let out = Arc::clone(&out);
            let mut stdout = stdout;
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
        // SAFETY: fresh handles from CreateProcessW, owned only by these values.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(pi.hProcess),
                OwnedHandle::from_raw_handle(pi.hThread),
            )
        };
        Self {
            proc: Proc {
                pid: pi.dwProcessId,
                h: process,
            },
            thread,
            stdin: Some(stdin),
            out,
            reader: Some(reader),
            mark: 0,
        }
    }

    fn resume(&self) {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: valid primary thread handle of a suspended process.
        let previous = unsafe { ResumeThread(self.thread.as_raw_handle()) };
        assert_ne!(previous, u32::MAX);
    }

    fn attach(ep: &str, size: &str, hidden_console: bool) -> Self {
        let c = Self::spawn(
            &format!("\"{}\" host attach --endpoint {ep} --size {size}", subak()),
            hidden_console,
            false,
        );
        let mut c = c;
        c.expect("Ctrl+B d detaches");
        c
    }

    fn send(&mut self, bytes: &[u8]) {
        let w = self.stdin.as_mut().expect("stdin open");
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    fn text(&self) -> String {
        visible(&self.out.lock().unwrap()[self.mark..])
    }

    /// Waits until `needle` appears in the output after the last mark; moves the mark past it.
    fn expect(&mut self, needle: &str) -> String {
        let until = Instant::now() + LONG;
        loop {
            let text = self.text();
            if text.contains(needle) {
                self.mark = self.out.lock().unwrap().len();
                return text;
            }
            assert!(
                Instant::now() < until,
                "`{needle}` not seen from pid {} (alive: {}):\n{text}",
                self.proc.pid,
                self.proc.alive()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Waits for exit and for the end of output; returns the exit code.
    fn finish(&mut self, t: Duration) -> u32 {
        assert!(
            self.proc.wait(t),
            "pid {} did not exit:\n{}",
            self.proc.pid,
            self.text()
        );
        drop(self.stdin.take());
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        self.proc.exit_code().unwrap()
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.proc.alive() {
            self.proc.kill();
        }
    }
}

/// The console window hosted for any of `pids` (console windows belong to conhost).
fn console_window(pids: &[u32]) -> Option<HWND> {
    windows_of(pids)
        .into_iter()
        .find(|(_, class, _)| class == "ConsoleWindowClass")
        .map(|(hwnd, _, _)| hwnd)
}

/// Every top-level window owned by one of `pids`: (handle, class, visible).
fn windows_of(pids: &[u32]) -> Vec<(HWND, String, bool)> {
    struct Find<'a> {
        pids: &'a [u32],
        found: Vec<(HWND, String, bool)>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> i32 {
        // SAFETY: `lp` is the &mut Find passed to EnumWindows below, alive for the call.
        let f = unsafe { &mut *(lp as *mut Find<'_>) };
        let mut pid = 0u32;
        let mut class = [0u16; 64];
        // SAFETY: valid window from the enumeration; writable buffers.
        let (n, visible) = unsafe {
            GetWindowThreadProcessId(hwnd, &raw mut pid);
            (
                GetClassNameW(hwnd, class.as_mut_ptr(), 64),
                IsWindowVisible(hwnd) != 0,
            )
        };
        if f.pids.contains(&pid) {
            let class = String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)]);
            f.found.push((hwnd, class, visible));
        }
        1
    }
    let mut f = Find {
        pids,
        found: Vec::new(),
    };
    // SAFETY: the callback only dereferences `f`, which outlives the call.
    unsafe { EnumWindows(Some(each), (&raw mut f) as LPARAM) };
    f.found
}

/// A Host started for one test; stopped (or killed) on drop. Checks nothing survives.
struct HostGuard {
    ep: String,
    host: Proc,
    shell: Proc,
}

impl HostGuard {
    fn start(tag: &str) -> Self {
        let ep = endpoint(tag);
        let t = Instant::now();
        let (code, doc) = run(&["host", "start", "--endpoint", &ep]);
        assert_eq!(code, 0, "{doc}");
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "`host start` with captured output returned only after {:?}: a handle leaked into the Host",
            t.elapsed()
        );
        Self::adopt(ep)
    }

    fn adopt(ep: String) -> Self {
        let st = status(&ep);
        Self {
            host: Proc::open(pid_of(&st["host"]["pid"])),
            shell: Proc::open(pid_of(&st["pane"]["pid"])),
            ep,
        }
    }

    fn assert_alive(&self, when: &str) {
        assert!(self.host.alive(), "{when}: Host pid {} died", self.host.pid);
        assert!(
            self.shell.alive(),
            "{when}: shell pid {} died",
            self.shell.pid
        );
        let st = status(&self.ep);
        assert_eq!(
            pid_of(&st["host"]["pid"]),
            self.host.pid,
            "{when}: same Host"
        );
        assert_eq!(
            pid_of(&st["pane"]["pid"]),
            self.shell.pid,
            "{when}: same shell"
        );
        assert_eq!(st["pane"]["running"], true);
    }

    /// The pseudo console host (conhost.exe) the Host's ConPTY runs in.
    fn conpty_host(&self) -> Vec<Proc> {
        children(self.host.pid, "conhost.exe")
            .into_iter()
            .map(Proc::open)
            .collect()
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        if self.host.alive() {
            let _ = run(&["host", "stop", "--endpoint", &self.ep, "--yes"]);
            if !self.host.wait(Duration::from_secs(15)) {
                self.host.kill();
            }
        }
        // The pane job is kill-on-close: the shell cannot outlive its Host.
        let shell_gone = self.shell.wait(Duration::from_secs(10));
        if !shell_gone {
            self.shell.kill();
        }
        if !std::thread::panicking() {
            assert!(shell_gone, "shell pid {} outlived its Host", self.shell.pid);
        }
    }
}

#[test]
fn host_survives_its_launching_console_closing_and_client_crashes_then_reattaches() {
    // A shell in a hidden console window plays the user's terminal: it starts the Host and
    // attaches to it, like typing `subak host start` and `subak host attach` in Windows Terminal.
    let ep = endpoint("survive");
    let mut term = Child::spawn("cmd.exe /d /q /k", true, false);
    term.send(format!("\"{}\" host start --endpoint {ep} --json\r\n", subak()).as_bytes());
    term.expect("\"started\":true");
    let h = HostGuard::adopt(ep.clone());
    term.send(
        format!(
            "\"{}\" host attach --endpoint {ep} --size 100x30\r\n",
            subak()
        )
        .as_bytes(),
    );
    term.expect("Ctrl+B d detaches");
    term.send(b"set KIMSTATE=al^ive-42\r");
    term.send(b"echo mark^er-111\r");
    term.expect("marker-111");
    let attach_pid = children(term.proc.pid, "subak.exe");
    assert_eq!(
        attach_pid.len(),
        1,
        "the attach client runs inside the terminal's shell"
    );
    let attach = Proc::open(attach_pid[0]);
    let conhost = children(term.proc.pid, "conhost.exe");
    assert_eq!(conhost.len(), 1, "the hidden console has its own conhost");
    let conhost = Proc::open(conhost[0]);
    let hwnd = console_window(&[term.proc.pid, conhost.pid]).expect("console window");
    // SAFETY: valid window handle from the enumeration.
    let visible_window = unsafe { IsWindowVisible(hwnd) };
    assert_eq!(visible_window, 0, "the console window stays hidden");
    eprintln!(
        "EVIDENCE close-window: host={} shell={} terminal-shell={} attach-client={} console-host={} (before WM_CLOSE)",
        h.host.pid, h.shell.pid, term.proc.pid, attach.pid, conhost.pid
    );

    // Close the window, exactly like its close button: every process attached to that console
    // gets CTRL_CLOSE_EVENT and ends.
    // SAFETY: posting a message to a window; no pointers involved.
    assert_ne!(unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) }, 0);
    assert!(attach.wait(LONG), "attach client ended with its window");
    assert!(term.proc.wait(LONG), "terminal shell ended with its window");
    assert!(conhost.wait(LONG), "console host ended");
    h.assert_alive("after the launching console window closed");
    eprintln!(
        "EVIDENCE close-window: attach-client={} exit={:?}, terminal-shell exit={:?}, console-host exit={:?}; host={} shell={} alive",
        attach.pid,
        attach.exit_code(),
        term.proc.exit_code(),
        conhost.exit_code(),
        h.host.pid,
        h.shell.pid
    );
    assert_eq!(
        status(&ep)["clients"]["attached"],
        0,
        "the Host dropped the closed client"
    );

    // A client that crashes (TerminateProcess) changes nothing either.
    let mut b = Child::attach(&ep, "100x30", false);
    b.send(b"echo %KIMSTATE%-mark^er-222\r");
    b.expect("alive-42-marker-222");
    b.proc.kill();
    h.assert_alive("after a client was killed");
    eprintln!(
        "EVIDENCE crash-client: client={} killed; host={} shell={} alive",
        b.proc.pid, h.host.pid, h.shell.pid
    );

    // A client in a hidden console whose console host is killed first.
    let c = Child::attach(&ep, "100x30", true);
    let c_conhost = children(c.proc.pid, "conhost.exe");
    assert_eq!(c_conhost.len(), 1);
    let c_conhost = Proc::open(c_conhost[0]);
    c_conhost.kill();
    let client_died_with_console = c.proc.wait(Duration::from_secs(5));
    eprintln!(
        "EVIDENCE kill-console-host: console-host={} killed; client={} ended by itself: {client_died_with_console}",
        c_conhost.pid, c.proc.pid
    );
    c.proc.kill();
    h.assert_alive("after a client's console host was killed");

    // Reattach: the same shell process, with the state set before the window closed.
    let mut d = Child::attach(&ep, "100x30", false);
    d.send(b"echo %KIMSTATE%-mark^er-333\r");
    d.expect("alive-42-marker-333");
    d.send(&[0x02, b'd']); // Ctrl+B d
    assert_eq!(d.finish(LONG), 0);
    assert!(
        d.text().contains("detached; the shell keeps running"),
        "{}",
        d.text()
    );
    h.assert_alive("after an explicit detach");
    eprintln!(
        "EVIDENCE reattach: client={} read state set before the window closed; host={} shell={} unchanged",
        d.proc.pid, h.host.pid, h.shell.pid
    );
}

#[test]
fn stop_needs_consent_and_ends_host_shell_and_console_host() {
    let h = HostGuard::start("stop");
    let consoles = h.conpty_host();
    assert_eq!(consoles.len(), 1, "one pseudo console host per Host");

    let (code, doc) = run(&["host", "stop", "--endpoint", &h.ep]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, doc) = run(&["host", "stop", "--endpoint", &h.ep, "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(
        pid_of(&doc["data"]["would_stop"]["pane"]["pid"]),
        h.shell.pid
    );
    h.assert_alive("after refused and dry-run stops");

    let mut watcher = Child::attach(&h.ep, "80x25", false);
    let (code, doc) = run(&["host", "stop", "--endpoint", &h.ep, "--yes"]);
    assert_eq!(code, 0, "{doc}");
    let d = &doc["data"];
    assert_eq!(d["stopped"], true);
    assert_eq!(d["host"]["exited"], true);
    assert_eq!(pid_of(&d["shell"]["pid"]), h.shell.pid);
    assert_eq!(d["shell"]["job_processes_left"], 0);
    assert!(h.host.wait(LONG) && h.shell.wait(LONG));
    for c in &consoles {
        assert!(c.wait(LONG), "pseudo console host {} ended", c.pid);
    }
    assert_eq!(watcher.finish(LONG), 0, "an attached client ends cleanly");
    assert!(
        watcher.text().contains("the Host stopped (stop_requested)"),
        "{}",
        watcher.text()
    );
    eprintln!(
        "EVIDENCE stop: host={} exit={:?} shell={} exit={:?} (graceful={}) console-host={:?} all ended",
        h.host.pid,
        h.host.exit_code(),
        h.shell.pid,
        h.shell.exit_code(),
        d["shell"]["forced"] == false,
        consoles.iter().map(|c| c.pid).collect::<Vec<_>>()
    );
    for args in [
        &["host", "status"][..],
        &["host", "stop", "--yes"],
        &["host", "attach", "--dry-run"],
    ] {
        let mut a = args.to_vec();
        a.extend(["--endpoint", &h.ep]);
        let (code, doc) = run(&a);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("host_not_running")),
            "{a:?}"
        );
    }
}

#[test]
fn host_crash_ends_the_whole_pane_tree() {
    let h = HostGuard::start("crash");
    let mut c = Child::attach(&h.ep, "80x25", false);
    c.send(b"start /b ping -n 600 127.0.0.1 >nul\r");
    let until = Instant::now() + LONG;
    let ping = loop {
        if let Some(p) = children(h.shell.pid, "ping.exe").first() {
            break Proc::open(*p);
        }
        assert!(Instant::now() < until, "ping did not start");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status(&h.ep)["pane"]["job_processes"].as_u64().unwrap() >= 2);
    let consoles = h.conpty_host();
    h.host.kill(); // a Host crash
    assert!(
        h.shell.wait(LONG),
        "shell ends with its Host (kill-on-close job)"
    );
    assert!(ping.wait(LONG), "grandchild ends with its Host");
    for con in &consoles {
        assert!(con.wait(LONG), "pseudo console host ends with its Host");
    }
    assert_ne!(c.finish(LONG), 0, "the client reports the lost Host");
    assert!(
        c.text().contains("host_disconnected") || c.text().contains("closed the connection"),
        "{}",
        c.text()
    );
    eprintln!(
        "EVIDENCE host-crash: host={} killed; shell={} ping={} console-host={:?} all ended",
        h.host.pid,
        h.shell.pid,
        ping.pid,
        consoles.iter().map(|c| c.pid).collect::<Vec<_>>()
    );
}

#[test]
fn ctrl_c_typed_into_the_pane_interrupts_a_running_command() {
    // Regression: the Host is created with CREATE_NEW_PROCESS_GROUP, which makes Windows ignore
    // Ctrl+C in it and in every child it starts, unless the Host re-enables it (ADR 0016).
    let h = HostGuard::start("ctrlc");
    let mut c = Child::attach(&h.ep, "80x25", false);
    c.send(b"ping -n 60 127.0.0.1\r");
    c.expect("Pinging 127.0.0.1");
    let started = Instant::now();
    c.send(&[0x03]);
    c.send(b"echo mark^er-interrupted\r");
    c.expect("marker-interrupted");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "Ctrl+C did not interrupt ping (took {:?})",
        started.elapsed()
    );
    assert!(children(h.shell.pid, "ping.exe").is_empty(), "ping ended");
    h.assert_alive("after Ctrl+C: only the foreground command ends");
    eprintln!(
        "EVIDENCE ctrl-c: ping in shell={} interrupted after {:?}; host={} and shell alive",
        h.shell.pid,
        started.elapsed(),
        h.host.pid
    );
}

#[test]
fn shell_exit_ends_the_host_and_its_clients() {
    let h = HostGuard::start("exit");
    let mut c = Child::attach(&h.ep, "80x25", false);
    c.send(b"exit 7\r");
    assert_eq!(c.finish(LONG), 0);
    assert!(c.text().contains("exit code 7"), "{}", c.text());
    // Step 09 (ADR 0003 §4): with no pane and no client left, the Host exits after its idle
    // grace period (10 s by default; tests/host_ipc.rs measures it).
    assert!(
        h.host.wait(LONG),
        "the Host idles out once its pane ended and no client is connected"
    );
    assert_eq!(h.shell.exit_code(), Some(7));
    let (code, _) = run(&["host", "status", "--endpoint", &h.ep]);
    assert_eq!(code, 3);
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

#[test]
fn resize_reaches_the_shell_and_reattach_replays_recent_output() {
    let h = HostGuard::start("resize");
    let mut a = Child::attach(&h.ep, "100x30", false);
    a.send(b"mode con & echo mark^er-cols-1\r");
    let text = a.expect("marker-cols-1");
    assert_eq!(columns(&text), Some(100), "{text}");
    a.send(b"echo mark^er-444\r");
    a.expect("marker-444");
    a.send(&[0x02, b'd']);
    assert_eq!(a.finish(LONG), 0);
    let mut b = Child::attach(&h.ep, "132x40", false);
    // Replay: output produced before this client existed (raw history, ADR 0008 prototype).
    b.mark = 0;
    b.expect("marker-444");
    b.send(b"mode con & echo mark^er-cols-2\r");
    let text = b.expect("marker-cols-2");
    assert_eq!(columns(&text), Some(132), "{text}");
    let st = status(&h.ep);
    assert_eq!(
        (
            st["pane"]["size"]["cols"].as_u64(),
            st["pane"]["size"]["rows"].as_u64()
        ),
        (Some(132), Some(40))
    );
    assert_eq!(st["clients"]["attached"], 1);
}

#[test]
fn host_outlives_a_closed_launcher_job_that_allows_breakaway() {
    let ep = endpoint("job-ok");
    let job = Job::new(true);
    let mut start = Child::spawn(
        &format!("\"{}\" host start --endpoint {ep} --json", subak()),
        false,
        true,
    );
    job.assign(&start.proc);
    start.resume();
    assert_eq!(start.finish(LONG), 0, "{}", start.text());
    let doc: Value = serde_json::from_str(start.text().trim()).unwrap();
    let h = HostGuard::adopt(ep.clone());
    let breakaway = doc["data"]["host"]["job_breakaway"] == true;
    let mut client = Child::spawn(
        &format!("\"{}\" host attach --endpoint {ep} --size 80x25", subak()),
        false,
        true,
    );
    job.assign(&client.proc);
    client.resume();
    client.expect("Ctrl+B d detaches");
    let in_launcher_job = job.contains(&h.host);
    let mut me_in_job = 0;
    // SAFETY: pseudo handle of this process; null job = any job.
    unsafe {
        IsProcessInJob(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            std::ptr::null_mut(),
            &raw mut me_in_job,
        )
    };
    eprintln!(
        "EVIDENCE job-breakaway-allowed: this test process in a job: {}, that job forbids breakaway: {}",
        me_in_job != 0,
        outer_job_forbids_breakaway()
    );
    eprintln!(
        "EVIDENCE job-breakaway-allowed: host={} job_breakaway={breakaway} in_launcher_job={in_launcher_job} in_any_job={} client={} (in job)",
        h.host.pid,
        status(&ep)["host"]["in_job"],
        client.proc.pid
    );
    drop(job); // kill-on-close: ends everything still in the launcher's job
    assert!(client.proc.wait(LONG), "the client in the job was killed");
    if breakaway {
        assert!(!in_launcher_job);
        h.assert_alive("after the launcher's job was closed");
        eprintln!(
            "EVIDENCE job-breakaway-allowed: job closed; client={} ended; host={} shell={} alive",
            client.proc.pid, h.host.pid, h.shell.pid
        );
    } else {
        // Only possible when an outer job of this test run forbids breakaway (risk R-03).
        assert!(
            outer_job_forbids_breakaway(),
            "breakaway refused without a reason"
        );
        assert!(
            h.host.wait(LONG),
            "a Host inside the launcher's job dies with it"
        );
        eprintln!(
            "NOTE: this test process runs in a job without JOB_OBJECT_LIMIT_BREAKAWAY_OK; R-03 applies"
        );
    }
}

#[test]
fn host_dies_with_a_closed_launcher_job_that_forbids_breakaway() {
    // Risk R-03, measured: a launcher job without JOB_OBJECT_LIMIT_BREAKAWAY_OK keeps the Host.
    let ep = endpoint("job-deny");
    let job = Job::new(false);
    let mut start = Child::spawn(
        &format!("\"{}\" host start --endpoint {ep} --json", subak()),
        false,
        true,
    );
    job.assign(&start.proc);
    start.resume();
    assert_eq!(start.finish(LONG), 0, "{}", start.text());
    let doc: Value = serde_json::from_str(start.text().trim()).unwrap();
    assert_eq!(
        doc["data"]["host"]["job_breakaway"], false,
        "breakaway is denied and reported"
    );
    assert_eq!(doc["data"]["host"]["in_job"], true);
    assert!(
        doc["warnings"][0]
            .as_str()
            .is_some_and(|w| w.contains("forbids breakaway") && w.contains("R-03")),
        "the user is warned: {doc}"
    );
    let h = HostGuard::adopt(ep.clone());
    assert!(
        job.contains(&h.host),
        "the Host stayed in the launcher's job"
    );
    assert_eq!(
        status(&ep)["host"]["job_breakaway"],
        false,
        "status reports it (ADR 0003)"
    );
    let consoles = h.conpty_host();
    drop(job);
    assert!(
        h.host.wait(LONG),
        "closing the launcher's kill-on-close job ends the Host"
    );
    assert!(h.shell.wait(LONG), "…and its shell");
    for c in &consoles {
        assert!(c.wait(LONG), "…and its pseudo console host");
    }
    eprintln!(
        "EVIDENCE job-breakaway-denied: host={} shell={} console-host={:?} all ended with the launcher's job",
        h.host.pid,
        h.shell.pid,
        consoles.iter().map(|c| c.pid).collect::<Vec<_>>()
    );
}

#[test]
fn attach_detach_cycles_and_status_calls_do_not_leak_host_handles() {
    let h = HostGuard::start("leak");
    let cycle = |i: usize| {
        let mut c = Child::attach(&h.ep, "80x25", false);
        c.send(format!("echo cyc^le-{i}\r").as_bytes());
        c.expect(&format!("cycle-{i}"));
        drop(c.stdin.take()); // end of input = detach
        assert_eq!(c.finish(LONG), 0);
        let _ = status(&h.ep);
    };
    for i in 0..3 {
        cycle(i);
    }
    let before = h.host.handle_count();
    for i in 3..23 {
        cycle(i);
    }
    let after = h.host.handle_count();
    eprintln!(
        "EVIDENCE handles: host={} handle count {before} -> {after} over 20 attach/detach cycles",
        h.host.pid
    );
    // A leaked pipe instance, event or thread per connection would add >= 20.
    assert!(
        after <= before + 8,
        "Host handles grew from {before} to {after}"
    );
}

#[test]
fn only_one_host_per_endpoint_and_start_dry_run_changes_nothing() {
    let h = HostGuard::start("single");
    let (code, doc) = run(&["host", "start", "--endpoint", &h.ep]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("host_already_running"))
    );
    let (code, doc) = run(&["host", "serve", "--endpoint", &h.ep]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("host_already_running"))
    );
    h.assert_alive("after refused duplicate Hosts");

    let ep = endpoint("dry");
    let (code, doc) = run(&["host", "start", "--endpoint", &ep, "--dry-run"]);
    assert_eq!(code, 0);
    let plan = &doc["data"]["would_start"];
    assert!(
        plan["creation_flags"]
            .to_string()
            .contains("DETACHED_PROCESS")
    );
    assert!(
        plan["creation_flags"]
            .to_string()
            .contains("CREATE_BREAKAWAY_FROM_JOB")
    );
    let (code, _) = run(&["host", "status", "--endpoint", &ep]);
    assert_eq!(code, 3, "a dry run starts nothing");

    let (code, doc) = run(&[
        "host",
        "start",
        "--endpoint",
        &ep,
        "--shell",
        "no-such-program-kim.exe",
    ]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (1, Some("spawn_failed")),
        "{doc}"
    );
    let (code, _) = run(&["host", "status", "--endpoint", &ep]);
    assert_eq!(code, 3, "a failed start leaves no Host behind");
}

/// An outer pseudo console owned by the test: a real, invisible console for a client, whose
/// keyboard input and window size the test controls.
struct OuterConsole {
    hpc: HPCON,
    input: File,
    out: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    proc: Proc,
    mark: usize,
}

impl OuterConsole {
    fn spawn(cmdline: &str, cols: i16, rows: i16) -> Self {
        let (in_r, in_w) = pipe_pair();
        let (out_r, out_w) = pipe_pair();
        let mut hpc: HPCON = 0;
        // SAFETY: valid pipe ends; `hpc` receives the pseudo console.
        let hr = unsafe {
            CreatePseudoConsole(COORD { X: cols, Y: rows }, in_r, out_w, 0, &raw mut hpc)
        };
        assert!(hr >= 0, "CreatePseudoConsole: 0x{hr:08X}");
        // SAFETY: conhost duplicated both; ours are no longer needed.
        unsafe {
            CloseHandle(in_r);
            CloseHandle(out_w);
        }
        let mut size = 0usize;
        // SAFETY: size query.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &raw mut size) };
        let mut attr = vec![0u64; size.div_ceil(8)];
        // SAFETY: buffer of `size` bytes; for this attribute the value is the HPCON itself.
        let ok = unsafe {
            InitializeProcThreadAttributeList(attr.as_mut_ptr().cast(), 1, 0, &raw mut size) != 0
                && UpdateProcThreadAttribute(
                    attr.as_mut_ptr().cast(),
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    hpc as *const c_void,
                    size_of::<HPCON>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ) != 0
        };
        assert!(ok);
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES; // null std handles: use the console
        si.lpAttributeList = attr.as_mut_ptr().cast();
        let mut cmd = wide(cmdline);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: writable command line; live startup info; no handle inheritance.
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        // SAFETY: the list was initialized above.
        unsafe { DeleteProcThreadAttributeList(attr.as_mut_ptr().cast()) };
        assert_ne!(ok, 0);
        // SAFETY: fresh handles from CreatePipe/CreateProcessW, owned from here on.
        let (input, output, process) = unsafe {
            CloseHandle(pi.hThread);
            (
                File::from_raw_handle(in_w),
                File::from_raw_handle(out_r),
                OwnedHandle::from_raw_handle(pi.hProcess),
            )
        };
        let out = Arc::new(Mutex::new(Vec::new()));
        let reader = std::thread::spawn({
            let out = Arc::clone(&out);
            let mut output = output;
            move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = output.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    out.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            }
        });
        Self {
            hpc,
            input,
            out,
            reader: Some(reader),
            proc: Proc {
                pid: pi.dwProcessId,
                h: process,
            },
            mark: 0,
        }
    }

    /// Types `bytes` on the console's keyboard.
    fn keys(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }

    fn expect(&mut self, needle: &str) -> String {
        let until = Instant::now() + LONG;
        loop {
            let text = visible(&self.out.lock().unwrap()[self.mark..]);
            if text.contains(needle) {
                self.mark = self.out.lock().unwrap().len();
                return text;
            }
            assert!(Instant::now() < until, "`{needle}` not seen:\n{text}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Resizes the console window, as dragging a terminal window's border does.
    fn resize(&self, cols: i16, rows: i16) {
        // SAFETY: open pseudo console.
        let hr = unsafe { ResizePseudoConsole(self.hpc, COORD { X: cols, Y: rows }) };
        assert!(hr >= 0);
    }
}

impl Drop for OuterConsole {
    fn drop(&mut self) {
        if self.proc.alive() {
            self.proc.kill();
        }
        // SAFETY: created in `spawn`, closed once; the reader keeps draining meanwhile.
        unsafe { ClosePseudoConsole(self.hpc) };
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
    }
}

#[test]
fn console_attach_sends_raw_keys_and_follows_window_resizes() {
    let h = HostGuard::start("console");
    // `-v` logs every control key record the client reads, so a failure shows what the
    // console delivered.
    let mut con = OuterConsole::spawn(
        &format!("\"{}\" -v host attach --endpoint {}", subak(), h.ep),
        100,
        30,
    );
    con.expect("Ctrl+B d detaches");
    // A pseudo console has no visible window: nothing appears on the user's desktop.
    let mut owners = vec![con.proc.pid];
    owners.extend(children(std::process::id(), "conhost.exe"));
    let shown: Vec<_> = windows_of(&owners)
        .into_iter()
        .filter(|(_, _, visible)| *visible)
        .map(|(_, class, _)| class)
        .collect();
    assert!(shown.is_empty(), "visible windows: {shown:?}");
    // Typed keys arrive raw (no line editing by the client's console) and reach the shell.
    con.keys(b"echo mark^er-555\r");
    con.expect("marker-555");
    // The client took the pane size from its own window.
    con.keys(b"mode con & echo mark^er-w1\r");
    let text = con.expect("marker-w1");
    assert_eq!(columns(&text), Some(100), "{text}");
    // The user resizes the window: the client notices and resizes the pane.
    con.resize(90, 24);
    let until = Instant::now() + LONG;
    while status(&h.ep)["pane"]["size"]["cols"] != 90 {
        assert!(
            Instant::now() < until,
            "the pane did not follow the window size"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    con.keys(b"mode con & echo mark^er-w2\r");
    let text = con.expect("marker-w2");
    assert_eq!(columns(&text), Some(90), "{text}");
    // Ctrl+C is a key for the shell (it interrupts ping), not a signal for the client.
    con.keys(b"ping -n 60 127.0.0.1\r");
    con.expect("Pinging 127.0.0.1");
    let started = Instant::now();
    con.keys(&[0x03]);
    con.keys(b"echo mark^er-after-ctrl-c\r");
    con.expect("marker-after-ctrl-c");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "Ctrl+C typed in the console did not interrupt ping"
    );
    assert!(con.proc.alive(), "Ctrl+C did not end the client");
    // Ctrl+B d detaches; the client restores its console and exits 0.
    con.keys(&[0x02, b'd']);
    if !con.proc.wait(LONG) {
        let tail = visible(&con.out.lock().unwrap());
        let tail = &tail[tail.len().saturating_sub(1500)..];
        panic!(
            "client did not exit after Ctrl+B d; attached clients per Host: {}; console:\n{tail}",
            status(&h.ep)["clients"]["attached"]
        );
    }
    assert_eq!(con.proc.exit_code(), Some(0));
    con.mark = 0;
    let all = con.expect("detached; the shell keeps running");
    assert!(all.contains("subak: detach acknowledged"), "{all}");
    let keys: Vec<&str> = all
        .split("subak: key ")
        .filter_map(|r| r.split(['\r', '\n']).next())
        .filter(|r| r.contains("-> 0x0002") || r.contains("-> 0x0003"))
        .collect();
    eprintln!("EVIDENCE console-keys: control key records seen by the client: {keys:?}");
    h.assert_alive("after a console client detached");
    assert_eq!(status(&h.ep)["clients"]["attached"], 0);
    eprintln!(
        "EVIDENCE console-attach: client={} raw keys, Ctrl+C and Ctrl+B d in a console; resize 100x30 -> 90x24 reached shell={} of host={}",
        con.proc.pid, h.shell.pid, h.host.pid
    );
}
