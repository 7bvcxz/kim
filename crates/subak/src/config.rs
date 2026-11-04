//! Subak's settings: `%APPDATA%\mouv\subak\config.toml` (Step 20, ADR 0051, docs/SUBAK_GUI.md
//! §13, docs/contracts/STATE_SCHEMA.md §2; `SUBAK-FR-046`, `SUBAK-FR-048`, `SUBAK-FR-049`).
//!
//! What the window looks like and does (theme, terminal font, text size, line height, padding,
//! window opacity, motion; copy on select, smart `Ctrl+C`, the right and middle click, word
//! separators) and the basic local shell **profiles** (`[[profiles]]`: program, arguments,
//! working directory, environment overrides) that `Ctrl+T`, the window's "New Terminal" menu and
//! `subak new --profile` start. Key bindings stay in `hotkeys.json` (ADR 0017); the window's
//! placement and remembered tabs stay in `gui.json`.
//!
//! Portable and headless: this module reads and edits a text file. The **Host never reads it**
//! (`crates/subak/tests/config_boundary.rs`): a profile is resolved by the client into the
//! explicit `command`, `cwd` and `env` of `terminal.create`, so editing the settings never
//! changes a running Terminal (ADR 0051 §3).
//!
//! Rules (ADR 0006 writing rules, `SUBAK-FR-046`):
//!
//! - a missing file means the defaults;
//! - a value that is invalid falls back to its default alone, with a problem that names the key
//!   and its line; an invalid profile is left out with a problem; nothing crashes;
//! - a file that is not TOML, or whose `config_version` is newer than this build's, is used as
//!   defaults and **never rewritten** by an edit (`config_invalid`, `config_from_newer_version`);
//!   only an explicit `reset --all` or `import` replaces an invalid file, after a backup;
//! - an **edit** (`subak config set|reset`, the window's settings menu, the zoom keys) changes
//!   only the lines of its keys: comments, unknown keys and sections stay. The edited text is
//!   parsed again and must differ from the old one in exactly those keys, or nothing is written;
//! - writes are atomic (a temporary file, flushed, then renamed);
//! - bulk changes (`reset`, `import`, the `gui.json` v1 migration) first copy the current file to
//!   `backups\config-<UTC time>.toml`; the newest [`BACKUPS_KEPT`] backups are kept;
//! - **no secrets**: Subak never writes a secret-looking environment value into the file
//!   (`secret_in_config`), and every value shown (CLI, window, export) is redacted with the one
//!   secret detector of `crate::redact`. A secret-looking value someone typed into the file
//!   by hand is used, reported as a warning and shown as `***`.

use crate::redact;
use serde_json::{Map, Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The file name inside the configuration directory.
pub const FILE: &str = "config.toml";
/// The `config_version` this build reads and writes.
pub const VERSION: i64 = 1;
/// The folder (inside the configuration directory) that holds backups.
pub const BACKUP_DIR: &str = "backups";
/// How many backups of each file are kept.
pub const BACKUPS_KEPT: usize = 5;
/// Most profiles a file may define.
pub const MAX_PROFILES: usize = 64;
/// The largest settings file read (bytes).
pub const MAX_BYTES: u64 = 1024 * 1024;

/// `%APPDATA%\mouv\subak` (Linux, M4: `$XDG_CONFIG_HOME/mouv/subak`), from the environment, so a
/// test can point it at a sandbox.
pub fn config_dir() -> Option<PathBuf> {
    crate::hotkeys::config_dir()
}

// ---------------------------------------------------------------------------------------------
// The settings.

/// Which colours the window uses (`appearance.theme`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    /// `kim-dark`, or the system colours while Windows High Contrast is on (the default).
    Auto,
    /// `kim-light` while Windows apps use the light mode, else `kim-dark`; the system colours
    /// under High Contrast.
    System,
    KimDark,
    KimLight,
    /// The system's High Contrast colours, always.
    HighContrast,
}

impl ThemeChoice {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::System,
        Self::KimDark,
        Self::KimLight,
        Self::HighContrast,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::System => "system",
            Self::KimDark => "kim-dark",
            Self::KimLight => "kim-light",
            Self::HighContrast => "high-contrast",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }

    /// The theme this choice draws with, given whether Windows High Contrast is on and whether
    /// Windows apps use the light mode: `kim-dark`, `kim-light` or `high-contrast`.
    pub fn resolve(self, high_contrast: bool, light_mode: bool) -> &'static str {
        match self {
            Self::Auto | Self::System if high_contrast => "high-contrast",
            Self::System if light_mode => "kim-light",
            Self::Auto | Self::System | Self::KimDark => "kim-dark",
            Self::KimLight => "kim-light",
            Self::HighContrast => "high-contrast",
        }
    }

    /// The menu label.
    pub fn title(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (Kim dark; High Contrast when Windows uses it)",
            Self::System => "Follow Windows light or dark mode",
            Self::KimDark => "Kim dark",
            Self::KimLight => "Kim light",
            Self::HighContrast => "High Contrast (system colours)",
        }
    }
}

/// Whether the window animates its chrome (`appearance.motion`; DESIGN_SYSTEM §2.5, §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Follow Windows' "Show animations in Windows" (the default).
    System,
    /// No motion: every transition is instant.
    Reduced,
    /// Short transitions even when Windows turns animations off.
    Full,
}

impl Motion {
    pub const ALL: [Self; 3] = [Self::System, Self::Reduced, Self::Full];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Reduced => "reduced",
            Self::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// Whether transitions run, given whether Windows shows animations.
    pub fn animates(self, system_animations: bool) -> bool {
        match self {
            Self::System => system_animations,
            Self::Reduced => false,
            Self::Full => true,
        }
    }
}

/// What a right click in the pane does (`terminal.right_click`, Tabby `rightClick`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RightClick {
    /// Copy a selection, else paste (Windows Terminal's default; the Step 16 behaviour).
    CopyPaste,
    /// Open the pane menu.
    Menu,
    /// Paste (asking first as every paste does).
    Paste,
    /// Nothing (a program that tracks the mouse still gets it).
    None,
}

impl RightClick {
    pub const ALL: [Self; 4] = [Self::CopyPaste, Self::Menu, Self::Paste, Self::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CopyPaste => "copy-paste",
            Self::Menu => "menu",
            Self::Paste => "paste",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// What a middle click in the pane does (`terminal.middle_click`, Tabby `pasteOnMiddleClick`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiddleClick {
    None,
    Paste,
}

impl MiddleClick {
    pub const ALL: [Self; 2] = [Self::None, Self::Paste];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Paste => "paste",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// `[appearance]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    pub theme: ThemeChoice,
    /// Tried first; the design system's chain follows (`""`: the chain alone).
    pub font_family: String,
    /// Logical pixels, [`FONT_SIZE`] range; the zoom keys change it.
    pub font_size: f32,
    /// The cell height as a multiple of the text size.
    pub line_height: f32,
    /// Logical pixels around the cell grid.
    pub padding: u32,
    /// Window opacity in percent ([`OPACITY`] range; High Contrast always draws opaque).
    pub opacity: u32,
    pub motion: Motion,
    /// Bold text in ANSI colours 0–7 uses their bright variants.
    pub bold_is_bright: bool,
}

/// `[terminal]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPrefs {
    /// The profile `Ctrl+T`, `+` and `subak new` (without a program) start (`""`: the Host's
    /// default shell).
    pub default_profile: String,
    /// A mouse selection is copied when the button is released (PuTTY style; off by default,
    /// so the clipboard changes only on an explicit copy).
    pub copy_on_select: bool,
    /// `Ctrl+C` copies when text is selected, else it interrupts the program as always.
    pub smart_ctrl_c: bool,
    pub right_click: RightClick,
    pub middle_click: MiddleClick,
    /// Characters that end a word for double-click selection and mark mode.
    pub word_separators: String,
}

/// One basic local shell profile (`[[profiles]]`; SSH and advanced profiles are Step 33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    /// An absolute working directory; `None`: the Host's default (the user's profile folder).
    pub cwd: Option<String>,
    /// Variables set in the pane (sorted by name).
    pub env: Vec<(String, String)>,
    /// Variables removed from the pane's environment.
    pub unset_env: Vec<String>,
    /// The 1-based line of its `[[profiles]]` header (messages).
    pub line: Option<usize>,
}

/// Everything `config.toml` decides.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub appearance: Appearance,
    pub terminal: TerminalPrefs,
    pub profiles: Vec<Profile>,
}

/// `appearance.font_size` bounds and default (`size.terminal`).
pub const FONT_SIZE: (f32, f32, f32) = (8.0, 36.0, 13.0);
/// `appearance.line_height` bounds and default (`line-height.terminal`).
pub const LINE_HEIGHT: (f32, f32, f32) = (1.0, 2.0, 1.2);
/// `appearance.padding` bounds and default (DESIGN_SYSTEM §3.5: 8, configurable 0–16).
pub const PADDING: (u32, u32, u32) = (0, 16, 8);
/// `appearance.opacity` bounds and default, percent.
pub const OPACITY: (u32, u32, u32) = (50, 100, 100);
/// The longest `appearance.font_family` (a GDI face name holds 31 characters).
pub const FONT_FAMILY_MAX: usize = 31;

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance {
                theme: ThemeChoice::Auto,
                font_family: String::new(),
                font_size: FONT_SIZE.2,
                line_height: LINE_HEIGHT.2,
                padding: PADDING.2,
                opacity: OPACITY.2,
                motion: Motion::System,
                bold_is_bright: true,
            },
            terminal: TerminalPrefs {
                default_profile: String::new(),
                copy_on_select: false,
                smart_ctrl_c: false,
                right_click: RightClick::CopyPaste,
                middle_click: MiddleClick::None,
                word_separators: subak_term::view::WORD_DELIMITERS.to_string(),
            },
            profiles: Vec::new(),
        }
    }
}

