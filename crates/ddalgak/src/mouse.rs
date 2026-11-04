//! Mouse actions: `mouse_move`, `mouse_scroll`, `mouse_click` (docs/DDALGAK_RULE_MODEL.md
//! §5.7–§5.9; `DDALGAK-FR-015`, `DDALGAK-SEC-002`, `DDALGAK-SEC-005`, `DDALGAK-NFR-001`; ADR 0040).
//!
//! Everything here is portable and pure: the platform supplies a [`Screen`] (the monitor layout,
//! the pointer position, the active window's content area and who would receive input at a
//! point) and an injection [`Backend`](crate::inject::Backend); this module decides every event.
//!
//! **Coordinates.** The engine works in physical virtual-desktop pixels (what a per-monitor DPI
//! aware program sees; the Windows layer reads every position in that mode). Rule coordinates are
//! logical pixels scaled by the DPI of the monitor they refer to: a relative `dx` by the monitor
//! the pointer is on, a `primary_monitor` point by the primary monitor, an `active_window` point
//! by the window's monitor. A `virtual_screen` point names a physical desktop pixel directly,
//! because a point spanning monitors of different scales has no single logical size (ADR 0040).
//!
//! **Never outside the desktop.** Every target goes through [`Layout::clamp`]: the nearest pixel
//! on any monitor, so a target in the gap between monitors of different heights or beyond the
//! last monitor lands on the nearest edge (`clamped: true`). Absolute moves are sent as
//! normalized `MOUSEEVENTF_VIRTUALDESK` coordinates (0–65535 over the whole virtual desktop) of
//! that pixel ([`Layout::normalize`]); relative moves are also sent that way, from the pointer
//! position read just before, so mouse acceleration settings never change the distance and a
//! move can never leave the desktop.
//!
//! **Held actions.** A `mouse_click` with `op: hold` is a remap whose target is the button (the
//! engine core's reference-counted slots and the worker's ledger release it on every path of
//! `DDALGAK-SEC-005`). A `mouse_move` with `while_held` is a [`MotionSpec`]: continuous
//! ([`Motion`]: speed, linear acceleration over the first second, clamped to [`MAX_SPEED`], at
//! most [`MAX_TICK_MS`] of distance per tick so a late tick never jumps) or stepped (the step at
//! once, then after `delay_ms` every `repeat_ms`, in pixels or grid cells). The worker ticks
//! motions only while one is active, so an idle engine still has no timer.
//!
//! **Safe preview.** [`SimPointer`] is a simulated pointer that is both a `Screen` and an
//! injection backend: it applies the events the real code path would send to a pretend cursor.
//! The playground preview ([`Playground`]), the debug test host and the tests use it; it never
//! moves, scrolls or clicks the real pointer.
//!
//! No pointer tracking: positions are read only when a mouse rule runs (`DDALGAK-SEC-001`).

use crate::inject::{Backend, Button, Injector, OutCode, OutEvent};
use crate::remap::Field;
use crate::text::FieldProblem;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// One wheel notch (`WHEEL_DELTA`, *documented*).
pub const WHEEL_DELTA: i16 = 120;
/// The DPI of 100 % scaling.
pub const BASE_DPI: u32 = 96;
/// The fastest continuous movement after acceleration, in logical pixels per second.
pub const MAX_SPEED: u32 = 10_000;
/// How often a continuous motion moves the pointer.
pub const TICK_MS: u32 = 10;
/// A tick that comes late moves at most this much time's distance (no jump after a stall).
pub const MAX_TICK_MS: u32 = 50;
/// Acceleration grows the speed linearly over this time, then it stays.
pub const RAMP_MS: u32 = 1000;
/// The first repeat of a stepped hold, unless the rule says otherwise.
pub const DEFAULT_DELAY_MS: u32 = 300;
/// A held motion ends with `timeout` after this long: the safety net for a trigger release the
/// hook never saw (the engine's run limit, DDALGAK_RULE_MODEL §5.12).
pub const MAX_HOLD_MS: u64 = 60_000;
/// The largest normalized coordinate.
const NORM_MAX: i64 = 65_535;

/// A point in physical virtual-desktop pixels (may be negative left of or above the primary
/// monitor).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn to_json(self) -> Value {
        json!({ "x": self.x, "y": self.y })
    }
}

/// A rectangle; `right` and `bottom` are exclusive (as `RECT`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    /// `width` × `height` pixels with the top-left corner at (`left`, `top`).
    pub fn sized(left: i32, top: i32, width: i32, height: i32) -> Rect {
        Rect::new(left, top, left + width, top + height)
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left && p.x < self.right && p.y >= self.top && p.y < self.bottom
    }

    /// The pixel of this rectangle nearest to `p`.
    pub fn clamp(&self, p: Point) -> Point {
        Point {
            x: p.x.clamp(self.left, self.right - 1),
            y: p.y.clamp(self.top, self.bottom - 1),
        }
    }

    fn union(&self, o: &Rect) -> Rect {
        Rect::new(
            self.left.min(o.left),
            self.top.min(o.top),
            self.right.max(o.right),
            self.bottom.max(o.bottom),
        )
    }
}

/// One monitor: its rectangle in physical pixels and its effective DPI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Monitor {
    pub rect: Rect,
    pub dpi: u32,
    pub primary: bool,
}

impl Monitor {
    pub fn new(rect: Rect, dpi: u32, primary: bool) -> Monitor {
        Monitor { rect, dpi, primary }
    }

    /// The scale in percent (96 DPI = 100 %).
    pub fn scale_percent(&self) -> u32 {
        (self.dpi * 100 + BASE_DPI / 2) / BASE_DPI
    }
}

/// Logical pixels at `dpi` in physical pixels, rounded half away from zero.
pub fn to_physical(logical: i32, dpi: u32) -> i32 {
    let num = i64::from(logical) * i64::from(dpi.max(1)) * 2;
    let den = i64::from(BASE_DPI) * 2;
    let half = i64::from(BASE_DPI);
    let v = if num >= 0 {
        (num + half) / den
    } else {
        (num - half) / den
    };
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// The monitors of the desktop (at least one).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    monitors: Vec<Monitor>,
}

impl Layout {
    /// A layout of the non-empty monitors; `None` without one. When no monitor is marked primary,
    /// the one at (0, 0) (or the first) is.
    pub fn new(monitors: Vec<Monitor>) -> Option<Layout> {
        let mut monitors: Vec<Monitor> = monitors
            .into_iter()
            .filter(|m| !m.rect.is_empty())
            .map(|m| Monitor {
                dpi: m.dpi.clamp(48, 960),
                ..m
            })
            .collect();
        if monitors.is_empty() {
            return None;
        }
        if !monitors.iter().any(|m| m.primary) {
            let i = monitors
                .iter()
                .position(|m| m.rect.contains(Point::default()))
                .unwrap_or(0);
            monitors[i].primary = true;
        }
        let first = monitors.iter().position(|m| m.primary).unwrap_or(0);
        for (i, m) in monitors.iter_mut().enumerate() {
            m.primary = i == first;
        }
        Some(Layout { monitors })
    }

    /// One monitor of `width` × `height` at 100 % with its top-left corner at (0, 0) (tests, the
    /// debug test host, a playground without monitor information).
    pub fn single(width: i32, height: i32) -> Layout {
        Layout {
            monitors: vec![Monitor::new(
                Rect::sized(0, 0, width.max(1), height.max(1)),
                BASE_DPI,
                true,
            )],
        }
    }

    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    pub fn primary(&self) -> &Monitor {
        self.monitors
            .iter()
            .find(|m| m.primary)
            .unwrap_or(&self.monitors[0])
    }

    /// The bounding rectangle of every monitor (the virtual screen).
    pub fn bounds(&self) -> Rect {
        self.monitors
            .iter()
            .skip(1)
            .fold(self.monitors[0].rect, |a, m| a.union(&m.rect))
    }

