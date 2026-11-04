//! Windows platform layer of the Host and its clients (ADR 0004, ADR 0005, ADR 0016, ADR 0019).
//!
//! Only this module tree calls Win32. Every `unsafe` block states why it is sound.

pub mod conpty;
pub mod console;
pub mod launch;
pub mod pipe;
pub mod spawn;

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, DeleteProcThreadAttributeList, GetCurrentProcess,
    GetCurrentProcessId, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, ReleaseMutex, SetEvent,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

/// Held by the unit tests that count this process's handles, and by the one that creates many
/// pseudo consoles (each keeps a handle on Windows 10), so they never measure each other.
#[cfg(test)]
pub(crate) fn handle_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

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

/// Restores normal Ctrl+C processing for this process, which every child inherits.
///
/// A process created with `CREATE_NEW_PROCESS_GROUP` starts with Ctrl+C *ignored*, and that flag
/// is inherited: without this call, Ctrl+C typed into a pane (sent as `0x03` through ConPTY)
/// would not interrupt the shell's programs (measured in Step 08; ADR 0016). Works without a
/// console.
pub fn enable_ctrl_c_for_children() -> bool {
    // SAFETY: a null handler with FALSE only clears the process's "ignore Ctrl+C" flag.
    unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) != 0 }
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

/// Open handles of this process (a local metric, ADR 0012).
pub fn handle_count() -> u32 {
    let mut n = 0u32;
    // SAFETY: pseudo handle of this process; `n` is writable.
    unsafe {
        windows_sys::Win32::System::Threading::GetProcessHandleCount(
            GetCurrentProcess(),
            &raw mut n,
        )
    };
    n
}

/// Memory of this process in bytes (a local metric, ADR 0012).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    pub working_set: u64,
    pub private: u64,
    /// The highest commit charge (private bytes) since the process started.
    pub peak_private: u64,
}

pub fn memory() -> Memory {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).expect("small"),
        ..Default::default()
    };
    // SAFETY: pseudo handle of this process; `c` is writable and its size is passed in `cb`
    // (the _EX layout starts with the basic one, as the API documents).
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        )
    };
    if ok == 0 {
        return Memory::default();
    }
    Memory {
        working_set: c.WorkingSetSize as u64,
        private: c.PrivateUsage as u64,
        peak_private: c.PeakPagefileUsage as u64,
    }
}

/// The string SID (`S-1-5-21-…`) of the user running this process.
pub fn user_sid() -> io::Result<String> {
    token_user_sid(&own_token(TOKEN_QUERY)?)
}

fn own_token(access: u32) -> io::Result<Handle> {
    let mut token = std::ptr::null_mut();
    // SAFETY: pseudo handle of the current process; `token` receives a new handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Handle::new(token)
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

fn open_query(pid: u32) -> io::Result<Handle> {
    // SAFETY: plain open with limited query rights; checked by Handle::new.
    Handle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) })
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

/// A held named mutex (the per-endpoint start lock of ADR 0003 §3). Released on drop.
///
/// A Win32 mutex belongs to the *thread* that acquired it: drop the lock on the same thread.
#[derive(Debug)]
pub struct NamedLock {
    h: Handle,
    /// The previous owner ended without releasing it (a starter crashed mid-start).
    pub abandoned: bool,
}

