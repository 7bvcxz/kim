//! `config.toml` (NOEUL-FR-102): versioned settings, read at start.
//!
//! Step 49 knows one setting, `[general] tray_icon` (default `true`, NOEUL-FR-004). The rules
//! that later settings inherit are implemented here:
//!
//! - a missing file means defaults;
//! - a file that is not TOML, or a `schema_version` other than 1, leaves the defaults in place
//!   and is reported (`newer_version` when it is higher; an older Noeul never rewrites it);
//! - an invalid value falls back to its default with a message naming the key, never a crash;
//! - unknown keys are kept: Noeul does not write this file yet, and when it does (settings
//!   window, Step 61) it edits keys in place.
//!
//! Settings hold no secrets, pixels or clipboard content.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 1;

/// Effective settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Show the notification-area icon (NOEUL-FR-004; on by default).
    pub tray_icon: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { tray_icon: true }
    }
}

/// Where the settings came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Default,
    User,
    Invalid,
    NewerVersion,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Default => "default",
            State::User => "user",
            State::Invalid => "invalid",
            State::NewerVersion => "newer_version",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Loaded {
    pub settings: Settings,
    pub path: PathBuf,
    pub state: State,
    /// One sentence per problem, naming the key (shown as warnings).
    pub problems: Vec<String>,
}

impl Loaded {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path.display().to_string(),
            "state": self.state.as_str(),
            "tray_icon": self.settings.tray_icon,
            "problems": self.problems,
        })
    }
}

pub fn load(path: &Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                settings: Settings::default(),
                path: path.to_path_buf(),
                state: State::Default,
                problems: Vec::new(),
            };
        }
        Err(e) => {
            return invalid(path, &format!("cannot read {}: {e}", path.display()));
        }
    };
    parse(path, &text)
}

fn invalid(path: &Path, problem: &str) -> Loaded {
    Loaded {
        settings: Settings::default(),
        path: path.to_path_buf(),
        state: State::Invalid,
        problems: vec![format!("{problem}; the default settings apply")],
    }
}

pub fn parse(path: &Path, text: &str) -> Loaded {
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let first = e.to_string();
            let first = first.lines().next().unwrap_or("parse error").to_string();
            return invalid(
                path,
                &format!("{} is not valid TOML ({first})", path.display()),
            );
        }
    };
    match table.get("schema_version") {
        None => {}
        Some(toml::Value::Integer(SCHEMA_VERSION)) => {}
        Some(toml::Value::Integer(n)) if *n > SCHEMA_VERSION => {
            return Loaded {
                settings: Settings::default(),
                path: path.to_path_buf(),
                state: State::NewerVersion,
                problems: vec![format!(
                    "{} has schema_version {n}, written by a newer Noeul; it is read-only and the default settings apply",
                    path.display()
                )],
            };
        }
        Some(_) => {
            return invalid(
                path,
                &format!("`schema_version` in {} must be 1", path.display()),
            );
        }
    }
    let mut settings = Settings::default();
    let mut problems = Vec::new();
    if let Some(general) = table.get("general") {
        match general {
            toml::Value::Table(g) => match g.get("tray_icon") {
                None => {}
                Some(toml::Value::Boolean(b)) => settings.tray_icon = *b,
                Some(_) => problems.push(
                    "`general.tray_icon` must be true or false; the default (true) applies"
                        .to_string(),
                ),
            },
            _ => problems.push("`general` must be a table; its defaults apply".to_string()),
        }
    }
    Loaded {
        settings,
        path: path.to_path_buf(),
        state: State::User,
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> PathBuf {
        PathBuf::from("config.toml")
    }

    #[test]
    fn missing_file_means_defaults() {
        let l = load(Path::new("definitely/not/here/config.toml"));
        assert_eq!(l.state, State::Default);
        assert!(l.settings.tray_icon);
        assert!(l.problems.is_empty());
    }

    #[test]
    fn values_unknown_keys_and_fallbacks() {
        let l = parse(
            &p(),
            "schema_version = 1\n[general]\ntray_icon = false\nfuture_key = \"kept\"\n[later]\nx = 1\n",
        );
        assert_eq!(l.state, State::User);
        assert!(!l.settings.tray_icon);
        assert!(l.problems.is_empty(), "unknown keys are not errors");

        let l = parse(&p(), "[general]\ntray_icon = \"no\"\n");
        assert_eq!(l.state, State::User);
        assert!(
            l.settings.tray_icon,
            "an invalid value falls back to its default"
        );
        assert!(
            l.problems[0].contains("general.tray_icon"),
            "{:?}",
            l.problems
        );
    }

    #[test]
    fn broken_or_newer_files_never_crash() {
        let l = parse(&p(), "this is = = not toml");
        assert_eq!(l.state, State::Invalid);
        assert!(l.settings.tray_icon);
        assert!(l.problems[0].contains("not valid TOML"));

        let l = parse(&p(), "schema_version = 2\n[general]\ntray_icon = false\n");
        assert_eq!(l.state, State::NewerVersion);
        assert!(l.settings.tray_icon, "a newer file is not interpreted");

        let l = parse(&p(), "schema_version = \"1\"\n");
        assert_eq!(l.state, State::Invalid);

        let l = parse(&p(), "general = 3\n");
        assert_eq!(l.state, State::User);
        assert_eq!(l.problems.len(), 1);
    }
}