    pub fn monitor_at(&self, p: Point) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.rect.contains(p))
    }

    /// The pixel on a monitor nearest to `p`, the monitor, and whether `p` was outside every
    /// monitor.
    pub fn nearest(&self, p: Point) -> (Point, &Monitor, bool) {
        if let Some(m) = self.monitor_at(p) {
            return (p, m, false);
        }
        let mut best = (self.monitors[0].rect.clamp(p), &self.monitors[0]);
        let dist = |q: Point| {
            let (dx, dy) = (i64::from(q.x - p.x), i64::from(q.y - p.y));
            dx * dx + dy * dy
        };
        for m in &self.monitors[1..] {
            let q = m.rect.clamp(p);
            if dist(q) < dist(best.0) {
                best = (q, m);
            }
        }
        (best.0, best.1, true)
    }

    /// `p` moved onto the visible desktop, and whether it had to be moved.
    pub fn clamp(&self, p: Point) -> (Point, bool) {
        let (q, _, clamped) = self.nearest(p);
        (q, clamped)
    }

    /// The monitor `p` is on (or nearest to).
    pub fn monitor_near(&self, p: Point) -> &Monitor {
        self.nearest(p).1
    }

    /// The normalized `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` coordinates of a pixel:
    /// 0–65535 over the virtual screen, the middle of the pixel's interval so either rounding
    /// Windows may apply lands on that pixel. `p` is clamped to the virtual screen first.
    pub fn normalize(&self, p: Point) -> (u16, u16) {
        let b = self.bounds();
        let axis = |v: i32, lo: i32, len: i32| -> u16 {
            let len = i64::from(len.max(1));
            let off = i64::from(v.clamp(lo, lo + i32::try_from(len).unwrap_or(1) - 1) - lo);
            let n = ((2 * off + 1) * 65_536 / (2 * len)).clamp(0, NORM_MAX);
            u16::try_from(n).unwrap_or(u16::MAX)
        };
        (axis(p.x, b.left, b.width()), axis(p.y, b.top, b.height()))
    }

    /// The pixel Windows maps normalized coordinates to (`left + n × width / 65536`, the inverse
    /// of [`Layout::normalize`]; tests and the simulated pointer).
    pub fn denormalize(&self, n: (u16, u16)) -> Point {
        let b = self.bounds();
        let axis = |n: u16, lo: i32, len: i32| -> i32 {
            let v = i64::from(n) * i64::from(len.max(1)) / 65_536;
            lo + i32::try_from(v).unwrap_or(0)
        };
        Point {
            x: axis(n.0, b.left, b.width()),
            y: axis(n.1, b.top, b.height()),
        }
    }
}

/// What a rule point is relative to (`relative_to`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Anchor {
    VirtualScreen,
    PrimaryMonitor,
    ActiveWindow,
}

impl Anchor {
    pub fn parse(s: &str) -> Option<Anchor> {
        Some(match s {
            "virtual_screen" => Anchor::VirtualScreen,
            "primary_monitor" => Anchor::PrimaryMonitor,
            "active_window" => Anchor::ActiveWindow,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Anchor::VirtualScreen => "virtual_screen",
            Anchor::PrimaryMonitor => "primary_monitor",
            Anchor::ActiveWindow => "active_window",
        }
    }
}

/// A point of a rule (`to`, `at`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PointSpec {
    pub x: i32,
    pub y: i32,
    pub anchor: Anchor,
}

/// The active window's content area: its top-left corner in physical pixels and its DPI.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowArea {
    pub origin: Point,
    pub dpi: u32,
}

/// Why a point cannot be resolved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unresolved {
    /// `relative_to: active_window` and no window is in the foreground.
    NoWindow,
}

/// The physical pixel a rule point names, before clamping.
pub fn resolve(
    spec: PointSpec,
    layout: &Layout,
    window: Option<WindowArea>,
) -> Result<Point, Unresolved> {
    Ok(match spec.anchor {
        Anchor::VirtualScreen => Point::new(spec.x, spec.y),
        Anchor::PrimaryMonitor => {
            let m = layout.primary();
            Point::new(
                m.rect.left + to_physical(spec.x, m.dpi),
                m.rect.top + to_physical(spec.y, m.dpi),
            )
        }
        Anchor::ActiveWindow => {
            let w = window.ok_or(Unresolved::NoWindow)?;
            Point::new(
                w.origin.x.saturating_add(to_physical(spec.x, w.dpi)),
                w.origin.y.saturating_add(to_physical(spec.y, w.dpi)),
            )
        }
    })
}

/// A virtual grid over each monitor (`unit: grid`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grid {
    pub columns: u32,
    pub rows: u32,
}

/// How `dx`/`dy` are counted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    /// Logical pixels scaled by the DPI of the monitor the pointer is on.
    Pixels,
    /// Cells of the grid on the monitor the pointer is on; the pointer lands on cell centres.
    Grid(Grid),
}

/// The part of the segment `a` → `b` (as parameters 0…1) inside a monitor's area, with its right
/// and bottom edges included so that monitors side by side touch (Liang–Barsky clipping).
fn clip(a: Point, b: Point, r: Rect) -> Option<(f64, f64)> {
    let (x0, y0) = (f64::from(a.x), f64::from(a.y));
    let (dx, dy) = (f64::from(b.x) - x0, f64::from(b.y) - y0);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, x0 - f64::from(r.left)),
        (dx, f64::from(r.right) - x0),
        (-dy, y0 - f64::from(r.top)),
        (dy, f64::from(r.bottom) - y0),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else if p < 0.0 {
            t0 = t0.max(q / p);
        } else {
            t1 = t1.min(q / p);
        }
    }
    (t0 <= t1).then_some((t0, t1))
}

/// Where the pointer gets when it travels from `from` towards `to`: the whole way when the path
/// stays on monitors (crossing from one onto the next), otherwise the last pixel before the path
/// leaves the desktop, the way a physical mouse stops at an edge with no monitor behind it. The
/// flag says whether it stopped early.
pub fn reach(layout: &Layout, from: Point, to: Point) -> (Point, bool) {
    let (from, _, _) = layout.nearest(from);
    if from == to {
        return (to, false);
    }
    let mut spans: Vec<(f64, f64)> = layout
        .monitors()
        .iter()
        .filter_map(|m| clip(from, to, m.rect))
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut covered = 0.0f64;
    for (t0, t1) in spans {
        if t0 <= covered + 1e-9 {
            covered = covered.max(t1);
        }
    }
    if covered >= 1.0 && layout.monitor_at(to).is_some() {
        return (to, false);
    }
    let t = covered.min(1.0);
    let lerp = |a: i32, b: i32| -> i32 {
        // Within the segment's own bounding box: no overflow.
        (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as i32
    };
    let (p, _) = layout.clamp(Point::new(lerp(from.x, to.x), lerp(from.y, to.y)));
    (p, true)
}

/// The pointer `dx`, `dy` logical pixels from `from` (scaled by the DPI of the monitor `from`
/// is on), along the way on the desktop ([`reach`]), and whether it stopped at an edge.
pub fn relative_target(layout: &Layout, from: Point, dx: i32, dy: i32) -> (Point, bool) {
    let (from, m, _) = layout.nearest(from);
    let p = Point::new(
        from.x.saturating_add(to_physical(dx, m.dpi)),
        from.y.saturating_add(to_physical(dy, m.dpi)),
    );
    reach(layout, from, p)
}

/// The cell of `grid` the pointer is in on its monitor.
pub fn grid_cell(layout: &Layout, from: Point, grid: Grid) -> (i64, i64) {
    let (from, m, _) = layout.nearest(from);
    let r = m.rect;
    let col = i64::from(from.x - r.left) * i64::from(grid.columns) / i64::from(r.width());
    let row = i64::from(from.y - r.top) * i64::from(grid.rows) / i64::from(r.height());
    (col, row)
}

/// The centre of the cell `dx`, `dy` cells from the pointer's cell on the pointer's monitor; the
/// cell stays on the grid (`clamped` when it had to).
pub fn grid_target(layout: &Layout, from: Point, dx: i32, dy: i32, grid: Grid) -> (Point, bool) {
    let grid = Grid {
        columns: grid.columns.max(1),
        rows: grid.rows.max(1),
    };
    let (from, m, _) = layout.nearest(from);
    let r = m.rect;
    let (col, row) = grid_cell(layout, from, grid);
    let (cols, rows) = (i64::from(grid.columns), i64::from(grid.rows));
    let want = (col + i64::from(dx), row + i64::from(dy));
    let c = want.0.clamp(0, cols - 1);
    let w = want.1.clamp(0, rows - 1);
    let clamped = (c, w) != want;
    let centre = |lo: i32, len: i32, i: i64, n: i64| -> i32 {
        lo + i32::try_from((2 * i + 1) * i64::from(len) / (2 * n)).unwrap_or(0)
    };
    (
        Point::new(
            centre(r.left, r.width(), c, cols),
            centre(r.top, r.height(), w, rows),
        ),
        clamped,
    )
}

/// The speed of a continuous motion after `held_ms` (logical px/s): linear acceleration over
/// [`RAMP_MS`] up to `speed × (1 + acceleration)`, never above [`MAX_SPEED`].
pub fn speed_at(speed: u32, acceleration: u32, held_ms: u64) -> f64 {
    let t = held_ms.min(u64::from(RAMP_MS)) as f64 / f64::from(RAMP_MS);
    let v = f64::from(speed) * (1.0 + f64::from(acceleration) * t);
    v.min(f64::from(MAX_SPEED))
}

/// A continuous motion's integrator: direction, speed and the sub-pixel remainder.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Motion {
    ux: f64,
    uy: f64,
    speed: u32,
    acceleration: u32,
    rx: f64,
    ry: f64,
}

