//! The floating icon's settings and saved place: `%APPDATA%\mouv\subak\float.json`
//! (docs/SUBAK_GUI.md §11.6, schema `docs/contracts/schemas/subak-float-v1.schema.json`).
//!
//! A file of its own, separate from the window's `gui.json`: the floating icon and the windows
//! are different processes that may run at the same time, and each rewrites only its own file,
//! so neither loses the other's changes. The rules are `gui.json`'s (ADR 0006, `SUBAK-FR-046`):
//! a missing file means defaults; an invalid value falls back to its default alone (reported
//! once); a file that is not JSON, not a `subak.float` document or from a newer schema version
//! is used as defaults and **never rewritten** by this run; unknown keys are kept; writes are
//! atomic and happen only on a change (a drag that ended, a menu choice), never while idle.

use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::Path;

pub const SCHEMA: &str = "subak.float";
pub const VERSION: u64 = 1;
pub const FILE: &str = "float.json";

/// The icon's saved place: its top-left corner in physical screen pixels and the DPI of the
/// monitor it was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub dpi: u32,
}

/// The settings.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatPrefs {
    /// Show the floating icon (default on: running `subak-gui --float` is the user's choice).
    pub icon: bool,
    /// Show the tray icon (default off).
    pub tray: bool,
    /// Keep the floating icon and the manager above other windows (pinned; default on).
    pub on_top: bool,
    /// Keep the manager open when it loses the focus (default off: it closes like a flyout).
    pub manager_pinned: bool,
    pub position: Option<Position>,
    extra: Map<String, Value>,
}

impl Default for FloatPrefs {
    fn default() -> Self {
        Self {
            icon: true,
            tray: false,
            on_top: true,
            manager_pinned: false,
            position: None,
            extra: Map::new(),
        }
    }
}

/// The result of loading the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub prefs: FloatPrefs,
    pub problem: Option<String>,
    pub read_only: bool,
}

fn defaults_with(problem: String) -> Loaded {
    Loaded {
        prefs: FloatPrefs::default(),
        problem: Some(problem),
        read_only: true,
    }
}

impl FloatPrefs {
    /// Parses a document; never fails (see the module rules).
    pub fn parse(text: &str) -> Loaded {
        let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(text) else {
            return defaults_with(format!(
                "{FILE} is not a JSON object; the defaults are used and the file is left as it is"
            ));
        };
        let version = map.get("schema_version").and_then(Value::as_u64);
        if map.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || version.is_none_or(|v| v == 0)
        {
            return defaults_with(format!(
                "{FILE} is not a `{SCHEMA}` document; the defaults are used and the file is left as it is"
            ));
        }
        if let Some(v) = version.filter(|v| *v > VERSION) {
            return defaults_with(format!(
                "{FILE} was written by a newer Subak (schema version {v}); the defaults are used and the file is left as it is"
            ));
        }
        map.remove("schema");
        map.remove("schema_version");
        let mut p = FloatPrefs::default();
        let mut bad: Vec<&str> = Vec::new();
        for (key, slot) in [
            ("icon", &mut p.icon),
            ("tray", &mut p.tray),
            ("on_top", &mut p.on_top),
            ("manager_pinned", &mut p.manager_pinned),
        ] {
            if let Some(v) = map.remove(key) {
                match v.as_bool() {
                    Some(b) => *slot = b,
                    None => bad.push(key),
                }
            }
        }
        if let Some(v) = map.remove("position") {
            let i = |k: &str| {
                v.get(k)
                    .and_then(Value::as_i64)
                    .and_then(|n| i32::try_from(n).ok())
                    .filter(|n| n.unsigned_abs() <= 1_000_000)
            };
            let dpi = v
                .get("dpi")
                .and_then(Value::as_u64)
                .and_then(|d| u32::try_from(d).ok())
                .filter(|d| (48..=960).contains(d));
            match (i("x"), i("y"), dpi) {
                (Some(x), Some(y), Some(dpi)) => p.position = Some(Position { x, y, dpi }),
                _ if v.is_null() => {}
                _ => bad.push("position"),
            }
        }
        p.extra = map;
        Loaded {
            problem: (!bad.is_empty()).then(|| {
                format!(
                    "{FILE}: invalid {} — the default is used for {}",
                    bad.join(", "),
                    if bad.len() == 1 { "it" } else { "them" }
                )
            }),
            prefs: p,
            read_only: false,
        }
    }

