//! One pseudo console (ConPTY) with one root process contained in a kill-on-close Job Object
//! (ADR 0004, validated by Step 08 / ADR 0016).
//!
//! Order matters and is part of what Step 08 validated on Windows 10 19045:
//! 1. two anonymous pipes, `CreatePseudoConsole(size, in_read, out_write)`, then close our copies
//!    of `in_read`/`out_write` (conhost holds its own), so the output pipe reports EOF once the
//!    pseudo console is gone;
//! 2. the root process is created **suspended** with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, put
//!    into a new job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then resumed, so no descendant can
//!    start outside the job;
//! 3. the output pipe must be drained continuously (a reader thread), otherwise the child blocks
//!    on a full pipe and `ClosePseudoConsole` can hang;
//! 4. teardown: `ClosePseudoConsole` (attached processes get `CTRL_CLOSE_EVENT`) while the reader
//!    keeps draining, then `TerminateJobObject` for anything left, then close the handles.

use super::{AttrList, Handle, exit_code, wait, wide};
use std::ffi::c_void;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess,
};

/// A running pseudo console session.
#[derive(Debug)]
pub struct Pty {
    hpc: Mutex<Option<HPCON>>,
    input: Mutex<Option<Handle>>,
    output: Mutex<Option<Handle>>,
    process: Handle,
    job: Handle,
    pid: u32,
}

fn hresult(hr: i32) -> io::Error {
    // HRESULT_FROM_WIN32 keeps the Win32 code in the low 16 bits.
    if (hr as u32) & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error(hr & 0xFFFF)
    } else {
        io::Error::other(format!("HRESULT 0x{:08X}", hr as u32))
    }
}

fn anonymous_pipe() -> io::Result<(Handle, Handle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    // SAFETY: both out-pointers are writable; null attributes = non-inheritable handles.
    if unsafe { CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Handle::new(read)?, Handle::new(write)?))
}

