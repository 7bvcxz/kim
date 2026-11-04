//! The instance core (ADR 0023 §1): one state machine that owns the capture sessions, the pins
//! and the shortcuts, and answers every request on the instance's UI thread.
//!
//! Requests arrive from four separate sources, and none of them can reach the others' state
//! except through [`App`]: IPC from the CLI ([`App::request`]), global hotkeys and tray clicks
//! ([`App::action`], [`App::tray_command`]) and the pin windows' own events
//! ([`App::pin_event`]). Everything the core needs from the OS goes through [`Shell`] (pin
//! windows, the clipboard, the pointer, notices) and [`CaptureBackend`], so the whole lifecycle
//! runs headless in tests with fakes; `win::runtime` supplies the Win32 implementations.
//!
//! The core never captures on its own (NOEUL-SEC-001), reads the clipboard only for
//! `paste.float` (NOEUL-SEC-002), writes no picture to disk (NOEUL-SEC-005) and reports only IDs,
//! geometry and counts (NOEUL-SEC-008).
//!
//! **The capture overlay** (Step 50, ADR 0026): `capture.region` (`F1`, the tray's left click,
//! `noeul capture --interactive`) freezes every monitor into one frame, opens a capture session
//! with it and shows an [`OverlayWindow`] over the virtual desktop. The window reports
//! [`OverlayEvent`]s; the core feeds them to the [`Overlay`] model, redraws only what changed,
//! and on `capture.pin` hands the session's [`crate::selection::CaptureSelection`] to a new pin.
//! While an overlay is open, further capture requests do nothing (NOEUL-FR-013).
//!
//! **Outputs of the overlay** (Step 51, UX spec §4.2): the overlay model reports
//! `Output(Pin | Copy | Save | QuickSave, rect, via)` for a double-click or middle-click inside
//! the selection, a key, the toolbar or the selection's context menu. A pin goes through
//! [`App::pin_selection`] (no clipboard, no file; a pointer gesture guards the new pin for one
//! double-click time); a copy encodes PNG and `CF_DIBV5` in memory and hands them to
//! [`Shell::clipboard_write`]; a save asks the runtime for the save dialog ([`SaveRequest`],
//! shown with the core released) and writes only the file the user chose; a quick save writes a
//! new file in the quick-save folder. Every failure keeps the overlay and its selection with a
//! banner (NOEUL-FR-025); a clipboard that is busy or disabled never affects Pin.
//!
//! **The pin engine** (Step 53, ADR 0029): every pin, whatever made it, goes through
//! [`crate::pins::NewPin`]: the overlay's selection through [`App::pin_selection`] (the entry
//! point of Step 51's double-click, with no clipboard in between), a CLI region through
//! `capture`/`pin.create`, the clipboard through `paste.float`/`pin.create`. Pins are moved,
//! zoomed, hidden, shown, closed and destroyed independently from their keys, the mouse, their
//! context menu ([`PinMenu`], shown by the runtime with the core released), the tray and IPC
//! (`pin.list`, `pin.hide`, `pin.show`, `pin.close`, `pin.reopen`). A display change
//! ([`App::display_changed`]) keeps pins where they are and offers to bring the lost ones back.

use crate::capture::{CaptureBackend, DesktopState, check_region};
use crate::error::{Code, NoeulError, Result};
use crate::frame::{Frame, Ledger, Rect};
use crate::hotkeys::Manager;
use crate::ipc;
use crate::output;
use crate::overlay::{
    self, Action as OverlayAction, Cursor, GestureAction, Gestures, Input as OverlayInput, Measure,
    Nav, Outcome as OverlayOutcome, Output, Overlay, Scene, SystemValues, Via,
};
use crate::paths::Paths;
use crate::pins::{NewPin, PinError, PinId, PinWindows, Pins, Placement, Source};
use crate::selection::{
    CaptureSelection, Captures, Origin, Outcome as SessionOutcome, SessionError,
};
use crate::settings::{self, KeyboardHelp, UiState};
use crate::tray::{Click, MenuItem, menu};
use crate::util::rfc3339;
use crate::zoom;
use kim_hotkeys::keys::Chord;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// What the core needs from the desktop besides capturing.
pub trait Shell {
    fn pin_windows(&mut self) -> &mut dyn PinWindows;
    /// Reads the clipboard once and returns its image (`nothing_to_pin`,
    /// `clipboard_unavailable` or `image_invalid` otherwise). Called only for `paste.float`.
    fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Frame>;
    /// Puts a picture on the clipboard (Copy, NOEUL-FR-030): `png` as the registered `PNG`
    /// format and `dib` as `CF_DIBV5`, the clipboard opened for the write only and retried
    /// briefly when another program holds it (`clipboard_unavailable`). Never called for a pin.
    fn clipboard_write(&mut self, png: &[u8], dib: &[u8]) -> Result<()>;
    /// The user's Pictures folder (quick save defaults to its `Noeul` subfolder); `None` under
    /// the debug-build test hook, so a test never writes there.
    fn pictures_dir(&self) -> Option<PathBuf>;
    /// The local time `[year, month, day, hour, minute, second]` (saved files' names).
    fn local_time(&self) -> [u16; 6];
    /// The pointer position in device pixels.
    fn pointer(&self) -> (i32, i32);
    /// A short non-modal notice (a tray balloon when the icon is shown; nothing otherwise).
    fn notice(&mut self, text: &str);
    /// Whether the notification-area icon is shown.
    fn tray_shown(&self) -> bool;
    /// Asks once before quitting with open pins (GUI only; the CLI uses `--yes`).
    fn confirm_quit(&mut self, pins: usize) -> bool;
    /// Creates the capture overlay's window over `picture` (the frozen picture's rectangle in
    /// virtual-desktop device pixels), not shown yet.
    fn open_overlay(
        &mut self,
        picture: Rect,
    ) -> std::result::Result<Box<dyn OverlayWindow>, String>;
    /// The user's pointer settings, read now (NOEUL-NFR-012).
    fn system_values(&self) -> SystemValues;
    /// Whether Noeul's windows may take keyboard focus and stay on top. `false` only in the
    /// debug-build test hook, whose windows are off-screen and must never move the user's focus.
    fn interactive(&self) -> bool;
}

/// The capture overlay's window (`win::overlay`; a fake in tests). It owns a back buffer of
/// the picture's size and measures the labels' text.
pub trait OverlayWindow: Measure {
    /// Composes `dirty` of `scene` over `frozen` into the back buffer and repaints those parts.
    fn present(&mut self, frozen: &Frame, scene: &Scene, dirty: &[Rect]);
    /// Shows the window, on top and taking keyboard focus when `activate`, and paints it
    /// completely before returning; `false` when it could not be shown.
    fn show(&mut self, activate: bool) -> bool;
    fn set_cursor(&mut self, cursor: Cursor);
    /// Bytes of its back buffer (reported by `noeul status`).
    fn buffer_bytes(&self) -> u64;
    /// Destroys the window and frees its back buffer now.
    fn destroy(&mut self);
}

/// Something the overlay window reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayEvent {
    /// A pointer or focus input, in virtual-desktop device pixels.
    Input(OverlayInput),
    /// A key press in the overlay (matched against the `capture-overlay` context); `true` when
    /// it is an auto-repeat.
    Key(Chord, bool),
}

/// One first-frame measurement (NOEUL-NFR-003): from the request (the hotkey press reaching
/// the UI thread, a tray click, an IPC request) to the overlay painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySample {
    pub total_us: u64,
    /// Until the frozen picture was taken.
    pub capture_us: u64,
    /// From there until the first frame was painted (window, back buffer, composition).
    pub render_us: u64,
    pub width: u32,
    pub height: u32,
}

/// The last first-frame measurements of this instance (a local metric, ADR 0012).
#[derive(Debug, Default)]
pub struct Latency {
    totals: VecDeque<u64>,
    last: Option<LatencySample>,
    count: u64,
}

impl Latency {
    /// How many samples the percentiles cover.
    pub const KEEP: usize = 100;

    pub fn push(&mut self, s: LatencySample) {
        if self.totals.len() == Latency::KEEP {
            self.totals.pop_front();
        }
        self.totals.push_back(s.total_us);
        self.last = Some(s);
        self.count += 1;
    }

    /// The nearest-rank percentile `p` (1–100) of the kept totals.
    pub fn percentile(&self, p: u32) -> Option<u64> {
        if self.totals.is_empty() {
            return None;
        }
        let mut v: Vec<u64> = self.totals.iter().copied().collect();
        v.sort_unstable();
        let rank = (u64::from(p.clamp(1, 100)) * v.len() as u64).div_ceil(100);
        v.get(usize::try_from(rank).unwrap_or(1).saturating_sub(1))
            .copied()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "count": self.count,
            "last": self.last.map(|s| json!({
                "total_us": s.total_us, "capture_us": s.capture_us, "render_us": s.render_us,
                "width": s.width, "height": s.height,
            })),
            "p50_us": self.percentile(50),
            "p95_us": self.percentile(95),
        })
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// The open overlay: its capture session, model, window and the scene on screen.
struct OverlaySession {
    id: u64,
    model: Overlay,
    window: Box<dyn OverlayWindow>,
    scene: Scene,
}

impl OverlaySession {
    fn to_json(&self) -> Value {
        json!({
            "session": self.id,
            "phase": self.model.phase().as_str(),
            "selection": self.model.selection().map(|r| r.to_json()),
            "picture": self.model.picture().to_json(),
            "monitors": self.model.monitors().len(),
            "buffer_bytes": self.window.buffer_bytes(),
            "help": self.model.help_shown(),
            "focus": self.model.toolbar_focus().map_or("canvas", overlay::ToolbarItem::as_str),
        })
    }
}

/// The overlay's keyboard help, from the effective bindings and gestures (so a changed key or
/// gesture shows as such).
pub fn overlay_help(m: &Manager, gestures: Gestures) -> Vec<String> {
    let k = |a: &str| m.keys_for(a).map(str::to_string);
    let mut lines = vec!["Drag to select a region. Esc or right-click cancels.".to_string()];
    let mut outputs = Vec::new();
    match gestures.double_click {
        GestureAction::Pin => outputs.push("Double-click inside pins it".to_string()),
        GestureAction::Copy => outputs.push("Double-click inside copies it".to_string()),
        GestureAction::Save => outputs.push("Double-click inside saves it".to_string()),
        GestureAction::None => {}
    }
    for (action, verb) in [
        ("capture.copy", "copies"),
        ("capture.save", "saves"),
        ("capture.quick-save", "quick-saves"),
    ] {
        if let Some(x) = k(action) {
            outputs.push(format!("{x} {verb}"));
        }
    }
    if !outputs.is_empty() {
        lines.push(outputs.join(" \u{b7} "));
    }
    let group = |kind: &str| -> Option<String> {
        let mut keys = Vec::new();
        for d in ["up", "down", "left", "right"] {
            keys.push(k(&format!("selection.{kind}-{d}"))?);
        }
        let arrows = ["Up", "Down", "Left", "Right"];
        let prefix = keys[0].strip_suffix("Up").map(str::to_string);
        Some(match prefix {
            Some(p)
                if keys
                    .iter()
                    .zip(arrows)
                    .all(|(k, a)| k.strip_suffix(a) == Some(p.as_str())) =>
            {
                if p.is_empty() {
                    "Arrows".to_string()
                } else {
                    format!("{p}arrows")
                }
            }
            _ => keys.join("/"),
        })
    };
    let mut second = Vec::new();
    for (kind, verb) in [("move", "move"), ("grow", "grow"), ("shrink", "shrink")] {
        if let Some(g) = group(kind) {
            second.push(format!("{g} {verb}"));
        }
    }
    if !second.is_empty() {
        lines.push(second.join(" \u{b7} "));
    }
    let mut third = Vec::new();
    if let Some(x) = k("selection.full") {
        third.push(format!("{x} whole monitor"));
    }
    if let Some(x) = k("selection.last") {
        third.push(format!("{x} last area"));
    }
    if let Some(x) = k("capture.pin") {
        third.push(format!("{x} pins the selection"));
    }
    if !third.is_empty() {
        lines.push(third.join(" \u{b7} "));
    }
    lines
}

/// Something a pin window reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinEvent {
    /// A key press in a focused pin (matched against the `pin` context); `true` when it is an
    /// auto-repeat.
    Key(PinId, Chord, bool),
    /// The user dragged the pin to a new position.
    Moved(PinId, i32, i32),
    /// The window was closed by the system (e.g. Alt+F4 or end of session).
    Closed(PinId),
    /// The context menu was asked for: a right-click at this screen point, or the keyboard
    /// (`Shift+F10`, the menu key) with `None`.
    Menu(PinId, Option<(i32, i32)>),
    /// The mouse wheel turned over the pin: the delta (120 per notch, positive away from the
    /// user), the pointer, and whether `Ctrl` was held.
    Wheel(PinId, i32, (i32, i32), bool),
    /// A middle-click on the pin.
    MiddleClick(PinId),
}

/// One entry of a pin's context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMenuItem {
    /// The pin action it runs (`None` for a separator).
    pub action: Option<&'static str>,
    /// The text, with its key after a tab (`Close\tEscape`).
    pub label: String,
    pub enabled: bool,
}

/// A pin's context menu to show (NOEUL-FR-061: it lists every pin action of this build). The
/// runtime shows it modally with the core released, then reports the choice through
/// [`App::pin_menu_choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMenu {
    pub pin: PinId,
    /// Where it opens: the pointer of a right-click; `None` from the keyboard (the runtime then
    /// opens it at the pin's top-left corner).
    pub at: Option<(i32, i32)>,
    pub items: Vec<PinMenuItem>,
}

/// The selection's context menu to show (UX spec §3.2): the runtime shows it modally with the
/// core released, owned by the overlay window, then reports the choice through
/// [`App::overlay_menu_choice`]. Never shown under the debug-build test hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayMenu {
    /// Where it opens (the pointer of a right-click, or the selection's corner).
    pub at: (i32, i32),
    pub items: Vec<PinMenuItem>,
}

/// A save the user asked for (`capture.save`, the Save button): the runtime shows the save
/// dialog with the core released and answers with [`App::save_chosen`]. Under the debug-build
/// test hook no dialog is shown: the answer is a new file in the explicitly configured
/// quick-save folder, or "cancelled" when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    /// The capture session it belongs to (a later answer for another session is ignored).
    pub session: u64,
    pub rect: Rect,
    /// The suggested file name, `noeul-YYYYMMDD-HHMMSS.png` (NOEUL-FR-031).
    pub name: String,
    /// The folder the dialog starts in (the quick-save folder), if known.
    pub folder: Option<PathBuf>,
    /// Whether that folder was set by the user (`[capture] quick_save_folder`).
    pub explicit: bool,
}

/// The selection's context menu: this build's outputs and keys (NOEUL-FR-114).
const OVERLAY_MENU: &[Option<(&str, &str)>] = &[
    Some(("capture.pin", "Pin")),
    Some(("capture.copy", "Copy")),
    Some(("capture.save", "Save\u{2026}")),
    Some(("capture.quick-save", "Quick save")),
    None,
    Some(("selection.full", "Select monitor")),
    Some(("selection.last", "Last rectangle")),
    None,
    Some(("capture.cancel", "Cancel")),
];