impl Settings {
    /// The profile named `name` (case ignored).
    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name.to_lowercase() == name.trim().to_lowercase())
    }

    /// The default profile, when one is set and exists.
    pub fn default_profile(&self) -> Option<&Profile> {
        let d = self.terminal.default_profile.trim();
        if d.is_empty() { None } else { self.profile(d) }
    }

    /// The settings as JSON (the `subak-config-v1` data model; profile values redacted).
    #[cfg(test)]
    pub fn to_json(&self) -> Value {
        let mut doc = Map::new();
        doc.insert("config_version".into(), json!(VERSION));
        for section in SECTIONS {
            let mut m = Map::new();
            for k in KEYS.iter().filter(|k| k.section == *section) {
                m.insert(k.name.into(), self.value(k));
            }
            doc.insert((*section).into(), Value::Object(m));
        }
        doc.insert(
            "profiles".into(),
            Value::Array(
                self.profiles
                    .iter()
                    .map(|p| p.to_json(self.is_default(p)))
                    .collect(),
            ),
        );
        Value::Object(doc)
    }

    #[cfg(test)]
    fn is_default(&self, p: &Profile) -> bool {
        self.default_profile().is_some_and(|d| d.name == p.name)
    }

    /// The value of a known key, as JSON.
    pub fn value(&self, k: &Key) -> Value {
        let a = &self.appearance;
        let t = &self.terminal;
        match (k.section, k.name) {
            ("appearance", "theme") => json!(a.theme.as_str()),
            ("appearance", "font_family") => json!(a.font_family),
            ("appearance", "font_size") => rounded(a.font_size),
            ("appearance", "line_height") => rounded(a.line_height),
            ("appearance", "padding") => json!(a.padding),
            ("appearance", "opacity") => json!(a.opacity),
            ("appearance", "motion") => json!(a.motion.as_str()),
            ("appearance", "bold_is_bright") => json!(a.bold_is_bright),
            ("terminal", "default_profile") => json!(t.default_profile),
            ("terminal", "copy_on_select") => json!(t.copy_on_select),
            ("terminal", "smart_ctrl_c") => json!(t.smart_ctrl_c),
            ("terminal", "right_click") => json!(t.right_click.as_str()),
            ("terminal", "middle_click") => json!(t.middle_click.as_str()),
            ("terminal", "word_separators") => json!(t.word_separators),
            _ => Value::Null,
        }
    }
}

/// A size as JSON with at most two decimals (`1.2`, not `1.2000000476837158`).
fn rounded(v: f32) -> Value {
    let r = (f64::from(v) * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        json!(r as i64)
    } else {
        json!(r)
    }
}

impl Profile {
    /// Variables whose name or value looks secret (by the detector of `crate::redact`).
    pub fn secret_env(&self) -> Vec<&str> {
        self.env
            .iter()
            .filter(|(n, v)| secret_looking(n, v))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Every part that looks secret: `env.<NAME>` per variable, `args` and `program` when the
    /// detector hides something in them (`--password x`, a token in a URL).
    pub fn secrets(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .secret_env()
            .into_iter()
            .map(|n| format!("env.{n}"))
            .collect();
        if redact::args(&self.args) != self.args {
            out.push("args".into());
        }
        if redact::program(&self.program) != self.program {
            out.push("program".into());
        }
        out
    }

    /// The profile as it may be shown: secret-looking environment values and arguments
    /// redacted.
    pub fn to_json(&self, default: bool) -> Value {
        let env: Map<String, Value> = self
            .env
            .iter()
            .map(|(n, v)| (n.clone(), json!(shown_value(n, v))))
            .collect();
        json!({
            "name": self.name,
            "program": redact::program(&self.program),
            "args": redact::args(&self.args),
            "cwd": self.cwd,
            "env": env,
            "unset_env": self.unset_env,
            "secret_env": self.secret_env(),
            "default": default,
        })
    }

    /// The `terminal.create` parameters this profile decides: `command`, `cwd` and `env`
    /// (explicit values; the Host never reads profiles, ADR 0051 §3).
    pub fn create_params(&self) -> Value {
        let mut env = Map::new();
        for (n, v) in &self.env {
            env.insert(n.clone(), json!(v));
        }
        for n in &self.unset_env {
            env.insert(n.clone(), Value::Null);
        }
        json!({
            "command": {"program": self.program, "args": self.args},
            "cwd": self.cwd,
            "env": if env.is_empty() { Value::Null } else { Value::Object(env) },
        })
    }
}

/// Whether an environment variable looks secret: its name (`GITHUB_TOKEN`, `API_KEY`) or its
/// value (a token shape, a URL password) by the one secret detector (`KIM-SEC-013`).
pub fn secret_looking(name: &str, value: &str) -> bool {
    redact::secret_name(name) || redact::text(value) != value || value == redact::MASK
}

/// A value as it may be shown.
pub fn shown_value(name: &str, value: &str) -> String {
    if redact::secret_name(name) {
        redact::MASK.to_string()
    } else {
        redact::text(value)
    }
}

// ---------------------------------------------------------------------------------------------
// The keys.

/// A setting of a section (`[[profiles]]` aside).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub section: &'static str,
    pub name: &'static str,
    /// What it takes, in words.
    pub takes: &'static str,
    kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Bool,
    Number,
    Integer,
}

impl Key {
    /// `section.name`.
    pub fn id(&self) -> String {
        format!("{}.{}", self.section, self.name)
    }
}

/// The sections of keys, in file order.
pub const SECTIONS: &[&str] = &["appearance", "terminal"];

/// Every key, in file order.
pub const KEYS: &[Key] = &[
    Key {
        section: "appearance",
        name: "theme",
        takes: "auto, system, kim-dark, kim-light or high-contrast",
        kind: Kind::Text,
    },
    Key {
        section: "appearance",
        name: "font_family",
        takes: "a font family name of at most 31 characters (\"\" = Cascadia Mono, Consolas, …)",
        kind: Kind::Text,
    },
    Key {
        section: "appearance",
        name: "font_size",
        takes: "a number from 8 to 36 (logical pixels)",
        kind: Kind::Number,
    },
    Key {
        section: "appearance",
        name: "line_height",
        takes: "a number from 1.0 to 2.0",
        kind: Kind::Number,
    },
    Key {
        section: "appearance",
        name: "padding",
        takes: "a whole number from 0 to 16 (logical pixels)",
        kind: Kind::Integer,
    },
    Key {
        section: "appearance",
        name: "opacity",
        takes: "a whole number from 50 to 100 (percent)",
        kind: Kind::Integer,
    },
    Key {
        section: "appearance",
        name: "motion",
        takes: "system, reduced or full",
        kind: Kind::Text,
    },
    Key {
        section: "appearance",
        name: "bold_is_bright",
        takes: "true or false",
        kind: Kind::Bool,
    },
    Key {
        section: "terminal",
        name: "default_profile",
        takes: "the name of a profile (\"\" = the Host's default shell)",
        kind: Kind::Text,
    },
    Key {
        section: "terminal",
        name: "copy_on_select",
        takes: "true or false",
        kind: Kind::Bool,
    },
    Key {
        section: "terminal",
        name: "smart_ctrl_c",
        takes: "true or false",
        kind: Kind::Bool,
    },
    Key {
        section: "terminal",
        name: "right_click",
        takes: "copy-paste, menu, paste or none",
        kind: Kind::Text,
    },
    Key {
        section: "terminal",
        name: "middle_click",
        takes: "none or paste",
        kind: Kind::Text,
    },
    Key {
        section: "terminal",
        name: "word_separators",
        takes: "up to 128 characters, no control characters",
        kind: Kind::Text,
    },
];

/// The key `section.name`.
pub fn key(id: &str) -> Option<&'static Key> {
    let (s, n) = id.split_once('.')?;
    KEYS.iter().find(|k| k.section == s && k.name == n)
}

/// Every key ID, for messages.
pub fn key_ids() -> Vec<String> {
    KEYS.iter().map(Key::id).collect()
}

fn as_number(v: &toml::Value) -> Option<f64> {
    match v {
        toml::Value::Integer(i) => Some(*i as f64),
        toml::Value::Float(f) if f.is_finite() => Some(*f),
        _ => None,
    }
}

fn clean_text(s: &str, max: usize) -> Option<String> {
    (s.chars().count() <= max && !s.chars().any(char::is_control)).then(|| s.to_string())
}

