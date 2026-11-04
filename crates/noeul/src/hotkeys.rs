//! Noeul's keyboard shortcuts on the shared `kim-hotkeys` library (docs/HOTKEY_CONTRACT.md §13.3,
//! ADR 0017, ADR 0021 §5, NOEUL-FR-100, NOEUL-FR-114, NOEUL-SEC-012).
//!
//! - **Manifest.** The final manifest of every Noeul action is the contract example
//!   `docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json` (85 actions). A build ships only
//!   the actions it implements, under the same IDs and defaults (NOEUL-FR-114); this build's
//!   subset is [`MANIFEST_JSON`] (`hotkeys.manifest.json`, shipped next to `capabilities.json`). A
//!   test keeps it equal to the final manifest filtered by [`IMPLEMENTED`].
//! - **Global keys.** Backend `register` only: [`Manager`] registers the effective `global`
//!   bindings with `RegisterHotKey` through `kim_hotkeys::register` while the instance runs. A key
//!   another program owns is reported as `taken` with remap offers and never retried in the
//!   background (`KIM-SEC-020`). No hook, no polling, no keystroke is ever seen except a
//!   registered chord (`KIM-SEC-022`). `--no-hotkeys`, `KIM_NO_HOTKEYS`, `"enabled": false` and
//!   safe mode register nothing (§8.3, §12.4).
//! - **Context keys.** The `pin`, `capture-overlay` and `annotate` contexts (and `app` bindings)
//!   are matched from the pin and overlay windows' own key messages ([`ContextKeys`], backend
//!   `focus` semantics inside Noeul's windows); `annotate` is inside `capture-overlay`, so its keys
//!   win there while a selection is confirmed (Step 56). The overlay also always leaves on the reserved
//!   navigation key `Escape` (`nav.cancel`, §3.5), even with its shortcuts off.
//! - **Reload.** A running instance re-reads `hotkeys.json` when the file changes (a blocking
//!   change notification, §8.3 5) and when `noeul hotkeys …` tells it to over IPC. A valid file is
//!   applied at once; an invalid one is reported and the previous bindings stay.
//!
//! `capture.region` (`F1`) is registered since Step 50, which added its selection overlay and the
//! overlay keys it implements (ADR 0026); Step 49 left it out because the contract forbids
//! registering an action that does nothing (NOEUL-FR-114). Step 51 added the outputs
//! `capture.copy` / `pin.copy` (`Ctrl+C`), `capture.save` / `pin.save` (`Ctrl+S`) and
//! `capture.quick-save` / `pin.quick-save` (`Ctrl+Shift+S`). Step 52 added the precision keys
//! of the overlay: `selection.mark` (`Space`), `pointer.move-*` (`code:KeyW` `code:KeyA`
//! `code:KeyS` `code:KeyD`, matched by the key's physical position from its scan code,
//! [`physical_chord`]), `detect.cycle` (`E`), `magnifier.toggle` (`M`), `history.previous`
//! (`Comma`) and `history.next` (`Period`). Step 56 added the `annotate` context — `annotate.done`
//! (`Escape`), `annotate.undo` (`Ctrl+Z`), `annotate.redo` (`Ctrl+Y`, `Ctrl+Shift+Z`),
//! `annotate.width-down`/`-up` (`char:[`/`char:]`, matched from the typed character), the seven
//! tools and `annotate.clear-all` (bindable, no default), and the object keys `annotate.delete`
//! (`Delete`), `annotate.select-next`/`-previous` (`N`/`Shift+N`) and the order keys (`PageUp`,
//! `PageDown`, `Shift+PageUp`, `Shift+PageDown`) — and `pin.annotate` (`Space`). Step 57 added
//! the privacy tools `annotate.mosaic`, `annotate.blur`, `annotate.eraser` and `annotate.redact`
//! (bindable, no default). Step 54 added the pin transforms — `pin.rotate-cw`/`-ccw` (`1`/`2`),
//! `pin.flip-horizontal`/`-vertical` (`3`/`4`), `pin.opacity-up`/`-down` (`Ctrl+Plus`/
//! `Ctrl+Minus`), `pin.thumbnail` (`T`), `pin.click-through` (`Ctrl+Shift+T`) and `pin.group-new`
//! (no default) — and the first global actions without defaults, `pins.clickable` and
//! `pins.toggle-hidden` (the user binds them; HOTKEY_CONTRACT §7.5). Step 55 added
//! `pin.copy-text` (`Ctrl+Shift+C`): a text, HTML, colour or file card's text to the clipboard.
//! Step 58 added the global actions `capture.monitor` and `capture.window` (no default keys,
//! HOTKEY_CONTRACT §7.5): the monitor under the pointer, or the active window, captured without
//! the overlay and pinned in place. Step 59 added the colour picker's keys in the overlay,
//! `color.copy` (`C`) and `color.format-next` (`Shift+C`); while a label is edited they are
//! suspended with every other overlay key, so `C` types a "c" (ADR 0053).

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
    "pins.clickable",
    "pins.toggle-hidden",
    "capture.monitor",
    "capture.window",
    "capture.pin",
    "capture.copy",
    "capture.save",
    "capture.quick-save",
    "capture.cancel",
    "selection.mark",
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
    "pointer.move-up",
    "pointer.move-down",
    "pointer.move-left",
    "pointer.move-right",
    "detect.cycle",
    "magnifier.toggle",
    "color.copy",
    "color.format-next",
    "history.previous",
    "history.next",
    "annotate.done",
    "annotate.undo",
    "annotate.redo",
    "annotate.width-down",
    "annotate.width-up",
    "annotate.rectangle",
    "annotate.ellipse",
    "annotate.line",
    "annotate.arrow",
    "annotate.pen",
    "annotate.marker",
    "annotate.text",
    "annotate.mosaic",
    "annotate.blur",
    "annotate.eraser",
    "annotate.redact",
    "annotate.clear-all",
    "annotate.delete",
    "annotate.select-next",
    "annotate.select-previous",
    "annotate.bring-forward",
    "annotate.send-backward",
    "annotate.bring-to-front",
    "annotate.send-to-back",
    "pin.close",
    "pin.destroy",
    "pin.copy",
    "pin.save",
    "pin.quick-save",
    "pin.copy-text",
    "pin.zoom-in",
    "pin.zoom-out",
    "pin.zoom-reset",
    "pin.opacity-up",
    "pin.opacity-down",
    "pin.rotate-cw",
    "pin.rotate-ccw",
    "pin.flip-horizontal",
    "pin.flip-vertical",
    "pin.move-up",
    "pin.move-down",
    "pin.move-left",
    "pin.move-right",
    "pin.thumbnail",
    "pin.hide",
    "pin.click-through",
    "pin.group-new",
    "pin.annotate",
];

