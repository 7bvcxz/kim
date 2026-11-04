//! `config.toml` (NOEUL-FR-102): versioned settings, read at start and on change.
//!
//! Settings so far: `[general] tray_icon` (default `true`, NOEUL-FR-004; Step 49),
//! `[capture] keyboard_help` (`"first-run"` by default, `"always"` or `"never"`: the keyboard
//! help inside the capture overlay; Step 50, ADR 0026), and since Step 51 (NOEUL-FR-024,
//! NOEUL-FR-032):
//!
//! - `[capture] quick_save_folder`: an absolute folder for quick save (default: the user's
//!   Pictures folder + `Noeul`);
//! - `[gestures] double_click`: what a double-click inside a confirmed selection does, `"pin"`
//!   (default), `"copy"`, `"save"` or `"none"`;
//! - `[gestures] middle_click`: the same for a middle-click, `"pin"` (default), `"copy"` or
//!   `"none"`;
//! - `[gestures] preset`: `"noeul"` (default) or `"snipaste"`, the name of the opt-in
//!   "Snipaste-style gestures" preset. `noeul config set gestures.preset snipaste` writes it with
//!   the preset's values (double-click copies, middle-click pins) and the preset's keys into
//!   `hotkeys.json` (`Enter` copies, `Ctrl+T` pins); `noeul config reset gestures` undoes both.
//!
//! Since Step 52 (NOEUL-FR-040, NOEUL-FR-041, ADR 0035):
//!
//! - `[capture] detection`: what the overlay detects under the pointer when it opens,
//!   `"window"` (default), `"element"` (UI Automation rectangles) or `"off"`; `E` cycles it
//!   during a capture;
//! - `[capture] magnifier`: whether the magnifier shows when the overlay opens, `"on"`
//!   (default) or `"off"`; `M` toggles it during a capture.
//!
//! Since Step 54 (UX spec §10, ADR 0044):
//!
//! - `[pins] border`: the one-pixel `border.subtle` outline around every pin, `"on"` (default) or
//!   `"off"`;
//! - `[pins] shadow`: the soft pin shadow around every pin (also the band that resizes it),
//!   `"on"` (default) or `"off"`. A running Noeul redraws its pins when either changes.
//!
//! The rules every setting follows:
//!
//! - a missing file means defaults;
//! - a file that is not TOML, or a `schema_version` other than 1, leaves the defaults in place
//!   and is reported (`newer_version` when it is higher; an older Noeul never rewrites it);
//! - an invalid value falls back to its default with a message naming the key, never a crash;
//! - **writing** ([`set`], [`reset`]; `noeul config set|reset`) edits only the lines of the keys
//!   it changes: comments, unknown keys and sections stay as they are. The edited text is parsed
//!   again before it is written and must differ from the old one in exactly those keys;
//!   otherwise nothing is written (`config_invalid`). The file is replaced atomically.
//!
//! Settings hold no secrets, pixels or clipboard content.
//!
//! [`UiState`] is not a setting: it is what Noeul remembers about its own UI (so far, whether the
//! first-run keyboard help was seen), in `%LOCALAPPDATA%\mouv\noeul\ui-state.json`, written once
//! after the first capture finished from the overlay.

use crate::detect::DetectMode;
use crate::error::{Code, NoeulError};
use crate::overlay::{GestureAction, Gestures};
use crate::transform::Look;
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

