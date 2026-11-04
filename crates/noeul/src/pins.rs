//! The pin engine's model: pins, their IDs, geometry, visibility and lifecycle (NOEUL-FR-060,
//! NOEUL-FR-061, NOEUL-FR-065, NOEUL-FR-067, NOEUL-FR-025, NOEUL-FR-026; ADR 0023 §4, ADR 0029).
//!
//! A pin is a floating window that owns one [`Frame`] (its picture at 100 %). The registry gives
//! every pin a stable ID (`pin-<n>`, never reused while the instance runs), keeps the windows
//! behind the narrow [`PinWindow`]/[`PinWindows`] traits (the Win32 implementation is
//! `win::pinwnd`; tests use [`fake`]), and enforces the lifecycle:
//!
//! ```text
//!   create ──► open (shown ⇄ hidden, moved, zoomed) ──close──► closed (reopen memory: the last one)
//!                │                                             │            ──reopen──► open (same id)
//!                └──destroy──► gone ◄──────────────────────────┘ (a newer close replaces it; quit clears it)
//! ```
//!
//! - **Independent.** Each pin has its own ID, position, zoom, visibility and lifetime; an
//!   operation on one never touches another's window.
//! - **Geometry.** The picture's rectangle holds the window's top-left corner; the window's size
//!   is the picture's size at the pin's zoom ([`crate::zoom`]). [`Placement::Exact`] keeps a
//!   capture at its place and pixel size (NOEUL-FR-020); [`Placement::At`] puts a paste at a
//!   point inside the work area of the monitor there and zooms a picture larger than that work
//!   area out until it fits, so a huge paste never covers every screen (NOEUL-SEC-013).
//! - **Display changes.** Pins keep their place when monitors change, even off-screen
//!   (NOEUL-FR-067): [`Pins::sync`] only follows where the system really put a window, and
//!   [`Pins::bring_on_screen`] moves a pin that no monitor shows into the nearest work area,
//!   on request.
//! - **Memory.** Closing destroys the window but keeps the picture so "Reopen closed pin" can bring
//!   it back (default memory: the last pin, NOEUL-FR-065); destroying frees it for good. Closing the
//!   capture overlay never closes pins. Quitting releases every picture. Nothing is written to
//!   disk, so pins do not survive a restart (NOEUL-SEC-005; opt-in backup is NOEUL-FR-066, N4).

use crate::capture::Desktop;
use crate::frame::{Frame, Rect};
use crate::util::rfc3339;
use crate::zoom;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// A pin's stable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinId(pub u64);

impl PinId {
    pub fn parse(s: &str) -> Option<PinId> {
        let n = s.strip_prefix("pin-")?;
        if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok().map(PinId)
    }
}

impl std::fmt::Display for PinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pin-{}", self.0)
    }
}

/// Where a pin's picture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Capture,
    Clipboard,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Capture => "capture",
            Source::Clipboard => "clipboard",
        }
    }
}

/// How a pin window is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSpec {
    /// Position and size of the window; the picture is scaled to its size.
    pub rect: Rect,
    /// Give it keyboard focus (only when the user created it, NOEUL-FR-060).
    pub focus: bool,
    /// Ignore presses on it for this long after it appears, so the rest of the gesture that
    /// created it (the remaining clicks of a double-click) cannot move, close or zoom it
    /// (NOEUL_UX_SPEC §4.2 rule 10).
    pub guard: Option<Duration>,
}

/// One pin window. Implementations own an OS window and destroy it in [`PinWindow::destroy`]
/// (and on drop). No method activates the window except [`PinWindow::focus`].
pub trait PinWindow {
    /// Moves the window (device pixels).
    fn set_position(&mut self, x: i32, y: i32);
    /// Shows `frame` scaled to `rect` (the window's new position and size).
    fn present(&mut self, frame: &Frame, rect: Rect) -> Result<(), String>;
    /// Hides or shows the window without changing which window has focus; a window shown again
    /// is above the other pins.
    fn set_visible(&mut self, visible: bool);
    /// Gives the window keyboard focus; `false` when the OS refused.
    fn focus(&mut self) -> bool;
    /// Where the window really is (the system may move windows when monitors change).
    fn rect(&self) -> Option<Rect>;
    /// Destroys the window now.
    fn destroy(&mut self);
}

/// Creates pin windows.
pub trait PinWindows {
    /// A visible window showing `frame` as `spec` says.
    fn create(
        &mut self,
        id: PinId,
        frame: &Frame,
        spec: &WindowSpec,
    ) -> Result<Box<dyn PinWindow>, String>;
}