/// Global actions of the final manifest that later Steps add, for `--help` and the docs (none
/// since Step 50 shipped `capture.region`).
pub const PLANNED_GLOBAL: &[(&str, &str, u32)] = &[];

/// The overlay's context (UX spec §6).
pub const OVERLAY_CONTEXT: &str = "capture-overlay";
/// The pins' context.
pub const PIN_CONTEXT: &str = "pin";
/// The annotation context, inside the overlay's (Step 56).
pub const ANNOTATE_CONTEXT: &str = "annotate";

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
        self.lookup_any(&[*pressed], repeat)
    }

    /// The action of a key press among the context's own bindings only (not `app` bindings): an
    /// inner context that falls through to its outer one when nothing matches (Step 56).
    pub fn lookup_context(&self, pressed: &[Chord], repeat: bool) -> Option<&str> {
        pressed.iter().find_map(|chord| {
            let step = Step::Chord(*chord);
            self.active
                .iter()
                .filter(|(_, _, _, inside)| *inside)
                .find(|(_, s, _, _)| steps_overlap(&step, s))
                .filter(|(_, _, r, _)| *r || !repeat)
                .map(|(a, _, _, _)| a.as_str())
        })
    }

    /// The same for a key press that has several identities — its logical key and its physical
    /// position (`code:`, HOTKEY_CONTRACT §6.1) — tried in this order inside each precedence
    /// level, so the context still beats `app` bindings.
    pub fn lookup_any(&self, pressed: &[Chord], repeat: bool) -> Option<&str> {
        let hit = |want_context: bool, chord: &Chord| {
            let step = Step::Chord(*chord);
            self.active
                .iter()
                .filter(|(_, _, _, inside)| *inside == want_context)
                .find(|(_, s, _, _)| steps_overlap(&step, s))
                .filter(|(_, _, r, _)| *r || !repeat)
                .map(|(a, _, _, _)| a.as_str())
        };
        [true, false]
            .into_iter()
            .find_map(|ctx| pressed.iter().find_map(|c| hit(ctx, c)))
    }
}

