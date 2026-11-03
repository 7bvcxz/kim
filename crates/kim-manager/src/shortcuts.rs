//! Kim Manager's keyboard shortcuts (docs/HOTKEY_CONTRACT.md §13.2), on the shared
//! `kim-hotkeys` library.
//!
//! Backend `focus` only: keys are read from the window's own egui events while Kim Manager has
//! focus. Nothing is registered system-wide, no hook is installed, and nothing runs when the
//! window is closed (`KIM-NFR-020`). The bindings come from `%APPDATA%\mouv\kim\hotkeys.json`
//! over the compiled-in defaults; a broken file means defaults (safe mode), and a file that
//! breaks while the window is open keeps the previous bindings (§8.3). The file is re-read when
//! the window regains focus, so a change made with `kim hotkeys set` applies without a restart.

use eframe::egui;
use kim::hotkeys as kh;
use kim::paths::Paths;
use kim_hotkeys::check::{ToolState, analyze};
use kim_hotkeys::keys::{Chord, Key, Mods, NAMED_KEYS, Side, Step, steps_overlap};
use kim_hotkeys::store::{self, Loaded};
use kim_hotkeys::{Scope, view};

/// One active binding: its action and its steps.
#[derive(Debug, Clone)]
struct Active {
    action: String,
    steps: Vec<Step>,
}

/// What a key press did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not a shortcut; the key goes on to the focused control.
    Ignored,
    /// The first steps of a sequence; waiting for the next one (no timer).
    Pending,
    /// A pending sequence was cancelled by a key that matches nothing.
    Cancelled,
    /// Run this action.
    Fire(String),
}

/// One row of the shortcut list (§11.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub action: String,
    pub title: String,
    pub keys: String,
    pub state: String,
}

#[derive(Debug)]
pub struct Shortcuts {
    loaded: Loaded,
    active: Vec<Active>,
    pending: Option<(usize, Vec<usize>)>,
    composing: bool,
    off: bool,
    /// Why the file is not used (safe mode) or why a reload was ignored.
    pub problem: Option<String>,
}

impl Shortcuts {
    /// Loads Kim's bindings. `off` (`--no-hotkeys`, `KIM_NO_HOTKEYS`) turns every shortcut off
    /// without touching the file (§8.3 4).
    pub fn load(paths: &Paths, off: bool) -> Shortcuts {
        let (_, loaded) = kh::load(paths);
        let mut s = Shortcuts {
            problem: kh::safe_mode_warning("Kim Manager", &loaded),
            loaded,
            active: Vec::new(),
            pending: None,
            composing: false,
            off,
        };
        s.rebuild();
        s
    }

    fn state(&self) -> ToolState {
        let mut t = ToolState::new(kh::manifest(), self.loaded.layer.clone());
        t.safe_mode = self.loaded.state.safe_mode();
        t
    }

    fn rebuild(&mut self) {
        self.pending = None;
        self.active = if self.off {
            Vec::new()
        } else {
            analyze(&[self.state()], &[])
                .active
                .into_iter()
                .filter(|b| b.scope == Scope::App)
                .map(|b| Active {
                    action: b.action,
                    steps: b.seq.steps().to_vec(),
                })
                .collect()
        };
    }

    /// Re-reads `hotkeys.json` (window focus regained). An invalid file keeps the current
    /// bindings and is reported.
    pub fn reload(&mut self) {
        let (next, problems) = store::reload(&self.loaded, &kh::manifest());
        self.problem = match problems.first() {
            Some(p) => Some(format!(
                "hotkeys.json is invalid ({p}); the previous shortcuts stay active"
            )),
            None => kh::safe_mode_warning("Kim Manager", &next),
        };
        self.loaded = next;
        self.rebuild();
    }

    pub fn is_off(&self) -> bool {
        self.off
    }