/// Where a new pin goes (ADR 0029).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Exactly the picture's rectangle at 100 %: a capture stays at its place and pixel size
    /// (NOEUL-FR-020, UX spec §4.2 rule 8).
    Exact,
    /// The top-left corner at this point, moved inside the work area of the monitor there (or
    /// the nearest one), zoomed out when the picture is larger than that work area (a paste).
    At(i32, i32),
}

/// A pin to create: the engine's entry point for every source (capture overlay, CLI, paste and
/// the Step 51 gestures).
#[derive(Debug)]
pub struct NewPin {
    pub frame: Frame,
    pub source: Source,
    pub placement: Placement,
    pub focus: bool,
    pub guard: Option<Duration>,
}

/// What [`Pins::info`] reports about one open pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinInfo {
    /// The window's rectangle: position and size on screen.
    pub rect: Rect,
    /// The picture's size in pixels (100 %).
    pub image: (u32, u32),
    /// Percent; 100 = one image pixel per device pixel.
    pub zoom: u32,
    pub visible: bool,
    pub source: Source,
}

struct Pin {
    /// The picture; its origin is the window's top-left corner.
    frame: Frame,
    zoom: u32,
    visible: bool,
    window: Box<dyn PinWindow>,
    source: Source,
    created: SystemTime,
}

