//! The region-selection overlay's model: its state machine, geometry, DPI math and pixels
//! (NOEUL-FR-010…014, NOEUL-FR-042, NOEUL-FR-090, NOEUL-NFR-012; NOEUL_UX_SPEC §4; ADR 0026).
//!
//! Everything here is platform-independent and runs headless: [`Overlay`] is fed [`Input`]s in
//! virtual-desktop device pixels and answers with an [`Outcome`]; [`Overlay::scene`] says what
//! the overlay shows, [`dirty`] which pixels changed between two scenes, and [`compose`] writes
//! those pixels of a [`Scene`] over the frozen picture into a back buffer. The Win32 window
//! (`win::overlay`) only turns messages into inputs, draws the labels' text and copies the back
//! buffer to the screen; tests drive the same code with synthetic monitor layouts.
//!
//! States (UX spec §4.1, the part Step 50 implements):
//!
//! ```text
//!   Hover ──left press──► Pressed ──moved beyond the drag threshold──► Dragging ──release──► Selected
//!     ▲                     │ (released in place: a click; nothing yet)                     │
//!     │                     ▼                                                               │
//!     └── right-click outside the selection ◄── Selected ──press inside/on a handle──► Pressed
//!                                                  ▲         ──beyond threshold──► Moving / Resizing
//!                                                  └──────────────── release ─────────────────┘
//!   Esc (any state), right-click without a selection, another program activated → Cancel
//!   Enter (`capture.pin`) with a selection → Pin(rect)
//! ```
//!
//! A press that waits (the pointer pauses with the button held) stays `Pressed` for as long as
//! it likes: the drag starts from the press point as soon as the pointer moves beyond the
//! user's drag threshold, whatever the pause. No timer exists (NOEUL-NFR-001).
//!
//! Lengths of Noeul's own drawing are *logical* pixels scaled by the DPI of the monitor they are
//! drawn on (handles 7, handle zones 9, border band 3, small-selection limit 27); every
//! selection, readout and pin rectangle is in *device* pixels (NOEUL-FR-090).

use crate::capture::{Monitor, scale_px};
use crate::frame::{Frame, Rect};

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Modifier keys held during a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
    };
    pub const SHIFT: Mods = Mods {
        ctrl: false,
        shift: true,
        alt: false,
    };
}

/// The user's Windows pointer settings, read each time an overlay opens, never Noeul's own
/// constants (NOEUL-NFR-012, UX spec §4.2 rule 13). Defaults are Windows' defaults (*documented*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemValues {
    /// `SM_CXDRAG` × `SM_CYDRAG`: the drag rectangle centered on a press.
    pub drag: (u32, u32),
    /// `GetDoubleClickTime` in ms (used by the double-click gesture of Step 51).
    pub double_click_ms: u32,
    /// `SM_CXDOUBLECLK` × `SM_CYDOUBLECLK` (Step 51).
    pub double_click: (u32, u32),
}

impl Default for SystemValues {
    fn default() -> SystemValues {
        SystemValues {
            drag: (4, 4),
            double_click_ms: 500,
            double_click: (4, 4),
        }
    }
}

/// A direction of a keyboard step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// The overlay actions this build implements (context `capture-overlay`, UX spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `capture.pin` (`Enter`): pin the confirmed selection.
    Pin,
    /// `capture.cancel` (`Escape`).
    Cancel,
    /// `selection.full` (`Ctrl+A`): the monitor under the pointer, again all monitors.
    Full,
    /// `selection.last` (`R`): the rectangle of the last successful capture.
    Last,
    /// `selection.move-*` (arrows): one device pixel.
    Move(Dir),
    /// `selection.grow-*` (`Ctrl`+arrows): that edge outwards by one device pixel.
    Grow(Dir),
    /// `selection.shrink-*` (`Shift`+arrows): from the opposite edge by one device pixel.
    Shrink(Dir),
}

/// The action IDs of [`Action`], in manifest order.
pub const ACTION_IDS: &[&str] = &[
    "capture.pin",
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
];

impl Action {
    pub fn from_id(id: &str) -> Option<Action> {
        let dir = |s: &str| match s {
            "up" => Some(Dir::Up),
            "down" => Some(Dir::Down),
            "left" => Some(Dir::Left),
            "right" => Some(Dir::Right),
            _ => None,
        };
        Some(match id {
            "capture.pin" => Action::Pin,
            "capture.cancel" => Action::Cancel,
            "selection.full" => Action::Full,
            "selection.last" => Action::Last,
            _ => {
                let rest = id.strip_prefix("selection.")?;
                let (kind, d) = rest.split_once('-')?;
                let d = dir(d)?;
                match kind {
                    "move" => Action::Move(d),
                    "grow" => Action::Grow(d),
                    "shrink" => Action::Shrink(d),
                    _ => return None,
                }
            }
        })
    }
}

/// One input, in virtual-desktop device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Move {
        x: i32,
        y: i32,
        mods: Mods,
    },
    Press {
        button: Button,
        x: i32,
        y: i32,
        mods: Mods,
        /// The message time in ms (wraps); the double-click gesture of Step 51 uses it.
        time_ms: u32,
    },
    Release {
        button: Button,
        x: i32,
        y: i32,
    },
    /// A key press matched to an overlay action (the `capture-overlay` bindings).
    Action(Action),
    /// The reserved navigation key `Escape` (`nav.cancel`), `Alt+F4` or a close request: always
    /// leaves the overlay, also when its shortcuts are off (no trapped UI, NOEUL-SEC-013).
    Leave,
    /// Another program's window became active (NOEUL-FR-013).
    FocusLost,
    /// The pointer capture was taken away during a drag.
    CaptureLost,
}

/// What the owner of the overlay must do after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond redrawing what changed.
    Stay,
    /// End the capture without pinning, copying or saving anything (NOEUL-FR-012).
    Cancel,
    /// Hand this confirmed selection to the pin (`capture.pin`). The overlay stays `Selected`
    /// until the owner closes it, so a failed pin keeps the work (NOEUL-FR-025).
    Pin(Rect),
}

/// Which edges a handle or the border band moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Where a point lies relative to a confirmed selection (UX spec §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Outside,
    Interior,
    /// A handle zone or the border band.
    Edge(Edges),
}

/// The overlay's state, as reported by `noeul status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Hover,
    Pressed,
    Dragging,
    Selected,
    Moving,
    Resizing,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Hover => "hover",
            Phase::Pressed => "pressed",
            Phase::Dragging => "dragging",
            Phase::Selected => "selected",
            Phase::Moving => "moving",
            Phase::Resizing => "resizing",
        }
    }
}

/// The pointer shape the overlay asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Cross,
    Move,
    /// Left or right edge.
    SizeWe,
    /// Top or bottom edge.
    SizeNs,
    /// Top-left or bottom-right corner.
    SizeNwse,
    /// Top-right or bottom-left corner.
    SizeNesw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hover,
    /// The left button is down and has not moved beyond the drag threshold yet.
    Pressed {
        at: (i32, i32),
        zone: Zone,
    },
    Dragging {
        anchor: (i32, i32),
        to: (i32, i32),
        square: bool,
        before: Option<Rect>,
    },
    Selected,
    Moving {
        grab: (i32, i32),
        start: Rect,
    },
    Resizing {
        edges: Edges,
        grab: (i32, i32),
        start: Rect,
    },
}

/// The overlay of one capture session. See the module documentation.
#[derive(Debug, Clone)]
pub struct Overlay {
    picture: Rect,
    monitors: Vec<Monitor>,
    sys: SystemValues,
    state: State,
    selection: Option<Rect>,
    pointer: (i32, i32),
    shift: bool,
    last: Option<Rect>,
    help: Option<Vec<String>>,
    help_monitor: usize,
    banner: Option<String>,
}