/// Applies `v` to `s` for key `k`; `Err` when the value is not valid for it.
fn apply(s: &mut Settings, k: &Key, v: &toml::Value) -> Result<(), ()> {
    let a = &mut s.appearance;
    let t = &mut s.terminal;
    let text = || v.as_str().ok_or(());
    let num = |lo: f64, hi: f64| as_number(v).filter(|n| (lo..=hi).contains(n)).ok_or(());
    let int = |lo: u32, hi: u32| match v {
        toml::Value::Integer(i) => u32::try_from(*i)
            .ok()
            .filter(|n| (lo..=hi).contains(n))
            .ok_or(()),
        _ => Err(()),
    };
    let flag = || v.as_bool().ok_or(());
    match (k.section, k.name) {
        ("appearance", "theme") => a.theme = ThemeChoice::parse(text()?).ok_or(())?,
        ("appearance", "font_family") => {
            a.font_family = clean_text(text()?.trim(), FONT_FAMILY_MAX).ok_or(())?;
        }
        ("appearance", "font_size") => {
            a.font_size = num(f64::from(FONT_SIZE.0), f64::from(FONT_SIZE.1))? as f32;
        }
        ("appearance", "line_height") => {
            a.line_height = num(f64::from(LINE_HEIGHT.0), f64::from(LINE_HEIGHT.1))? as f32;
        }
        ("appearance", "padding") => a.padding = int(PADDING.0, PADDING.1)?,
        ("appearance", "opacity") => a.opacity = int(OPACITY.0, OPACITY.1)?,
        ("appearance", "motion") => a.motion = Motion::parse(text()?).ok_or(())?,
        ("appearance", "bold_is_bright") => a.bold_is_bright = flag()?,
        ("terminal", "default_profile") => {
            t.default_profile = clean_text(text()?.trim(), 64).ok_or(())?;
        }
        ("terminal", "copy_on_select") => t.copy_on_select = flag()?,
        ("terminal", "smart_ctrl_c") => t.smart_ctrl_c = flag()?,
        ("terminal", "right_click") => t.right_click = RightClick::parse(text()?).ok_or(())?,
        ("terminal", "middle_click") => {
            t.middle_click = MiddleClick::parse(text()?).ok_or(())?;
        }
        ("terminal", "word_separators") => {
            t.word_separators =
                clean_text(text()?, subak_term::view::MAX_WORD_DELIMITERS).ok_or(())?;
        }
        _ => return Err(()),
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Loading.

/// How a settings file was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No file: the defaults.
    Missing,
    /// Read; invalid values (if any) fell back alone.
    User,
    /// Not TOML, or not a settings document: the defaults; never rewritten by an edit.
    Invalid,
    /// Written by a newer Subak: the defaults; never rewritten.
    Newer,
    /// Could not be read (permissions, a folder, too large): the defaults.
    Unreadable,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::User => "user",
            Self::Invalid => "invalid",
            Self::Newer => "newer",
            Self::Unreadable => "unreadable",
        }
    }

    /// Whether an edit may rewrite the file.
    pub fn editable(self) -> bool {
        matches!(self, Self::Missing | Self::User)
    }
}

/// How bad a problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A value or profile was not used (its default applies, or the profile is left out).
    Error,
    /// Used, but worth fixing (a secret-looking value, a default profile that does not exist).
    Warning,
}

/// One problem of a file, naming the key and its line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub key: String,
    pub line: Option<usize>,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "line": self.line,
            "severity": match self.severity { Severity::Error => "error", Severity::Warning => "warning" },
            "message": self.message,
        })
    }

    /// One line for people: `config.toml line 4: …`.
    pub fn text(&self) -> String {
        match self.line {
            Some(l) => format!("{FILE} line {l}: {}", self.message),
            None => format!("{FILE}: {}", self.message),
        }
    }
}

/// A parsed document.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub settings: Settings,
    pub state: State,
    pub problems: Vec<Problem>,
    /// Keys this build does not know (kept as they are), as `section.name` or `name`.
    pub unknown: Vec<String>,
    /// Which known keys the file sets.
    pub set_keys: Vec<String>,
}

/// The result of loading the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub path: PathBuf,
    pub parsed: Parsed,
    /// The file's text as read (`None`: missing or unreadable).
    pub text: Option<String>,
}

impl Loaded {
    pub fn settings(&self) -> &Settings {
        &self.parsed.settings
    }

    pub fn state(&self) -> State {
        self.parsed.state
    }

    /// The problem to show once in the window, in one sentence (errors first).
    pub fn notice(&self) -> Option<String> {
        let p = &self.parsed;
        let errors: Vec<&Problem> = p
            .problems
            .iter()
            .filter(|x| x.severity == Severity::Error)
            .collect();
        let first = errors.first().copied().or(p.problems.first())?;
        let more = p.problems.len() - 1;
        Some(if more == 0 {
            first.text()
        } else {
            format!(
                "{} (and {more} more; `subak config check` lists them)",
                first.text()
            )
        })
    }
}

/// The 1-based line of the `[table]` or `[[array]]` header that `line` opens, if it is one: its
/// name, and whether it is an array of tables.
fn header(line: &str) -> Option<(&str, bool)> {
    let t = line.trim();
    let (rest, array) = match t.strip_prefix("[[") {
        Some(r) => (r, true),
        None => (t.strip_prefix('[')?, false),
    };
    let close = if array { "]]" } else { "]" };
    let end = rest.find(close)?;
    let after = rest[end + close.len()..].trim();
    (after.is_empty() || after.starts_with('#')).then(|| (rest[..end].trim(), array))
}

/// Whether `line` assigns `name` (`name = …`, `"name" = …`).
fn assigns(line: &str, name: &str) -> bool {
    let t = line.trim_start();
    let rest = t
        .strip_prefix(name)
        .or_else(|| t.strip_prefix(&format!("\"{name}\"")));
    rest.is_some_and(|r| r.trim_start().starts_with('='))
}

/// The 1-based line where `section.name` is set (`section` `""`: the top level), if found.
fn line_of(text: &str, section: &str, name: &str) -> Option<usize> {
    let mut current = String::new();
    let mut in_array = false;
    for (i, line) in text.lines().enumerate() {
        if let Some((h, array)) = header(line) {
            current = h.to_string();
            in_array = array;
            continue;
        }
        if !in_array && current == section && assigns(line, name) {
            return Some(i + 1);
        }
    }
    None
}

/// The 1-based line of the `index`-th `[[profiles]]` header, and of `name` inside it.
fn profile_line(text: &str, index: usize, name: Option<&str>) -> Option<usize> {
    let mut seen = 0usize;
    let mut inside = false;
    let mut header_line = None;
    for (i, line) in text.lines().enumerate() {
        if let Some((h, array)) = header(line) {
            if inside {
                break;
            }
            if array && h == "profiles" {
                if seen == index {
                    inside = true;
                    header_line = Some(i + 1);
                    if name.is_none() {
                        return header_line;
                    }
                }
                seen += 1;
            }
            continue;
        }
        if inside && let Some(n) = name {
            let first = n.split('.').next().unwrap_or(n);
            if assigns(line, first) {
                return Some(i + 1);
            }
        }
    }
    header_line
}

/// Parses a document. Never fails: see the module rules.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed {
        settings: Settings::default(),
        state: State::User,
        problems: Vec::new(),
        unknown: Vec::new(),
        set_keys: Vec::new(),
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            let first = msg
                .lines()
                .next()
                .unwrap_or("parse error")
                .trim()
                .to_string();
            out.state = State::Invalid;
            out.problems.push(Problem {
                key: String::new(),
                line: toml_error_line(&msg),
                severity: Severity::Error,
                message: format!(
                    "the file is not valid TOML ({first}); the default settings apply and the file is left as it is"
                ),
            });
            return out;
        }
    };
    match table.get("config_version") {
        None | Some(toml::Value::Integer(VERSION)) => {}
        Some(toml::Value::Integer(n)) if *n > VERSION => {
            out.state = State::Newer;
            out.problems.push(Problem {
                key: "config_version".into(),
                line: line_of(text, "", "config_version"),
                severity: Severity::Error,
                message: format!(
                    "config_version {n} was written by a newer Subak (this one reads {VERSION}); it is read-only and the default settings apply"
                ),
            });
            return out;
        }
        Some(_) => {
            out.state = State::Invalid;
            out.problems.push(Problem {
                key: "config_version".into(),
                line: line_of(text, "", "config_version"),
                severity: Severity::Error,
                message: format!(
                    "config_version must be {VERSION}; the default settings apply and the file is left as it is"
                ),
            });
            return out;
        }
    }
    for (top, v) in &table {
        match top.as_str() {
            "config_version" => {}
            "profiles" => parse_profiles(text, v, &mut out),
            s if SECTIONS.contains(&s) => {
                let Some(t) = v.as_table() else {
                    out.problems.push(Problem {
                        key: s.into(),
                        line: None,
                        severity: Severity::Error,
                        message: format!("`{s}` must be a table ([{s}]); its defaults apply"),
                    });
                    continue;
                };
                for (name, value) in t {
                    match KEYS.iter().find(|k| k.section == s && k.name == name) {
                        Some(k) => {
                            out.set_keys.push(k.id());
                            if apply(&mut out.settings, k, value).is_err() {
                                out.problems.push(Problem {
                                    key: k.id(),
                                    line: line_of(text, s, name),
                                    severity: Severity::Error,
                                    message: format!(
                                        "`{}` must be {}; the default ({}) applies",
                                        k.id(),
                                        k.takes,
                                        default_text(k)
                                    ),
                                });
                            }
                        }
                        None => out.unknown.push(format!("{s}.{name}")),
                    }
                }
            }
            other => out.unknown.push(other.to_string()),
        }
    }
    // The default profile must exist.
    let d = out.settings.terminal.default_profile.clone();
    if !d.is_empty() && out.settings.profile(&d).is_none() {
        out.problems.push(Problem {
            key: "terminal.default_profile".into(),
            line: line_of(text, "terminal", "default_profile"),
            severity: Severity::Warning,
            message: format!(
                "no profile is named `{d}`; new Terminals use the Host's default shell"
            ),
        });
    }
    out
}