/// How a pin is made from a capture selection (Step 51's gestures pass their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinOptions {
    /// Take keyboard focus: the user's own gesture (never under the debug-build test hook).
    pub focus: bool,
    /// Ignore presses on the new pin for this long: after a double-click, the system
    /// double-click time, so the rest of the gesture cannot touch the pin (UX spec §4.2 rule 10).
    pub guard: Option<Duration>,
}

/// The menu's entries: implemented pin actions only (NOEUL-FR-114).
const MENU: &[Option<(&str, &str)>] = &[
    Some(("pin.zoom-in", "Zoom in")),
    Some(("pin.zoom-out", "Zoom out")),
    Some(("pin.zoom-reset", "Zoom 100 %")),
    None,
    Some(("pin.hide", "Hide")),
    Some(("pin.close", "Close")),
    Some(("pin.destroy", "Destroy")),
];

/// One wheel notch.
const WHEEL_DELTA: i32 = 120;

/// Static facts about the running instance.
#[derive(Debug, Clone)]
pub struct Identity {
    pub pid: u32,
    pub endpoint: String,
    pub session: u32,
}

pub struct App<S: Shell> {
    shell: S,
    capture: Box<dyn CaptureBackend>,
    ledger: Arc<Ledger>,
    captures: Captures,
    pins: Pins,
    hotkeys: Manager,
    settings: settings::Loaded,
    paths: Paths,
    id: Identity,
    started_at: SystemTime,
    started: Instant,
    quit: bool,
    last_action: Option<(String, &'static str)>,
    overlay: Option<OverlaySession>,
    latency: Latency,
    ui_state: UiState,
    menu: Option<PinMenu>,
    /// The selection's context menu the runtime should show.
    overlay_menu: Option<OverlayMenu>,
    /// The save dialog the runtime should show.
    save_request: Option<SaveRequest>,
    /// Wheel deltas not yet worth a zoom step, per pin (high-resolution wheels send small ones).
    wheel: BTreeMap<PinId, i32>,
    /// Pins left off-screen by the last display change (a notice says so when it changes).
    lost: usize,
}

impl<S: Shell> std::fmt::Debug for App<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("pins", &self.pins.len())
            .field("overlay", &self.overlay.as_ref().map(|o| o.model.phase()))
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

fn session_error(e: SessionError) -> NoeulError {
    match e {
        SessionError::Busy { .. } => NoeulError::new(Code::Busy, "another capture is in progress")
            .hint("finish or cancel it first"),
        other => NoeulError::new(Code::Internal, format!("capture session: {other:?}")),
    }
}

fn pin_not_found(id: &str) -> NoeulError {
    NoeulError::new(Code::PinNotFound, format!("no pin `{id}`"))
        .hint("run `noeul pin list` to see the open pins")
}

impl<S: Shell> App<S> {
    pub fn new(
        shell: S,
        capture: Box<dyn CaptureBackend>,
        hotkeys: Manager,
        settings: settings::Loaded,
        paths: Paths,
        id: Identity,
    ) -> App<S> {
        App {
            shell,
            capture,
            ledger: Ledger::new(),
            captures: Captures::new(),
            pins: Pins::new(),
            hotkeys,
            settings,
            ui_state: UiState::load(&paths.ui_state_file()),
            paths,
            id,
            started_at: SystemTime::now(),
            started: Instant::now(),
            quit: false,
            last_action: None,
            overlay: None,
            latency: Latency::default(),
            menu: None,
            overlay_menu: None,
            save_request: None,
            wheel: BTreeMap::new(),
            lost: 0,
        }
    }

    /// Whether the capture overlay is open.
    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// The first-frame measurements so far.
    pub fn latency(&self) -> &Latency {
        &self.latency
    }

    pub fn shell(&mut self) -> &mut S {
        &mut self.shell
    }

    pub fn hotkeys(&mut self) -> &mut Manager {
        &mut self.hotkeys
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    pub fn pins(&self) -> &Pins {
        &self.pins
    }

    pub fn started_at(&self) -> String {
        rfc3339(self.started_at)
    }

    /// Set once a quit was accepted; the runtime then tears down.
    pub fn quitting(&self) -> bool {
        self.quit
    }

    /// One IPC request. Errors carry registered codes.
    pub fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        let dry_run = ipc::flag(params, "dry_run")?;
        match method {
            "status" => Ok(self.status()),
            "activate" => Ok(self.activate()),
            "capture" => self.capture(params, dry_run),
            "capture.region" => self.capture_region(Origin::Cli, Instant::now(), dry_run),
            "capture.cancel" => Ok(self.capture_cancel(dry_run)),
            "pin.list" => Ok(self.pins.list(&self.capture.desktop())),
            "pin.create" => self.pin_create(params, dry_run),
            "pin.hide" => self.pin_hide(params, dry_run),
            "pin.show" => self.pin_show(params, dry_run),
            "pin.close" => self.pin_close(params, dry_run),
            "pin.reopen" => self.pin_reopen(dry_run),
            "hotkeys.reload" => {
                let problem = self.hotkeys.reload();
                Ok(json!({"reloaded": problem.is_none(), "problem": problem,
                          "hotkeys": self.hotkeys.status_json()}))
            }
            "settings.reload" => Ok(json!({"reloaded": true, "settings": self.reload_settings()})),
            "quit" => self.quit_request(ipc::flag(params, "force")?, dry_run),
            other => Err(NoeulError::new(
                Code::Usage,
                format!("unknown method `{other}`"),
            )),
        }
    }

    /// Re-reads `config.toml` (the settings watcher, IPC `settings.reload`). The gestures, the
    /// keyboard help and the quick-save folder apply from the next capture; the tray icon
    /// setting at the next start.
    pub fn reload_settings(&mut self) -> Value {
        self.settings = settings::load(&self.paths.config_file());
        self.settings.to_json()
    }

    pub fn settings(&self) -> &settings::Loaded {
        &self.settings
    }

    pub fn status(&self) -> Value {
        let (started, pinned, cancelled) = self.captures.counts();
        let (copied, saved) = self.captures.output_counts();
        json!({
            "running": true,
            "instance": {
                "pid": self.id.pid,
                "version": crate::VERSION,
                "started_at": rfc3339(self.started_at),
                "uptime_s": self.started.elapsed().as_secs(),
                "endpoint": self.id.endpoint,
                "session": self.id.session,
            },
            "desktop": self.capture.desktop().to_json(),
            "capture_backend": self.capture.name(),
            "pins": {"open": self.pins.len(), "reopenable": self.pins.reopenable().map(|p| p.to_string()),
                     "hidden": self.pins.hidden(), "off_screen": self.pins.off_screen(&self.capture.desktop())},
            "captures": {"started": started, "pinned": pinned, "copied": copied, "saved": saved,
                         "cancelled": cancelled, "open": self.captures.is_open(),
                         "ignored": self.captures.ignored(),
                         "overlay": self.overlay.as_ref().map(OverlaySession::to_json),
                         "first_frame": self.latency.to_json()},
            "memory": self.ledger.to_json(),
            "hotkeys": self.hotkeys.status_json(),
            "tray": {"enabled": self.settings.settings.tray_icon, "shown": self.shell.tray_shown()},
            "settings": self.settings.to_json(),
            "last_action": self.last_action.as_ref().map(|(a, r)| json!({"action": a, "result": r})),
        })
    }

    /// A second start handed over (NOEUL-FR-003). The settings window it should open is N5.
    fn activate(&mut self) -> Value {
        self.shell.notice("Noeul is already running.");
        json!({"activated": true, "pid": self.id.pid, "settings_window": "planned"})
    }

    fn capture(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let region = params
            .get("region")
            .and_then(Rect::from_json)
            .ok_or_else(|| {
                NoeulError::new(
                    Code::Usage,
                    "`region` must be {x, y, width, height} in device pixels",
                )
            })?;
        if params.get("pin") != Some(&Value::Bool(true)) {
            return Err(NoeulError::new(
                Code::Usage,
                "the instance only pins captures in this build (`pin: true`)",
            )
            .hint("capture to a file or the clipboard is planned (NOEUL-FR-112)"));
        }
        let desktop = self.capture.desktop();
        check_region(&desktop, region).map_err(NoeulError::from)?;
        if dry_run {
            return Ok(
                json!({"dry_run": true, "region": region.to_json(), "pin": null,
                             "desktop": desktop.state.as_str()}),
            );
        }
        let frame = self
            .capture
            .capture(&self.ledger, region)
            .map_err(NoeulError::from)?;
        let sid = self
            .captures
            .begin(Origin::Cli, frame, Vec::new())
            .map_err(session_error)?;
        self.captures.select(sid, region).map_err(session_error)?;
        let frame = self.captures.finish_pin(sid).map_err(session_error)?;
        let id = self.create_pin(&desktop, frame, Source::Capture, Placement::Exact)?;
        self.last_action = Some(("capture".into(), "pinned"));
        Ok(
            json!({"dry_run": false, "region": region.to_json(), "pin": self.pin_ref(id),
                   "desktop": desktop.state.as_str()}),
        )
    }

    /// A pin requested over IPC: never with focus, so it never takes the keyboard from the
    /// program the user is in (NOEUL-FR-060); announced like every CLI capture or clipboard read
    /// (the default notice of NOEUL-SEC-010).
    fn create_pin(
        &mut self,
        desktop: &crate::capture::Desktop,
        frame: Frame,
        source: Source,
        placement: Placement,
    ) -> Result<PinId> {
        let new = NewPin {
            frame,
            source,
            placement,
            focus: false,
            guard: None,
        };
        match self.pins.create(self.shell.pin_windows(), desktop, new) {
            Ok(id) => {
                self.shell.notice(match source {
                    Source::Capture => "Noeul captured a region (command line).",
                    Source::Clipboard => "Noeul pinned the clipboard (command line).",
                });
                Ok(id)
            }
            Err((frame, why)) => {
                drop(frame);
                Err(NoeulError::new(
                    Code::Internal,
                    format!("the pin window could not be created: {why}"),
                )
                .hint("nothing was pinned, copied or saved"))
            }
        }
    }

    /// `{id, rect, source, zoom}` of an open pin.
    fn pin_ref(&self, id: PinId) -> Value {
        self.pins.info(id).map_or(Value::Null, |i| {
            json!({"id": id.to_string(), "rect": i.rect.to_json(), "source": i.source.as_str(),
                   "zoom": i.zoom})
        })
    }

    /// The pins a request names: `all: true`, or one `id` that must be open.
    fn pin_targets(&self, params: &Value) -> Result<Vec<PinId>> {
        if ipc::flag(params, "all")? {
            return Ok(self.pins.ids());
        }
        let text = params["id"]
            .as_str()
            .ok_or_else(|| NoeulError::new(Code::Usage, "give a pin id (`id`) or `all: true`"))?;
        let id = PinId::parse(text).ok_or_else(|| pin_not_found(text))?;
        if !self.pins.contains(id) {
            return Err(pin_not_found(text));
        }
        Ok(vec![id])
    }

    /// `pin.create`: the instance reads the source itself (pictures never cross the pipe).
    /// `source: "region"` captures that region of the unlocked desktop and pins it in place;
    /// `source: "clipboard"` reads the clipboard once and pins its image at `at` (or the
    /// pointer), inside that monitor's work area and zoomed out when larger (NOEUL-SEC-002,
    /// ADR 0029). A dry run captures and reads nothing.
    fn pin_create(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let source = params["source"].as_str().unwrap_or("");
        let desktop = self.capture.desktop();
        match source {
            "region" => {
                let region = params
                    .get("region")
                    .and_then(Rect::from_json)
                    .ok_or_else(|| {
                        NoeulError::new(
                            Code::Usage,
                            "`region` must be {x, y, width, height} in device pixels",
                        )
                    })?;
                check_region(&desktop, region).map_err(NoeulError::from)?;
                if dry_run {
                    return Ok(json!({"dry_run": true, "source": "region", "pin": null}));
                }
                let frame = self
                    .capture
                    .capture(&self.ledger, region)
                    .map_err(NoeulError::from)?;
                let sid = self
                    .captures
                    .begin(Origin::Cli, frame, Vec::new())
                    .map_err(session_error)?;
                self.captures.select(sid, region).map_err(session_error)?;
                let frame = self.captures.finish_pin(sid).map_err(session_error)?;
                let id = self.create_pin(&desktop, frame, Source::Capture, Placement::Exact)?;
                self.last_action = Some(("pin.create".into(), "pinned"));
                Ok(json!({"dry_run": false, "source": "region", "pin": self.pin_ref(id)}))
            }
            "clipboard" => {
                if let Some(e) = desktop.state.error() {
                    return Err(e);
                }
                let at = match params.get("at") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(point(v).ok_or_else(|| {
                        NoeulError::new(Code::Usage, "`at` must be {x, y} in device pixels")
                    })?),
                };
                if dry_run {
                    // A dry run never opens the clipboard.
                    return Ok(json!({"dry_run": true, "source": "clipboard", "pin": null}));
                }
                let frame = self.shell.clipboard_image(&self.ledger)?;
                let (x, y) = at.unwrap_or_else(|| self.shell.pointer());
                let id =
                    self.create_pin(&desktop, frame, Source::Clipboard, Placement::At(x, y))?;
                self.last_action = Some(("pin.create".into(), "pinned"));
                Ok(json!({"dry_run": false, "source": "clipboard", "pin": self.pin_ref(id)}))
            }
            _ => Err(NoeulError::new(
                Code::Usage,
                "`source` must be \"region\" (with `region`) or \"clipboard\"",
            )
            .hint("pinning a file is planned (NOEUL-FR-073)")),
        }
    }

    /// `pin.hide`: hides pins without closing them. Nothing takes focus; a hidden pin that had
    /// it hands it back to the window the user was in before.
    fn pin_hide(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let mut hidden = Vec::new();
        for id in ids {
            let visible = self.pins.info(id).is_some_and(|i| i.visible);
            if visible && (dry_run || self.pins.hide(id).unwrap_or(false)) {
                hidden.push(id.to_string());
            }
        }
        if !dry_run && !hidden.is_empty() {
            self.last_action = Some(("pin.hide".into(), "hidden"));
        }
        Ok(json!({"dry_run": dry_run, "hidden": hidden}))
    }

    /// `pin.show`: shows hidden pins (above the others, without focus); with `on_screen`, also
    /// moves every named pin that no monitor shows into the nearest work area (NOEUL-FR-067).
    fn pin_show(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let bring = ipc::flag(params, "on_screen")?;
        let desktop = self.capture.desktop();
        let (mut shown, mut moved) = (Vec::new(), Vec::new());
        for id in ids {
            let Some(info) = self.pins.info(id) else {
                continue;
            };
            if !info.visible && (dry_run || self.pins.show(id).unwrap_or(false)) {
                shown.push(id.to_string());
            }
            let lost = bring
                && desktop.state == DesktopState::Available
                && !crate::pins::on_screen(&desktop, info.rect);
            if lost && (dry_run || self.pins.bring_on_screen(id, &desktop).unwrap_or(false)) {
                moved.push(id.to_string());
            }
        }
        if !dry_run && !(shown.is_empty() && moved.is_empty()) {
            self.last_action = Some(("pin.show".into(), "shown"));
        }
        Ok(json!({"dry_run": dry_run, "shown": shown, "moved": moved}))
    }

