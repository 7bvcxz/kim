//! Shared harness of the Subak Host integration tests (`host_ipc.rs`, `session_lifecycle.rs`):
//! real `subak.exe` Hosts on private endpoints, a raw IPC v1 client (overlapped pipe I/O) whose
//! every received control message is validated against `docs/contracts/schemas/ipc-v1.schema.json`,
//! CLI runs whose `--json` envelopes are validated, and process inspection. Nothing here opens a
//! window; every Host and child it starts is stopped or killed on drop.
#![allow(dead_code, unsafe_code)]
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, OpenProcess, OpenThread, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    TerminateProcess, WaitForSingleObject,
};

pub const LONG: Duration = Duration::from_secs(20);

pub fn subak() -> &'static str {
    env!("CARGO_BIN_EXE_subak")
}

pub fn endpoint(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("subak-ipc-{tag}-{}-{nanos}", std::process::id())
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn pipe_name(ep: &str) -> Vec<u16> {
    wide(&format!(r"\\.\pipe\{ep}"))
}

pub fn schema(name: &str) -> &'static Value {
    static CACHE: OnceLock<Mutex<Vec<(String, &'static Value)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let mut c = cache.lock().unwrap();
    if let Some((_, v)) = c.iter().find(|(n, _)| n == name) {
        return v;
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../docs/contracts/schemas/{name}.schema.json"));
    let v: &'static Value = Box::leak(Box::new(
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap(),
    ));
    c.push((name.to_string(), v));
    v
}

pub fn validate(name: &str, doc: &Value) {
    let errors = crate::jsonschema::Validator::new(schema(name))
        .validate(doc)
        .unwrap();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{doc:#}");
}

/// Checks `v` against one definition of the IPC v1 schema (e.g. a method's `result_*`).
pub fn ipc_def(def: &str, v: &Value) {
    let s = schema("ipc-v1");
    assert!(s["$defs"][def].is_object(), "no definition `{def}`");
    let wrapper = json!({"$defs": s["$defs"], "$ref": format!("#/$defs/{def}")});
    let errors = crate::jsonschema::Validator::new(&wrapper)
        .validate(v)
        .unwrap();
    assert!(errors.is_empty(), "{def}: {errors:?}\n{v:#}");
}

/// Every control message the real Host sends must be valid IPC v1.
pub fn ipc_valid(msg: &Value) {
    validate("ipc-v1", msg);
}

/// `subak --json <args>` without a window (and with `env` set); returns (exit code, envelope).
/// `--json` comes first because everything after a `--` in `args` is a Terminal's command.
pub fn run_env(args: &[&str], env: &[(&str, &str)]) -> (i32, Value) {
    let mut cmd = Command::new(subak());
    cmd.arg("--json")
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    // Step 20: `subak new` reads the user's settings (a default shell profile) from %APPDATA%; a
    // test never reads the maintainer's real settings unless it gives its own APPDATA.
    if !env.iter().any(|(k, _)| k.eq_ignore_ascii_case("APPDATA")) {
        cmd.env("APPDATA", empty_appdata());
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "`subak {}`: not one JSON document ({e}): {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    validate("cli-envelope-v1", &doc);
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

pub fn run(args: &[&str]) -> (i32, Value) {
    run_env(args, &[])
}

/// An empty `APPDATA` for CLI runs that do not bring their own (no settings, no shortcuts).
pub fn empty_appdata() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("subak-test-appdata-empty-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn pid_of(v: &Value) -> u32 {
    u32::try_from(v.as_u64().unwrap_or_else(|| panic!("not a pid: {v}"))).unwrap()
}

/// A process we can watch, suspend and end.
#[derive(Debug)]
pub struct Proc {
    pub pid: u32,
    pub h: OwnedHandle,
}

impl Proc {
    pub fn open(pid: u32) -> Self {
        Self::try_open(pid).unwrap_or_else(|| panic!("process {pid} is not running"))
    }

    /// `None` when no such process exists (or it cannot be opened).
    pub fn try_open(pid: u32) -> Option<Self> {
        // SAFETY: plain open; null is checked.
        let h = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        if h.is_null() {
            return None;
        }
        // SAFETY: a fresh handle owned only by this value.
        let h = unsafe { OwnedHandle::from_raw_handle(h) };
        Some(Self { pid, h })
    }

    pub fn wait(&self, t: Duration) -> bool {
        // SAFETY: valid process handle.
        unsafe {
            WaitForSingleObject(
                self.h.as_raw_handle(),
                u32::try_from(t.as_millis()).unwrap(),
            ) == WAIT_OBJECT_0
        }
    }

    pub fn alive(&self) -> bool {
        !self.wait(Duration::ZERO)
    }

    pub fn kill(&self) {
        // SAFETY: valid handle with terminate access.
        unsafe { TerminateProcess(self.h.as_raw_handle(), 1) };
        self.wait(Duration::from_secs(10));
    }

    /// Suspends every thread (a hung Host). Resumed by [`Suspended`]'s drop.
    pub fn suspend(&self) -> Suspended {
        let mut threads = Vec::new();
        // SAFETY: Toolhelp snapshot, closed below.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        let mut e = THREADENTRY32 {
            dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
            ..Default::default()
        };
        // SAFETY: `e.dwSize` is set.
        let mut ok = unsafe { Thread32First(snap, &raw mut e) } != 0;
        while ok {
            if e.th32OwnerProcessID == self.pid {
                // SAFETY: plain open of a thread of our own test Host.
                let t = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, e.th32ThreadID) };
                if !t.is_null() {
                    // SAFETY: valid thread handle with suspend access.
                    unsafe { SuspendThread(t) };
                    // SAFETY: fresh owned handle.
                    threads.push(unsafe { OwnedHandle::from_raw_handle(t) });
                }
            }
            // SAFETY: same snapshot and entry.
            ok = unsafe { Thread32Next(snap, &raw mut e) } != 0;
        }
        // SAFETY: valid snapshot handle.
        unsafe { CloseHandle(snap) };
        Suspended(threads)
    }
}

