//! Windows platform layer for the Step 08 Host prototype (ADR 0004, ADR 0005, ADR 0016).
//!
//! Only this module tree calls Win32. Every `unsafe` block states why it is sound.

pub mod conpty;
pub mod console;
pub mod pipe;
pub mod spawn;

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateEventW, DeleteProcThreadAttributeList, GetCurrentProcess, GetCurrentProcessId,
    GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    SetEvent, UpdateProcThreadAttribute, WaitForSingleObject,
};

/// An owned kernel handle, closed exactly once on drop.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide token, not thread-affine; the kernel objects used
// here (pipes, processes, jobs, events) may be used from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; shared use only passes the value to thread-safe kernel calls.
unsafe impl Sync for Handle {}

impl Handle {
    /// Takes ownership of `h`; null and `INVALID_HANDLE_VALUE` become the last OS error.
    pub fn new(h: HANDLE) -> io::Result<Self> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(h))
        }
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid handle owned by this value and closed only here.
        unsafe { CloseHandle(self.0) };
    }
}

/// A manual-reset event.
#[derive(Debug)]
pub struct Event(Handle);

impl Event {
    pub fn new() -> io::Result<Self> {
        // SAFETY: anonymous manual-reset event, initially non-signaled; checked by Handle::new.
        Handle::new(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }).map(Self)
    }

    pub fn set(&self) {
        // SAFETY: `self.0` is a valid event handle.
        unsafe { SetEvent(self.0.raw()) };
    }

    pub fn raw(&self) -> HANDLE {
        self.0.raw()
    }
}

/// NUL-terminated UTF-16.
pub fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` must point to a valid NUL-terminated UTF-16 string.
unsafe fn from_wide_ptr(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a terminator, so every offset up to it is in bounds.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just read successfully.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// Waits for a waitable handle; `true` when it is signaled.
pub fn wait(h: HANDLE, timeout: Option<Duration>) -> bool {
    let ms = timeout.map_or(INFINITE, |d| {
        u32::try_from(d.as_millis()).unwrap_or(INFINITE - 1)
    });
    // SAFETY: `h` is a valid waitable handle owned by the caller for the duration of the call.
    unsafe { WaitForSingleObject(h, ms) == WAIT_OBJECT_0 }
}

/// The exit code of a process that has exited, `None` while it runs.
pub fn exit_code(process: HANDLE) -> Option<u32> {
    if !wait(process, Some(Duration::ZERO)) {
        return None;
    }
    let mut code = 0u32;
    // SAFETY: valid process handle with query access; `code` is a writable u32.
    (unsafe { GetExitCodeProcess(process, &raw mut code) } != 0).then_some(code)
}

/// Opens a process for waiting and querying; `None` when it is gone or inaccessible.
pub fn open_process(pid: u32) -> Option<Handle> {
    // SAFETY: plain open with limited rights; the result is checked by Handle::new.
    Handle::new(unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        )
    })
    .ok()
}

/// Whether the current process belongs to any job object.
pub fn current_process_in_job() -> bool {
    let mut result = 0;
    // SAFETY: pseudo handle of the current process; null job = "any job"; `result` is writable.
    let ok = unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &raw mut result) };
    ok != 0 && result != 0
}

/// The Windows (Terminal Services) session id of the current process.
pub fn session_id() -> u32 {
    let mut id = 0u32;
    // SAFETY: `id` is a writable u32; the pid is our own.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut id) };
    id
}

pub fn current_pid() -> u32 {
    std::process::id()
}

/// The string SID (`S-1-5-21-…`) of the user running this process.
pub fn user_sid() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo handle of the current process; `token` receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle::new(token)?;
    let mut len = 0u32;
    // SAFETY: size query with a null buffer; fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &raw mut len,
        )
    };
    // u64 elements keep the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds at least `len` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            TokenUser,
            buf.as_mut_ptr().cast::<c_void>(),
            len,
            &raw mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success the buffer starts with an initialized, aligned TOKEN_USER.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    let mut text: *mut u16 = std::ptr::null_mut();
    // SAFETY: the SID pointer comes from the token buffer, which is alive here.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `text` is a LocalAlloc'ed NUL-terminated string, freed right after.
    let sid = unsafe { from_wide_ptr(text) };
    // SAFETY: `text` was allocated by ConvertSidToStringSidW with LocalAlloc.
    unsafe { LocalFree(text as HLOCAL) };
    Ok(sid)
}

/// A self-relative security descriptor allocated by the system, freed on drop.
#[derive(Debug)]
pub struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: an immutable, heap-allocated descriptor that is only read by kernel calls.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above.
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `text` is NUL-terminated; `sd` receives a LocalAlloc'ed descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &raw mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(sd))
    }

    pub fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc).
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// A `PROC_THREAD_ATTRIBUTE_LIST` with room for `n` attributes. Values passed to [`set`] must
/// outlive the `CreateProcessW` call that uses the list.
///
/// [`set`]: AttrList::set
#[derive(Debug)]
pub struct AttrList {
    buf: Vec<u64>,
}

impl AttrList {
    pub fn new(n: u32) -> io::Result<Self> {
        let mut size = 0usize;
        // SAFETY: size query with a null list; fails with ERROR_INSUFFICIENT_BUFFER, sets `size`.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), n, 0, &raw mut size) };
        let mut buf = vec![0u64; size.div_ceil(8).max(1)];
        // SAFETY: `buf` has at least `size` writable, 8-byte aligned bytes.
        if unsafe {
            InitializeProcThreadAttributeList(buf.as_mut_ptr().cast(), n, 0, &raw mut size)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { buf })
    }

    pub fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buf.as_mut_ptr().cast()
    }

    /// # Safety
    /// `value` must point to `size` readable bytes that stay valid until the process is created
    /// (or, for `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, be the `HPCON` value itself).
    pub unsafe fn set(
        &mut self,
        attribute: u32,
        value: *const c_void,
        size: usize,
    ) -> io::Result<()> {
        // SAFETY: the list was initialized in `new`; the caller guarantees `value`.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.as_ptr(),
                0,
                attribute as usize,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        // SAFETY: initialized in `new`, deleted exactly once.
        unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
    }
}

/// Quotes one argument for `CreateProcessW` (MSVCRT/`CommandLineToArgvW` rules).
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_commandlinetoargvw() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("cmd.exe /d"), "\"cmd.exe /d\"");
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
    }

    #[test]
    fn identity_helpers_work_on_this_machine() {
        let sid = user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let _ = session_id();
        let me = open_process(current_pid()).expect("open self");
        assert_eq!(exit_code(me.raw()), None, "we are running");
    }
}
