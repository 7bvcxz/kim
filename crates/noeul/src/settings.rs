//! `config.toml` (NOEUL-FR-102): versioned settings, read at start.
//!
//! Settings so far: `[general] tray_icon` (default `true`, NOEUL-FR-004; Step 49) and
//! `[capture] keyboard_help` (`"first-run"` by default, `"always"` or `"never"`: the keyboard
//! help inside the capture overlay; Step 50, ADR 0026). The rules that later settings inherit
//! are implemented here:
//!
//! - a missing file means defaults;
//! - a file that is not TOML, or a `schema_version` other than 1, leaves the defaults in place
//!   and is reported (`newer_version` when it is higher; an older Noeul never rewrites it);
//! - an invalid value falls back to its default with a message naming the key, never a crash;
//! - unknown keys are kept: Noeul does not write this file yet, and when it does (settings
//!   window, Step 61) it edits keys in place.
//!
//! Settings hold no secrets, pixels or clipboard content.
//!
//! [`UiState`] is not a setting: it is what Noeul remembers about its own UI (so far, whether the
//! first-run keyboard help was seen), in `%LOCALAPPDATA%\mouv\noeul\ui-state.json`, written once
//! after the first capture finished from the overlay.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 1;

/// When the overlay shows its keyboard help (`[capture] keyboard_help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardHelp {
    /// Until the first capture finished from the overlay (the default).
    FirstRun,
    Always,
    Never,
}

impl KeyboardHelp {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyboardHelp::FirstRun => "first-run",
            KeyboardHelp::Always => "always",
            KeyboardHelp::Never => "never",
        }
    }

    fn parse(s: &str) -> Option<KeyboardHelp> {
        [
            KeyboardHelp::FirstRun,
            KeyboardHelp::Always,
            KeyboardHelp::Never,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// Effective settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Show the notification-area icon (NOEUL-FR-004; on by default).
    pub tray_icon: bool,
    /// The overlay's keyboard help (ADR 0026).
    pub keyboard_help: KeyboardHelp,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            tray_icon: true,
            keyboard_help: KeyboardHelp::FirstRun,
        }
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
            "keyboard_help": self.settings.keyboard_help.as_str(),
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
    if let Some(capture) = table.get("capture") {
        match capture {
            toml::Value::Table(c) => match c.get("keyboard_help") {
                None => {}
                Some(toml::Value::String(v)) if KeyboardHelp::parse(v).is_some() => {
                    settings.keyboard_help = KeyboardHelp::parse(v).unwrap_or(KeyboardHelp::FirstRun);
                }
                Some(_) => problems.push(
                    "`capture.keyboard_help` must be \"first-run\", \"always\" or \"never\"; the default (\"first-run\") applies"
                        .to_string(),
                ),
            },
            _ => problems.push("`capture` must be a table; its defaults apply".to_string()),
        }
    }
    Loaded {
        settings,
        path: path.to_path_buf(),
        state: State::User,
        problems,
    }
}

/// What Noeul remembers about its own UI (not a setting; see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UiState {
    /// The first-run keyboard help of the overlay was shown and a capture finished from it.
    pub overlay_help_seen: bool,
}

impl UiState {
    /// Reads `ui-state.json`; a missing or unreadable file means "nothing seen yet".
    pub fn load(path: &Path) -> UiState {
        let v: Value = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        UiState {
            overlay_help_seen: v["overlay_help_seen"] == true,
        }
    }

    /// Writes `ui-state.json` atomically (a temporary file in the same folder, then a rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let doc = json!({"schema": "noeul.ui-state", "schema_version": 1,
                         "overlay_help_seen": self.overlay_help_seen});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&doc).unwrap_or_default())?;
        std::fs::rename(&tmp, path)
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
        assert_eq!(l.settings.keyboard_help, KeyboardHelp::FirstRun);

        let l = parse(&p(), "[capture]\nkeyboard_help = \"never\"\n");
        assert_eq!(l.settings.keyboard_help, KeyboardHelp::Never);
        assert_eq!(l.to_json()["keyboard_help"], "never");
        let l = parse(&p(), "[capture]\nkeyboard_help = \"sometimes\"\n");
        assert_eq!(l.settings.keyboard_help, KeyboardHelp::FirstRun);
        assert!(
            l.problems[0].contains("capture.keyboard_help"),
            "{:?}",
            l.problems
        );
        let l = parse(&p(), "capture = 1\n");
        assert_eq!(l.problems.len(), 1);

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

    #[test]
    fn the_ui_state_is_remembered_atomically() {
        let dir = std::env::temp_dir().join(format!("noeul-ui-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = dir.join("sub").join("ui-state.json");
        assert_eq!(
            UiState::load(&f),
            UiState::default(),
            "missing: nothing seen"
        );
        UiState {
            overlay_help_seen: true,
        }
        .save(&f)
        .unwrap();
        assert!(UiState::load(&f).overlay_help_seen);
        let names: Vec<_> = std::fs::read_dir(f.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temporary file is left: {names:?}");
        std::fs::write(&f, b"{ broken").unwrap();
        assert_eq!(UiState::load(&f), UiState::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