    fn pin_close(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
        if !dry_run {
            for id in &ids {
                let _ = self.pins.close(*id);
            }
        }
        Ok(json!({"dry_run": dry_run, "closed": names,
                  "reopenable": if dry_run { names.last().cloned() } else { self.pins.reopenable().map(|p| p.to_string()) }}))
    }

    fn pin_reopen(&mut self, dry_run: bool) -> Result<Value> {
        let Some(id) = self.pins.reopenable() else {
            return Err(
                NoeulError::new(Code::PinNotFound, "no closed pin to reopen")
                    .hint("Noeul remembers the last closed pin until it quits"),
            );
        };
        if dry_run {
            return Ok(json!({"dry_run": true, "pin": id.to_string()}));
        }
        match self.pins.reopen(self.shell.pin_windows(), false) {
            Ok(id) => Ok(json!({"dry_run": false, "pin": id.to_string()})),
            Err(PinError::Window(why)) => Err(NoeulError::new(
                Code::Internal,
                format!("the pin window could not be created: {why}"),
            )),
            Err(_) => Err(NoeulError::new(
                Code::PinNotFound,
                "no closed pin to reopen",
            )),
        }
    }

    fn quit_request(&mut self, force: bool, dry_run: bool) -> Result<Value> {
        let open = self.pins.len();
        if open > 0 && !force {
            return Err(NoeulError::new(
                Code::ConfirmationRequired,
                format!("{open} pin(s) are open; quitting closes them, and pins are not backed up"),
            )
            .hint("re-run with --yes, or --dry-run to preview"));
        }
        if dry_run {
            return Ok(json!({"dry_run": true, "quitting": false, "closed_pins": open}));
        }
        self.shutdown();
        Ok(json!({"dry_run": false, "quitting": true, "closed_pins": open}))
    }

    /// Ends everything the instance holds: the capture session, every pin and the global
    /// hotkeys (NOEUL-FR-002). Idempotent; the runtime calls it again on any exit path.
    pub fn shutdown(&mut self) {
        self.quit = true;
        if let Some(mut ov) = self.overlay.take() {
            ov.window.destroy();
        }
        self.overlay_menu = None;
        self.save_request = None;
        self.captures.cancel();
        self.pins.clear();
        self.hotkeys.release_all();
    }

    /// A Noeul action from a global hotkey, the tray, a pin's keys or its menu (by action ID).
    pub fn action(&mut self, action: &str, pin: Option<PinId>) {
        let found = |ok: bool, word: &'static str| if ok { word } else { "not_found" };
        let result = match (action, pin) {
            ("capture.region", _) => self.capture_region_action(Origin::Hotkey, Instant::now()),
            ("paste.float", _) => self.paste_float(),
            ("pin.close", Some(id)) => found(self.pins.close(id).is_ok(), "closed"),
            ("pin.destroy", Some(id)) => found(self.pins.destroy(id).is_ok(), "destroyed"),
            ("pin.hide", Some(id)) => match self.pins.hide(id) {
                Ok(_) => {
                    if self.pins.hidden() == 1 {
                        self.shell.notice(
                            "Pin hidden. Show it again from the tray (Show all pins) or with `noeul pin show --all`.",
                        );
                    }
                    "hidden"
                }
                Err(_) => "not_found",
            },
            ("pin.move-up", Some(id)) => self.nudge(id, 0, -1),
            ("pin.move-down", Some(id)) => self.nudge(id, 0, 1),
            ("pin.move-left", Some(id)) => self.nudge(id, -1, 0),
            ("pin.move-right", Some(id)) => self.nudge(id, 1, 0),
            ("pin.zoom-in" | "pin.zoom-out" | "pin.zoom-reset", Some(id)) => {
                let anchor = self.pins.zoom_anchor(id, self.shell.pointer());
                self.zoom_step(id, action, anchor)
            }
            _ => "ignored",
        };
        self.last_action = Some((action.to_string(), result));
    }