impl Pin {
    fn rect(&self) -> Rect {
        display_rect(&self.frame, self.zoom)
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pin")
            .field("rect", &self.rect())
            .field("zoom", &self.zoom)
            .field("visible", &self.visible)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

fn display_rect(frame: &Frame, zoom: u32) -> Rect {
    let r = frame.rect();
    let (w, h) = zoom::size(r.width, r.height, zoom);
    Rect::new(r.x, r.y, w, h)
}

#[derive(Debug)]
struct Closed {
    id: PinId,
    frame: Frame,
    zoom: u32,
    source: Source,
    created: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    NotFound(PinId),
    NothingToReopen,
    /// The window could not be created or updated; nothing changed.
    Window(String),
}

/// How much of a pin must lie on one monitor for it to count as on screen: enough to see and
/// grab it (device pixels per side, or the whole pin when it is smaller).
pub const ON_SCREEN_MIN: u32 = 24;

/// Whether the desktop shows enough of `r` to reach it.
pub fn on_screen(desktop: &Desktop, r: Rect) -> bool {
    let need = (r.width.min(ON_SCREEN_MIN), r.height.min(ON_SCREEN_MIN));
    desktop.monitors.iter().any(|m| {
        crate::overlay::intersection(m.rect, r)
            .is_some_and(|i| i.width >= need.0 && i.height >= need.1)
    })
}

/// Where a `w`×`h` picture placed at `point` goes, and at which zoom: inside the work area of
/// the monitor at the point (or the nearest monitor), zoomed out when it is larger than that work
/// area. Without a monitor the point is kept at 100 %.
pub fn place_at(desktop: &Desktop, w: u32, h: u32, point: (i32, i32)) -> (i32, i32, u32) {
    let Some(m) = desktop.monitor_near(point.0, point.1) else {
        return (point.0, point.1, zoom::FULL);
    };
    let z = zoom::fit(w, h, m.work.width, m.work.height);
    let (dw, dh) = zoom::size(w, h, z);
    let (x, y) = clamp_into((point.0, point.1), (dw, dh), m.work);
    (x, y, z)
}

/// `pos` moved so that a `size` box lies inside `area` (its top-left on `area` when it is larger).
fn clamp_into(pos: (i32, i32), size: (u32, u32), area: Rect) -> (i32, i32) {
    let c = |p: i32, len: u32, start: i32, span: u32| {
        let max = i64::from(start) + i64::from(span) - i64::from(len);
        i64::from(p).min(max).max(i64::from(start)) as i32
    };
    (
        c(pos.0, size.0, area.x, area.width),
        c(pos.1, size.1, area.y, area.height),
    )
}

/// The pins of one instance.
#[derive(Debug, Default)]
pub struct Pins {
    open: BTreeMap<PinId, Pin>,
    closed: Option<Closed>,
    next: u64,
}

impl Pins {
    pub fn new() -> Pins {
        Pins::default()
    }

    /// Creates a pin as `new` says. On failure the frame is handed back (NOEUL-FR-025) and no
    /// ID is used up.
    pub fn create(
        &mut self,
        windows: &mut dyn PinWindows,
        desktop: &Desktop,
        new: NewPin,
    ) -> Result<PinId, (Frame, String)> {
        let NewPin {
            frame,
            source,
            placement,
            focus,
            guard,
        } = new;
        let (frame, zoom) = match placement {
            Placement::Exact => (frame, zoom::FULL),
            Placement::At(x, y) => {
                let (x, y, z) = place_at(desktop, frame.width(), frame.height(), (x, y));
                (frame.moved_to(x, y), z)
            }
        };
        let id = PinId(self.next + 1);
        let spec = WindowSpec {
            rect: display_rect(&frame, zoom),
            focus,
            guard,
        };
        match windows.create(id, &frame, &spec) {
            Ok(window) => {
                self.next += 1;
                self.open.insert(
                    id,
                    Pin {
                        frame,
                        zoom,
                        visible: true,
                        window,
                        source,
                        created: SystemTime::now(),
                    },
                );
                Ok(id)
            }
            Err(e) => Err((frame, e)),
        }
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn contains(&self, id: PinId) -> bool {
        self.open.contains_key(&id)
    }

    pub fn ids(&self) -> Vec<PinId> {
        self.open.keys().copied().collect()
    }

    /// How many open pins are hidden.
    pub fn hidden(&self) -> usize {
        self.open.values().filter(|p| !p.visible).count()
    }

    /// How many open pins no monitor shows (see [`on_screen`]).
    pub fn off_screen(&self, desktop: &Desktop) -> usize {
        self.open
            .values()
            .filter(|p| !on_screen(desktop, p.rect()))
            .count()
    }

    pub fn info(&self, id: PinId) -> Option<PinInfo> {
        self.open.get(&id).map(|p| PinInfo {
            rect: p.rect(),
            image: (p.frame.width(), p.frame.height()),
            zoom: p.zoom,
            visible: p.visible,
            source: p.source,
        })
    }

    /// The window's rectangle of an open pin.
    pub fn rect(&self, id: PinId) -> Option<Rect> {
        self.open.get(&id).map(Pin::rect)
    }

    /// The pin closed last (reopen memory), if any.
    pub fn reopenable(&self) -> Option<PinId> {
        self.closed.as_ref().map(|c| c.id)
    }

    /// Closes a pin: the window goes, the picture stays in the reopen memory (replacing the
    /// previously closed one, which is freed).
    pub fn close(&mut self, id: PinId) -> Result<(), PinError> {
        let mut pin = self.open.remove(&id).ok_or(PinError::NotFound(id))?;
        pin.window.destroy();
        self.closed = Some(Closed {
            id,
            frame: pin.frame,
            zoom: pin.zoom,
            source: pin.source,
            created: pin.created,
        });
        Ok(())
    }

    /// Removes a pin for good (`pin.destroy`): window and picture are freed.
    pub fn destroy(&mut self, id: PinId) -> Result<(), PinError> {
        let mut pin = self.open.remove(&id).ok_or(PinError::NotFound(id))?;
        pin.window.destroy();
        Ok(())
    }

    /// Reopens the last closed pin with its old ID, position and zoom.
    pub fn reopen(&mut self, windows: &mut dyn PinWindows, focus: bool) -> Result<PinId, PinError> {
        let c = self.closed.take().ok_or(PinError::NothingToReopen)?;
        let spec = WindowSpec {
            rect: display_rect(&c.frame, c.zoom),
            focus,
            guard: None,
        };
        match windows.create(c.id, &c.frame, &spec) {
            Ok(window) => {
                self.open.insert(
                    c.id,
                    Pin {
                        frame: c.frame,
                        zoom: c.zoom,
                        visible: true,
                        window,
                        source: c.source,
                        created: c.created,
                    },
                );
                Ok(c.id)
            }
            Err(e) => {
                self.closed = Some(c);
                Err(PinError::Window(e))
            }
        }
    }

    /// Hides a pin; `Ok(false)` when it was hidden already.
    pub fn hide(&mut self, id: PinId) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        if !p.visible {
            return Ok(false);
        }
        p.window.set_visible(false);
        p.visible = false;
        Ok(true)
    }

    /// Shows a hidden pin again (above the other pins, without focus); `Ok(false)` when it was
    /// shown already.
    pub fn show(&mut self, id: PinId) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        if p.visible {
            return Ok(false);
        }
        p.window.set_visible(true);
        p.visible = true;
        Ok(true)
    }

    /// Zooms a pin to `zoom` percent around the screen point `anchor`. `Ok(false)` when nothing
    /// changed; on a window error the pin keeps its old zoom.
    pub fn set_zoom(&mut self, id: PinId, zoom: u32, anchor: (i32, i32)) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let zoom = zoom.clamp(zoom::MIN, zoom::MAX);
        let (w, h) = zoom::size(p.frame.width(), p.frame.height(), zoom);
        if zoom == p.zoom || !zoom::allowed(w, h) {
            return Ok(false);
        }
        let r = p.frame.rect();
        let (x, y) = zoom::around((r.x, r.y), anchor, p.zoom, zoom);
        p.window
            .present(&p.frame, Rect::new(x, y, w, h))
            .map_err(PinError::Window)?;
        p.frame.set_origin(x, y);
        p.zoom = zoom;
        Ok(true)
    }

