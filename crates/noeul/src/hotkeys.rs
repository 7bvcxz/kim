//! Noeul's keyboard shortcuts on the shared `kim-hotkeys` library (docs/HOTKEY_CONTRACT.md §13.3,
//! ADR 0017, ADR 0021 §5, NOEUL-FR-100, NOEUL-FR-114, NOEUL-SEC-012).
//!
//! - **Manifest.** The final manifest of every Noeul action is the contract example
//!   `docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json` (73 actions). A build ships only
//!   the actions it implements, under the same IDs and defaults (NOEUL-FR-114); this build's
//!   subset is [`MANIFEST_JSON`] (`hotkeys.manifest.json`, shipped next to `capabilities.json`). A
//!   test keeps it equal to the final manifest filtered by [`IMPLEMENTED`].
//! - **Global keys.** Backend `register` only: [`Manager`] registers the effective `global`
//!   bindings with `RegisterHotKey` through `kim_hotkeys::register` while the instance runs. A key
//!   another program owns is reported as `taken` with remap offers and never retried in the
//!   background (`KIM-SEC-020`). No hook, no polling, no keystroke is ever seen except a
//!   registered chord (`KIM-SEC-022`). `--no-hotkeys`, `KIM_NO_HOTKEYS`, `"enabled": false` and
//!   safe mode register nothing (§8.3, §12.4).
//! - **Context keys.** The `pin` and `capture-overlay` contexts (and `app` bindings) are matched
//!   from the pin and overlay windows' own key messages ([`ContextKeys`], backend `focus`
//!   semantics inside Noeul's windows). The overlay also always leaves on the reserved
//!   navigation key `Escape` (`nav.cancel`, §3.5), even with its shortcuts off.
//! - **Reload.** A running instance re-reads `hotkeys.json` when the file changes (a blocking
//!   change notification, §8.3 5) and when `noeul hotkeys …` tells it to over IPC. A valid file is
//!   applied at once; an invalid one is reported and the previous bindings stay.
//!
//! `capture.region` (`F1`) is registered since Step 50, which added its selection overlay and the
//! overlay keys it implements (ADR 0026); Step 49 left it out because the contract forbids
//! registering an action that does nothing (NOEUL-FR-114). Step 51 added the outputs
//! `capture.copy` / `pin.copy` (`Ctrl+C`), `capture.save` / `pin.save` (`Ctrl+S`) and
//! `capture.quick-save` / `pin.quick-save` (`Ctrl+Shift+S`).

use crate::paths::Paths;
use kim_hotkeys::check::{Conflict, Severity, ToolState, analyze};
use kim_hotkeys::keys::{Chord, Step, steps_overlap};
use kim_hotkeys::manifest::Manifest;
use kim_hotkeys::register::{GlobalHotkeys, HotkeyId, RegisterError, Registrar, Status};
use kim_hotkeys::store::{self, ConfigState, Loaded};
use kim_hotkeys::{Scope, Sequence};
use serde_json::{Value, json};
use std::path::PathBuf;

/// This build's hotkey manifest (shipped as `hotkeys.manifest.json`; `version` = crate version).
pub const MANIFEST_JSON: &str = include_str!("hotkeys.manifest.json");

/// The tool ID of Noeul's shortcuts.
pub const TOOL: &str = "noeul";

/// The actions this build implements, in manifest order (NOEUL-FR-114).
pub const IMPLEMENTED: &[&str] = &[
    "capture.region",
    "paste.float",
    "capture.pin",
    "capture.copy",
    "capture.save",
    "capture.quick-save",
    "capture.cancel",
    "selection.full",
    "selection.last",
    "selection.move-up",
    "selection.move-down",
    "selection.move-left",
    "selection.move-right",
    "selection.grow-up",
    "selection.grow-down",
    "selection.grow-left",
    "selection.grow-right",
    "selection.shrink-up",
    "selection.shrink-down",
    "selection.shrink-left",
    "selection.shrink-right",
    "pin.close",
    "pin.destroy",
    "pin.copy",
    "pin.save",
    "pin.quick-save",
    "pin.zoom-in",
    "pin.zoom-out",
    "pin.zoom-reset",
    "pin.move-up",
    "pin.move-down",
    "pin.move-left",
    "pin.move-right",
    "pin.hide",
];

