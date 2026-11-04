//! Named-pipe transport of the CLI ↔ app protocol (ADR 0024 §5), adapted from Subak's IPC v1
//! transport (ADR 0019) without depending on Subak (`KIM-FR-001`).
//!
//! - **Overlapped I/O** with a timeout on every read, so a stuck peer never hangs the CLI or the
//!   app's IPC thread.
//! - The pipe is local only (`PIPE_REJECT_REMOTE_CLIENTS`), its first instance is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (one app per endpoint), and it carries an owner-only DACL
//!   (deny NETWORK, allow the current user).
//! - **Peer identity.** The server reads the user SID from the token of the connection
//!   ([`Pipe::client_user_sid`], identification-level impersonation); the client looks up the
//!   server process ([`Pipe::server_pid`]) and checks its image and user before sending.
//! - Clients connect with `SECURITY_IDENTIFICATION`, so the app can read their identity but
//!   never act as them.

use super::{Event, Handle, SecurityDescriptor, token_user_sid, user_sid, wide};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, INFINITE, OpenThreadToken, WaitForMultipleObjects, WaitForSingleObject,
};

const BUFFER: u32 = 64 * 1024;

/// `\\.\pipe\<endpoint>`.
pub fn path(endpoint: &str) -> String {
    format!(r"\\.\pipe\{endpoint}")
}

/// Ddalgak's endpoint: one per user SID and Windows (logon) session (ADR 0024 §5).
pub fn default_endpoint() -> io::Result<String> {
    Ok(format!(
        "mouv-ddalgak-{}-s{}",
        user_sid()?,
        super::session_id()
    ))
}

/// One connected pipe end (either side).
#[derive(Debug)]
pub struct Pipe {
    h: Handle,
}

