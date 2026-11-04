//! Windows platform layer (ADR 0024). Only this module tree calls Win32; every `unsafe` block
//! states why it is sound. The helpers follow Subak's (ADR 0019) without depending on Subak:
//! no tool depends on another tool's crate (`KIM-FR-001`).

pub mod context;
pub mod hook;
pub mod host;
pub mod inject;
pub mod instance;
pub mod pipe;
pub mod startup;
pub mod tray;
pub mod watch;

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::{
    CloseHandle, FILETIME, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, PSECURITY_DESCRIPTOR,
    TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenElevation,
    TokenIntegrityLevel, TokenUser,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetProcessTimes, OpenProcess,
    OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, SetEvent,
};

/// An owned kernel handle, closed exactly once on drop.
#[derive(Debug)]
pub struct Handle(HANDLE);

// SAFETY: a kernel handle is a process-wide token, not thread-affine; the objects used here
// (pipes, processes, events, mutexes, tokens) may be used from any thread.
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
pub(crate) unsafe fn from_wide_ptr(p: *const u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a terminator, so every offset up to it is in bounds.
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just read successfully.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// The Windows (Terminal Services) session id of this process.
pub fn session_id() -> u32 {
    let mut id = 0u32;
    // SAFETY: `id` is a writable u32; the pid is our own.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &raw mut id) };
    id
}

fn own_token() -> io::Result<Handle> {
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo handle of the current process; `token` receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Handle::new(token)
}

fn open_query(pid: u32) -> io::Result<Handle> {
    // SAFETY: plain open with limited query rights; checked by Handle::new.
    Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
}

fn process_token(pid: u32) -> io::Result<Handle> {
    let process = open_query(pid)?;
    let mut token = std::ptr::null_mut();
    // SAFETY: valid process handle; `token` receives a new handle.
    if unsafe { OpenProcessToken(process.raw(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Handle::new(token)
}

/// Reads a variable-size token information class into an aligned buffer.
fn token_info(token: &Handle, class: i32) -> io::Result<Vec<u64>> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer; fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe { GetTokenInformation(token.raw(), class, std::ptr::null_mut(), 0, &raw mut len) };
    // u64 elements keep the buffer aligned for the token structures.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buf` holds at least `len` writable bytes.
    if unsafe {
        GetTokenInformation(
            token.raw(),
            class,
            buf.as_mut_ptr().cast::<c_void>(),
            len,
            &raw mut len,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

/// The string SID (`S-1-5-21-…`) of the user of `token`.
pub(crate) fn token_user_sid(token: &Handle) -> io::Result<String> {
    let buf = token_info(token, TokenUser)?;
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

/// The string SID of the user running this process.
pub fn user_sid() -> io::Result<String> {
    token_user_sid(&own_token()?)
}

/// The string SID of the user that owns process `pid`.
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    token_user_sid(&process_token(pid)?)
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

/// The mandatory integrity level RID of a token (`0x2000` medium, `0x3000` high, …).
fn token_integrity(token: &Handle) -> io::Result<u32> {
    let buf = token_info(token, TokenIntegrityLevel)?;
    // SAFETY: on success the buffer starts with an initialized, aligned TOKEN_MANDATORY_LABEL
    // whose SID points into the same buffer.
    let label = unsafe { &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
    let sid = label.Label.Sid;
    // SAFETY: `sid` is a valid SID inside `buf`; the count is read before indexing.
    let count = unsafe { *GetSidSubAuthorityCount(sid) };
    if count == 0 {
        return Err(io::Error::other("integrity SID without sub-authority"));
    }
    // SAFETY: index `count - 1` is within the SID's sub-authorities.
    Ok(unsafe { *GetSidSubAuthority(sid, u32::from(count - 1)) })
}

/// This process's integrity level RID.
pub fn own_integrity() -> io::Result<u32> {
    token_integrity(&own_token()?)
}

/// The integrity level RID of process `pid`; an error when it cannot be read (a protected or
/// elevated process may refuse even limited query access).
pub fn process_integrity(pid: u32) -> io::Result<u32> {
    token_integrity(&process_token(pid)?)
}

/// Whether this process runs elevated (an administrator's full token). Ddalgak refuses to turn
/// its engine on then (DDALGAK_SAFETY §3).
pub fn is_elevated() -> bool {
    let Ok(token) = own_token() else {
        return false;
    };
    let Ok(buf) = token_info(&token, TokenElevation) else {
        return false;
    };
    // SAFETY: on success the buffer starts with an initialized TOKEN_ELEVATION.
    let e = unsafe { &*buf.as_ptr().cast::<TOKEN_ELEVATION>() };
    e.TokenIsElevated != 0
}

/// Whether this session has an interactive input desktop named `Default` that this process
/// may read: `false` while the workstation is locked, on the secure desktop (UAC, Ctrl+Alt+Del)
/// or in a non-interactive session. Hooks then see nothing and `SendInput` is refused, so
/// tests report **skipped (locked)** instead of failing or faking a pass.
pub fn input_desktop_available() -> bool {
    // SAFETY: plain open with read rights; the handle is closed below.
    let desk = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desk.is_null() {
        return false;
    }
    let mut name = [0u16; 64];
    let mut needed = 0u32;
    // SAFETY: `desk` is valid; `name` has the given number of writable bytes.
    let ok = unsafe {
        GetUserObjectInformationW(
            desk.cast(),
            UOI_NAME,
            name.as_mut_ptr().cast(),
            u32::try_from(size_of_val(&name)).expect("small"),
            &raw mut needed,
        )
    } != 0;
    // SAFETY: closes the desktop handle opened above.
    unsafe { CloseDesktop(desk) };
    let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    ok && String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("Default")
}

fn filetime_100ns(f: &FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

/// User plus kernel CPU time of this process, in microseconds (idle benchmark, ADR 0012).
pub fn process_cpu_us() -> u64 {
    let mut c = FILETIME::default();
    let mut e = FILETIME::default();
    let mut k = FILETIME::default();
    let mut u = FILETIME::default();
    // SAFETY: pseudo handle of this process; four writable FILETIMEs.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut c,
            &raw mut e,
            &raw mut k,
            &raw mut u,
        )
    };
    if ok == 0 {
        return 0;
    }
    (filetime_100ns(&k) + filetime_100ns(&u)) / 10
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_integrity_and_cpu_time_of_this_process() {
        let sid = user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        assert_eq!(process_user_sid(std::process::id()).unwrap(), sid);
        let il = own_integrity().unwrap();
        // Medium (0x2000) for a normal user, high (0x3000) when elevated (CI runners).
        assert!(il >= 0x2000, "{il:#x}");
        assert_eq!(process_integrity(std::process::id()).unwrap(), il);
        assert_eq!(is_elevated(), il >= 0x3000);
        assert!(process_image(std::process::id()).unwrap().is_file());
        let a = process_cpu_us();
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        assert!(x > 0);
        assert!(process_cpu_us() >= a);
        // Locked or not, the probe answers without failing.
        let _ = input_desktop_available();
    }
}