/// Global actions of the final manifest that later Steps add, for `--help` and the docs (none
/// since Step 50 shipped `capture.region`).
pub const PLANNED_GLOBAL: &[(&str, &str, u32)] = &[];

/// The overlay's context (UX spec §6).
pub const OVERLAY_CONTEXT: &str = "capture-overlay";
/// The pins' context.
pub const PIN_CONTEXT: &str = "pin";

pub fn manifest() -> Manifest {
    Manifest::parse(MANIFEST_JSON.as_bytes()).expect("the compiled-in hotkey manifest is valid")
}

/// `%APPDATA%\mouv\noeul\hotkeys.json`.
pub fn layer_path(paths: &Paths) -> PathBuf {
    paths.hotkeys_file()
}

pub fn state(manifest: Manifest, loaded: &Loaded) -> ToolState {
    let mut s = ToolState::new(manifest, loaded.layer.clone());
    s.safe_mode = loaded.state.safe_mode();
    s
}

/// Noeul's user layer and the tool state the checks use.
pub fn load(paths: &Paths) -> (ToolState, Loaded) {
    let m = manifest();
    let loaded = store::load(&layer_path(paths), &m);
    (state(m, &loaded), loaded)
}

/// A sentence for a file that puts Noeul in safe mode, or `None`.
pub fn safe_mode_warning(loaded: &Loaded) -> Option<String> {
    match loaded.state {
        ConfigState::Invalid => Some(format!(
            "{} is invalid ({}); Noeul uses its default shortcuts and registers no global hotkey until it is fixed or reset with `noeul hotkeys reset --all --yes`",
            loaded.path.display(),
            loaded
                .problems
                .first()
                .map_or("unknown problem", String::as_str)
        )),
        ConfigState::NewerVersion => Some(format!(
            "{} was written by a newer version and is read-only; Noeul uses its default shortcuts and registers no global hotkey",
            loaded.path.display()
        )),
        _ => None,
    }
}

/// Another installed Kim tool, as read from its files.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub entry: kim_hotkeys::check::ToolEntry,
    pub state: ToolState,
}

/// Every other installed Kim tool that ships a valid hotkey manifest, with its user layer, plus
/// a warning per tool that could not be checked (`hotkeys check --all-tools`, §9.2). Reads
/// Kim's install records (`<programs>\<tool>\current.json`), the installed version's
/// `hotkeys.manifest.json` and the tool's `hotkeys.json`; starts nothing.
pub fn installed(paths: &Paths) -> (Vec<Discovered>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Ok(rd) = std::fs::read_dir(&paths.programs) else {
        return (found, warnings);
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| !id.starts_with('.') && id != "bin" && id != TOOL)
        .collect();
    ids.sort();
    for id in ids {
        let dir = paths.programs.join(&id);
        let record = match std::fs::read(dir.join("current.json")) {
            Ok(b) => b,
            Err(_) if dir.join("versions").is_dir() => {
                warnings.push(format!(
                    "{id}: damaged install; its shortcuts were not checked"
                ));
                continue;
            }
            Err(_) => continue,
        };
        let Some(version) = serde_json::from_slice::<Value>(&record)
            .ok()
            .and_then(|v| v["version"].as_str().map(str::to_string))
        else {
            warnings.push(format!(
                "{id}: unreadable install record; its shortcuts were not checked"
            ));
            continue;
        };
        let path = dir
            .join("versions")
            .join(&version)
            .join(kim_hotkeys::manifest::MANIFEST_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                warnings.push(format!(
                    "{id} {version} ships no hotkey manifest; its shortcuts were not checked"
                ));
                continue;
            }
        };
        let m = match Manifest::parse(&bytes) {
            Ok(m) if m.tool == id && m.version == version => m,
            Ok(m) => {
                warnings.push(format!(
                    "{id}: {} describes {} {} instead of {id} {version}; it is not trusted",
                    path.display(),
                    m.tool,
                    m.version
                ));
                continue;
            }
            Err(e) => {
                warnings.push(format!(
                    "{id}: {} is not a usable hotkey manifest ({e}); its shortcuts were not checked",
                    path.display()
                ));
                continue;
            }
        };
        let loaded = store::load(&paths.roaming_root.join(&id).join(store::FILE_NAME), &m);
        let st = state(m, &loaded);
        found.push(Discovered {
            entry: kim_hotkeys::check::ToolEntry {
                tool: id.clone(),
                version: Some(version),
                source: "manifest",
                enabled: st.enabled(),
            },
            state: st,
        });
    }
    (found, warnings)
}