pub struct Suspended(pub Vec<OwnedHandle>);

impl Drop for Suspended {
    fn drop(&mut self) {
        for t in &self.0 {
            // SAFETY: valid thread handle that we suspended once.
            unsafe { ResumeThread(t.as_raw_handle()) };
        }
    }
}

/// (pid, parent pid, exe name) of every process.
pub fn processes() -> Vec<(u32, u32, String)> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp snapshot, closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let mut e = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap(),
        ..Default::default()
    };
    // SAFETY: `e.dwSize` is set.
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
    drop_stale_parents(&mut out);
    out
}

/// Windows keeps a process's parent PID after the parent exits, and PIDs are reused quickly
/// (CI runners especially). A process created *before* the process now holding its parent PID
/// cannot be that process's child, so its parent is cleared (0). Without this, another test's
/// Host, whose launcher had exited, showed up as a child of this test's Host when the Host
/// received the launcher's old PID ("the Host has no child process: [(3764, 5828, \"subak.exe\")]").
fn drop_stale_parents(list: &mut [(u32, u32, String)]) {
    let pids: std::collections::HashSet<u32> = list.iter().map(|(p, _, _)| *p).collect();
    let mut created = std::collections::HashMap::new();
    for (pid, parent, _) in list.iter() {
        if pids.contains(parent) {
            for p in [*pid, *parent] {
                created.entry(p).or_insert_with(|| creation_time(p));
            }
        }
    }
    for (pid, parent, _) in list.iter_mut() {
        if let (Some(Some(child)), Some(Some(holder))) = (created.get(pid), created.get(parent))
            && child < holder
        {
            *parent = 0;
        }
    }
}