fn os_err(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

impl Pipe {
    /// One overlapped read or write. `timeout` bounds the wait (reads only).
    fn io(
        &self,
        write: bool,
        buf: *mut u8,
        len: usize,
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        let event = Event::new()?;
        let mut ov = OVERLAPPED {
            hEvent: event.raw(),
            ..Default::default()
        };
        let len = u32::try_from(len.min(1 << 20)).expect("bounded");
        let mut n = 0u32;
        // SAFETY: `buf` has `len` valid bytes (callers pass slices); `ov` and its event live until
        // the operation has completed or been cancelled and reaped below.
        let ok = unsafe {
            if write {
                WriteFile(self.h.raw(), buf, len, &raw mut n, &raw mut ov)
            } else {
                ReadFile(self.h.raw(), buf, len, &raw mut n, &raw mut ov)
            }
        };
        if ok == 0 {
            // SAFETY: trivially safe thread-local read.
            let err = unsafe { GetLastError() };
            if err != ERROR_IO_PENDING {
                return Err(os_err(err));
            }
            let ms = timeout.map_or(INFINITE, |d| {
                u32::try_from(d.as_millis()).unwrap_or(INFINITE - 1)
            });
            // SAFETY: valid event handle.
            if unsafe { WaitForSingleObject(event.raw(), ms) } == WAIT_TIMEOUT {
                // SAFETY: cancels exactly this operation; then wait for it to finish so `ov` and
                // `buf` are no longer referenced by the kernel.
                let done = unsafe {
                    CancelIoEx(self.h.raw(), &raw const ov);
                    GetOverlappedResult(self.h.raw(), &raw const ov, &raw mut n, 1)
                };
                // The operation may have completed just before the cancel: keep those bytes.
                return if done != 0 && n > 0 {
                    Ok(n as usize)
                } else {
                    Err(io::ErrorKind::TimedOut.into())
                };
            }
        }
        // SAFETY: the operation completed (or was signaled); reap its result.
        if unsafe { GetOverlappedResult(self.h.raw(), &raw const ov, &raw mut n, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Reads at least one byte, or returns 0 at end of stream (peer closed).
    pub fn read_some(&self, buf: &mut [u8], timeout: Option<Duration>) -> io::Result<usize> {
        match self.io(false, buf.as_mut_ptr(), buf.len(), timeout) {
            Err(e) if is_closed(&e) => Ok(0),
            other => other,
        }
    }

    pub fn write_all_bytes(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            // The kernel only reads from `buf` for a write.
            let n = self.io(true, buf.as_ptr().cast_mut(), buf.len(), None)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            buf = &buf[n..];
        }
        Ok(())
    }

    /// Aborts pending I/O on this handle from another thread (used to drop a connection).
    pub fn cancel(&self) {
        // SAFETY: valid handle; null = all I/O issued on it by any thread.
        unsafe { CancelIoEx(self.h.raw(), std::ptr::null()) };
    }

    /// Server side: the client's process id.
    pub fn client_pid(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: valid server pipe handle; `pid` is writable.
        (unsafe { GetNamedPipeClientProcessId(self.h.raw(), &raw mut pid) } != 0).then_some(pid)
    }

    /// Client side: the id of the process that created the server end (the app, or a squatter).
    pub fn server_pid(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: valid client pipe handle; `pid` is writable.
        (unsafe { GetNamedPipeServerProcessId(self.h.raw(), &raw mut pid) } != 0).then_some(pid)
    }

    /// Server side: the user SID of the connected client, read from the token of this
    /// connection rather than looked up by process id: no pid-reuse race, and it also works for
    /// an elevated client whose process token a normal Host may not open.
    ///
    /// Clients connect with `SECURITY_IDENTIFICATION`, so the impersonation only lets this thread
    /// *read* the identity, and it reverts before returning. Windows allows it only after data
    /// was read from the pipe, so call it after the first frame.
    pub fn client_user_sid(&self) -> io::Result<String> {
        // SAFETY: valid server pipe handle; the impersonation is undone below on every path.
        if unsafe { ImpersonateNamedPipeClient(self.h.raw()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut token = std::ptr::null_mut();
        // SAFETY: opens this thread's impersonation token; OpenAsSelf = TRUE checks access
        // against the Host's own token (an identification-level token cannot open objects).
        let opened =
            unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) } != 0;
        let open_error = io::Error::last_os_error();
        // SAFETY: ends the impersonation started above.
        if unsafe { RevertToSelf() } == 0 {
            // The caller drops the connection, and its thread ends.
            return Err(io::Error::other(
                "RevertToSelf failed after reading the client's identity",
            ));
        }
        if !opened {
            return Err(open_error);
        }
        token_user_sid(&Handle::new(token)?)
    }
}

fn is_closed(e: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
    )
}

/// The server side: owns the next unconnected instance and a stop event.
#[derive(Debug)]
pub struct Listener {
    name: Vec<u16>,
    sd: SecurityDescriptor,
    next: Option<Pipe>,
    stop: Arc<Event>,
}

impl Listener {
    /// Creates the first instance. Fails with `AlreadyExists` when any process already serves
    /// this endpoint.
    pub fn bind(endpoint: &str) -> io::Result<Self> {
        // Deny NETWORK logons, allow only the current user (generic all). Protected: no
        // inherited ACEs. The peer identity checks come on top (ADR 0019).
        let sd =
            SecurityDescriptor::from_sddl(&format!("D:P(D;;GA;;;NU)(A;;GA;;;{})", user_sid()?))?;
        let mut l = Self {
            name: wide(path(endpoint)),
            sd,
            next: None,
            stop: Arc::new(Event::new()?),
        };
        l.next = Some(l.instance(true).map_err(|e| {
            // ACCESS_DENIED: an instance exists (FILE_FLAG_FIRST_PIPE_INSTANCE). PIPE_BUSY: an
            // instance exists and its creator's instance limit is reached (measured with a
            // squatter that allows one instance, Step 09).
            if matches!(
                e.raw_os_error().map(|c| c as u32),
                Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY)
            ) {
                io::Error::new(io::ErrorKind::AlreadyExists, "endpoint already served")
            } else {
                e
            }
        })?);
        Ok(l)
    }

    fn instance(&self, first: bool) -> io::Result<Pipe> {
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).expect("small"),
            lpSecurityDescriptor: self.sd.as_ptr(),
            bInheritHandle: 0,
        };
        let open = PIPE_ACCESS_DUPLEX
            | FILE_FLAG_OVERLAPPED
            | if first {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
        // SAFETY: `name` is NUL-terminated; `sa` and its descriptor are alive for the call.
        let h = unsafe {
            CreateNamedPipeW(
                self.name.as_ptr(),
                open,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                &raw const sa,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe { h: Handle::new(h)? })
    }

    /// The event that makes [`accept`](Self::accept) return `None`.
    pub fn stopper(&self) -> Arc<Event> {
        Arc::clone(&self.stop)
    }

    /// Waits for the next client; `None` once the stop event is set.
    pub fn accept(&mut self) -> io::Result<Option<Pipe>> {
        loop {
            let inst = match self.next.take() {
                Some(p) => p,
                None => self.instance(false)?,
            };
            let event = Event::new()?;
            let mut ov = OVERLAPPED {
                hEvent: event.raw(),
                ..Default::default()
            };
            // SAFETY: valid instance handle; `ov` lives until the connect completes or is reaped.
            let ok = unsafe { ConnectNamedPipe(inst.h.raw(), &raw mut ov) };
            let connected = if ok != 0 {
                true
            } else {
                // SAFETY: trivially safe thread-local read.
                match unsafe { GetLastError() } {
                    ERROR_PIPE_CONNECTED => true,
                    ERROR_NO_DATA => false, // client came and went; serve a fresh instance
                    ERROR_IO_PENDING => {
                        let handles = [event.raw(), self.stop.raw()];
                        // SAFETY: two valid handles.
                        let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                        let mut n = 0u32;
                        if w != WAIT_OBJECT_0 {
                            // SAFETY: cancel and reap the pending connect before `ov` goes away.
                            unsafe {
                                CancelIoEx(inst.h.raw(), &raw const ov);
                                GetOverlappedResult(inst.h.raw(), &raw const ov, &raw mut n, 1);
                            }
                            return Ok(None);
                        }
                        // SAFETY: completed operation.
                        let ok = unsafe {
                            GetOverlappedResult(inst.h.raw(), &raw const ov, &raw mut n, 0)
                        };
                        ok != 0
                    }
                    e => return Err(os_err(e)),
                }
            };
            if connected {
                // Keep one listening instance at all times so the name never vanishes.
                self.next = Some(self.instance(false)?);
                return Ok(Some(inst));
            }
        }
    }
}

/// Connects to `endpoint`, waiting up to `timeout` while all instances are busy.
/// `NotFound` means no app serves the endpoint.
pub fn connect(endpoint: &str, timeout: Duration) -> io::Result<Pipe> {
    let name = wide(path(endpoint));
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: `name` is NUL-terminated; no security attributes; checked below.
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
            return Ok(Pipe { h: Handle::new(h)? });
        }
        // SAFETY: trivially safe thread-local read.
        let err = unsafe { GetLastError() };
        let left = deadline.saturating_duration_since(Instant::now());
        if err == ERROR_PIPE_BUSY && !left.is_zero() {
            let ms = u32::try_from(left.as_millis()).unwrap_or(1000).max(1);
            // SAFETY: NUL-terminated name.
            unsafe { WaitNamedPipeW(name.as_ptr(), ms) };
            continue;
        }
        return Err(os_err(err));
    }
}