/// Why every shortcut is off for this run (start flags, §8.3 4), if it is.
pub fn off_reason(no_hotkeys_flag: bool) -> Option<&'static str> {
    if no_hotkeys_flag {
        Some("--no-hotkeys")
    } else if kim_hotkeys::no_hotkeys_env() {
        Some("KIM_NO_HOTKEYS")
    } else {
        None
    }
}

/// One global binding the instance wants registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub action: String,
    pub keys: String,
    pub chord: Chord,
}

/// The effective global bindings of `state`, split into registrable ones and ones the
/// `register` backend cannot express (sequences, side-specific modifiers, `code:` keys…).
pub fn global_plan(state: &ToolState) -> (Vec<Wanted>, Vec<(String, String, String)>) {
    let mut wanted = Vec::new();
    let mut unsupported = Vec::new();
    if state.safe_mode {
        return (wanted, unsupported);
    }
    for b in analyze(std::slice::from_ref(state), &[]).active {
        if b.scope != Scope::Global || !IMPLEMENTED.contains(&b.action.as_str()) {
            continue;
        }
        match b.seq.single_chord() {
            Some(c) => match kim_hotkeys::register::win32_chord(c) {
                Ok(_) => wanted.push(Wanted {
                    action: b.action.clone(),
                    keys: b.keys.clone(),
                    chord: *c,
                }),
                Err(e) => unsupported.push((b.action.clone(), b.keys.clone(), e.to_string())),
            },
            None => unsupported.push((
                b.action.clone(),
                b.keys.clone(),
                "sequences cannot be registered globally".into(),
            )),
        }
    }
    (wanted, unsupported)
}

/// The state of one global binding in the running instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub action: String,
    pub keys: String,
    /// `registered`, `taken`, `unsupported` or `failed`.
    pub status: &'static str,
    pub message: Option<String>,
}

/// The keys of one of Noeul's windows: the bindings of its context and the `app` bindings of
/// implemented actions, matched from the window's own key messages.
#[derive(Debug, Clone, Default)]
pub struct ContextKeys {
    /// `(action, first step, repeat, in the context)`.
    active: Vec<(String, Step, bool, bool)>,
}

impl ContextKeys {
    pub fn from_state(state: &ToolState, off: bool, context: &str) -> ContextKeys {
        if off {
            return ContextKeys::default();
        }
        let ctx = Scope::Context(context.into());
        let active = analyze(std::slice::from_ref(state), &[])
            .active
            .into_iter()
            .filter(|b| {
                (b.scope == ctx || b.scope == Scope::App)
                    && IMPLEMENTED.contains(&b.action.as_str())
            })
            .filter_map(|b| {
                let (repeat, inside) = (b.repeat, b.scope == ctx);
                b.seq
                    .steps()
                    .first()
                    .cloned()
                    .map(|s| (b.action, s, repeat, inside))
            })
            .collect();
        ContextKeys { active }
    }

    /// The action a key press in the focused window runs. Context bindings come first (§9.1);
    /// an auto-repeated press fires only bindings with `repeat: true` (§5.4).
    pub fn lookup(&self, pressed: &Chord, repeat: bool) -> Option<&str> {
        let step = Step::Chord(*pressed);
        let hit = |want_context: bool| {
            self.active
                .iter()
                .filter(|(_, _, _, inside)| *inside == want_context)
                .find(|(_, s, _, _)| steps_overlap(&step, s))
                .filter(|(_, _, r, _)| *r || !repeat)
                .map(|(a, _, _, _)| a.as_str())
        };
        hit(true).or_else(|| hit(false))
    }
}