    /// Feeds egui's events of one frame; returns the actions to run, in order.
    pub fn handle(&mut self, events: &[egui::Event]) -> Vec<String> {
        let mut fire = Vec::new();
        for e in events {
            match e {
                egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                    self.composing = !text.is_empty();
                }
                egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                    self.composing = false;
                }
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                } => {
                    if let Outcome::Fire(a) = self.press(*key, *physical_key, *modifiers) {
                        fire.push(a);
                    }
                }
                _ => {}
            }
        }
        fire
    }

    /// One key press (logical key, physical key, modifiers).
    pub fn press(
        &mut self,
        key: egui::Key,
        physical: Option<egui::Key>,
        modifiers: egui::Modifiers,
    ) -> Outcome {
        // IME composition guard: keys belong to the input method (§6.2).
        if self.composing || self.active.is_empty() {
            return Outcome::Ignored;
        }
        let mods = mods(modifiers);
        let logical = logical_key(key).map(|key| Step::Chord(Chord { mods, key }));
        let physical = physical
            .and_then(physical_key)
            .map(|key| Step::Chord(Chord { mods, key }));
        let hits = |s: &Step| {
            logical.as_ref().is_some_and(|l| steps_overlap(l, s))
                || physical.as_ref().is_some_and(|p| steps_overlap(p, s))
        };
        let (step, pool) = match self.pending.take() {
            Some((step, pool)) => (step, pool),
            None => (0, (0..self.active.len()).collect()),
        };
        let matched: Vec<usize> = pool
            .into_iter()
            .filter(|i| self.active[*i].steps.get(step).is_some_and(hits))
            .collect();
        if matched.is_empty() {
            return if step > 0 {
                Outcome::Cancelled
            } else {
                Outcome::Ignored
            };
        }
        if let Some(i) = matched
            .iter()
            .find(|i| self.active[**i].steps.len() == step + 1)
        {
            return Outcome::Fire(self.active[*i].action.clone());
        }
        self.pending = Some((step + 1, matched));
        Outcome::Pending
    }

    /// Every action with its keys and state, the reserved navigation keys included.
    pub fn rows(&self) -> Vec<Row> {
        let state = self.state();
        let analysis = analyze(std::slice::from_ref(&state), &[]);
        let data = view::list_data(
            &state,
            &analysis,
            &self.loaded.path.display().to_string(),
            self.loaded.state,
            &|_, _| "not_applicable",
        );
        data["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| {
                let keys: Vec<&str> = a["bindings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b["keys"].as_str())
                    .collect();
                let src = a["source"].as_str().unwrap_or("");
                let state = if self.off && src != "reserved" {
                    "off".to_string()
                } else {
                    match src {
                        "reserved" => "fixed".into(),
                        "unbound" => "no shortcut".into(),
                        s => s.to_string(),
                    }
                };
                Row {
                    action: a["id"].as_str().unwrap_or("").into(),
                    title: a["title"].as_str().unwrap_or("").into(),
                    keys: if keys.is_empty() {
                        "—".into()
                    } else {
                        keys.join(", ")
                    },
                    state,
                }
            })
            .collect()
    }
}

fn mods(m: egui::Modifiers) -> Mods {
    let on = |b: bool| if b { Side::Any } else { Side::None };
    Mods {
        ctrl: on(m.ctrl),
        alt: on(m.alt),
        altgr: false,
        shift: on(m.shift),
        win: Side::None,
    }
}

/// egui's logical key as a contract key name (§5.2); egui names most keys the same way.
fn logical_key(k: egui::Key) -> Option<Key> {
    let name = k.name();
    NAMED_KEYS
        .iter()
        .find(|n| **n == name)
        .map(|n| Key::Named(n))
}

