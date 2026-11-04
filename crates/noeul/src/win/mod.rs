//! Win32 layer of Noeul (ADR 0023). Only this module tree calls Win32; every `unsafe` block says
//! why it is sound. The identity helpers and the named-pipe transport are adapted from Subak's
//! (`crates/subak/src/win`, ADR 0005, ADR 0019); Noeul does not depend on the Subak crate.

pub mod client;
pub mod clipboard;
pub mod gdi;
pub mod overlay;
pub mod pinwnd;
pub mod pipe;
pub mod runtime;
pub mod spawn;
pub mod tray;

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, SetEvent,
};

/// An owned kernel handle, closed exactly once on drop.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide token, not thread-affine; the objects used here
// (pipes, processes, events, change notifications) may be used from any thread.
unsafe impl Send for Handle {}
// SAFETY: as above; shared use only passes the value to thread-safe kernel calls.
unsafe impl Sync for Handle {}

impl Handle {
    /// Takes ownership of `h`; null and `INVALID_HANDLE_VALUE` become the last OS error.
    pub fn new(h: HANDLE) -> io::Result<Handle> {
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Handle(h))
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
    pub fn new() -> io::Result<Event> {
        // SAFETY: anonymous manual-reset event, initially non-signaled; checked by Handle::new.
        Handle::new(unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) }).map(Event)
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

/// The Windows (Terminal Services) session id of the current process.
pub fn session_id() -> u32 {
    let mut id = 0u32;
    // SAFETY: `id` is a writable u32; the pid is our own.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut id) };
    id
}

/// The string SID (`S-1-5-21-…`) of the user running this process.
pub fn user_sid() -> io::Result<String> {
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo handle of the current process; `token` receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    token_user_sid(&Handle::new(token)?)
}

fn open_query(pid: u32) -> io::Result<Handle> {
    // SAFETY: plain open with limited query rights; checked by Handle::new.
    Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
}

/// The string SID of the user that owns process `pid` (needs query access to its token).
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    let process = open_query(pid)?;
    let mut token = std::ptr::null_mut();
    // SAFETY: valid process handle; `token` receives a new handle.
    if unsafe { OpenProcessToken(process.raw(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    token_user_sid(&Handle::new(token)?)
}

/// The full path of the executable of process `pid`.
pub fn process_image(pid: u32) -> io::Result<std::path::PathBuf> {
    let process = open_query(pid)?;
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).expect("small");
    // SAFETY: `buf` has `len` writable UTF-16 units; 0 = Win32 path format.
    if unsafe { QueryFullProcessImageNameW(process.raw(), 0, buf.as_mut_ptr(), &raw mut len) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(std::path::PathBuf::from(String::from_utf16_lossy(&buf)))
}

/// The string SID of the user of `token` (needs `TOKEN_QUERY`).
pub(crate) fn token_user_sid(token: &Handle) -> io::Result<String> {
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
    pub fn from_sddl(sddl: &str) -> io::Result<SecurityDescriptor> {
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
        Ok(SecurityDescriptor(sd))
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

/// Waits up to `timeout` for process `pid` to end; `true` when it has ended (or is gone).
pub fn wait_exit(pid: u32, timeout: std::time::Duration) -> bool {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: plain open with synchronize rights; checked by Handle::new.
    let Ok(h) = Handle::new(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) }) else {
        return true;
    };
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: a valid process handle.
    unsafe { WaitForSingleObject(h.raw(), ms) == WAIT_OBJECT_0 }
}

/// Makes this process per-monitor DPI aware (v2), so every coordinate is a device pixel
/// (NOEUL-FR-090). Must run before any window is created; harmless when already set.
pub fn per_monitor_dpi_awareness() -> bool {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: a documented process-wide setting with a constant argument.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_helpers_work_on_this_machine() {
        let sid = user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let me = std::process::id();
        assert_eq!(process_user_sid(me).unwrap(), sid);
        assert_eq!(process_image(me).unwrap(), std::env::current_exe().unwrap());
        assert!(process_user_sid(u32::MAX - 3).is_err(), "no such process");
        let _ = session_id();
    }
}