/// Process creation time (100 ns units since 1601), if the process can still be opened.
fn creation_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    // SAFETY: plain query; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid handle and out-pointers.
    let ok = unsafe { GetProcessTimes(h, &raw mut c, &raw mut e, &raw mut k, &raw mut u) } != 0;
    // SAFETY: valid handle.
    unsafe { CloseHandle(h) };
    ok.then(|| (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime))
}

/// A Host started for one test; stopped (or killed) on drop.
pub struct Host {
    pub ep: String,
    pub proc: Proc,
}

impl Host {
    pub fn start(tag: &str, extra: &[&str]) -> Self {
        let ep = endpoint(tag);
        let mut args = vec!["host", "start", "--endpoint", &ep];
        args.extend_from_slice(extra);
        let (code, doc) = run(&args);
        assert_eq!(code, 0, "{doc}");
        Self {
            proc: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
            ep,
        }
    }

    pub fn status(&self) -> Value {
        let (code, doc) = run(&["host", "status", "--endpoint", &self.ep]);
        assert_eq!(code, 0, "{doc}");
        doc["data"].clone()
    }

    /// `host.stats` through a raw `cli` connection (there is no CLI command for it yet).
    pub fn stats(&self) -> Value {
        let c = Raw::handshake(&self.ep, "cli");
        let r = c.request(1, "host.stats", &json!({}));
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    }

    pub fn connected(&self) -> u64 {
        self.status()["clients"]["connected"].as_u64().unwrap()
    }