/// The pin window's keys (`context:pin`).
pub type PinKeys = ContextKeys;

/// The instance's shortcut state: the loaded layer, the global registrations and the pin keys.
#[derive(Debug)]
pub struct Manager {
    manifest: Manifest,
    loaded: Loaded,
    off: Option<&'static str>,
    globals: Option<GlobalHotkeys>,
    registrar_error: Option<RegisterError>,
    registrations: Vec<Registration>,
    taken: Vec<Step>,
    conflicts: Vec<Conflict>,
    /// Why the file is not used (safe mode) or why the last reload was ignored.
    pub problem: Option<String>,
    pin_keys: ContextKeys,
    overlay_keys: ContextKeys,
    /// The effective keys of each active binding of an implemented action (`action`, `keys`).
    keys: Vec<(String, String)>,
}

impl Manager {
    /// Loads the layer and registers the global bindings. `registrar` starts the registration
    /// thread (with a waker in the instance); it is not called when shortcuts are off.
    pub fn start(
        paths: &Paths,
        off: Option<&'static str>,
        registrar: impl FnOnce() -> Result<Registrar, RegisterError>,
    ) -> Manager {
        let m = manifest();
        let loaded = store::load(&layer_path(paths), &m);
        let mut mgr = Manager {
            problem: safe_mode_warning(&loaded),
            manifest: m,
            loaded,
            off,
            globals: None,
            registrar_error: None,
            registrations: Vec::new(),
            taken: Vec::new(),
            conflicts: Vec::new(),
            pin_keys: ContextKeys::default(),
            overlay_keys: ContextKeys::default(),
            keys: Vec::new(),
        };
        if off.is_none() {
            match registrar() {
                Ok(r) => mgr.globals = Some(GlobalHotkeys::new(r)),
                Err(e) => mgr.registrar_error = Some(e),
            }
        }
        mgr.apply();
        mgr
    }

    fn tool_state(&self) -> ToolState {
        state(self.manifest.clone(), &self.loaded)
    }

    /// Makes the registrations match the current layer: releases what is no longer wanted,
    /// registers what is new, leaves working registrations alone (§8.3 5). A taken chord is
    /// recorded and offered a remap; it is retried only by the next explicit apply (a reload).
    pub fn apply(&mut self) {
        let st = self.tool_state();
        let off = self.off.is_some();
        self.pin_keys = ContextKeys::from_state(&st, off, PIN_CONTEXT);
        self.overlay_keys = ContextKeys::from_state(&st, off, OVERLAY_CONTEXT);
        self.keys = if off {
            Vec::new()
        } else {
            analyze(std::slice::from_ref(&st), &[])
                .active
                .into_iter()
                .filter(|b| IMPLEMENTED.contains(&b.action.as_str()))
                .map(|b| (b.action, b.keys))
                .collect()
        };
        let (wanted, unsupported) = if self.off.is_some() {
            (Vec::new(), Vec::new())
        } else {
            global_plan(&st)
        };
        let mut regs = Vec::new();
        let mut taken = Vec::new();
        if let Some(g) = &mut self.globals {
            let pairs: Vec<(String, Chord)> =
                wanted.iter().map(|w| (w.action.clone(), w.chord)).collect();
            for (action, result) in g.sync(&pairs) {
                let w = wanted.iter().find(|w| w.action == action).expect("wanted");
                let (status, message) = match result {
                    Ok(Status::Registered) => ("registered", None),
                    Ok(Status::Taken) => {
                        taken.push(Step::Chord(w.chord));
                        (
                            "taken",
                            Some(format!(
                                "{} is already registered by another program; Noeul did not take it",
                                w.keys
                            )),
                        )
                    }
                    Err(e) => ("failed", Some(e.to_string())),
                };
                regs.push(Registration {
                    action,
                    keys: w.keys.clone(),
                    status,
                    message,
                });
            }
        } else if let Some(e) = &self.registrar_error {
            for w in &wanted {
                regs.push(Registration {
                    action: w.action.clone(),
                    keys: w.keys.clone(),
                    status: "failed",
                    message: Some(e.to_string()),
                });
            }
        }
        for (action, keys, why) in unsupported {
            regs.push(Registration {
                action,
                keys,
                status: "unsupported",
                message: Some(why),
            });
        }
        self.conflicts = analyze(std::slice::from_ref(&st), &taken)
            .conflicts
            .into_iter()
            .filter(|c| c.severity() != Severity::Info)
            .collect();
        self.taken = taken;
        self.registrations = regs;
    }