impl Overlay {
    /// An overlay over `picture` (the frozen picture's rectangle, the virtual desktop) with its
    /// monitors. `pointer` is where the pointer is when it opens (it may never move before the
    /// first press); `last` is the rectangle of the last successful capture (`selection.last`);
    /// `help` the first-run keyboard help lines, if shown.
    pub fn new(
        picture: Rect,
        monitors: Vec<Monitor>,
        sys: SystemValues,
        pointer: (i32, i32),
        last: Option<Rect>,
        help: Option<Vec<String>>,
    ) -> Overlay {
        let mut o = Overlay {
            picture,
            monitors,
            sys,
            state: State::Hover,
            selection: None,
            pointer: (picture.x, picture.y),
            shift: false,
            last,
            help,
            help_monitor: 0,
            banner: None,
        };
        o.pointer = o.clamp(pointer);
        o.help_monitor = o.monitor_index(o.pointer);
        o
    }

    pub fn picture(&self) -> Rect {
        self.picture
    }

    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    pub fn phase(&self) -> Phase {
        match self.state {
            State::Hover => Phase::Hover,
            State::Pressed { .. } => Phase::Pressed,
            State::Dragging { .. } => Phase::Dragging,
            State::Selected => Phase::Selected,
            State::Moving { .. } => Phase::Moving,
            State::Resizing { .. } => Phase::Resizing,
        }
    }

    /// The confirmed selection (released after a drag, or set by a key).
    pub fn selection(&self) -> Option<Rect> {
        match self.state {
            State::Dragging { .. } => None,
            _ => self.selection,
        }
    }

    /// The rectangle shown undimmed now: the rubber band while dragging, else the selection.
    pub fn live(&self) -> Option<Rect> {
        match self.state {
            State::Dragging {
                anchor, to, square, ..
            } => Some(self.band(anchor, to, square)),
            _ => self.selection,
        }
    }

    pub fn pointer(&self) -> (i32, i32) {
        self.pointer
    }

    /// Hides the first-run help (after the first successful capture, ADR 0026).
    pub fn hide_help(&mut self) {
        self.help = None;
    }

    pub fn help_shown(&self) -> bool {
        self.help.is_some()
    }

    /// A one-line message at the top of the overlay (a failed pin, NOEUL-FR-025); `None` clears.
    pub fn set_banner(&mut self, text: Option<String>) {
        self.banner = text;
    }

    fn clamp(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let p = self.picture;
        let max_x = i32::try_from(p.right() - 1).unwrap_or(i32::MAX);
        let max_y = i32::try_from(p.bottom() - 1).unwrap_or(i32::MAX);
        (x.clamp(p.x, max_x.max(p.x)), y.clamp(p.y, max_y.max(p.y)))
    }