fn default_text(k: &Key) -> String {
    let v = Settings::default().value(k);
    match v {
        Value::String(s) if s.is_empty() => "\"\"".into(),
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// The line a TOML error message names (`--> line 3, column 5` / `at line 3`), if any.
fn toml_error_line(msg: &str) -> Option<usize> {
    let i = msg.find("line ")?;
    let digits: String = msg[i + 5..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn parse_profiles(text: &str, v: &toml::Value, out: &mut Parsed) {
    let Some(list) = v.as_array() else {
        out.problems.push(Problem {
            key: "profiles".into(),
            line: None,
            severity: Severity::Error,
            message: "`profiles` must be an array of tables ([[profiles]]); no profile is used"
                .into(),
        });
        return;
    };
    for (i, item) in list.iter().enumerate() {
        let line = profile_line(text, i, None);
        if out.settings.profiles.len() >= MAX_PROFILES {
            out.problems.push(Problem {
                key: format!("profiles[{i}]"),
                line,
                severity: Severity::Error,
                message: format!("at most {MAX_PROFILES} profiles are used; this one is left out"),
            });
            continue;
        }
        match parse_profile(item) {
            Ok(mut p) => {
                p.line = line;
                if out
                    .settings
                    .profiles
                    .iter()
                    .any(|q| q.name.to_lowercase() == p.name.to_lowercase())
                {
                    out.problems.push(Problem {
                        key: format!("profiles[{i}].name"),
                        line: profile_line(text, i, Some("name")),
                        severity: Severity::Error,
                        message: format!(
                            "another profile is already named `{}` (names ignore case); this one is left out",
                            p.name
                        ),
                    });
                    continue;
                }
                for part in p.secrets() {
                    let field = part.split('.').next().unwrap_or("env");
                    let what = part.strip_prefix("env.").map_or_else(
                        || format!("its `{part}` hold"),
                        |n| format!("it sets `{n}`, which looks"),
                    );
                    out.problems.push(Problem {
                        key: format!("profiles[{i}].{part}"),
                        line: profile_line(text, i, Some(field)),
                        severity: Severity::Warning,
                        message: format!(
                            "profile `{}`: {what} secret; secrets do not belong in {FILE} (shown as {}): let the program read them from your environment instead",
                            p.name,
                            redact::MASK
                        ),
                    });
                }
                out.settings.profiles.push(p);
            }
            Err((field, why)) => out.problems.push(Problem {
                key: format!(
                    "profiles[{i}]{}",
                    field.map(|f| format!(".{f}")).unwrap_or_default()
                ),
                line: field.and_then(|f| profile_line(text, i, Some(f))).or(line),
                severity: Severity::Error,
                message: format!("profile {}: {why}; it is left out", i + 1),
            }),
        }
    }
}

/// One profile, or the field that is wrong and why.
fn parse_profile(v: &toml::Value) -> Result<Profile, (Option<&'static str>, String)> {
    let t = v
        .as_table()
        .ok_or((None, "must be a table ([[profiles]])".to_string()))?;
    let text = |field: &'static str, required: bool| -> Result<Option<String>, _> {
        match t.get(field) {
            None if required => Err((Some(field), format!("`{field}` is required"))),
            None => Ok(None),
            Some(toml::Value::String(s)) if s.contains('\0') => {
                Err((Some(field), format!("`{field}` contains a NUL character")))
            }
            Some(toml::Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err((Some(field), format!("`{field}` must be a string"))),
        }
    };
    let name = text("name", true)?.unwrap_or_default();
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err((
            Some("name"),
            "`name` must be 1 to 64 characters without control characters".into(),
        ));
    }
    let program = text("program", true)?.unwrap_or_default();
    if program.trim().is_empty() {
        return Err((Some("program"), "`program` must not be empty".into()));
    }
    let args = match t.get("args") {
        None => Vec::new(),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().filter(|s| !s.contains('\0')).map(String::from))
            .collect::<Option<Vec<_>>>()
            .filter(|a| a.len() <= 256)
            .ok_or((
                Some("args"),
                "`args` must be a list of at most 256 strings".to_string(),
            ))?,
        Some(_) => {
            return Err((Some("args"), "`args` must be a list of strings".into()));
        }
    };
    let cwd = text("cwd", false)?;
    if let Some(c) = &cwd
        && !Path::new(c).is_absolute()
    {
        return Err((
            Some("cwd"),
            format!("`cwd` must be an absolute path (`{c}` is not)"),
        ));
    }
    let mut env = Vec::new();
    if let Some(e) = t.get("env") {
        let e = e.as_table().ok_or((
            Some("env"),
            "`env` must be a table of names to strings".to_string(),
        ))?;
        for (n, v) in e {
            crate::pty::check_env_name(n).map_err(|m| (Some("env"), m))?;
            let v = v
                .as_str()
                .filter(|s| !s.contains('\0'))
                .ok_or((Some("env"), format!("the value of `{n}` must be a string")))?;
            env.push((n.clone(), v.to_string()));
        }
    }
    let mut unset_env = Vec::new();
    if let Some(u) = t.get("unset_env") {
        let u = u.as_array().ok_or((
            Some("unset_env"),
            "`unset_env` must be a list of names".to_string(),
        ))?;
        for n in u {
            let n = n.as_str().ok_or((
                Some("unset_env"),
                "`unset_env` must be a list of names".to_string(),
            ))?;
            crate::pty::check_env_name(n).map_err(|m| (Some("unset_env"), m))?;
            unset_env.push(n.to_string());
        }
    }
    if env.len() + unset_env.len() > crate::pty::MAX_ENV_OVERRIDES {
        return Err((
            Some("env"),
            format!(
                "at most {} environment overrides are allowed",
                crate::pty::MAX_ENV_OVERRIDES
            ),
        ));
    }
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if let Some(n) = unset_env
        .iter()
        .find(|u| env.iter().any(|(e, _)| same(e, u)))
    {
        return Err((
            Some("unset_env"),
            format!("`{n}` is both set (env) and removed (unset_env)"),
        ));
    }
    if let Some(bad) = t
        .keys()
        .find(|k| !["name", "program", "args", "cwd", "env", "unset_env"].contains(&k.as_str()))
    {
        return Err((
            None,
            format!(
                "`{bad}` is not a profile field (name, program, args, cwd, env, unset_env; SSH and other profile kinds come in Step 33)"
            ),
        ));
    }
    Ok(Profile {
        name,
        program,
        args,
        cwd,
        env,
        unset_env,
        line: None,
    })
}

/// Loads `dir/config.toml`; a missing file (or directory) means the defaults.
pub fn load(dir: &Path) -> Loaded {
    let path = dir.join(FILE);
    let unreadable = |why: String| Loaded {
        path: path.clone(),
        parsed: Parsed {
            settings: Settings::default(),
            state: State::Unreadable,
            problems: vec![Problem {
                key: String::new(),
                line: None,
                severity: Severity::Error,
                message: format!("{FILE} cannot be read ({why}); the default settings apply"),
            }],
            unknown: Vec::new(),
            set_keys: Vec::new(),
        },
        text: None,
    };
    match std::fs::metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                path: path.clone(),
                parsed: Parsed {
                    settings: Settings::default(),
                    state: State::Missing,
                    problems: Vec::new(),
                    unknown: Vec::new(),
                    set_keys: Vec::new(),
                },
                text: None,
            };
        }
        Err(e) => return unreadable(e.to_string()),
        Ok(m) if m.is_dir() => return unreadable("it is a folder".into()),
        Ok(m) if m.len() > MAX_BYTES => {
            return unreadable(format!("it is larger than {MAX_BYTES} bytes"));
        }
        Ok(_) => {}
    }
    match std::fs::read(&path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Loaded {
                path,
                parsed: parse(&text),
                text: Some(text),
            },
            Err(_) => {
                let mut l = unreadable("it is not UTF-8 text".into());
                l.parsed.state = State::Invalid;
                l
            }
        },
        Err(e) => unreadable(e.to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// Editing.

/// A TOML basic string.
pub fn toml_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What an edit failed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// Not a key of this build.
    UnknownKey(String),
    /// Not a value of the key: the key and what it takes.
    BadValue(String, &'static str),
    /// The file cannot be edited in place without changing other keys.
    Layout(String),
    /// The file is invalid or newer (never rewritten by an edit).
    NotEditable(State, Vec<Problem>),
}

impl EditError {
    pub fn message(&self) -> String {
        match self {
            Self::UnknownKey(k) => format!("`{k}` is not a Subak setting"),
            Self::BadValue(k, takes) => format!("`{k}` must be {takes}"),
            Self::Layout(why) => format!(
                "{FILE} uses a layout Subak cannot edit in place ({why}); nothing was changed"
            ),
            Self::NotEditable(State::Newer, _) => {
                format!("{FILE} was written by a newer Subak and is read-only; nothing was changed")
            }
            Self::NotEditable(_, _) => format!("{FILE} is invalid; nothing was changed"),
        }
    }
}

/// The TOML literal of `value` for key `k`, after checking it.
pub fn literal(k: &Key, value: &str) -> Result<String, EditError> {
    let bad = || EditError::BadValue(k.id(), k.takes);
    let v = match k.kind {
        Kind::Text => toml::Value::String(value.to_string()),
        Kind::Bool => match value.trim() {
            "true" | "on" => toml::Value::Boolean(true),
            "false" | "off" => toml::Value::Boolean(false),
            _ => return Err(bad()),
        },
        Kind::Integer => toml::Value::Integer(value.trim().parse::<i64>().map_err(|_| bad())?),
        Kind::Number => {
            let n: f64 = value.trim().parse().map_err(|_| bad())?;
            if !n.is_finite() {
                return Err(bad());
            }
            toml::Value::Float(n)
        }
    };
    let mut probe = Settings::default();
    apply(&mut probe, k, &v).map_err(|()| bad())?;
    Ok(match v {
        toml::Value::String(s) => toml_string(&s),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => format_number(f),
        _ => unreachable!("only strings, booleans and numbers are written"),
    })
}

/// A number as TOML writes it: whole numbers as integers (`15`), others with up to two decimals.
fn format_number(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e9 {
        format!("{}", f as i64)
    } else {
        let s = format!("{f:.2}");
        s.trim_end_matches('0').to_string()
    }
}

/// `text` with `section.name` set to `literal` (`None`: the line removed). The key's line is
/// replaced in place (a trailing comment is kept); a new key goes after the last line of its
/// section; a missing section is appended.
fn edit_text(text: &str, section: &str, name: &str, literal: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut current = String::new();
    let mut in_array = false;
    let mut section_end: Option<usize> = None;
    let mut found: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some((h, array)) = header(line) {
            current = h.to_string();
            in_array = array;
            if current == section && !array {
                section_end = Some(i + 1);
            }
            continue;
        }
        if current == section && !in_array {
            if !line.trim().is_empty() && !line.trim_start().starts_with('#') {
                section_end = Some(i + 1);
            }
            if found.is_none() && assigns(line, name) {
                found = Some(i);
            }
        }
    }
    match (found, literal) {
        (Some(i), Some(l)) => {
            let comment = trailing_comment(&lines[i]);
            let indent: String = lines[i].chars().take_while(|c| c.is_whitespace()).collect();
            lines[i] = format!("{indent}{name} = {l}{comment}");
        }
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, None) => {}
        (None, Some(l)) => match section_end {
            Some(end) => lines.insert(end, format!("{name} = {l}")),
            None => {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push(format!("[{section}]"));
                lines.push(format!("{name} = {l}"));
            }
        },
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// The ` # comment` after a simple value on `line` (none for strings holding `#`).
fn trailing_comment(line: &str) -> String {
    let Some(eq) = line.find('=') else {
        return String::new();
    };
    let value = &line[eq + 1..];
    let mut in_str: Option<char> = None;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        match in_str {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    in_str = None;
                }
            }
            None if c == '"' || c == '\'' => in_str = Some(c),
            None if c == '#' => {
                let before = &value[..i];
                let pad: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                return format!("{pad}{}", &value[i..]);
            }
            None => {}
        }
    }
    String::new()
}