impl Motion {
    /// A motion in the `dx`, `dy` direction (normalized: a diagonal is as fast as a straight
    /// line); `None` without a direction.
    pub fn new(dx: i32, dy: i32, speed: u32, acceleration: u32) -> Option<Motion> {
        let len = f64::from(dx).hypot(f64::from(dy));
        (len > 0.0).then(|| Motion {
            ux: f64::from(dx) / len,
            uy: f64::from(dy) / len,
            speed,
            acceleration,
            rx: 0.0,
            ry: 0.0,
        })
    }

    /// The whole physical pixels to move for a tick of `dt_ms` (at most [`MAX_TICK_MS`]) after
    /// the trigger was held `held_ms`, on a monitor of `dpi`; the fraction is kept for the next.
    pub fn advance(&mut self, held_ms: u64, dt_ms: u64, dpi: u32) -> (i32, i32) {
        let dt = dt_ms.min(u64::from(MAX_TICK_MS)) as f64 / 1000.0;
        let px = speed_at(self.speed, self.acceleration, held_ms) * dt * f64::from(dpi)
            / f64::from(BASE_DPI);
        self.rx += self.ux * px;
        self.ry += self.uy * px;
        let (sx, sy) = (self.rx.trunc(), self.ry.trunc());
        self.rx -= sx;
        self.ry -= sy;
        // At most MAX_SPEED × MAX_TICK_MS × 10 (960 DPI): far inside i32.
        (sx as i32, sy as i32)
    }
}

/// A wheel direction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScrollDir {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollDir {
    pub fn parse(s: &str) -> Option<ScrollDir> {
        Some(match s {
            "up" => ScrollDir::Up,
            "down" => ScrollDir::Down,
            "left" => ScrollDir::Left,
            "right" => ScrollDir::Right,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ScrollDir::Up => "up",
            ScrollDir::Down => "down",
            ScrollDir::Left => "left",
            ScrollDir::Right => "right",
        }
    }

    /// `(horizontal, delta per notch)`: up is a positive `WHEEL` delta (away from the user),
    /// right a positive `HWHEEL` delta (tilt right), as Windows defines them.
    pub fn wheel(self) -> (bool, i16) {
        match self {
            ScrollDir::Up => (false, WHEEL_DELTA),
            ScrollDir::Down => (false, -WHEEL_DELTA),
            ScrollDir::Right => (true, WHEEL_DELTA),
            ScrollDir::Left => (true, -WHEEL_DELTA),
        }
    }
}

/// A button by its rule name (`left` … `x2`).
pub fn button(name: &str) -> Option<Button> {
    Some(match name {
        "left" => Button::Left,
        "right" => Button::Right,
        "middle" => Button::Middle,
        "x1" => Button::X1,
        "x2" => Button::X2,
        _ => return None,
    })
}

/// The rule name of a button (`left` … `x2`).
pub fn button_name(b: Button) -> &'static str {
    match b {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
        Button::X1 => "x1",
        Button::X2 => "x2",
    }
}

fn button_words(b: Button) -> &'static str {
    match b {
        Button::Left => "the left button",
        Button::Right => "the right button",
        Button::Middle => "the middle button",
        Button::X1 => "the back button (X1)",
        Button::X2 => "the forward button (X2)",
    }
}

/// Where a move goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveTo {
    By { dx: i32, dy: i32, unit: Unit },
    At(PointSpec),
}

/// One runnable mouse step (not a held one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseStep {
    Move(MoveTo),
    Scroll {
        dir: ScrollDir,
        notches: u32,
    },
    Click {
        button: Button,
        double: bool,
        at: Option<PointSpec>,
    },
}

/// Mouse steps run together, in order, as one run (Step 71).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MousePlan {
    pub steps: Vec<MouseStep>,
    /// `on_error: stop` (default): a failing step ends the run.
    pub stop_on_error: bool,
}

/// How a held move goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionKind {
    /// Smoothly at `speed` logical px/s with `acceleration` (0–10).
    Continuous { speed: u32, acceleration: u32 },
    /// The step at once, after `delay_ms` again, then every `repeat_ms`.
    Stepped { repeat_ms: u32, delay_ms: u32 },
}

/// A `mouse_move` with `while_held`: it lasts exactly as long as the trigger is held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MotionSpec {
    pub dx: i32,
    pub dy: i32,
    pub unit: Unit,
    pub kind: MotionKind,
}

/// A compiled mouse step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compiled {
    /// Runs at once, with the rule's other mouse steps.
    Run(MouseStep),
    /// `mouse_click` with `op: hold`: the button is down exactly as long as the trigger.
    Hold {
        button: Button,
        at: Option<PointSpec>,
    },
    /// `mouse_move` with `while_held`.
    Motion(MotionSpec),
}

fn point_spec(v: &Value) -> Result<PointSpec, String> {
    let n = |k: &str| {
        v.get(k)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (-32768..=32767).contains(n))
            .ok_or_else(|| format!("a point needs an integer `{k}` (−32768…32767)"))
    };
    let anchor = match v.get("relative_to") {
        None => Anchor::VirtualScreen,
        Some(r) => r
            .as_str()
            .and_then(Anchor::parse)
            .ok_or("`relative_to` is virtual_screen, primary_monitor or active_window")?,
    };
    Ok(PointSpec {
        x: n("x")?,
        y: n("y")?,
        anchor,
    })
}

fn int_in(v: &Value, k: &str, lo: i64, hi: i64) -> Result<Option<i64>, String> {
    match v.get(k) {
        None => Ok(None),
        Some(x) => x
            .as_i64()
            .filter(|n| (lo..=hi).contains(n))
            .map(Some)
            .ok_or_else(|| format!("`{k}` is an integer {lo}–{hi}")),
    }
}