/// The W3C UI Events `code` name of a scan code (set 1, as Windows reports it in a key message's
/// `lParam`) for the keys a `code:` binding may name: letters, digits and the punctuation of the
/// main block (HOTKEY_CONTRACT §5.2). Extended scan codes are not on that block.
pub fn code_of_scancode(scancode: u32, extended: bool) -> Option<&'static str> {
    if extended {
        return None;
    }
    Some(match scancode {
        0x02 => "Digit1",
        0x03 => "Digit2",
        0x04 => "Digit3",
        0x05 => "Digit4",
        0x06 => "Digit5",
        0x07 => "Digit6",
        0x08 => "Digit7",
        0x09 => "Digit8",
        0x0A => "Digit9",
        0x0B => "Digit0",
        0x0C => "Minus",
        0x0D => "Equal",
        0x10 => "KeyQ",
        0x11 => "KeyW",
        0x12 => "KeyE",
        0x13 => "KeyR",
        0x14 => "KeyT",
        0x15 => "KeyY",
        0x16 => "KeyU",
        0x17 => "KeyI",
        0x18 => "KeyO",
        0x19 => "KeyP",
        0x1A => "BracketLeft",
        0x1B => "BracketRight",
        0x1E => "KeyA",
        0x1F => "KeyS",
        0x20 => "KeyD",
        0x21 => "KeyF",
        0x22 => "KeyG",
        0x23 => "KeyH",
        0x24 => "KeyJ",
        0x25 => "KeyK",
        0x26 => "KeyL",
        0x27 => "Semicolon",
        0x28 => "Quote",
        0x29 => "Backquote",
        0x2B => "Backslash",
        0x2C => "KeyZ",
        0x2D => "KeyX",
        0x2E => "KeyC",
        0x2F => "KeyV",
        0x30 => "KeyB",
        0x31 => "KeyN",
        0x32 => "KeyM",
        0x33 => "Comma",
        0x34 => "Period",
        0x35 => "Slash",
        0x56 => "IntlBackslash",
        _ => return None,
    })
}

/// The physical chord (`code:…` with the modifiers held) of a key message's scan code.
pub fn physical_chord(
    scancode: u32,
    extended: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
) -> Option<Chord> {
    let code = code_of_scancode(scancode, extended)?;
    chord_of(&format!("code:{code}"), ctrl, alt, shift)
}

