//! Stable per-user locations (ADR 0006, NOEUL-FR-102, NOEUL-SEC-005).
//!
//! | What | Windows | Linux (N7, not claimed) |
//! | --- | --- | --- |
//! | Settings `config.toml`, shortcuts `hotkeys.json` | `%APPDATA%\mouv\noeul\` | `$XDG_CONFIG_HOME/mouv/noeul/` |
//! | User-only data (opt-in history, pin backup; none in Step 49) | `%LOCALAPPDATA%\mouv\noeul\` | `$XDG_DATA_HOME/mouv/noeul/` |
//!
//! The paths come from the environment, so tests and the e2e suite redirect a whole profile by
//! setting `APPDATA` and `LOCALAPPDATA` (Linux: `XDG_CONFIG_HOME`, `XDG_DATA_HOME`). Nothing here
//! creates a folder; Step 49 writes only `hotkeys.json` (on an explicit `hotkeys` change) and
//! creates the settings folder only when it writes there.

use crate::error::{Code, NoeulError, Result};
use std::path::PathBuf;

/// The tool ID: folder name, hotkey tool ID and binary name.
pub const TOOL: &str = "noeul";
/// The settings file name (NOEUL-FR-102).
pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `%APPDATA%\mouv\noeul`: `config.toml` and `hotkeys.json`.
    pub config_dir: PathBuf,
    /// `%LOCALAPPDATA%\mouv\noeul`: user-only data; nothing is written there in Step 49.
    pub data_dir: PathBuf,
    /// `%APPDATA%\mouv`: every Kim tool's settings folder (read by `hotkeys check --all-tools`).
    pub roaming_root: PathBuf,
    /// `%LOCALAPPDATA%\Programs\mouv`: Kim's per-user tool installs (read only).
    pub programs: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths> {
        let (config, data) = bases()?;
        Ok(Paths {
            config_dir: config.join("mouv").join(TOOL),
            data_dir: data.join("mouv").join(TOOL),
            roaming_root: config.join("mouv"),
            programs: programs_base(&data),
        })
    }

    /// Everything below one root (tests).
    pub fn under(root: impl Into<PathBuf>) -> Paths {
        let root = root.into();
        Paths {
            config_dir: root.join("Roaming").join("mouv").join(TOOL),
            data_dir: root.join("Local").join("mouv").join(TOOL),
            roaming_root: root.join("Roaming").join("mouv"),
            programs: root.join("Local").join("Programs").join("mouv"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE)
    }

    pub fn hotkeys_file(&self) -> PathBuf {
        self.config_dir.join(kim_hotkeys::store::FILE_NAME)
    }
}

/// Kim's install root (ADR 0006): `Programs\mouv` on Windows, `mouv/programs` elsewhere.
fn programs_base(data: &std::path::Path) -> PathBuf {
    if cfg!(windows) {
        data.join("Programs").join("mouv")
    } else {
        data.join("mouv").join("programs")
    }
}

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn bases() -> Result<(PathBuf, PathBuf)> {
    let missing = |v: &str| NoeulError::new(Code::Io, format!("%{v}% is not set"));
    let data = var("LOCALAPPDATA").ok_or_else(|| missing("LOCALAPPDATA"))?;
    let config = var("APPDATA").ok_or_else(|| missing("APPDATA"))?;
    Ok((config, data))
}

#[cfg(not(windows))]
fn bases() -> Result<(PathBuf, PathBuf)> {
    let home = var("HOME").ok_or_else(|| NoeulError::new(Code::Io, "$HOME is not set"))?;
    let config = var("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    let data = var("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share"));
    Ok((config, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable() {
        let p = Paths::under("/r");
        assert_eq!(
            p.config_file(),
            PathBuf::from("/r")
                .join("Roaming")
                .join("mouv")
                .join("noeul")
                .join("config.toml")
        );
        assert_eq!(
            p.hotkeys_file(),
            PathBuf::from("/r")
                .join("Roaming")
                .join("mouv")
                .join("noeul")
                .join("hotkeys.json")
        );
        assert_eq!(
            p.data_dir,
            PathBuf::from("/r").join("Local").join("mouv").join("noeul")
        );
    }

    #[cfg(windows)]
    #[test]
    fn real_environment_maps_to_appdata() {
        let p = Paths::from_env().unwrap();
        let appdata = PathBuf::from(std::env::var_os("APPDATA").unwrap());
        assert_eq!(p.config_dir, appdata.join("mouv").join("noeul"));
        let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap());
        assert_eq!(p.data_dir, local.join("mouv").join("noeul"));
    }
}