fn small(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

fn int(n: i64) -> i32 {
    i32::try_from(n).unwrap_or(0)
}

/// Compiles one mouse step of a checked rule (the schema already refused most of this; these
/// checks are the last line of defense, "never broader").
pub fn compile_step(s: &Value) -> Result<Compiled, String> {
    match s.get("type").and_then(Value::as_str) {
        Some("mouse_scroll") => {
            let dir = s
                .get("direction")
                .and_then(Value::as_str)
                .and_then(ScrollDir::parse)
                .ok_or("`direction` is up, down, left or right")?;
            let notches = int_in(s, "notches", 1, 50)?.map_or(1, small);
            Ok(Compiled::Run(MouseStep::Scroll { dir, notches }))
        }
        Some("mouse_click") => {
            let b = s
                .get("button")
                .and_then(Value::as_str)
                .and_then(button)
                .ok_or("`button` is left, right, middle, x1 or x2")?;
            let at = s.get("at").map(point_spec).transpose()?;
            match s.get("op").and_then(Value::as_str) {
                None | Some("click") => Ok(Compiled::Run(MouseStep::Click {
                    button: b,
                    double: false,
                    at,
                })),
                Some("double_click") => Ok(Compiled::Run(MouseStep::Click {
                    button: b,
                    double: true,
                    at,
                })),
                Some("hold") => Ok(Compiled::Hold { button: b, at }),
                Some(_) => Err("`op` is click, double_click or hold".into()),
            }
        }
        Some("mouse_move") => compile_move(s),
        _ => Err("not a mouse step".into()),
    }
}

fn compile_move(s: &Value) -> Result<Compiled, String> {
    let unit = match (s.get("unit").and_then(Value::as_str), s.get("grid")) {
        (None | Some("pixels"), None) => Unit::Pixels,
        (Some("grid"), Some(g)) => Unit::Grid(Grid {
            columns: int_in(g, "columns", 2, 64)?.map_or(0, small),
            rows: int_in(g, "rows", 2, 64)?.map_or(0, small),
        }),
        _ => return Err("`unit: grid` and `grid` go together".into()),
    };
    if let Unit::Grid(g) = unit
        && (g.columns == 0 || g.rows == 0)
    {
        return Err("a grid needs `columns` and `rows` (2–64)".into());
    }
    match s.get("mode").and_then(Value::as_str) {
        Some("absolute") => {
            if s.get("dx").is_some() || s.get("dy").is_some() || s.get("while_held").is_some() {
                return Err("an absolute move has `to` only".into());
            }
            if unit != Unit::Pixels {
                return Err("grid cells are for relative moves".into());
            }
            let to = s
                .get("to")
                .map(point_spec)
                .transpose()?
                .ok_or("an absolute move needs `to`")?;
            Ok(Compiled::Run(MouseStep::Move(MoveTo::At(to))))
        }
        Some("relative") => {
            if s.get("to").is_some() {
                return Err("a relative move has `dx` and `dy`, not `to`".into());
            }
            let dx = int_in(s, "dx", -10_000, 10_000)?.ok_or("a relative move needs `dx`")?;
            let dy = int_in(s, "dy", -10_000, 10_000)?.ok_or("a relative move needs `dy`")?;
            let (dx, dy) = (int(dx), int(dy));
            let Some(w) = s.get("while_held") else {
                return Ok(Compiled::Run(MouseStep::Move(MoveTo::By { dx, dy, unit })));
            };
            if dx == 0 && dy == 0 {
                return Err("a move while held needs a direction (`dx` or `dy` not 0)".into());
            }
            let speed = int_in(w, "speed", 50, 5000)?;
            let repeat = int_in(w, "repeat_ms", 20, 2000)?;
            let kind = match (speed, repeat) {
                (Some(speed), None) => {
                    if w.get("delay_ms").is_some() {
                        return Err("`delay_ms` belongs to a stepped hold (`repeat_ms`)".into());
                    }
                    if unit != Unit::Pixels {
                        return Err("grid moves are steps: use `repeat_ms`, not `speed`".into());
                    }
                    MotionKind::Continuous {
                        speed: small(speed),
                        acceleration: int_in(w, "acceleration", 0, 10)?.map_or(0, small),
                    }
                }
                (None, Some(repeat_ms)) => {
                    if w.get("acceleration").is_some() {
                        return Err("`acceleration` belongs to a continuous hold (`speed`)".into());
                    }
                    MotionKind::Stepped {
                        repeat_ms: small(repeat_ms),
                        delay_ms: int_in(w, "delay_ms", 0, 2000)?.map_or(DEFAULT_DELAY_MS, small),
                    }
                }
                _ => return Err("`while_held` has `speed` or `repeat_ms`, not both".into()),
            };
            Ok(Compiled::Motion(MotionSpec { dx, dy, unit, kind }))
        }
        _ => Err("`mode` is relative or absolute".into()),
    }
}

/// Who would receive pointer input at a point.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PointerTarget {
    /// The session is locked or the secure desktop is shown.
    pub secure_desktop: bool,
    /// The window with mouse capture, or else the window at the point, belongs to a process with
    /// a higher integrity level (UIPI would drop the input silently).
    pub elevated: bool,
}

/// The platform's pointer facts (Windows: monitor enumeration with each monitor's effective DPI,
/// `GetCursorPos`, the foreground window's client area, the capture window or the window at a
/// point and its integrity level; every position in physical pixels). Read-only.
pub trait Screen: Send {
    fn layout(&mut self) -> Option<Layout>;
    fn cursor(&mut self) -> Option<Point>;
    /// The foreground window's content area.
    fn window(&mut self) -> Option<WindowArea>;
    fn target(&mut self, at: Point) -> PointerTarget;
}

/// The state of a [`SimPointer`].
#[derive(Clone, Debug)]
pub struct SimState {
    pub layout: Layout,
    pub cursor: Point,
    pub window: Option<WindowArea>,
    pub target: PointerTarget,
    /// Buttons held (logical).
    pub buttons: Vec<Button>,
    /// Wheel notches received: vertical (positive up) and horizontal (positive right).
    pub wheel: (i32, i32),
    /// Every event the backend received, keys included.
    pub events: Vec<OutEvent>,
    /// Insert at most this many events per call (a simulated partial `SendInput`).
    pub limit: Option<usize>,
}

/// A simulated pointer: a [`Screen`] and an injection backend over the same pretend cursor.
/// Moves, clicks and wheel notches change only this state, never the real pointer.
#[derive(Clone, Debug)]
pub struct SimPointer(pub Arc<Mutex<SimState>>);

impl SimPointer {
    pub fn new(layout: Layout, cursor: Point) -> SimPointer {
        SimPointer(Arc::new(Mutex::new(SimState {
            layout,
            cursor,
            window: None,
            target: PointerTarget::default(),
            buttons: Vec::new(),
            wheel: (0, 0),
            events: Vec::new(),
            limit: None,
        })))
    }

    /// The test host's pointer: one 1920 × 1080 monitor, the pointer in its middle.
    pub fn test_host() -> SimPointer {
        SimPointer::new(Layout::single(1920, 1080), Point::new(960, 540))
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut SimState) -> R) -> R {
        let mut g = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut g)
    }

    pub fn cursor(&self) -> Point {
        self.with(|s| s.cursor)
    }

    pub fn events(&self) -> Vec<OutEvent> {
        self.with(|s| s.events.clone())
    }

    /// Forgets the recorded events.
    pub fn clear(&self) {
        self.with(|s| s.events.clear());
    }

    /// The pointer positions of the recorded moves, in order.
    pub fn moves(&self) -> Vec<Point> {
        self.with(|s| {
            s.events
                .iter()
                .filter_map(|e| match e.code {
                    OutCode::MoveTo { x, y } => Some(s.layout.denormalize((x, y))),
                    _ => None,
                })
                .collect()
        })
    }

    pub fn screen(&self) -> SimScreen {
        SimScreen(self.clone())
    }

    pub fn backend(&self) -> SimBackend {
        SimBackend(self.clone())
    }
}

/// The [`Screen`] side of a [`SimPointer`].
#[derive(Clone, Debug)]
pub struct SimScreen(pub SimPointer);

impl Screen for SimScreen {
    fn layout(&mut self) -> Option<Layout> {
        Some(self.0.with(|s| s.layout.clone()))
    }

    fn cursor(&mut self) -> Option<Point> {
        Some(self.0.cursor())
    }

    fn window(&mut self) -> Option<WindowArea> {
        self.0.with(|s| s.window)
    }

    fn target(&mut self, _at: Point) -> PointerTarget {
        self.0.with(|s| s.target)
    }
}

/// The backend side of a [`SimPointer`]: records every event and applies mouse events to the
/// simulated pointer.
#[derive(Clone, Debug)]
pub struct SimBackend(pub SimPointer);

impl Backend for SimBackend {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        self.0.with(|s| {
            let n = s.limit.map_or(events.len(), |l| l.min(events.len()));
            for e in &events[..n] {
                s.events.push(*e);
                match e.code {
                    OutCode::MoveTo { x, y } => s.cursor = s.layout.denormalize((x, y)),
                    OutCode::Wheel { horizontal, delta } => {
                        let notches = i32::from(delta) / i32::from(WHEEL_DELTA);
                        if horizontal {
                            s.wheel.1 += notches;
                        } else {
                            s.wheel.0 += notches;
                        }
                    }
                    OutCode::Button(b) if e.up => s.buttons.retain(|x| *x != b),
                    OutCode::Button(b) if !s.buttons.contains(&b) => s.buttons.push(b),
                    _ => {}
                }
            }
            n
        })
    }
}

/// The event that moves the pointer to `p` (clamped onto the desktop first).
pub fn move_event(layout: &Layout, p: Point) -> OutEvent {
    let (p, _) = layout.clamp(p);
    let (x, y) = layout.normalize(p);
    OutEvent::down(OutCode::MoveTo { x, y })
}

/// A click (down, up) or a double click (down, up, down, up) of a logical button, in one call so
/// the two clicks arrive within the double-click time and at the same point.
pub fn click_events(b: Button, double: bool) -> Vec<OutEvent> {
    let one = [
        OutEvent::down(OutCode::Button(b)),
        OutEvent::up(OutCode::Button(b)),
    ];
    if double {
        [one, one].concat()
    } else {
        one.to_vec()
    }
}

