//! The window's placement and remembered tabs: `%APPDATA%\mouv\subak\gui.json` (docs/SUBAK_GUI.md
//! §5, schema `docs/contracts/schemas/subak-gui-v2.schema.json`).
//!
//! The file is the GUI's own and is independent of the Host's session state: it never holds a
//! process id as truth, terminal content or anything secret, only where the window was, and
//! which Terminal it showed last (a hint by id and name). **Version 2** (Step 20, ADR 0051): the
//! text size and theme of version 1 moved to `config.toml` ([`crate::config`]); a version 1 file
//! is migrated once — its values go into `config.toml` where that file is silent, the old file is
//! backed up, then it is rewritten as version 2 ([`Prefs::legacy`]). Rules (ADR 0006 writing
//! rules, `SUBAK-FR-046`):
//!
//! - a missing file means defaults;
//! - a value that is invalid falls back to its default, the rest is used, and the problem is
//!   reported once (the window shows it in a banner);
//! - a file that is not JSON, or was written by a newer version, is used as defaults and **never
//!   rewritten** by this run (read-only), so nothing the user or a newer Subak wrote is lost;
//! - unknown keys are kept when the file is rewritten;
//! - writes are atomic (a temporary file, flushed, then renamed over the old one) and happen only
//!   on a change (a move or resize that ended, a zoom, another Terminal, closing), never while
//!   idle.

use serde_json::{Map, Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The document's `schema` and `schema_version`.
pub const SCHEMA: &str = "subak.gui";
pub const VERSION: u64 = 2;
/// The file name inside the configuration directory.
pub const FILE: &str = "gui.json";

/// Terminal text sizes the window allows, in logical pixels (`size.terminal`, user-adjustable;
/// the setting `appearance.font_size` of `config.toml` since Step 20).
pub const FONT_MIN: f32 = crate::config::FONT_SIZE.0;
pub const FONT_MAX: f32 = crate::config::FONT_SIZE.1;
pub const FONT_DEFAULT: f32 = crate::config::FONT_SIZE.2;

/// Where the window was: its restored (not maximized) outer rectangle in physical screen pixels
/// at `dpi`, and whether it was maximized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub dpi: u32,
    pub maximized: bool,
}

/// The Terminal shown last: reopened when it still runs (by id, else by name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTerminal {
    pub id: String,
    pub name: String,
}

/// The preferences, plus what the file said that this version does not know.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    pub window: Option<Placement>,
    pub last_terminal: Option<LastTerminal>,
    /// Terminals hidden in the window when it last ran (ids; Step 18 keeps tab hiding across
    /// runs). The Terminal manager shows them; opening one there shows its tab again.
    pub hidden_terminals: Vec<String>,
    /// The text size and theme a version 1 file held (Step 20: they belong in `config.toml`
    /// now); `None` when the file was version 2 or had neither. Never written back.
    pub legacy: Option<crate::config::Legacy>,
    /// Unknown keys of the file, written back unchanged.
    extra: Map<String, Value>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            window: None,
            last_terminal: None,
            hidden_terminals: Vec::new(),
            legacy: None,
            extra: Map::new(),
        }
    }
}

/// The result of loading the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub prefs: Prefs,
    /// What was wrong with the file, in one sentence (shown once), if anything.
    pub problem: Option<String>,
    /// The file must not be rewritten by this run (not JSON, or from a newer version).
    pub read_only: bool,
}

/// `%APPDATA%\mouv\subak` (Linux, M4: `$XDG_CONFIG_HOME/mouv/subak`), from the environment, so a
/// test can point it at a sandbox. `None` when the variable is not set (nothing is persisted).
pub fn config_dir() -> Option<PathBuf> {
    crate::hotkeys::config_dir()
}