    /// Waits until the Host counts `n` connections (the status call itself is one).
    pub fn wait_connected(&self, n: u64) {
        let until = Instant::now() + LONG;
        while self.connected() != n {
            assert!(Instant::now() < until, "connections did not settle at {n}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if self.proc.alive() {
            let _ = run(&[
                "host",
                "stop",
                "--endpoint",
                &self.ep,
                "--kill-terminals",
                "--yes",
            ]);
            if !self.proc.wait(Duration::from_secs(15)) {
                self.proc.kill();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A raw IPC v1 client: overlapped pipe I/O (synchronous handles deadlock duplex use, ADR 0016).

pub fn control(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = u32::try_from(body.len() + 1)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    out.push(1);
    out.extend_from_slice(body.as_bytes());
    out
}

pub fn data(stream: u32, seq: u64, flags: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(bytes.len() + 14)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    out.push(2);
    out.extend_from_slice(&stream.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(bytes);
    out
}

pub fn hello(kind: &str) -> Value {
    json!({"type": "hello", "protocol": {"min": 1, "max": 1},
           "client": {"name": "host-ipc-test", "version": "0.0.1", "kind": kind}})
}

#[derive(Debug, PartialEq)]
pub enum Got {
    Control(Value),
    Data(Vec<u8>),
    Eof,
    Timeout,
}

pub struct Raw {
    pub h: OwnedHandle,
    /// Bytes read but not yet returned as a frame. A timeout in the middle of a frame keeps
    /// them here, so the next call continues the same frame instead of reading the rest of it
    /// as a new header (CI saw "bad frame length" and "frame kind 92" from exactly that).
    pending: std::cell::RefCell<Vec<u8>>,
    /// `(stream, seq, flags)` of the last data frame returned (Step 13 checks seq and the
    /// snapshot flag).
    pub last_data: std::cell::Cell<Option<(u32, u64, u8)>>,
}

impl Raw {
    pub fn try_open(ep: &str) -> std::io::Result<Self> {
        let name = pipe_name(ep);
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            // SAFETY: NUL-terminated name; checked below.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                // SAFETY: fresh owned handle.
                let h = unsafe { OwnedHandle::from_raw_handle(h) };
                return Ok(Self {
                    h,
                    pending: std::cell::RefCell::new(Vec::new()),
                    last_data: std::cell::Cell::new(None),
                });
            }
            // SAFETY: thread-local read.
            let err = unsafe { GetLastError() };
            if err == ERROR_PIPE_BUSY && Instant::now() < until {
                // SAFETY: NUL-terminated name.
                unsafe { WaitNamedPipeW(name.as_ptr(), 500) };
                continue;
            }
            return Err(std::io::Error::from_raw_os_error(err as i32));
        }
    }

    pub fn open(ep: &str) -> Self {
        Self::try_open(ep).unwrap_or_else(|e| panic!("connect to {ep}: {e}"))
    }

    pub fn io(
        &self,
        write: bool,
        buf: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> std::io::Result<usize> {
        // SAFETY: anonymous manual-reset event, closed below.
        let ev = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        let mut ov = OVERLAPPED {
            hEvent: ev,
            ..Default::default()
        };
        let h = self.h.as_raw_handle();
        let len = u32::try_from(len.min(1 << 20)).unwrap();
        let mut n = 0u32;
        // SAFETY: `buf` has `len` bytes; `ov` lives until the operation is reaped below.
        let ok = unsafe {
            if write {
                WriteFile(h, buf, len, &raw mut n, &raw mut ov)
            } else {
                ReadFile(h, buf, len, &raw mut n, &raw mut ov)
            }
        };
        let result = (|| {
            if ok == 0 {
                // SAFETY: thread-local read.
                let err = unsafe { GetLastError() };
                if err != ERROR_IO_PENDING {
                    return Err(std::io::Error::from_raw_os_error(err as i32));
                }
                let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
                // SAFETY: valid event.
                if unsafe { WaitForSingleObject(ev, ms) } == WAIT_TIMEOUT {
                    // SAFETY: cancel and reap this operation before `ov` and `buf` go away.
                    let done = unsafe {
                        CancelIoEx(h, &raw const ov);
                        GetOverlappedResult(h, &raw const ov, &raw mut n, 1)
                    };
                    return if done != 0 && n > 0 {
                        Ok(n as usize)
                    } else {
                        Err(std::io::ErrorKind::TimedOut.into())
                    };
                }
            }
            // SAFETY: the operation completed; reap it.
            if unsafe { GetOverlappedResult(h, &raw const ov, &raw mut n, 1) } == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(n as usize)
        })();
        // SAFETY: the event belongs to this call.
        unsafe { CloseHandle(ev) };
        match result {
            Err(e)
                if matches!(
                    e.raw_os_error().map(|c| c as u32),
                    Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
                ) =>
            {
                Ok(0)
            }
            other => other,
        }
    }

    pub fn write(&self, mut bytes: &[u8]) -> std::io::Result<()> {
        while !bytes.is_empty() {
            let n = self.io(true, bytes.as_ptr().cast_mut(), bytes.len(), LONG)?;
            if n == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            bytes = &bytes[n..];
        }
        Ok(())
    }

    pub fn send(&self, msg: &Value) {
        self.write(&control(msg)).expect("send to the Host");
    }

    /// The next frame, end of stream, or a timeout. Control messages are schema-validated.
    ///
    /// Reads into `pending` and returns a frame only once it is complete, so a timeout never
    /// loses bytes or the frame boundary.
    pub fn next(&self, timeout: Duration) -> Got {
        let until = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.take_frame() {
                return frame;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Got::Timeout;
            }
            let mut chunk = vec![0u8; 64 * 1024];
            match self.io(false, chunk.as_mut_ptr(), chunk.len(), left) {
                Ok(n) if n > 0 => self.pending.borrow_mut().extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Got::Timeout,
                // End of stream (or a broken pipe, reported as 0 by `io`).
                _ => {
                    assert!(
                        self.pending.borrow().is_empty(),
                        "the Host sent a truncated frame"
                    );
                    return Got::Eof;
                }
            }
        }
    }

    /// One complete frame from `pending`, if it holds one.
    fn take_frame(&self) -> Option<Got> {
        let mut pending = self.pending.borrow_mut();
        if pending.len() < 5 {
            return None;
        }
        let len = u32::from_le_bytes(pending[..4].try_into().unwrap()) as usize;
        assert!(
            (1..=(1 << 20) + 14).contains(&len),
            "the Host sent a bad frame length {len}"
        );
        if pending.len() < 4 + len {
            return None;
        }
        let frame: Vec<u8> = pending.drain(..4 + len).collect();
        drop(pending);
        let body = &frame[5..];
        Some(match frame[4] {
            1 => {
                let v: Value = serde_json::from_slice(body).expect("the Host sends JSON");
                ipc_valid(&v);
                Got::Control(v)
            }
            2 => {
                let stream = u32::from_le_bytes(body[..4].try_into().unwrap());
                let seq = u64::from_le_bytes(body[4..12].try_into().unwrap());
                self.last_data.set(Some((stream, seq, body[12])));
                Got::Data(body[13..].to_vec())
            }
            k => panic!("the Host sent frame kind {k}"),
        })
    }

    /// The next control message, skipping data frames.
    pub fn next_control(&self, timeout: Duration) -> Got {
        let until = Instant::now() + timeout;
        loop {
            match self.next(until.saturating_duration_since(Instant::now())) {
                Got::Data(_) => {}
                other => return other,
            }
        }
    }

    pub fn handshake(ep: &str, kind: &str) -> Self {
        let c = Self::open(ep);
        c.send(&hello(kind));
        match c.next(LONG) {
            Got::Control(m) if m["type"] == "welcome" => {
                assert_eq!(m["protocol"], 1);
                c
            }
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    /// Sends a request; returns its response (events and data in between are skipped).
    pub fn request(&self, id: u64, method: &str, params: &Value) -> Value {
        self.send(&json!({"type": "request", "id": id, "method": method, "params": params}));
        let until = Instant::now() + LONG;
        loop {
            match self.next_control(until.saturating_duration_since(Instant::now())) {
                Got::Control(m) if m["type"] == "response" && m["id"] == id => return m,
                Got::Control(m) if m["type"] == "event" => {}
                other => panic!("no response to {method}: {other:?}"),
            }
        }
    }

    /// The connection-level error the Host sends before closing, then end of stream.
    pub fn expect_closed_with(&self, code: &str, within: Duration) -> Value {
        let m = match self.next_control(within) {
            Got::Control(m) if m["type"] == "error" => m,
            other => panic!("expected a `{code}` error, got {other:?}"),
        };
        assert_eq!(m["error"]["code"], code, "{m}");
        assert_eq!(
            self.next_control(LONG),
            Got::Eof,
            "the Host closes after `{code}`"
        );
        m
    }
}

pub fn error_code(response: &Value) -> &str {
    assert_eq!(response["ok"], false, "{response}");
    response["error"]["code"].as_str().unwrap()
}

/// Strips CSI/OSC escape sequences.
pub fn visible(bytes: &[u8]) -> String {
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

/// A `subak host attach` (or `attach <target>`) client with piped stdin and output
/// (stdout and stderr together).
pub struct Attach {
    pub child: Child,
    pub out: Arc<Mutex<Vec<u8>>>,
}

impl Attach {
    pub fn spawn(ep: &str) -> Self {
        Self::with(&["host", "attach", "--endpoint", ep, "--size", "100x30"])
    }

    /// `subak attach <target>`.
    pub fn terminal(ep: &str, target: &str) -> Self {
        Self::with(&["attach", target, "--endpoint", ep, "--size", "100x30"])
    }

    fn with(args: &[&str]) -> Self {
        let mut child = Command::new(subak())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("attach");
        let out = Arc::new(Mutex::new(Vec::new()));
        for mut stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let sink = Arc::clone(&out);
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
        let a = Self { child, out };
        a.expect("Ctrl+B d detaches");
        a
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    /// What a user would see (escape sequences removed).
    pub fn text(&self) -> String {
        visible(&self.out.lock().unwrap())
    }

    pub fn expect(&self, needle: &str) {
        let until = Instant::now() + LONG;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < until,
                "`{needle}` not seen by attach pid {}:\n{}",
                self.child.id(),
                self.text()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Attach {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