/// egui's physical key as a `code:` key (§6.1).
fn physical_key(k: egui::Key) -> Option<Key> {
    let name = k.name();
    let code = match k {
        egui::Key::Minus => "Minus",
        egui::Key::Equals => "Equal",
        egui::Key::OpenBracket => "BracketLeft",
        egui::Key::CloseBracket => "BracketRight",
        egui::Key::Backslash => "Backslash",
        egui::Key::Semicolon => "Semicolon",
        egui::Key::Quote => "Quote",
        egui::Key::Backtick => "Backquote",
        egui::Key::Comma => "Comma",
        egui::Key::Period => "Period",
        egui::Key::Slash => "Slash",
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() => {
            return kim_hotkeys::keys::CODE_KEYS
                .iter()
                .find(|c| c.strip_prefix("Key") == Some(name))
                .map(|c| Key::Code(c));
        }
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_digit() => {
            return kim_hotkeys::keys::CODE_KEYS
                .iter()
                .find(|c| c.strip_prefix("Digit") == Some(name))
                .map(|c| Key::Code(c));
        }
        _ => return None,
    };
    Some(Key::Code(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(tag: &str, layer: Option<serde_json::Value>) -> (Dir, Paths) {
        let d = std::env::temp_dir().join(format!("kim-shortcuts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let paths = Paths::under(&d);
        if let Some(l) = layer {
            let f = kh::layer_path(&paths, kh::TOOL);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, l.to_string()).unwrap();
        }
        (Dir(d), paths)
    }

    fn press(s: &mut Shortcuts, m: egui::Modifiers, k: egui::Key) -> Outcome {
        s.press(k, Some(k), m)
    }

    const NONE: egui::Modifiers = egui::Modifiers::NONE;

    #[test]
    fn defaults_sequences_and_physical_keys() {
        let (_d, paths) = setup(
            "seq",
            Some(
                serde_json::json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim",
                "overrides": [
                    {"action": "tool.install", "bindings": ["Ctrl+K Ctrl+I"]},
                    {"action": "tool.update", "bindings": ["Ctrl+Shift+code:KeyU"]}]}),
            ),
        );
        let mut s = Shortcuts::load(&paths, false);
        assert_eq!(
            press(&mut s, NONE, egui::Key::F5),
            Outcome::Fire("catalog.refresh".into())
        );
        assert_eq!(
            press(&mut s, NONE, egui::Key::F1),
            Outcome::Fire("help.show".into())
        );
        assert_eq!(press(&mut s, NONE, egui::Key::F2), Outcome::Ignored);
        let ctrl = egui::Modifiers::CTRL;
        assert_eq!(press(&mut s, ctrl, egui::Key::K), Outcome::Pending);
        assert_eq!(
            press(&mut s, ctrl, egui::Key::I),
            Outcome::Fire("tool.install".into())
        );
        // A wrong second step cancels the sequence (swallowed); the next key starts over.
        assert_eq!(press(&mut s, ctrl, egui::Key::K), Outcome::Pending);
        assert_eq!(press(&mut s, NONE, egui::Key::Escape), Outcome::Cancelled);
        assert_eq!(
            press(&mut s, NONE, egui::Key::F5),
            Outcome::Fire("catalog.refresh".into())
        );
        // A physical binding matches the key position whatever the layout reports as logical.
        let cs = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        assert_eq!(
            s.press(egui::Key::Z, Some(egui::Key::U), cs),
            Outcome::Fire("tool.update".into()),
            "the key at U's position"
        );
    }

    #[test]
    fn ime_composition_and_the_off_switch_block_everything() {
        let (_d, paths) = setup("ime", None);
        let mut s = Shortcuts::load(&paths, false);
        let composing = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "\u{314e}".into(),
            active_range_chars: None,
        });
        let f5 = egui::Event::Key {
            key: egui::Key::F5,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: NONE,
        };
        assert!(s.handle(&[composing, f5.clone()]).is_empty(), "IME guard");
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("\u{d558}".into()));
        assert_eq!(s.handle(&[commit, f5.clone()]), ["catalog.refresh"]);
        let mut off = Shortcuts::load(&paths, true);
        assert!(off.is_off() && off.handle(&[f5]).is_empty());
        assert!(off.rows().iter().any(|r| r.state == "off"));
        assert!(
            off.rows()
                .iter()
                .any(|r| r.action == "nav.cancel" && r.state == "fixed")
        );
    }

    #[test]
    fn reload_applies_valid_files_and_keeps_bindings_on_invalid_ones() {
        let (_d, paths) = setup("reload", None);
        let mut s = Shortcuts::load(&paths, false);
        let f = kh::layer_path(&paths, kh::TOOL);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(
            &f,
            serde_json::json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim",
                "overrides": [{"action": "catalog.refresh", "bindings": ["Ctrl+R"]}]})
            .to_string(),
        )
        .unwrap();
        s.reload();
        assert_eq!(press(&mut s, NONE, egui::Key::F5), Outcome::Ignored);
        assert_eq!(
            press(&mut s, egui::Modifiers::CTRL, egui::Key::R),
            Outcome::Fire("catalog.refresh".into())
        );
        std::fs::write(&f, b"{ broken").unwrap();
        s.reload();
        assert!(
            s.problem
                .as_deref()
                .unwrap()
                .contains("previous shortcuts stay active")
        );
        assert_eq!(
            press(&mut s, egui::Modifiers::CTRL, egui::Key::R),
            Outcome::Fire("catalog.refresh".into())
        );
    }
}