    /// The monitor that contains `pt`, or the nearest one.
    fn monitor_index(&self, (x, y): (i32, i32)) -> usize {
        let dist = |m: &Monitor| {
            let r = m.rect;
            let dx = (i64::from(r.x) - i64::from(x))
                .max(i64::from(x) - (r.right() - 1))
                .max(0);
            let dy = (i64::from(r.y) - i64::from(y))
                .max(i64::from(y) - (r.bottom() - 1))
                .max(0);
            dx * dx + dy * dy
        };
        self.monitors
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| dist(m))
            .map_or(0, |(i, _)| i)
    }

    /// The monitor at `pt` (a 100 % stand-in when the layout is empty).
    pub fn monitor_at(&self, pt: (i32, i32)) -> Monitor {
        self.monitors
            .get(self.monitor_index(pt))
            .copied()
            .unwrap_or(Monitor::new(self.picture, true))
    }

    fn beyond_drag(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let (cx, cy) = self.sys.drag;
        let dx = (i64::from(to.0) - i64::from(from.0)).unsigned_abs();
        let dy = (i64::from(to.1) - i64::from(from.1)).unsigned_abs();
        dx > u64::from(cx / 2) || dy > u64::from(cy / 2)
    }

    /// The rubber band from `anchor` to `to` (both inclusive pixels); `square` keeps the sides
    /// equal (Shift, NOEUL-FR-011) within the picture.
    fn band(&self, anchor: (i32, i32), to: (i32, i32), square: bool) -> Rect {
        let (ax, ay) = (i64::from(anchor.0), i64::from(anchor.1));
        let (mut tx, mut ty) = (i64::from(to.0), i64::from(to.1));
        if square {
            let sx = if tx < ax { -1 } else { 1 };
            let sy = if ty < ay { -1 } else { 1 };
            let d = (tx - ax).abs().max((ty - ay).abs());
            let (cx, cy) = self.clamp((
                i32::try_from(ax + sx * d).unwrap_or(i32::MAX),
                i32::try_from(ay + sy * d).unwrap_or(i32::MAX),
            ));
            let d = (i64::from(cx) - ax).abs().min((i64::from(cy) - ay).abs());
            tx = ax + sx * d;
            ty = ay + sy * d;
        }
        let x0 = ax.min(tx);
        let y0 = ay.min(ty);
        let w = (ax - tx).unsigned_abs() + 1;
        let h = (ay - ty).unsigned_abs() + 1;
        Rect::new(
            i32::try_from(x0).unwrap_or(i32::MAX),
            i32::try_from(y0).unwrap_or(i32::MAX),
            u32::try_from(w).unwrap_or(u32::MAX),
            u32::try_from(h).unwrap_or(u32::MAX),
        )
    }

    /// Lengths of the selection's handles and band, from the DPI of the monitor at its center.
    fn metrics(&self, s: Rect) -> SelMetrics {
        let c = (
            i32::try_from(i64::from(s.x) + i64::from(s.width / 2)).unwrap_or(s.x),
            i32::try_from(i64::from(s.y) + i64::from(s.height / 2)).unwrap_or(s.y),
        );
        let m = self.monitor_at(c);
        SelMetrics {
            half: m.px(9) / 2,
            band: m.px(3),
            handle: m.px(7),
            small: s.width < m.px(27) || s.height < m.px(27),
        }
    }

    /// The zone of a point relative to the confirmed selection (UX spec §1): a handle zone
    /// (9×9 logical pixels), the border band (3 logical pixels on each side of an edge), the
    /// interior, or outside. A selection smaller than 27 logical pixels in either direction has
    /// its handles and band outside it, so its whole area is interior.
    pub fn zone(&self, (x, y): (i32, i32)) -> Zone {
        let Some(s) = self.selection else {
            return Zone::Outside;
        };
        let m = self.metrics(s);
        let (px, py) = (i64::from(x), i64::from(y));
        let half = i64::from(m.half);
        for (edges, (cx, cy)) in handle_centers(s, &m) {
            if (px - cx).abs() <= half && (py - cy).abs() <= half {
                return Zone::Edge(edges);
            }
        }
        let (l, r, t, b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
        let band = i64::from(m.band);
        let inside = px >= l && px < r && py >= t && py < b;
        if m.small && inside {
            return Zone::Interior;
        }
        let in_y = py >= t - band && py < b + band;
        let in_x = px >= l - band && px < r + band;
        let edges = if m.small {
            Edges {
                left: in_y && px >= l - band && px < l,
                right: in_y && px >= r && px < r + band,
                top: in_x && py >= t - band && py < t,
                bottom: in_x && py >= b && py < b + band,
            }
        } else {
            Edges {
                left: in_y && (px - l).abs() <= band,
                right: in_y && (px - r).abs() <= band,
                top: in_x && (py - t).abs() <= band,
                bottom: in_x && (py - b).abs() <= band,
            }
        };
        if edges != Edges::default() {
            Zone::Edge(edges)
        } else if inside {
            Zone::Interior
        } else {
            Zone::Outside
        }
    }

    /// The pointer shape for the current state and position.
    pub fn cursor(&self) -> Cursor {
        match self.state {
            State::Moving { .. } => Cursor::Move,
            State::Resizing { edges, .. } => edge_cursor(edges),
            State::Pressed { zone, .. } => zone_cursor(zone),
            State::Selected => zone_cursor(self.zone(self.pointer)),
            State::Hover | State::Dragging { .. } => Cursor::Cross,
        }
    }

    /// Feeds one input. See the module documentation for the rules.
    pub fn handle(&mut self, input: Input) -> Outcome {
        match input {
            Input::Leave | Input::FocusLost | Input::Action(Action::Cancel) => Outcome::Cancel,
            Input::CaptureLost => {
                self.abort_drag();
                Outcome::Stay
            }
            Input::Move { x, y, mods } => {
                self.pointer = self.clamp((x, y));
                self.shift = mods.shift;
                self.pointer_moved();
                Outcome::Stay
            }
            Input::Press {
                button, x, y, mods, ..
            } => {
                self.pointer = self.clamp((x, y));
                self.shift = mods.shift;
                self.press(button)
            }
            Input::Release { button, x, y } => {
                if button == Button::Left {
                    self.pointer = self.clamp((x, y));
                    self.pointer_moved();
                    self.release();
                }
                Outcome::Stay
            }
            Input::Action(a) => self.action(a),
        }
    }

    fn press(&mut self, button: Button) -> Outcome {
        match button {
            Button::Left => {
                if matches!(self.state, State::Hover | State::Selected) {
                    let zone = if self.state == State::Selected {
                        self.zone(self.pointer)
                    } else {
                        Zone::Outside
                    };
                    self.state = State::Pressed {
                        at: self.pointer,
                        zone,
                    };
                }
                Outcome::Stay
            }
            Button::Right => match self.state {
                State::Hover => Outcome::Cancel,
                State::Selected => {
                    if self.zone(self.pointer) == Zone::Outside {
                        // Right-click outside a confirmed selection clears it (NOEUL-FR-012).
                        self.selection = None;
                        self.state = State::Hover;
                    }
                    // Inside: the selection's context menu (planned, Step 51).
                    Outcome::Stay
                }
                _ => {
                    // During a drag: undo that drag.
                    self.abort_drag();
                    Outcome::Stay
                }
            },
            // Middle-click pins in Step 51; nothing yet.
            Button::Middle => Outcome::Stay,
        }
    }

    fn pointer_moved(&mut self) {
        let p = self.pointer;
        match self.state {
            State::Pressed { at, zone } if self.beyond_drag(at, p) => {
                self.state = match zone {
                    Zone::Outside => State::Dragging {
                        anchor: at,
                        to: p,
                        square: self.shift,
                        before: self.selection,
                    },
                    Zone::Interior => State::Moving {
                        grab: at,
                        start: self.selection.unwrap_or(self.picture),
                    },
                    Zone::Edge(edges) => State::Resizing {
                        edges,
                        grab: at,
                        start: self.selection.unwrap_or(self.picture),
                    },
                };
                self.pointer_moved();
            }
            State::Dragging { anchor, before, .. } => {
                self.state = State::Dragging {
                    anchor,
                    to: p,
                    square: self.shift,
                    before,
                };
            }
            State::Moving { grab, start } => {
                self.selection = Some(self.moved(start, p.0 - grab.0, p.1 - grab.1));
            }
            State::Resizing { edges, grab, start } => {
                self.selection = Some(self.resized(start, edges, p.0 - grab.0, p.1 - grab.1));
            }
            _ => {}
        }
    }

    fn release(&mut self) {
        match self.state {
            State::Pressed { .. } => {
                // A click without a drag: nothing in this build (window detection is Step 52,
                // the double-click gesture Step 51).
                self.state = self.rest();
            }
            State::Dragging {
                anchor, to, square, ..
            } => {
                self.selection = Some(self.band(anchor, to, square));
                self.state = State::Selected;
            }
            State::Moving { .. } | State::Resizing { .. } => self.state = State::Selected,
            State::Hover | State::Selected => {}
        }
    }

    fn rest(&self) -> State {
        if self.selection.is_some() {
            State::Selected
        } else {
            State::Hover
        }
    }

    fn abort_drag(&mut self) {
        match self.state {
            State::Dragging { before, .. } => self.selection = before,
            State::Moving { start, .. } | State::Resizing { start, .. } => {
                self.selection = Some(start);
            }
            _ => {}
        }
        self.state = self.rest();
    }

    fn dragging(&self) -> bool {
        matches!(
            self.state,
            State::Pressed { .. }
                | State::Dragging { .. }
                | State::Moving { .. }
                | State::Resizing { .. }
        )
    }

    fn action(&mut self, a: Action) -> Outcome {
        if self.dragging() {
            // Keys wait until the button is released (Esc still cancels, above).
            return Outcome::Stay;
        }
        match a {
            Action::Cancel => Outcome::Cancel,
            Action::Pin => self.selection.map_or(Outcome::Stay, Outcome::Pin),
            Action::Full => {
                let m = self.monitor_at(self.pointer).rect;
                let next = if self.selection == Some(m) {
                    self.picture
                } else {
                    m
                };
                self.set_selection(next);
                Outcome::Stay
            }
            Action::Last => {
                if let Some(r) = self.last.and_then(|l| intersection(l, self.picture)) {
                    self.set_selection(r);
                }
                Outcome::Stay
            }
            Action::Move(d) | Action::Grow(d) | Action::Shrink(d) => {
                if let Some(s) = self.selection {
                    let next = match a {
                        Action::Move(_) => {
                            let (dx, dy) = step(d);
                            self.moved(s, dx, dy)
                        }
                        Action::Grow(_) => self.grown(s, d),
                        _ => shrunk(s, d),
                    };
                    self.selection = Some(next);
                }
                Outcome::Stay
            }
        }
    }

    fn set_selection(&mut self, r: Rect) {
        self.selection = Some(r);
        self.state = State::Selected;
    }

    /// `start` moved by (dx, dy), kept inside the picture.
    fn moved(&self, start: Rect, dx: i32, dy: i32) -> Rect {
        let p = self.picture;
        let max_x = p.right() - i64::from(start.width);
        let max_y = p.bottom() - i64::from(start.height);
        let x =
            (i64::from(start.x) + i64::from(dx)).clamp(i64::from(p.x), max_x.max(i64::from(p.x)));
        let y =
            (i64::from(start.y) + i64::from(dy)).clamp(i64::from(p.y), max_y.max(i64::from(p.y)));
        Rect::new(
            i32::try_from(x).unwrap_or(start.x),
            i32::try_from(y).unwrap_or(start.y),
            start.width,
            start.height,
        )
    }

    /// `start` with the edges in `edges` moved by (dx, dy), at least one pixel, inside the picture;
    /// an edge dragged across the opposite one flips the rectangle.
    fn resized(&self, start: Rect, edges: Edges, dx: i32, dy: i32) -> Rect {
        let p = self.picture;
        let (pl, pr, pt, pb) = (i64::from(p.x), p.right(), i64::from(p.y), p.bottom());
        let mut l = i64::from(start.x);
        let mut r = start.right();
        let mut t = i64::from(start.y);
        let mut b = start.bottom();
        if edges.left {
            l = (l + i64::from(dx)).clamp(pl, pr);
        }
        if edges.right {
            r = (r + i64::from(dx)).clamp(pl, pr);
        }
        if edges.top {
            t = (t + i64::from(dy)).clamp(pt, pb);
        }
        if edges.bottom {
            b = (b + i64::from(dy)).clamp(pt, pb);
        }
        let (l, r) = span(l, r, pl, pr);
        let (t, b) = span(t, b, pt, pb);
        rect_from(l, t, r, b)
    }

    fn grown(&self, s: Rect, d: Dir) -> Rect {
        let p = self.picture;
        let (mut l, mut r, mut t, mut b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
        match d {
            Dir::Up => t = (t - 1).max(i64::from(p.y)),
            Dir::Down => b = (b + 1).min(p.bottom()),
            Dir::Left => l = (l - 1).max(i64::from(p.x)),
            Dir::Right => r = (r + 1).min(p.right()),
        }
        rect_from(l, t, r, b)
    }

    /// The scene for the current state (what [`compose`] draws). `measure` sizes the labels'
    /// text in device pixels.
    pub fn scene(&self, measure: &dyn Measure) -> Scene {
        let live = self.live();
        let confirmed = !matches!(self.state, State::Dragging { .. });
        let mut scene = Scene {
            picture: self.picture,
            selection: live,
            accent: None,
            halo: None,
            handles: Vec::new(),
            chip: None,
            help: None,
        };
        if let Some(s) = live {
            // The border sits just outside the selection; on a picture edge it moves inside so
            // it stays visible (UX spec §10).
            scene.accent = Some(clamp_into(grow(s, 1), self.picture));
            scene.halo = Some(clamp_into(grow(s, 2), self.picture));
            if confirmed {
                let m = self.metrics(s);
                scene.handles = handle_centers(s, &m)
                    .iter()
                    .filter_map(|(_, (cx, cy))| {
                        let h = i64::from(m.handle);
                        intersection(
                            rect_from(cx - h / 2, cy - h / 2, cx - h / 2 + h, cy - h / 2 + h),
                            self.picture,
                        )
                    })
                    .collect();
            }
        }
        scene.chip = Some(self.chip(live, measure));
        if !self.dragging() {
            let lines = match &self.banner {
                Some(b) => Some(vec![b.clone()]),
                None => self.help.clone(),
            };
            if let Some(lines) = lines {
                scene.help = Some(self.help_label(lines, live, measure));
            }
        }
        scene
    }

    /// The size and position readout (device pixels): next to the selection, or next to the
    /// pointer while nothing is selected.
    fn chip(&self, live: Option<Rect>, measure: &dyn Measure) -> Label {
        let (text, anchor) = match live {
            Some(s) => (
                format!("{} \u{d7} {}  at {}, {}", s.width, s.height, s.x, s.y),
                (s.x, s.y),
            ),
            None => (
                format!("{}, {}", self.pointer.0, self.pointer.1),
                self.pointer,
            ),
        };
        let m = self.monitor_at(anchor);
        let mut label = Label::new(vec![text], Font::Mono, m.dpi, measure);
        let (w, h) = (i64::from(label.rect.width), i64::from(label.rect.height));
        let mr = m.rect;
        let (ml, mt, mrr, mb) = (i64::from(mr.x), i64::from(mr.y), mr.right(), mr.bottom());
        let gap = i64::from(m.px(6));
        let (mut x, mut y) = match live {
            Some(s) => {
                let above = i64::from(s.y) - 2 - gap - h;
                if above >= mt {
                    (i64::from(s.x), above)
                } else {
                    (i64::from(s.x) + 2 + gap, i64::from(s.y) + 2 + gap)
                }
            }
            None => {
                let off = i64::from(m.px(16));
                let (px, py) = (i64::from(self.pointer.0), i64::from(self.pointer.1));
                let x = if px + off + w <= mrr {
                    px + off
                } else {
                    px - off - w
                };
                let y = if py + off + h <= mb {
                    py + off
                } else {
                    py - off - h
                };
                (x, y)
            }
        };
        x = x.min(mrr - w).max(ml);
        y = y.min(mb - h).max(mt);
        label.rect.x = i32::try_from(x).unwrap_or(mr.x);
        label.rect.y = i32::try_from(y).unwrap_or(mr.y);
        label
    }

    /// The first-run help (or a banner): top center of the monitor the pointer was on when the
    /// overlay opened, or its bottom when the selection is in the way.
    fn help_label(&self, lines: Vec<String>, live: Option<Rect>, measure: &dyn Measure) -> Label {
        let m = self
            .monitors
            .get(self.help_monitor)
            .copied()
            .unwrap_or(Monitor::new(self.picture, true));
        let mut label = Label::new(lines, Font::Ui, m.dpi, measure);
        let mr = m.rect;
        let w = i64::from(label.rect.width);
        let h = i64::from(label.rect.height);
        let margin = i64::from(m.px(24));
        let x = (i64::from(mr.x) + (i64::from(mr.width) - w) / 2).max(i64::from(mr.x));
        let top = i64::from(mr.y) + margin;
        let bottom = (mr.bottom() - margin - h).max(i64::from(mr.y));
        label.rect.x = i32::try_from(x).unwrap_or(mr.x);
        label.rect.y = i32::try_from(top).unwrap_or(mr.y);
        if let Some(s) = live
            && label
                .rect
                .intersects(&grow(s, i32::try_from(m.px(12)).unwrap_or(12)))
        {
            label.rect.y = i32::try_from(bottom).unwrap_or(mr.y);
        }
        label
    }
}

#[derive(Debug, Clone, Copy)]
struct SelMetrics {
    half: u32,
    band: u32,
    handle: u32,
    small: bool,
}

/// The centers of the eight handles (corners and edge midpoints, on the edge lines), moved
/// outside a small selection so its whole area stays interior (UX spec §1).
fn handle_centers(s: Rect, m: &SelMetrics) -> [(Edges, (i64, i64)); 8] {
    let (l, r, t, b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
    let off = if m.small { i64::from(m.half) + 1 } else { 0 };
    let (xl, xr, yt, yb) = (l - off, r + off, t - off, b + off);
    let (xm, ym) = ((l + r) / 2, (t + b) / 2);
    let e = |left, right, top, bottom| Edges {
        left,
        right,
        top,
        bottom,
    };
    [
        (e(true, false, true, false), (xl, yt)),
        (e(false, true, true, false), (xr, yt)),
        (e(true, false, false, true), (xl, yb)),
        (e(false, true, false, true), (xr, yb)),
        (e(false, false, true, false), (xm, yt)),
        (e(false, false, false, true), (xm, yb)),
        (e(true, false, false, false), (xl, ym)),
        (e(false, true, false, false), (xr, ym)),
    ]
}

fn edge_cursor(e: Edges) -> Cursor {
    match (e.left || e.right, e.top || e.bottom) {
        (true, true) if (e.left && e.top) || (e.right && e.bottom) => Cursor::SizeNwse,
        (true, true) => Cursor::SizeNesw,
        (true, false) => Cursor::SizeWe,
        _ => Cursor::SizeNs,
    }
}

fn zone_cursor(z: Zone) -> Cursor {
    match z {
        Zone::Outside => Cursor::Cross,
        Zone::Interior => Cursor::Move,
        Zone::Edge(e) => edge_cursor(e),
    }
}

fn step(d: Dir) -> (i32, i32) {
    match d {
        Dir::Up => (0, -1),
        Dir::Down => (0, 1),
        Dir::Left => (-1, 0),
        Dir::Right => (1, 0),
    }
}

/// `Shift`+arrow: shrink from the edge opposite to the arrow (the manifest's titles: `Shift+Up`
/// shrinks from the bottom edge), never below one pixel.
fn shrunk(s: Rect, d: Dir) -> Rect {
    let (mut l, mut r, mut t, mut b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
    match d {
        Dir::Up => b = (b - 1).max(t + 1),
        Dir::Down => t = (t + 1).min(b - 1),
        Dir::Left => r = (r - 1).max(l + 1),
        Dir::Right => l = (l + 1).min(r - 1),
    }
    rect_from(l, t, r, b)
}

/// Orders two edges and keeps at least one pixel between them, inside `lo..hi`.
fn span(a: i64, b: i64, lo: i64, hi: i64) -> (i64, i64) {
    let (mut a, mut b) = if a <= b { (a, b) } else { (b, a) };
    if a == b {
        if b < hi {
            b += 1;
        } else {
            a = (a - 1).max(lo);
        }
    }
    (a, b)
}

fn rect_from(l: i64, t: i64, r: i64, b: i64) -> Rect {
    Rect::new(
        i32::try_from(l).unwrap_or(i32::MAX),
        i32::try_from(t).unwrap_or(i32::MAX),
        u32::try_from((r - l).max(0)).unwrap_or(0),
        u32::try_from((b - t).max(0)).unwrap_or(0),
    )
}

/// `r` grown by `by` pixels on every side (may stick out of the picture).
pub fn grow(r: Rect, by: i32) -> Rect {
    let by64 = i64::from(by);
    rect_from(
        i64::from(r.x) - by64,
        i64::from(r.y) - by64,
        r.right() + by64,
        r.bottom() + by64,
    )
}

/// The overlap of two rectangles, if any.
pub fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let l = i64::from(a.x).max(i64::from(b.x));
    let t = i64::from(a.y).max(i64::from(b.y));
    let r = a.right().min(b.right());
    let bt = a.bottom().min(b.bottom());
    (l < r && t < bt).then(|| rect_from(l, t, r, bt))
}

/// `r` shrunk where it sticks out of `into` (an outline then runs along `into`'s edge).
fn clamp_into(r: Rect, into: Rect) -> Rect {
    intersection(r, into).unwrap_or(into)
}

/// `a` minus `b`, as up to four rectangles.
pub fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let Some(i) = intersection(a, b) else {
        return if a.is_empty() { Vec::new() } else { vec![a] };
    };
    let (al, at, ar, ab) = (i64::from(a.x), i64::from(a.y), a.right(), a.bottom());
    let (il, it, ir, ib) = (i64::from(i.x), i64::from(i.y), i.right(), i.bottom());
    [
        rect_from(al, at, ar, it),
        rect_from(al, ib, ar, ab),
        rect_from(al, it, il, ib),
        rect_from(ir, it, ar, ib),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// A text font of Noeul's overlay (DESIGN_SYSTEM §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    /// The mono family at caption size (11): the size and position readout.
    Mono,
    /// The UI family at body size (12): the help and banners.
    Ui,
}

/// Measures one line of text in device pixels (GDI on Windows; [`Approx`] in tests).
pub trait Measure {
    /// `(width, height)` of `text` in `font` at `dpi`.
    fn text(&self, font: Font, dpi: u32, text: &str) -> (u32, u32);
}

/// A stand-in measure: 7 logical pixels per character, 15 per line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Approx;

impl Measure for Approx {
    fn text(&self, _: Font, dpi: u32, text: &str) -> (u32, u32) {
        let n = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        (n.saturating_mul(scale_px(7, dpi)), scale_px(15, dpi))
    }
}

/// A box with lines of text: `bg.overlay` with a `border.subtle` outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub rect: Rect,
    pub lines: Vec<String>,
    pub font: Font,
    pub dpi: u32,
    /// Padding inside the box and the height of one line (device pixels).
    pub pad: u32,
    pub line_height: u32,
}

impl Label {
    fn new(lines: Vec<String>, font: Font, dpi: u32, measure: &dyn Measure) -> Label {
        let pad = scale_px(if font == Font::Mono { 4 } else { 8 }, dpi);
        let mut w = 0;
        let mut lh = 1;
        for l in &lines {
            let (tw, th) = measure.text(font, dpi, l);
            w = w.max(tw);
            lh = lh.max(th);
        }
        let n = u32::try_from(lines.len()).unwrap_or(1).max(1);
        Label {
            rect: Rect::new(0, 0, w + 2 * pad, lh * n + 2 * pad),
            lines,
            font,
            dpi,
            pad,
            line_height: lh,
        }
    }

    /// Where line `i` starts.
    pub fn line_origin(&self, i: usize) -> (i32, i32) {
        let pad = i32::try_from(self.pad).unwrap_or(0);
        let lh = i32::try_from(self.line_height).unwrap_or(0);
        (
            self.rect.x + pad,
            self.rect.y + pad + lh * i32::try_from(i).unwrap_or(0),
        )
    }
}

/// What the overlay shows. Every rectangle is in device pixels of the virtual desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    /// The frozen picture's rectangle (the whole overlay).
    pub picture: Rect,
    /// Shown undimmed.
    pub selection: Option<Rect>,
    /// One-pixel outlines: the accent border and its dark halo (UX spec §10).
    pub accent: Option<Rect>,
    pub halo: Option<Rect>,
    /// Resize handles (accent squares with a halo outline), only on a confirmed selection.
    pub handles: Vec<Rect>,
    /// The size and position readout.
    pub chip: Option<Label>,
    /// The first-run keyboard help, or a banner.
    pub help: Option<Label>,
}