    /// The anchor a zoom key uses: the pointer when it is over the pin, else the pin's top-left
    /// corner (so a pin zoomed by keyboard does not jump away).
    pub fn zoom_anchor(&self, id: PinId, pointer: (i32, i32)) -> Option<(i32, i32)> {
        let r = self.rect(id)?;
        let inside = i64::from(pointer.0) >= i64::from(r.x)
            && i64::from(pointer.0) < r.right()
            && i64::from(pointer.1) >= i64::from(r.y)
            && i64::from(pointer.1) < r.bottom();
        Some(if inside { pointer } else { (r.x, r.y) })
    }

    /// The window was moved (by the user dragging it, or a key); keeps the model in sync.
    pub fn moved(&mut self, id: PinId, x: i32, y: i32) -> bool {
        match self.open.get_mut(&id) {
            Some(p) => {
                p.frame.set_origin(x, y);
                true
            }
            None => false,
        }
    }

    /// Moves a pin by a delta (`pin.move-*`, one device pixel per press).
    pub fn nudge(&mut self, id: PinId, dx: i32, dy: i32) -> bool {
        let Some(p) = self.open.get_mut(&id) else {
            return false;
        };
        let r = p.frame.rect();
        let (x, y) = (r.x.saturating_add(dx), r.y.saturating_add(dy));
        p.window.set_position(x, y);
        self.moved(id, x, y)
    }

    /// Follows where the system put the windows (after a display change), without moving any.
    /// Returns how many pins moved.
    pub fn sync(&mut self) -> usize {
        let mut n = 0;
        for p in self.open.values_mut() {
            if let Some(r) = p.window.rect() {
                let old = p.frame.rect();
                if (r.x, r.y) != (old.x, old.y) {
                    p.frame.set_origin(r.x, r.y);
                    n += 1;
                }
            }
        }
        n
    }

