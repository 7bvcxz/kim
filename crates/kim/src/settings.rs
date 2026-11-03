//! User preferences for Kim (`%APPDATA%\mouv\kim\settings.json`). Privacy-first defaults:
//! no launch-time update check, not offline.

use crate::error::{ErrorCode, KimError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Defaults (all `false`) are the privacy-first choices; see the `defaults_are_private_and_online` test.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Let Kim Manager refresh the catalog when it starts, to *show* available updates.
    /// Off by default (KIM-SEC-002). Never downloads or installs anything.
    pub check_updates_on_launch: bool,
    /// Refuse every network operation (catalog refresh, downloads).
    pub offline_mode: bool,
    /// Set when the settings file exists but cannot be read or parsed. Kim then fails closed:
    /// network operations are refused (the user may have chosen offline mode) until the file is
    /// fixed or removed. Never serialized.
    #[serde(skip)]
    pub unreadable: Option<String>,
}

/// The keys that `kim settings set` accepts.
pub const KEYS: &[&str] = &["check_updates_on_launch", "offline_mode"];

impl Settings {
    pub fn path() -> Result<PathBuf> {
        settings_dir().map(|d| d.join("settings.json"))
    }

    /// Loads settings; a missing file yields defaults. A corrupt file also yields defaults plus a
    /// warning (never a crash).
    pub fn load_from(path: &Path) -> (Self, Option<String>) {
        // A missing file means defaults. A file that exists but cannot be used fails closed:
        // the user might have enabled offline mode in it, so network access is refused.
        let fail_closed = |why: String| {
            let s = Self {
                offline_mode: true,
                unreadable: Some(why.clone()),
                ..Self::default()
            };
            (
                s,
                Some(format!(
                    "{why}; network operations are disabled until the file is fixed or removed"
                )),
            )
        };
        match std::fs::read(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => fail_closed(format!("cannot read {}: {e}", path.display())),
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(s) => (s, None),
                Err(e) => fail_closed(format!("{} is invalid ({e})", path.display())),
            },
        }
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let io = |e: std::io::Error| KimError::io(path, &e);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| KimError::new(ErrorCode::Internal, e.to_string()))?;
        std::fs::write(&tmp, json).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(|e| {
            // Leave no stray temp file behind; the old settings stay exactly as they were.
            let _ = std::fs::remove_file(&tmp);
            io(e)
        })
    }

    /// Current value of a setting by name (`None` for an unknown name).
    pub fn get(&self, key: &str) -> Option<bool> {
        match key {
            "check_updates_on_launch" => Some(self.check_updates_on_launch),
            "offline_mode" => Some(self.offline_mode),
            _ => None,
        }
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let b = match value {
            "true" | "on" | "yes" | "1" => true,
            "false" | "off" | "no" | "0" => false,
            _ => {
                return Err(KimError::new(
                    ErrorCode::Usage,
                    format!("`{value}` is not a boolean (use true/false)"),
                ));
            }
        };
        match key {
            "check_updates_on_launch" => self.check_updates_on_launch = b,
            "offline_mode" => self.offline_mode = b,
            _ => {
                return Err(
                    KimError::new(ErrorCode::Usage, format!("unknown setting `{key}`"))
                        .with_hint(format!("known settings: {}", KEYS.join(", "))),
                );
            }
        }
        Ok(())
    }

    /// Error to return when a network operation is attempted in offline mode.
    pub fn ensure_online(&self) -> Result<()> {
        if let Some(why) = &self.unreadable {
            return Err(KimError::new(
                ErrorCode::OfflineMode,
                format!("{why}; Kim will not use the network while its settings are unreadable"),
            )
            .with_hint("fix or delete the settings file, then retry (defaults allow explicit network operations)"));
        }
        if self.offline_mode {
            Err(KimError::new(
                ErrorCode::OfflineMode,
                "offline mode is on; Kim will not use the network",
            )
            .with_hint(
                "run `kim settings set offline_mode false` to allow explicit network operations",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
fn settings_dir() -> Result<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(|a| PathBuf::from(a).join("mouv").join("kim"))
        .ok_or_else(|| KimError::new(ErrorCode::Io, "%APPDATA% is not set"))
}

#[cfg(not(windows))]
fn settings_dir() -> Result<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(x).join("mouv").join("kim"));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".config").join("mouv").join("kim"))
        .ok_or_else(|| KimError::new(ErrorCode::Io, "neither $XDG_CONFIG_HOME nor $HOME is set"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_private_and_online() {
        let s = Settings::default();
        assert!(
            !s.check_updates_on_launch,
            "launch-time checks are opt-in (KIM-SEC-002)"
        );
        assert!(!s.offline_mode);
    }

    #[test]
    fn round_trip_set_and_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("kim-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let (s, warn) = Settings::load_from(&path);
        assert_eq!((s, warn), (Settings::default(), None));
        let mut s = Settings::default();
        s.set("offline_mode", "true").unwrap();
        assert_eq!(s.ensure_online().unwrap_err().code, ErrorCode::OfflineMode);
        s.save_to(&path).unwrap();
        assert!(Settings::load_from(&path).0.offline_mode);
        assert_eq!(s.set("nope", "true").unwrap_err().code, ErrorCode::Usage);
        assert_eq!(
            s.set("offline_mode", "maybe").unwrap_err().code,
            ErrorCode::Usage
        );
        std::fs::write(&path, b"{oops").unwrap();
        let (s, warn) = Settings::load_from(&path);
        // Fail closed: a corrupt file must not silently re-enable the network.
        assert!(s.offline_mode && s.unreadable.is_some());
        assert!(!s.check_updates_on_launch);
        assert!(warn.unwrap().contains("network operations are disabled"));
        let e = s.ensure_online().unwrap_err();
        assert_eq!(e.code, ErrorCode::OfflineMode);
        assert!(e.message.contains("unreadable"), "{}", e.message);
        let _ = std::fs::remove_dir_all(dir);
    }
}