impl Scene {
    /// Rectangles drawn over the dimmed or plain picture.
    fn parts(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        if let (Some(a), Some(h)) = (self.accent, self.halo) {
            out.extend(outline_strips(h, 2));
            out.extend(outline_strips(a, 1));
        }
        out.extend(self.handles.iter().copied());
        out.extend(self.chip.iter().map(|l| l.rect));
        out.extend(self.help.iter().map(|l| l.rect));
        out
    }
}

/// The four strips of an outline `width` pixels thick along the inside of `r`.
fn outline_strips(r: Rect, width: u32) -> Vec<Rect> {
    let w = i64::from(width);
    let (l, t, rr, b) = (i64::from(r.x), i64::from(r.y), r.right(), r.bottom());
    [
        rect_from(l, t, rr, (t + w).min(b)),
        rect_from(l, (b - w).max(t), rr, b),
        rect_from(l, t, (l + w).min(rr), b),
        rect_from((rr - w).max(l), t, rr, b),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// The pixels that differ between two scenes: every part of both, and where the undimmed area
/// changed. Clipped to the picture; may overlap.
pub fn dirty(old: &Scene, new: &Scene) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut push = |r: Rect| {
        if let Some(r) = intersection(r, new.picture)
            && !out.contains(&r)
        {
            out.push(r);
        }
    };
    if old.selection != new.selection {
        match (old.selection, new.selection) {
            (Some(a), Some(b)) => {
                subtract(a, b).into_iter().for_each(&mut push);
                subtract(b, a).into_iter().for_each(&mut push);
            }
            (Some(a), None) | (None, Some(a)) => push(a),
            (None, None) => {}
        }
    }
    if old != new {
        for r in old.parts().into_iter().chain(new.parts()) {
            push(r);
        }
    }
    out
}

/// BGRA colors of the overlay (DESIGN_SYSTEM §2: `accent`, `text.on-accent` as the halo,
/// `bg.overlay`, `border.subtle`).
pub mod color {
    pub const ACCENT: [u8; 4] = [0xF5, 0xA6, 0xB7, 0xFF];
    pub const HALO: [u8; 4] = [0x12, 0x0E, 0x0E, 0xFF];
    pub const BG_OVERLAY: [u8; 4] = [0x24, 0x1B, 0x1C, 0xFF];
    pub const BORDER_SUBTLE: [u8; 4] = [0x2F, 0x25, 0x26, 0xFF];
    /// `text.primary` and `text.secondary` as RGB.
    pub const TEXT_PRIMARY: [u8; 3] = [0xE8, 0xE6, 0xF0];
    pub const TEXT_SECONDARY: [u8; 3] = [0xA9, 0xA6, 0xB8];
}

/// Brightness kept outside the selection: a black veil of about 45 % (UX spec §10), as n/256.
pub const DIM: u32 = 141;

/// One dimmed BGRA pixel (alpha opaque).
#[inline]
pub fn dim_pixel(p: [u8; 4]) -> [u8; 4] {
    let d = |c: u8| u8::try_from((u32::from(c) * DIM) >> 8).unwrap_or(255);
    [d(p[0]), d(p[1]), d(p[2]), 0xFF]
}

/// Writes the pixels of `scene` inside `area` into `out`, a BGRA buffer of the frozen picture's
/// size and position (`frozen.rect()`): the picture dimmed outside the selection and plain inside
/// it, then the outlines, handles and label boxes. Label text is drawn by the platform after
/// this, clipped to `area`.
pub fn compose(frozen: &Frame, scene: &Scene, area: Rect, out: &mut [u8]) {
    let pic = frozen.rect();
    let Some(area) = intersection(area, pic) else {
        return;
    };
    let stride = pic.width as usize * 4;
    if out.len() < stride * pic.height as usize {
        return;
    }
    let src = frozen.pixels();
    let ox = (i64::from(area.x) - i64::from(pic.x)) as usize;
    let oy = (i64::from(area.y) - i64::from(pic.y)) as usize;
    let w = area.width as usize;
    let sel = scene.selection.and_then(|s| intersection(s, area));
    for row in 0..area.height as usize {
        let y = oy + row;
        let start = y * stride + ox * 4;
        let line_src = &src[start..start + w * 4];
        let line_out = &mut out[start..start + w * 4];
        let ay = i64::from(area.y) + row as i64;
        let (k0, k1) = match sel {
            Some(s) if ay >= i64::from(s.y) && ay < s.bottom() => (
                (i64::from(s.x) - i64::from(area.x)) as usize,
                (s.right() - i64::from(area.x)) as usize,
            ),
            _ => (w, w),
        };
        for (i, (o, s)) in line_out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(line_src.as_chunks::<4>().0)
            .enumerate()
        {
            *o = if i >= k0 && i < k1 { *s } else { dim_pixel(*s) };
        }
    }
    let mut fill = |r: Rect, c: [u8; 4]| {
        let Some(r) = intersection(r, area) else {
            return;
        };
        let x0 = (i64::from(r.x) - i64::from(pic.x)) as usize;
        let y0 = (i64::from(r.y) - i64::from(pic.y)) as usize;
        for y in y0..y0 + r.height as usize {
            let s = y * stride + x0 * 4;
            for p in out[s..s + r.width as usize * 4].as_chunks_mut::<4>().0 {
                *p = c;
            }
        }
    };
    if let Some(h) = scene.halo {
        outline_strips(h, 1)
            .into_iter()
            .for_each(|r| fill(r, color::HALO));
    }
    if let Some(a) = scene.accent {
        outline_strips(a, 1)
            .into_iter()
            .for_each(|r| fill(r, color::ACCENT));
    }
    for h in &scene.handles {
        fill(*h, color::HALO);
        fill(grow(*h, -1), color::ACCENT);
    }
    for l in scene.chip.iter().chain(scene.help.iter()) {
        fill(l.rect, color::BORDER_SUBTLE);
        fill(grow(l.rect, -1), color::BG_OVERLAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Synthetic;
    use crate::frame::Ledger;

    fn one(w: u32, h: u32) -> Overlay {
        Overlay::new(
            Rect::new(0, 0, w, h),
            vec![Monitor::new(Rect::new(0, 0, w, h), true)],
            SystemValues::default(),
            (5, 5),
            None,
            None,
        )
    }

    fn mv(o: &mut Overlay, x: i32, y: i32) -> Outcome {
        o.handle(Input::Move {
            x,
            y,
            mods: Mods::NONE,
        })
    }

    fn press(o: &mut Overlay, b: Button, x: i32, y: i32) -> Outcome {
        o.handle(Input::Press {
            button: b,
            x,
            y,
            mods: Mods::NONE,
            time_ms: 0,
        })
    }

    fn release(o: &mut Overlay, x: i32, y: i32) -> Outcome {
        o.handle(Input::Release {
            button: Button::Left,
            x,
            y,
        })
    }

    fn drag(o: &mut Overlay, from: (i32, i32), to: (i32, i32)) {
        press(o, Button::Left, from.0, from.1);
        mv(o, (from.0 + to.0) / 2, (from.1 + to.1) / 2);
        mv(o, to.0, to.1);
        release(o, to.0, to.1);
    }

    #[test]
    fn press_drag_release_selects_inclusive_pixels_and_tracks_the_pointer() {
        let mut o = one(800, 600);
        assert_eq!(o.phase(), Phase::Hover);
        press(&mut o, Button::Left, 100, 100);
        assert_eq!(o.phase(), Phase::Pressed);
        mv(&mut o, 300, 250);
        assert_eq!(o.phase(), Phase::Dragging);
        assert_eq!(o.live(), Some(Rect::new(100, 100, 201, 151)), "live band");
        assert_eq!(o.selection(), None, "not confirmed while dragging");
        mv(&mut o, 499, 399);
        release(&mut o, 499, 399);
        assert_eq!(o.phase(), Phase::Selected);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 400, 300)));
        // Dragging up and left works the same way.
        drag(&mut o, (50, 60), (10, 20));
        assert_eq!(o.selection(), Some(Rect::new(10, 20, 41, 41)));
        // The pointer is clamped to the picture.
        drag(&mut o, (700, 500), (5000, -40));
        assert_eq!(o.selection(), Some(Rect::new(700, 0, 100, 501)));
    }

    #[test]
    fn the_drag_starts_only_beyond_the_users_threshold_whatever_the_pause() {
        let mut o = one(200, 200);
        let sys = SystemValues {
            drag: (10, 10),
            ..SystemValues::default()
        };
        o.sys = sys;
        press(&mut o, Button::Left, 50, 50);
        mv(&mut o, 55, 50); // within ±5
        assert_eq!(o.phase(), Phase::Pressed);
        // A long pause with the button held changes nothing (there is no timer); the next
        // movement beyond the threshold starts the drag from the press point.
        mv(&mut o, 56, 50);
        assert_eq!(o.phase(), Phase::Dragging);
        assert_eq!(o.live(), Some(Rect::new(50, 50, 7, 1)));
        release(&mut o, 56, 50);
        // A click in place (released within the threshold) selects nothing.
        let mut c = one(200, 200);
        press(&mut c, Button::Left, 30, 30);
        mv(&mut c, 31, 31);
        release(&mut c, 31, 31);
        assert_eq!((c.phase(), c.selection()), (Phase::Hover, None));
        // A press right after opening, before any move, starts where it happened.
        let mut d = one(200, 200);
        press(&mut d, Button::Left, 120, 80);
        mv(&mut d, 140, 90);
        release(&mut d, 140, 90);
        assert_eq!(d.selection(), Some(Rect::new(120, 80, 21, 11)));
    }

    #[test]
    fn shift_makes_a_square_inside_the_picture() {
        let mut o = one(300, 200);
        o.handle(Input::Press {
            button: Button::Left,
            x: 100,
            y: 100,
            mods: Mods::SHIFT,
            time_ms: 0,
        });
        o.handle(Input::Move {
            x: 160,
            y: 120,
            mods: Mods::SHIFT,
        });
        assert_eq!(o.live(), Some(Rect::new(100, 100, 61, 61)));
        // Towards the bottom edge the square shrinks to what fits.
        o.handle(Input::Move {
            x: 290,
            y: 150,
            mods: Mods::SHIFT,
        });
        assert_eq!(o.live(), Some(Rect::new(100, 100, 100, 100)));
        // Releasing Shift during the drag frees the shape again.
        mv(&mut o, 130, 110);
        assert_eq!(o.live(), Some(Rect::new(100, 100, 31, 11)));
    }

    #[test]
    fn escape_right_click_and_focus_loss_cancel() {
        let mut o = one(100, 100);
        assert_eq!(press(&mut o, Button::Right, 10, 10), Outcome::Cancel);
        let mut o = one(100, 100);
        drag(&mut o, (10, 10), (40, 40));
        assert_eq!(o.handle(Input::Leave), Outcome::Cancel);
        assert_eq!(o.handle(Input::FocusLost), Outcome::Cancel);
        assert_eq!(o.handle(Input::Action(Action::Cancel)), Outcome::Cancel);
        // Escape cancels even in the middle of a drag.
        let mut o = one(100, 100);
        press(&mut o, Button::Left, 10, 10);
        mv(&mut o, 50, 50);
        assert_eq!(o.handle(Input::Leave), Outcome::Cancel);
    }

    #[test]
    fn right_click_outside_clears_inside_keeps_and_during_a_drag_undoes_it() {
        let mut o = one(400, 400);
        drag(&mut o, (100, 100), (199, 199));
        let sel = o.selection();
        assert_eq!(press(&mut o, Button::Right, 150, 150), Outcome::Stay);
        assert_eq!(o.selection(), sel, "inside: kept (menu is Step 51)");
        assert_eq!(press(&mut o, Button::Right, 350, 350), Outcome::Stay);
        assert_eq!((o.phase(), o.selection()), (Phase::Hover, None), "cleared");
        // A new drag over an old selection, undone with a right-click, restores it.
        drag(&mut o, (100, 100), (199, 199));
        press(&mut o, Button::Left, 300, 300);
        mv(&mut o, 380, 380);
        assert_eq!(o.live(), Some(Rect::new(300, 300, 81, 81)));
        press(&mut o, Button::Right, 380, 380);
        assert_eq!(o.selection(), sel, "the drag is undone");
        assert_eq!(o.phase(), Phase::Selected);
        // A lost pointer capture undoes the drag the same way.
        press(&mut o, Button::Left, 300, 300);
        mv(&mut o, 380, 380);
        o.handle(Input::CaptureLost);
        assert_eq!(o.selection(), sel);
    }

    #[test]
    fn a_drag_outside_replaces_inside_moves_and_handles_resize() {
        let mut o = one(800, 600);
        drag(&mut o, (100, 100), (499, 399)); // (100,100) 400×300
        // Move by dragging the interior.
        drag(&mut o, (300, 250), (320, 260));
        assert_eq!(o.selection(), Some(Rect::new(120, 110, 400, 300)));
        // The right handle (edge midpoint at x = 520, y = 260) resizes the right edge.
        assert_eq!(
            o.zone((520, 260)),
            Zone::Edge(Edges {
                right: true,
                ..Edges::default()
            })
        );
        drag(&mut o, (520, 260), (560, 300));
        assert_eq!(o.selection(), Some(Rect::new(120, 110, 440, 300)));
        // The top-left corner handle moves two edges.
        drag(&mut o, (120, 110), (100, 90));
        assert_eq!(o.selection(), Some(Rect::new(100, 90, 460, 320)));
        // The border band on the bottom edge (not at a handle).
        assert_eq!(
            o.zone((200, 411)),
            Zone::Edge(Edges {
                bottom: true,
                ..Edges::default()
            })
        );
        // Dragging an edge across the opposite one flips, never below one pixel.
        drag(&mut o, (330, 90), (330, 500));
        let s = o.selection().unwrap();
        assert_eq!((s.y, s.height), (410, 90));
        // A drag that starts outside starts a new selection that replaces the old one.
        drag(&mut o, (700, 50), (760, 80));
        assert_eq!(o.selection(), Some(Rect::new(700, 50, 61, 31)));
        // Moving stays inside the picture.
        drag(&mut o, (730, 60), (2000, 2000));
        assert_eq!(o.selection(), Some(Rect::new(739, 569, 61, 31)));
    }

    #[test]
    fn zones_and_cursors_follow_dpi_and_small_selections() {
        // 150 %: handle zones 14 px (half 7), band 5 px, small below 41 px.
        let mut o = Overlay::new(
            Rect::new(0, 0, 1000, 1000),
            vec![Monitor::new(Rect::new(0, 0, 1000, 1000), true).with_dpi(144)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        o.handle(Input::Action(Action::Full));
        o.selection = Some(Rect::new(100, 100, 400, 400));
        o.state = State::Selected;
        assert!(matches!(o.zone((93, 93)), Zone::Edge(e) if e.left && e.top));
        assert_eq!(o.zone((92, 93)), Zone::Outside, "outside the 150 % zone");
        assert!(
            matches!(o.zone((105, 250)), Zone::Edge(e) if e.left),
            "band"
        );
        assert_eq!(o.zone((106, 250)), Zone::Interior);
        assert!(
            matches!(o.zone((106, 300)), Zone::Edge(e) if e.left),
            "handle zone"
        );
        mv(&mut o, 300, 300);
        assert_eq!(o.cursor(), Cursor::Move);
        mv(&mut o, 500, 500);
        assert_eq!(o.cursor(), Cursor::SizeNwse);
        mv(&mut o, 500, 100);
        assert_eq!(o.cursor(), Cursor::SizeNesw);
        mv(&mut o, 300, 100);
        assert_eq!(o.cursor(), Cursor::SizeNs);
        mv(&mut o, 900, 900);
        assert_eq!(o.cursor(), Cursor::Cross);
        // A 10×10 selection is interior everywhere; its handles sit outside it.
        o.selection = Some(Rect::new(200, 200, 10, 10));
        for p in [(200, 200), (209, 209), (204, 200)] {
            assert_eq!(o.zone(p), Zone::Interior, "{p:?}");
        }
        assert!(matches!(o.zone((192, 192)), Zone::Edge(e) if e.left && e.top));
        assert!(matches!(o.zone((205, 212)), Zone::Edge(e) if e.bottom));
    }

    #[test]
    fn keyboard_moves_grows_shrinks_and_pins() {
        let mut o = one(100, 100);
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Stay,
            "nothing to pin without a selection"
        );
        o.handle(Input::Action(Action::Move(Dir::Left)));
        assert_eq!(o.selection(), None, "keys need a selection");
        drag(&mut o, (10, 10), (19, 19)); // 10×10 at (10,10)
        let key = |o: &mut Overlay, a| {
            o.handle(Input::Action(a));
            o.selection().unwrap()
        };
        assert_eq!(
            key(&mut o, Action::Move(Dir::Right)),
            Rect::new(11, 10, 10, 10)
        );
        assert_eq!(key(&mut o, Action::Move(Dir::Up)), Rect::new(11, 9, 10, 10));
        assert_eq!(key(&mut o, Action::Grow(Dir::Up)), Rect::new(11, 8, 10, 11));
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Right)),
            Rect::new(11, 8, 11, 11)
        );
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Left)),
            Rect::new(10, 8, 12, 11)
        );
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Down)),
            Rect::new(10, 8, 12, 12)
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Up)),
            Rect::new(10, 8, 12, 11),
            "Shift+Up shrinks from the bottom edge"
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Down)),
            Rect::new(10, 9, 12, 10),
            "Shift+Down shrinks from the top edge"
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Left)),
            Rect::new(10, 9, 11, 10)
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Right)),
            Rect::new(11, 9, 10, 10)
        );
        for _ in 0..50 {
            key(&mut o, Action::Shrink(Dir::Right));
        }
        assert_eq!(o.selection().unwrap().width, 1, "never below one pixel");
        for _ in 0..500 {
            key(&mut o, Action::Move(Dir::Left));
            key(&mut o, Action::Grow(Dir::Left));
        }
        assert_eq!(o.selection().unwrap().x, 0, "kept inside the picture");
        let s = o.selection().unwrap();
        assert_eq!(o.handle(Input::Action(Action::Pin)), Outcome::Pin(s));
        assert_eq!(
            o.phase(),
            Phase::Selected,
            "the owner decides when it closes"
        );
        // Keys wait while a button is held.
        press(&mut o, Button::Left, 90, 90);
        mv(&mut o, 95, 95);
        assert_eq!(o.handle(Input::Action(Action::Pin)), Outcome::Stay);
    }

    #[test]
    fn ctrl_a_selects_the_monitor_then_all_and_r_restores_the_last() {
        // Two monitors: 100 % on the left, 150 % on the right, the right one higher.
        let mons = vec![
            Monitor::new(Rect::new(0, 100, 1920, 1080), true),
            Monitor::new(Rect::new(1920, 0, 2560, 1440), false).with_dpi(144),
        ];
        let pic = Rect::new(0, 0, 4480, 1440);
        let mut o = Overlay::new(
            pic,
            mons,
            SystemValues::default(),
            (2000, 50),
            Some(Rect::new(10, 150, 20, 30)),
            None,
        );
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(1920, 0, 2560, 1440)));
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(pic), "again: every monitor");
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(1920, 0, 2560, 1440)));
        o.handle(Input::Action(Action::Last));
        assert_eq!(o.selection(), Some(Rect::new(10, 150, 20, 30)));
        // A pointer in the gap above the left monitor belongs to the nearest monitor.
        mv(&mut o, 500, 20);
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(0, 100, 1920, 1080)));
    }

    #[test]
    fn action_ids_round_trip() {
        for id in ACTION_IDS {
            assert!(Action::from_id(id).is_some(), "{id}");
        }
        assert_eq!(
            Action::from_id("selection.grow-left"),
            Some(Action::Grow(Dir::Left))
        );
        assert_eq!(Action::from_id("selection.mark"), None, "not in this build");
        assert_eq!(Action::from_id("selection.move-sideways"), None);
    }

    #[test]
    fn the_scene_places_readout_help_and_handles_per_monitor_dpi() {
        let mons = vec![
            Monitor::new(Rect::new(0, 0, 1000, 800), true),
            Monitor::new(Rect::new(1000, 0, 1500, 1200), false).with_dpi(144),
        ];
        let help = vec!["Drag to select".to_string(), "Esc cancels".to_string()];
        let mut o = Overlay::new(
            Rect::new(0, 0, 2500, 1200),
            mons,
            SystemValues::default(),
            (1500, 600),
            None,
            Some(help.clone()),
        );
        let s = o.scene(&Approx);
        let chip = s.chip.clone().unwrap();
        assert_eq!(chip.lines, ["1500, 600"]);
        assert_eq!(chip.dpi, 144, "drawn at the monitor's scale");
        assert_eq!(
            (chip.rect.x, chip.rect.y),
            (1524, 624),
            "16 logical px from the pointer"
        );
        let h = s.help.unwrap();
        assert_eq!(h.lines, help);
        assert_eq!(h.rect.y, 36, "24 logical px from the top at 150 %");
        assert!(Rect::new(1000, 0, 1500, 1200).contains(&h.rect));
        // A selection under the help pushes it to the bottom; dragging hides it.
        drag(&mut o, (1100, 10), (2400, 300));
        let s = o.scene(&Approx);
        let h = s.help.unwrap();
        assert!(
            h.rect.bottom() <= 1200 - 36 && h.rect.y > 300,
            "{:?}",
            h.rect
        );
        assert_eq!(s.chip.unwrap().lines, ["1301 \u{d7} 291  at 1100, 10"]);
        assert_eq!(s.handles.len(), 8);
        assert!(
            s.handles.iter().all(|r| r.width == 11),
            "7 logical px at 150 %"
        );
        press(&mut o, Button::Left, 1500, 600);
        mv(&mut o, 1600, 700);
        let s = o.scene(&Approx);
        assert!(
            s.help.is_none() && s.handles.is_empty(),
            "nothing extra while dragging"
        );
        // A banner replaces the help.
        release(&mut o, 1600, 700);
        o.set_banner(Some("The pin could not be created.".into()));
        assert_eq!(
            o.scene(&Approx).help.unwrap().lines,
            ["The pin could not be created."]
        );
        o.set_banner(None);
        o.hide_help();
        assert!(o.scene(&Approx).help.is_none());
    }

    #[test]
    fn the_border_stays_visible_on_the_picture_edge() {
        let mut o = one(200, 100);
        o.handle(Input::Action(Action::Full));
        let s = o.scene(&Approx);
        assert_eq!(s.selection, Some(Rect::new(0, 0, 200, 100)));
        assert_eq!(s.accent, Some(Rect::new(0, 0, 200, 100)), "drawn inside");
        assert!(
            s.handles
                .iter()
                .all(|h| Rect::new(0, 0, 200, 100).contains(h))
        );
        // The chip moves inside when there is no room above.
        let c = s.chip.unwrap();
        assert!(c.rect.y > 0 && c.rect.x > 0);
    }

    #[test]
    fn dirty_covers_exactly_what_changed() {
        let mut o = one(400, 300);
        drag(&mut o, (100, 100), (199, 199));
        let a = o.scene(&Approx);
        assert!(dirty(&a, &a).is_empty(), "no change, no work");
        o.handle(Input::Action(Action::Move(Dir::Right)));
        let b = o.scene(&Approx);
        let d = dirty(&a, &b);
        // The column that became dimmed and the one that became plain are both covered.
        let covered = |x: i32, y: i32| d.iter().any(|r| r.contains(&Rect::new(x, y, 1, 1)));
        assert!(covered(100, 150) && covered(200, 150));
        assert!(!covered(150, 150), "the unchanged middle is not redrawn");
        assert!(covered(97, 97), "old halo");
        // A full repaint area: every pixel of the new scene composed into a buffer equals
        // composing only the dirty parts over the old picture.
        let ledger = Ledger::new();
        let syn = Synthetic::new(vec![Monitor::new(Rect::new(0, 0, 400, 300), true)]);
        let frozen =
            crate::capture::CaptureBackend::capture(&syn, &ledger, Rect::new(0, 0, 400, 300))
                .unwrap();
        let mut full = vec![0u8; 400 * 300 * 4];
        compose(&frozen, &b, b.picture, &mut full);
        let mut inc = vec![0u8; 400 * 300 * 4];
        compose(&frozen, &a, a.picture, &mut inc);
        for r in &d {
            compose(&frozen, &b, *r, &mut inc);
        }
        assert!(full == inc, "incremental composition equals a full one");
    }

    #[test]
    fn compose_dims_outside_keeps_inside_and_draws_the_border() {
        let ledger = Ledger::new();
        let pic = Rect::new(-50, -20, 300, 200);
        let syn = Synthetic::new(vec![Monitor::new(pic, true)]);
        let frozen = crate::capture::CaptureBackend::capture(&syn, &ledger, pic).unwrap();
        let mut o = Overlay::new(
            pic,
            vec![Monitor::new(pic, true)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        drag(&mut o, (-40, -10), (59, 59)); // 100×70 at (-40,-10)
        let scene = o.scene(&Approx);
        let mut out = vec![0u8; 300 * 200 * 4];
        compose(&frozen, &scene, pic, &mut out);
        let at = |x: i32, y: i32| {
            let i = (((y - pic.y) * 300 + (x - pic.x)) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        assert_eq!(
            at(0, 40),
            Synthetic::pixel(0, 40),
            "inside: the frozen pixel"
        );
        assert_eq!(
            at(200, 150),
            dim_pixel(Synthetic::pixel(200, 150)),
            "outside: dimmed"
        );
        assert_eq!(at(0, -11), color::ACCENT, "border one pixel outside");
        assert_eq!(at(0, -12), color::HALO, "halo beyond it");
        assert_eq!(at(0, 60), color::ACCENT);
        assert_eq!(at(-40, 25), color::ACCENT, "inside a handle");
        let chip = scene.chip.as_ref().unwrap().rect;
        assert_eq!(at(chip.x, chip.y), color::BORDER_SUBTLE);
        assert_eq!(at(chip.x + 2, chip.y + 2), color::BG_OVERLAY);
        let d = dim_pixel([200, 100, 0, 255]);
        assert_eq!(d, [110, 55, 0, 255], "about 45 % darker");
        // The frozen picture itself is never changed.
        assert_eq!(&frozen.pixels()[..4], &Synthetic::pixel(-50, -20));
    }

    #[test]
    fn rect_helpers() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            intersection(a, Rect::new(5, 5, 10, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(intersection(a, Rect::new(10, 0, 5, 5)), None);
        let parts = subtract(a, Rect::new(2, 2, 3, 3));
        let area: u64 = parts
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        assert_eq!(area, 100 - 9);
        assert_eq!(subtract(a, a), Vec::<Rect>::new());
        assert_eq!(subtract(a, Rect::new(50, 50, 1, 1)), vec![a]);
        assert_eq!(grow(a, 2), Rect::new(-2, -2, 14, 14));
        assert_eq!(grow(a, -1), Rect::new(1, 1, 8, 8));
    }
}