/// The gesture preset (`[gestures] preset`, NOEUL-FR-024).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Noeul's own gestures: double-click and `Enter` pin.
    Noeul,
    /// The opt-in "Snipaste-style gestures": double-click and `Enter` copy, `Ctrl+T` and
    /// middle-click pin.
    Snipaste,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Noeul => "noeul",
            Preset::Snipaste => "snipaste",
        }
    }

    pub fn parse(s: &str) -> Option<Preset> {
        [Preset::Noeul, Preset::Snipaste]
            .into_iter()
            .find(|p| p.as_str() == s)
    }

    /// The name shown in settings and the shortcut list.
    pub fn title(self) -> &'static str {
        match self {
            Preset::Noeul => "Noeul gestures",
            Preset::Snipaste => "Snipaste-style gestures",
        }
    }

    /// The gesture values the preset sets.
    pub fn gestures(self) -> Gestures {
        match self {
            Preset::Noeul => Gestures::default(),
            Preset::Snipaste => Gestures {
                double_click: GestureAction::Copy,
                middle_click: GestureAction::Pin,
            },
        }
    }

    /// The overlay bindings the preset writes into `hotkeys.json` (`None`: the defaults).
    pub fn bindings(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Preset::Noeul => &[],
            Preset::Snipaste => &[
                ("capture.pin", &["Ctrl+T"]),
                ("capture.copy", &["Ctrl+C", "Enter"]),
            ],
        }
    }
}

/// The actions whose bindings a preset may change (reset by `noeul config reset gestures`).
pub const PRESET_ACTIONS: &[&str] = &["capture.pin", "capture.copy"];

