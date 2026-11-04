//! The pin engine's model: pins, their IDs, geometry, visibility, transforms, groups and
//! lifecycle (NOEUL-FR-060…NOEUL-FR-065, NOEUL-FR-067, NOEUL-FR-068, NOEUL-FR-025, NOEUL-FR-026;
//! ADR 0023 §4, ADR 0029, ADR 0044).
//!
//! A pin is a floating window that owns one [`Frame`] (its picture at 100 %). The registry gives
//! every pin a stable ID (`pin-<n>`, never reused while the instance runs), keeps the windows
//! behind the narrow [`PinWindow`]/[`PinWindows`] traits (the Win32 implementation is
//! `win::pinwnd`; tests use [`fake`]), and enforces the lifecycle:
//!
//! ```text
//!   create ──► open (shown ⇄ hidden, moved, zoomed, transformed) ──close──► closed (reopen memory: the last one)
//!                │                                             │            ──reopen──► open (same id)
//!                └──destroy──► gone ◄──────────────────────────┘ (a newer close replaces it; quit clears it)
//! ```
//!
//! - **Independent.** Each pin has its own ID, position, zoom, transforms, visibility, group and
//!   lifetime; an operation on one never touches another's window.
//! - **Geometry.** The picture's rectangle holds the shown picture's top-left corner; its shown
//!   size is the picture's size turned by its orientation, at the pin's zoom ([`crate::zoom`]) or
//!   as a thumbnail. The window is larger by the outline and shadow around the picture
//!   ([`crate::transform::View`]); everything here is about the picture's rectangle.
//!   [`Placement::Exact`] keeps a capture at its place and pixel size (NOEUL-FR-020);
//!   [`Placement::At`] puts a paste at a point inside the work area of the monitor there and
//!   zooms a picture larger than that work area out until it fits, so a huge paste never covers
//!   every screen (NOEUL-SEC-013).
//! - **Transforms** (Step 54, ADR 0044): rotation by 90° and flips ([`Orientation`], turning
//!   around the pin's centre), opacity (10–100 %, a constant alpha that renders nothing again),
//!   zoom by keys, the wheel or by dragging the border ([`Pins::resize`], aspect ratio kept),
//!   thumbnail mode and click-through. Every one is reversible and per pin. The picture and its
//!   annotations are never changed by a transform: the window shows the annotations drawn on the
//!   picture, then turned ([`shown`]); copy and save output exactly that at 100 % and opaque.
//! - **Groups** (Step 54): every pin belongs to a named group (new pins to the current one); a
//!   group is switched to (its pins shown, the others hidden), shown, hidden or closed at once.
//! - **Display changes.** Pins keep their place when monitors change, even off-screen
//!   (NOEUL-FR-067): [`Pins::sync`] only follows where the system really put a window, and
//!   [`Pins::bring_on_screen`] moves a pin that no monitor shows into the nearest work area,
//!   on request.
//! - **Memory.** Closing destroys the window but keeps the picture so "Reopen closed pin" can bring
//!   it back with its transforms (default memory: the last pin, NOEUL-FR-065); destroying frees it
//!   for good. Closing the capture overlay never closes pins. Quitting releases every picture.
//!   Nothing is written to disk, so pins do not survive a restart (NOEUL-SEC-005; opt-in backup is
//!   NOEUL-FR-066, N4).
//! - **Annotations** (Step 56). A pin keeps its picture unchanged and its annotations as vector
//!   objects in image pixels ([`crate::annotation`]); its window shows them flattened, and copy and
//!   save output them flattened ([`Pins::rendered`]). Editing a pin ([`Pins::set_editing`],
//!   [`Pins::commit_edit`]) replaces the objects and may crop the picture; a cancelled edit changes
//!   nothing. Closing and reopening keeps them.
//! - **Animated pictures** (NOEUL-FR-068): this build has no animated pins. A pin made from an
//!   animated GIF shows its first frame and says so ([`Pins::mark_animation`], `pin.list`), never
//!   silently.

use crate::annotation::{self, Object};
use crate::capture::{BASE_DPI, Desktop};
use crate::frame::{Frame, Rect};
use crate::transform::{self, Edge, Look, Orientation, View};
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
    /// What it shows: the picture's rectangle (the picture is scaled to its size), the look, the
    /// opacity and whether it lets clicks through.
    pub view: View,
    /// Give it keyboard focus (only when the user created it, NOEUL-FR-060).
    pub focus: bool,
    /// Ignore presses on it for this long after it appears, so the rest of the gesture that
    /// created it (the remaining clicks of a double-click) cannot move, close or zoom it
    /// (NOEUL_UX_SPEC §4.2 rule 10).
    pub guard: Option<Duration>,
}