/// One change of an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub key: String,
    /// The value before, as TOML text (`None`: not in the file, so the default applied).
    pub before: Option<String>,
    pub after: Option<String>,
}

impl Change {
    pub fn to_json(&self) -> Value {
        json!({"key": self.key, "before": self.before, "after": self.after})
    }
}

/// An edit's new text and what changed (nothing is written yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub text: String,
    pub changes: Vec<Change>,
}

fn value_text(v: Option<&toml::Value>) -> Option<String> {
    Some(match v? {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::String(s) => toml_string(s),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => format_number(*f),
        other => format!("{other:?}"),
    })
}

/// The text a first edit starts from when there is no file.
pub fn new_file_text() -> String {
    format!(
        "# Subak settings (docs/SUBAK_GUI.md §13). Edit by hand or with `subak config set`;\n\
         # `subak config check` validates this file. Keys you do not set use their defaults.\n\
         config_version = {VERSION}\n"
    )
}

/// The current text for an edit: the file's, or [`new_file_text`] when it is missing; an
/// invalid or newer file is refused (it is never rewritten by an edit).
pub fn editable_text(loaded: &Loaded) -> Result<String, EditError> {
    match loaded.state() {
        State::Missing => Ok(new_file_text()),
        State::User => Ok(loaded.text.clone().unwrap_or_else(new_file_text)),
        s => Err(EditError::NotEditable(s, loaded.parsed.problems.clone())),
    }
}

/// Applies `edits` (`(key, Some(literal))` sets, `None` removes) to `text`, checked: the
/// result parses and differs from `text` in exactly the edited keys.
pub fn checked_edit(text: &str, edits: &[(&Key, Option<String>)]) -> Result<Edited, EditError> {
    let before: toml::Table = text
        .parse()
        .map_err(|_| EditError::Layout("it is not valid TOML".into()))?;
    let mut out = text.to_string();
    let mut changes = Vec::new();
    for (k, lit) in edits {
        out = edit_text(&out, k.section, k.name, lit.as_deref());
        let old = before
            .get(k.section)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(k.name));
        changes.push(Change {
            key: k.id(),
            before: value_text(old),
            after: lit.clone(),
        });
    }
    let after: toml::Table = out
        .parse()
        .map_err(|_| EditError::Layout("the result would not parse".into()))?;
    let strip = |t: &toml::Table| {
        let mut t = t.clone();
        for (k, _) in edits {
            if let Some(toml::Value::Table(s)) = t.get_mut(k.section) {
                s.remove(k.name);
                if s.is_empty() {
                    t.remove(k.section);
                }
            }
        }
        t
    };
    if strip(&before) != strip(&after) {
        return Err(EditError::Layout("other keys would change".into()));
    }
    for (k, lit) in edits {
        let got = after
            .get(k.section)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(k.name));
        if value_text(got) != *lit {
            return Err(EditError::Layout(format!(
                "`{}` is defined in another form",
                k.id()
            )));
        }
    }
    changes.retain(|c| c.before != c.after);
    Ok(Edited { text: out, changes })
}

/// `subak config set`: the edited text for `values` (`(key, value)`), checked; nothing is
/// written here.
pub fn set(loaded: &Loaded, values: &[(&str, &str)]) -> Result<Edited, EditError> {
    let mut edits = Vec::new();
    for (id, value) in values {
        let k = key(id).ok_or_else(|| EditError::UnknownKey((*id).to_string()))?;
        edits.push((k, Some(literal(k, value)?)));
    }
    let text = editable_text(loaded)?;
    checked_edit(&text, &edits)
}

/// Sets key `id` to `value` in `s` with the same checks as [`set`] (the window applies a menu
/// choice at once, then saves it with [`set`]).
pub fn set_value(s: &mut Settings, id: &str, value: &str) -> Result<(), EditError> {
    let k = key(id).ok_or_else(|| EditError::UnknownKey(id.to_string()))?;
    let lit = literal(k, value)?;
    let v = format!("x = {lit}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut t| t.remove("x"))
        .ok_or(EditError::BadValue(k.id(), k.takes))?;
    apply(s, k, &v).map_err(|()| EditError::BadValue(k.id(), k.takes))
}

/// What `subak config reset` resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset<'a> {
    Key(&'a Key),
    Section(&'a str),
    All,
}

impl<'a> Reset<'a> {
    /// A key ID, a section name, or `None` for everything.
    pub fn parse(what: Option<&'a str>) -> Result<Self, EditError> {
        match what {
            None => Ok(Self::All),
            Some(s) if SECTIONS.contains(&s) => Ok(Self::Section(s)),
            Some(id) => key(id)
                .map(Self::Key)
                .ok_or_else(|| EditError::UnknownKey(id.to_string())),
        }
    }
}

/// `subak config reset`: the text with the keys removed, so their defaults apply; unknown keys,
/// profiles and comments stay. `Reset::All` of an invalid file gives a new default file (the
/// caller backs the old one up first).
pub fn reset(loaded: &Loaded, what: Reset<'_>) -> Result<Edited, EditError> {
    if what == Reset::All && matches!(loaded.state(), State::Invalid | State::Unreadable) {
        return Ok(Edited {
            text: new_file_text(),
            changes: vec![Change {
                key: "*".into(),
                before: Some("(the invalid file, kept as a backup)".into()),
                after: None,
            }],
        });
    }
    let text = editable_text(loaded)?;
    let edits: Vec<(&Key, Option<String>)> = KEYS
        .iter()
        .filter(|k| match what {
            Reset::Key(one) => *k == one,
            Reset::Section(s) => k.section == s,
            Reset::All => true,
        })
        .map(|k| (k, None))
        .collect();
    checked_edit(&text, &edits)
}

/// Writes `text` to `path` atomically: a temporary file next to it, flushed to disk, then
/// renamed over the old one.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!("{name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A file-name-safe UTC time: `20261104T120000Z`.
pub fn stamp(t: std::time::SystemTime) -> String {
    crate::proto::rfc3339(t).replace(['-', ':'], "")
}

/// Copies `file` (in `dir`) to `dir\backups\<stem>-<UTC time>.<ext>` and keeps the newest
/// [`BACKUPS_KEPT`] backups of that stem. Returns the backup's path, or `None` when there is no
/// file to back up.
pub fn backup(dir: &Path, file: &str, tag: &str) -> std::io::Result<Option<PathBuf>> {
    let src = dir.join(file);
    if !src.is_file() {
        return Ok(None);
    }
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, "bak"));
    let stem = if tag.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}-{tag}")
    };
    let folder = dir.join(BACKUP_DIR);
    std::fs::create_dir_all(&folder)?;
    let now = stamp(std::time::SystemTime::now());
    // Backups made within the same second count up from the highest counter there is (an older
    // one of that second may already have been pruned, and its name must not come back as the
    // oldest).
    let taken = backups(&folder, &stem, ext)
        .into_iter()
        .filter(|(t, _, _)| *t == now)
        .map(|(_, n, _)| n)
        .max();
    let dest = match taken {
        None => folder.join(format!("{stem}-{now}.{ext}")),
        Some(n) => folder.join(format!("{stem}-{now}-{}.{ext}", n + 1)),
    };
    std::fs::copy(&src, &dest)?;
    prune(&folder, &stem, ext);
    Ok(Some(dest))
}