/// Effective settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Show the notification-area icon (NOEUL-FR-004; on by default).
    pub tray_icon: bool,
    /// The overlay's keyboard help (ADR 0026).
    pub keyboard_help: KeyboardHelp,
    /// Where quick save writes; `None`: the user's Pictures folder + `Noeul` (NOEUL-FR-032).
    pub quick_save_folder: Option<PathBuf>,
    /// The double-click and middle-click inside a selection (NOEUL-FR-024).
    pub gestures: Gestures,
    /// The gesture preset's name (NOEUL-FR-024).
    pub preset: Preset,
    /// What the overlay detects when it opens (NOEUL-FR-040).
    pub detection: DetectMode,
    /// Whether the overlay shows the magnifier when it opens (NOEUL-FR-041).
    pub magnifier: bool,
    /// The outline and shadow around pins (`[pins] border`, `[pins] shadow`; UX spec §10).
    pub pin_look: Look,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            tray_icon: true,
            keyboard_help: KeyboardHelp::FirstRun,
            quick_save_folder: None,
            gestures: Gestures::default(),
            preset: Preset::Noeul,
            detection: DetectMode::Window,
            magnifier: true,
            pin_look: Look::default(),
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
        let s = &self.settings;
        json!({
            "path": self.path.display().to_string(),
            "state": self.state.as_str(),
            "tray_icon": s.tray_icon,
            "keyboard_help": s.keyboard_help.as_str(),
            "quick_save_folder": s.quick_save_folder.as_ref().map(|p| p.display().to_string()),
            "detection": s.detection.as_str(),
            "magnifier": if s.magnifier { "on" } else { "off" },
            "pins": {
                "border": if s.pin_look.border { "on" } else { "off" },
                "shadow": if s.pin_look.shadow { "on" } else { "off" },
            },
            "gestures": {
                "double_click": s.gestures.double_click.as_str(),
                "middle_click": s.gestures.middle_click.as_str(),
                "preset": s.preset.as_str(),
            },
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

/// One known key: its section, name, the values it takes (for messages) and how a value is read.
struct Key {
    section: &'static str,
    name: &'static str,
    takes: &'static str,
    default: &'static str,
}

/// Every key Noeul reads and `noeul config set` writes.
const KEYS: &[Key] = &[
    Key {
        section: "general",
        name: "tray_icon",
        takes: "true or false",
        default: "true",
    },
    Key {
        section: "capture",
        name: "keyboard_help",
        takes: "\"first-run\", \"always\" or \"never\"",
        default: "\"first-run\"",
    },
    Key {
        section: "capture",
        name: "quick_save_folder",
        takes: "an absolute folder path",
        default: "the Pictures folder + Noeul",
    },
    Key {
        section: "capture",
        name: "detection",
        takes: "\"window\", \"element\" or \"off\"",
        default: "\"window\"",
    },
    Key {
        section: "capture",
        name: "magnifier",
        takes: "\"on\" or \"off\"",
        default: "\"on\"",
    },
    Key {
        section: "gestures",
        name: "double_click",
        takes: "\"pin\", \"copy\", \"save\" or \"none\"",
        default: "\"pin\"",
    },
    Key {
        section: "gestures",
        name: "middle_click",
        takes: "\"pin\", \"copy\" or \"none\"",
        default: "\"pin\"",
    },
    Key {
        section: "gestures",
        name: "preset",
        takes: "\"noeul\" or \"snipaste\"",
        default: "\"noeul\"",
    },
    Key {
        section: "pins",
        name: "border",
        takes: "\"on\" or \"off\"",
        default: "\"on\"",
    },
    Key {
        section: "pins",
        name: "shadow",
        takes: "\"on\" or \"off\"",
        default: "\"on\"",
    },
];

/// The dotted names of every key `noeul config set` accepts.
pub fn key_names() -> Vec<String> {
    KEYS.iter()
        .map(|k| format!("{}.{}", k.section, k.name))
        .collect()
}

/// The sections that hold keys (`noeul config reset <section>`).
pub const SECTIONS: &[&str] = &["general", "capture", "gestures", "pins"];

/// Applies one value to `settings`; `Err` names what the key takes.
fn apply(settings: &mut Settings, section: &str, name: &str, v: &toml::Value) -> Result<(), ()> {
    let s = v.as_str();
    match (section, name) {
        ("general", "tray_icon") => settings.tray_icon = v.as_bool().ok_or(())?,
        ("capture", "keyboard_help") => {
            settings.keyboard_help = s.and_then(KeyboardHelp::parse).ok_or(())?;
        }
        ("capture", "quick_save_folder") => {
            let p = s
                .filter(|p| !p.trim().is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or(())?;
            settings.quick_save_folder = Some(p);
        }
        ("capture", "detection") => {
            settings.detection = s.and_then(DetectMode::parse).ok_or(())?;
        }
        ("capture", "magnifier") => {
            settings.magnifier = match s {
                Some("on") => true,
                Some("off") => false,
                _ => return Err(()),
            };
        }
        ("gestures", "double_click") => {
            settings.gestures.double_click = s.and_then(GestureAction::parse).ok_or(())?;
        }
        ("gestures", "middle_click") => {
            settings.gestures.middle_click = s
                .and_then(GestureAction::parse)
                .filter(|g| *g != GestureAction::Save)
                .ok_or(())?;
        }
        ("gestures", "preset") => settings.preset = s.and_then(Preset::parse).ok_or(())?,
        ("pins", "border" | "shadow") => {
            let on = match s {
                Some("on") => true,
                Some("off") => false,
                _ => return Err(()),
            };
            if name == "border" {
                settings.pin_look.border = on;
            } else {
                settings.pin_look.shadow = on;
            }
        }
        _ => {}
    }
    Ok(())
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
    for section in SECTIONS {
        match table.get(*section) {
            None => {}
            Some(toml::Value::Table(t)) => {
                for k in KEYS.iter().filter(|k| k.section == *section) {
                    if let Some(v) = t.get(k.name)
                        && apply(&mut settings, k.section, k.name, v).is_err()
                    {
                        problems.push(format!(
                            "`{}.{}` must be {}; the default ({}) applies",
                            k.section, k.name, k.takes, k.default
                        ));
                    }
                }
            }
            Some(_) => problems.push(format!("`{section}` must be a table; its defaults apply")),
        }
    }
    Loaded {
        settings,
        path: path.to_path_buf(),
        state: State::User,
        problems,
    }
}

/// A TOML basic string.
fn toml_string(s: &str) -> String {
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

/// The TOML text of `value` for `section.name`, after checking it (`usage` otherwise).
fn literal(section: &str, name: &str, value: &str) -> Result<(String, toml::Value), NoeulError> {
    let key = KEYS
        .iter()
        .find(|k| k.section == section && k.name == name)
        .ok_or_else(|| unknown_key(&format!("{section}.{name}")))?;
    let v = if section == "general" {
        match value {
            "true" => toml::Value::Boolean(true),
            "false" => toml::Value::Boolean(false),
            _ => toml::Value::String(value.to_string()),
        }
    } else {
        toml::Value::String(value.to_string())
    };
    let mut probe = Settings::default();
    if apply(&mut probe, section, name, &v).is_err() {
        return Err(NoeulError::new(
            Code::Usage,
            format!("`{section}.{name}` must be {}", key.takes),
        ));
    }
    let text = match &v {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::String(s) => toml_string(s),
        _ => unreachable!("only booleans and strings are written"),
    };
    Ok((text, v))
}

fn unknown_key(key: &str) -> NoeulError {
    NoeulError::new(Code::Usage, format!("`{key}` is not a Noeul setting"))
        .hint(format!("settings: {}", key_names().join(", ")))
}

/// Splits `section.name`.
pub fn split_key(key: &str) -> Result<(&'static str, &'static str), NoeulError> {
    let (s, n) = key.split_once('.').ok_or_else(|| unknown_key(key))?;
    KEYS.iter()
        .find(|k| k.section == s && k.name == n)
        .map(|k| (k.section, k.name))
        .ok_or_else(|| unknown_key(key))
}

/// The header name of a `[table]` line (not `[[array]]`), ignoring a trailing comment.
fn header(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.starts_with("[[") {
        return Some("");
    }
    let rest = t.strip_prefix('[')?;
    let end = rest.find(']')?;
    let after = rest[end + 1..].trim();
    (after.is_empty() || after.starts_with('#')).then(|| rest[..end].trim())
}

/// Whether `line` assigns `name` (`name = …`, `"name" = …`).
fn assigns(line: &str, name: &str) -> bool {
    let t = line.trim_start();
    let rest = t
        .strip_prefix(name)
        .or_else(|| t.strip_prefix(&format!("\"{name}\"")));
    rest.is_some_and(|r| r.trim_start().starts_with('='))
}

/// `text` with `section.name` set to `literal` (`None`: the key removed). The line of the key
/// is replaced in place; a new key goes after the last line of its section; a missing section
/// is appended.
fn edit_text(text: &str, section: &str, name: &str, literal: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut current = String::new();
    let mut section_end: Option<usize> = None; // index after the section's last content line
    let mut found: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(h) = header(line) {
            current = h.to_string();
            if current == section {
                section_end = Some(i + 1);
            }
            continue;
        }
        if current == section {
            if !line.trim().is_empty() {
                section_end = Some(i + 1);
            }
            if found.is_none() && assigns(line, name) {
                found = Some(i);
            }
        }
    }
    match (found, literal) {
        (Some(i), Some(l)) => lines[i] = format!("{name} = {l}"),
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

/// One change of `noeul config set|reset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub key: String,
    /// The value before (as TOML text; `None`: not in the file, so the default applied).
    pub before: Option<String>,
    pub after: Option<String>,
}

impl Change {
    pub fn to_json(&self) -> Value {
        json!({"key": self.key, "before": self.before, "after": self.after})
    }
}

/// The result of an edit: the new text and what changed.
#[derive(Debug, Clone)]
pub struct Edited {
    pub text: String,
    pub changes: Vec<Change>,
}

fn value_text(v: Option<&toml::Value>) -> Option<String> {
    match v? {
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::String(s) => Some(toml_string(s)),
        other => Some(format!("{other:?}")),
    }
}

/// Reads the file for an edit: refuses an invalid or newer file (never rewritten).
fn current(path: &Path) -> Result<String, NoeulError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(format!("schema_version = {SCHEMA_VERSION}\n"));
        }
        Err(e) => return Err(NoeulError::io(path.display(), &e)),
    };
    let loaded = parse(path, &text);
    match loaded.state {
        State::Invalid => Err(NoeulError::new(
            Code::ConfigInvalid,
            format!("{} is invalid; nothing was changed", path.display()),
        )
        .hint("fix the file by hand, or delete it to start from the defaults")
        .details(loaded.problems)),
        State::NewerVersion => Err(NoeulError::new(
            Code::ConfigFromNewerVersion,
            format!(
                "{} was written by a newer Noeul and is read-only; nothing was changed",
                path.display()
            ),
        )
        .hint("use the newer Noeul to change settings")),
        _ => Ok(text),
    }
}

/// Applies `edits` (`(section, name, Some(literal))` to set, `None` to remove) to `text` and
/// checks the result: it must parse and differ from `text` in exactly the edited keys.
fn checked_edit(
    path: &Path,
    text: &str,
    edits: &[(&str, &str, Option<String>)],
) -> Result<Edited, NoeulError> {
    let refuse = |why: &str| {
        NoeulError::new(
            Code::ConfigInvalid,
            format!(
                "{} uses a layout Noeul cannot edit in place ({why}); nothing was changed",
                path.display()
            ),
        )
        .hint("edit the file by hand (keys go under [general], [capture], [gestures] and [pins])")
    };
    let before: toml::Table = text.parse().map_err(|_| refuse("not valid TOML"))?;
    let mut out = text.to_string();
    let mut changes = Vec::new();
    for (section, name, lit) in edits {
        out = edit_text(&out, section, name, lit.as_deref());
        let old = before
            .get(*section)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(*name));
        changes.push(Change {
            key: format!("{section}.{name}"),
            before: value_text(old),
            after: lit.clone(),
        });
    }
    let after: toml::Table = out
        .parse()
        .map_err(|_| refuse("the result would not parse"))?;
    let strip = |t: &toml::Table| {
        let mut t = t.clone();
        for (section, name, _) in edits {
            if let Some(toml::Value::Table(s)) = t.get_mut(*section) {
                s.remove(*name);
                if s.is_empty() {
                    t.remove(*section);
                }
            }
        }
        t
    };
    if strip(&before) != strip(&after) {
        return Err(refuse("other keys would change"));
    }
    for (section, name, lit) in edits {
        let got = after
            .get(*section)
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get(*name));
        if value_text(got) != *lit {
            return Err(refuse("a key is defined in another form"));
        }
    }
    Ok(Edited { text: out, changes })
}

