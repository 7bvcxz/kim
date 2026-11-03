//! Detached Host spawn (ADR 0003 §2, validated in Step 08 / ADR 0016).
//!
//! `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`, first with
//! `CREATE_BREAKAWAY_FROM_JOB` and, if a job of the launcher forbids that (`ERROR_ACCESS_DENIED`),
//! again without it (`job_breakaway = false`). `CREATE_NO_WINDOW` is redundant next to
//! `DETACHED_PROCESS` (Windows ignores it) but harmless; it is kept as ADR 0003 states.
//!
//! **No inherited handles.** The Host's stdin/stdout/stderr are a fresh NUL device handle, and
//! `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` restricts inheritance to exactly that handle. Without the
//! list, a launcher that was itself started with redirected output (an agent, a test, a CI job)
//! would leak its pipe into the Host, and its caller would wait for EOF until the Host exits.

use super::{AttrList, Handle, quote_arg, wide};
use std::ffi::c_void;
use std::io;
use std::path::Path;
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// The creation flags used for the Host, by name (reported by `host start --dry-run`).
pub const FLAG_NAMES: [&str; 4] = [
    "DETACHED_PROCESS",
    "CREATE_NEW_PROCESS_GROUP",
    "CREATE_NO_WINDOW",
    "CREATE_BREAKAWAY_FROM_JOB (falls back without it when a job forbids breakaway)",
];

/// A started Host. Whether breakaway was used reaches the Host as `--job-breakaway`.
#[derive(Debug)]
pub struct Spawned {
    pub pid: u32,
    pub process: Handle,
}

fn nul_inheritable() -> io::Result<Handle> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).expect("small"),
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let name = wide("NUL");
    // SAFETY: NUL-terminated name; `sa` is alive for the call; checked by Handle::new.
    Handle::new(unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &raw const sa,
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    })
}

/// Starts `exe args…` detached, in `cwd`. `args_for` builds the arguments given whether the
/// attempt uses job breakaway, so the Host can report it.
pub fn spawn_detached(
    exe: &Path,
    args_for: impl Fn(bool) -> Vec<String>,
    cwd: &Path,
) -> io::Result<Spawned> {
    match try_spawn(exe, &args_for(true), cwd, true) {
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            try_spawn(exe, &args_for(false), cwd, false)
        }
        other => other,
    }
}

fn try_spawn(exe: &Path, args: &[String], cwd: &Path, breakaway: bool) -> io::Result<Spawned> {
    let nul = nul_inheritable()?;
    let handles = [nul.raw()];
    let mut attrs = AttrList::new(1)?;
    // SAFETY: `handles` outlives CreateProcessW below; size matches the array.
    unsafe {
        attrs.set(
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            handles.as_ptr().cast::<c_void>(),
            size_of_val(&handles),
        )?;
    }
    let mut si = STARTUPINFOEXW::default();
    si.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).expect("small");
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = nul.raw();
    si.StartupInfo.hStdOutput = nul.raw();
    si.StartupInfo.hStdError = nul.raw();
    si.lpAttributeList = attrs.as_ptr();

    let mut line = quote_arg(&exe.to_string_lossy());
    for a in args {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    let mut line = wide(line);
    let app = wide(exe);
    let cwd = wide(cwd);
    let mut flags = DETACHED_PROCESS
        | CREATE_NEW_PROCESS_GROUP
        | CREATE_NO_WINDOW
        | EXTENDED_STARTUPINFO_PRESENT
        | CREATE_UNICODE_ENVIRONMENT;
    if breakaway {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: all strings are NUL-terminated and `line` is writable; `si`, the attribute list and
    // `handles` are alive; only the listed NUL handle is inheritable into the child.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            flags,
            std::ptr::null(),
            cwd.as_ptr(),
            &raw const si.StartupInfo,
            &raw mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    drop(Handle::new(pi.hThread));
    Ok(Spawned {
        pid: pi.dwProcessId,
        process: Handle::new(pi.hProcess)?,
    })
}
