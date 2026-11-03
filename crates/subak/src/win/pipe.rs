//! Prototype named-pipe transport (ADR 0005; hardened in Step 09).
//!
//! - **Overlapped I/O on both ends.** On a handle opened for synchronous I/O, Windows serializes
//!   operations per file object: a blocking `ReadFile` in one thread blocks a `WriteFile` in
//!   another, which deadlocks a duplex terminal stream. Every read and write here is overlapped
//!   with its own event, so one reader and one writer thread can share a connection.
//! - The pipe is local only (`PIPE_REJECT_REMOTE_CLIENTS`), its first instance is created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE` (exactly one Host per endpoint), and it carries an
//!   owner-only DACL (deny NETWORK, allow the current user). Step 09 adds the peer identity
//!   checks and the final pipe name of ADR 0005.
//! - Clients connect with `SECURITY_IDENTIFICATION` so a server can never impersonate them.

use super::{Event, Handle, SecurityDescriptor, user_sid, wide};
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    INFINITE, WaitForMultipleObjects, WaitForSingleObject,
};

const BUFFER: u32 = 64 * 1024;

/// `\\.\pipe\<endpoint>`.
pub fn path(endpoint: &str) -> String {
    format!(r"\\.\pipe\{endpoint}")
}

/// The default prototype endpoint: per user SID and Windows session, marked `proto` so it can
/// never collide with Step 09's real pipe name.
pub fn default_endpoint() -> io::Result<String> {
    Ok(format!(
        "mouv-subak-proto-{}-s{}",
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
                unsafe {
                    CancelIoEx(self.h.raw(), &raw const ov);
                    GetOverlappedResult(self.h.raw(), &raw const ov, &raw mut n, 1);
                }
                return Err(io::ErrorKind::TimedOut.into());
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
}

fn is_closed(e: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA)
    )
}

impl Read for &Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf, None)
    }
}

impl Write for &Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all_bytes(buf).map(|()| buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A `Read` adapter that fails with `TimedOut` after a deadline (handshake limits).
#[derive(Debug)]
pub struct Deadline<'a> {
    pub pipe: &'a Pipe,
    pub until: Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.pipe.read_some(buf, Some(left))
    }
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
        // inherited ACEs. Peer identity checks follow in Step 09.
        let sd =
            SecurityDescriptor::from_sddl(&format!("D:P(D;;GA;;;NU)(A;;GA;;;{})", user_sid()?))?;
        let mut l = Self {
            name: wide(path(endpoint)),
            sd,
            next: None,
            stop: Arc::new(Event::new()?),
        };
        l.next = Some(l.instance(true).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
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
/// `NotFound` means no Host serves the endpoint.
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    fn endpoint(tag: &str) -> String {
        format!(
            "subak-unit-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        )
        .replace(['(', ')'], "")
    }

    #[test]
    fn duplex_overlapped_io_does_not_deadlock() {
        let ep = endpoint("duplex");
        let mut l = Listener::bind(&ep).unwrap();
        let client = std::thread::spawn(move || connect(&ep, Duration::from_secs(5)).unwrap());
        let server = l.accept().unwrap().unwrap();
        let client = Arc::new(client.join().unwrap());
        assert_eq!(server.client_pid(), Some(std::process::id()));
        // A blocked read on the client must not block a write on the same handle.
        let reader = std::thread::spawn({
            let c = Arc::clone(&client);
            move || {
                let mut b = [0u8; 5];
                (&*c).read_exact(&mut b).unwrap();
                b
            }
        });
        std::thread::sleep(Duration::from_millis(100));
        client.write_all_bytes(b"ping!").unwrap();
        let mut b = [0u8; 5];
        (&server).read_exact(&mut b).unwrap();
        assert_eq!(&b, b"ping!");
        server.write_all_bytes(b"pong!").unwrap();
        assert_eq!(&reader.join().unwrap(), b"pong!");
        drop(server);
        let mut rest = [0u8; 1];
        assert_eq!(
            client.read_some(&mut rest, None).unwrap(),
            0,
            "EOF after server close"
        );
    }

    #[test]
    fn one_host_per_endpoint_timeouts_and_stop() {
        let ep = endpoint("single");
        let mut l = Listener::bind(&ep).unwrap();
        let second = Listener::bind(&ep).unwrap_err();
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);
        let c = connect(&ep, Duration::from_secs(5)).unwrap();
        let s = l.accept().unwrap().unwrap();
        let mut b = [0u8; 1];
        let t = Instant::now();
        let e = Deadline {
            pipe: &s,
            until: Instant::now() + Duration::from_millis(200),
        }
        .read(&mut b)
        .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(3));
        drop(c);
        let stop = l.stopper();
        let t = std::thread::spawn(move || l.accept().map(|p| p.is_none()));
        std::thread::sleep(Duration::from_millis(100));
        stop.set();
        assert!(t.join().unwrap().unwrap(), "accept returns None on stop");
        drop(s);
        let gone = connect(&ep, Duration::from_millis(100)).unwrap_err();
        assert_eq!(
            gone.kind(),
            io::ErrorKind::NotFound,
            "pipe name removed: {gone}"
        );
    }

    #[test]
    fn pipe_dacl_is_owner_only_and_denies_network() {
        let ep = endpoint("dacl");
        let l = Listener::bind(&ep).unwrap();
        let inst = l.next.as_ref().unwrap();
        let mut sd = std::ptr::null_mut();
        // SAFETY: valid handle; `sd` receives a LocalAlloc'ed descriptor (leaked in a test).
        let rc = unsafe {
            GetSecurityInfo(
                inst.h.raw(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut sd,
            )
        };
        assert_eq!(rc, 0);
        let to_sddl = |sd: PSECURITY_DESCRIPTOR, info| {
            let mut text: *mut u16 = std::ptr::null_mut();
            // SAFETY: `sd` is valid; `text` receives a LocalAlloc'ed string (leaked in a test).
            let ok = unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    sd,
                    SDDL_REVISION_1,
                    info,
                    &raw mut text,
                    std::ptr::null_mut(),
                )
            };
            assert_ne!(ok, 0);
            // SAFETY: NUL-terminated string from the call above.
            unsafe { super::super::from_wide_ptr(text) }
        };
        let sddl = to_sddl(sd, DACL_SECURITY_INFORMATION);
        // SDDL prints well-known SIDs as aliases (the built-in Administrator, e.g. on CI runners,
        // is `LA`), so the expected trustee goes through the same conversion.
        let owner = SecurityDescriptor::from_sddl(&format!("O:{}", user_sid().unwrap())).unwrap();
        let me = to_sddl(owner.as_ptr(), OWNER_SECURITY_INFORMATION);
        let me = me.strip_prefix("O:").unwrap();
        assert!(sddl.starts_with("D:P"), "{sddl}");
        assert!(
            sddl.contains(";;;NU)") && sddl.contains("(D;"),
            "denies NETWORK: {sddl}"
        );
        assert!(
            sddl.contains(&format!(";;;{me})")),
            "allows the user ({me}): {sddl}"
        );
        assert_eq!(
            sddl.matches("(A;").count(),
            1,
            "exactly one allow ACE: {sddl}"
        );
    }
}