/// `noeul config set`: the edited text for `values` (`(section.name, value)`), checked; nothing
/// is written here.
pub fn set(path: &Path, values: &[(&str, &str)]) -> Result<Edited, NoeulError> {
    let text = current(path)?;
    let mut edits = Vec::new();
    for (key, value) in values {
        let (section, name) = split_key(key)?;
        let (lit, _) = literal(section, name, value)?;
        edits.push((section, name, Some(lit)));
    }
    checked_edit(path, &text, &edits)
}

/// `noeul config reset`: the edited text with every key of `section` (all sections when `None`)
/// removed, so the defaults apply; unknown keys and comments stay.
pub fn reset(path: &Path, section: Option<&str>) -> Result<Edited, NoeulError> {
    if let Some(s) = section
        && !SECTIONS.contains(&s)
    {
        return Err(
            NoeulError::new(Code::Usage, format!("`{s}` is not a settings section"))
                .hint(format!("sections: {}", SECTIONS.join(", "))),
        );
    }
    let text = current(path)?;
    let edits: Vec<(&str, &str, Option<String>)> = KEYS
        .iter()
        .filter(|k| section.is_none_or(|s| s == k.section))
        .map(|k| (k.section, k.name, None))
        .collect();
    checked_edit(path, &text, &edits)
}