impl Prefs {
    /// Parses a document. Never fails: see the module rules.
    pub fn parse(text: &str) -> Loaded {
        let doc: Value = match serde_json::from_str(text) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) | Err(_) => {
                return Loaded {
                    prefs: Prefs::default(),
                    problem: Some(format!(
                        "{FILE} is not a JSON object; the defaults are used and the file is left as it is"
                    )),
                    read_only: true,
                };
            }
        };
        let Value::Object(mut map) = doc else {
            unreachable!("matched above")
        };
        let version = map.get("schema_version").and_then(Value::as_u64);
        if map.get("schema").and_then(Value::as_str) != Some(SCHEMA)
            || version.is_none_or(|v| v == 0)
        {
            return Loaded {
                prefs: Prefs::default(),
                problem: Some(format!(
                    "{FILE} is not a `{SCHEMA}` document; the defaults are used and the file is left as it is"
                )),
                read_only: true,
            };
        }
        if version.is_some_and(|v| v > VERSION) {
            return Loaded {
                prefs: Prefs::default(),
                problem: Some(format!(
                    "{FILE} was written by a newer Subak (schema version {}); the defaults are used and the file is left as it is",
                    version.unwrap_or(0)
                )),
                read_only: true,
            };
        }
        let mut p = Prefs::default();
        let mut bad: Vec<&str> = Vec::new();
        for key in ["schema", "schema_version"] {
            map.remove(key);
        }
        // Version 1's display settings (Step 20: `config.toml`): kept aside for the migration,
        // never written back; an invalid one is simply dropped.
        let font_size = map
            .remove("font_size")
            .and_then(|v| v.as_f64())
            .filter(|f| (f64::from(FONT_MIN)..=f64::from(FONT_MAX)).contains(f));
        let theme = map
            .remove("theme")
            .and_then(|v| v.as_str().and_then(crate::config::ThemeChoice::parse));
        if version == Some(1) && (font_size.is_some() || theme.is_some()) {
            p.legacy = Some(crate::config::Legacy {
                font_size: font_size.map(|f| f as f32),
                theme,
            });
        }
        if let Some(v) = map.remove("window") {
            match parse_placement(&v) {
                Some(w) => p.window = Some(w),
                None if v.is_null() => {}
                None => bad.push("window"),
            }
        }
        if let Some(v) = map.remove("last_terminal") {
            let id = v["id"].as_str().filter(|s| !s.is_empty() && s.len() <= 64);
            let name = v["name"].as_str().filter(|s| s.chars().count() <= 64);
            match (id, name) {
                (Some(id), Some(name)) => {
                    p.last_terminal = Some(LastTerminal {
                        id: id.to_string(),
                        name: name.to_string(),
                    });
                }
                _ if v.is_null() => {}
                _ => bad.push("last_terminal"),
            }
        }
        if let Some(v) = map.remove("hidden_terminals") {
            let ids: Option<Vec<String>> = v.as_array().and_then(|a| {
                a.iter()
                    .map(|x| {
                        x.as_str()
                            .filter(|s| !s.is_empty() && s.len() <= 64)
                            .map(String::from)
                    })
                    .collect()
            });
            match ids {
                Some(ids) if ids.len() <= 1000 => p.hidden_terminals = ids,
                _ => bad.push("hidden_terminals"),
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

    /// Loads `dir/gui.json`; a missing file (or directory) means defaults.
    pub fn load(dir: &Path) -> Loaded {
        match std::fs::read_to_string(dir.join(FILE)) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded {
                prefs: Prefs::default(),
                problem: None,
                read_only: false,
            },
            Err(e) => Loaded {
                prefs: Prefs::default(),
                problem: Some(format!(
                    "{FILE} cannot be read ({e}); the defaults are used"
                )),
                read_only: true,
            },
        }
    }

    /// The document (known keys in a fixed order, then the unknown ones).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), json!(SCHEMA));
        m.insert("schema_version".into(), json!(VERSION));
        m.insert(
            "window".into(),
            self.window.map_or(Value::Null, |w| {
                json!({
                    "x": w.x, "y": w.y, "width": w.width, "height": w.height,
                    "dpi": w.dpi, "maximized": w.maximized,
                })
            }),
        );
        m.insert(
            "last_terminal".into(),
            self.last_terminal
                .as_ref()
                .map_or(Value::Null, |t| json!({"id": t.id, "name": t.name})),
        );
        m.insert("hidden_terminals".into(), json!(self.hidden_terminals));
        for (k, v) in &self.extra {
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
        Value::Object(m)
    }

    /// Writes `dir/gui.json` atomically: a temporary file next to it, flushed to disk, then
    /// renamed over the old file, so a crash leaves the old file or the new one, never a torn
    /// one.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(FILE);
        let tmp = dir.join(format!("{FILE}.tmp"));
        let mut text =
            serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        text.push('\n');
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)
    }

    /// The text size one zoom step larger (`dir` > 0) or smaller, kept inside the allowed range.
    pub fn zoomed(font_size: f32, dir: i32) -> f32 {
        let next = if dir > 0 {
            font_size + 1.0
        } else if dir < 0 {
            font_size - 1.0
        } else {
            FONT_DEFAULT
        };
        next.round().clamp(FONT_MIN, FONT_MAX)
    }
}