impl WindowSpec {
    /// A window of `rect` with the default look, without focus or guard (tests and measurements).
    pub fn plain(rect: Rect) -> WindowSpec {
        WindowSpec {
            view: View::plain(rect),
            focus: false,
            guard: None,
        }
    }
}

/// One pin window. Implementations own an OS window and destroy it in [`PinWindow::destroy`]
/// (and on drop). No method activates the window except [`PinWindow::focus`].
pub trait PinWindow {
    /// Moves the window so the picture's top-left is at (`x`, `y`) (device pixels).
    fn set_position(&mut self, x: i32, y: i32);
    /// Shows `frame` scaled to `view.rect` (the picture's new position and size) with its
    /// outline, shadow, badge, opacity and click-through.
    fn present(&mut self, frame: &Frame, view: &View) -> Result<(), String>;
    /// Changes only the opacity (percent): nothing is rendered again.
    fn set_opacity(&mut self, opacity: u32);
    /// Hides or shows the window without changing which window has focus; a window shown again
    /// is above the other pins.
    fn set_visible(&mut self, visible: bool);
    /// Gives the window keyboard focus; `false` when the OS refused.
    fn focus(&mut self) -> bool;
    /// Where the picture really is (the system may move windows when monitors change).
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
    /// Annotations in image pixels (`(0, 0)` is the picture's top-left), kept editable (Step 56).
    pub annotations: Vec<Object>,
}

/// A pin's transforms (Step 54): how it is shown, each reversible, each its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transforms {
    pub orientation: Orientation,
    /// Percent, 10–100.
    pub opacity: u32,
    /// The thumbnail's size while the pin is a thumbnail (its zoom is kept for later).
    pub thumbnail: Option<(u32, u32)>,
    /// The pin lets the mouse through to the window below (NOEUL-FR-063).
    pub click_through: bool,
}

impl Default for Transforms {
    fn default() -> Transforms {
        Transforms {
            orientation: Orientation::IDENTITY,
            opacity: transform::OPACITY_FULL,
            thumbnail: None,
            click_through: false,
        }
    }
}

/// What [`Pins::info`] reports about one open pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinInfo {
    /// The picture's rectangle on screen: position and shown size.
    pub rect: Rect,
    /// The picture's size in pixels (100 %, as captured, before any rotation).
    pub image: (u32, u32),
    /// Percent; 100 = one image pixel per device pixel (kept while it is a thumbnail).
    pub zoom: u32,
    pub visible: bool,
    pub source: Source,
    /// How many annotations it carries (Step 56).
    pub annotations: usize,
    /// Whether its annotations are being edited (its window is hidden meanwhile).
    pub editing: bool,
    pub transforms: Transforms,
    /// Frames of the animated picture it was made from, of which it shows the first (Step 54).
    pub animation: Option<u32>,
}

struct Pin {
    /// The picture; its origin is the shown picture's top-left corner.
    frame: Frame,
    zoom: u32,
    visible: bool,
    window: Box<dyn PinWindow>,
    source: Source,
    created: SystemTime,
    /// In image pixels (Step 56).
    annotations: Vec<Object>,
    editing: bool,
    state: Transforms,
    group: String,
    animation: Option<u32>,
    /// The effective DPI of the monitor the pin is on (its outline, shadow and badge scale).
    dpi: u32,
}

impl Pin {
    /// The shown size at 100 %: the picture's size turned by its orientation.
    fn base(&self) -> (u32, u32) {
        self.state
            .orientation
            .size(self.frame.width(), self.frame.height())
    }

    fn rect(&self) -> Rect {
        let r = self.frame.rect();
        let (w, h) = self
            .state
            .thumbnail
            .unwrap_or_else(|| shown_size(self.base(), self.zoom));
        Rect::new(r.x, r.y, w, h)
    }

    fn view(&self, look: Look) -> View {
        View {
            rect: self.rect(),
            look,
            dpi: self.dpi,
            opacity: self.state.opacity,
            click_through: self.state.click_through,
            resizable: self.state.thumbnail.is_none(),
        }
    }

    /// Everything a change may touch, to undo it when the window refuses.
    fn saved(&self) -> (Transforms, u32, Rect, u32) {
        (self.state, self.zoom, self.frame.rect(), self.dpi)
    }

