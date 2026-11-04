//! Single-instance ownership per user session (ADR 0024 §4).
//!
//! The tray app holds a named mutex in the session's `Local\` namespace whose name includes the
//! user SID. A second start finds it and exits (`app_already_running`) without touching the
//! first: no hook, no tray icon, no window. The pipe adds a second guard: its first instance is
//! created with `FILE_FLAG_FIRST_PIPE_INSTANCE`. The mutex is only an ownership marker; the
//! security boundary is the pipe's DACL and identity checks.

use super::{Handle, session_id, user_sid, wide};
use std::io;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;

/// The mutex name for this user and session (and, for development endpoints, the endpoint).
pub fn name(tag: Option<&str>) -> io::Result<String> {
    let base = format!("Local\\mouv-ddalgak-{}-s{}", user_sid()?, session_id());
    Ok(match tag {
        Some(t) => format!("{base}-{t}"),
        None => base,
    })
}

/// Held while this process owns the session's engine; released when dropped or at exit.
#[derive(Debug)]
pub struct Instance {
    _h: Handle,
}

/// Takes ownership, or `Ok(None)` when another process already owns it.
pub fn acquire(name: &str) -> io::Result<Option<Instance>> {
    let w = wide(name);
    // SAFETY: NUL-terminated name; default security; not initially owned (existence is the
    // marker, so the mutex is never waited on).
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, w.as_ptr()) };
    // SAFETY: read right after the call.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let h = Handle::new(h)?;
    if existed {
        return Ok(None);
    }
    Ok(Some(Instance { _h: h }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_owner_is_refused_until_the_first_is_gone() {
        let n = name(Some(&format!("unit-{}", std::process::id()))).unwrap();
        assert!(n.starts_with("Local\\mouv-ddalgak-S-1-5-"));
        let first = acquire(&n).unwrap().expect("first owner");
        assert!(acquire(&n).unwrap().is_none(), "single instance");
        drop(first);
        assert!(
            acquire(&n).unwrap().is_some(),
            "free again after the owner ended"
        );
    }
}
