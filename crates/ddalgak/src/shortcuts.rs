//! Ddalgak's own global shortcuts (docs/HOTKEY_CONTRACT.md §13.4, `DDALGAK-FR-061`).
//!
//! These are hotkey-contract actions in `hotkeys.json`, not rules. The tray app registers the
//! global ones with `RegisterHotKey`, so they work while the hook is off (`DDALGAK-FR-004`):
//!
//! | Action | Step 66 |
//! | --- | --- |
//! | `engine.toggle` | turns the engine on or off |
//! | `engine.pause` | pauses or resumes |
//! | `editor.open` | not registered: the editor arrives in Step 74 (reported as `unavailable`) |
//! | `engine.emergency-off` | not a registration: the hook checks the chord on physical keys |
//!
//! All four ship without a default (§7.5); a user binds them in `hotkeys.json` (the
//! `ddalgak hotkeys` commands follow with the CLI Step 81; until then `hotkeys.json` is edited by
//! hand or with another Kim tool's editor). An invalid or newer file is safe mode: nothing
//! global is registered (`KIM-SEC-023`). `--no-hotkeys` and `KIM_NO_HOTKEYS` register nothing.

use crate::paths::Paths;
use kim_hotkeys::check::{ToolState, analyze};
use kim_hotkeys::keys::Chord;
use kim_hotkeys::manifest::Manifest;
use kim_hotkeys::register::win32_chord;
use kim_hotkeys::scope::Scope;
use kim_hotkeys::store;
use serde_json::{Value, json};

/// The compiled-in manifest (identical to docs/contracts/examples/kim-hotkey-manifest-v1/ddalgak.json).
pub const MANIFEST: &str = include_str!("../hotkeys.manifest.json");

pub fn manifest() -> Manifest {
    Manifest::parse(MANIFEST.as_bytes()).expect("the built-in manifest is valid")
}

/// Actions the tray app registers.
pub const REGISTERED: [&str; 2] = ["engine.toggle", "engine.pause"];

/// One global shortcut and what became of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shortcut {
    pub action: String,
    pub keys: String,
    /// `wanted` (to register), `unavailable` (no feature yet), `unsupported` (needs a hook).
    pub status: &'static str,
    pub chord: Option<Chord>,
}

/// The global shortcuts from `hotkeys.json`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shortcuts {
    /// `default`, `user`, `invalid`, `newer_version` (kim-hotkeys `ConfigState`).
    pub config: &'static str,
    pub problem: Option<String>,
    pub off: bool,
    pub list: Vec<Shortcut>,
}

impl Shortcuts {
    /// Reads `hotkeys.json`. `off` (`--no-hotkeys`, `KIM_NO_HOTKEYS`) keeps everything off
    /// without touching the file.
    pub fn load(paths: &Paths, off: bool) -> Shortcuts {
        let m = manifest();
        let loaded = store::load(&paths.hotkeys(), &m);
        let mut state = ToolState::new(m, loaded.layer.clone());
        state.safe_mode = loaded.state.safe_mode();
        let problem = loaded.problems.first().cloned();
        let mut list = Vec::new();
        if !off {
            for b in analyze(&[state], &[]).active {
                if b.scope != Scope::Global || b.action == "engine.emergency-off" {
                    continue;
                }
                let chord = b.seq.single_chord().copied();
                let status = if b.action == "editor.open" {
                    "unavailable"
                } else if !REGISTERED.contains(&b.action.as_str()) {
                    continue;
                } else if chord.is_some_and(|c| win32_chord(&c).is_ok()) {
                    "wanted"
                } else {
                    "unsupported"
                };
                list.push(Shortcut {
                    action: b.action,
                    keys: b.keys,
                    status,
                    chord,
                });
            }
        }
        Shortcuts {
            config: loaded.state.as_str(),
            problem,
            off,
            list,
        }
    }

    /// The chords to register.
    pub fn wanted(&self) -> impl Iterator<Item = &Shortcut> {
        self.list.iter().filter(|s| s.status == "wanted")
    }

    /// `shortcuts` in status output; `registration` comes from the running app.
    pub fn to_json(&self, registration: &dyn Fn(&str) -> Option<&'static str>) -> Value {
        json!({
            "config": self.config,
            "off": self.off,
            "problem": self.problem,
            "global": self.list.iter().map(|s| json!({
                "action": s.action,
                "keys": s.keys,
                "registration": registration(&s.action).unwrap_or(s.status),
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "ddalgak-shortcuts-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        Paths::under(root)
    }

    #[test]
    fn the_built_in_manifest_matches_the_contract_example() {
        let example = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/ddalgak.json"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&example).unwrap(),
            serde_json::from_str::<Value>(MANIFEST).unwrap()
        );
        assert!(manifest().problems().is_empty());
    }

    #[test]
    fn no_defaults_bindings_from_the_user_layer_and_safe_mode() {
        let p = sandbox("layer");
        let s = Shortcuts::load(&p, false);
        assert_eq!(s.config, "default");
        assert!(s.list.is_empty(), "global actions ship unbound");
        std::fs::create_dir_all(&p.config).unwrap();
        std::fs::write(
            p.hotkeys(),
            r#"{"schema":"kim.hotkeys","schema_version":1,"tool":"ddalgak",
                "overrides":[
                  {"action":"engine.toggle","bindings":["Ctrl+Alt+Shift+F13"]},
                  {"action":"engine.pause","bindings":["Ctrl+Alt+Shift+F14"]},
                  {"action":"editor.open","bindings":["Ctrl+Alt+Shift+F15"]}]}"#,
        )
        .unwrap();
        let s = Shortcuts::load(&p, false);
        assert_eq!(s.config, "user", "{:?}", s.problem);
        let got: Vec<(&str, &str, &str)> = s
            .list
            .iter()
            .map(|x| (x.action.as_str(), x.keys.as_str(), x.status))
            .collect();
        assert!(
            got.contains(&("engine.toggle", "Ctrl+Alt+Shift+F13", "wanted")),
            "{got:?}"
        );
        assert!(got.contains(&("engine.pause", "Ctrl+Alt+Shift+F14", "wanted")));
        assert!(got.contains(&("editor.open", "Ctrl+Alt+Shift+F15", "unavailable")));
        assert_eq!(s.wanted().count(), 2);
        assert!(
            Shortcuts::load(&p, true).list.is_empty(),
            "--no-hotkeys registers nothing"
        );
        std::fs::write(p.hotkeys(), b"{broken").unwrap();
        let s = Shortcuts::load(&p, false);
        assert_eq!(s.config, "invalid");
        assert!(s.list.is_empty(), "safe mode: nothing global");
        let _ = std::fs::remove_dir_all(
            p.config
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap(),
        );
    }
}