/// One wheel event per notch (applications that handle one notch per message see each).
pub fn wheel_events(dir: ScrollDir, notches: u32) -> Vec<OutEvent> {
    let (horizontal, delta) = dir.wheel();
    (0..notches.clamp(1, 50))
        .map(|_| OutEvent::down(OutCode::Wheel { horizontal, delta }))
        .collect()
}

/// Where one step sends the pointer, from `from` (for a click or scroll without a point: the
/// pointer stays). Returns the target and whether it was clamped.
pub fn step_target(
    step: &MouseStep,
    layout: &Layout,
    from: Point,
    window: Option<WindowArea>,
) -> Result<(Point, bool), Unresolved> {
    Ok(match *step {
        MouseStep::Move(MoveTo::By {
            dx,
            dy,
            unit: Unit::Pixels,
        }) => relative_target(layout, from, dx, dy),
        MouseStep::Move(MoveTo::By {
            dx,
            dy,
            unit: Unit::Grid(g),
        }) => grid_target(layout, from, dx, dy, g),
        MouseStep::Move(MoveTo::At(spec)) | MouseStep::Click { at: Some(spec), .. } => {
            layout.clamp(resolve(spec, layout, window)?)
        }
        MouseStep::Click { at: None, .. } | MouseStep::Scroll { .. } => layout.clamp(from),
    })
}

/// The events of one step: a move (when the pointer goes somewhere), then the clicks or wheel
/// notches, all for one `SendInput` call.
pub fn step_events(step: &MouseStep, layout: &Layout, from: Point, to: Point) -> Vec<OutEvent> {
    let mut ev = Vec::new();
    let moves = matches!(
        step,
        MouseStep::Move(_) | MouseStep::Click { at: Some(_), .. }
    );
    if moves && to != from {
        ev.push(move_event(layout, to));
    }
    match *step {
        MouseStep::Click { button, double, .. } => ev.extend(click_events(button, double)),
        MouseStep::Scroll { dir, notches } => ev.extend(wheel_events(dir, notches)),
        MouseStep::Move(_) => {}
    }
    ev
}

/// The outcome code of a pointer target that refuses input, if it does.
pub fn refusal(t: PointerTarget) -> Option<&'static str> {
    if t.secure_desktop {
        Some("skipped_secure_desktop")
    } else if t.elevated {
        Some("skipped_elevated_target")
    } else {
        None
    }
}

/// Runs one mouse step: reads the layout, the pointer and (for `active_window`) the window,
/// checks who would receive the input at the target, and sends the step's events in one call.
/// Returns the outcome code (`ok`, `skipped_*`, `injection_failed`).
pub fn run_step(
    step: &MouseStep,
    screen: &mut dyn Screen,
    inj: &mut Injector<impl Backend>,
) -> &'static str {
    let Some(layout) = screen.layout() else {
        return "injection_failed";
    };
    let Some(from) = screen.cursor() else {
        return "skipped_secure_desktop"; // no pointer position: another desktop has the input
    };
    let window = match step {
        MouseStep::Move(MoveTo::At(s)) | MouseStep::Click { at: Some(s), .. }
            if s.anchor == Anchor::ActiveWindow =>
        {
            screen.window()
        }
        _ => None,
    };
    let Ok((to, _)) = step_target(step, &layout, from, window) else {
        return "skipped_disabled_field"; // relative to the active window, and there is none
    };
    if let Some(code) = refusal(screen.target(to)) {
        return code;
    }
    let ev = step_events(step, &layout, from, to);
    if inj.send_balanced(&ev) {
        "ok"
    } else {
        "injection_failed"
    }
}

/// Runs a plan's steps in order; the first failing step ends it unless `on_error: continue`.
/// Returns `ok` or the first failure.
pub fn run_plan(
    plan: &MousePlan,
    screen: &mut dyn Screen,
    inj: &mut Injector<impl Backend>,
) -> &'static str {
    let mut first = "ok";
    for s in &plan.steps {
        let code = run_step(s, screen, inj);
        if code != "ok" {
            if first == "ok" {
                first = code;
            }
            if plan.stop_on_error {
                break;
            }
        }
    }
    first
}

/// The point a held button goes down at, if the rule names one (moved there first).
pub fn hold_target(
    at: PointSpec,
    screen: &mut dyn Screen,
) -> Result<(Layout, Point, Point), &'static str> {
    let layout = screen.layout().ok_or("injection_failed")?;
    let from = screen.cursor().ok_or("skipped_secure_desktop")?;
    let window = if at.anchor == Anchor::ActiveWindow {
        screen.window()
    } else {
        None
    };
    let p = resolve(at, &layout, window).map_err(|_| "skipped_disabled_field")?;
    let (to, _) = layout.clamp(p);
    Ok((layout, from, to))
}

/// One held motion as the worker runs it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HeldMotion {
    pub spec: MotionSpec,
    /// Milliseconds since the trigger went down (the worker's clock).
    pub started_ms: u64,
    /// When the motion moves next (ms on the same clock).
    pub due_ms: u64,
    last_ms: u64,
    motion: Option<Motion>,
    steps: u32,
}

impl HeldMotion {
    /// A motion that starts at `now_ms`: a continuous one moves at its first tick, a stepped one
    /// at once.
    pub fn new(spec: MotionSpec, now_ms: u64) -> HeldMotion {
        let motion = match spec.kind {
            MotionKind::Continuous {
                speed,
                acceleration,
            } => Motion::new(spec.dx, spec.dy, speed, acceleration),
            MotionKind::Stepped { .. } => None,
        };
        let due_ms = match spec.kind {
            MotionKind::Continuous { .. } => now_ms + u64::from(TICK_MS),
            MotionKind::Stepped { .. } => now_ms,
        };
        HeldMotion {
            spec,
            started_ms: now_ms,
            due_ms,
            last_ms: now_ms,
            motion,
            steps: 0,
        }
    }

    /// Whether the motion ran out of time ([`MAX_HOLD_MS`]).
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.started_ms) >= MAX_HOLD_MS
    }

    /// Moves `p` by this motion's share of a tick at `now_ms` (when it is due) and schedules the
    /// next one. Returns the new point and whether a grid step had to stay on the grid.
    pub fn apply(&mut self, layout: &Layout, p: Point, now_ms: u64) -> (Point, bool) {
        if now_ms < self.due_ms {
            return (p, false);
        }
        match self.spec.kind {
            MotionKind::Continuous { .. } => {
                let dpi = layout.monitor_near(p).dpi;
                let held = now_ms.saturating_sub(self.started_ms);
                let dt = now_ms.saturating_sub(self.last_ms);
                let (dx, dy) = self
                    .motion
                    .as_mut()
                    .map_or((0, 0), |m| m.advance(held, dt, dpi));
                self.last_ms = now_ms;
                self.due_ms = now_ms + u64::from(TICK_MS);
                (
                    Point::new(p.x.saturating_add(dx), p.y.saturating_add(dy)),
                    false,
                )
            }
            MotionKind::Stepped {
                repeat_ms,
                delay_ms,
            } => {
                self.steps += 1;
                let wait = if self.steps == 1 { delay_ms } else { repeat_ms };
                self.due_ms = now_ms + u64::from(wait.max(1));
                let (dx, dy) = (self.spec.dx, self.spec.dy);
                match self.spec.unit {
                    Unit::Pixels => (relative_target(layout, p, dx, dy).0, false),
                    Unit::Grid(g) => grid_target(layout, p, dx, dy, g),
                }
            }
        }
    }
}

/// Moves `from` by every due motion at `now_ms`, along the way on the desktop ([`reach`]).
pub fn tick_motions(
    motions: &mut [HeldMotion],
    layout: &Layout,
    from: Point,
    now_ms: u64,
) -> (Point, bool) {
    let mut p = from;
    let mut clamped = false;
    for m in motions.iter_mut() {
        let (q, c) = m.apply(layout, p, now_ms);
        p = q;
        clamped |= c;
    }
    let (p, c) = reach(layout, from, p);
    (p, clamped || c)
}

// ---------------------------------------------------------------------------------------------
// Previews, summaries and the safe playground (DDALGAK_RULE_MODEL §11, GUI_JOURNEYS J4/J5).
// ---------------------------------------------------------------------------------------------

