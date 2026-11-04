//! Opt-in start at sign-in (DDALGAK_SAFETY §1, ADR 0024 §6).
//!
//! Off by default. `ddalgak startup enable` writes one value under the user's own Run key:
//!
//! ```text
//! HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run
//!     mouv.ddalgak = "<install folder>\ddalgak-gui.exe" --startup
//! ```
//!
//! That starts the tray app (with its tray icon) when the user signs in, with the engine `off`
//! unless the user chose `engine_on_start: on` (`DDALGAK-FR-003`). No service, no scheduled task,
//! no SYSTEM or elevated component, nothing for other users (`DDALGAK-SEC-007`, `KIM-SEC-004`).
//! `ddalgak startup disable` removes the value; Windows' Startup apps settings page can turn it
//! off too. The store is behind [`Store`] so tests never touch the real Run key.

use std::io;
use std::path::Path;

/// The user's Run key (relative to `HKEY_CURRENT_USER`).
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The value name Ddalgak writes.
pub const VALUE_NAME: &str = "mouv.ddalgak";
/// The flag the app gets when started at sign-in.
pub const STARTUP_FLAG: &str = "--startup";

/// Where the sign-in command is kept.
pub trait Store {
    fn get(&self) -> io::Result<Option<String>>;
    fn set(&mut self, command: &str) -> io::Result<()>;
    /// Removes the value; `false` when there was none.
    fn remove(&mut self) -> io::Result<bool>;
    /// A human description of the location (`HKCU\…\Run\mouv.ddalgak`).
    fn location(&self) -> String;
}

/// The command line written for `gui`.
pub fn command_for(gui: &Path) -> String {
    format!("\"{}\" {STARTUP_FLAG}", gui.display())
}

/// In-memory store (tests).
#[derive(Clone, Debug, Default)]
pub struct Memory {
    pub value: Option<String>,
    pub fail: bool,
}

impl Store for Memory {
    fn get(&self) -> io::Result<Option<String>> {
        Ok(self.value.clone())
    }

    fn set(&mut self, command: &str) -> io::Result<()> {
        if self.fail {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.value = Some(command.to_string());
        Ok(())
    }

    fn remove(&mut self) -> io::Result<bool> {
        if self.fail {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(self.value.take().is_some())
    }

    fn location(&self) -> String {
        "memory".into()
    }
}

/// The state shown by `ddalgak startup status`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct State {
    pub enabled: bool,
    pub command: Option<String>,
    /// The value exists but does not start this installation's `ddalgak-gui.exe`.
    pub foreign: bool,
}

pub fn state(store: &dyn Store, gui: &Path) -> io::Result<State> {
    let command = store.get()?;
    let expected = command_for(gui);
    Ok(State {
        enabled: command.is_some(),
        foreign: command.as_ref().is_some_and(|c| *c != expected),
        command,
    })
}

/// What `enable` did or would do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outcome {
    pub changed: bool,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// Writes the sign-in command for `gui` (unless `dry_run`). The program must exist.
pub fn enable(store: &mut dyn Store, gui: &Path, dry_run: bool) -> Result<Outcome, String> {
    if !gui.is_file() {
        return Err(format!(
            "{} does not exist; startup at sign-in needs ddalgak-gui.exe next to ddalgak.exe",
            gui.display()
        ));
    }
    let before = store.get().map_err(|e| e.to_string())?;
    let after = command_for(gui);
    let changed = before.as_deref() != Some(after.as_str());
    if changed && !dry_run {
        store.set(&after).map_err(|e| e.to_string())?;
    }
    Ok(Outcome {
        changed,
        before,
        after: Some(after),
    })
}

/// Removes the sign-in command (unless `dry_run`).
pub fn disable(store: &mut dyn Store, dry_run: bool) -> Result<Outcome, String> {
    let before = store.get().map_err(|e| e.to_string())?;
    let changed = before.is_some();
    if changed && !dry_run {
        store.remove().map_err(|e| e.to_string())?;
    }
    Ok(Outcome {
        changed,
        before,
        after: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_disable_and_dry_run() {
        let exe = std::env::current_exe().unwrap();
        let mut m = Memory::default();
        assert!(!state(&m, &exe).unwrap().enabled);
        let o = enable(&mut m, &exe, true).unwrap();
        assert!(o.changed && m.value.is_none(), "dry run writes nothing");
        let o = enable(&mut m, &exe, false).unwrap();
        assert!(o.changed);
        assert_eq!(m.value.as_deref(), Some(command_for(&exe).as_str()));
        assert!(m.value.as_ref().unwrap().ends_with("\" --startup"));
        assert!(!enable(&mut m, &exe, false).unwrap().changed, "idempotent");
        let s = state(&m, &exe).unwrap();
        assert!(s.enabled && !s.foreign);
        m.value = Some("\"C:\\other\\ddalgak-gui.exe\" --startup".into());
        assert!(state(&m, &exe).unwrap().foreign);
        assert!(disable(&mut m, true).unwrap().changed && m.value.is_some());
        assert!(disable(&mut m, false).unwrap().changed && m.value.is_none());
        assert!(!disable(&mut m, false).unwrap().changed);
    }

    #[test]
    fn a_missing_program_or_a_refusing_store_is_an_error() {
        let mut m = Memory::default();
        assert!(enable(&mut m, Path::new("Z:/nope/ddalgak-gui.exe"), false).is_err());
        let mut f = Memory {
            fail: true,
            ..Memory::default()
        };
        let exe = std::env::current_exe().unwrap();
        assert!(enable(&mut f, &exe, false).is_err());
        assert!(
            enable(&mut f, &exe, true).is_ok(),
            "a dry run does not write"
        );
    }
}