/// Keeps the newest [`BACKUPS_KEPT`] backups named `<stem>-<time>.<ext>`.
fn prune(folder: &Path, stem: &str, ext: &str) {
    for (_, _, p) in backups(folder, stem, ext).into_iter().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(p);
    }
}

/// The backups named `<stem>-<time>[-<n>].<ext>`, newest first: `(time, n, path)`.
fn backups(folder: &Path, stem: &str, ext: &str) -> Vec<(String, u32, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let prefix = format!("{stem}-");
    let suffix = format!(".{ext}");
    // Ordered by the time in the name, then the counter of backups made in the same second.
    let mut found: Vec<(String, u32, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
            // `<time>` or `<time>-<n>` only: another stem's backups (`config-invalid-…`) differ.
            rest.starts_with(|c: char| c.is_ascii_digit())
                .then_some(())?;
            let (time, n) = match rest.split_once('-') {
                Some((t, n)) => (t.to_string(), n.parse().ok()?),
                None => (rest.to_string(), 1),
            };
            Some((time, n, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    found
}

// ---------------------------------------------------------------------------------------------
// Export and import.

/// The file's text with every secret-looking profile value replaced by `***`, and how many were
/// replaced (`subak config export`, for sharing; `KIM-SEC-013`).
pub fn redacted_text(text: &str, settings: &Settings) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut n = 0;
    // (the value as written, what replaces it): secret-looking environment values, and every
    // argument the detector hides something in (`--password x`).
    let mut secrets: Vec<(String, String)> = settings
        .profiles
        .iter()
        .flat_map(|p| p.env.iter())
        .filter(|(k, v)| secret_looking(k, v))
        .map(|(_, v)| (v.clone(), redact::MASK.to_string()))
        .collect();
    for p in &settings.profiles {
        for (a, r) in p.args.iter().zip(redact::args(&p.args)) {
            if *a != r {
                secrets.push((a.clone(), r));
            }
        }
    }
    for line in text.lines() {
        let mut l = line.to_string();
        for (v, r) in &secrets {
            if v.is_empty() || v == redact::MASK {
                continue;
            }
            let quoted = toml_string(v);
            if l.contains(&quoted) {
                l = l.replace(&quoted, &toml_string(r));
                n += 1;
            } else if l.contains(v.as_str()) {
                l = l.replace(v.as_str(), r);
                n += 1;
            }
        }
        // Anything else the detector finds (a token in an argument, a URL password).
        let r = redact::text(&l);
        if r != l {
            n += 1;
            l = r;
        }
        out.push_str(&l);
        out.push('\n');
    }
    (out, n)
}

/// Why a document cannot be imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    Invalid(Vec<Problem>),
    Newer(Vec<Problem>),
    /// Secret-looking environment values: `profile.NAME` (never written, `KIM-SEC-013`).
    Secrets(Vec<String>),
}

/// Checks a document before `subak config import` writes it: it must parse with no error, be of
/// this version or older, and hold no secret-looking value (redacted `***` values included).
pub fn check_import(text: &str) -> Result<Parsed, ImportError> {
    let parsed = parse(text);
    match parsed.state {
        State::Newer => return Err(ImportError::Newer(parsed.problems)),
        State::Invalid | State::Unreadable => return Err(ImportError::Invalid(parsed.problems)),
        _ => {}
    }
    let errors: Vec<Problem> = parsed
        .problems
        .iter()
        .filter(|p| p.severity == Severity::Error)
        .cloned()
        .collect();
    if !errors.is_empty() {
        return Err(ImportError::Invalid(errors));
    }
    let secrets: Vec<String> = parsed
        .settings
        .profiles
        .iter()
        .flat_map(|p| {
            p.secrets()
                .into_iter()
                .map(move |part| format!("{}.{part}", p.name))
        })
        .collect();
    if !secrets.is_empty() {
        return Err(ImportError::Secrets(secrets));
    }
    Ok(parsed)
}

/// What differs between two settings, key by key (profiles by name).
pub fn differences(before: &Settings, after: &Settings) -> Vec<Change> {
    let mut out = Vec::new();
    for k in KEYS {
        let (a, b) = (before.value(k), after.value(k));
        if a != b {
            out.push(Change {
                key: k.id(),
                before: Some(a.to_string()),
                after: Some(b.to_string()),
            });
        }
    }
    let names =
        |s: &Settings| -> Vec<String> { s.profiles.iter().map(|p| p.name.clone()).collect() };
    for p in &before.profiles {
        match after.profile(&p.name) {
            None => out.push(Change {
                key: format!("profiles.{}", p.name),
                before: Some("defined".into()),
                after: None,
            }),
            Some(q)
                if q != p
                    && (q.program != p.program
                        || q.args != p.args
                        || q.cwd != p.cwd
                        || q.env != p.env
                        || q.unset_env != p.unset_env) =>
            {
                out.push(Change {
                    key: format!("profiles.{}", p.name),
                    before: Some("defined".into()),
                    after: Some("changed".into()),
                });
            }
            Some(_) => {}
        }
    }
    for n in names(after) {
        if before.profile(&n).is_none() {
            out.push(Change {
                key: format!("profiles.{n}"),
                before: None,
                after: Some("defined".into()),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The gui.json v1 migration (Step 20).

/// What the window's `gui.json` version 1 held that moved here: its text size and theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Legacy {
    pub font_size: Option<f32>,
    pub theme: Option<ThemeChoice>,
}

/// The edit that moves `legacy` into `config.toml`: only keys the file does not set yet (a
/// value already in `config.toml` wins). `None` when there is nothing to move.
pub fn adopt(loaded: &Loaded, legacy: Legacy) -> Result<Option<Edited>, EditError> {
    let text = editable_text(loaded)?;
    let set = |id: &str| loaded.parsed.set_keys.iter().any(|k| k == id);
    let mut edits: Vec<(&Key, Option<String>)> = Vec::new();
    if let (Some(f), false) = (legacy.font_size, set("appearance.font_size"))
        && let Some(k) = key("appearance.font_size")
        && (f - FONT_SIZE.2).abs() > f32::EPSILON
    {
        edits.push((k, Some(literal(k, &f.to_string())?)));
    }
    if let (Some(t), false) = (legacy.theme, set("appearance.theme"))
        && let Some(k) = key("appearance.theme")
        && t != ThemeChoice::Auto
    {
        edits.push((k, Some(literal(k, t.as_str())?)));
    }
    if edits.is_empty() {
        return Ok(None);
    }
    checked_edit(&text, &edits).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "subak-config-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn loaded(text: &str) -> Loaded {
        Loaded {
            path: PathBuf::from(FILE),
            parsed: parse(text),
            text: Some(text.to_string()),
        }
    }

    const SAMPLE: &str = r#"# my settings
config_version = 1

[appearance]
theme = "kim-light"   # easier on paper
font_family = "Consolas"
font_size = 15
line_height = 1.4
padding = 4
opacity = 90
motion = "reduced"
future_knob = "kept"

[terminal]
default_profile = "PowerShell"
copy_on_select = true
smart_ctrl_c = true
right_click = "menu"
middle_click = "paste"
word_separators = " ()"

[[profiles]]
name = "PowerShell"
program = "powershell.exe"
args = ["-NoLogo", "-NoProfile"]
cwd = 'C:\work'
env = { RUST_BACKTRACE = "1", GREETING = "안녕" }
unset_env = ["HTTP_PROXY"]

[[profiles]]
name = "cmd"
program = "cmd.exe"

[later_section]
x = 1
"#;

    #[test]
    fn missing_file_is_the_defaults() {
        let d = sandbox("missing");
        let l = load(&d);
        assert_eq!(l.state(), State::Missing);
        assert_eq!(*l.settings(), Settings::default());
        assert!(l.parsed.problems.is_empty());
        assert_eq!(l.notice(), None);
        let s = Settings::default();
        assert_eq!(s.appearance.font_size, 13.0);
        assert_eq!(s.appearance.line_height, 1.2);
        assert_eq!(s.appearance.padding, 8);
        assert_eq!(s.appearance.opacity, 100);
        assert!(!s.terminal.copy_on_select && !s.terminal.smart_ctrl_c);
        assert_eq!(s.terminal.right_click, RightClick::CopyPaste);
        assert_eq!(
            s.terminal.word_separators,
            subak_term::view::WORD_DELIMITERS
        );
    }

    #[test]
    fn every_setting_and_profile_is_read() {
        let p = parse(SAMPLE);
        assert_eq!(p.state, State::User);
        assert!(p.problems.is_empty(), "{:?}", p.problems);
        let s = &p.settings;
        assert_eq!(s.appearance.theme, ThemeChoice::KimLight);
        assert_eq!(s.appearance.font_family, "Consolas");
        assert_eq!(s.appearance.font_size, 15.0);
        assert_eq!(s.appearance.line_height, 1.4);
        assert_eq!(s.appearance.padding, 4);
        assert_eq!(s.appearance.opacity, 90);
        assert_eq!(s.appearance.motion, Motion::Reduced);
        assert!(s.terminal.copy_on_select && s.terminal.smart_ctrl_c);
        assert_eq!(s.terminal.right_click, RightClick::Menu);
        assert_eq!(s.terminal.middle_click, MiddleClick::Paste);
        assert_eq!(s.terminal.word_separators, " ()");
        assert_eq!(s.profiles.len(), 2);
        let ps = s.default_profile().unwrap();
        assert_eq!(ps.name, "PowerShell");
        assert_eq!(ps.args, ["-NoLogo", "-NoProfile"]);
        assert_eq!(ps.cwd.as_deref(), Some("C:\\work"));
        assert_eq!(ps.line, Some(22));
        assert!(s.profile("CMD").is_some(), "names ignore case");
        assert_eq!(p.unknown, ["appearance.future_knob", "later_section"]);
        // The profile becomes explicit terminal.create parameters (argv exact, cwd, env).
        let c = ps.create_params();
        assert_eq!(c["command"]["program"], "powershell.exe");
        assert_eq!(c["command"]["args"], json!(["-NoLogo", "-NoProfile"]));
        assert_eq!(c["cwd"], "C:\\work");
        assert_eq!(c["env"]["GREETING"], "안녕");
        assert_eq!(c["env"]["HTTP_PROXY"], Value::Null, "removed");
        let parsed = crate::pty::EnvOverrides::from_json(&c["env"]).unwrap();
        assert_eq!(parsed.vars.len(), 3);
        let cmd = s.profile("cmd").unwrap().create_params();
        assert_eq!(cmd["env"], Value::Null);
        assert_eq!(cmd["cwd"], Value::Null);
    }

    #[test]
    fn invalid_values_fall_back_one_by_one_naming_key_and_line() {
        let text = "config_version = 1\n[appearance]\nfont_size = 200\ntheme = \"neon\"\nopacity = 20\nline_height = 1.5\n[terminal]\nright_click = 3\ndefault_profile = \"nope\"\n";
        let p = parse(text);
        assert_eq!(p.state, State::User);
        let s = &p.settings;
        assert_eq!(s.appearance.font_size, 13.0);
        assert_eq!(s.appearance.theme, ThemeChoice::Auto);
        assert_eq!(s.appearance.opacity, 100);
        assert_eq!(s.appearance.line_height, 1.5, "the valid one is used");
        assert_eq!(s.terminal.right_click, RightClick::CopyPaste);
        let by = |k: &str| p.problems.iter().find(|x| x.key == k).unwrap().clone();
        assert_eq!(by("appearance.font_size").line, Some(3));
        assert!(by("appearance.font_size").message.contains("8 to 36"));
        assert_eq!(by("appearance.theme").line, Some(4));
        assert_eq!(by("appearance.opacity").line, Some(5));
        assert_eq!(by("terminal.right_click").line, Some(8));
        let d = by("terminal.default_profile");
        assert_eq!((d.severity, d.line), (Severity::Warning, Some(9)));
        assert!(d.text().starts_with("config.toml line 9: "));
        let l = loaded(text);
        assert!(
            l.notice().unwrap().contains("and 4 more"),
            "{:?}",
            l.notice()
        );
    }

    #[test]
    fn broken_or_newer_files_are_defaults_and_never_edited() {
        for (text, state, needle) in [
            ("[appearance\ntheme=", State::Invalid, "not valid TOML"),
            ("config_version = \"one\"", State::Invalid, "must be 1"),
            (
                "config_version = 2\n[appearance]\ntheme = \"kim-light\"",
                State::Newer,
                "newer Subak",
            ),
        ] {
            let l = loaded(text);
            assert_eq!(l.state(), state, "{text}");
            assert_eq!(*l.settings(), Settings::default());
            assert!(l.notice().unwrap().contains(needle), "{:?}", l.notice());
            assert!(!state.editable());
            let e = set(&l, &[("appearance.theme", "kim-dark")]).unwrap_err();
            assert!(
                matches!(e, EditError::NotEditable(s, _) if s == state),
                "{e:?}"
            );
        }
        // The line of a TOML syntax error is named.
        let p = parse("config_version = 1\n[appearance]\ntheme = \n");
        assert_eq!(p.problems[0].line, Some(3), "{:?}", p.problems);
    }

    #[test]
    fn bad_profiles_are_left_out_with_a_reason() {
        let text = r#"
[[profiles]]
name = "ok"
program = "cmd.exe"

[[profiles]]
name = "no program"

[[profiles]]
name = "relative"
program = "pwsh"
cwd = "src"

[[profiles]]
name = "OK"
program = "cmd.exe"

[[profiles]]
name = "ssh"
program = "ssh.exe"
host = "example"

[[profiles]]
name = "both"
program = "cmd.exe"
env = { A = "1" }
unset_env = ["a"]

[[profiles]]
name = "reserved"
program = "cmd.exe"
env = { SUBAK_PANE_ID = "x" }
"#;
        let p = parse(text);
        let names: Vec<&str> = p
            .settings
            .profiles
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(names, ["ok"]);
        let msgs: Vec<String> = p.problems.iter().map(Problem::text).collect();
        assert!(
            msgs[0].contains("line 6") && msgs[0].contains("`program` is required"),
            "{msgs:?}"
        );
        assert!(
            msgs[1].contains("line 12") && msgs[1].contains("absolute"),
            "{msgs:?}"
        );
        assert!(msgs[2].contains("already named `OK`"), "{msgs:?}");
        assert!(msgs[3].contains("Step 33"), "{msgs:?}");
        assert!(msgs[4].contains("both set"), "{msgs:?}");
        assert!(msgs[5].contains("SUBAK_PANE_ID"), "{msgs:?}");
    }

    #[test]
    fn secret_looking_values_are_warned_about_and_never_shown() {
        let text = r#"
[[profiles]]
name = "dev"
program = "cmd.exe"
env = { GITHUB_TOKEN = "ghp_abcdefghijklmnopqrstuvwxyz0123456789", MODE = "fast", PROXY = "http://me:hunter2@proxy:8080" }
args = ["/k", "--password", "hunter2"]
"#;
        let p = parse(text);
        let dev = &p.settings.profiles[0];
        assert_eq!(dev.secret_env(), ["GITHUB_TOKEN", "PROXY"]);
        let warned: Vec<&str> = p.problems.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(
            warned,
            [
                "profiles[0].env.GITHUB_TOKEN",
                "profiles[0].env.PROXY",
                "profiles[0].args"
            ]
        );
        assert_eq!(p.problems[2].line, Some(6), "the args line");
        assert!(p.problems.iter().all(|x| x.severity == Severity::Warning));
        assert!(!format!("{:?}", p.problems).contains("ghp_abc"));
        let shown = dev.to_json(false).to_string();
        assert!(
            !shown.contains("ghp_abc") && !shown.contains("hunter2"),
            "{shown}"
        );
        assert!(shown.contains("\"MODE\":\"fast\""));
        assert!(shown.contains("***"));
        // The profile still works with its real values (the user's own file).
        assert_eq!(dev.create_params()["env"]["MODE"], "fast");
        // Export redacts; import refuses secrets, redacted or not.
        let (out, n) = redacted_text(text, &p.settings);
        assert!(n >= 2, "{out}");
        assert!(
            !out.contains("ghp_abc") && !out.contains("hunter2"),
            "{out}"
        );
        assert!(
            matches!(check_import(text), Err(ImportError::Secrets(s)) if s == ["dev.env.GITHUB_TOKEN", "dev.env.PROXY", "dev.args"])
        );
        assert!(matches!(check_import(&out), Err(ImportError::Secrets(_))));
        assert!(check_import(SAMPLE).is_ok());
        assert!(matches!(
            check_import("config_version = 9"),
            Err(ImportError::Newer(_))
        ));
        assert!(matches!(
            check_import("[appearance]\nfont_size = 99"),
            Err(ImportError::Invalid(_))
        ));
    }

    #[test]
    fn set_edits_lines_in_place_keeping_comments_unknown_keys_and_profiles() {
        let l = loaded(SAMPLE);
        let e = set(
            &l,
            &[
                ("appearance.theme", "kim-dark"),
                ("appearance.font_size", "16.5"),
            ],
        )
        .unwrap();
        assert!(
            e.text.contains("theme = \"kim-dark\"   # easier on paper"),
            "{}",
            e.text
        );
        assert!(e.text.contains("font_size = 16.5\n"));
        assert!(e.text.contains("future_knob = \"kept\""));
        assert!(e.text.contains("[later_section]\nx = 1"));
        assert!(e.text.starts_with("# my settings\n"));
        assert_eq!(e.changes.len(), 2);
        assert_eq!(e.changes[0].before.as_deref(), Some("\"kim-light\""));
        let again = parse(&e.text);
        assert_eq!(again.settings.appearance.theme, ThemeChoice::KimDark);
        assert_eq!(again.settings.appearance.font_size, 16.5);
        assert_eq!(again.settings.profiles, parse(SAMPLE).settings.profiles);
        // A new key goes into its section; a new section is appended.
        let e = set(&l, &[("appearance.bold_is_bright", "false")]).unwrap();
        let i = e.text.find("bold_is_bright = false").unwrap();
        assert!(i < e.text.find("[terminal]").unwrap(), "{}", e.text);
        let fresh = loaded("config_version = 1\n");
        let e = set(&fresh, &[("terminal.copy_on_select", "on")]).unwrap();
        assert_eq!(
            e.text,
            "config_version = 1\n\n[terminal]\ncopy_on_select = true\n"
        );
        // The same value: no change.
        let e = set(&l, &[("appearance.padding", "4")]).unwrap();
        assert!(e.changes.is_empty());
        // Refusals.
        assert_eq!(
            set(&l, &[("appearance.color", "x")]).unwrap_err(),
            EditError::UnknownKey("appearance.color".into())
        );
        for (k, v) in [
            ("appearance.font_size", "7"),
            ("appearance.font_size", "big"),
            ("appearance.line_height", "2.5"),
            ("appearance.padding", "4.5"),
            ("appearance.opacity", "10"),
            ("appearance.theme", "neon"),
            ("terminal.copy_on_select", "maybe"),
            (
                "appearance.font_family",
                "A very long font family name that goes on",
            ),
            ("terminal.word_separators", "a\u{7}b"),
        ] {
            assert!(
                matches!(set(&l, &[(k, v)]), Err(EditError::BadValue(..))),
                "{k}={v}"
            );
        }
        // A key defined as a dotted key at the top level cannot be edited in place.
        let dotted = loaded("appearance.theme = \"kim-dark\"\n");
        assert!(matches!(
            set(&dotted, &[("appearance.theme", "kim-light")]),
            Err(EditError::Layout(_))
        ));
        // A missing file starts from the header.
        let missing = Loaded {
            path: FILE.into(),
            parsed: Parsed {
                state: State::Missing,
                ..parse("")
            },
            text: None,
        };
        let e = set(&missing, &[("appearance.theme", "system")]).unwrap();
        assert!(e.text.starts_with("# Subak settings"), "{}", e.text);
        assert_eq!(
            parse(&e.text).settings.appearance.theme,
            ThemeChoice::System
        );
    }

    #[test]
    fn reset_removes_keys_and_replaces_only_an_invalid_file() {
        let l = loaded(SAMPLE);
        let e = reset(&l, Reset::parse(Some("appearance.theme")).unwrap()).unwrap();
        assert!(!e.text.contains("theme ="));
        assert!(e.text.contains("font_family"));
        let e = reset(&l, Reset::parse(Some("terminal")).unwrap()).unwrap();
        assert!(!e.text.contains("right_click") && e.text.contains("[terminal]"));
        assert!(e.text.contains("[[profiles]]"), "profiles stay");
        let e = reset(&l, Reset::All).unwrap();
        let p = parse(&e.text);
        assert_eq!(p.settings.appearance, Settings::default().appearance);
        assert_eq!(p.settings.profiles.len(), 2);
        assert_eq!(p.unknown.len(), 2);
        assert!(Reset::parse(Some("nope")).is_err());
        let bad = loaded("[[[");
        assert!(reset(&bad, Reset::parse(Some("appearance")).unwrap()).is_err());
        let e = reset(&bad, Reset::All).unwrap();
        assert_eq!(e.text, new_file_text());
        let newer = loaded("config_version = 3");
        assert!(
            reset(&newer, Reset::All).is_err(),
            "a newer file is never replaced by reset"
        );
    }

    #[test]
    fn writes_are_atomic_and_backups_are_kept_and_pruned() {
        let d = sandbox("backups");
        let path = d.join(FILE);
        assert_eq!(backup(&d, FILE, "").unwrap(), None, "nothing to back up");
        write_atomic(&path, SAMPLE).unwrap();
        assert!(!d.join("config.toml.tmp").exists());
        assert_eq!(load(&d).settings().appearance.theme, ThemeChoice::KimLight);
        let mut made = Vec::new();
        for i in 0..8 {
            std::fs::write(&path, format!("# v{i}\n")).unwrap();
            made.push(backup(&d, FILE, "").unwrap().unwrap());
        }
        let kept: Vec<String> = std::fs::read_dir(d.join(BACKUP_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept.len(), BACKUPS_KEPT, "{kept:?}");
        let newest = made.last().unwrap();
        assert_eq!(
            std::fs::read_to_string(newest)
                .unwrap_or_else(|e| panic!("{e}: {made:?} kept {kept:?}")),
            "# v7
"
        );
        assert!(
            newest
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config-2")
        );
        // Another stem's backups are pruned separately.
        let b = backup(&d, FILE, "invalid").unwrap().unwrap();
        assert!(
            b.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config-invalid-")
        );
        assert!(b.exists());
        assert_eq!(stamp(std::time::UNIX_EPOCH), "19700101T000000Z");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unreadable_and_non_utf8_files() {
        let d = sandbox("unreadable");
        std::fs::create_dir_all(d.join(FILE)).unwrap();
        assert_eq!(load(&d).state(), State::Unreadable);
        let d2 = sandbox("latin1");
        std::fs::write(d2.join(FILE), b"theme = \"\xff\"").unwrap();
        assert_eq!(load(&d2).state(), State::Invalid);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn the_gui_json_v1_values_move_only_where_the_file_is_silent() {
        let empty = Loaded {
            path: FILE.into(),
            parsed: Parsed {
                state: State::Missing,
                ..parse("")
            },
            text: None,
        };
        let legacy = Legacy {
            font_size: Some(15.0),
            theme: Some(ThemeChoice::KimDark),
        };
        let e = adopt(&empty, legacy).unwrap().unwrap();
        let p = parse(&e.text);
        assert_eq!(p.settings.appearance.font_size, 15.0);
        assert_eq!(p.settings.appearance.theme, ThemeChoice::KimDark);
        // config.toml already says something: it wins.
        let l = loaded(SAMPLE);
        assert_eq!(adopt(&l, legacy).unwrap(), None);
        // Defaults need no line.
        let defaults = Legacy {
            font_size: Some(13.0),
            theme: Some(ThemeChoice::Auto),
        };
        assert_eq!(adopt(&empty, defaults).unwrap(), None);
        assert!(adopt(&loaded("[["), legacy).is_err());
    }

    #[test]
    fn differences_name_keys_and_profiles() {
        let a = parse(SAMPLE).settings;
        let mut b = a.clone();
        b.appearance.opacity = 80;
        b.profiles.remove(1);
        b.profiles[0].args.push("-X".into());
        b.profiles.push(Profile {
            name: "new".into(),
            program: "x".into(),
            args: vec![],
            cwd: None,
            env: vec![],
            unset_env: vec![],
            line: None,
        });
        let d: Vec<String> = differences(&a, &b).iter().map(|c| c.key.clone()).collect();
        assert_eq!(
            d,
            [
                "appearance.opacity",
                "profiles.PowerShell",
                "profiles.cmd",
                "profiles.new"
            ]
        );
    }

    #[test]
    fn the_json_form_matches_its_schema() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../docs/contracts/schemas/subak-config-v1.schema.json"
        ))
        .unwrap();
        for s in [Settings::default(), parse(SAMPLE).settings] {
            let doc = s.to_json();
            let errors = crate::jsonschema::Validator::new(&schema)
                .validate(&doc)
                .unwrap();
            assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
        }
    }

    /// Step 20: which theme draws, from the setting and what Windows reports (High Contrast,
    /// the light app mode); and a menu choice applied in memory with the file's checks.
    #[test]
    fn themes_resolve_and_values_apply_in_memory() {
        use ThemeChoice::*;
        for (c, hc, light, want) in [
            (Auto, false, false, "kim-dark"),
            (Auto, false, true, "kim-dark"),
            (Auto, true, false, "high-contrast"),
            (System, false, true, "kim-light"),
            (System, false, false, "kim-dark"),
            (System, true, true, "high-contrast"),
            (KimDark, true, true, "kim-dark"),
            (KimLight, true, false, "kim-light"),
            (HighContrast, false, false, "high-contrast"),
        ] {
            assert_eq!(c.resolve(hc, light), want, "{c:?} hc={hc} light={light}");
        }
        let mut s = Settings::default();
        set_value(&mut s, "appearance.theme", "kim-light").unwrap();
        set_value(&mut s, "appearance.line_height", "1.6").unwrap();
        set_value(&mut s, "terminal.right_click", "menu").unwrap();
        set_value(&mut s, "terminal.copy_on_select", "true").unwrap();
        assert_eq!(s.appearance.theme, KimLight);
        assert_eq!(s.appearance.line_height, 1.6);
        assert_eq!(s.terminal.right_click, RightClick::Menu);
        assert!(s.terminal.copy_on_select);
        assert!(set_value(&mut s, "appearance.opacity", "5").is_err());
        assert!(set_value(&mut s, "nope.key", "1").is_err());
        assert_eq!(s.appearance.opacity, 100, "a refused value changes nothing");
    }

    #[test]
    fn keys_and_choices_are_consistent() {
        for k in KEYS {
            assert!(SECTIONS.contains(&k.section));
            // The default of every key is a valid value of it.
            let d = Settings::default().value(k);
            let text = match &d {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            literal(k, &text).unwrap_or_else(|e| panic!("{}: {e:?}", k.id()));
        }
        assert_eq!(key("appearance.theme").unwrap().id(), "appearance.theme");
        assert_eq!(key_ids().len(), KEYS.len());
        for t in ThemeChoice::ALL {
            assert_eq!(ThemeChoice::parse(t.as_str()), Some(t));
        }
        assert!(Motion::Full.animates(false) && !Motion::Reduced.animates(true));
        assert!(Motion::System.animates(true) && !Motion::System.animates(false));
        assert_eq!(format_number(1.2), "1.2");
        assert_eq!(format_number(15.0), "15");
        assert_eq!(trailing_comment("a = \"x # y\"  # real"), "  # real");
        assert_eq!(trailing_comment("a = 1"), "");
    }
}