/// How a binding's keys read in the overlay's help: `code:KeyW` as `W` (its position on a US
/// keyboard), everything else as written.
pub fn key_label(keys: &str) -> String {
    keys.split('+')
        .map(|part| match part.strip_prefix("code:") {
            Some(code) => code
                .strip_prefix("Key")
                .or_else(|| code.strip_prefix("Digit"))
                .unwrap_or(code)
                .to_string(),
            None => part.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
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
    annotate_keys: ContextKeys,
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
            annotate_keys: ContextKeys::default(),
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
        self.annotate_keys = ContextKeys::from_state(&st, off, ANNOTATE_CONTEXT);
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

    /// The overlay action of a key press in the capture overlay: its logical chord, then its
    /// physical one (`code:` bindings such as `W A S D`, Step 52).
    pub fn overlay_action(
        &self,
        chord: &Chord,
        code: Option<&Chord>,
        repeat: bool,
    ) -> Option<String> {
        let mut pressed = vec![*chord];
        pressed.extend(code.copied());
        self.overlay_keys
            .lookup_any(&pressed, repeat)
            .map(str::to_string)
    }

    /// The `annotate` action of a key press in the overlay or the pin editor (Step 56): its
    /// logical chord, then its physical one; only the context's own bindings, so the outer
    /// context gets the key when none matches.
    pub fn annotate_action(
        &self,
        chord: &Chord,
        code: Option<&Chord>,
        repeat: bool,
    ) -> Option<String> {
        let mut pressed = vec![*chord];
        pressed.extend(code.copied());
        self.annotate_keys
            .lookup_context(&pressed, repeat)
            .map(str::to_string)
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

    /// Whether a global binding of `action` is registered now (a way back from click-through,
    /// Step 54).
    pub fn is_registered(&self, action: &str) -> bool {
        self.registrations
            .iter()
            .any(|r| r.action == action && r.status == "registered")
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

/// The chord of a typed character (`char:[`), for `char:` bindings (Step 56); `None` for
/// characters a binding cannot name.
pub fn char_chord(c: char) -> Option<Chord> {
    if c.is_control() || c.is_whitespace() {
        return None;
    }
    match Sequence::parse(&format!("char:{c}")).ok()?.steps() {
        [Step::Chord(chord)] => Some(*chord),
        _ => None,
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
            (
                c("Space", false, false, false),
                false,
                Some("selection.mark"),
            ),
            (c("E", false, false, false), false, Some("detect.cycle")),
            (c("M", false, false, false), false, Some("magnifier.toggle")),
            (
                c("Comma", false, false, false),
                false,
                Some("history.previous"),
            ),
            (
                c("Period", false, false, false),
                false,
                Some("history.next"),
            ),
            (c("Space", false, false, false), true, None),
            // W A S D are physical keys: the logical letters do nothing.
            (c("W", false, false, false), false, None),
        ] {
            assert_eq!(
                keys.lookup(&chord, repeat),
                want,
                "{chord:?} repeat {repeat}"
            );
        }
        // The physical position of W A S D, whatever the layout, with repeat (Step 52).
        let phys = |sc: u32| physical_chord(sc, false, false, false, false).unwrap();
        for (sc, want) in [
            (0x11, "pointer.move-up"),
            (0x1F, "pointer.move-down"),
            (0x1E, "pointer.move-left"),
            (0x20, "pointer.move-right"),
        ] {
            assert_eq!(keys.lookup(&phys(sc), true), Some(want));
            // On AZERTY the key at W's position is Z; its logical chord is tried first and
            // binds nothing, so the position wins.
            assert_eq!(
                keys.lookup_any(&[c("Z", false, false, false), phys(sc)], false),
                Some(want)
            );
        }
        // The logical chord wins inside the same level: `E` stays detection, not a position.
        assert_eq!(
            keys.lookup_any(&[c("E", false, false, false), phys(0x12)], false),
            Some("detect.cycle")
        );
        assert_eq!(physical_chord(0x11, true, false, false, false), None);
        assert_eq!(code_of_scancode(0x32, false), Some("KeyM"));
        assert_eq!(code_of_scancode(0x00, false), None);
        assert_eq!(key_label("code:KeyW"), "W");
        assert_eq!(key_label("Ctrl+code:Digit1"), "Ctrl+1");
        assert_eq!(key_label("Comma"), "Comma");
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
        // Step 54: the transforms.
        for (chord, want) in [
            (c("T", false, false, false), "pin.thumbnail"),
            (c("1", false, false, false), "pin.rotate-cw"),
            (c("2", false, false, false), "pin.rotate-ccw"),
            (c("3", false, false, false), "pin.flip-horizontal"),
            (c("4", false, false, false), "pin.flip-vertical"),
            (c("Plus", true, false, false), "pin.opacity-up"),
            (c("Minus", true, false, false), "pin.opacity-down"),
            (c("T", true, false, true), "pin.click-through"),
        ] {
            assert_eq!(keys.lookup(&chord, false), Some(want), "{chord:?}");
        }
        assert_eq!(
            keys.lookup(&c("Plus", false, false, false), false),
            Some("pin.zoom-in"),
            "Ctrl tells opacity from zoom: no conflict"
        );
        assert_eq!(
            keys.lookup(&c("1", false, false, false), true),
            None,
            "a held 1 turns the pin once"
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