    /// Re-reads `hotkeys.json`. A valid file is applied at once; an invalid one keeps the
    /// previous bindings and is reported (§8.3 5). Returns the problem, if any.
    pub fn reload(&mut self) -> Option<String> {
        let (next, problems) = store::reload(&self.loaded, &self.manifest);
        self.problem = match problems.first() {
            Some(p) => Some(format!(
                "hotkeys.json is invalid ({p}); the previous shortcuts stay active"
            )),
            None => safe_mode_warning(&next),
        };
        self.loaded = next;
        self.apply();
        self.problem.clone()
    }

    /// The action of a pressed global hotkey.
    pub fn action_for(&self, id: HotkeyId) -> Option<String> {
        self.globals
            .as_ref()
            .and_then(|g| g.action_for(id))
            .map(str::to_string)
    }

    /// The next queued press, without waiting (the instance's waker says when to look).
    pub fn take_press(&self) -> Option<HotkeyId> {
        self.globals
            .as_ref()
            .and_then(|g| g.registrar().next_press(std::time::Duration::ZERO))
    }

    pub fn pin_action(&self, chord: &Chord, repeat: bool) -> Option<String> {
        self.pin_keys.lookup(chord, repeat).map(str::to_string)
    }

    /// The overlay action of a key press in the capture overlay.
    pub fn overlay_action(&self, chord: &Chord, repeat: bool) -> Option<String> {
        self.overlay_keys.lookup(chord, repeat).map(str::to_string)
    }

