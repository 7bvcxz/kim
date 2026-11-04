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

use crate::capture::{CaptureBackend, DesktopState, check_region};
use crate::error::{Code, NoeulError, Result};
use crate::frame::{Frame, Ledger, Rect};
use crate::hotkeys::Manager;
use crate::ipc;
use crate::overlay::{
    self, Action as OverlayAction, Cursor, Input as OverlayInput, Measure,
    Outcome as OverlayOutcome, Overlay, Scene, SystemValues,
};
use crate::paths::Paths;
use crate::pins::{PinError, PinId, PinWindows, Pins, Source};
use crate::selection::{Captures, Origin, Outcome as SessionOutcome, SessionError};
use crate::settings::{self, KeyboardHelp, UiState};
use crate::tray::{Click, MenuItem, menu};
use crate::util::rfc3339;
use kim_hotkeys::keys::Chord;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// What the core needs from the desktop besides capturing.
pub trait Shell {
    fn pin_windows(&mut self) -> &mut dyn PinWindows;
    /// Reads the clipboard once and returns its image (`nothing_to_pin`,
    /// `clipboard_unavailable` or `image_invalid` otherwise). Called only for `paste.float`.
    fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Frame>;
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
        })
    }
}

/// The overlay's keyboard help, from the effective bindings (so a changed key shows as such).
pub fn overlay_help(m: &Manager) -> Vec<String> {
    let k = |a: &str| m.keys_for(a).map(str::to_string);
    let mut lines = vec!["Drag to select a region. Esc or right-click cancels.".to_string()];
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
}

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
            "pin.list" => Ok(self.pins.list()),
            "pin.close" => self.pin_close(params, dry_run),
            "pin.reopen" => self.pin_reopen(dry_run),
            "hotkeys.reload" => {
                let problem = self.hotkeys.reload();
                Ok(json!({"reloaded": problem.is_none(), "problem": problem,
                          "hotkeys": self.hotkeys.status_json()}))
            }
            "quit" => self.quit_request(ipc::flag(params, "force")?, dry_run),
            other => Err(NoeulError::new(
                Code::Usage,
                format!("unknown method `{other}`"),
            )),
        }
    }

    pub fn status(&self) -> Value {
        let (started, pinned, cancelled) = self.captures.counts();
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
            "pins": {"open": self.pins.len(), "reopenable": self.pins.reopenable().map(|p| p.to_string())},
            "captures": {"started": started, "pinned": pinned, "cancelled": cancelled, "open": self.captures.is_open(),
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
        // A CLI-made pin never steals focus from the program the user is in (NOEUL-FR-060).
        match self
            .pins
            .create(self.shell.pin_windows(), frame, Source::Capture, false)
        {
            Ok(id) => {
                self.last_action = Some(("capture".into(), "pinned"));
                Ok(
                    json!({"dry_run": false, "region": region.to_json(), "pin": {
                    "id": id.to_string(), "rect": region.to_json(), "source": "capture",
                }, "desktop": desktop.state.as_str()}),
                )
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

    fn pin_close(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let all = ipc::flag(params, "all")?;
        let ids: Vec<PinId> = if all {
            self.pins.ids()
        } else {
            let text = params["id"].as_str().ok_or_else(|| {
                NoeulError::new(Code::Usage, "give a pin id (`id`) or `all: true`")
            })?;
            let id = PinId::parse(text).ok_or_else(|| pin_not_found(text))?;
            if !self.pins.contains(id) {
                return Err(pin_not_found(text));
            }
            vec![id]
        };
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
        self.captures.cancel();
        self.pins.clear();
        self.hotkeys.release_all();
    }

    /// A Noeul action from a global hotkey, the tray or a pin (by action ID).
    pub fn action(&mut self, action: &str, pin: Option<PinId>) {
        let result = match (action, pin) {
            ("capture.region", _) => self.capture_region_action(Origin::Hotkey, Instant::now()),
            ("paste.float", _) => self.paste_float(),
            ("pin.close", Some(id)) => {
                if self.pins.close(id).is_ok() {
                    "closed"
                } else {
                    "not_found"
                }
            }
            ("pin.move-up", Some(id)) => self.nudge(id, 0, -1),
            ("pin.move-down", Some(id)) => self.nudge(id, 0, 1),
            ("pin.move-left", Some(id)) => self.nudge(id, -1, 0),
            ("pin.move-right", Some(id)) => self.nudge(id, 1, 0),
            _ => "ignored",
        };
        self.last_action = Some((action.to_string(), result));
    }

    fn nudge(&mut self, id: PinId, dx: i32, dy: i32) -> &'static str {
        if self.pins.nudge(id, dx, dy) {
            "moved"
        } else {
            "not_found"
        }
    }

    /// `paste.float`: read the clipboard once and pin its image at the pointer, at 100 %
    /// (NOEUL-FR-070, image formats only in this build). Nothing to pin → a notice, no change
    /// (NOEUL-FR-074).
    fn paste_float(&mut self) -> &'static str {
        if self.capture.desktop().state != DesktopState::Available {
            return "desktop_unavailable";
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
        let (x, y) = place(&frame, (px, py), desktop.virtual_rect);
        let frame = frame.moved_to(x, y);
        match self
            .pins
            .create(self.shell.pin_windows(), frame, Source::Clipboard, true)
        {
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
        );
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
        show.then(|| overlay_help(&self.hotkeys))
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
            OverlayEvent::Input(i) => i,
            OverlayEvent::Key(chord, repeat) => match self
                .hotkeys
                .overlay_action(&chord, repeat)
                .as_deref()
                .and_then(OverlayAction::from_id)
            {
                Some(a) => OverlayInput::Action(a),
                // The reserved navigation key always leaves (nav.cancel, NOEUL-SEC-013).
                None if !repeat && is_escape(&chord) => OverlayInput::Leave,
                None => return,
            },
        };
        let outcome = match self.overlay.as_mut() {
            Some(ov) => ov.model.handle(input),
            None => return,
        };
        match outcome {
            OverlayOutcome::Stay => self.redraw_overlay(),
            OverlayOutcome::Cancel => {
                self.cancel_overlay();
            }
            OverlayOutcome::Pin(rect) => self.pin_from_overlay(rect),
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
        self.last_action = Some(("capture.cancel".into(), "cancelled"));
        true
    }

    /// `capture.pin`: the session's selection becomes a pin at the same place and size, with
    /// keyboard focus; the overlay closes. Nothing touches the clipboard or the disk
    /// (NOEUL-FR-020, NOEUL-SEC-005). If the pin cannot be created, the overlay stays with its
    /// selection and says so (NOEUL-FR-025).
    fn pin_from_overlay(&mut self, rect: Rect) {
        let Some(id) = self.overlay.as_ref().map(|o| o.id) else {
            return;
        };
        let selection = self
            .captures
            .select(id, rect)
            .and_then(|()| self.captures.selection(id));
        let failed = match selection {
            Ok(sel) => {
                let focus = self.shell.interactive();
                match self.pins.create(
                    self.shell.pin_windows(),
                    sel.into_frame(),
                    Source::Capture,
                    focus,
                ) {
                    Ok(_) => {
                        let _ = self.captures.complete(id, SessionOutcome::Pinned);
                        if let Some(mut ov) = self.overlay.take() {
                            ov.window.destroy();
                        }
                        self.help_done();
                        self.last_action = Some(("capture.pin".into(), "pinned"));
                        return;
                    }
                    Err((frame, _)) => {
                        drop(frame);
                        "The pin could not be created. Your selection is kept: try again, or press Esc."
                    }
                }
            }
            Err(_) => "The selection could not be taken. Try again, or press Esc.",
        };
        if let Some(ov) = self.overlay.as_mut() {
            ov.model.set_banner(Some(failed.to_string()));
        }
        self.redraw_overlay();
        self.last_action = Some(("capture.pin".into(), "failed"));
    }

    /// Routes a pin window event.
    pub fn pin_event(&mut self, e: PinEvent) {
        match e {
            PinEvent::Key(id, chord, repeat) => {
                if let Some(a) = self.hotkeys.pin_action(&chord, repeat) {
                    self.action(&a, Some(id));
                }
            }
            PinEvent::Moved(id, x, y) => {
                self.pins.moved(id, x, y);
            }
            PinEvent::Closed(id) => {
                let _ = self.pins.close(id);
            }
        }
    }

    /// The tray menu for the current state (items appear only once implemented, NOEUL-FR-004).
    pub fn tray_menu(&self) -> Vec<MenuItem> {
        menu(&crate::tray::MenuState {
            reopenable: self.pins.reopenable().is_some(),
            shortcuts_on: self.hotkeys.status_json()["enabled"] == true,
            shortcuts_switchable: !hotkeys_forced_off(&self.hotkeys),
        })
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
            MenuItem::ReopenClosedPin => {
                let _ = self.pins.reopen(self.shell.pin_windows(), true);
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

fn hotkeys_forced_off(m: &Manager) -> bool {
    let s = m.status_json();
    matches!(
        s["off_reason"].as_str(),
        Some("--no-hotkeys" | "KIM_NO_HOTKEYS" | "safe_mode")
    )
}

/// Where a pasted picture goes: its top-left corner at the pointer, moved back inside the
/// virtual desktop when it would stick out (a picture larger than the desktop keeps its top-left
/// on the desktop).
pub fn place(frame: &Frame, pointer: (i32, i32), desktop: Option<Rect>) -> (i32, i32) {
    let (mut x, mut y) = pointer;
    if let Some(d) = desktop {
        let max_x = d.right() - i64::from(frame.width());
        let max_y = d.bottom() - i64::from(frame.height());
        x = i64::from(x).min(max_x).max(i64::from(d.x)) as i32;
        y = i64::from(y).min(max_y).max(i64::from(d.y)) as i32;
    }
    (x, y)
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
            SystemValues::default()
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

    fn tmp(tag: &str) -> std::path::PathBuf {
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
    fn overlay_app(tag: &str) -> (App<FakeShell>, Arc<Synthetic>, std::path::PathBuf) {
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
            assert_eq!(at(400, 400), overlay::dim_pixel(Synthetic::pixel(400, 400)));
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
            overlay_help(&off),
            ["Drag to select a region. Esc or right-click cancels."],
            "without shortcuts only the mouse and Escape remain"
        );
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
        let lines = overlay_help(&m);
        assert_eq!(
            lines[1],
            "code:KeyW/Down/Left/Right move \u{b7} Ctrl+arrows grow \u{b7} Shift+arrows shrink"
        );
        assert_eq!(
            lines[2],
            "Ctrl+A whole monitor \u{b7} Ctrl+Enter pins the selection"
        );
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
}