impl NamedLock {
    /// Waits up to `timeout` for the mutex `name` (e.g. `Local\…`, per Windows session).
    pub fn acquire(name: &str, timeout: Duration) -> io::Result<Self> {
        let w = wide(name);
        // SAFETY: NUL-terminated name; default security (the creator's default DACL).
        let h = Handle::new(unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) })?;
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(INFINITE - 1);
        // SAFETY: valid mutex handle.
        match unsafe { WaitForSingleObject(h.raw(), ms) } {
            WAIT_OBJECT_0 => Ok(Self {
                h,
                abandoned: false,
            }),
            WAIT_ABANDONED => Ok(Self { h, abandoned: true }),
            WAIT_TIMEOUT => Err(io::ErrorKind::TimedOut.into()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for NamedLock {
    fn drop(&mut self) {
        // SAFETY: this thread owns the mutex (acquired in `acquire`); the handle closes after.
        unsafe { ReleaseMutex(self.h.raw()) };
    }
}

/// A Unicode environment block (`K=V\0…\0\0`) for `CreateProcessW`.
#[derive(Debug, Clone)]
pub struct EnvBlock(Vec<u16>);

impl EnvBlock {
    /// The environment of a fresh logon of the current user (ADR 0003 §6): system and user
    /// variables from the registry, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, the user's `PATH`,
    /// and `HOME` only if the user defined it, without anything a launching terminal added.
    pub fn for_current_user() -> io::Result<Self> {
        let token = own_token(TOKEN_QUERY | TOKEN_DUPLICATE)?;
        let mut block: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid primary token with the documented access; FALSE = do not inherit the
        // current process's environment.
        if unsafe { CreateEnvironmentBlock(&raw mut block, token.raw(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let p = block.cast::<u16>();
        let mut len = 0usize;
        // SAFETY: a valid block ends with an empty string (two NULs in a row); every offset up
        // to it is in bounds.
        unsafe {
            while !(*p.add(len) == 0 && (len == 0 || *p.add(len - 1) == 0)) {
                len += 1;
            }
        }
        // SAFETY: `len + 1` units were just read.
        let mut v = unsafe { std::slice::from_raw_parts(p, len + 1) }.to_vec();
        // SAFETY: allocated by CreateEnvironmentBlock, destroyed exactly once.
        unsafe { DestroyEnvironmentBlock(block) };
        if v.len() < 2 {
            v = vec![0, 0];
        }
        Ok(Self(v))
    }

    /// This process's own environment, without the hidden per-drive `=C:=C:\…` entries (they
    /// describe this process's working directories, not a pane's). The fallback when the logon
    /// environment cannot be built.
    pub fn from_process() -> Self {
        Self::from_entries(
            std::env::vars_os()
                .filter(|(k, _)| !k.to_string_lossy().starts_with('='))
                .map(|(k, v)| {
                    let mut e: Vec<u16> = k.encode_wide().collect();
                    e.push(u16::from(b'='));
                    e.extend(v.encode_wide());
                    e
                })
                .collect(),
        )
    }

    /// A block of `K=V` entries (UTF-16, no terminators), sorted by name case-insensitively as
    /// Windows keeps environment blocks.
    fn from_entries(mut entries: Vec<Vec<u16>>) -> Self {
        entries.sort_by_cached_key(|e| crate::pty::var_key(&entry_name(e)));
        let mut v: Vec<u16> = Vec::new();
        for e in entries {
            v.extend(e);
            v.push(0);
        }
        if v.is_empty() {
            v.push(0);
        }
        v.push(0);
        Self(v)
    }

    /// The raw `K=V` entries.
    fn entries(&self) -> impl Iterator<Item = &[u16]> + '_ {
        self.0.split(|&c| c == 0).filter(|s| !s.is_empty())
    }

    /// This block with `overrides` applied (in order: an unset removes, a set replaces or adds;
    /// names compare case-insensitively) and `host_vars` set last, so they always win. Values
    /// are copied as UTF-16, never through a lossy conversion. Hidden `=C:` entries are dropped.
    pub fn with_overrides(
        &self,
        overrides: &crate::pty::EnvOverrides,
        host_vars: &[(&str, &str)],
    ) -> Self {
        use crate::pty::same_var;
        let replaced = |name: &str| {
            name.starts_with('=')
                || overrides.vars.iter().any(|(k, _)| same_var(k, name))
                || host_vars.iter().any(|(k, _)| same_var(k, name))
        };
        let mut entries: Vec<Vec<u16>> = self
            .entries()
            .filter(|e| !replaced(&entry_name(e)))
            .map(<[u16]>::to_vec)
            .collect();
        let entry = |k: &str, v: &str| -> Vec<u16> { format!("{k}={v}").encode_utf16().collect() };
        for (k, v) in &overrides.vars {
            if let Some(v) = v {
                entries.push(entry(k, v));
            }
        }
        for (k, v) in host_vars {
            entries.push(entry(k, v));
        }
        Self::from_entries(entries)
    }

    /// `(name, value)` pairs.
    pub fn vars(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.entries().filter_map(|s| {
            let text = String::from_utf16_lossy(s);
            // `=C:=C:\…` entries start with '='; their name is up to the second '='.
            let skip = text.chars().next()?.len_utf8();
            let at = text[skip..].find('=')? + skip;
            Some((text[..at].to_string(), text[at + 1..].to_string()))
        })
    }

    /// A variable, case-insensitively as Windows does.
    pub fn get(&self, name: &str) -> Option<String> {
        self.vars()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    pub fn as_ptr(&self) -> *const c_void {
        self.0.as_ptr().cast()
    }
}

/// The name of a `K=V` entry (a leading `=` belongs to the name, as in `=C:=C:\…`).
fn entry_name(e: &[u16]) -> String {
    let eq = u16::from(b'=');
    let end = e
        .iter()
        .skip(1)
        .position(|&c| c == eq)
        .map_or(e.len(), |i| i + 1);
    String::from_utf16_lossy(&e[..end])
}

/// The limits of the job the current process belongs to (its immediate job when nested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobLimits {
    pub kill_on_close: bool,
    pub breakaway_ok: bool,
    pub silent_breakaway_ok: bool,
}

/// `None` when the process is in no job (or its job cannot be queried).
pub fn current_job_limits() -> Option<JobLimits> {
    if !current_process_in_job() {
        return None;
    }
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: null job = the job of the calling process; `info` is writable, of the size passed.
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut info).cast::<c_void>(),
            u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).expect("small"),
            std::ptr::null_mut(),
        )
    };
    let f = info.BasicLimitInformation.LimitFlags;
    (ok != 0).then_some(JobLimits {
        kill_on_close: f & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0,
        breakaway_ok: f & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0,
        silent_breakaway_ok: f & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK != 0,
    })
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

    /// What the OS parser (`CommandLineToArgvW`, the rules of the MSVC runtime) reads back.
    fn os_argv(line: &str) -> Vec<String> {
        use windows_sys::Win32::UI::Shell::CommandLineToArgvW;
        let w = wide(line);
        let mut n = 0i32;
        // SAFETY: NUL-terminated line; on success the result is one LocalAlloc'ed block of `n`
        // NUL-terminated strings, freed below.
        let argv = unsafe { CommandLineToArgvW(w.as_ptr(), &raw mut n) };
        assert!(!argv.is_null(), "CommandLineToArgvW failed for {line:?}");
        let out = (0..usize::try_from(n).unwrap())
            // SAFETY: `argv` holds `n` valid string pointers.
            .map(|i| unsafe { from_wide_ptr(*argv.add(i)) })
            .collect();
        // SAFETY: allocated by CommandLineToArgvW, freed exactly once.
        unsafe { LocalFree(argv as HLOCAL) };
        out
    }

    #[test]
    fn quoting_round_trips_through_the_os_parser() {
        let tricky = [
            "",
            " ",
            "a b",
            "\t",
            "\"",
            "\"\"",
            "\\",
            "\\\\",
            "a\\",
            "a\\\\",
            "a\\\"b",
            "C:\\Program Files\\x\\",
            "\\\\server\\share\\",
            "say \"hi\" there",
            "--flag=\"quoted value\"",
            "100%",
            "^&|<>()",
            "한글 인자",
            "emoji 🙂 \"x\"",
            "trailing space ",
            "multi\nline",
            "=C:=C:\\",
        ];
        let line = std::iter::once("prog.exe".to_string())
            .chain(tricky.iter().map(|a| quote_arg(a)))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(&os_argv(&line)[1..], tricky.map(String::from).as_slice());
        // Generated arguments over the characters that matter, from a fixed seed.
        let alphabet = ['a', ' ', '\t', '"', '\\', '한', '=', '-'];
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..500 {
            let args: Vec<String> = (0..(next() % 4 + 1))
                .map(|_| {
                    (0..next() % 9)
                        .map(|_| alphabet[usize::try_from(next() % 8).unwrap()])
                        .collect()
                })
                .collect();
            let line = std::iter::once("p".to_string())
                .chain(args.iter().map(|a| quote_arg(a)))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(os_argv(&line)[1..], args[..], "line {line:?}");
        }
    }

    #[test]
    fn overrides_replace_remove_and_add_case_insensitively() {
        use crate::pty::EnvOverrides;
        let base = EnvBlock::from_entries(
            [
                "Path=C:\\a",
                "HOME=C:\\h",
                "=C:=C:\\x",
                "Keep=1",
                "SUBAK_PANE_ID=forged",
            ]
            .iter()
            .map(|e| e.encode_utf16().collect())
            .collect(),
        );
        let o = EnvOverrides {
            vars: vec![
                ("PATH".into(), Some("C:\\b".into())),
                ("home".into(), None),
                ("GREETING".into(), Some("안녕하세요 =x".into())),
            ],
        };
        let env = base.with_overrides(&o, &[("SUBAK_PANE_ID", "p-0123456789ab")]);
        let vars: Vec<(String, String)> = env.vars().collect();
        assert_eq!(
            vars,
            [
                ("GREETING".to_string(), "안녕하세요 =x".to_string()),
                ("Keep".into(), "1".into()),
                ("PATH".into(), "C:\\b".into()),
                ("SUBAK_PANE_ID".into(), "p-0123456789ab".into()),
            ],
            "sorted case-insensitively; replaced, removed, added; no hidden `=C:` entry; the Host's variable wins"
        );
        assert_eq!(env.get("path").as_deref(), Some("C:\\b"));
        assert_eq!(env.get("HOME"), None);
        // Without overrides the block is the base (minus hidden entries), still terminated.
        let same = base.with_overrides(&EnvOverrides::default(), &[]);
        assert_eq!(same.vars().count(), 4);
        assert_eq!(same.0[same.0.len() - 2..], [0, 0]);
        let empty = EnvBlock::from_entries(Vec::new());
        assert_eq!(empty.0, [0, 0], "an empty block is two NULs");
        // The process's own environment is a usable fallback without hidden entries.
        let own = EnvBlock::from_process();
        assert!(own.get("SystemRoot").is_some());
        assert!(own.vars().all(|(k, _)| !k.starts_with('=')));
    }

    #[test]
    fn identity_helpers_work_on_this_machine() {
        let sid = user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let _ = session_id();
        let me = open_process(current_pid()).expect("open self");
        assert_eq!(exit_code(me.raw()), None, "we are running");
        assert_eq!(process_user_sid(current_pid()).unwrap(), sid);
        assert_eq!(
            process_image(current_pid()).unwrap(),
            std::env::current_exe().unwrap()
        );
        assert!(process_user_sid(u32::MAX - 3).is_err(), "no such process");
    }

    #[test]
    fn the_user_environment_comes_from_the_logon_not_from_this_process() {
        // A variable only this process has; the block must not carry it.
        let marker = format!("SUBAK_ENV_LEAK_{}", current_pid());
        // SAFETY: set_var is unsafe in edition 2024 because of concurrent getenv; the name is
        // unique to this test and only read below.
        unsafe { std::env::set_var(&marker, "leak") };
        let env = EnvBlock::for_current_user().unwrap();
        assert_eq!(
            env.get(&marker),
            None,
            "the launcher's variables are not inherited"
        );
        let profile = env.get("USERPROFILE").expect("USERPROFILE");
        assert!(std::path::Path::new(&profile).is_dir(), "{profile}");
        assert!(env.get("path").is_some(), "case-insensitive lookup");
        assert!(env.get("LOCALAPPDATA").is_some());
        assert!(env.vars().all(|(k, _)| !k.is_empty()));
    }

    #[test]
    fn named_lock_is_exclusive_and_survives_an_abandoning_owner() {
        let name = format!("Local\\subak-unit-lock-{}", current_pid());
        let first = NamedLock::acquire(&name, Duration::from_secs(1)).unwrap();
        assert!(!first.abandoned);
        let other = {
            let name = name.clone();
            std::thread::spawn(move || {
                NamedLock::acquire(&name, Duration::from_millis(100)).map(|_| ())
            })
        };
        assert_eq!(
            other.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut,
            "a second starter waits"
        );
        drop(first);
        // A starter thread that ends while holding the lock abandons it; the next one gets it.
        let quitter = {
            let name = name.clone();
            std::thread::spawn(move || {
                std::mem::forget(NamedLock::acquire(&name, Duration::from_secs(1)).unwrap());
            })
        };
        quitter.join().unwrap();
        let next = NamedLock::acquire(&name, Duration::from_secs(1)).unwrap();
        assert!(
            next.abandoned,
            "an abandoned start lock is reported, not a deadlock"
        );
    }

    #[test]
    fn job_limits_match_membership() {
        assert_eq!(current_job_limits().is_some(), current_process_in_job());
    }
}
