//! The editor's own keyboard shortcuts (docs/DDALGAK_GUI_JOURNEYS.md §4,
//! docs/HOTKEY_CONTRACT.md §13.4, `DDALGAK-NFR-007`).
//!
//! The rule list's actions of Ddalgak's hotkey manifest (context `rules`: `rules.new`,
//! `rules.duplicate`, `rules.delete`, `rules.search`, `rules.toggle`, `rules.test`,
//! `rules.export`) come from `%APPDATA%\mouv\ddalgak\hotkeys.json` over the manifest's defaults,
//! read from the window's own key events (backend `focus`: nothing registered, no hook). An
//! invalid file is safe mode (defaults), `--no-hotkeys`/`KIM_NO_HOTKEYS` turns them off. The
//! fixed navigation keys of §4 (`Enter`, `F2`, `Alt+Up`/`Alt+Down`, `F6`, `Esc`, `F1`,
//! `Ctrl+Z`, `Ctrl+A`) are not rebindable and are handled by the window directly.

use crate::capture;
use ddalgak::paths::Paths;
use eframe::egui;
use kim_hotkeys::check::{ToolState, analyze};
use kim_hotkeys::scope::Scope;
use kim_hotkeys::store;

/// One active binding of the rule list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: String,
    /// Canonical keys (`Ctrl+N`).
    pub keys: String,
}

/// The rule list's shortcuts.
#[derive(Clone, Debug, Default)]
pub struct Shortcuts {
    pub bindings: Vec<Binding>,
    pub off: bool,
    /// Why `hotkeys.json` is not used (safe mode), if so.
    pub problem: Option<String>,
}

impl Shortcuts {
    pub fn load(paths: &Paths, off: bool) -> Shortcuts {
        let m = ddalgak::shortcuts::manifest();
        let loaded = store::load(&paths.hotkeys(), &m);
        let mut state = ToolState::new(m, loaded.layer.clone());
        state.safe_mode = loaded.state.safe_mode();
        let bindings = if off {
            Vec::new()
        } else {
            analyze(&[state], &[])
                .active
                .into_iter()
                .filter(|b| matches!(&b.scope, Scope::Context(c) if c == "rules"))
                .filter(|b| b.seq.single_chord().is_some())
                .map(|b| Binding {
                    action: b.action,
                    keys: b.keys,
                })
                .collect()
        };
        Shortcuts {
            bindings,
            off,
            problem: loaded.problems.first().cloned(),
        }
    }

    /// The action a key press fires, if any (single chords; the window sees no Win key).
    pub fn action_for(&self, key: egui::Key, m: egui::Modifiers) -> Option<&str> {
        let pressed = capture::chord(key, m)?;
        self.bindings
            .iter()
            .find(|b| b.keys.eq_ignore_ascii_case(&pressed))
            .map(|b| b.action.as_str())
    }

    /// The keys bound to `action` (for buttons' tooltips and the F1 list).
    pub fn keys_of(&self, action: &str) -> Option<&str> {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.keys.as_str())
    }
}

/// The fixed keys of GUI_JOURNEYS §4 (shown in the F1 list).
pub const FIXED: &[(&str, &str)] = &[
    ("Enter", "Edit the selected rule"),
    ("F2", "Rename the selected rule"),
    ("Up / Down", "Select the previous / next rule"),
    ("Shift+Up / Shift+Down", "Extend the selection"),
    ("Space", "Add or remove the focused rule from the selection"),
    ("Alt+Up / Alt+Down", "Move the selected rule up / down"),
    ("Ctrl+A", "Select every shown rule"),
    ("Ctrl+Z", "Undo the last change made in this window"),
    ("F6 / Shift+F6", "Next / previous region"),
    ("Esc", "Cancel a dialog or the key capture; close the form"),
    ("F1", "This list"),
    ("Ctrl+Alt+Shift+Esc", "Emergency off (system-wide, always)"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};

    fn sandbox(tag: &str) -> Paths {
        let root = std::env::temp_dir().join(format!(
            "ddalgak-editor-keys-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        Paths::under(root)
    }

    #[test]
    fn manifest_defaults_user_bindings_and_the_off_switch() {
        let p = sandbox("defaults");
        let s = Shortcuts::load(&p, false);
        assert_eq!(s.action_for(Key::N, Modifiers::CTRL), Some("rules.new"));
        assert_eq!(
            s.action_for(Key::D, Modifiers::CTRL),
            Some("rules.duplicate")
        );
        assert_eq!(
            s.action_for(Key::Delete, Modifiers::NONE),
            Some("rules.delete")
        );
        assert_eq!(s.action_for(Key::F, Modifiers::CTRL), Some("rules.search"));
        assert_eq!(s.action_for(Key::F5, Modifiers::NONE), Some("rules.test"));
        assert_eq!(s.keys_of("rules.new"), Some("Ctrl+N"));
        assert!(Shortcuts::load(&p, true).bindings.is_empty());

        // A user binding in hotkeys.json replaces the default.
        std::fs::create_dir_all(&p.config).unwrap();
        std::fs::write(
            p.hotkeys(),
            r#"{"schema":"kim.hotkeys","schema_version":1,"tool":"ddalgak",
               "overrides":[{"action":"rules.new","bindings":["Ctrl+Shift+N"]}]}"#,
        )
        .unwrap();
        let s = Shortcuts::load(&p, false);
        assert_eq!(
            s.action_for(Key::N, Modifiers::CTRL | Modifiers::SHIFT),
            Some("rules.new")
        );
        assert_eq!(s.action_for(Key::N, Modifiers::CTRL), None);
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
