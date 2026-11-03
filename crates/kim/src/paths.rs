//! Per-user locations (ADR 0006, ADR 0013). Derived from the standard environment variables so
//! that tests and sandboxes can redirect them.

use crate::error::{ErrorCode, KimError, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// Kim's own data: `%LOCALAPPDATA%\mouv\kim` (Linux: `$XDG_DATA_HOME/mouv/kim`).
    pub kim_data: PathBuf,
    /// Root of per-user tool installs: `%LOCALAPPDATA%\Programs\mouv`.
    pub programs: PathBuf,
    /// Per-user local data root of all mouv tools: `%LOCALAPPDATA%\mouv`.
    pub local_data: PathBuf,
    /// Per-user roaming data root of all mouv tools: `%APPDATA%\mouv`.
    pub roaming_data: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let base = local_data_base()?;
        Ok(Self {
            kim_data: base.join("mouv").join("kim"),
            programs: programs_base(&base),
            local_data: base.join("mouv"),
            roaming_data: roaming_base().unwrap_or_else(|| base.join("mouv")),
        })
    }

    /// For tests: everything below one root directory.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            kim_data: root.join("mouv").join("kim"),
            programs: root.join("Programs").join("mouv"),
            local_data: root.join("mouv"),
            roaming_data: root.join("Roaming").join("mouv"),
        }
    }

    /// `<programs>\<tool>`: everything Kim installed for a tool (program files only).
    pub fn tool_dir(&self, tool_id: &str) -> PathBuf {
        self.programs.join(tool_id)
    }

    /// The tool's *user data* locations (configuration, state). Uninstall keeps them unless the
    /// user separately confirms deleting them (KIM-SEC-005).
    pub fn user_data_dirs(&self, tool_id: &str) -> Vec<PathBuf> {
        vec![
            self.roaming_data.join(tool_id),
            self.local_data.join(tool_id),
        ]
    }

    /// Scratch space for downloads and unpacking; leftovers are removed by the next operation.
    pub fn staging_dir(&self) -> PathBuf {
        self.programs.join(".staging")
    }

    pub fn catalog_cache_dir(&self) -> PathBuf {
        self.kim_data.join("catalog")
    }

    /// `<programs>\<tool>\current.json`, the active-version pointer of an installed tool.
    pub fn install_record(&self, tool_id: &str) -> PathBuf {
        self.programs.join(tool_id).join("current.json")
    }
}

#[cfg(windows)]
fn local_data_base() -> Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| KimError::new(ErrorCode::Io, "%LOCALAPPDATA% is not set"))
}

#[cfg(windows)]
fn roaming_base() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(|a| PathBuf::from(a).join("mouv"))
}

#[cfg(not(windows))]
fn roaming_base() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|b| b.join("mouv"))
}

#[cfg(windows)]
fn programs_base(base: &std::path::Path) -> PathBuf {
    base.join("Programs").join("mouv")
}

#[cfg(not(windows))]
fn local_data_base() -> Result<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(x));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|h| PathBuf::from(h).join(".local").join("share"))
        .ok_or_else(|| KimError::new(ErrorCode::Io, "neither $XDG_DATA_HOME nor $HOME is set"))
}

#[cfg(not(windows))]
fn programs_base(base: &std::path::Path) -> PathBuf {
    base.join("mouv").join("programs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_under_root() {
        let p = Paths::under("/tmp/x");
        assert!(p.catalog_cache_dir().ends_with("mouv/kim/catalog"));
        assert!(
            p.install_record("subak")
                .ends_with("Programs/mouv/subak/current.json")
        );
    }
}
