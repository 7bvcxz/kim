//! Ddalgak's per-user folders (the `Paths` interface of ADR 0011).
//!
//! | What | Windows | Linux (D7, not claimed) |
//! | --- | --- | --- |
//! | `rules.json`, `hotkeys.json` | `%APPDATA%\mouv\ddalgak\` | `$XDG_CONFIG_HOME/mouv/ddalgak/` |
//! | run and emergency markers | `%LOCALAPPDATA%\mouv\ddalgak\` | `$XDG_DATA_HOME/mouv/ddalgak/` |
//!
//! The folders are in the user's profile and inherit its per-user ACL (`DDALGAK-SEC-011`).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// Roaming configuration: rules and shortcuts.
    pub config: PathBuf,
    /// Local state: markers.
    pub local: PathBuf,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Paths {
    /// From the environment (`%APPDATA%`, `%LOCALAPPDATA%`; on Linux the XDG variables).
    pub fn from_env() -> Result<Paths, String> {
        #[cfg(windows)]
        let (config, local) = (
            env_dir("APPDATA").ok_or("%APPDATA% is not set")?,
            env_dir("LOCALAPPDATA").ok_or("%LOCALAPPDATA% is not set")?,
        );
        #[cfg(not(windows))]
        let (config, local) = {
            let home = env_dir("HOME").ok_or("$HOME is not set")?;
            (
                env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
                env_dir("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local").join("share")),
            )
        };
        Ok(Paths {
            config: config.join("mouv").join("ddalgak"),
            local: local.join("mouv").join("ddalgak"),
        })
    }

    /// For tests: everything below one root.
    pub fn under(root: impl Into<PathBuf>) -> Paths {
        let root = root.into();
        Paths {
            config: root.join("Roaming").join("mouv").join("ddalgak"),
            local: root.join("Local").join("mouv").join("ddalgak"),
        }
    }

    pub fn rules(&self) -> PathBuf {
        self.config.join("rules.json")
    }

    pub fn hotkeys(&self) -> PathBuf {
        self.config.join(kim_hotkeys::store::FILE_NAME)
    }
}