/// Reads one `\n`-terminated line of at most `max` bytes before `deadline`.
pub fn read_line(pipe: &Pipe, max: usize, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let n = pipe.read_some(&mut buf, Some(left))?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.extend_from_slice(&buf[..n]);
        if let Some(i) = out.iter().position(|b| *b == b'\n') {
            out.truncate(i);
            return Ok(out);
        }
        if out.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(tag: &str) -> String {
        format!(
            "ddalgak-unit-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], "")
    }

    #[test]
    fn one_app_per_endpoint_lines_identity_and_stop() {
        let ep = endpoint("pipe");
        let mut l = Listener::bind(&ep).unwrap();
        assert_eq!(
            Listener::bind(&ep).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists,
            "a second server for the same endpoint is refused"
        );
        let ep2 = ep.clone();
        let client = std::thread::spawn(move || {
            let c = connect(&ep2, Duration::from_secs(5)).unwrap();
            assert_eq!(c.server_pid(), Some(std::process::id()));
            c.write_all_bytes(b"{\"hello\":1}\n").unwrap();
            read_line(&c, 1024, Instant::now() + Duration::from_secs(5)).unwrap()
        });
        let s = l.accept().unwrap().unwrap();
        let line = read_line(&s, 1024, Instant::now() + Duration::from_secs(5)).unwrap();
        assert_eq!(line, b"{\"hello\":1}");
        assert_eq!(s.client_pid(), Some(std::process::id()));
        assert_eq!(s.client_user_sid().unwrap(), user_sid().unwrap());
        s.write_all_bytes(b"pong\n").unwrap();
        assert_eq!(client.join().unwrap(), b"pong");
        drop(s);
        // A silent peer times out instead of hanging.
        let c = connect(&ep, Duration::from_secs(5)).unwrap();
        let s = l.accept().unwrap().unwrap();
        let t = Instant::now();
        let e = read_line(&s, 1024, Instant::now() + Duration::from_millis(200)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(3));
        drop(c);
        let stop = l.stopper();
        let waiter = std::thread::spawn(move || l.accept().map(|p| p.is_none()));
        std::thread::sleep(Duration::from_millis(100));
        stop.set();
        assert!(
            waiter.join().unwrap().unwrap(),
            "accept returns None on stop"
        );
        drop(s);
        assert_eq!(
            connect(&ep, Duration::from_millis(100)).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