fn direction_words(dx: i32, dy: i32, unit: &str) -> String {
    let mut parts = Vec::new();
    let amount = |n: i32| {
        if unit == "cell" {
            if n.unsigned_abs() == 1 {
                "one cell".to_string()
            } else {
                format!("{} cells", n.unsigned_abs())
            }
        } else {
            format!("{} px", n.unsigned_abs())
        }
    };
    if dx != 0 {
        parts.push(format!(
            "{} {}",
            amount(dx),
            if dx > 0 { "right" } else { "left" }
        ));
    }
    if dy != 0 {
        parts.push(format!(
            "{} {}",
            amount(dy),
            if dy > 0 { "down" } else { "up" }
        ));
    }
    if parts.is_empty() {
        "nowhere".into()
    } else {
        parts.join(" and ")
    }
}

fn heading(dx: i32, dy: i32) -> &'static str {
    match (dx.signum(), dy.signum()) {
        (1, 0) => "right",
        (-1, 0) => "left",
        (0, -1) => "up",
        (0, 1) => "down",
        (1, -1) => "up and right",
        (-1, -1) => "up and left",
        (1, 1) => "down and right",
        (-1, 1) => "down and left",
        _ => "nowhere",
    }
}

fn point_words(p: PointSpec) -> String {
    let place = match p.anchor {
        Anchor::VirtualScreen => "on the desktop",
        Anchor::PrimaryMonitor => "on the primary monitor",
        Anchor::ActiveWindow => "in the active window",
    };
    format!("({}, {}) {place}", p.x, p.y)
}

/// The editor's and the playground's summary sentence of one mouse step (no personal content).
/// `trigger` names the keys for held steps ("while Right is held").
pub fn summary(c: &Compiled, trigger: &str) -> String {
    match *c {
        Compiled::Run(MouseStep::Move(MoveTo::By { dx, dy, unit })) => match unit {
            Unit::Pixels => format!("Moves the pointer {}", direction_words(dx, dy, "px")),
            Unit::Grid(g) => format!(
                "Moves the pointer {} on a {}×{} grid",
                direction_words(dx, dy, "cell"),
                g.columns,
                g.rows
            ),
        },
        Compiled::Run(MouseStep::Move(MoveTo::At(p))) => {
            format!("Moves the pointer to {}", point_words(p))
        }
        Compiled::Run(MouseStep::Scroll { dir, notches }) => format!(
            "Scrolls {} {notches} notch{}",
            dir.as_str(),
            if notches == 1 { "" } else { "es" }
        ),
        Compiled::Run(MouseStep::Click { button, double, at }) => {
            let mut s = format!(
                "{} {}",
                if double { "Double-clicks" } else { "Clicks" },
                button_words(button)
            );
            if let Some(p) = at {
                s.push_str(&format!(" at {}", point_words(p)));
            }
            s
        }
        Compiled::Hold { button, at } => {
            let mut s = format!("Holds {} while {trigger} is held", button_words(button));
            if let Some(p) = at {
                s.push_str(&format!(", starting at {}", point_words(p)));
            }
            s
        }
        Compiled::Motion(m) => match (m.kind, m.unit) {
            (MotionKind::Continuous { speed, .. }, _) => format!(
                "Moves the pointer {} at {speed} px/s while {trigger} is held",
                heading(m.dx, m.dy)
            ),
            (MotionKind::Stepped { repeat_ms, .. }, Unit::Pixels) => format!(
                "Moves the pointer {}, again every {repeat_ms} ms while {trigger} is held",
                direction_words(m.dx, m.dy, "px")
            ),
            (MotionKind::Stepped { repeat_ms, .. }, Unit::Grid(g)) => format!(
                "Moves the pointer {} on a {}×{} grid, again every {repeat_ms} ms while {trigger} is held",
                direction_words(m.dx, m.dy, "cell"),
                g.columns,
                g.rows
            ),
        },
    }
}

/// The playground preview of one mouse step (exactly the `preview_mouse_move`,
/// `preview_mouse_scroll` or `preview_mouse_click` shape of `ddalgak-simulation-v1`), from the
/// simulated pointer at `from` on `layout`. Nothing is sent anywhere.
pub fn preview(
    c: &Compiled,
    layout: &Layout,
    from: Point,
    window: Option<WindowArea>,
) -> Result<Value, Unresolved> {
    Ok(match *c {
        Compiled::Run(ref step @ MouseStep::Move(m)) => {
            let (to, clamped) = step_target(step, layout, from, window)?;
            let mut v = json!({ "to": to.to_json(), "clamped": clamped });
            if matches!(m, MoveTo::By { .. }) {
                v["from"] = from.to_json();
            }
            if let MoveTo::By {
                unit: Unit::Grid(_),
                ..
            } = m
            {
                v["unit"] = json!("grid");
            }
            v
        }
        Compiled::Run(MouseStep::Scroll { dir, notches }) => {
            json!({ "direction": dir.as_str(), "notches": notches })
        }
        Compiled::Run(ref step @ MouseStep::Click { button, double, at }) => {
            let mut v = json!({
                "button": button_name(button),
                "op": if double { "double_click" } else { "click" },
            });
            if at.is_some() {
                v["at"] = step_target(step, layout, from, window)?.0.to_json();
            }
            v
        }
        Compiled::Hold { button, at } => {
            let mut v = json!({ "button": button_name(button), "op": "hold" });
            if let Some(spec) = at {
                v["at"] = layout.clamp(resolve(spec, layout, window)?).0.to_json();
            }
            v
        }
        Compiled::Motion(m) => {
            // The first movement: one tick of a continuous motion's direction (the preview of a
            // held move shows where it heads), or the first step.
            let mut held = HeldMotion::new(m, 0);
            let first = match m.kind {
                MotionKind::Continuous { .. } => {
                    let (to, clamped) =
                        layout.clamp(Point::new(from.x + m.dx.signum(), from.y + m.dy.signum()));
                    (to, clamped)
                }
                MotionKind::Stepped { .. } => {
                    let (p, c) = held.apply(layout, from, 0);
                    let (q, c2) = layout.clamp(p);
                    (q, c || c2)
                }
            };
            let mut v = json!({
                "from": from.to_json(), "to": first.0.to_json(), "clamped": first.1,
                "while_held": true,
            });
            match m.kind {
                MotionKind::Continuous { speed, .. } => v["speed"] = json!(speed),
                MotionKind::Stepped { repeat_ms, .. } => v["repeat_ms"] = json!(repeat_ms),
            }
            if let Unit::Grid(_) = m.unit {
                v["unit"] = json!("grid");
            }
            v
        }
    })
}

/// One effect of the playground: where the simulated pointer went.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlayEffect {
    pub code: &'static str,
    pub from: Point,
    pub to: Point,
    /// The simulated buttons held afterwards.
    pub buttons: Vec<Button>,
    /// Wheel notches received so far (vertical, horizontal).
    pub wheel: (i32, i32),
}

/// The Test Playground's safe pointer (GUI_JOURNEYS J5): mouse steps run through the very code
/// the engine uses ([`run_step`], [`HeldMotion`]) against a [`SimPointer`], so the preview shows
/// where the pointer would go, clamping included, while the real pointer never moves.
#[derive(Debug)]
pub struct Playground {
    sim: SimPointer,
    inj: Injector<SimBackend>,
    held: Vec<HeldMotion>,
    hold_button: Option<Button>,
    clock_ms: u64,
}

impl Playground {
    /// A playground over `layout` (the real monitor layout may be shown; reading it moves
    /// nothing) with the simulated pointer at `cursor`.
    pub fn new(layout: Layout, cursor: Point, window: Option<WindowArea>) -> Playground {
        let sim = SimPointer::new(layout, cursor);
        sim.with(|s| s.window = window);
        Playground {
            inj: Injector::new(sim.backend()),
            sim,
            held: Vec::new(),
            hold_button: None,
            clock_ms: 0,
        }
    }

    pub fn pointer(&self) -> &SimPointer {
        &self.sim
    }

    fn effect(&self, code: &'static str, from: Point) -> PlayEffect {
        self.sim.with(|s| PlayEffect {
            code,
            from,
            to: s.cursor,
            buttons: s.buttons.clone(),
            wheel: s.wheel,
        })
    }