/// Writes `text` to `path` atomically (a temporary file in the same folder, then a rename).
pub fn write(path: &Path, text: &str) -> Result<(), NoeulError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| NoeulError::io(dir.display(), &e))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| NoeulError::io(tmp.display(), &e))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        NoeulError::io(path.display(), &e)
    })
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
    fn detection_and_magnifier_settings() {
        let d = parse(&p(), "");
        assert_eq!(
            (d.settings.detection, d.settings.magnifier),
            (DetectMode::Window, true)
        );
        let l = parse(
            &p(),
            "[capture]\ndetection = \"element\"\nmagnifier = \"off\"\n",
        );
        assert_eq!(
            (l.settings.detection, l.settings.magnifier),
            (DetectMode::Element, false)
        );
        assert_eq!(l.to_json()["detection"], "element");
        assert_eq!(l.to_json()["magnifier"], "off");
        let bad = parse(&p(), "[capture]\ndetection = \"text\"\nmagnifier = true\n");
        assert_eq!(
            (bad.settings.detection, bad.settings.magnifier),
            (DetectMode::Window, true),
            "invalid values fall back to the defaults"
        );
        assert_eq!(bad.problems.len(), 2, "{:?}", bad.problems);
        assert!(key_names().contains(&"capture.detection".to_string()));
        assert!(key_names().contains(&"capture.magnifier".to_string()));
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

    #[test]
    fn gestures_and_the_quick_save_folder_are_read_with_fallbacks() {
        let l = parse(&p(), "");
        assert_eq!(l.settings.gestures, Gestures::default(), "pin by default");
        assert_eq!(l.settings.preset, Preset::Noeul);
        assert_eq!(l.settings.quick_save_folder, None);
        assert_eq!(l.to_json()["gestures"]["double_click"], "pin");
        let folder = if cfg!(windows) {
            "C:\\\\Shots"
        } else {
            "/shots"
        };
        let l = parse(
            &p(),
            &format!(
                "[gestures]\ndouble_click = \"copy\"\nmiddle_click = \"none\"\npreset = \"snipaste\"\n[capture]\nquick_save_folder = \"{folder}\"\n"
            ),
        );
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.settings.gestures.double_click, GestureAction::Copy);
        assert_eq!(l.settings.gestures.middle_click, GestureAction::None);
        assert_eq!(l.settings.preset, Preset::Snipaste);
        assert!(l.settings.quick_save_folder.unwrap().is_absolute());
        // Invalid values fall back to their defaults, naming the key.
        let l = parse(
            &p(),
            "[gestures]\ndouble_click = \"close\"\nmiddle_click = \"save\"\npreset = 3\n[capture]\nquick_save_folder = \"relative\\\\dir\"\n",
        );
        assert_eq!(l.settings, Settings::default());
        let keys: Vec<&str> = [
            "gestures.double_click",
            "gestures.middle_click",
            "gestures.preset",
            "capture.quick_save_folder",
        ]
        .into_iter()
        .filter(|k| l.problems.iter().any(|m| m.contains(k)))
        .collect();
        assert_eq!(keys.len(), 4, "{:?}", l.problems);
        assert_eq!(Preset::Snipaste.title(), "Snipaste-style gestures");
        assert_eq!(
            Preset::Snipaste.gestures().double_click,
            GestureAction::Copy
        );
        assert_eq!(
            Preset::Snipaste.bindings()[0],
            ("capture.pin", &["Ctrl+T"][..])
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noeul-settings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    #[test]
    fn set_edits_keys_in_place_and_keeps_comments_and_unknown_keys() {
        let f = scratch("set");
        // A missing file: created with the schema version and the section.
        let e = set(&f, &[("gestures.double_click", "copy")]).unwrap();
        assert_eq!(
            e.text,
            "schema_version = 1\n\n[gestures]\ndouble_click = \"copy\"\n"
        );
        assert_eq!(
            e.changes,
            [Change {
                key: "gestures.double_click".into(),
                before: None,
                after: Some("\"copy\"".into())
            }]
        );
        assert!(!f.exists(), "set only computes; write writes");
        let original = "# my settings\nschema_version = 1\n\n[general]\ntray_icon = false # no icon\nfuture = 1\n\n[gestures]\n# keep this comment\ndouble_click = \"none\"\n\n[later]\nx = \"y\"\n";
        std::fs::write(&f, original).unwrap();
        let e = set(
            &f,
            &[
                ("gestures.double_click", "pin"),
                ("gestures.middle_click", "copy"),
                (
                    "capture.quick_save_folder",
                    if cfg!(windows) {
                        "C:\\Users\\me\\Pictures\\Shots"
                    } else {
                        "/home/me/shots"
                    },
                ),
            ],
        )
        .unwrap();
        write(&f, &e.text).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        for kept in [
            "# my settings",
            "tray_icon = false # no icon",
            "future = 1",
            "# keep this comment",
            "[later]",
            "x = \"y\"",
        ] {
            assert!(text.contains(kept), "{kept:?} kept in:\n{text}");
        }
        assert!(
            text.contains("double_click = \"pin\"\nmiddle_click = \"copy\""),
            "{text}"
        );
        let l = load(&f);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.settings.gestures.middle_click, GestureAction::Copy);
        assert!(!l.settings.tray_icon, "untouched keys keep their values");
        if cfg!(windows) {
            assert_eq!(
                l.settings.quick_save_folder,
                Some(PathBuf::from("C:\\Users\\me\\Pictures\\Shots")),
                "backslashes survive the TOML string"
            );
        }
        // Values are checked; unknown keys are refused.
        for (k, v) in [
            ("gestures.double_click", "close"),
            ("gestures.middle_click", "save"),
            ("general.tray_icon", "yes"),
            ("capture.quick_save_folder", "relative"),
            ("gestures.preset", "other"),
        ] {
            assert_eq!(set(&f, &[(k, v)]).unwrap_err().code, Code::Usage, "{k} {v}");
        }
        let e = set(&f, &[("gestures.colour", "red")]).unwrap_err();
        assert_eq!(e.code, Code::Usage);
        assert!(e.hint.unwrap().contains("gestures.double_click"));
        // Reset of a section removes its keys only (defaults apply), comments stay.
        let e = reset(&f, Some("gestures")).unwrap();
        write(&f, &e.text).unwrap();
        let l = load(&f);
        assert_eq!(l.settings.gestures, Gestures::default());
        assert!(!l.settings.tray_icon);
        assert!(
            std::fs::read_to_string(&f)
                .unwrap()
                .contains("# keep this comment")
        );
        assert_eq!(reset(&f, Some("history")).unwrap_err().code, Code::Usage);
        let e = reset(&f, None).unwrap();
        write(&f, &e.text).unwrap();
        assert_eq!(load(&f).settings, Settings::default());
        assert!(
            std::fs::read_to_string(&f).unwrap().contains("future = 1"),
            "unknown keys stay"
        );
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn files_noeul_cannot_edit_safely_are_never_rewritten() {
        let f = scratch("refuse");
        for (text, code) in [
            ("this is = = not toml", Code::ConfigInvalid),
            ("schema_version = 2\n", Code::ConfigFromNewerVersion),
            // The key in another form: a dotted key at the top and an inline table.
            ("gestures.double_click = \"copy\"\n", Code::ConfigInvalid),
            (
                "gestures = { double_click = \"copy\" }\n",
                Code::ConfigInvalid,
            ),
        ] {
            std::fs::write(&f, text).unwrap();
            let e = set(&f, &[("gestures.double_click", "pin")]).unwrap_err();
            assert_eq!(e.code, code, "{text:?}: {}", e.message);
            assert_eq!(std::fs::read_to_string(&f).unwrap(), text, "untouched");
        }
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }
}