    /// Loads `dir/float.json`; a missing file (or folder) means defaults.
    pub fn load(dir: &Path) -> Loaded {
        match std::fs::read_to_string(dir.join(FILE)) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded {
                prefs: FloatPrefs::default(),
                problem: None,
                read_only: false,
            },
            Err(e) => defaults_with(format!(
                "{FILE} cannot be read ({e}); the defaults are used"
            )),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), json!(SCHEMA));
        m.insert("schema_version".into(), json!(VERSION));
        m.insert("icon".into(), json!(self.icon));
        m.insert("tray".into(), json!(self.tray));
        m.insert("on_top".into(), json!(self.on_top));
        m.insert("manager_pinned".into(), json!(self.manager_pinned));
        m.insert(
            "position".into(),
            self.position
                .map_or(Value::Null, |p| json!({"x": p.x, "y": p.y, "dpi": p.dpi})),
        );
        for (k, v) in &self.extra {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(m)
    }

    /// Writes `dir/float.json` atomically (a temporary file, flushed, renamed over the old one).
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        let mut text =
            serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        text.push('\n');
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, dir.join(FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "subak-float-prefs-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn defaults_show_the_icon_on_top_and_no_tray() {
        let d = sandbox("missing");
        let l = FloatPrefs::load(&d);
        assert_eq!(l.prefs, FloatPrefs::default());
        assert!(l.prefs.icon && !l.prefs.tray && l.prefs.on_top && !l.prefs.manager_pinned);
        assert!(!l.read_only && l.problem.is_none());
    }

    #[test]
    fn round_trip_keeps_unknown_keys_and_writes_atomically() {
        let d = sandbox("roundtrip");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(FILE),
            r#"{"schema":"subak.float","schema_version":1,"icon":false,"tray":true,
                "on_top":false,"manager_pinned":true,"position":{"x":-1500,"y":300,"dpi":144},
                "future":{"keep":true}}"#,
        )
        .unwrap();
        let l = FloatPrefs::load(&d);
        assert_eq!(l.problem, None);
        let mut p = l.prefs;
        assert!(!p.icon && p.tray && !p.on_top && p.manager_pinned);
        assert_eq!(
            p.position,
            Some(Position {
                x: -1500,
                y: 300,
                dpi: 144
            })
        );
        p.icon = true;
        p.save(&d).unwrap();
        assert!(!d.join("float.json.tmp").exists());
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(d.join(FILE)).unwrap()).unwrap();
        assert_eq!(doc["future"], json!({"keep": true}));
        assert_eq!(FloatPrefs::load(&d).prefs, p);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_values_fall_back_alone_and_broken_files_are_read_only() {
        let l = FloatPrefs::parse(
            r#"{"schema":"subak.float","schema_version":1,"icon":"yes","tray":true,
                "position":{"x":1,"y":2,"dpi":5}}"#,
        );
        assert!(!l.read_only);
        assert!(l.prefs.icon, "default kept");
        assert!(l.prefs.tray);
        assert_eq!(l.prefs.position, None);
        let problem = l.problem.unwrap();
        assert!(
            problem.contains("icon") && problem.contains("position"),
            "{problem}"
        );
        for (text, needle) in [
            ("{oops", "not a JSON object"),
            (
                r#"{"schema":"subak.gui","schema_version":1}"#,
                "not a `subak.float`",
            ),
            (
                r#"{"schema":"subak.float","schema_version":7}"#,
                "newer Subak",
            ),
        ] {
            let l = FloatPrefs::parse(text);
            assert!(l.read_only, "{text}");
            assert!(l.problem.unwrap().contains(needle));
        }
    }

    #[test]
    fn the_written_document_matches_its_schema() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../../../docs/contracts/schemas/subak-float-v1.schema.json"
        ))
        .unwrap();
        let mut p = FloatPrefs {
            position: Some(Position {
                x: -20000,
                y: -20000,
                dpi: 96,
            }),
            ..FloatPrefs::default()
        };
        p.extra.insert("future".into(), json!(1));
        for doc in [FloatPrefs::default().to_json(), p.to_json()] {
            let errors = crate::jsonschema::Validator::new(&schema)
                .validate(&doc)
                .unwrap();
            assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
        }
    }
}