    /// The trigger goes down: a step runs, a hold presses its button, a motion starts.
    pub fn press(&mut self, c: &Compiled) -> PlayEffect {
        let from = self.sim.cursor();
        let mut screen = self.sim.screen();
        let code = match *c {
            Compiled::Run(step) => run_step(&step, &mut screen, &mut self.inj),
            Compiled::Hold { button, at } => {
                let mut ev = Vec::new();
                if let Some(spec) = at {
                    match hold_target(spec, &mut screen) {
                        Ok((layout, from, to)) if to != from => ev.push(move_event(&layout, to)),
                        Ok(_) => {}
                        Err(code) => return self.effect(code, from),
                    }
                }
                ev.push(OutEvent::down(OutCode::Button(button)));
                self.hold_button = Some(button);
                let mixed: Vec<(OutEvent, bool)> = ev
                    .iter()
                    .map(|e| (*e, matches!(e.code, OutCode::Button(_))))
                    .collect();
                if self.inj.send_mixed(&mixed) {
                    "ok"
                } else {
                    "injection_failed"
                }
            }
            Compiled::Motion(m) => {
                let mut h = HeldMotion::new(m, self.clock_ms);
                if matches!(m.kind, MotionKind::Stepped { .. }) {
                    let layout = self.sim.with(|s| s.layout.clone());
                    let (p, _) =
                        tick_motions(std::slice::from_mut(&mut h), &layout, from, self.clock_ms);
                    if p != from {
                        self.inj.send_balanced(&[move_event(&layout, p)]);
                    }
                }
                self.held.push(h);
                "ok"
            }
        };
        self.effect(code, from)
    }

    /// Time passes while the trigger is held: due motions move the simulated pointer (one tick at
    /// a time, exactly as the worker does).
    pub fn advance(&mut self, ms: u64) -> PlayEffect {
        let from = self.sim.cursor();
        let end = self.clock_ms + ms;
        while !self.held.is_empty() {
            let next = self.held.iter().map(|h| h.due_ms).min().unwrap_or(end);
            if next > end {
                break;
            }
            self.clock_ms = next;
            let layout = self.sim.with(|s| s.layout.clone());
            let cur = self.sim.cursor();
            let (p, _) = tick_motions(&mut self.held, &layout, cur, self.clock_ms);
            if p != cur {
                self.inj.send_balanced(&[move_event(&layout, p)]);
            }
        }
        self.clock_ms = end;
        self.effect("ok", from)
    }

    /// The trigger goes up: motions stop and a held button is released.
    pub fn release(&mut self) -> PlayEffect {
        let from = self.sim.cursor();
        self.held.clear();
        if let Some(b) = self.hold_button.take() {
            self.inj.release_code(OutCode::Button(b));
        }
        self.effect("ok", from)
    }

    /// Reset: everything held is released and the pointer goes back to `cursor`.
    pub fn reset(&mut self, cursor: Point) {
        self.held.clear();
        self.hold_button = None;
        self.inj.release_all();
        self.sim.with(|s| {
            s.cursor = cursor;
            s.wheel = (0, 0);
            s.events.clear();
        });
    }
}

// ---------------------------------------------------------------------------------------------
// The editor's data model (GUI_JOURNEYS J4 "Mouse", Step 74 draws it).
// ---------------------------------------------------------------------------------------------

/// How a relative move behaves while its trigger is held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HoldChoice {
    /// One move per press (with the trigger's `repeat`, the keyboard's auto-repeat repeats it).
    Once,
    /// Smooth movement while held.
    Continuous { speed: u32, acceleration: u32 },
    /// The step at once, then again every `repeat_ms` after `delay_ms` (independent of the
    /// keyboard's repeat settings: an accessible alternative to holding a key steady).
    Repeat { repeat_ms: u32, delay_ms: u32 },
}

/// The segmented "Mouse" form: move by, move to, scroll or click.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseAction {
    MoveBy {
        dx: i32,
        dy: i32,
        /// `None`: pixels; `Some`: cells of this grid.
        grid: Option<Grid>,
        hold: HoldChoice,
    },
    MoveTo(PointSpec),
    Scroll {
        dir: ScrollDir,
        notches: u32,
    },
    Click {
        button: Button,
        /// `click`, `double_click` or `hold`.
        op: &'static str,
        at: Option<PointSpec>,
    },
}

/// The mouse editor's draft (Step 74 draws it): the fields, the live summary, the screen-map
/// preview and what **Save** and **Test** may do now. Nothing is saved or performed by itself;
/// Test runs in the playground's simulated pointer only ([`Playground`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MouseDraft {
    pub title: String,
    /// The trigger keys (for the summary sentence of held actions).
    pub trigger: String,
    pub action: MouseAction,
    pub saved: Option<Box<MouseDraft>>,
}

/// One monitor of the editor's small screen map, in map units (the map is `width` × `height`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MapMonitor {
    pub rect: Rect,
    pub primary: bool,
    pub scale_percent: u32,
}

/// The small screen map under the Mouse form ("showing the target point across monitors").
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScreenMap {
    pub width: i32,
    pub height: i32,
    pub monitors: Vec<MapMonitor>,
    /// The simulated pointer and the step's target, in map units.
    pub from: Point,
    pub to: Option<Point>,
    pub clamped: bool,
}

fn scale_into(p: Point, b: Rect, w: i32, h: i32) -> Point {
    let sx = |v: i32| -> i32 {
        i32::try_from(i64::from(v - b.left) * i64::from(w) / i64::from(b.width().max(1)))
            .unwrap_or(0)
    };
    let sy = |v: i32| -> i32 {
        i32::try_from(i64::from(v - b.top) * i64::from(h) / i64::from(b.height().max(1)))
            .unwrap_or(0)
    };
    Point::new(sx(p.x), sy(p.y))
}

/// The screen map of `layout` scaled into `width` × `height` with the pointer at `from` and the
/// step's target.
pub fn screen_map(
    layout: &Layout,
    from: Point,
    to: Option<(Point, bool)>,
    width: i32,
    height: i32,
) -> ScreenMap {
    let b = layout.bounds();
    ScreenMap {
        width,
        height,
        monitors: layout
            .monitors()
            .iter()
            .map(|m| {
                let tl = scale_into(Point::new(m.rect.left, m.rect.top), b, width, height);
                let br = scale_into(Point::new(m.rect.right, m.rect.bottom), b, width, height);
                MapMonitor {
                    rect: Rect::new(tl.x, tl.y, br.x, br.y),
                    primary: m.primary,
                    scale_percent: m.scale_percent(),
                }
            })
            .collect(),
        from: scale_into(from, b, width, height),
        to: to.map(|(p, _)| scale_into(p, b, width, height)),
        clamped: to.is_some_and(|(_, c)| c),
    }
}

fn point_json(p: PointSpec) -> Value {
    let mut v = json!({ "x": p.x, "y": p.y });
    if p.anchor != Anchor::VirtualScreen {
        v["relative_to"] = json!(p.anchor.as_str());
    }
    v
}

impl MouseDraft {
    pub fn new(title: &str, trigger: &str, action: MouseAction) -> MouseDraft {
        MouseDraft {
            title: title.into(),
            trigger: trigger.into(),
            action,
            saved: None,
        }
    }