/// A new job whose processes all die when its last handle closes.
pub fn kill_on_close_job() -> io::Result<Handle> {
    // SAFETY: anonymous job, default security; checked by Handle::new.
    let job = Handle::new(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is a valid struct of exactly the size passed.
    let ok = unsafe {
        SetInformationJobObject(
            job.raw(),
            JobObjectExtendedLimitInformation,
            (&raw const info).cast::<c_void>(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).expect("small"),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

impl Pty {
    /// Starts `command_line` in a new pseudo console of `cols`×`rows`, inside a new job.
    pub fn spawn(command_line: &str, cwd: Option<&Path>, cols: u16, rows: u16) -> io::Result<Self> {
        let (in_read, in_write) = anonymous_pipe()?;
        let (out_read, out_write) = anonymous_pipe()?;
        let mut hpc: HPCON = 0;
        // SAFETY: valid pipe ends; `hpc` receives the pseudo console handle.
        let hr = unsafe {
            CreatePseudoConsole(
                size(cols, rows),
                in_read.raw(),
                out_write.raw(),
                0,
                &raw mut hpc,
            )
        };
        if hr < 0 {
            return Err(hresult(hr));
        }
        // conhost duplicated these; keeping ours would hide EOF on the output pipe.
        drop(in_read);
        drop(out_write);
        let pty = PartialPty { hpc };
        let job = kill_on_close_job()?;

        let mut attrs = AttrList::new(1)?;
        // SAFETY: for this attribute the value is the HPCON itself (documented), which stays
        // open until CreateProcessW returns.
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                hpc as *const c_void,
                size_of::<HPCON>(),
            )?;
        }
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).expect("small");
        // Null std handles: the child must use the pseudo console, never handles of the Host
        // (whose own stdio is NUL when it runs detached).
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = attrs.as_ptr();
        let mut cmd = wide(command_line);
        let cwd_w = cwd.map(wide);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: `cmd` is a writable NUL-terminated buffer as CreateProcessW requires; `si` and
        // the attribute list are alive; handles are not inherited (bInheritHandles = FALSE).
        let ok = unsafe {
            CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                std::ptr::null(),
                cwd_w.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
                &raw const si.StartupInfo,
                &raw mut pi,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = Handle::new(pi.hProcess)?;
        let thread = Handle::new(pi.hThread)?;
        // SAFETY: both handles are valid; the child has not executed a single instruction yet.
        if unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } == 0 {
            let err = io::Error::last_os_error();
            // SAFETY: valid process handle with terminate access (we created it).
            unsafe { TerminateProcess(process.raw(), 1) };
            return Err(err);
        }
        // SAFETY: valid thread handle of the suspended primary thread.
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let err = io::Error::last_os_error();
            // SAFETY: valid job handle.
            unsafe { TerminateJobObject(job.raw(), 1) };
            return Err(err);
        }
        Ok(Self {
            hpc: Mutex::new(Some(pty.take())),
            input: Mutex::new(Some(in_write)),
            output: Mutex::new(Some(out_read)),
            process,
            job,
            pid: pi.dwProcessId,
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn process_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.process.raw()
    }

    /// The output read end, for the reader thread (once).
    pub fn take_output(&self) -> Option<Handle> {
        self.output.lock().ok()?.take()
    }

    pub fn write_input(&self, bytes: &[u8]) -> io::Result<()> {
        let guard = self
            .input
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?;
        let Some(h) = guard.as_ref() else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        let mut rest = bytes;
        while !rest.is_empty() {
            let mut n = 0u32;
            let len = u32::try_from(rest.len().min(64 * 1024)).expect("bounded");
            // SAFETY: `rest` has `len` readable bytes; synchronous anonymous pipe.
            if unsafe {
                WriteFile(
                    h.raw(),
                    rest.as_ptr(),
                    len,
                    &raw mut n,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            rest = &rest[n as usize..];
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let guard = self.hpc.lock().map_err(|_| io::Error::other("poisoned"))?;
        let Some(hpc) = *guard else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        // SAFETY: `hpc` is open while it is in the mutex.
        let hr = unsafe { ResizePseudoConsole(hpc, size(cols, rows)) };
        if hr < 0 { Err(hresult(hr)) } else { Ok(()) }
    }

    /// Closes the pseudo console once (processes attached to it get `CTRL_CLOSE_EVENT`) and our
    /// input pipe end. On Windows 10 this blocks until conhost exits, so the output must still
    /// be drained by the reader while it runs.
    pub fn close_console(&self) {
        let hpc = self.hpc.lock().ok().and_then(|mut g| g.take());
        if let Some(hpc) = hpc {
            // SAFETY: taken out of the mutex, so it is closed exactly once.
            unsafe { ClosePseudoConsole(hpc) };
        }
        if let Ok(mut g) = self.input.lock() {
            g.take();
        }
    }

    /// Waits for the root process; `Some(exit code)` once it exited.
    pub fn wait_exit(&self, timeout: Option<Duration>) -> Option<u32> {
        if wait(self.process.raw(), timeout) {
            exit_code(self.process.raw())
        } else {
            None
        }
    }

    /// Processes currently in the pane's job.
    pub fn job_processes(&self) -> u32 {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` is writable and exactly the size passed.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job.raw(),
                JobObjectBasicAccountingInformation,
                (&raw mut info).cast::<c_void>(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).expect("small"),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 { 0 } else { info.ActiveProcesses }
    }

    /// Ends every process in the pane's job.
    pub fn terminate_job(&self, code: u32) {
        // SAFETY: valid job handle.
        unsafe { TerminateJobObject(self.job.raw(), code) };
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // The job handle closes after this (kill-on-close) even if teardown was skipped.
        self.close_console();
    }
}

/// Closes the pseudo console if spawning fails half-way.
struct PartialPty {
    hpc: HPCON,
}

impl PartialPty {
    fn take(self) -> HPCON {
        let hpc = self.hpc;
        std::mem::forget(self);
        hpc
    }
}

impl Drop for PartialPty {
    fn drop(&mut self) {
        // SAFETY: created by CreatePseudoConsole and not handed out (take() forgets self).
        unsafe { ClosePseudoConsole(self.hpc) };
    }
}

fn size(cols: u16, rows: u16) -> COORD {
    COORD {
        X: i16::try_from(cols).unwrap_or(i16::MAX),
        Y: i16::try_from(rows).unwrap_or(i16::MAX),
    }
}

/// Reads pane output; `Ok(0)` once the pseudo console closed its end.
pub fn read_output(h: &Handle, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0u32;
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    // SAFETY: `buf` has `len` writable bytes; synchronous anonymous pipe read by one thread.
    if unsafe {
        ReadFile(
            h.raw(),
            buf.as_mut_ptr(),
            len,
            &raw mut n,
            std::ptr::null_mut(),
        )
    } == 0
    {
        let err = io::Error::last_os_error();
        return match err.raw_os_error().map(|c| c as u32) {
            Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA) => Ok(0),
            _ => Err(err),
        };
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    //! In-process ConPTY tests on the real OS: interactive I/O, resize, teardown and handle
    //! hygiene (`SUBAK-FR-016`, ADR 0004). They never open a window: a pseudo console has none.
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Instant;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    /// Strips CSI/OSC escape sequences so text checks see what a user sees.
    pub fn visible(bytes: &[u8]) -> String {
        let s = String::from_utf8_lossy(bytes);
        let mut out = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\u{1b}' {
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
            } else {
                out.push(c);
            }
        }
        out
    }

    struct Session {
        pty: Arc<Pty>,
        rx: mpsc::Receiver<Vec<u8>>,
        seen: Vec<u8>,
        reader: std::thread::JoinHandle<()>,
    }

    impl Session {
        fn start(cols: u16, rows: u16) -> Self {
            let pty = Arc::new(Pty::spawn("cmd.exe /d /q", None, cols, rows).expect("spawn cmd"));
            let out = pty.take_output().expect("output");
            let (tx, rx) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = read_output(&out, &mut buf) {
                    if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            });
            Self {
                pty,
                rx,
                seen: Vec::new(),
                reader,
            }
        }

        fn expect(&mut self, needle: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let text = visible(&self.seen);
                if text.contains(needle) {
                    return text;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "`{needle}` not seen; got:\n{text}");
                if let Ok(chunk) = self.rx.recv_timeout(left.min(Duration::from_millis(200))) {
                    self.seen.extend(chunk);
                }
            }
        }

        /// The Host's teardown order: close the pseudo console, give the shell a grace period,
        /// then end the job. Returns whether closing the console alone ended the shell: true on
        /// Windows 10 19045; Windows Server 2025's inbox ConPTY (CI) needed the job (ADR 0016).
        fn end(self) -> bool {
            self.pty.close_console();
            let graceful = self.pty.wait_exit(Some(Duration::from_secs(3))).is_some();
            if !graceful {
                self.pty.terminate_job(1);
            }
            let code = self.pty.wait_exit(Some(Duration::from_secs(10)));
            assert!(code.is_some(), "the shell ended (graceful: {graceful})");
            let until = Instant::now() + Duration::from_secs(10);
            while self.pty.job_processes() > 0 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(self.pty.job_processes(), 0, "job is empty");
            while !self.reader.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(self.reader.is_finished(), "the output pipe reached EOF");
            self.reader.join().unwrap();
            graceful
        }
    }

    #[test]
    fn interactive_io_resize_and_clean_teardown() {
        let mut s = Session::start(100, 30);
        s.pty.write_input(b"echo mark^er-io-1\r").unwrap();
        s.expect("marker-io-1");
        s.pty.write_input(b"mode con\r").unwrap();
        let text = s.expect("Columns:");
        assert!(
            text.contains("100"),
            "initial size reaches the shell:\n{text}"
        );
        s.pty.resize(132, 40).unwrap();
        s.seen.clear();
        s.pty.write_input(b"mode con\r").unwrap();
        let text = s.expect("Columns:");
        s.expect("132");
        assert!(text.contains("Lines:"), "{text}");
        assert!(s.pty.job_processes() >= 1, "the shell is in the pane job");
        let graceful = s.end();
        eprintln!("ConPTY teardown: ClosePseudoConsole alone ended the shell: {graceful}");
    }

    #[test]
    fn repeated_sessions_do_not_leak_handles() {
        let count = || {
            let mut n = 0u32;
            // SAFETY: pseudo handle of this process; `n` is writable.
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &raw mut n) };
            n
        };
        // Warm up lazily created runtime handles before measuring.
        let mut s = Session::start(80, 25);
        s.expect(">"); // the shell is up (prompt)
        s.end();
        let before = count();
        for i in 0..8 {
            let mut s = Session::start(80, 25);
            s.pty
                .write_input(format!("echo cyc^le-{i}\r").as_bytes())
                .unwrap();
            s.expect(&format!("cycle-{i}"));
            s.end();
        }
        let after = count();
        // Each session opens ~8 handles (2 pipes, job, process, thread, attribute data); a leak
        // would grow by ~64. Other tests in this binary run concurrently, hence the slack.
        assert!(
            after <= before + 16,
            "handles grew from {before} to {after}"
        );
    }
}
