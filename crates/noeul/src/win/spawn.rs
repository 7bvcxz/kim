//! Starting the resident instance detached from the terminal (`noeul start`, NOEUL-FR-002).
//!
//! The instance is this same executable running `noeul run`, created with `CreateProcessW`:
//!
//! - `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`: no console (none is ever shown), and closing
//!   the terminal or pressing Ctrl+C there never ends it;
//! - **no inherited handles** (`bInheritHandles = FALSE`, no standard handles): a launcher that
//!   was itself started with redirected output (a script, an agent, a test) would otherwise leak
//!   its pipe into the long-lived instance, and its caller would wait for end of file until
//!   Noeul quits (the same lesson as Subak's Host, ADR 0016);
//! - `CREATE_BREAKAWAY_FROM_JOB` first, and without it when the launcher's job forbids breakaway;
//! - the user profile as working folder, so the instance never holds the caller's folder open.

use super::{Handle, wide};
use std::io;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{STILL_ACTIVE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS,
    GetExitCodeProcess, PROCESS_INFORMATION, STARTUPINFOW, WaitForSingleObject,
};

/// A started instance.
#[derive(Debug)]
pub struct Spawned {
    pub pid: u32,
    process: Handle,
}

impl Spawned {
    /// The exit code once the process has ended, `None` while it runs.
    pub fn try_wait(&self) -> Option<u32> {
        // SAFETY: a process handle owned by `self`; a zero timeout only checks the state.
        if unsafe { WaitForSingleObject(self.process.raw(), 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = STILL_ACTIVE as u32;
        // SAFETY: as above; `code` is writable.
        unsafe { GetExitCodeProcess(self.process.raw(), &raw mut code) };
        Some(code)
    }
}

/// Quotes one argument for `CreateProcessW` (`CommandLineToArgvW` rules).
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

fn cwd() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
        })
        .unwrap_or_else(std::env::temp_dir)
}

fn create(exe: &std::path::Path, args: &[String], flags: u32) -> io::Result<Spawned> {
    let mut line = quote_arg(&exe.to_string_lossy());
    for a in std::iter::once(&"run".to_string()).chain(args) {
        line.push(' ');
        line.push_str(&quote_arg(a));
    }
    let mut line = wide(line);
    let app = wide(exe);
    let dir = wide(cwd());
    let si = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).expect("small"),
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated strings, `line` writable; no handle is inherited (FALSE); the
    // environment is this process's (null); `si` and `pi` are valid for the call.
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            flags,
            std::ptr::null(),
            dir.as_ptr(),
            &raw const si,
            &raw mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // The thread handle is not needed.
    drop(Handle::new(pi.hThread));
    Ok(Spawned {
        pid: pi.dwProcessId,
        process: Handle::new(pi.hProcess)?,
    })
}

/// Starts `<this exe> run <args…>` detached.
pub fn instance(args: &[String]) -> io::Result<Spawned> {
    let exe = std::env::current_exe()?;
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // A job without JOB_OBJECT_LIMIT_BREAKAWAY_OK refuses the flag (access denied).
    create(&exe, args, base | CREATE_BREAKAWAY_FROM_JOB).or_else(|_| create(&exe, args, base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_follows_commandlinetoargvw() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
    }
}