    /// The field problems (the same limits as the schema).
    pub fn problems(&self) -> Vec<FieldProblem> {
        let mut out = Vec::new();
        let mut bad = |path: &'static str, message: &str| {
            out.push(FieldProblem {
                path,
                message: message.into(),
            });
        };
        if !(1..=80).contains(&self.title.chars().count()) {
            bad("title", "the rule name needs 1–80 characters");
        }
        let point =
            |p: PointSpec| (-32768..=32767).contains(&p.x) && (-32768..=32767).contains(&p.y);
        match self.action {
            MouseAction::MoveBy { dx, dy, grid, hold } => {
                if !(-10_000..=10_000).contains(&dx) {
                    bad("steps[0].dx", "−10000…10000");
                }
                if !(-10_000..=10_000).contains(&dy) {
                    bad("steps[0].dy", "−10000…10000");
                }
                if let Some(g) = grid
                    && (!(2..=64).contains(&g.columns) || !(2..=64).contains(&g.rows))
                {
                    bad("steps[0].grid", "2–64 columns and rows");
                }
                if hold != HoldChoice::Once && dx == 0 && dy == 0 {
                    bad("steps[0].dx", "a move while held needs a direction");
                }
                match hold {
                    HoldChoice::Once => {}
                    HoldChoice::Continuous {
                        speed,
                        acceleration,
                    } => {
                        if grid.is_some() {
                            bad(
                                "steps[0].while_held.speed",
                                "grid moves are steps: choose \"repeat every\" instead",
                            );
                        }
                        if !(50..=5000).contains(&speed) {
                            bad("steps[0].while_held.speed", "50–5000 px/s");
                        }
                        if acceleration > 10 {
                            bad("steps[0].while_held.acceleration", "0–10");
                        }
                    }
                    HoldChoice::Repeat {
                        repeat_ms,
                        delay_ms,
                    } => {
                        if !(20..=2000).contains(&repeat_ms) {
                            bad("steps[0].while_held.repeat_ms", "20–2000 ms");
                        }
                        if delay_ms > 2000 {
                            bad("steps[0].while_held.delay_ms", "0–2000 ms");
                        }
                    }
                }
            }
            MouseAction::MoveTo(p) => {
                if !point(p) {
                    bad("steps[0].to", "x and y are −32768…32767");
                }
            }
            MouseAction::Scroll { notches, .. } => {
                if !(1..=50).contains(&notches) {
                    bad("steps[0].notches", "1–50 notches");
                }
            }
            MouseAction::Click { op, at, .. } => {
                if !["click", "double_click", "hold"].contains(&op) {
                    bad("steps[0].op", "click, double click or hold");
                }
                if at.is_some_and(|p| !point(p)) {
                    bad("steps[0].at", "x and y are −32768…32767");
                }
            }
        }
        out
    }

    /// The step the draft saves as (canonical: defaults left out).
    pub fn step(&self) -> Value {
        match self.action {
            MouseAction::MoveBy { dx, dy, grid, hold } => {
                let mut s = json!({ "type": "mouse_move", "mode": "relative", "dx": dx, "dy": dy });
                if let Some(g) = grid {
                    s["unit"] = json!("grid");
                    s["grid"] = json!({ "columns": g.columns, "rows": g.rows });
                }
                match hold {
                    HoldChoice::Once => {}
                    HoldChoice::Continuous {
                        speed,
                        acceleration,
                    } => {
                        s["while_held"] = json!({ "speed": speed });
                        if acceleration > 0 {
                            s["while_held"]["acceleration"] = json!(acceleration);
                        }
                    }
                    HoldChoice::Repeat {
                        repeat_ms,
                        delay_ms,
                    } => {
                        s["while_held"] = json!({ "repeat_ms": repeat_ms });
                        if delay_ms != DEFAULT_DELAY_MS {
                            s["while_held"]["delay_ms"] = json!(delay_ms);
                        }
                    }
                }
                s
            }
            MouseAction::MoveTo(p) => {
                json!({ "type": "mouse_move", "mode": "absolute", "to": point_json(p) })
            }
            MouseAction::Scroll { dir, notches } => {
                let mut s = json!({ "type": "mouse_scroll", "direction": dir.as_str() });
                if notches != 1 {
                    s["notches"] = json!(notches);
                }
                s
            }
            MouseAction::Click { button, op, at } => {
                let mut s = json!({ "type": "mouse_click", "button": button_name(button) });
                if op != "click" {
                    s["op"] = json!(op);
                }
                if let Some(p) = at {
                    s["at"] = point_json(p);
                }
                s
            }
        }
    }

    /// Whether the action lasts while the trigger is held (it must then be the rule's only step,
    /// on a single chord pressed without repeat).
    pub fn holds_trigger(&self) -> bool {
        matches!(
            self.action,
            MouseAction::MoveBy {
                hold: HoldChoice::Continuous { .. } | HoldChoice::Repeat { .. },
                ..
            } | MouseAction::Click { op: "hold", .. }
        )
    }

    /// The summary sentence ("Moves the pointer right at 600 px/s while Right is held").
    pub fn summary(&self) -> String {
        compile_step(&self.step()).map_or_else(
            |e| format!("Not ready: {e}"),
            |c| summary(&c, &self.trigger),
        )
    }

    /// The screen map with the simulated pointer at `from` on `layout` (the playground's safe
    /// preview; nothing moves).
    pub fn screen_map(
        &self,
        layout: &Layout,
        from: Point,
        window: Option<WindowArea>,
        width: i32,
        height: i32,
    ) -> ScreenMap {
        let to = compile_step(&self.step()).ok().and_then(|c| match c {
            Compiled::Run(s) => step_target(&s, layout, from, window).ok(),
            Compiled::Hold { at: Some(p), .. } => {
                resolve(p, layout, window).ok().map(|p| layout.clamp(p))
            }
            Compiled::Hold { at: None, .. } => Some((from, false)),
            Compiled::Motion(m) => {
                let mut h = HeldMotion::new(m, 0);
                Some(tick_motions(
                    std::slice::from_mut(&mut h),
                    layout,
                    from,
                    match m.kind {
                        MotionKind::Continuous { .. } => u64::from(TICK_MS),
                        MotionKind::Stepped { .. } => 0,
                    },
                ))
            }
        });
        screen_map(layout, from, to, width, height)
    }

    /// Whether the draft differs from what was saved.
    pub fn dirty(&self) -> bool {
        self.saved
            .as_ref()
            .is_none_or(|s| s.title != self.title || s.action != self.action)
    }

    /// Save is enabled for a changed, valid draft; otherwise the problems say why not.
    pub fn can_save(&self) -> Result<(), Vec<FieldProblem>> {
        let p = self.problems();
        if !p.is_empty() {
            return Err(p);
        }
        if !self.dirty() {
            return Err(vec![FieldProblem {
                path: "",
                message: "nothing changed since the last save".into(),
            }]);
        }
        Ok(())
    }

    /// Test is enabled for a valid draft: it runs on the playground's simulated pointer only.
    pub fn can_test(&self) -> bool {
        self.problems().is_empty()
    }
}

/// The "Mouse" form, in display order (GUI_JOURNEYS J4, Step 74). The segmented control picks
/// Move, Scroll or Click; each field names the rule-document field it edits.
pub const MOUSE_FIELDS: &[Field] = &[
    Field {
        path: "steps[0].type",
        input: "segmented",
        required: true,
        default: Some("mouse_move"),
        label: "Mouse",
        help: "Move the pointer, scroll the wheel or click a button. Every choice works from the keyboard alone; Windows' own Mouse Keys is the system alternative.",
    },
    Field {
        path: "steps[0].mode",
        input: "choice",
        required: true,
        default: Some("relative"),
        label: "Move",
        help: "By a distance from where the pointer is, or to a point. The pointer never leaves the visible desktop.",
    },
    Field {
        path: "steps[0].dx",
        input: "number",
        required: true,
        default: Some("0"),
        label: "Right (negative: left)",
        help: "Logical pixels, scaled by the monitor's display scale (or grid cells).",
    },
    Field {
        path: "steps[0].dy",
        input: "number",
        required: true,
        default: Some("0"),
        label: "Down (negative: up)",
        help: "Logical pixels, scaled by the monitor's display scale (or grid cells).",
    },
    Field {
        path: "steps[0].unit",
        input: "choice",
        required: false,
        default: Some("pixels"),
        label: "Count in",
        help: "Pixels, or cells of a virtual grid over the monitor: the pointer jumps from cell centre to cell centre.",
    },
    Field {
        path: "steps[0].grid",
        input: "grid",
        required: false,
        default: None,
        label: "Grid",
        help: "2–64 columns and rows per monitor.",
    },
    Field {
        path: "steps[0].while_held",
        input: "choice",
        required: false,
        default: None,
        label: "While the keys are held",
        help: "Once per press; keep moving smoothly; or repeat the step at a pace you choose (independent of the keyboard's repeat settings).",
    },
    Field {
        path: "steps[0].to",
        input: "point",
        required: true,
        default: None,
        label: "To",
        help: "x and y on the desktop, the primary monitor or the active window's content area. The map shows the point across your monitors.",
    },
    Field {
        path: "steps[0].direction",
        input: "choice",
        required: true,
        default: Some("down"),
        label: "Scroll",
        help: "Up, down, left or right.",
    },
    Field {
        path: "steps[0].notches",
        input: "number",
        required: false,
        default: Some("1"),
        label: "Notches",
        help: "1–50 wheel notches.",
    },
    Field {
        path: "steps[0].button",
        input: "choice",
        required: true,
        default: Some("left"),
        label: "Button",
        help: "Left and right follow the \"swap buttons\" setting; middle, back (X1) and forward (X2).",
    },
    Field {
        path: "steps[0].op",
        input: "choice",
        required: false,
        default: Some("click"),
        label: "Click",
        help: "Click, double click, or hold the button exactly as long as the keys are held (released on pause, lock and every stop). To keep a button down without holding keys, use Windows' ClickLock.",
    },
    Field {
        path: "steps[0].at",
        input: "point",
        required: false,
        default: None,
        label: "At",
        help: "Move to this point first (optional).",
    },
];

#[cfg(test)]
mod tests;