    /// One zoom step of a pin around `anchor` (`pin.zoom-in`, `-out`, `-reset`).
    fn zoom_step(&mut self, id: PinId, action: &str, anchor: Option<(i32, i32)>) -> &'static str {
        let (Some(info), Some(anchor)) = (self.pins.info(id), anchor) else {
            return "not_found";
        };
        let (w, h) = info.image;
        let to = match action {
            "pin.zoom-in" => zoom::zoom_in(w, h, info.zoom),
            "pin.zoom-out" => zoom::zoom_out(info.zoom),
            _ => Some(zoom::FULL),
        };
        match to.map(|z| self.pins.set_zoom(id, z, anchor)) {
            Some(Ok(true)) => "zoomed",
            Some(Ok(false)) | None => "unchanged",
            Some(Err(_)) => "failed",
        }
    }

    fn nudge(&mut self, id: PinId, dx: i32, dy: i32) -> &'static str {
        if self.pins.nudge(id, dx, dy) {
            "moved"
        } else {
            "not_found"
        }
    }

    /// `paste.float`: read the clipboard once and pin its image at the pointer, at 100 % unless
    /// it is larger than the work area of the monitor there (then zoomed out to fit, ADR 0029;
    /// NOEUL-FR-070, image formats only in this build). Nothing to pin → a notice, no change
    /// (NOEUL-FR-074). While the capture overlay is open it does nothing: a new pin would cover
    /// the overlay or take its focus.
    fn paste_float(&mut self) -> &'static str {
        if self.capture.desktop().state != DesktopState::Available {
            return "desktop_unavailable";
        }
        if self.overlay.is_some() {
            return "overlay_open";
        }
        let frame = match self.shell.clipboard_image(&self.ledger) {
            Ok(f) => f,
            Err(e) => {
                let text = match e.code {
                    Code::NothingToPin => "Nothing to pin: the clipboard holds no image.",
                    Code::ClipboardUnavailable => {
                        "The clipboard is in use by another program; try again."
                    }
                    Code::ImageInvalid => "The clipboard image could not be read.",
                    _ => "The clipboard could not be read.",
                };
                self.shell.notice(text);
                return e.code.as_str();
            }
        };
        let (px, py) = self.shell.pointer();
        let desktop = self.capture.desktop();
        let new = NewPin {
            frame,
            source: Source::Clipboard,
            placement: Placement::At(px, py),
            focus: self.shell.interactive(),
            guard: None,
        };
        match self.pins.create(self.shell.pin_windows(), &desktop, new) {
            Ok(_) => "pinned",
            Err((_, _)) => {
                self.shell.notice("The pin window could not be created.");
                "failed"
            }
        }
    }

    /// `capture.region`: freezes every monitor and opens the selection overlay over the whole
    /// virtual desktop (NOEUL-FR-010). While an overlay is open it does nothing and counts the
    /// request as ignored, so rapid repeated presses never open a second one and never capture
    /// again (NOEUL-FR-013). On a locked, secure or missing desktop nothing is captured
    /// (NOEUL-SEC-003). `trigger` is when the request arrived; the first-frame latency is
    /// measured from it (NOEUL-NFR-003).
    pub fn capture_region(
        &mut self,
        origin: Origin,
        trigger: Instant,
        dry_run: bool,
    ) -> Result<Value> {
        if let Some(open) = self.captures.open_id() {
            if !dry_run {
                self.captures.note_ignored();
            }
            return Ok(
                json!({"dry_run": dry_run, "opened": false, "reason": "already_open",
                "session": open, "picture": self.overlay.as_ref().map(|o| o.model.picture().to_json()),
                "monitors": self.overlay.as_ref().map_or(0, |o| o.model.monitors().len()),
                "first_frame_us": null}),
            );
        }
        let desktop = self.capture.desktop();
        if let Some(e) = desktop.state.error() {
            return Err(e);
        }
        let Some(picture) = desktop.virtual_rect else {
            return Err(DesktopState::NoDisplay
                .error()
                .unwrap_or_else(|| NoeulError::new(Code::Internal, "no display")));
        };
        if dry_run {
            return Ok(
                json!({"dry_run": true, "opened": false, "reason": null, "session": null,
                "picture": picture.to_json(), "monitors": desktop.monitors.len(), "first_frame_us": null}),
            );
        }
        let frame = self
            .capture
            .capture(&self.ledger, picture)
            .map_err(NoeulError::from)?;
        let captured = Instant::now();
        let id = self
            .captures
            .begin(origin, frame, desktop.monitors.clone())
            .map_err(session_error)?;
        let mut window = match self.shell.open_overlay(picture) {
            Ok(w) => w,
            Err(why) => {
                self.captures.cancel();
                return Err(NoeulError::new(
                    Code::Internal,
                    format!("the capture overlay could not be created: {why}"),
                )
                .hint("the frozen picture was discarded; nothing was pinned, copied or saved"));
            }
        };
        let help = self.help_lines();
        let monitors = desktop.monitors.len();
        let model = Overlay::new(
            picture,
            desktop.monitors,
            self.shell.system_values(),
            self.shell.pointer(),
            self.captures.last(),
            help,
        )
        .with_gestures(self.settings.settings.gestures);
        let scene = model.scene(&*window);
        if let Some(frozen) = self.captures.frozen() {
            window.present(frozen, &scene, &[picture]);
        }
        window.set_cursor(model.cursor());
        if !window.show(self.shell.interactive()) {
            window.destroy();
            self.captures.cancel();
            return Err(NoeulError::new(
                Code::Internal,
                "the capture overlay could not be shown",
            ));
        }
        let done = Instant::now();
        let sample = LatencySample {
            total_us: micros(done.saturating_duration_since(trigger)),
            capture_us: micros(captured.saturating_duration_since(trigger)),
            render_us: micros(done.saturating_duration_since(captured)),
            width: picture.width,
            height: picture.height,
        };
        self.latency.push(sample);
        self.overlay = Some(OverlaySession {
            id,
            model,
            window,
            scene,
        });
        self.last_action = Some(("capture.region".into(), "opened"));
        Ok(
            json!({"dry_run": false, "opened": true, "reason": null, "session": id,
            "picture": picture.to_json(), "monitors": monitors, "first_frame_us": sample.total_us}),
        )
    }

    /// `capture.region` from a hotkey or the tray: the result word for `last_action`; a failure
    /// that is not "busy" or "locked" is shown as a notice.
    fn capture_region_action(&mut self, origin: Origin, trigger: Instant) -> &'static str {
        match self.capture_region(origin, trigger, false) {
            Ok(v) if v["opened"] == true => "opened",
            Ok(_) => "ignored",
            Err(e) => {
                if e.code != Code::DesktopUnavailable {
                    self.shell.notice("Noeul could not capture the screen.");
                }
                e.code.as_str()
            }
        }
    }

    /// IPC `capture.cancel`: closes an open overlay without output.
    fn capture_cancel(&mut self, dry_run: bool) -> Value {
        let open = self.captures.open_id();
        if !dry_run && !self.cancel_overlay() {
            self.captures.cancel();
        }
        json!({"dry_run": dry_run, "cancelled": open.is_some() && !dry_run, "session": open})
    }

    /// Whether the overlay shows its keyboard help now (`[capture] keyboard_help`).
    fn help_lines(&self) -> Option<Vec<String>> {
        let show = match self.settings.settings.keyboard_help {
            KeyboardHelp::FirstRun => !self.ui_state.overlay_help_seen,
            KeyboardHelp::Always => true,
            KeyboardHelp::Never => false,
        };
        show.then(|| overlay_help(&self.hotkeys, self.settings.settings.gestures))
    }

    /// The first capture finished from the overlay: the first-run help is not shown again.
    fn help_done(&mut self) {
        if self.settings.settings.keyboard_help == KeyboardHelp::FirstRun
            && !self.ui_state.overlay_help_seen
        {
            self.ui_state.overlay_help_seen = true;
            // A hint only: if the file cannot be written, the help shows again next time.
            let _ = self.ui_state.save(&self.paths.ui_state_file());
        }
    }

    /// Routes an overlay window event.
    pub fn overlay_event(&mut self, e: OverlayEvent) {
        if self.overlay.is_none() {
            return;
        }
        let input = match e {
            OverlayEvent::Input(i) => {
                if matches!(i, OverlayInput::Press { .. }) {
                    // The user's double-click time, rectangle and drag threshold as they are now,
                    // so a change in Windows applies from the next press (UX spec §4.2 rule 13).
                    let sys = self.shell.system_values();
                    if let Some(ov) = self.overlay.as_mut() {
                        ov.model.set_system_values(sys);
                    }
                }
                i
            }
            OverlayEvent::Key(chord, repeat) => match self.overlay_key(&chord, repeat) {
                Some(i) => i,
                None => return,
            },
        };
        let outcome = match self.overlay.as_mut() {
            Some(ov) => ov.model.handle(input),
            None => return,
        };
        self.overlay_outcome(outcome);
    }

    fn overlay_outcome(&mut self, outcome: OverlayOutcome) {
        match outcome {
            OverlayOutcome::Stay => self.redraw_overlay(),
            OverlayOutcome::Cancel => {
                self.cancel_overlay();
            }
            OverlayOutcome::Output(out, rect, via) => match out {
                Output::Pin => self.pin_from_overlay(rect, via),
                Output::Copy => self.copy_from_overlay(rect),
                Output::Save => self.request_save(rect),
                Output::QuickSave => self.quick_save(rect),
            },
            OverlayOutcome::Menu(at) => self.open_overlay_menu(at),
        }
    }

    /// A key press in the overlay: the navigation keys go to a focused toolbar button first
    /// (ADR 0021 §5), then the `capture-overlay` bindings, then the reserved `Escape`; `Tab`
    /// and `Shift+Tab` move the focus into the toolbar when no binding uses them.
    fn overlay_key(&self, chord: &Chord, repeat: bool) -> Option<OverlayInput> {
        let focused = self
            .overlay
            .as_ref()
            .is_some_and(|o| o.model.toolbar_focus().is_some());
        let nav = nav_of(chord);
        if focused
            && let Some(n) = nav
            && !(repeat && matches!(n, Nav::Activate | Nav::Escape))
        {
            return Some(OverlayInput::Nav(n));
        }
        if let Some(a) = self
            .hotkeys
            .overlay_action(chord, repeat)
            .as_deref()
            .and_then(OverlayAction::from_id)
        {
            return Some(OverlayInput::Action(a));
        }
        // The reserved navigation key always leaves (nav.cancel, NOEUL-SEC-013).
        if !repeat && is_escape(chord) {
            return Some(OverlayInput::Leave);
        }
        match nav {
            Some(n @ (Nav::Next | Nav::Prev)) => Some(OverlayInput::Nav(n)),
            _ => None,
        }
    }

    /// Redraws what changed since the last frame (nothing when nothing changed).
    fn redraw_overlay(&mut self) {
        let Some(ov) = self.overlay.as_mut() else {
            return;
        };
        let scene = ov.model.scene(&*ov.window);
        let dirty = overlay::dirty(&ov.scene, &scene);
        if !dirty.is_empty()
            && let Some(frozen) = self.captures.frozen()
        {
            ov.window.present(frozen, &scene, &dirty);
        }
        ov.window.set_cursor(ov.model.cursor());
        ov.scene = scene;
    }

    /// Closes the overlay without any output: the frozen picture is freed (NOEUL-FR-012).
    fn cancel_overlay(&mut self) -> bool {
        let Some(mut ov) = self.overlay.take() else {
            return false;
        };
        ov.window.destroy();
        self.captures.cancel();
        self.overlay_menu = None;
        self.save_request = None;
        self.last_action = Some(("capture.cancel".into(), "cancelled"));
        true
    }

    /// The open session's selection set to `rect`, as a [`CaptureSelection`].
    fn overlay_selection(&mut self, rect: Rect) -> Option<(u64, CaptureSelection)> {
        let id = self.overlay.as_ref()?.id;
        self.captures
            .select(id, rect)
            .and_then(|()| self.captures.selection(id))
            .ok()
            .map(|s| (id, s))
    }

    /// An output succeeded: the session ends (its frozen picture is freed), the overlay closes.
    fn finish_overlay(
        &mut self,
        id: u64,
        how: SessionOutcome,
        action: &'static str,
        result: &'static str,
    ) {
        let _ = self.captures.complete(id, how);
        if let Some(mut ov) = self.overlay.take() {
            ov.window.destroy();
        }
        self.overlay_menu = None;
        self.save_request = None;
        self.help_done();
        self.last_action = Some((action.into(), result));
    }

    /// An output failed: the overlay stays with its selection and says why (NOEUL-FR-025).
    fn fail_overlay(&mut self, action: &'static str, result: &'static str, banner: &str) {
        if let Some(ov) = self.overlay.as_mut() {
            ov.model.set_banner(Some(banner.to_string()));
        }
        self.redraw_overlay();
        self.last_action = Some((action.into(), result));
    }

    /// `capture.pin`, the double-click, the middle-click, the Pin button or the menu: the
    /// session's selection becomes a pin at the same place and size, with keyboard focus; the
    /// overlay closes. Nothing touches the clipboard or the disk (NOEUL-FR-020, NOEUL-SEC-005).
    /// After a mouse gesture the new pin ignores presses for one double-click time, so the rest
    /// of a triple or quadruple click cannot move, zoom or close it (UX spec §4.2 rule 10). If
    /// the pin cannot be created, the overlay stays with its selection and says so
    /// (NOEUL-FR-025).
    fn pin_from_overlay(&mut self, rect: Rect, via: Via) {
        let Some(ov) = self.overlay.as_ref() else {
            return;
        };
        let guard = via
            .pointer()
            .then(|| Duration::from_millis(u64::from(ov.model.system_values().double_click_ms)));
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.pin",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let opts = PinOptions { focus: true, guard };
        match self.pin_selection(sel, opts) {
            Ok(_) => self.finish_overlay(id, SessionOutcome::Pinned, "capture.pin", "pinned"),
            Err(_) => self.fail_overlay(
                "capture.pin",
                "failed",
                "The pin could not be created. Your selection is kept: try again, copy, save, or press Esc.",
            ),
        }
    }

    /// `capture.copy`, the Copy button, the menu (or the double-click set to `copy`): the
    /// selection goes to the clipboard as PNG and `CF_DIBV5`, then the overlay closes
    /// (NOEUL-FR-023, NOEUL-FR-030). Nothing is pinned or written to disk. A busy or failing
    /// clipboard keeps the overlay with a banner, so the user can retry, pin or save.
    fn copy_from_overlay(&mut self, rect: Rect) {
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.copy",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let encoded = output::png(sel.frame()).map(|png| (png, output::dib_v5(sel.frame())));
        drop(sel);
        let written = match encoded {
            Ok((png, dib)) => self.shell.clipboard_write(&png, &dib),
            Err(why) => Err(NoeulError::new(
                Code::Internal,
                format!("PNG encoding failed: {why}"),
            )),
        };
        match written {
            Ok(()) => self.finish_overlay(id, SessionOutcome::Copied, "capture.copy", "copied"),
            Err(e) if e.code == Code::ClipboardUnavailable => self.fail_overlay(
                "capture.copy",
                "clipboard_unavailable",
                "The clipboard is in use by another program; nothing was copied. Try again, pin, save, or press Esc.",
            ),
            Err(_) => self.fail_overlay(
                "capture.copy",
                "failed",
                "The picture could not be copied. Try again, pin, save, or press Esc.",
            ),
        }
    }

    /// The quick-save folder: `[capture] quick_save_folder`, else the Pictures folder + `Noeul`
    /// (never under the debug-build test hook), and whether the user set it.
    fn quick_save_folder(&self) -> (Option<PathBuf>, bool) {
        match &self.settings.settings.quick_save_folder {
            Some(f) => (Some(f.clone()), true),
            None => (self.shell.pictures_dir().map(|p| p.join("Noeul")), false),
        }
    }

    /// `capture.save`, the Save button, the menu (or the double-click set to `save`): asks the
    /// runtime for the save dialog. The overlay stays open behind it; nothing is written until
    /// the user chose a file ([`App::save_chosen`]).
    fn request_save(&mut self, rect: Rect) {
        let Some(id) = self.overlay.as_ref().map(|o| o.id) else {
            return;
        };
        if self.captures.select(id, rect).is_err() {
            return self.fail_overlay(
                "capture.save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        }
        let (folder, explicit) = self.quick_save_folder();
        self.save_request = Some(SaveRequest {
            session: id,
            rect,
            name: output::file_name(self.shell.local_time()),
            folder,
            explicit,
        });
        self.last_action = Some(("capture.save".into(), "asked"));
        self.redraw_overlay();
    }

    /// The save dialog the runtime should show now, if one was asked for.
    pub fn take_save_request(&mut self) -> Option<SaveRequest> {
        self.save_request.take()
    }

    /// The user's answer to the save dialog: `Some(path)` writes the selection there as PNG
    /// (the dialog already asked before replacing a file) and closes the overlay; `None`
    /// (cancelled) leaves the overlay as it was. An answer for a session that has ended since is
    /// ignored.
    pub fn save_chosen(&mut self, req: &SaveRequest, path: Option<PathBuf>) {
        if self.overlay.as_ref().map(|o| o.id) != Some(req.session) {
            return;
        }
        let Some(path) = path else {
            self.last_action = Some(("capture.save".into(), "cancelled"));
            return self.redraw_overlay();
        };
        let Some((id, sel)) = self.overlay_selection(req.rect) else {
            return self.fail_overlay(
                "capture.save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let written = output::png(sel.frame())
            .map_err(std::io::Error::other)
            .and_then(|bytes| output::write_file(&path, &bytes, true));
        drop(sel);
        match written {
            Ok(()) => {
                self.shell.notice(&format!("Saved to {}", path.display()));
                self.finish_overlay(id, SessionOutcome::Saved, "capture.save", "saved");
            }
            Err(_) => self.fail_overlay(
                "capture.save",
                "failed",
                "The picture could not be saved there. Try another folder, copy, pin, or press Esc.",
            ),
        }
    }

    /// `capture.quick-save` (or `Shift`+Save): a new PNG in the quick-save folder, never
    /// overwriting a file, then the overlay closes (NOEUL-FR-032). Without a folder (none set,
    /// none known) the overlay stays and says how to set one.
    fn quick_save(&mut self, rect: Rect) {
        let (Some(folder), _) = self.quick_save_folder() else {
            return self.fail_overlay(
                "capture.quick-save",
                "no_folder",
                "No quick-save folder: set one with `noeul config set capture.quick_save_folder <folder>`, or press Esc.",
            );
        };
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.quick-save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let name = output::file_name(self.shell.local_time());
        let written = output::png(sel.frame())
            .map_err(std::io::Error::other)
            .and_then(|bytes| output::quick_save(&folder, &name, &bytes));
        drop(sel);
        match written {
            Ok(path) => {
                self.shell.notice(&format!("Saved to {}", path.display()));
                self.finish_overlay(
                    id,
                    SessionOutcome::Saved,
                    "capture.quick-save",
                    "saved",
                );
            }
            Err(_) => self.fail_overlay(
                "capture.quick-save",
                "failed",
                "The picture could not be saved in the quick-save folder. Copy, pin, save elsewhere, or press Esc.",
            ),
        }
    }

    /// The selection's context menu (right-click inside, `Shift+F10`, the menu key): at the
    /// pointer, or at the selection's corner from the keyboard. Under the debug-build test hook
    /// it is recorded, never shown (a popup menu always lands on a real monitor).
    fn open_overlay_menu(&mut self, at: Option<(i32, i32)>) {
        let Some(sel) = self.overlay.as_ref().and_then(|o| o.model.selection()) else {
            return;
        };
        if !self.shell.interactive() {
            self.last_action = Some(("capture.menu".into(), "not_shown"));
            return;
        }
        let last = self.captures.last().is_some();
        let items = OVERLAY_MENU
            .iter()
            .map(|entry| match entry {
                None => PinMenuItem {
                    action: None,
                    label: String::new(),
                    enabled: false,
                },
                Some((action, text)) => PinMenuItem {
                    action: Some(action),
                    label: self
                        .hotkeys
                        .keys_for(action)
                        .or(match *action {
                            "capture.cancel" => Some("Escape"),
                            _ => None,
                        })
                        .map_or_else(|| (*text).to_string(), |k| format!("{text}\t{k}")),
                    enabled: *action != "selection.last" || last,
                },
            })
            .collect();
        self.overlay_menu = Some(OverlayMenu {
            at: at.unwrap_or((sel.x.saturating_add(8), sel.y.saturating_add(8))),
            items,
        });
        self.last_action = Some(("capture.menu".into(), "shown"));
    }

    /// The selection's context menu the runtime should show now, if one was asked for.
    pub fn take_overlay_menu(&mut self) -> Option<OverlayMenu> {
        self.overlay_menu.take()
    }

    /// The user chose `action` in the selection's context menu (the overlay may be gone).
    pub fn overlay_menu_choice(&mut self, action: &str) {
        if !OVERLAY_MENU.iter().flatten().any(|(a, _)| *a == action) {
            return;
        }
        let Some(a) = OverlayAction::from_id(action) else {
            return;
        };
        let outcome = match self.overlay.as_mut() {
            Some(ov) => ov.model.handle(OverlayInput::Action(a)),
            None => return,
        };
        self.overlay_outcome(match outcome {
            OverlayOutcome::Output(o, r, _) => OverlayOutcome::Output(o, r, Via::Menu),
            other => other,
        });
    }

    /// The pin engine's entry point for a confirmed capture selection: a pin of exactly that
    /// rectangle at the same place and pixel size, at 100 %, with no clipboard or file in between
    /// (NOEUL-FR-020, NOEUL-FR-025, NOEUL-SEC-005). `Enter` (`capture.pin`) uses it today; Step
    /// 51's double-click calls it with `guard` set to the system double-click time. Focus is
    /// taken only when `opts.focus` asks and the instance is interactive (never under the test
    /// hook). On failure the selection's copy is dropped and the reason returned; the capture
    /// session and its frozen picture are untouched, so the overlay can stay open.
    pub fn pin_selection(
        &mut self,
        sel: CaptureSelection,
        opts: PinOptions,
    ) -> std::result::Result<PinId, String> {
        let desktop = self.capture.desktop();
        let new = NewPin {
            frame: sel.into_frame(),
            source: Source::Capture,
            placement: Placement::Exact,
            focus: opts.focus && self.shell.interactive(),
            guard: opts.guard,
        };
        self.pins
            .create(self.shell.pin_windows(), &desktop, new)
            .map_err(|(frame, why)| {
                drop(frame);
                why
            })
    }

    /// Routes a pin window event.
    pub fn pin_event(&mut self, e: PinEvent) {
        match e {
            PinEvent::Key(id, chord, repeat) => {
                if let Some(a) = self.hotkeys.pin_action(&chord, repeat) {
                    self.action(&a, Some(id));
                } else if !repeat && is_escape(&chord) {
                    // The reserved navigation key always closes the focused pin when no pin
                    // binding uses it, even with shortcuts off: no trapped UI (NOEUL-SEC-013).
                    self.action("pin.close", Some(id));
                }
            }
            PinEvent::Moved(id, x, y) => {
                self.pins.moved(id, x, y);
            }
            PinEvent::Closed(id) => {
                let _ = self.pins.close(id);
            }
            PinEvent::Menu(id, at) => {
                if !self.pins.contains(id) {
                    return;
                }
                if self.shell.interactive() {
                    self.menu = Some(PinMenu {
                        pin: id,
                        at,
                        items: self.pin_menu_items(id),
                    });
                } else {
                    // Under the debug-build test hook a popup menu would open on the user's
                    // screen (Windows keeps menus on a monitor): it is recorded, not shown.
                    self.last_action = Some(("pin.menu".into(), "not_shown"));
                }
            }
            PinEvent::Wheel(id, delta, pointer, ctrl) => {
                // Ctrl+wheel is opacity (NOEUL-FR-062, N2): not in this build, so it does nothing.
                if ctrl || !self.pins.contains(id) {
                    return;
                }
                let acc = self.wheel.entry(id).or_insert(0);
                *acc += delta;
                let steps = *acc / WHEEL_DELTA;
                *acc %= WHEEL_DELTA;
                let action = if steps > 0 {
                    "pin.zoom-in"
                } else {
                    "pin.zoom-out"
                };
                for _ in 0..steps.unsigned_abs() {
                    // Around the pointer when it is over the pin (it is, for a real wheel).
                    let anchor = self.pins.zoom_anchor(id, pointer);
                    let r = self.zoom_step(id, action, anchor);
                    self.last_action = Some((action.to_string(), r));
                }
            }
            PinEvent::MiddleClick(id) => {
                let anchor = self.pins.zoom_anchor(id, self.shell.pointer());
                let r = self.zoom_step(id, "pin.zoom-reset", anchor);
                self.last_action = Some(("pin.zoom-reset".into(), r));
            }
        }
        self.wheel.retain(|id, _| self.pins.contains(*id));
    }

    /// The context menu of one pin, from the effective keys (a changed key reads as such).
    pub fn pin_menu_items(&self, id: PinId) -> Vec<PinMenuItem> {
        let info = self.pins.info(id);
        let zoom = info.map_or(zoom::FULL, |i| i.zoom);
        let (w, h) = info.map_or((1, 1), |i| i.image);
        MENU.iter()
            .map(|entry| match entry {
                None => PinMenuItem {
                    action: None,
                    label: String::new(),
                    enabled: false,
                },
                Some((action, text)) => {
                    let keys = self.hotkeys.keys_for(action).or(match *action {
                        // Escape closes a pin even without a binding (nav.cancel).
                        "pin.close" => Some("Escape"),
                        _ => None,
                    });
                    let enabled = match *action {
                        "pin.zoom-in" => zoom::zoom_in(w, h, zoom).is_some(),
                        "pin.zoom-out" => zoom::zoom_out(zoom).is_some(),
                        "pin.zoom-reset" => zoom != zoom::FULL,
                        _ => true,
                    };
                    PinMenuItem {
                        action: Some(action),
                        label: keys.map_or_else(|| (*text).to_string(), |k| format!("{text}\t{k}")),
                        enabled,
                    }
                }
            })
            .collect()
    }

    /// The pin menu the runtime should show now, if one was asked for.
    pub fn take_pin_menu(&mut self) -> Option<PinMenu> {
        self.menu.take()
    }

    /// The user chose `action` in the menu of `pin` (the pin may be gone by then).
    pub fn pin_menu_choice(&mut self, pin: PinId, action: &str) {
        if MENU.iter().flatten().any(|(a, _)| *a == action) {
            self.action(action, Some(pin));
        }
    }

    /// Monitors were attached, removed or rearranged, or a work area changed
    /// (`WM_DISPLAYCHANGE`, `WM_SETTINGCHANGE`). Pins keep their place, even off-screen
    /// (NOEUL-FR-067): the model only follows where the system put each window. When a pin is
    /// left where no monitor shows it, a notice names the way back (NOEUL-SEC-013).
    pub fn display_changed(&mut self) {
        self.pins.sync();
        let desktop = self.capture.desktop();
        if desktop.state != DesktopState::Available {
            return;
        }
        let off = self.pins.off_screen(&desktop);
        let before = std::mem::replace(&mut self.lost, off);
        if off > 0 && off != before {
            self.shell.notice(&format!(
                "{off} pin(s) are off-screen after the display change. Use Bring pins on screen in the tray, or `noeul pin show --all --on-screen`."
            ));
        }
        self.last_action = Some((
            "display.change".into(),
            if off > 0 { "off_screen" } else { "ok" },
        ));
    }

    /// The tray menu for the current state (items appear only once implemented, NOEUL-FR-004).
    pub fn tray_menu(&self) -> Vec<MenuItem> {
        menu(&crate::tray::MenuState {
            reopenable: self.pins.reopenable().is_some(),
            shortcuts_on: self.hotkeys.status_json()["enabled"] == true,
            shortcuts_switchable: !hotkeys_forced_off(&self.hotkeys),
            open: self.pins.len(),
            hidden: self.pins.hidden(),
            off_screen: self.pins.off_screen(&self.capture.desktop()),
        })
    }

    /// Hides or shows every pin, or brings the lost ones on screen (tray items).
    fn all_pins(&mut self, item: MenuItem) {
        let desktop = self.capture.desktop();
        for id in self.pins.ids() {
            match item {
                MenuItem::HideAllPins => {
                    let _ = self.pins.hide(id);
                }
                MenuItem::ShowAllPins => {
                    let _ = self.pins.show(id);
                }
                MenuItem::BringPinsOnScreen => {
                    let _ = self.pins.show(id);
                    let _ = self.pins.bring_on_screen(id, &desktop);
                }
                _ => {}
            }
        }
    }

    pub fn tray_click(&mut self, click: Click) {
        match click {
            Click::Middle => self.action("paste.float", None),
            // Left click starts a capture (NOEUL-FR-004).
            Click::Left => {
                let r = self.capture_region_action(Origin::Tray, Instant::now());
                self.last_action = Some(("capture.region".into(), r));
            }
            // The menu opens (the runtime shows it).
            Click::Right => {}
        }
    }

    pub fn tray_command(&mut self, item: MenuItem) {
        match item {
            MenuItem::CaptureRegion => {
                let r = self.capture_region_action(Origin::Tray, Instant::now());
                self.last_action = Some(("capture.region".into(), r));
            }
            MenuItem::PinClipboard => self.action("paste.float", None),
            MenuItem::HideAllPins | MenuItem::ShowAllPins | MenuItem::BringPinsOnScreen => {
                self.all_pins(item);
            }
            MenuItem::ReopenClosedPin => {
                let focus = self.shell.interactive();
                let _ = self.pins.reopen(self.shell.pin_windows(), focus);
            }
            MenuItem::Shortcuts { on } => {
                if let Err(e) = self.switch_shortcuts(!on) {
                    self.shell.notice(&e.message);
                }
            }
            MenuItem::Quit => {
                let open = self.pins.len();
                if open == 0 || self.shell.confirm_quit(open) {
                    self.shutdown();
                }
            }
        }
    }

    /// The tray's "Keyboard shortcuts on/off": the same switch as `noeul hotkeys disable --all`
    /// (§12.4), written to `hotkeys.json` and applied at once.
    fn switch_shortcuts(&mut self, on: bool) -> Result<()> {
        use kim_hotkeys::edit::{self, Switch};
        let (state, loaded) = crate::hotkeys::load(&self.paths);
        if loaded.state.safe_mode() {
            return Err(NoeulError::new(
                Code::HotkeysInvalid,
                "hotkeys.json is invalid or newer; fix or reset it first",
            ));
        }
        let switch = if on {
            Switch::EnableAll
        } else {
            Switch::DisableAll
        };
        let applied = edit::apply_switch(&[state], &[], 0, &switch);
        if applied.changed {
            let layer = applied
                .layer
                .unwrap_or_else(|| kim_hotkeys::UserLayer::new(crate::hotkeys::TOOL));
            kim_hotkeys::store::save(&loaded.path, &layer, false)
                .map_err(|e| NoeulError::io(loaded.path.display(), &e))?;
        }
        self.hotkeys.reload();
        Ok(())
    }

    /// A pressed global hotkey (from the registration thread's waker).
    pub fn hotkey_pressed(&mut self, id: kim_hotkeys::register::HotkeyId) {
        // The first-frame latency of the overlay counts from here (NOEUL-NFR-003).
        let trigger = Instant::now();
        // NOEUL-SEC-003: no action while the user's desktop is not the input desktop.
        if self.capture.desktop().state != DesktopState::Available {
            self.last_action = Some(("hotkey".into(), "desktop_unavailable"));
            return;
        }
        match self.hotkeys.action_for(id) {
            Some(a) if a == "capture.region" => {
                let r = self.capture_region_action(Origin::Hotkey, trigger);
                self.last_action = Some((a, r));
            }
            Some(a) => self.action(&a, None),
            None => {}
        }
    }
}

fn is_escape(c: &Chord) -> bool {
    crate::hotkeys::chord_of("Escape", false, false, false).is_some_and(|e| e == *c)
}

/// The toolbar navigation a key means (UX spec §5): `Tab`, `Shift+Tab`, the arrows, `Enter`,
/// `Space` and `Escape`, without other modifiers.
fn nav_of(c: &Chord) -> Option<Nav> {
    let is = |name: &str, shift: bool| {
        crate::hotkeys::chord_of(name, false, false, shift).is_some_and(|k| k == *c)
    };
    [
        ("Tab", false, Nav::Next),
        ("Tab", true, Nav::Prev),
        ("Left", false, Nav::Left),
        ("Up", false, Nav::Left),
        ("Right", false, Nav::Right),
        ("Down", false, Nav::Right),
        ("Enter", false, Nav::Activate),
        ("Space", false, Nav::Activate),
        ("Escape", false, Nav::Escape),
    ]
    .into_iter()
    .find(|(name, shift, _)| is(name, *shift))
    .map(|(_, _, n)| n)
}

fn hotkeys_forced_off(m: &Manager) -> bool {
    let s = m.status_json();
    matches!(
        s["off_reason"].as_str(),
        Some("--no-hotkeys" | "KIM_NO_HOTKEYS" | "safe_mode")
    )
}

/// Reads `{"x", "y"}` (device pixels).
fn point(v: &Value) -> Option<(i32, i32)> {
    let o = v.as_object()?;
    if o.len() != 2 {
        return None;
    }
    let c = |k: &str| {
        o.get(k)?
            .as_i64()
            .filter(|n| n.unsigned_abs() <= 1_000_000)
            .and_then(|n| i32::try_from(n).ok())
    };
    Some((c("x")?, c("y")?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capture::{CaptureError, Desktop, Monitor, Synthetic};
    use crate::overlay::{Approx, Button, Mods, color};
    use crate::pins::fake::FakeWindows;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What the fake overlay windows saw, shared with the test.
    #[derive(Debug, Default)]
    pub struct OverlayLog {
        pub opened: Vec<Rect>,
        pub presents: usize,
        pub dirty_rects: usize,
        pub shown: Vec<bool>,
        pub cursor: Option<Cursor>,
        pub alive: usize,
        pub destroyed: usize,
        /// The back buffer of the last window, composed exactly as the Win32 window does.
        pub buffer: Vec<u8>,
        pub scene: Option<Scene>,
    }

    #[derive(Debug, Default, Clone)]
    pub struct FakeOverlays {
        pub log: Rc<RefCell<OverlayLog>>,
        pub fail_open: bool,
        pub fail_show: bool,
    }

    struct FakeOverlay {
        log: Rc<RefCell<OverlayLog>>,
        picture: Rect,
        fail_show: bool,
        destroyed: bool,
    }

    impl Measure for FakeOverlay {
        fn text(&self, font: overlay::Font, dpi: u32, text: &str) -> (u32, u32) {
            Approx.text(font, dpi, text)
        }
    }

    impl OverlayWindow for FakeOverlay {
        fn present(&mut self, frozen: &Frame, scene: &Scene, dirty: &[Rect]) {
            let mut l = self.log.borrow_mut();
            if l.buffer.len() != frozen.pixels().len() {
                l.buffer = vec![0; frozen.pixels().len()];
            }
            for r in dirty {
                overlay::compose(frozen, scene, *r, &mut l.buffer);
            }
            l.presents += 1;
            l.dirty_rects += dirty.len();
            l.scene = Some(scene.clone());
        }
        fn show(&mut self, activate: bool) -> bool {
            if self.fail_show {
                return false;
            }
            self.log.borrow_mut().shown.push(activate);
            true
        }
        fn set_cursor(&mut self, cursor: Cursor) {
            self.log.borrow_mut().cursor = Some(cursor);
        }
        fn buffer_bytes(&self) -> u64 {
            self.picture.bgra_bytes()
        }
        fn destroy(&mut self) {
            if !self.destroyed {
                self.destroyed = true;
                let mut l = self.log.borrow_mut();
                l.alive -= 1;
                l.destroyed += 1;
                l.buffer = Vec::new();
            }
        }
    }

    impl Drop for FakeOverlay {
        fn drop(&mut self) {
            self.destroy();
        }
    }

    /// A desktop of one 100×100 monitor that captures solid frames.
    pub struct FakeCapture {
        pub state: DesktopState,
    }

    impl CaptureBackend for FakeCapture {
        fn name(&self) -> &'static str {
            "gdi" // stands in for the real backend (the schema knows only real names)
        }
        fn desktop(&self) -> Desktop {
            Desktop {
                state: self.state,
                virtual_rect: Some(Rect::new(0, 0, 100, 100)),
                monitors: vec![Monitor::new(Rect::new(0, 0, 100, 100), true)],
            }
        }
        fn capture(
            &self,
            ledger: &Arc<Ledger>,
            r: Rect,
        ) -> std::result::Result<Frame, CaptureError> {
            check_region(&self.desktop(), r)?;
            Ok(Frame::solid(ledger, r, [10, 20, 30, 255])?)
        }
    }

    #[derive(Default)]
    pub struct FakeShell {
        pub windows: FakeWindows,
        pub clipboard: Option<(u32, u32)>,
        pub clipboard_reads: usize,
        pub notices: Vec<String>,
        pub confirm: bool,
        pub overlays: FakeOverlays,
        pub pointer_at: Option<(i32, i32)>,
        /// `Some(false)` stands for the debug-build test hook.
        pub interactive: Option<bool>,
        /// What Copy put on the (fake) clipboard: `(png, dib)` per write.
        pub copies: Vec<(Vec<u8>, Vec<u8>)>,
        /// Every clipboard write attempt, also the refused ones.
        pub clipboard_writes: usize,
        /// The fake clipboard is held by another program (or disabled).
        pub clipboard_busy: bool,
        /// The Pictures folder (quick save's default parent).
        pub pictures: Option<PathBuf>,
        /// The user's pointer settings (Windows defaults when `None`).
        pub sys: Option<SystemValues>,
    }

    impl Shell for FakeShell {
        fn pin_windows(&mut self) -> &mut dyn PinWindows {
            &mut self.windows
        }
        fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Frame> {
            self.clipboard_reads += 1;
            match self.clipboard {
                Some((w, h)) => {
                    Ok(Frame::solid(ledger, Rect::new(0, 0, w, h), [1, 1, 1, 255]).unwrap())
                }
                None => Err(NoeulError::new(Code::NothingToPin, "no image")),
            }
        }
        fn clipboard_write(&mut self, png: &[u8], dib: &[u8]) -> Result<()> {
            self.clipboard_writes += 1;
            if self.clipboard_busy {
                return Err(NoeulError::new(Code::ClipboardUnavailable, "held (test)"));
            }
            self.copies.push((png.to_vec(), dib.to_vec()));
            Ok(())
        }
        fn pictures_dir(&self) -> Option<PathBuf> {
            self.pictures.clone()
        }
        fn local_time(&self) -> [u16; 6] {
            [2026, 11, 4, 9, 5, 7]
        }
        fn pointer(&self) -> (i32, i32) {
            self.pointer_at.unwrap_or((90, 95))
        }
        fn notice(&mut self, text: &str) {
            self.notices.push(text.to_string());
        }
        fn tray_shown(&self) -> bool {
            false
        }
        fn confirm_quit(&mut self, _: usize) -> bool {
            self.confirm
        }
        fn open_overlay(
            &mut self,
            picture: Rect,
        ) -> std::result::Result<Box<dyn OverlayWindow>, String> {
            if self.overlays.fail_open {
                return Err("window creation failed (test)".into());
            }
            let mut l = self.overlays.log.borrow_mut();
            l.opened.push(picture);
            l.alive += 1;
            Ok(Box::new(FakeOverlay {
                log: Rc::clone(&self.overlays.log),
                picture,
                fail_show: self.overlays.fail_show,
                destroyed: false,
            }))
        }
        fn system_values(&self) -> SystemValues {
            self.sys.unwrap_or_default()
        }
        fn interactive(&self) -> bool {
            self.interactive.unwrap_or(true)
        }
    }

    pub fn app(dir: &std::path::Path, state: DesktopState) -> App<FakeShell> {
        let paths = Paths::under(dir);
        let hotkeys = Manager::start(&paths, Some("--no-hotkeys"), || unreachable!());
        App::new(
            FakeShell::default(),
            Box::new(FakeCapture { state }),
            hotkeys,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        )
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noeul-app-{tag}-{}", std::process::id()))
    }

    /// Two monitors: 640×360 at 100 % left of the origin, and the primary 960×540 at 150 %.
    pub fn layout() -> Vec<Monitor> {
        vec![
            Monitor::new(Rect::new(-640, 0, 640, 360), false),
            Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(144),
        ]
    }

    /// An instance on the synthetic two-monitor desktop with the default keys (no registrar).
    fn overlay_app(tag: &str) -> (App<FakeShell>, Arc<Synthetic>, PathBuf) {
        let dir = tmp(tag);
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::under(&dir);
        let syn = Arc::new(Synthetic::new(layout()));
        let hotkeys = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        let shell = FakeShell {
            pointer_at: Some((500, 300)),
            ..FakeShell::default()
        };
        let a = App::new(
            shell,
            Box::new(Arc::clone(&syn)),
            hotkeys,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        );
        (a, syn, dir)
    }

    fn ev(a: &mut App<FakeShell>, i: OverlayInput) {
        a.overlay_event(OverlayEvent::Input(i));
    }

    fn key(a: &mut App<FakeShell>, name: &str, ctrl: bool, shift: bool) {
        let c = crate::hotkeys::chord_of(name, ctrl, false, shift).unwrap();
        a.overlay_event(OverlayEvent::Key(c, false));
    }

    fn drag(a: &mut App<FakeShell>, from: (i32, i32), to: (i32, i32)) {
        ev(
            a,
            OverlayInput::Press {
                button: Button::Left,
                x: from.0,
                y: from.1,
                mods: Mods::NONE,
                time_ms: 0,
            },
        );
        for (x, y) in [((from.0 + to.0) / 2, (from.1 + to.1) / 2), to] {
            ev(
                a,
                OverlayInput::Move {
                    x,
                    y,
                    mods: Mods::NONE,
                },
            );
        }
        ev(
            a,
            OverlayInput::Release {
                button: Button::Left,
                x: to.0,
                y: to.1,
            },
        );
    }

    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.extend(files_under(&e.path()));
                } else {
                    out.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out
    }

    #[test]
    fn f1_opens_one_overlay_over_every_monitor_and_esc_cancels_cleanly() {
        let (mut a, syn, _dir) = overlay_app("ov-open");
        a.action("capture.region", None);
        let picture = Rect::new(-640, 0, 1600, 540);
        {
            let l = a.shell.overlays.log.borrow();
            assert_eq!(l.opened, [picture], "one overlay over the virtual desktop");
            assert_eq!(l.shown, [true], "on top, with keyboard focus");
            assert_eq!(l.cursor, Some(Cursor::Cross));
            // The first frame is the whole frozen picture, dimmed (nothing selected yet).
            let i = (10 * 1600 + 10) * 4;
            assert_eq!(
                &l.buffer[i..i + 4],
                &overlay::dim_pixel(Synthetic::pixel(-630, 10))
            );
        }
        assert_eq!(syn.captures(), 1);
        assert_eq!(a.ledger.live(), (1, picture.bgra_bytes()));
        let st = a.status();
        assert_eq!(st["captures"]["open"], true);
        assert_eq!(st["captures"]["overlay"]["phase"], "hover");
        assert_eq!(st["captures"]["overlay"]["monitors"], 2);
        assert_eq!(st["captures"]["first_frame"]["count"], 1);
        assert_eq!(st["last_action"]["result"], "opened");
        // Rapid repeated presses: ignored, no second overlay, no second capture.
        for _ in 0..5 {
            a.action("capture.region", None);
        }
        assert_eq!(a.shell.overlays.log.borrow().opened.len(), 1);
        assert_eq!(syn.captures(), 1, "the screen is not captured again");
        assert_eq!(a.status()["captures"]["ignored"], 5);
        assert_eq!(a.status()["last_action"]["result"], "ignored");
        // The selection tracks the pointer across the monitor seam.
        drag(&mut a, (-100, 50), (199, 249));
        let st = a.status();
        assert_eq!(st["captures"]["overlay"]["phase"], "selected");
        assert_eq!(
            st["captures"]["overlay"]["selection"],
            json!({"x": -100, "y": 50, "width": 300, "height": 200})
        );
        {
            let l = a.shell.overlays.log.borrow();
            let at = |x: i32, y: i32| {
                let i = ((y as usize) * 1600 + (x + 640) as usize) * 4;
                [
                    l.buffer[i],
                    l.buffer[i + 1],
                    l.buffer[i + 2],
                    l.buffer[i + 3],
                ]
            };
            assert_eq!(at(0, 150), Synthetic::pixel(0, 150), "plain inside");
            assert_eq!(at(-101, 150), color::ACCENT, "border outside the left edge");
            assert_eq!(
                at(-600, 300),
                overlay::dim_pixel(Synthetic::pixel(-600, 300))
            );
            assert!(l.dirty_rects > 0 && l.presents >= 4);
        }
        // Escape cancels: the window and the frozen picture are gone, nothing was pinned.
        key(&mut a, "Escape", false, false);
        assert!(!a.overlay_open());
        let l = a.shell.overlays.log.borrow();
        assert_eq!((l.alive, l.destroyed), (0, 1));
        drop(l);
        assert_eq!(a.ledger.live(), (0, 0), "nothing is kept");
        assert!(a.pins.is_empty());
        assert_eq!(a.status()["captures"]["cancelled"], 1);
        assert_eq!(a.shell.clipboard_reads, 0, "the clipboard is never touched");
    }

    #[test]
    fn enter_pins_exactly_the_selection_and_nothing_is_copied_or_written() {
        let (mut a, syn, dir) = overlay_app("ov-pin");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (499, 399));
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open(), "the overlay closed");
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(100, 100, 400, 300), true)],
            "same place and size, with focus"
        );
        assert_eq!(
            a.ledger.live(),
            (1, 400 * 300 * 4),
            "only the pin's pixels are kept"
        );
        assert_eq!(syn.captures(), 1, "the pin shows the frozen picture");
        assert_eq!(a.shell.clipboard_reads, 0);
        let st = a.status();
        assert_eq!(st["captures"]["pinned"], 1);
        assert_eq!(st["last_action"]["action"], "capture.pin");
        // Nothing reached the disk except the "help seen" flag.
        assert_eq!(files_under(&dir), ["ui-state.json"]);
        // R brings the last rectangle back in the next capture.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 100, "y": 100, "width": 400, "height": 300})
        );
        assert!(
            a.shell
                .overlays
                .log
                .borrow()
                .scene
                .as_ref()
                .unwrap()
                .help
                .is_none(),
            "the first-run help is gone after the first capture"
        );
        a.shutdown();
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(a.shell.overlays.log.borrow().alive, 0);
    }

    #[test]
    fn keyboard_only_capture_with_ctrl_a_arrows_and_enter() {
        let (mut a, _syn, _dir) = overlay_app("ov-keys");
        a.action("capture.region", None);
        key(&mut a, "Enter", false, false);
        assert!(a.overlay_open(), "Enter without a selection pins nothing");
        key(&mut a, "A", true, false); // the monitor under the pointer (500,300): the primary
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 0, "y": 0, "width": 960, "height": 540})
        );
        key(&mut a, "Right", false, true); // shrink from the left edge
        key(&mut a, "Up", false, true); // shrink from the bottom edge
        key(&mut a, "Left", false, false); // move left (stays on the picture)
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 0, "y": 0, "width": 959, "height": 539})
        );
        key(&mut a, "Left", true, false); // grow left across the seam
        key(&mut a, "Enter", false, false);
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(created, [(PinId(1), Rect::new(-1, 0, 960, 539), true)]);
    }

    #[test]
    fn a_failed_pin_keeps_the_overlay_and_its_selection() {
        let (mut a, _syn, _dir) = overlay_app("ov-fail");
        a.action("capture.region", None);
        drag(&mut a, (10, 10), (109, 59));
        *a.shell.windows.fail.borrow_mut() = true;
        key(&mut a, "Enter", false, false);
        assert!(a.overlay_open(), "the overlay stays (NOEUL-FR-025)");
        let st = a.status();
        assert_eq!(st["captures"]["overlay"]["phase"], "selected");
        assert_eq!(st["captures"]["overlay"]["selection"]["width"], 100);
        assert_eq!(st["last_action"]["result"], "failed");
        let banner = a.shell.overlays.log.borrow().scene.clone().unwrap().help;
        assert!(banner.unwrap().lines[0].starts_with("The pin could not be created"));
        assert_eq!(
            a.ledger.live().0,
            1,
            "only the frozen picture: the failed pin's copy was freed"
        );
        *a.shell.windows.fail.borrow_mut() = false;
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.pins.len(), 1);
    }

    #[test]
    fn ipc_and_tray_open_the_overlay_and_a_dry_run_captures_nothing() {
        let (mut a, syn, _dir) = overlay_app("ov-ipc");
        let dry = a
            .request("capture.region", &json!({"dry_run": true}))
            .unwrap();
        assert_eq!(
            (dry["opened"].as_bool(), dry["dry_run"].as_bool()),
            (Some(false), Some(true))
        );
        assert_eq!(dry["picture"]["width"], 1600);
        assert_eq!(syn.captures(), 0, "a dry run captures nothing");
        assert!(a.shell.overlays.log.borrow().opened.is_empty());
        let r = a.request("capture.region", &json!({})).unwrap();
        assert_eq!(r["opened"], true);
        assert!(r["first_frame_us"].as_u64().is_some());
        let again = a.request("capture.region", &json!({})).unwrap();
        assert_eq!(
            (again["opened"].as_bool(), again["reason"].as_str()),
            (Some(false), Some("already_open"))
        );
        let c = a
            .request("capture.cancel", &json!({"dry_run": true}))
            .unwrap();
        assert_eq!(c["cancelled"], false);
        assert!(a.overlay_open(), "a dry run cancels nothing");
        let c = a.request("capture.cancel", &json!({})).unwrap();
        assert_eq!(c["cancelled"], true);
        assert!(!a.overlay_open());
        let c = a.request("capture.cancel", &json!({})).unwrap();
        assert_eq!(
            (c["cancelled"].as_bool(), c["session"].clone()),
            (Some(false), Value::Null)
        );
        // The tray's left click and its menu item start a capture too.
        a.tray_click(Click::Left);
        assert!(a.overlay_open());
        assert!(a.tray_menu().contains(&MenuItem::CaptureRegion));
        a.request("capture.cancel", &json!({})).unwrap();
        a.tray_command(MenuItem::CaptureRegion);
        assert!(a.overlay_open());
        // Focus moving to another program cancels.
        ev(&mut a, OverlayInput::FocusLost);
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(syn.captures(), 3);
        assert_eq!(a.latency().to_json()["count"], 3);
    }

    #[test]
    fn locked_desktops_and_failing_windows_open_no_overlay() {
        let (mut a, syn, _dir) = overlay_app("ov-locked");
        syn.set_state(DesktopState::Locked);
        a.action("capture.region", None);
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "desktop_unavailable");
        let e = a.request("capture.region", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::DesktopUnavailable);
        assert_eq!(syn.captures(), 0, "a locked desktop is never captured");
        assert!(
            a.shell.notices.is_empty(),
            "a locked desktop is not an error to show"
        );
        syn.set_state(DesktopState::Available);
        a.shell.overlays.fail_open = true;
        let e = a.request("capture.region", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::Internal);
        assert_eq!(a.ledger.live(), (0, 0), "the frozen picture was discarded");
        a.shell.overlays.fail_open = false;
        a.shell.overlays.fail_show = true;
        a.action("capture.region", None);
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(a.shell.notices, ["Noeul could not capture the screen."]);
        assert_eq!(a.status()["captures"]["open"], false);
    }

    #[test]
    fn the_test_hook_never_takes_focus_and_the_help_follows_the_setting() {
        let (mut a, _syn, dir) = overlay_app("ov-hook");
        a.shell.interactive = Some(false);
        a.action("capture.region", None);
        assert_eq!(
            a.shell.overlays.log.borrow().shown,
            [false],
            "never activated"
        );
        let help = a
            .shell
            .overlays
            .log
            .borrow()
            .scene
            .clone()
            .unwrap()
            .help
            .unwrap();
        assert_eq!(
            help.lines,
            [
                "Drag to select a region. Esc or right-click cancels.",
                "Double-click inside pins it \u{b7} Ctrl+C copies \u{b7} Ctrl+S saves \u{b7} Ctrl+Shift+S quick-saves",
                "Arrows move \u{b7} Ctrl+arrows grow \u{b7} Shift+arrows shrink",
                "Ctrl+A whole monitor \u{b7} R last area \u{b7} Enter pins the selection",
            ]
        );
        assert_eq!(help.dpi, 144, "drawn at the primary monitor's 150 %");
        drag(&mut a, (10, 10), (19, 19));
        key(&mut a, "Enter", false, false);
        let created = a.shell.windows.log.borrow().created.clone();
        assert!(!created[0].2, "a pin from the hook never takes focus");
        // `always` shows the help again; `never` never.
        std::fs::create_dir_all(dir.join("Roaming").join("mouv").join("noeul")).unwrap();
        for (value, shown) in [("always", true), ("never", false), ("first-run", false)] {
            std::fs::write(
                a.paths.config_file(),
                format!("[capture]\nkeyboard_help = \"{value}\"\n"),
            )
            .unwrap();
            a.settings = settings::load(&a.paths.config_file());
            a.action("capture.region", None);
            let has = a
                .shell
                .overlays
                .log
                .borrow()
                .scene
                .clone()
                .unwrap()
                .help
                .is_some();
            assert_eq!(has, shown, "{value}");
            a.request("capture.cancel", &json!({})).unwrap();
        }
    }

    #[test]
    fn the_help_names_changed_keys_and_disappears_without_them() {
        let dir = tmp("ov-help");
        let paths = Paths::under(&dir);
        let off = Manager::start(&paths, Some("--no-hotkeys"), || unreachable!());
        assert_eq!(
            overlay_help(&off, Gestures::default()),
            [
                "Drag to select a region. Esc or right-click cancels.",
                "Double-click inside pins it"
            ],
            "without shortcuts only the mouse and Escape remain"
        );
        let copy = Gestures {
            double_click: GestureAction::Copy,
            ..Gestures::default()
        };
        assert_eq!(
            overlay_help(&off, copy)[1],
            "Double-click inside copies it",
            "the help names the gesture setting"
        );
        let none = Gestures {
            double_click: GestureAction::None,
            ..Gestures::default()
        };
        assert_eq!(overlay_help(&off, none).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.hotkeys_file(),
            serde_json::to_vec(
                &json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
                "overrides": [
                    {"action": "capture.pin", "bindings": ["Ctrl+Enter"]},
                    {"action": "selection.move-up", "bindings": ["code:KeyW"]},
                    {"action": "selection.last", "disabled": true}
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        let m = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        let lines = overlay_help(&m, Gestures::default());
        assert_eq!(
            lines[2],
            "code:KeyW/Down/Left/Right move \u{b7} Ctrl+arrows grow \u{b7} Shift+arrows shrink"
        );
        assert_eq!(
            lines[3],
            "Ctrl+A whole monitor \u{b7} Ctrl+Enter pins the selection"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real trigger path on an obscure chord: `kim-hotkeys` registers `capture.region` with
    /// `RegisterHotKey` on `Ctrl+Alt+Shift+F21`, and that registration's id (what a press
    /// delivers to the instance) opens the overlay. No key is pressed or synthesized, and F1 is
    /// never registered.
    #[cfg(windows)]
    #[test]
    fn the_registered_capture_chord_opens_the_overlay() {
        use kim_hotkeys::register::{HotkeyId, Registrar, Status};
        let dir = tmp("ov-hotkey");
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::under(&dir);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.hotkeys_file(),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.region", "bindings": ["Ctrl+Alt+Shift+F21"]},
                    {"action": "paste.float", "disabled": true}]}))
            .unwrap(),
        )
        .unwrap();
        let mgr = Manager::start(&paths, None, Registrar::start);
        let regs = mgr.registrations().to_vec();
        assert_eq!(regs.len(), 1, "{regs:?}");
        assert_eq!(
            (
                regs[0].action.as_str(),
                regs[0].keys.as_str(),
                regs[0].status
            ),
            ("capture.region", "Ctrl+Alt+Shift+F21", "registered"),
            "{:?}",
            regs[0].message
        );
        let id = (1..=16)
            .map(HotkeyId)
            .find(|id| mgr.action_for(*id).as_deref() == Some("capture.region"))
            .expect("the registration's id");
        let syn = Arc::new(Synthetic::new(layout()));
        let shell = FakeShell {
            pointer_at: Some((10, 10)),
            ..FakeShell::default()
        };
        let mut a = App::new(
            shell,
            Box::new(Arc::clone(&syn)),
            mgr,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        );
        a.hotkey_pressed(id);
        assert!(a.overlay_open(), "the press opened the overlay");
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "capture.region", "result": "opened"})
        );
        assert_eq!(a.latency().to_json()["count"], 1);
        a.hotkey_pressed(id);
        assert_eq!(syn.captures(), 1, "a second press while open does nothing");
        assert_eq!(a.status()["captures"]["ignored"], 1);
        a.shutdown();
        let probe = Registrar::start().unwrap();
        let (pid, st) = probe
            .register(&Chord::parse("Ctrl+Alt+Shift+F21").unwrap())
            .unwrap();
        assert_eq!(st, Status::Registered, "shutdown released the chord");
        probe.unregister(pid.unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latency_percentiles_use_the_nearest_rank() {
        let mut l = Latency::default();
        assert_eq!(l.to_json()["p95_us"], Value::Null);
        for t in 1..=20u64 {
            l.push(LatencySample {
                total_us: t * 1000,
                capture_us: t * 600,
                render_us: t * 400,
                width: 1,
                height: 1,
            });
        }
        assert_eq!(l.percentile(50), Some(10_000));
        assert_eq!(l.percentile(95), Some(19_000));
        for _ in 0..200 {
            l.push(LatencySample {
                total_us: 5,
                capture_us: 1,
                render_us: 4,
                width: 1,
                height: 1,
            });
        }
        assert_eq!(l.percentile(95), Some(5), "only the last 100 count");
        assert_eq!(l.to_json()["count"], 220);
    }

    #[test]
    fn cli_capture_pins_and_every_picture_is_released() {
        let mut a = app(&tmp("capture"), DesktopState::Available);
        let region = json!({"x": 10, "y": 10, "width": 20, "height": 5});
        let dry = a
            .request(
                "capture",
                &json!({"region": region, "pin": true, "dry_run": true}),
            )
            .unwrap();
        assert_eq!(dry["dry_run"], true);
        assert!(
            a.shell.windows.log.borrow().created.is_empty(),
            "a dry run creates nothing"
        );
        assert_eq!(a.ledger.live(), (0, 0), "and captures nothing");

        let r = a
            .request("capture", &json!({"region": region, "pin": true}))
            .unwrap();
        assert_eq!(r["pin"]["id"], "pin-1");
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(10, 10, 20, 5), false)],
            "no focus stealing"
        );
        assert_eq!(a.ledger.live(), (1, 400), "only the pin's picture is alive");
        assert_eq!(a.status()["captures"]["pinned"], 1);

        let outside = a.request(
            "capture",
            &json!({"region": {"x": 90, "y": 0, "width": 20, "height": 5}, "pin": true}),
        );
        assert_eq!(outside.unwrap_err().code, Code::Usage);
        assert!(
            a.request("capture", &json!({"region": region})).is_err(),
            "pin is required"
        );

        let listed = a.request("pin.list", &json!({})).unwrap();
        assert_eq!(listed["pins"][0]["rect"]["width"], 20);
        assert_eq!(
            a.request("pin.close", &json!({"id": "pin-9"}))
                .unwrap_err()
                .code,
            Code::PinNotFound
        );
        let closed = a.request("pin.close", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(closed["closed"][0], "pin-1");
        assert_eq!(a.request("pin.reopen", &json!({})).unwrap()["pin"], "pin-1");

        let q = a.request("quit", &json!({}));
        assert_eq!(
            q.unwrap_err().code,
            Code::ConfirmationRequired,
            "open pins need --yes"
        );
        assert!(!a.quitting());
        let q = a
            .request("quit", &json!({"force": true, "dry_run": true}))
            .unwrap();
        assert_eq!(q["closed_pins"], 1);
        assert!(!a.quitting(), "a dry run does not quit");
        a.request("quit", &json!({"force": true})).unwrap();
        assert!(a.quitting());
        assert!(a.shell.windows.log.borrow().alive.is_empty());
        assert_eq!(a.ledger.live(), (0, 0), "quit releases every picture");
    }

    #[test]
    fn locked_desktops_capture_nothing() {
        for s in [
            DesktopState::Locked,
            DesktopState::NonInteractive,
            DesktopState::NoDisplay,
        ] {
            let mut a = app(&tmp("locked"), s);
            let e = a
                .request(
                    "capture",
                    &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
                )
                .unwrap_err();
            assert_eq!(e.code, Code::DesktopUnavailable, "{s:?}");
            a.shell.clipboard = Some((2, 2));
            a.action("paste.float", None);
            assert!(
                a.shell.windows.log.borrow().created.is_empty(),
                "{s:?}: no pin"
            );
            assert_eq!(a.ledger.live(), (0, 0));
        }
        let mut a = app(&tmp("unsupported"), DesktopState::Unsupported);
        let e = a
            .request(
                "capture",
                &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
            )
            .unwrap_err();
        assert_eq!(e.code, Code::UnsupportedPlatform);
    }

    #[test]
    fn paste_float_pins_at_the_pointer_or_says_nothing_to_pin() {
        let mut a = app(&tmp("paste"), DesktopState::Available);
        a.action("paste.float", None);
        assert_eq!(a.shell.notices.len(), 1);
        assert!(a.shell.notices[0].starts_with("Nothing to pin"));
        assert!(a.pins.is_empty());
        a.shell.clipboard = Some((30, 20));
        a.action("paste.float", None);
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(70, 80, 30, 20), true)],
            "kept on the desktop, focused"
        );
        // Pin keys: Escape closes the focused pin, arrows move it.
        let key = |n: &str| crate::hotkeys::chord_of(n, false, false, false).unwrap();
        let mut b = app(&tmp("paste-keys"), DesktopState::Available);
        let paths = Paths::under(tmp("paste-keys"));
        b.hotkeys = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        b.shell.clipboard = Some((2, 2));
        b.action("paste.float", None);
        b.pin_event(PinEvent::Key(PinId(1), key("Left"), true));
        assert_eq!(b.pins.rect(PinId(1)).unwrap().x, 89);
        b.pin_event(PinEvent::Moved(PinId(1), 5, 6));
        assert_eq!(b.pins.rect(PinId(1)).unwrap().y, 6);
        b.pin_event(PinEvent::Key(PinId(1), key("Escape"), true));
        assert_eq!(
            b.pins.len(),
            1,
            "a held Escape does not close a new pin by repeat"
        );
        b.pin_event(PinEvent::Key(PinId(1), key("Escape"), false));
        assert!(b.pins.is_empty());
        assert_eq!(b.pins.reopenable(), Some(PinId(1)));
        let s = b.status();
        assert_eq!(
            s["hotkeys"]["registrations"][0]["status"], "failed",
            "a registrar failure is reported"
        );
    }

    #[test]
    fn tray_items_follow_the_state_and_quit_asks_once() {
        let mut a = app(&tmp("tray"), DesktopState::Available);
        let items = a.tray_menu();
        assert!(items.contains(&MenuItem::PinClipboard));
        assert!(!items.contains(&MenuItem::ReopenClosedPin));
        assert!(items.contains(&MenuItem::Quit));
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, MenuItem::Shortcuts { .. })),
            "--no-hotkeys cannot be switched from the tray"
        );
        a.shell.clipboard = Some((1, 1));
        a.tray_click(Click::Middle);
        assert_eq!(a.pins.len(), 1, "middle click pins the clipboard");
        a.action("pin.close", Some(PinId(1)));
        assert!(a.tray_menu().contains(&MenuItem::ReopenClosedPin));
        a.tray_command(MenuItem::ReopenClosedPin);
        assert_eq!(a.pins.len(), 1);
        a.tray_command(MenuItem::Quit);
        assert!(!a.quitting(), "declined");
        a.shell.confirm = true;
        a.tray_command(MenuItem::Quit);
        assert!(a.quitting());
        assert_eq!(a.ledger.live(), (0, 0));
    }

    #[test]
    fn every_ipc_answer_matches_the_schema() {
        let mut a = app(&tmp("schema"), DesktopState::Available);
        a.shell.clipboard = Some((20, 10));
        let check = |v: Value| {
            let errors = crate::schema_errors("noeul-ipc-v1", &v);
            assert!(errors.is_empty(), "{errors:?}\n{v:#}");
        };
        let region = json!({"x": 1, "y": 2, "width": 3, "height": 4});
        for (method, params) in [
            ("status", json!({})),
            ("activate", json!({"reason": "start"})),
            (
                "capture",
                json!({"region": region, "pin": true, "dry_run": true}),
            ),
            ("capture", json!({"region": region, "pin": true})),
            ("pin.list", json!({})),
            (
                "pin.create",
                json!({"source": "region", "region": region, "dry_run": true}),
            ),
            ("pin.create", json!({"source": "region", "region": region})),
            (
                "pin.create",
                json!({"source": "clipboard", "dry_run": true}),
            ),
            (
                "pin.create",
                json!({"source": "clipboard", "at": {"x": 5, "y": 5}}),
            ),
            ("pin.hide", json!({"id": "pin-2", "dry_run": true})),
            ("pin.hide", json!({"all": true})),
            ("pin.list", json!({})),
            (
                "pin.show",
                json!({"all": true, "on_screen": true, "dry_run": true}),
            ),
            ("pin.show", json!({"id": "pin-3", "on_screen": true})),
            ("status", json!({})),
            ("pin.close", json!({"all": true, "dry_run": true})),
            ("pin.close", json!({"id": "pin-1"})),
            ("pin.reopen", json!({"dry_run": true})),
            ("pin.reopen", json!({})),
            ("hotkeys.reload", json!({})),
            ("status", json!({})),
            ("capture.region", json!({"dry_run": true})),
            ("capture.region", json!({})),
            ("status", json!({})),
            ("capture.region", json!({})),
            ("capture.cancel", json!({"dry_run": true})),
            ("capture.cancel", json!({})),
            ("quit", json!({"force": true, "dry_run": true})),
        ] {
            check(ipc::request(7, method, params.clone()));
            let r = a.request(method, &params).unwrap();
            check(ipc::ok(7, r));
        }
        for (method, params) in [
            ("pin.close", json!({"id": "pin-9"})),
            ("pin.hide", json!({})),
            ("pin.show", json!({"id": "pin-77"})),
            ("pin.create", json!({"source": "file"})),
            ("pin.create", json!({"source": "clipboard", "at": {"x": 1}})),
            ("quit", json!({})),
            (
                "capture",
                json!({"region": {"x": 99, "y": 99, "width": 9, "height": 9}, "pin": true}),
            ),
        ] {
            let e = a.request(method, &params).unwrap_err();
            check(ipc::err(8, &e));
            check(ipc::connection_error(&e));
        }
        check(ipc::hello("noeul-cli"));
        check(ipc::welcome(1, 42, &a.started_at()));
        let mut locked = app(&tmp("schema-locked"), DesktopState::Locked);
        let e = locked
            .request("capture", &json!({"region": region, "pin": true}))
            .unwrap_err();
        check(ipc::err(2, &e));
        check(ipc::ok(1, locked.status()));
    }

    #[test]
    fn status_reports_ids_and_geometry_only() {
        let mut a = app(&tmp("status"), DesktopState::Available);
        a.request(
            "capture",
            &json!({"region": {"x": 0, "y": 0, "width": 3, "height": 3}, "pin": true}),
        )
        .unwrap();
        let s = a.status();
        assert_eq!(s["pins"]["open"], 1);
        assert_eq!(s["memory"]["bytes"], 36);
        assert_eq!(s["capture_backend"], "gdi");
        assert_eq!(s["hotkeys"]["off_reason"], "--no-hotkeys");
        let text = s.to_string();
        assert!(!text.contains("10, 20, 30"), "no pixel values");
        let act = a.request("activate", &json!({})).unwrap();
        assert_eq!(act["settings_window"], "planned");
    }

    /// An instance with the default keys (no registrar) on the fake 100×100 desktop.
    fn keyed(tag: &str) -> App<FakeShell> {
        let dir = tmp(tag);
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app(&dir, DesktopState::Available);
        a.hotkeys = Manager::start(&Paths::under(&dir), None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        a
    }

    fn chord(name: &str, ctrl: bool, shift: bool) -> Chord {
        crate::hotkeys::chord_of(name, ctrl, false, shift).unwrap()
    }

    fn press(a: &mut App<FakeShell>, id: PinId, name: &str, shift: bool) {
        a.pin_event(PinEvent::Key(id, chord(name, false, shift), false));
    }

    #[test]
    fn pin_keys_zoom_hide_destroy_and_escape_always_closes() {
        let mut a = keyed("pin-keys");
        a.shell.clipboard = Some((20, 10));
        a.shell.pointer_at = Some((10, 10));
        a.action("paste.float", None);
        let id = PinId(1);
        assert_eq!(a.pins.rect(id), Some(Rect::new(10, 10, 20, 10)));
        // Plus zooms in around the pointer (inside the pin), 0 returns to 100 %.
        a.shell.pointer_at = Some((20, 15));
        press(&mut a, id, "Plus", false);
        let info = a.pins.info(id).unwrap();
        assert_eq!((info.zoom, info.rect), (110, Rect::new(9, 10, 22, 11)));
        press(&mut a, id, "Minus", false);
        press(&mut a, id, "Minus", false);
        assert_eq!(a.pins.info(id).unwrap().zoom, 90);
        press(&mut a, id, "0", false);
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        assert_eq!(a.status()["last_action"]["action"], "pin.zoom-reset");
        // H hides it (with a hint how to get it back), IPC shows it again.
        press(&mut a, id, "H", false);
        assert!(!a.pins.info(id).unwrap().visible);
        assert!(a.shell.notices.last().unwrap().contains("noeul pin show"));
        assert_eq!(a.status()["pins"]["hidden"], 1);
        assert!(a.tray_menu().contains(&MenuItem::ShowAllPins));
        let r = a.request("pin.show", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(r["shown"], json!(["pin-1"]));
        // Shift+Escape destroys: nothing is kept for reopen.
        press(&mut a, id, "Escape", true);
        assert!(a.pins.is_empty());
        assert_eq!(a.pins.reopenable(), None);
        assert_eq!(a.ledger.live(), (0, 0));
        // With every shortcut off, Escape still closes the focused pin (NOEUL-SEC-013).
        let mut off = app(&tmp("pin-keys-off"), DesktopState::Available);
        off.shell.clipboard = Some((5, 5));
        off.action("paste.float", None);
        press(&mut off, PinId(1), "Plus", false);
        assert_eq!(
            off.pins.info(PinId(1)).unwrap().zoom,
            100,
            "no zoom key while off"
        );
        off.pin_event(PinEvent::Key(PinId(1), chord("Escape", false, false), true));
        assert_eq!(off.pins.len(), 1, "a held Escape does not close by repeat");
        press(&mut off, PinId(1), "Escape", false);
        assert!(off.pins.is_empty(), "the reserved Escape closes it");
        assert_eq!(off.pins.reopenable(), Some(PinId(1)));
    }

    #[test]
    fn the_pin_menu_lists_this_builds_actions_with_their_keys() {
        let mut a = keyed("pin-menu");
        a.shell.clipboard = Some((40, 30));
        a.action("paste.float", None);
        let id = PinId(1);
        a.pin_event(PinEvent::Menu(id, Some((50, 60))));
        let menu = a.take_pin_menu().expect("a menu request");
        assert_eq!((menu.pin, menu.at), (id, Some((50, 60))));
        let labels: Vec<(&str, bool)> = menu
            .items
            .iter()
            .map(|i| (i.label.as_str(), i.enabled))
            .collect();
        assert_eq!(
            labels,
            [
                ("Zoom in\tPlus", true),
                ("Zoom out\tMinus", true),
                ("Zoom 100 %\t0", false),
                ("", false),
                ("Hide\tH", true),
                ("Close\tEscape", true),
                ("Destroy\tShift+Escape", true),
            ]
        );
        for action in menu.items.iter().filter_map(|i| i.action) {
            assert!(
                crate::hotkeys::IMPLEMENTED.contains(&action),
                "{action} is implemented (NOEUL-FR-114)"
            );
        }
        assert!(a.take_pin_menu().is_none(), "taken once");
        // The choice runs the action on that pin only.
        a.pin_menu_choice(id, "pin.zoom-in");
        assert_eq!(a.pins.info(id).unwrap().zoom, 110);
        a.pin_event(PinEvent::Menu(id, None));
        let items = a.take_pin_menu().unwrap().items;
        assert!(items[2].enabled, "100 % is offered once zoomed");
        a.pin_menu_choice(id, "capture.region");
        assert!(
            !a.overlay_open(),
            "only pin actions are accepted from the menu"
        );
        a.pin_menu_choice(id, "pin.close");
        assert!(a.pins.is_empty());
        // A menu for a pin that is gone is not shown.
        a.pin_event(PinEvent::Menu(id, None));
        assert!(a.take_pin_menu().is_none());
        // Under the test hook a menu is never shown (it would open on the user's screen).
        a.shell.interactive = Some(false);
        a.action("paste.float", None);
        a.pin_event(PinEvent::Menu(PinId(2), None));
        assert!(a.take_pin_menu().is_none());
        assert_eq!(a.status()["last_action"]["action"], "pin.menu");
        a.shell.interactive = None;
        a.pin_menu_choice(id, "pin.hide");
        assert_eq!(a.status()["last_action"]["result"], "not_found");
    }

    #[test]
    fn the_wheel_zooms_around_the_pointer_and_middle_click_resets() {
        let mut a = keyed("pin-wheel");
        a.shell.clipboard = Some((40, 20));
        a.shell.pointer_at = Some((0, 0));
        a.action("paste.float", None);
        let id = PinId(1);
        // A high-resolution wheel: two half notches make one step.
        a.pin_event(PinEvent::Wheel(id, 60, (20, 10), false));
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        a.pin_event(PinEvent::Wheel(id, 60, (20, 10), false));
        let info = a.pins.info(id).unwrap();
        assert_eq!(
            (info.zoom, info.rect.x, info.rect.y),
            (110, -2, -1),
            "around (20,10)"
        );
        a.pin_event(PinEvent::Wheel(id, 120, (20, 10), true));
        assert_eq!(
            a.pins.info(id).unwrap().zoom,
            110,
            "Ctrl+wheel (opacity) is N2"
        );
        a.pin_event(PinEvent::Wheel(id, -240, (20, 10), false));
        assert_eq!(a.pins.info(id).unwrap().zoom, 90);
        a.pin_event(PinEvent::MiddleClick(id));
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        assert!(
            a.shell.windows.log.borrow().focused.len() <= 1,
            "zooming never focuses"
        );
        a.pin_event(PinEvent::Wheel(PinId(9), 120, (0, 0), false));
        assert!(
            !a.wheel.contains_key(&PinId(9)),
            "nothing is kept for pins that do not exist"
        );
    }

    #[test]
    fn a_monitor_disconnect_keeps_pins_and_offers_the_way_back() {
        let (mut a, syn, _dir) = overlay_app("pin-display");
        let left = a
            .request(
                "pin.create",
                &json!({"source": "region", "region": {"x": -600, "y": 50, "width": 200, "height": 100}}),
            )
            .unwrap();
        assert_eq!(left["pin"]["id"], "pin-1");
        a.request(
            "capture",
            &json!({"region": {"x": 10, "y": 10, "width": 30, "height": 30}, "pin": true}),
        )
        .unwrap();
        assert_eq!(a.status()["pins"]["off_screen"], 0);
        let notices = a.shell.notices.len();
        // The left monitor is unplugged; the runtime reports the display change.
        syn.set_monitors(vec![
            Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(144),
        ]);
        a.display_changed();
        let st = a.status();
        assert_eq!(st["pins"]["off_screen"], 1);
        assert!(
            a.shell.notices[notices].contains("off-screen"),
            "{:?}",
            a.shell.notices
        );
        assert_eq!(
            a.pins.rect(PinId(1)),
            Some(Rect::new(-600, 50, 200, 100)),
            "it keeps its place (NOEUL-FR-067)"
        );
        let list = a.request("pin.list", &json!({})).unwrap();
        assert_eq!(list["pins"][0]["on_screen"], false);
        assert_eq!(list["pins"][1]["on_screen"], true);
        assert!(a.tray_menu().contains(&MenuItem::BringPinsOnScreen));
        // A dry run moves nothing; the real request brings only the lost pin back.
        let dry = a
            .request(
                "pin.show",
                &json!({"all": true, "on_screen": true, "dry_run": true}),
            )
            .unwrap();
        assert_eq!(dry["moved"], json!(["pin-1"]));
        assert_eq!(a.pins.rect(PinId(1)).unwrap().x, -600);
        let r = a
            .request("pin.show", &json!({"all": true, "on_screen": true}))
            .unwrap();
        assert_eq!(
            (r["shown"].clone(), r["moved"].clone()),
            (json!([]), json!(["pin-1"]))
        );
        assert_eq!(a.pins.rect(PinId(1)), Some(Rect::new(0, 50, 200, 100)));
        assert_eq!(a.status()["pins"]["off_screen"], 0);
        assert!(!a.tray_menu().contains(&MenuItem::BringPinsOnScreen));
        // The tray's item does the same for every pin, and shows hidden ones.
        syn.set_monitors(vec![Monitor::new(Rect::new(2000, 0, 300, 300), true)]);
        a.display_changed();
        a.request("pin.hide", &json!({"all": true})).unwrap();
        a.tray_command(MenuItem::BringPinsOnScreen);
        let st = a.status();
        assert_eq!(
            (
                st["pins"]["off_screen"].as_u64(),
                st["pins"]["hidden"].as_u64()
            ),
            (Some(0), Some(0))
        );
        // While the desktop is locked nothing is reported as lost.
        syn.set_state(DesktopState::Locked);
        a.display_changed();
        assert_eq!(
            a.request("pin.list", &json!({})).unwrap()["pins"][0]["on_screen"],
            Value::Null
        );
    }

    #[test]
    fn pin_selection_is_the_step_51_entry_point_without_the_clipboard() {
        let (mut a, _syn, _dir) = overlay_app("pin-selection");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        let sid = a.overlay.as_ref().unwrap().id;
        let rect = Rect::new(100, 100, 200, 100);
        a.captures.select(sid, rect).unwrap();
        let sel = a.captures.selection(sid).unwrap();
        let guard = Duration::from_millis(500);
        let id = a
            .pin_selection(
                sel,
                PinOptions {
                    focus: true,
                    guard: Some(guard),
                },
            )
            .unwrap();
        {
            let log = a.shell.windows.log.borrow();
            assert_eq!(
                log.created,
                [(id, rect, true)],
                "same place and pixel size, focused"
            );
            assert_eq!(
                log.guards,
                [Some(guard)],
                "the double-click tail is swallowed"
            );
        }
        assert_eq!(a.shell.clipboard_reads, 0, "no clipboard in between");
        assert_eq!(
            a.ledger.live(),
            (
                2,
                rect.bgra_bytes() + Rect::new(-640, 0, 1600, 540).bgra_bytes()
            ),
            "the pin's copy and the frozen picture, still owned by the open session"
        );
        // A failure hands nothing to the clipboard and keeps the session.
        *a.shell.windows.fail.borrow_mut() = true;
        let sel = a.captures.selection(sid).unwrap();
        assert!(
            a.pin_selection(
                sel,
                PinOptions {
                    focus: true,
                    guard: None
                }
            )
            .is_err()
        );
        assert!(a.overlay_open() && a.captures.is_open());
        assert_eq!(a.ledger.live().0, 2, "the failed copy was freed");
        // Under the test hook a pin never takes focus, whatever the caller asks.
        *a.shell.windows.fail.borrow_mut() = false;
        a.shell.interactive = Some(false);
        let sel = a.captures.selection(sid).unwrap();
        a.pin_selection(
            sel,
            PinOptions {
                focus: true,
                guard: None,
            },
        )
        .unwrap();
        assert!(!a.shell.windows.log.borrow().created[1].2);
    }

    #[test]
    fn paste_never_lands_over_an_open_overlay_and_a_huge_paste_fits() {
        let (mut a, _syn, _dir) = overlay_app("pin-paste-overlay");
        a.shell.clipboard = Some((10, 10));
        a.action("capture.region", None);
        a.action("paste.float", None);
        assert!(a.pins.is_empty());
        assert_eq!(a.shell.clipboard_reads, 0, "the clipboard is not even read");
        assert_eq!(a.status()["last_action"]["result"], "overlay_open");
        assert!(a.overlay_open(), "the overlay keeps its focus");
        a.request("capture.cancel", &json!({})).unwrap();
        // A picture larger than the monitor's work area is zoomed out to fit it.
        let mut b = app(&tmp("pin-paste-huge"), DesktopState::Available);
        b.shell.clipboard = Some((300, 200));
        b.action("paste.float", None);
        let info = b.pins.info(PinId(1)).unwrap();
        assert_eq!(info.image, (300, 200));
        assert_eq!(
            (info.zoom, info.rect),
            (33, Rect::new(1, 34, 99, 66)),
            "fits, at the pointer"
        );
        // pin.create from the clipboard: at a point, never focused, announced, read once.
        let r = b
            .request(
                "pin.create",
                &json!({"source": "clipboard", "dry_run": true}),
            )
            .unwrap();
        assert_eq!(r["pin"], Value::Null);
        assert_eq!(b.shell.clipboard_reads, 1, "a dry run reads nothing");
        b.shell.clipboard = Some((10, 10));
        let r = b
            .request(
                "pin.create",
                &json!({"source": "clipboard", "at": {"x": 95, "y": 2}}),
            )
            .unwrap();
        assert_eq!(
            r["pin"]["rect"],
            json!({"x": 90, "y": 2, "width": 10, "height": 10})
        );
        assert_eq!(b.shell.clipboard_reads, 2);
        assert!(!b.shell.windows.log.borrow().created[1].2, "never focused");
        assert_eq!(
            b.shell.notices.last().unwrap(),
            "Noeul pinned the clipboard (command line)."
        );
        b.shell.clipboard = None;
        let e = b
            .request("pin.create", &json!({"source": "clipboard"}))
            .unwrap_err();
        assert_eq!(e.code, Code::NothingToPin);
        let e = b
            .request(
                "pin.create",
                &json!({"source": "region", "region": {"x": 95, "y": 0, "width": 10, "height": 1}}),
            )
            .unwrap_err();
        assert_eq!(e.code, Code::Usage, "outside the desktop");
    }

    #[test]
    fn pins_live_in_memory_only_and_a_restart_restores_nothing() {
        let dir = tmp("pin-restart");
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app(&dir, DesktopState::Available);
        for x in [0, 20, 40] {
            a.request(
                "capture",
                &json!({"region": {"x": x, "y": 0, "width": 10, "height": 10}, "pin": true}),
            )
            .unwrap();
        }
        a.request("pin.close", &json!({"id": "pin-2"})).unwrap();
        a.request("pin.hide", &json!({"id": "pin-3"})).unwrap();
        assert_eq!(
            a.ledger.live(),
            (3, 3 * 400),
            "two open and one closed for reopen"
        );
        a.request("quit", &json!({"force": true})).unwrap();
        assert_eq!(a.ledger.live(), (0, 0), "quit frees every picture");
        assert!(
            files_under(&dir).is_empty(),
            "nothing about the pins reached the disk: {:?}",
            files_under(&dir)
        );
        // The next instance starts with no pins, no reopen memory and fresh IDs.
        let mut b = app(&dir, DesktopState::Available);
        let list = b.request("pin.list", &json!({})).unwrap();
        assert_eq!(list, json!({"pins": [], "reopenable": null}));
        let e = b.request("pin.reopen", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::PinNotFound);
        let r = b
            .request(
                "capture",
                &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
            )
            .unwrap();
        assert_eq!(r["pin"]["id"], "pin-1", "IDs belong to one instance");
    }

    #[test]
    fn hide_show_and_close_name_their_pins_and_leave_the_others() {
        let mut a = app(&tmp("pin-hide-show"), DesktopState::Available);
        for x in [0, 30, 60] {
            a.request(
                "capture",
                &json!({"region": {"x": x, "y": 0, "width": 10, "height": 10}, "pin": true}),
            )
            .unwrap();
        }
        let dry = a
            .request("pin.hide", &json!({"all": true, "dry_run": true}))
            .unwrap();
        assert_eq!(dry["hidden"], json!(["pin-1", "pin-2", "pin-3"]));
        assert_eq!(a.pins.hidden(), 0, "a dry run hides nothing");
        let r = a.request("pin.hide", &json!({"id": "pin-2"})).unwrap();
        assert_eq!(r["hidden"], json!(["pin-2"]));
        let r = a.request("pin.hide", &json!({"all": true})).unwrap();
        assert_eq!(
            r["hidden"],
            json!(["pin-1", "pin-3"]),
            "pin-2 was hidden already"
        );
        a.tray_command(MenuItem::ShowAllPins);
        assert_eq!(a.pins.hidden(), 0);
        a.tray_command(MenuItem::HideAllPins);
        assert_eq!(a.pins.hidden(), 3);
        let r = a.request("pin.show", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(r["shown"], json!(["pin-1"]));
        a.request("pin.close", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(a.pins.ids(), [PinId(2), PinId(3)]);
        assert!(a.pins.info(PinId(2)).is_some_and(|i| !i.visible));
        let e = a.request("pin.show", &json!({"id": "pin-1"})).unwrap_err();
        assert_eq!(e.code, Code::PinNotFound);
        let e = a
            .request("pin.show", &json!({"id": "pin-2", "on_screen": "yes"}))
            .unwrap_err();
        assert_eq!(e.code, Code::Usage);
        assert!(
            a.shell.windows.log.borrow().focused.is_empty(),
            "no IPC request gave a pin focus"
        );
    }
}
