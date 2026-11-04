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

use crate::capture::{CaptureBackend, DesktopState, check_region};
use crate::error::{Code, NoeulError, Result};
use crate::frame::{Frame, Ledger, Rect};
use crate::hotkeys::Manager;
use crate::ipc;
use crate::paths::Paths;
use crate::pins::{PinError, PinId, PinWindows, Pins, Source};
use crate::selection::{Captures, Origin, SessionError};
use crate::settings;
use crate::tray::{Click, MenuItem, menu};
use crate::util::rfc3339;
use kim_hotkeys::keys::Chord;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

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
}

impl<S: Shell> std::fmt::Debug for App<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("pins", &self.pins.len())
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
            paths,
            id,
            started_at: SystemTime::now(),
            started: Instant::now(),
            quit: false,
            last_action: None,
        }
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
            "captures": {"started": started, "pinned": pinned, "cancelled": cancelled, "open": self.captures.is_open()},
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
            .begin(Origin::Cli, frame)
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
        self.captures.cancel();
        self.pins.clear();
        self.hotkeys.release_all();
    }

    /// A Noeul action from a global hotkey, the tray or a pin (by action ID).
    pub fn action(&mut self, action: &str, pin: Option<PinId>) {
        let result = match (action, pin) {
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
            // Left click starts a capture once the overlay exists (Step 50); the menu opens.
            Click::Left | Click::Right => {}
        }
    }

    pub fn tray_command(&mut self, item: MenuItem) {
        match item {
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
        // NOEUL-SEC-003: no action while the user's desktop is not the input desktop.
        if self.capture.desktop().state != DesktopState::Available {
            self.last_action = Some(("hotkey".into(), "desktop_unavailable"));
            return;
        }
        if let Some(a) = self.hotkeys.action_for(id) {
            self.action(&a, None);
        }
    }
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
    use crate::capture::{CaptureError, Desktop, Monitor};
    use crate::pins::fake::FakeWindows;

    /// A desktop of one 100×100 monitor that captures solid frames.
    pub struct FakeCapture {
        pub state: DesktopState,
    }

    impl CaptureBackend for FakeCapture {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn desktop(&self) -> Desktop {
            Desktop {
                state: self.state,
                virtual_rect: Some(Rect::new(0, 0, 100, 100)),
                monitors: vec![Monitor {
                    rect: Rect::new(0, 0, 100, 100),
                    primary: true,
                }],
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
        pub notices: Vec<String>,
        pub confirm: bool,
    }

    impl Shell for FakeShell {
        fn pin_windows(&mut self) -> &mut dyn PinWindows {
            &mut self.windows
        }
        fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Frame> {
            match self.clipboard {
                Some((w, h)) => {
                    Ok(Frame::solid(ledger, Rect::new(0, 0, w, h), [1, 1, 1, 255]).unwrap())
                }
                None => Err(NoeulError::new(Code::NothingToPin, "no image")),
            }
        }
        fn pointer(&self) -> (i32, i32) {
            (90, 95)
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
        assert_eq!(s["capture_backend"], "fake");
        assert_eq!(s["hotkeys"]["off_reason"], "--no-hotkeys");
        let text = s.to_string();
        assert!(!text.contains("10, 20, 30"), "no pixel values");
        let act = a.request("activate", &json!({})).unwrap();
        assert_eq!(act["settings_window"], "planned");
    }
}