    fn restore(&mut self, s: (Transforms, u32, Rect, u32)) {
        self.state = s.0;
        self.zoom = s.1;
        self.frame.set_origin(s.2.x, s.2.y);
        self.dpi = s.3;
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pin")
            .field("rect", &self.rect())
            .field("zoom", &self.zoom)
            .field("visible", &self.visible)
            .field("source", &self.source)
            .field("state", &self.state)
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

fn shown_size(base: (u32, u32), zoom: u32) -> (u32, u32) {
    zoom::size(base.0, base.1, zoom)
}

/// Shows the pin's picture in its window as its state says.
fn present(p: &mut Pin, look: Look) -> Result<(), String> {
    let pic = shown(&p.frame, &p.annotations, p.state.orientation);
    let view = p.view(look);
    p.window.present(pic.as_ref().unwrap_or(&p.frame), &view)
}

/// The picture with its annotations drawn on it, when it has any (the picture itself never
/// changes).
pub fn render(frame: &Frame, annotations: &[Object]) -> Option<Frame> {
    if annotations.is_empty() {
        return None;
    }
    let r = frame.rect();
    annotation::flatten(frame, &annotation::translate_all(annotations, r.x, r.y), r)
}

/// What a pin shows and what its copy and save output: the picture, its annotations drawn on it
/// (every redaction applied before anything else, ADR 0041), then turned by `orientation`. `None`
/// when that is the picture itself. The picture and the annotations stay as they are, so the
/// orientation can always be undone; a redaction stays irreversible in this output.
pub fn shown(frame: &Frame, annotations: &[Object], orientation: Orientation) -> Option<Frame> {
    let flat = render(frame, annotations);
    if orientation.is_identity() {
        return flat;
    }
    orientation.apply(flat.as_ref().unwrap_or(frame))
}

#[derive(Debug)]
struct Closed {
    id: PinId,
    frame: Frame,
    zoom: u32,
    source: Source,
    created: SystemTime,
    annotations: Vec<Object>,
    state: Transforms,
    group: String,
    animation: Option<u32>,
    dpi: u32,
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

/// The DPI of the monitor nearest the centre of `r` (100 % without monitors).
pub fn dpi_at(desktop: &Desktop, r: Rect) -> u32 {
    let (cx, cy) = center(r);
    desktop.monitor_near(cx, cy).map_or(BASE_DPI, |m| m.dpi)
}

fn center(r: Rect) -> (i32, i32) {
    (
        i32::try_from(i64::from(r.x) + i64::from(r.width / 2)).unwrap_or(r.x),
        i32::try_from(i64::from(r.y) + i64::from(r.height / 2)).unwrap_or(r.y),
    )
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

/// The group every pin starts in.
pub const DEFAULT_GROUP: &str = "default";

/// A valid group name (trimmed): 1–32 letters, digits, spaces, `-`, `_` or `.`, starting with a
/// letter or digit; `None` otherwise.
pub fn group_name(s: &str) -> Option<String> {
    let t = s.trim();
    let n = t.chars().count();
    let ok = (1..=32).contains(&n)
        && t.chars().next().is_some_and(char::is_alphanumeric)
        && t.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    ok.then(|| t.to_string())
}

/// One group of [`Pins::groups`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub pins: usize,
    pub hidden: usize,
}

/// The pins of one instance.
#[derive(Debug)]
pub struct Pins {
    open: BTreeMap<PinId, Pin>,
    closed: Option<Closed>,
    next: u64,
    look: Look,
    group: String,
}

impl Default for Pins {
    fn default() -> Pins {
        Pins {
            open: BTreeMap::new(),
            closed: None,
            next: 0,
            look: Look::default(),
            group: DEFAULT_GROUP.to_string(),
        }
    }
}

impl Pins {
    pub fn new() -> Pins {
        Pins::default()
    }

    /// Creates a pin as `new` says, in the current group. On failure the frame is handed back
    /// (NOEUL-FR-025) and no ID is used up.
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
            annotations,
        } = new;
        let (frame, zoom) = match placement {
            Placement::Exact => (frame, zoom::FULL),
            Placement::At(x, y) => {
                let (x, y, z) = place_at(desktop, frame.width(), frame.height(), (x, y));
                (frame.moved_to(x, y), z)
            }
        };
        let id = PinId(self.next + 1);
        let state = Transforms::default();
        let (w, h) = zoom::size(frame.width(), frame.height(), zoom);
        let rect = Rect::new(frame.rect().x, frame.rect().y, w, h);
        let dpi = dpi_at(desktop, rect);
        let spec = WindowSpec {
            view: View {
                rect,
                look: self.look,
                dpi,
                opacity: state.opacity,
                click_through: false,
                resizable: true,
            },
            focus,
            guard,
        };
        let pic = render(&frame, &annotations);
        match windows.create(id, pic.as_ref().unwrap_or(&frame), &spec) {
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
                        annotations,
                        editing: false,
                        state,
                        group: self.group.clone(),
                        animation: None,
                        dpi,
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

    /// How many open pins let the mouse through (NOEUL-FR-063).
    pub fn click_through(&self) -> usize {
        self.open.values().filter(|p| p.state.click_through).count()
    }

    /// How many open pins are thumbnails.
    pub fn thumbnails(&self) -> usize {
        self.open
            .values()
            .filter(|p| p.state.thumbnail.is_some())
            .count()
    }

    /// How many open pins no monitor shows (see [`on_screen`]); 0 while the desktop is not
    /// available (nobody knows then).
    pub fn off_screen(&self, desktop: &Desktop) -> usize {
        if desktop.state != crate::capture::DesktopState::Available {
            return 0;
        }
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
            annotations: p.annotations.len(),
            editing: p.editing,
            transforms: p.state,
            animation: p.animation,
        })
    }

    /// The picture's rectangle of an open pin.
    pub fn rect(&self, id: PinId) -> Option<Rect> {
        self.open.get(&id).map(Pin::rect)
    }

    /// What the window of an open pin shows besides its picture (tests and the runtime).
    pub fn view(&self, id: PinId) -> Option<View> {
        self.open.get(&id).map(|p| p.view(self.look))
    }

    /// The picture of an open pin at 100 %, as captured (the annotation editor works on it).
    pub fn picture(&self, id: PinId) -> Option<&Frame> {
        self.open.get(&id).map(|p| &p.frame)
    }

    /// The annotations of an open pin, in image pixels (Step 56).
    pub fn annotations(&self, id: PinId) -> Option<&[Object]> {
        self.open.get(&id).map(|p| p.annotations.as_slice())
    }

    /// The picture of an open pin as shown — its annotations drawn on it, then turned by its
    /// orientation — when that differs from the picture itself (copy and save output this, at
    /// 100 % and opaque; [`Pins::picture`] otherwise).
    pub fn rendered(&self, id: PinId) -> Option<Frame> {
        self.open
            .get(&id)
            .and_then(|p| shown(&p.frame, &p.annotations, p.state.orientation))
    }

    /// Hides a pin's window while its annotations are edited, and shows it again (if it was
    /// shown) when the edit ends; nothing else changes.
    pub fn set_editing(&mut self, id: PinId, editing: bool) -> Result<(), PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        if p.editing != editing {
            p.editing = editing;
            if p.visible {
                p.window.set_visible(!editing);
            }
        }
        Ok(())
    }