fn parse_placement(v: &Value) -> Option<Placement> {
    let i = |k: &str| {
        v.get(k)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
    };
    let p = Placement {
        x: i("x")?,
        y: i("y")?,
        width: i("width")?,
        height: i("height")?,
        dpi: v
            .get("dpi")
            .and_then(Value::as_u64)
            .and_then(|d| u32::try_from(d).ok())?,
        maximized: v.get("maximized").and_then(Value::as_bool).unwrap_or(false),
    };
    ((1..=100_000).contains(&p.width)
        && (1..=100_000).contains(&p.height)
        && (48..=960).contains(&p.dpi)
        && p.x.unsigned_abs() <= 1_000_000
        && p.y.unsigned_abs() <= 1_000_000)
        .then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeChoice;

    fn sandbox(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "subak-gui-prefs-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn missing_file_is_defaults_and_writable() {
        let d = sandbox("missing");
        let l = Prefs::load(&d);
        assert_eq!(l.prefs, Prefs::default());
        assert_eq!(l.problem, None);
        assert!(!l.read_only);
    }

    #[test]
    fn round_trip_through_the_sandbox_config_dir_keeps_unknown_keys() {
        let d = sandbox("roundtrip");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(FILE),
            r#"{"schema":"subak.gui","schema_version":2,
                "window":{"x":100,"y":80,"width":1000,"height":640,"dpi":144,"maximized":true},
                "last_terminal":{"id":"t-0123456789ab","name":"빌드"},
                "future_option":{"keep":[1,2,3]}}"#,
        )
        .unwrap();
        let l = Prefs::load(&d);
        assert_eq!(l.problem, None, "{l:?}");
        let mut p = l.prefs;
        assert_eq!(p.legacy, None);
        assert_eq!(
            p.window,
            Some(Placement {
                x: 100,
                y: 80,
                width: 1000,
                height: 640,
                dpi: 144,
                maximized: true
            })
        );
        assert_eq!(p.last_terminal.as_ref().unwrap().name, "빌드");
        p.hidden_terminals = vec!["t-0123456789ab".into()];
        p.save(&d).unwrap();
        assert!(
            !d.join("gui.json.tmp").exists(),
            "the temporary file was renamed"
        );
        let text = std::fs::read_to_string(d.join(FILE)).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["future_option"], json!({"keep": [1, 2, 3]}));
        assert_eq!(doc["schema_version"], json!(2));
        assert!(doc.get("font_size").is_none() && doc.get("theme").is_none());
        let again = Prefs::load(&d);
        assert_eq!(again.prefs, p);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Step 20: a version 1 file's text size and theme are kept aside for `config.toml` (the
    /// window migrates them once), the rest is read as before, and the next save is version 2.
    #[test]
    fn a_version_1_file_is_read_and_its_display_settings_kept_for_the_migration() {
        let l = Prefs::parse(
            r#"{"schema":"subak.gui","schema_version":1,"font_size":15,"theme":"kim-dark",
                "window":{"x":0,"y":0,"width":900,"height":600,"dpi":96},"last_terminal":null,
                "hidden_terminals":["t-aaaaaaaaaaaa"]}"#,
        );
        assert!(!l.read_only);
        assert_eq!(l.problem, None);
        assert_eq!(
            l.prefs.legacy,
            Some(crate::config::Legacy {
                font_size: Some(15.0),
                theme: Some(ThemeChoice::KimDark)
            })
        );
        assert_eq!(l.prefs.window.unwrap().width, 900);
        assert_eq!(l.prefs.hidden_terminals, ["t-aaaaaaaaaaaa"]);
        let doc = l.prefs.to_json();
        assert_eq!(doc["schema_version"], 2);
        assert!(doc.get("font_size").is_none() && doc.get("theme").is_none());
        // Invalid version 1 values are dropped (the defaults apply), never an error.
        let l = Prefs::parse(
            r#"{"schema":"subak.gui","schema_version":1,"font_size":200,"theme":"neon"}"#,
        );
        assert_eq!(l.prefs.legacy, None);
        assert_eq!(l.problem, None);
        // A version 2 file with stray display keys does not migrate them.
        let l = Prefs::parse(r#"{"schema":"subak.gui","schema_version":2,"font_size":20}"#);
        assert_eq!(l.prefs.legacy, None);
    }

    #[test]
    fn invalid_values_fall_back_one_by_one() {
        let l = Prefs::parse(
            r#"{"schema":"subak.gui","schema_version":2,
                "window":{"x":0,"y":0,"width":-5,"height":10,"dpi":96},"last_terminal":7,
                "hidden_terminals":["ok-id"]}"#,
        );
        assert!(!l.read_only);
        assert_eq!(l.prefs.window, None);
        assert_eq!(l.prefs.last_terminal, None);
        assert_eq!(l.prefs.hidden_terminals, ["ok-id"]);
        let problem = l.problem.unwrap();
        assert!(
            problem.contains("window") && problem.contains("last_terminal"),
            "{problem}"
        );
    }

    #[test]
    fn broken_or_newer_files_are_read_only() {
        for (text, needle) in [
            ("{not json", "not a JSON object"),
            ("[1,2]", "not a JSON object"),
            (
                r#"{"schema":"other","schema_version":1}"#,
                "not a `subak.gui`",
            ),
            (
                r#"{"schema":"subak.gui","schema_version":3}"#,
                "newer Subak",
            ),
        ] {
            let l = Prefs::parse(text);
            assert!(l.read_only, "{text}");
            assert_eq!(l.prefs, Prefs::default());
            assert!(l.problem.as_deref().unwrap().contains(needle), "{l:?}");
        }
    }

    #[test]
    fn zoom_steps_stay_in_range() {
        assert_eq!(Prefs::zoomed(13.0, 1), 14.0);
        assert_eq!(Prefs::zoomed(13.0, -1), 12.0);
        assert_eq!(Prefs::zoomed(20.0, 0), FONT_DEFAULT);
        assert_eq!(Prefs::zoomed(FONT_MAX, 1), FONT_MAX);
        assert_eq!(Prefs::zoomed(FONT_MIN, -1), FONT_MIN);
    }

    #[test]
    fn the_written_document_matches_its_schema() {
        let schema: Value = serde_json::from_str(include_str!(
            "../../../../docs/contracts/schemas/subak-gui-v2.schema.json"
        ))
        .unwrap();
        let check = |doc: &Value| {
            let errors = crate::jsonschema::Validator::new(&schema)
                .validate(doc)
                .unwrap();
            assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
        };
        let mut p = Prefs::default();
        check(&p.to_json());
        p.window = Some(Placement {
            x: -20000,
            y: -20000,
            width: 900,
            height: 600,
            dpi: 120,
            maximized: false,
        });
        p.last_terminal = Some(LastTerminal {
            id: "t-0123456789ab".into(),
            name: "build".into(),
        });
        p.extra.insert("future_option".into(), json!(true));
        p.hidden_terminals = vec!["t-0123456789ab".into()];
        check(&p.to_json());
    }

    #[test]
    fn hidden_tabs_are_kept_by_id_and_a_bad_list_falls_back() {
        let l = Prefs::parse(
            r#"{"schema":"subak.gui","schema_version":2,"hidden_terminals":["t-aaaaaaaaaaaa","t-bbbbbbbbbbbb"]}"#,
        );
        assert_eq!(l.problem, None);
        assert_eq!(
            l.prefs.hidden_terminals,
            ["t-aaaaaaaaaaaa", "t-bbbbbbbbbbbb"]
        );
        let l = Prefs::parse(r#"{"schema":"subak.gui","schema_version":2,"hidden_terminals":[1]}"#);
        assert!(l.prefs.hidden_terminals.is_empty());
        assert!(l.problem.unwrap().contains("hidden_terminals"));
    }
}