    /// The first active keys of `action` (for the overlay's keyboard help), if it has any.
    pub fn keys_for(&self, action: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(a, _)| a == action)
            .map(|(_, k)| k.as_str())
    }

    /// The `registration` value of a global binding for `hotkeys list` (§10.2).
    pub fn registration(&self, action: &str, keys: &str) -> &'static str {
        self.registrations
            .iter()
            .find(|r| r.action == action && r.keys == keys)
            .map_or("unknown", |r| match r.status {
                "registered" => "registered",
                "taken" => "taken",
                _ => "unknown",
            })
    }

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    /// Unregisters everything (quit, logoff). Also done on drop.
    pub fn release_all(&mut self) {
        if let Some(g) = &mut self.globals {
            g.release_all();
        }
        self.registrations.clear();
    }

    /// `hotkeys` part of `status` (IPC and CLI): what is registered, what is taken and why
    /// shortcuts are off. Action IDs and keys only, never typed text (`KIM-SEC-022`).
    pub fn status_json(&self) -> Value {
        json!({
            "enabled": self.off.is_none() && self.tool_state().enabled() && !self.loaded.state.safe_mode(),
            "off_reason": self.off.map(str::to_string).or_else(|| {
                if self.loaded.state.safe_mode() {
                    Some("safe_mode".to_string())
                } else if !self.tool_state().enabled() {
                    Some("disabled".to_string())
                } else {
                    None
                }
            }),
            "config": {"path": self.loaded.path.display().to_string(), "state": self.loaded.state.as_str()},
            "problem": self.problem,
            "registrations": self.registrations.iter().map(|r| json!({
                "action": r.action, "keys": r.keys, "status": r.status, "message": r.message,
            })).collect::<Vec<_>>(),
            "conflicts": self.conflicts.iter().map(Conflict::to_json).collect::<Vec<_>>(),
            "planned": PLANNED_GLOBAL.iter().map(|(a, k, step)| json!({
                "action": a, "default": k, "step": step,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Builds a `Chord` from a pin window's key message: the logical key name (`Escape`, `W`,
/// `Up`…) and the modifiers held.
pub fn chord_of(name: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Chord> {
    let mut text = String::new();
    if ctrl {
        text.push_str("Ctrl+");
    }
    if alt {
        text.push_str("Alt+");
    }
    if shift {
        text.push_str("Shift+");
    }
    text.push_str(name);
    Sequence::parse(&text)
        .ok()
        .and_then(|s| s.single_chord().copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kim_hotkeys::layer::{Override, UserLayer};
    use kim_hotkeys::manifest::BindingSpec;

    fn repo() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn the_shipped_manifest_is_the_final_one_filtered_to_this_build() {
        let m = manifest();
        assert_eq!(
            m.version,
            crate::VERSION,
            "bump hotkeys.manifest.json with the crate"
        );
        assert_eq!(m.tool, TOOL);
        assert!(m.problems().is_empty(), "{:?}", m.problems());
        let mut fin: Value = serde_json::from_slice(
            &std::fs::read(
                repo().join("docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json"),
            )
            .unwrap(),
        )
        .unwrap();
        fin["version"] = json!(crate::VERSION);
        let keep = |a: &Value| IMPLEMENTED.contains(&a["id"].as_str().unwrap());
        let actions: Vec<Value> = fin["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| keep(a))
            .cloned()
            .collect();
        let used: Vec<String> = actions
            .iter()
            .filter_map(|a| a["context"].as_str().map(str::to_string))
            .collect();
        fin["actions"] = json!(actions);
        let contexts: Vec<Value> = fin["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| used.contains(&c["id"].as_str().unwrap().to_string()))
            .cloned()
            .collect();
        fin["contexts"] = json!(contexts);
        let ours: Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        assert_eq!(
            ours, fin,
            "same IDs, titles and defaults as the final manifest (NOEUL_UX_SPEC §6)"
        );
        let ids: Vec<&str> = m.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids, IMPLEMENTED,
            "the manifest lists exactly the implemented actions"
        );
        for (planned, _, _) in PLANNED_GLOBAL {
            assert!(!ids.contains(planned));
        }
    }

    fn st(layer: Option<UserLayer>) -> ToolState {
        ToolState::new(manifest(), layer)
    }

    #[test]
    fn global_plan_follows_the_layer() {
        let (w, u) = global_plan(&st(None));
        let plan: Vec<(&str, &str)> = w
            .iter()
            .map(|w| (w.action.as_str(), w.keys.as_str()))
            .collect();
        assert_eq!(
            plan,
            [("capture.region", "F1"), ("paste.float", "F3")],
            "F1 and F3 are the only global defaults (NOEUL-FR-100); planning registers nothing"
        );
        assert!(u.is_empty());

        let mut l = UserLayer::new(TOOL);
        l.set_override(
            None,
            "paste.float",
            Some(Override::bound(
                "paste.float",
                vec![BindingSpec::Text("Ctrl+Alt+Shift+F22".into())],
            )),
        );
        let (w, _) = global_plan(&st(Some(l.clone())));
        assert_eq!(w[1].keys, "Ctrl+Alt+Shift+F22");

        l.set_override(None, "paste.float", Some(Override::disabled("paste.float")));
        l.set_override(
            None,
            "capture.region",
            Some(Override::disabled("capture.region")),
        );
        assert!(global_plan(&st(Some(l))).0.is_empty());

        let mut safe = st(None);
        safe.safe_mode = true;
        assert!(
            global_plan(&safe).0.is_empty(),
            "safe mode registers nothing global"
        );
    }

    #[test]
    fn overlay_keys_match_the_capture_overlay_context() {
        let keys = ContextKeys::from_state(&st(None), false, OVERLAY_CONTEXT);
        let c = |n: &str, ctrl, alt, shift| chord_of(n, ctrl, alt, shift).unwrap();
        for (chord, repeat, want) in [
            (c("Enter", false, false, false), false, Some("capture.pin")),
            (
                c("Escape", false, false, false),
                false,
                Some("capture.cancel"),
            ),
            (c("A", true, false, false), false, Some("selection.full")),
            (c("R", false, false, false), false, Some("selection.last")),
            (
                c("Left", false, false, false),
                true,
                Some("selection.move-left"),
            ),
            (c("Up", true, false, false), true, Some("selection.grow-up")),
            (
                c("Down", false, false, true),
                true,
                Some("selection.shrink-down"),
            ),
            (c("Enter", false, false, false), true, None),
            (c("C", true, false, false), false, Some("capture.copy")),
            (c("S", true, false, true), false, Some("capture.quick-save")),
            (c("Space", false, false, false), false, None),
        ] {
            assert_eq!(
                keys.lookup(&chord, repeat),
                want,
                "{chord:?} repeat {repeat}"
            );
        }
        assert!(
            ContextKeys::from_state(&st(None), true, OVERLAY_CONTEXT)
                .lookup(&c("Enter", false, false, false), false)
                .is_none(),
            "--no-hotkeys turns the overlay's keys off (Escape still leaves, nav.cancel)"
        );
        for id in crate::overlay::ACTION_IDS {
            assert!(IMPLEMENTED.contains(id), "{id} is shipped");
        }
    }

    #[test]
    fn pin_keys_match_context_bindings() {
        let keys = PinKeys::from_state(&st(None), false, PIN_CONTEXT);
        let c = |n: &str, ctrl, alt, shift| chord_of(n, ctrl, alt, shift).unwrap();
        assert_eq!(
            keys.lookup(&c("Escape", false, false, false), false),
            Some("pin.close")
        );
        assert_eq!(
            keys.lookup(&c("W", true, false, false), false),
            Some("pin.close")
        );
        assert_eq!(
            keys.lookup(&c("Escape", false, false, false), true),
            None,
            "no repeat for close"
        );
        assert_eq!(
            keys.lookup(&c("Left", false, false, false), true),
            Some("pin.move-left")
        );
        assert_eq!(keys.lookup(&c("W", false, false, false), false), None);
        assert_eq!(
            keys.lookup(&c("Escape", false, false, true), false),
            Some("pin.destroy"),
            "Shift+Escape destroys (Step 53)"
        );
        assert_eq!(
            keys.lookup(&c("Plus", false, false, false), false),
            Some("pin.zoom-in")
        );
        assert_eq!(
            keys.lookup(&c("NumpadSubtract", false, false, false), false),
            Some("pin.zoom-out")
        );
        assert_eq!(
            keys.lookup(&c("0", false, false, false), false),
            Some("pin.zoom-reset")
        );
        assert_eq!(
            keys.lookup(&c("H", false, false, false), false),
            Some("pin.hide")
        );
        assert_eq!(
            keys.lookup(&c("T", false, false, false), false),
            None,
            "thumbnails are not in this build"
        );
        assert_eq!(
            keys.lookup(&c("F3", false, false, false), false),
            None,
            "F3 is global, not app"
        );
        assert!(
            PinKeys::from_state(&st(None), true, PIN_CONTEXT)
                .lookup(&c("Escape", false, false, false), false)
                .is_none()
        );
        assert_eq!(
            keys.lookup(&c("Enter", false, false, false), false),
            None,
            "overlay keys do not leak into pins"
        );
    }

    #[test]
    fn manager_off_registers_nothing_and_says_why() {
        let dir = std::env::temp_dir().join(format!("noeul-hk-off-{}", std::process::id()));
        let paths = Paths::under(&dir);
        let mgr = Manager::start(&paths, Some("--no-hotkeys"), || {
            panic!("no registrar is started when shortcuts are off")
        });
        let s = mgr.status_json();
        assert_eq!(s["enabled"], false);
        assert_eq!(s["off_reason"], "--no-hotkeys");
        assert!(s["registrations"].as_array().unwrap().is_empty());
        assert_eq!(
            s["planned"],
            json!([]),
            "nothing global is planned any more"
        );
        assert_eq!(mgr.keys_for("capture.pin"), None, "no keys while off");
        assert!(
            mgr.pin_action(&chord_of("Escape", false, false, false).unwrap(), false)
                .is_none()
        );
    }
}