    /// Commits an edit of a pin (Step 56): `objects` are the annotations in the editor's pixels
    /// (the picture at 100 %, as captured, at the pin's top-left corner), `keep` the part of the
    /// picture the pin keeps (a crop; the whole picture when unchanged). A cropped pin shows the
    /// kept part where it was shown, at its zoom and orientation (a thumbnail stays at its
    /// corner). On a window error nothing changes.
    pub fn commit_edit(
        &mut self,
        id: PinId,
        objects: &[Object],
        keep: Rect,
    ) -> Result<(), PinError> {
        let look = self.look;
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let r = p.frame.rect();
        let keep = crate::overlay::intersection(keep, r).unwrap_or(r);
        let annotations = annotation::inside(
            &annotation::translate_all(objects, -keep.x, -keep.y),
            Rect::new(0, 0, keep.width, keep.height),
        );
        let cropped = if keep == r {
            None
        } else {
            let part = Rect::new(keep.x - r.x, keep.y - r.y, keep.width, keep.height);
            let at = p.state.orientation.map_rect(part, r.width, r.height);
            let (x, y) = if p.state.thumbnail.is_some() {
                (r.x, r.y)
            } else {
                let off = |o: i32, d: i32| {
                    let v = i64::from(o) + i64::from(d) * i64::from(p.zoom) / 100;
                    i32::try_from(v).unwrap_or(o)
                };
                (off(r.x, at.x), off(r.y, at.y))
            };
            match p.frame.crop(keep) {
                Some(f) => Some(f.moved_to(x, y)),
                None => return Err(PinError::Window("the crop is outside the picture".into())),
            }
        };
        let frame = cropped.as_ref().unwrap_or(&p.frame);
        let mut state = p.state;
        if state.thumbnail.is_some() {
            let (w, h) = state.orientation.size(frame.width(), frame.height());
            state.thumbnail = Some(transform::thumbnail_size(w, h, p.dpi));
        }
        let fr = frame.rect();
        let (w, h) = state.thumbnail.unwrap_or_else(|| {
            shown_size(
                state.orientation.size(frame.width(), frame.height()),
                p.zoom,
            )
        });
        let view = View {
            rect: Rect::new(fr.x, fr.y, w, h),
            look,
            dpi: p.dpi,
            opacity: state.opacity,
            click_through: state.click_through,
            resizable: state.thumbnail.is_none(),
        };
        let pic = shown(frame, &annotations, state.orientation);
        p.window
            .present(pic.as_ref().unwrap_or(frame), &view)
            .map_err(PinError::Window)?;
        if let Some(f) = cropped {
            p.frame = f;
        }
        p.state = state;
        p.annotations = annotations;
        Ok(())
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
            annotations: pin.annotations,
            state: pin.state,
            group: pin.group,
            animation: pin.animation,
            dpi: pin.dpi,
        });
        Ok(())
    }

    /// Removes a pin for good (`pin.destroy`): window and picture are freed.
    pub fn destroy(&mut self, id: PinId) -> Result<(), PinError> {
        let mut pin = self.open.remove(&id).ok_or(PinError::NotFound(id))?;
        pin.window.destroy();
        Ok(())
    }

    /// Reopens the last closed pin with its old ID, position, zoom, transforms and group.
    pub fn reopen(&mut self, windows: &mut dyn PinWindows, focus: bool) -> Result<PinId, PinError> {
        let c = self.closed.take().ok_or(PinError::NothingToReopen)?;
        let base = c.state.orientation.size(c.frame.width(), c.frame.height());
        let (w, h) = c
            .state
            .thumbnail
            .unwrap_or_else(|| shown_size(base, c.zoom));
        let fr = c.frame.rect();
        let spec = WindowSpec {
            view: View {
                rect: Rect::new(fr.x, fr.y, w, h),
                look: self.look,
                dpi: c.dpi,
                opacity: c.state.opacity,
                click_through: c.state.click_through,
                resizable: c.state.thumbnail.is_none(),
            },
            focus,
            guard: None,
        };
        let pic = shown(&c.frame, &c.annotations, c.state.orientation);
        match windows.create(c.id, pic.as_ref().unwrap_or(&c.frame), &spec) {
            Ok(window) => {
                drop(pic);
                self.open.insert(
                    c.id,
                    Pin {
                        frame: c.frame,
                        zoom: c.zoom,
                        visible: true,
                        window,
                        source: c.source,
                        created: c.created,
                        annotations: c.annotations,
                        editing: false,
                        state: c.state,
                        group: c.group,
                        animation: c.animation,
                        dpi: c.dpi,
                    },
                );
                Ok(c.id)
            }
            Err(e) => {
                drop(pic);
                self.closed = Some(c);
                Err(PinError::Window(e))
            }
        }
    }

    /// Hides a pin; `Ok(false)` when it was hidden already. A hidden pin renders nothing and
    /// receives no input: it costs no CPU (NOEUL-NFR-007).
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

    /// Applies `f` to a pin and shows the result; when `f` changed nothing, `Ok(false)`; when the
    /// window refuses, the pin keeps its old state and position.
    fn change(&mut self, id: PinId, f: impl FnOnce(&mut Pin) -> bool) -> Result<bool, PinError> {
        let look = self.look;
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let before = p.saved();
        if !f(p) {
            return Ok(false);
        }
        if let Err(e) = present(p, look) {
            p.restore(before);
            return Err(PinError::Window(e));
        }
        Ok(true)
    }

    /// Zooms a pin to `zoom` percent around the screen point `anchor`. `Ok(false)` when nothing
    /// changed (also for a thumbnail, whose zoom waits); on a window error the pin keeps its old
    /// zoom.
    pub fn set_zoom(&mut self, id: PinId, zoom: u32, anchor: (i32, i32)) -> Result<bool, PinError> {
        self.change(id, |p| {
            if p.state.thumbnail.is_some() {
                return false;
            }
            let zoom = zoom.clamp(zoom::MIN, zoom::MAX);
            let (w, h) = shown_size(p.base(), zoom);
            if zoom == p.zoom || !zoom::allowed(w, h) {
                return false;
            }
            let r = p.frame.rect();
            let (x, y) = zoom::around((r.x, r.y), anchor, p.zoom, zoom);
            p.frame.set_origin(x, y);
            p.zoom = zoom;
            true
        })
    }

    /// Resizes a pin by dragging `edge` (Step 54, NOEUL-FR-061): `start` is the picture's
    /// rectangle when the drag began and `delta` how far the pointer moved since, in device
    /// pixels. The aspect ratio is kept ([`transform::resize`]). `Ok(false)` for a thumbnail or
    /// when nothing changed.
    pub fn resize(
        &mut self,
        id: PinId,
        edge: Edge,
        start: Rect,
        delta: (i32, i32),
    ) -> Result<bool, PinError> {
        self.change(id, |p| {
            if p.state.thumbnail.is_some() {
                return false;
            }
            let (z, r) = transform::resize(start, p.base(), edge, delta);
            let o = p.frame.rect();
            if z == p.zoom && (r.x, r.y) == (o.x, o.y) {
                return false;
            }
            p.zoom = z;
            p.frame.set_origin(r.x, r.y);
            true
        })
    }

    /// Turns or flips a pin (`f` maps its orientation to the new one) around its centre, so the
    /// inverse brings it back to the same pixels at the same place. `Ok(false)` when unchanged.
    pub fn orient(
        &mut self,
        id: PinId,
        f: impl FnOnce(Orientation) -> Orientation,
    ) -> Result<bool, PinError> {
        self.change(id, |p| {
            let o = f(p.state.orientation);
            if o == p.state.orientation {
                return false;
            }
            let before = p.rect();
            p.state.orientation = o;
            if p.state.thumbnail.is_some() {
                let (w, h) = p.base();
                p.state.thumbnail = Some(transform::thumbnail_size(w, h, p.dpi));
            }
            let after = p.rect();
            let (cx, cy) = (
                i64::from(before.x) + i64::from(before.width / 2),
                i64::from(before.y) + i64::from(before.height / 2),
            );
            p.frame.set_origin(
                i32::try_from(cx - i64::from(after.width / 2)).unwrap_or(before.x),
                i32::try_from(cy - i64::from(after.height / 2)).unwrap_or(before.y),
            );
            true
        })
    }

    /// Sets a pin's opacity (percent, kept within 10–100 %) without rendering it again.
    /// `Ok(false)` when unchanged.
    pub fn set_opacity(&mut self, id: PinId, opacity: u32) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let o = transform::clamp_opacity(opacity);
        if o == p.state.opacity {
            return Ok(false);
        }
        p.window.set_opacity(o);
        p.state.opacity = o;
        Ok(true)
    }

    /// Turns thumbnail mode on or off: the whole picture at most 96 × 96 logical pixels at the
    /// pin's top-left, its zoom kept for when it ends (NOEUL-FR-062). `Ok(false)` when unchanged.
    pub fn set_thumbnail(&mut self, id: PinId, on: bool) -> Result<bool, PinError> {
        self.change(id, |p| {
            if on == p.state.thumbnail.is_some() {
                return false;
            }
            p.state.thumbnail = if on {
                let (w, h) = p.base();
                Some(transform::thumbnail_size(w, h, p.dpi))
            } else {
                None
            };
            true
        })
    }

    /// Lets the mouse through a pin, or not (NOEUL-FR-063); the window shows the badge meanwhile.
    /// The caller makes sure a recovery path exists. `Ok(false)` when unchanged.
    pub fn set_click_through(&mut self, id: PinId, on: bool) -> Result<bool, PinError> {
        self.change(id, |p| {
            if on == p.state.click_through {
                return false;
            }
            p.state.click_through = on;
            true
        })
    }

    /// The monitor under a pin changed DPI (it was dragged across, or a display changed): its
    /// outline, shadow and badge are drawn for the new DPI, its picture keeps its pixel size.
    pub fn set_dpi(&mut self, id: PinId, dpi: u32) -> Result<bool, PinError> {
        self.change(id, |p| {
            if p.dpi == dpi || dpi == 0 {
                return false;
            }
            p.dpi = dpi;
            if p.state.thumbnail.is_some() {
                let (w, h) = p.base();
                p.state.thumbnail = Some(transform::thumbnail_size(w, h, dpi));
            }
            true
        })
    }

    /// The outline and shadow every pin draws (`[pins] border` and `shadow`); every open pin is
    /// shown again with them. Returns how many windows refused (they keep their old look).
    pub fn set_look(&mut self, look: Look) -> usize {
        if look == self.look {
            return 0;
        }
        self.look = look;
        self.open
            .values_mut()
            .map(|p| present(p, look))
            .filter(Result::is_err)
            .count()
    }

    pub fn look(&self) -> Look {
        self.look
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
        let Some(r) = self.rect(id) else {
            return false;
        };
        self.move_to(id, r.x.saturating_add(dx), r.y.saturating_add(dy))
    }

    /// Puts a pin's picture at exactly (`x`, `y`) (precise placement, `noeul pin set --at`).
    pub fn move_to(&mut self, id: PinId, x: i32, y: i32) -> bool {
        let Some(p) = self.open.get_mut(&id) else {
            return false;
        };
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
        let r = self.rect(id).ok_or(PinError::NotFound(id))?;
        if on_screen(desktop, r) || desktop.monitors.is_empty() {
            return Ok(false);
        }
        let (cx, cy) = center(r);
        let Some(m) = desktop.monitor_near(cx, cy).copied() else {
            return Ok(false);
        };
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        let (bw, bh) = p.base();
        let fit = zoom::fit(bw, bh, m.work.width, m.work.height);
        let z = if p.state.thumbnail.is_some() {
            p.zoom
        } else {
            p.zoom.min(fit)
        };
        let (w, h) = p.state.thumbnail.unwrap_or_else(|| shown_size((bw, bh), z));
        let (x, y) = clamp_into((r.x, r.y), (w, h), m.work);
        if z == p.zoom && m.dpi == p.dpi {
            p.window.set_position(x, y);
            p.frame.set_origin(x, y);
            return Ok(true);
        }
        self.change(id, |p| {
            p.zoom = z;
            p.dpi = m.dpi;
            p.frame.set_origin(x, y);
            true
        })
        .map(|_| true)
    }

    pub fn focus(&mut self, id: PinId) -> bool {
        self.open.get_mut(&id).is_some_and(|p| p.window.focus())
    }

    /// Notes that an open pin was made from an animated picture of `frames` frames, of which it
    /// shows only the first (NOEUL-FR-068: never flattened silently).
    pub fn mark_animation(&mut self, id: PinId, frames: u32) -> bool {
        match self.open.get_mut(&id) {
            Some(p) if frames > 1 => {
                p.animation = Some(frames);
                true
            }
            _ => false,
        }
    }

    /// The group of an open pin.
    pub fn group(&self, id: PinId) -> Option<&str> {
        self.open.get(&id).map(|p| p.group.as_str())
    }

    /// The group new pins go to.
    pub fn current_group(&self) -> &str {
        &self.group
    }

    /// Moves a pin into the group `name` (validated by the caller). `Ok(false)` when it is in it.
    pub fn set_group(&mut self, id: PinId, name: &str) -> Result<bool, PinError> {
        let p = self.open.get_mut(&id).ok_or(PinError::NotFound(id))?;
        if p.group == name {
            return Ok(false);
        }
        p.group = name.to_string();
        Ok(true)
    }

    /// The open pins of group `name`.
    pub fn in_group(&self, name: &str) -> Vec<PinId> {
        self.open
            .iter()
            .filter(|(_, p)| p.group == name)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Every group with open pins, and the current group even when it is empty: the default
    /// group first, then by name.
    pub fn groups(&self) -> Vec<Group> {
        let mut by: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        by.insert(self.group.as_str(), (0, 0));
        for p in self.open.values() {
            let e = by.entry(p.group.as_str()).or_insert((0, 0));
            e.0 += 1;
            if !p.visible {
                e.1 += 1;
            }
        }
        let mut out: Vec<Group> = by
            .into_iter()
            .map(|(name, (pins, hidden))| Group {
                name: name.to_string(),
                pins,
                hidden,
            })
            .collect();
        out.sort_by_key(|g| g.name != DEFAULT_GROUP);
        out
    }

    /// A name for a new group: `group-2`, `group-3`, … (the first one no pin uses).
    pub fn new_group_name(&self) -> String {
        let used: Vec<String> = self.groups().into_iter().map(|g| g.name).collect();
        (2..)
            .map(|n| format!("group-{n}"))
            .find(|n| !used.contains(n))
            .unwrap_or_else(|| "group-x".into())
    }

    /// Switches to group `name` (validated by the caller): its pins are shown, every other pin is
    /// hidden, and new pins go to it. Returns the pins shown and hidden by the switch.
    pub fn switch_group(&mut self, name: &str) -> (Vec<PinId>, Vec<PinId>) {
        self.group = name.to_string();
        let (mut shown, mut hidden) = (Vec::new(), Vec::new());
        for id in self.ids() {
            let mine = self.group(id) == Some(name);
            let changed = if mine { self.show(id) } else { self.hide(id) };
            if changed == Ok(true) {
                if mine {
                    shown.push(id);
                } else {
                    hidden.push(id);
                }
            }
        }
        (shown, hidden)
    }

    /// Another shown pin under the screen point `at` (the newest when several overlap): where a
    /// pin dropped with `Ctrl` joins (Step 54).
    pub fn pin_under(&self, not: PinId, at: (i32, i32)) -> Option<PinId> {
        self.open
            .iter()
            .rev()
            .filter(|(id, p)| **id != not && p.visible)
            .find(|(_, p)| {
                let r = p.rect();
                i64::from(at.0) >= i64::from(r.x)
                    && i64::from(at.0) < r.right()
                    && i64::from(at.1) >= i64::from(r.y)
                    && i64::from(at.1) < r.bottom()
            })
            .map(|(id, _)| *id)
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

    /// One pin of `pin.list`.
    fn entry(&self, desktop: &Desktop, id: PinId, p: &Pin) -> Value {
        let known = desktop.state == crate::capture::DesktopState::Available;
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
            "annotations": p.annotations.len(),
            "editing": p.editing,
            "orientation": p.state.orientation.to_json(),
            "opacity": p.state.opacity,
            "thumbnail": p.state.thumbnail.is_some(),
            "click_through": p.state.click_through,
            "group": p.group,
            "animation": p.animation.map(|n| json!({"frames": n, "shown": "first_frame"})),
        })
    }

    /// One open pin as `pin.list` reports it.
    pub fn entry_of(&self, desktop: &Desktop, id: PinId) -> Option<Value> {
        self.open.get(&id).map(|p| self.entry(desktop, id, p))
    }

    /// The pin list (`pin.list`): IDs, geometry, zoom, transforms, visibility, group and origin
    /// only, never pixels (NOEUL-SEC-008). `on_screen` is `null` while the desktop is not
    /// available.
    pub fn list(&self, desktop: &Desktop) -> Value {
        json!({
            "pins": self.open.iter().map(|(id, p)| self.entry(desktop, *id, p)).collect::<Vec<_>>(),
            "reopenable": self.reopenable().map(|id| id.to_string()),
            "group": self.group,
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
        /// `(id, picture rect, focus)` of every created window.
        pub created: Vec<(PinId, Rect, bool)>,
        pub guards: Vec<Option<Duration>>,
        pub destroyed: Vec<PinId>,
        pub moved: Vec<(PinId, i32, i32)>,
        pub presented: Vec<(PinId, Rect)>,
        /// The view of every create and present, in order.
        pub views: Vec<(PinId, View)>,
        /// The size of the picture handed over with every create and present.
        pub sizes: Vec<(PinId, u32, u32)>,
        pub opacity: Vec<(PinId, u32)>,
        pub visible: Vec<(PinId, bool)>,
        pub focused: Vec<PinId>,
        pub alive: Vec<PinId>,
        /// Where each live window's picture is (tests move one to stand for the system moving it).
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
        fn present(&mut self, frame: &Frame, view: &View) -> Result<(), String> {
            if *self.fail.borrow() {
                return Err("window update failed (test)".into());
            }
            let mut l = self.log.borrow_mut();
            l.presented.push((self.id, view.rect));
            l.views.push((self.id, *view));
            l.sizes.push((self.id, frame.width(), frame.height()));
            l.rects.insert(self.id, view.rect);
            Ok(())
        }
        fn set_opacity(&mut self, opacity: u32) {
            self.log.borrow_mut().opacity.push((self.id, opacity));
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
            frame: &Frame,
            spec: &WindowSpec,
        ) -> Result<Box<dyn PinWindow>, String> {
            if *self.fail.borrow() {
                return Err("window creation failed (test)".into());
            }
            let mut l = self.log.borrow_mut();
            l.created.push((id, spec.view.rect, spec.focus));
            l.views.push((id, spec.view));
            l.sizes.push((id, frame.width(), frame.height()));
            l.guards.push(spec.guard);
            l.alive.push(id);
            l.rects.insert(id, spec.view.rect);
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
mod tests;