    /// Moves a pin that no monitor shows into the work area of the nearest monitor (zoomed out
    /// when it is larger). `Ok(false)` when it is on screen already (NOEUL-FR-067).
    pub fn bring_on_screen(&mut self, id: PinId, desktop: &Desktop) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let r = p.rect();
        if on_screen(desktop, r) || desktop.monitors.is_empty() {
            return Ok(false);
        }
        let center = (
            (i64::from(r.x) + i64::from(r.width) / 2) as i32,
            (i64::from(r.y) + i64::from(r.height) / 2) as i32,
        );
        let Some(m) = desktop.monitor_near(center.0, center.1) else {
            return Ok(false);
        };
        let fit = zoom::fit(
            p.frame.width(),
            p.frame.height(),
            m.work.width,
            m.work.height,
        );
        let z = p.zoom.min(fit);
        let (w, h) = zoom::size(p.frame.width(), p.frame.height(), z);
        let (x, y) = clamp_into((r.x, r.y), (w, h), m.work);
        if z == p.zoom {
            p.window.set_position(x, y);
        } else {
            p.window
                .present(&p.frame, Rect::new(x, y, w, h))
                .map_err(PinError::Window)?;
            p.zoom = z;
        }
        p.frame.set_origin(x, y);
        Ok(true)
    }

    pub fn focus(&mut self, id: PinId) -> bool {
        self.open.get_mut(&id).is_some_and(|p| p.window.focus())
    }

    /// Closes every pin and forgets everything, including the reopen memory (quit).
    pub fn clear(&mut self) -> usize {
        let n = self.open.len();
        for (_, mut p) in std::mem::take(&mut self.open) {
            p.window.destroy();
        }
        self.closed = None;
        n
    }

    /// The pin list (`pin.list`): IDs, geometry, zoom, visibility and origin only, never pixels
    /// (NOEUL-SEC-008). `on_screen` is `null` while the desktop is not available.
    pub fn list(&self, desktop: &Desktop) -> Value {
        let known = desktop.state == crate::capture::DesktopState::Available;
        json!({
            "pins": self.open.iter().map(|(id, p)| {
                let r = p.rect();
                json!({
                    "id": id.to_string(),
                    "rect": r.to_json(),
                    "image": {"width": p.frame.width(), "height": p.frame.height()},
                    "zoom": p.zoom,
                    "visible": p.visible,
                    "on_screen": known.then(|| on_screen(desktop, r)),
                    "source": p.source.as_str(),
                    "created_at": rfc3339(p.created),
                })
            }).collect::<Vec<_>>(),
            "reopenable": self.reopenable().map(|id| id.to_string()),
        })
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What the fake windows saw, shared with the test.
    #[derive(Debug, Default)]
    pub struct Log {
        /// `(id, window rect, focus)` of every created window.
        pub created: Vec<(PinId, Rect, bool)>,
        pub guards: Vec<Option<Duration>>,
        pub destroyed: Vec<PinId>,
        pub moved: Vec<(PinId, i32, i32)>,
        pub presented: Vec<(PinId, Rect)>,
        pub visible: Vec<(PinId, bool)>,
        pub focused: Vec<PinId>,
        pub alive: Vec<PinId>,
        /// Where each live window is (tests move one to stand for the system moving it).
        pub rects: BTreeMap<PinId, Rect>,
    }

    #[derive(Debug, Default, Clone)]
    pub struct FakeWindows {
        pub log: Rc<RefCell<Log>>,
        pub fail: Rc<RefCell<bool>>,
    }

    struct FakeWindow {
        id: PinId,
        log: Rc<RefCell<Log>>,
        fail: Rc<RefCell<bool>>,
        destroyed: bool,
    }

    impl PinWindow for FakeWindow {
        fn set_position(&mut self, x: i32, y: i32) {
            let mut l = self.log.borrow_mut();
            l.moved.push((self.id, x, y));
            if let Some(r) = l.rects.get_mut(&self.id) {
                r.x = x;
                r.y = y;
            }
        }
        fn present(&mut self, _: &Frame, rect: Rect) -> Result<(), String> {
            if *self.fail.borrow() {
                return Err("window update failed (test)".into());
            }
            let mut l = self.log.borrow_mut();
            l.presented.push((self.id, rect));
            l.rects.insert(self.id, rect);
            Ok(())
        }
        fn set_visible(&mut self, visible: bool) {
            self.log.borrow_mut().visible.push((self.id, visible));
        }
        fn focus(&mut self) -> bool {
            self.log.borrow_mut().focused.push(self.id);
            true
        }
        fn rect(&self) -> Option<Rect> {
            self.log.borrow().rects.get(&self.id).copied()
        }
        fn destroy(&mut self) {
            if !self.destroyed {
                self.destroyed = true;
                let mut l = self.log.borrow_mut();
                l.destroyed.push(self.id);
                l.alive.retain(|a| *a != self.id);
                l.rects.remove(&self.id);
            }
        }
    }

    impl Drop for FakeWindow {
        fn drop(&mut self) {
            self.destroy();
        }
    }

    impl PinWindows for FakeWindows {
        fn create(
            &mut self,
            id: PinId,
            _frame: &Frame,
            spec: &WindowSpec,
        ) -> Result<Box<dyn PinWindow>, String> {
            if *self.fail.borrow() {
                return Err("window creation failed (test)".into());
            }
            let mut l = self.log.borrow_mut();
            l.created.push((id, spec.rect, spec.focus));
            l.guards.push(spec.guard);
            l.alive.push(id);
            l.rects.insert(id, spec.rect);
            Ok(Box::new(FakeWindow {
                id,
                log: Rc::clone(&self.log),
                fail: Rc::clone(&self.fail),
                destroyed: false,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeWindows;
    use super::*;
    use crate::capture::{DesktopState, Monitor};
    use crate::frame::Ledger;

    fn frame(ledger: &std::sync::Arc<Ledger>, x: i32) -> Frame {
        Frame::solid(ledger, Rect::new(x, 0, 2, 2), [0, 0, 0, 255]).unwrap()
    }

    fn exact(frame: Frame, source: Source, focus: bool) -> NewPin {
        NewPin {
            frame,
            source,
            placement: Placement::Exact,
            focus,
            guard: None,
        }
    }

    /// A 1920×1080 primary with a 40-pixel taskbar at the bottom, and a 1280×1024 monitor left
    /// of it whose taskbar is at the top.
    fn desk() -> Desktop {
        Desktop {
            state: DesktopState::Available,
            virtual_rect: Some(Rect::new(-1280, 0, 3200, 1080)),
            monitors: vec![
                Monitor::new(Rect::new(0, 0, 1920, 1080), true)
                    .with_work(Rect::new(0, 0, 1920, 1040)),
                Monitor::new(Rect::new(-1280, 0, 1280, 1024), false)
                    .with_work(Rect::new(-1280, 40, 1280, 984)),
            ],
        }
    }

    #[test]
    fn ids_parse_and_print() {
        assert_eq!(PinId::parse("pin-12"), Some(PinId(12)));
        for bad in ["pin-", "pin-0", "pin-01", "p-1", "pin-1x", "pin--1"] {
            assert_eq!(PinId::parse(bad), None, "{bad}");
        }
        assert_eq!(PinId(7).to_string(), "pin-7");
    }

    #[test]
    fn lifecycle_close_reopen_destroy_and_quit() {
        let ledger = Ledger::new();
        let d = desk();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        let a = pins
            .create(&mut w, &d, exact(frame(&ledger, 0), Source::Capture, true))
            .unwrap();
        let b = pins
            .create(
                &mut w,
                &d,
                exact(frame(&ledger, 5), Source::Clipboard, false),
            )
            .unwrap();
        assert_eq!((a, b), (PinId(1), PinId(2)));
        assert_eq!(ledger.live(), (2, 32));
        assert!(w.log.borrow().created[0].2, "focus only when asked");
        assert!(!w.log.borrow().created[1].2);

        pins.close(a).unwrap();
        assert_eq!(w.log.borrow().destroyed, [a]);
        assert_eq!(pins.reopenable(), Some(a));
        assert_eq!(
            ledger.live(),
            (2, 32),
            "a closed pin keeps its picture for reopen"
        );
        assert_eq!(pins.close(a), Err(PinError::NotFound(a)));

        assert_eq!(
            pins.reopen(&mut w, false).unwrap(),
            a,
            "same id after reopen"
        );
        assert_eq!(pins.reopen(&mut w, false), Err(PinError::NothingToReopen));
        assert_eq!(pins.list(&d)["pins"].as_array().unwrap().len(), 2);

        pins.destroy(b).unwrap();
        assert_eq!(ledger.live(), (1, 16), "destroy frees the picture");
        assert_eq!(pins.reopenable(), None);

        let c = pins
            .create(&mut w, &d, exact(frame(&ledger, 9), Source::Capture, false))
            .unwrap();
        assert_eq!(c, PinId(3), "ids are never reused");
        pins.close(c).unwrap();
        pins.close(a).unwrap();
        assert_eq!(
            pins.reopenable(),
            Some(a),
            "the memory keeps the last closed pin"
        );
        assert_eq!(ledger.live(), (1, 16), "the older closed picture was freed");

        let e = pins
            .create(&mut w, &d, exact(frame(&ledger, 1), Source::Capture, false))
            .unwrap();
        assert_eq!(pins.clear(), 1);
        assert!(
            w.log.borrow().alive.is_empty(),
            "quit destroys every window"
        );
        assert_eq!(ledger.live(), (0, 0), "quit releases every picture");
        assert!(!pins.contains(e));
    }

    #[test]
    fn failed_windows_hand_the_picture_back_and_moves_are_tracked() {
        let ledger = Ledger::new();
        let d = desk();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        *w.fail.borrow_mut() = true;
        let (back, why) = pins
            .create(&mut w, &d, exact(frame(&ledger, 0), Source::Capture, true))
            .unwrap_err();
        assert!(why.contains("failed"));
        assert_eq!(
            back.rect(),
            Rect::new(0, 0, 2, 2),
            "nothing is lost (NOEUL-FR-025)"
        );
        *w.fail.borrow_mut() = false;
        let id = pins
            .create(&mut w, &d, exact(back, Source::Capture, true))
            .unwrap();
        assert_eq!(id, PinId(1), "a failed create uses no id");
        assert!(pins.nudge(id, -1, 0));
        assert!(pins.nudge(id, 0, 1));
        assert_eq!(pins.rect(id), Some(Rect::new(-1, 1, 2, 2)));
        assert_eq!(w.log.borrow().moved, [(id, -1, 0), (id, -1, 1)]);
        assert!(pins.moved(id, 300, 200));
        assert_eq!(pins.rect(id), Some(Rect::new(300, 200, 2, 2)));
        assert!(!pins.nudge(PinId(99), 1, 1));
        assert_eq!(ledger.live(), (1, 16), "moving copies nothing");
        let listed = pins.list(&d);
        assert_eq!(listed["pins"][0]["id"], "pin-1");
        assert_eq!(listed["pins"][0]["rect"]["x"], 300);
        assert_eq!(listed["pins"][0]["zoom"], 100);
        assert_eq!(listed["pins"][0]["on_screen"], true);
    }

    #[test]
    fn many_pins_move_zoom_hide_and_close_independently() {
        let ledger = Ledger::new();
        let d = desk();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        let ids: Vec<PinId> = (0..5)
            .map(|i| {
                let f =
                    Frame::solid(&ledger, Rect::new(i * 100, 100, 40, 30), [9, 9, 9, 255]).unwrap();
                pins.create(&mut w, &d, exact(f, Source::Capture, false))
                    .unwrap()
            })
            .collect();
        assert_eq!(pins.len(), 5);
        // Move the second, zoom the third around a point inside it, hide the fourth.
        assert!(pins.nudge(ids[1], 5, -3));
        assert!(pins.set_zoom(ids[2], 200, (210, 110)).unwrap());
        assert_eq!(
            pins.rect(ids[2]),
            Some(Rect::new(190, 90, 80, 60)),
            "zoomed around (210,110)"
        );
        assert!(pins.hide(ids[3]).unwrap());
        assert!(!pins.hide(ids[3]).unwrap(), "hiding twice changes nothing");
        assert_eq!(pins.hidden(), 1);
        // Close the first: every other window stays exactly as it was.
        pins.close(ids[0]).unwrap();
        assert_eq!(w.log.borrow().destroyed, [ids[0]]);
        assert_eq!(pins.rect(ids[1]), Some(Rect::new(105, 97, 40, 30)));
        assert_eq!(pins.info(ids[2]).unwrap().zoom, 200);
        assert!(!pins.info(ids[3]).unwrap().visible);
        assert_eq!(pins.rect(ids[4]), Some(Rect::new(400, 100, 40, 30)));
        assert_eq!(
            w.log.borrow().alive,
            ids[1..].to_vec(),
            "only the closed pin's window is gone"
        );
        assert!(pins.show(ids[3]).unwrap());
        assert_eq!(
            w.log.borrow().visible,
            [(ids[3], false), (ids[3], true)],
            "only the hidden pin's window was touched"
        );
        assert!(w.log.borrow().focused.is_empty(), "nothing took focus");
        // Zooming back to 100 % keeps the picture's pixels (the frame is never resampled).
        assert!(pins.set_zoom(ids[2], 100, (190, 90)).unwrap());
        assert_eq!(pins.rect(ids[2]), Some(Rect::new(190, 90, 40, 30)));
        assert!(!pins.set_zoom(ids[2], 100, (0, 0)).unwrap(), "no change");
        assert_eq!(
            ledger.live(),
            (5, 5 * 40 * 30 * 4),
            "the closed one is kept for reopen"
        );
        // A failed window update keeps the old zoom.
        *w.fail.borrow_mut() = true;
        assert!(matches!(
            pins.set_zoom(ids[4], 150, (0, 0)),
            Err(PinError::Window(_))
        ));
        assert_eq!(pins.info(ids[4]).unwrap().zoom, 100);
        assert_eq!(pins.hide(PinId(42)), Err(PinError::NotFound(PinId(42))));
    }

    #[test]
    fn a_huge_paste_fits_its_monitor_and_avoids_the_taskbar() {
        let ledger = Ledger::new();
        let d = desk();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        // A 4K picture pasted near the bottom right of the 1080p primary.
        let big = Frame::solid(&ledger, Rect::new(0, 0, 3840, 2160), [1, 2, 3, 255]).unwrap();
        let id = pins
            .create(
                &mut w,
                &d,
                NewPin {
                    frame: big,
                    source: Source::Clipboard,
                    placement: Placement::At(1900, 1000),
                    focus: true,
                    guard: None,
                },
            )
            .unwrap();
        let info = pins.info(id).unwrap();
        assert_eq!(info.zoom, 48, "zoomed out to fit the work area");
        assert_eq!(info.image, (3840, 2160), "the picture keeps every pixel");
        let r = info.rect;
        assert!(
            r.right() <= 1920 && r.bottom() <= 1040,
            "{r:?} inside the work area"
        );
        assert!(r.x >= 0 && r.y >= 0);
        assert_eq!(w.log.borrow().created[0].1, r, "the window is that size");
        // A small one near the left monitor's top lands below its top taskbar.
        let small = Frame::solid(&ledger, Rect::new(0, 0, 50, 50), [1, 2, 3, 255]).unwrap();
        let id2 = pins
            .create(
                &mut w,
                &d,
                NewPin {
                    frame: small,
                    source: Source::Clipboard,
                    placement: Placement::At(-600, 5),
                    focus: false,
                    guard: None,
                },
            )
            .unwrap();
        assert_eq!(pins.rect(id2), Some(Rect::new(-600, 40, 50, 50)));
        // A point off every monitor goes to the nearest one.
        assert_eq!(place_at(&d, 10, 10, (5000, 500)), (1910, 500, 100));
        // Without monitors the point is kept (nothing to fit).
        let none = Desktop {
            state: DesktopState::NoDisplay,
            virtual_rect: None,
            monitors: Vec::new(),
        };
        assert_eq!(place_at(&none, 10, 10, (5, 6)), (5, 6, 100));
    }

    #[test]
    fn a_monitor_disconnect_keeps_pins_in_place_until_they_are_brought_back() {
        let ledger = Ledger::new();
        let mut d = desk();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        let on_left = pins
            .create(
                &mut w,
                &d,
                exact(
                    Frame::solid(&ledger, Rect::new(-1000, 300, 800, 600), [0; 4]).unwrap(),
                    Source::Capture,
                    false,
                ),
            )
            .unwrap();
        let on_primary = pins
            .create(
                &mut w,
                &d,
                exact(
                    Frame::solid(&ledger, Rect::new(10, 10, 20, 20), [0; 4]).unwrap(),
                    Source::Capture,
                    false,
                ),
            )
            .unwrap();
        assert_eq!(pins.off_screen(&d), 0);
        // The left monitor is unplugged.
        d.monitors.truncate(1);
        d.virtual_rect = Some(Rect::new(0, 0, 1920, 1080));
        assert_eq!(pins.sync(), 0, "nothing moved by itself");
        assert_eq!(pins.off_screen(&d), 1);
        let l = pins.list(&d);
        assert_eq!(l["pins"][0]["on_screen"], false);
        assert_eq!(l["pins"][1]["on_screen"], true);
        assert_eq!(
            pins.rect(on_left),
            Some(Rect::new(-1000, 300, 800, 600)),
            "it keeps its place (NOEUL-FR-067)"
        );
        // The system moved the other window: the model follows.
        w.log.borrow_mut().rects.get_mut(&on_primary).unwrap().x = 50;
        assert_eq!(pins.sync(), 1);
        assert_eq!(pins.rect(on_primary).unwrap().x, 50);
        // "Bring pins on screen": only the lost one moves, into the work area.
        assert!(pins.bring_on_screen(on_left, &d).unwrap());
        assert!(!pins.bring_on_screen(on_primary, &d).unwrap());
        assert_eq!(pins.rect(on_left), Some(Rect::new(0, 300, 800, 600)));
        assert_eq!(pins.off_screen(&d), 0);
        // A larger pin brought onto a smaller monitor is zoomed out to fit.
        let big = pins
            .create(
                &mut w,
                &d,
                exact(
                    Frame::solid(&ledger, Rect::new(-4000, 0, 2560, 1440), [0; 4]).unwrap(),
                    Source::Capture,
                    false,
                ),
            )
            .unwrap();
        assert!(pins.bring_on_screen(big, &d).unwrap());
        let info = pins.info(big).unwrap();
        assert!(info.zoom < 100 && info.rect.width <= 1920 && info.rect.height <= 1040);
        assert_eq!((info.rect.x, info.rect.y), (0, 0));
        // While the desktop is locked nobody knows what is on screen.
        d.state = DesktopState::Locked;
        assert_eq!(pins.list(&d)["pins"][0]["on_screen"], Value::Null);
    }

    #[test]
    fn on_screen_needs_a_grabbable_part() {
        let d = desk();
        assert!(
            on_screen(&d, Rect::new(1890, 500, 100, 100)),
            "30 px wide on screen"
        );
        assert!(!on_screen(&d, Rect::new(1910, 500, 100, 100)), "only 10 px");
        assert!(
            on_screen(&d, Rect::new(1915, 500, 5, 5)),
            "a tiny pin is whole"
        );
        assert!(!on_screen(&d, Rect::new(-5000, 0, 100, 100)));
    }
}
