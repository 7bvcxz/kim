//! Displays (Step 58, ADR 0050): what changes when monitors are attached, removed, rearranged,
//! rotated or rescaled, which monitor or window a capture means, and how a captured rectangle
//! maps to logical pixels (NOEUL-FR-016, NOEUL-FR-017, NOEUL-FR-090…NOEUL-FR-093).
//!
//! Everything here is platform-independent and runs headless in tests over synthetic layouts:
//!
//! - [`Debounce`] coalesces a burst of display notifications (`WM_DISPLAYCHANGE`,
//!   `WM_SETTINGCHANGE` for the work area, `WM_DPICHANGED` of any Noeul window) into one handling
//!   after the layout has been quiet for [`SETTLE`], and at the latest [`MAX_WAIT`] after the
//!   first one. The runtime arms a one-shot timer only while a change is pending: nothing polls
//!   and an idle Noeul has no timer (NOEUL-NFR-001).
//! - [`compare`] says how two layouts differ: not at all, only in scale, work areas, HDR or which
//!   monitor is primary ([`LayoutChange::Details`]: a frozen picture still matches the screen), or
//!   in where the monitors are and how they are turned ([`LayoutChange::Geometry`]: it does not).
//! - [`MonitorSpec`] and [`resolve_monitor`] name the monitor of `noeul capture --monitor` and
//!   `capture.monitor`; [`Mapping`] is a captured rectangle in the logical pixels of the monitor
//!   that holds its centre, with the monitors it spans and whether their scales differ.
//! - [`fill_gaps`] makes the parts of a picture that no monitor shows (the corners of an L-shaped
//!   or staggered virtual desktop) opaque black, whatever the driver left there, so pins, copies
//!   and saves never carry undefined pixels.
//! - [`draw_pointer_sprite`] draws the synthetic desktop's mouse pointer; the Windows backend draws
//!   the real one with GDI (`win::gdi`).

use crate::capture::{Desktop, Monitor};
use crate::detect::{Affinity, WindowFacts};
use crate::dpi;
use crate::frame::{Frame, Rect};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How long the layout must be quiet before Noeul looks at it again.
pub const SETTLE: Duration = Duration::from_millis(250);

/// The longest a change waits while notifications keep arriving.
pub const MAX_WAIT: Duration = Duration::from_millis(1000);

/// Coalesces display notifications (see the module documentation).
#[derive(Debug, Default, Clone)]
pub struct Debounce {
    /// When the first notification not yet handled arrived.
    first: Option<Instant>,
    received: u64,
    handled: u64,
}

impl Debounce {
    /// A notification arrived at `now`. Returns the delay to (re)arm the one-shot timer with, or
    /// `None` to leave the armed timer alone: it then fires within [`MAX_WAIT`] of the first
    /// notification, however many follow.
    pub fn note(&mut self, now: Instant) -> Option<Duration> {
        self.received += 1;
        let first = *self.first.get_or_insert(now);
        let waited = now.saturating_duration_since(first);
        (waited + SETTLE <= MAX_WAIT).then_some(SETTLE)
    }

    /// The timer fired: `true` when a change is pending (it is now handled once).
    pub fn settle(&mut self) -> bool {
        if self.first.take().is_some() {
            self.handled += 1;
            true
        } else {
            false
        }
    }

    pub fn pending(&self) -> bool {
        self.first.is_some()
    }

    /// `(received, handled)`.
    pub fn counts(&self) -> (u64, u64) {
        (self.received, self.handled)
    }

    /// `status.displays` (counts only).
    pub fn to_json(&self) -> Value {
        json!({"changes": self.received, "handled": self.handled, "pending": self.pending()})
    }
}

/// How a new monitor layout differs from an old one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutChange {
    Same,
    /// The same monitors at the same places and turns; a scale, a work area, HDR or the primary
    /// monitor changed. A frozen picture still matches the screen pixel for pixel.
    Details,
    /// A monitor was attached, removed, moved, resized or rotated.
    Geometry,
}

/// Compares two layouts, whatever order the monitors are listed in.
pub fn compare(old: &[Monitor], new: &[Monitor]) -> LayoutChange {
    let key = |m: &Monitor| (m.rect.x, m.rect.y, m.rect.width, m.rect.height, m.rotation);
    let mut a: Vec<_> = old.iter().map(key).collect();
    let mut b: Vec<_> = new.iter().map(key).collect();
    a.sort_unstable();
    b.sort_unstable();
    if a != b {
        return LayoutChange::Geometry;
    }
    let detail = |m: &Monitor| {
        (
            key(m),
            m.dpi,
            m.primary,
            m.hdr,
            (m.work.x, m.work.y, m.work.width, m.work.height),
        )
    };
    let mut a: Vec<_> = old.iter().map(detail).collect();
    let mut b: Vec<_> = new.iter().map(detail).collect();
    a.sort_unstable();
    b.sort_unstable();
    if a == b {
        LayoutChange::Same
    } else {
        LayoutChange::Details
    }
}

/// The monitor a monitor capture means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorSpec {
    /// The primary monitor.
    Primary,
    /// The monitor under the pointer (or the nearest one).
    Pointer,
    /// The n-th monitor (1-based) in the order `noeul status` lists them.
    Number(u32),
}

/// The highest monitor number accepted (Windows supports far fewer).
pub const MAX_MONITORS: u32 = 64;

impl MonitorSpec {
    /// `primary`, `pointer` or a number from 1.
    pub fn parse(s: &str) -> Option<MonitorSpec> {
        match s.trim() {
            "primary" => Some(MonitorSpec::Primary),
            "pointer" => Some(MonitorSpec::Pointer),
            n => n
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=MAX_MONITORS).contains(n) && !s.trim().starts_with('0'))
                .map(MonitorSpec::Number),
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            MonitorSpec::Primary => json!("primary"),
            MonitorSpec::Pointer => json!("pointer"),
            MonitorSpec::Number(n) => json!(n),
        }
    }

    pub fn from_json(v: &Value) -> Option<MonitorSpec> {
        match v {
            Value::String(s) if s == "primary" || s == "pointer" => MonitorSpec::parse(s),
            Value::Number(n) => n
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (1..=MAX_MONITORS).contains(n))
                .map(MonitorSpec::Number),
            _ => None,
        }
    }
}

impl std::fmt::Display for MonitorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MonitorSpec::Primary => f.write_str("the primary monitor"),
            MonitorSpec::Pointer => f.write_str("the monitor under the pointer"),
            MonitorSpec::Number(n) => write!(f, "monitor {n}"),
        }
    }
}

/// The index into `desktop.monitors` of the monitor `spec` names; `None` when there is no such
/// monitor (`pointer` is where the pointer is, in device pixels).
pub fn resolve_monitor(desktop: &Desktop, spec: MonitorSpec, pointer: (i32, i32)) -> Option<usize> {
    let ms = &desktop.monitors;
    match spec {
        MonitorSpec::Primary => ms.iter().position(|m| m.primary).or_else(|| (!ms.is_empty()).then_some(0)),
        MonitorSpec::Pointer => dpi::monitor_index(ms, pointer),
        MonitorSpec::Number(n) => {
            let i = usize::try_from(n).ok()?.checked_sub(1)?;
            (i < ms.len()).then_some(i)
        }
    }
}

/// The part of `a` inside `b`, if any.
pub fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let l = i64::from(a.x).max(i64::from(b.x));
    let t = i64::from(a.y).max(i64::from(b.y));
    let r = a.right().min(b.right());
    let bt = a.bottom().min(b.bottom());
    (l < r && t < bt).then(|| {
        Rect::new(
            i32::try_from(l).unwrap_or(i32::MAX),
            i32::try_from(t).unwrap_or(i32::MAX),
            u32::try_from(r - l).unwrap_or(0),
            u32::try_from(bt - t).unwrap_or(0),
        )
    })
}

/// A captured rectangle in the logical pixels of the monitor that holds its centre (the monitor
/// Noeul's own drawing for it is scaled by), and the monitors it touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    /// Index into the layout.
    pub monitor: usize,
    pub dpi: u32,
    /// Relative to that monitor's top-left corner, in its logical pixels; each edge is rounded on
    /// its own ([`dpi::to_logical`]), so adjacent rectangles still touch.
    pub logical: (i64, i64, i64, i64),
    /// How many monitors the rectangle covers part of.
    pub spans: usize,
    /// Whether those monitors have different scales (the picture keeps every monitor's device
    /// pixels; nothing is rescaled).
    pub mixed_dpi: bool,
}

/// The [`Mapping`] of `region` on `monitors` (`None` without monitors).
pub fn mapping(monitors: &[Monitor], region: Rect) -> Option<Mapping> {
    let cx = i64::from(region.x) + i64::from(region.width / 2);
    let cy = i64::from(region.y) + i64::from(region.height / 2);
    let centre = (
        i32::try_from(cx).unwrap_or(region.x),
        i32::try_from(cy).unwrap_or(region.y),
    );
    let i = dpi::monitor_index(monitors, centre)?;
    let m = monitors[i];
    let (ox, oy) = (i64::from(m.rect.x), i64::from(m.rect.y));
    let l = dpi::to_logical(i64::from(region.x) - ox, m.dpi);
    let t = dpi::to_logical(i64::from(region.y) - oy, m.dpi);
    let r = dpi::to_logical(region.right() - ox, m.dpi);
    let b = dpi::to_logical(region.bottom() - oy, m.dpi);
    let touched: Vec<&Monitor> = monitors
        .iter()
        .filter(|o| intersect(o.rect, region).is_some())
        .collect();
    let mixed_dpi = touched.first().is_some_and(|f| touched.iter().any(|o| o.dpi != f.dpi));
    Some(Mapping {
        monitor: i,
        dpi: m.dpi,
        logical: (l, t, r - l, b - t),
        spans: touched.len(),
        mixed_dpi,
    })
}

impl Mapping {
    pub fn to_json(&self) -> Value {
        let (x, y, w, h) = self.logical;
        json!({
            "monitor": self.monitor + 1,
            "dpi": self.dpi,
            "scale_percent": dpi::scale_percent(self.dpi),
            "logical": {"x": x, "y": y, "width": w, "height": h},
            "spans": self.spans,
            "mixed_dpi": self.mixed_dpi,
        })
    }
}

/// One monitor of a capture result: its number (1-based, the order of `noeul status`) and
/// geometry, scale, turn and HDR state; never a device name or anything about its content.
pub fn monitor_json(monitors: &[Monitor], i: usize) -> Value {
    monitors.get(i).map_or(Value::Null, |m| {
        json!({
            "number": i + 1,
            "rect": m.rect.to_json(),
            "work": m.work.to_json(),
            "primary": m.primary,
            "dpi": m.dpi,
            "scale_percent": dpi::scale_percent(m.dpi),
            "rotation": m.rotation,
            "hdr": m.hdr,
        })
    })
}

/// Whether the mouse pointer was drawn into a picture (NOEUL-FR-016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerOutcome {
    /// Drawn; where its image lies (device pixels, possibly partly outside the picture).
    Drawn(Rect),
    /// The pointer is hidden (a program hid it, or touch or pen input suppresses it).
    Hidden,
    /// The pointer's image lies outside the picture.
    Outside,
    /// The backend cannot tell where the pointer is or what it looks like.
    Unavailable,
}

impl PointerOutcome {
    pub fn drawn(self) -> bool {
        matches!(self, PointerOutcome::Drawn(_))
    }

    /// `{drawn, rect, reason}`.
    pub fn to_json(self) -> Value {
        match self {
            PointerOutcome::Drawn(r) => json!({"drawn": true, "rect": r.to_json(), "reason": null}),
            PointerOutcome::Hidden => json!({"drawn": false, "rect": null, "reason": "hidden"}),
            PointerOutcome::Outside => json!({"drawn": false, "rect": null, "reason": "outside"}),
            PointerOutcome::Unavailable => {
                json!({"drawn": false, "rect": null, "reason": "unavailable"})
            }
        }
    }
}

/// The honest warnings of one capture (shown as CLI warnings and notices): a rectangle across
/// monitors of different scales, HDR monitors (Windows converts their picture to SDR), a window
/// that excludes itself from capture or lies partly off every monitor, a pointer that was asked
/// for and not drawn. Geometry and states only (NOEUL-SEC-008).
pub fn warnings(
    monitors: &[Monitor],
    region: Rect,
    window: Option<(&WindowFacts, Rect)>,
    pointer: Option<PointerOutcome>,
) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = mapping(monitors, region)
        && m.mixed_dpi
    {
        out.push(format!(
            "the picture spans {} monitors at different scales; it keeps every monitor's device pixels (nothing is rescaled), and its logical size refers to monitor {}",
            m.spans,
            m.monitor + 1
        ));
    }
    let hdr: Vec<String> = monitors
        .iter()
        .enumerate()
        .filter(|(_, m)| m.hdr == Some(true) && intersect(m.rect, region).is_some())
        .map(|(i, _)| (i + 1).to_string())
        .collect();
    if !hdr.is_empty() {
        out.push(format!(
            "monitor {} shows HDR (advanced colour): Windows converts its picture to 8-bit SDR for this capture, so bright HDR colours are clipped and colours can differ from what the display shows",
            hdr.join(", ")
        ));
    }
    if let Some((facts, captured)) = window {
        match facts.affinity {
            Affinity::Excluded => out.push(
                "the window excludes itself from capture (display affinity): Windows leaves it out of the picture, and Noeul does not get around that".into(),
            ),
            Affinity::Monitor => out.push(
                "the window protects its content from capture (display affinity): Windows shows it black in the picture, and Noeul does not get around that".into(),
            ),
            Affinity::None => {}
        }
        if captured != facts.rect {
            out.push(format!(
                "only the part of the window on the monitors was captured ({captured} of {})",
                facts.rect
            ));
        }
    }
    match pointer {
        Some(PointerOutcome::Hidden) => out.push("the pointer is hidden, so it is not in the picture".into()),
        Some(PointerOutcome::Outside) => {
            out.push("the pointer is outside the captured area, so it is not in the picture".into());
        }
        Some(PointerOutcome::Unavailable) => {
            out.push("the pointer could not be read, so it is not in the picture".into());
        }
        _ => {}
    }
    out
}

/// Makes every pixel of `frame` that no monitor shows opaque black (a gap of the virtual
/// desktop); returns how many pixels were filled.
pub fn fill_gaps(frame: &mut Frame, monitors: &[Monitor]) -> u64 {
    let area = frame.rect();
    let w = area.width as usize;
    // Horizontal bands between the monitors' top and bottom edges inside the frame.
    let mut edges: Vec<i64> = vec![i64::from(area.y), area.bottom()];
    for m in monitors {
        for e in [i64::from(m.rect.y), m.rect.bottom()] {
            if e > i64::from(area.y) && e < area.bottom() {
                edges.push(e);
            }
        }
    }
    edges.sort_unstable();
    edges.dedup();
    let mut filled = 0u64;
    let px = frame.pixels_mut();
    for band in edges.windows(2) {
        let (top, bottom) = (band[0], band[1]);
        // The covered columns of this band, merged.
        let mut spans: Vec<(i64, i64)> = monitors
            .iter()
            .filter(|m| i64::from(m.rect.y) <= top && m.rect.bottom() >= bottom)
            .map(|m| {
                (
                    i64::from(m.rect.x).max(i64::from(area.x)),
                    m.rect.right().min(area.right()),
                )
            })
            .filter(|(l, r)| l < r)
            .collect();
        spans.sort_unstable();
        let mut gaps = Vec::new();
        let mut at = i64::from(area.x);
        for (l, r) in spans {
            if l > at {
                gaps.push((at, l));
            }
            at = at.max(r);
        }
        if at < area.right() {
            gaps.push((at, area.right()));
        }
        for row in top..bottom {
            let ry = usize::try_from(row - i64::from(area.y)).unwrap_or(0);
            for (l, r) in &gaps {
                let a = usize::try_from(l - i64::from(area.x)).unwrap_or(0);
                let b = usize::try_from(r - i64::from(area.x)).unwrap_or(0).min(w);
                for p in px[(ry * w + a) * 4..(ry * w + b) * 4].as_chunks_mut::<4>().0 {
                    *p = [0, 0, 0, 255];
                }
                filled += (b - a) as u64;
            }
        }
    }
    filled
}

/// The synthetic desktop's pointer: an arrow of 12 × 19 pixels with its hotspot at the top-left
/// (`#` black outline, `o` white fill, `.` transparent), drawn at its own size on every monitor,
/// like the real pointer image GDI draws (ADR 0050).
pub const POINTER_SPRITE: [&str; 19] = [
    "#...........",
    "##..........",
    "#o#.........",
    "#oo#........",
    "#ooo#.......",
    "#oooo#......",
    "#ooooo#.....",
    "#oooooo#....",
    "#ooooooo#...",
    "#oooooooo#..",
    "#ooooooooo#.",
    "#oooooo#####",
    "#ooo#oo#....",
    "#oo#.#oo#...",
    "#o#..#oo#...",
    "##....#oo#..",
    "#.....#oo#..",
    "......#oo#..",
    ".......##...",
];

/// The sprite's pixel at `(x, y)` of its box (BGRA), `None` where it is transparent.
pub fn pointer_sprite_pixel(x: usize, y: usize) -> Option<[u8; 4]> {
    match POINTER_SPRITE.get(y)?.as_bytes().get(x)? {
        b'#' => Some([0, 0, 0, 255]),
        b'o' => Some([255, 255, 255, 255]),
        _ => None,
    }
}

/// Draws the synthetic pointer with its hotspot at `at` into `frame`.
pub fn draw_pointer_sprite(frame: &mut Frame, at: (i32, i32)) -> PointerOutcome {
    let size = (
        u32::try_from(POINTER_SPRITE[0].len()).unwrap_or(0),
        u32::try_from(POINTER_SPRITE.len()).unwrap_or(0),
    );
    let sprite = Rect::new(at.0, at.1, size.0, size.1);
    let area = frame.rect();
    if intersect(sprite, area).is_none() {
        return PointerOutcome::Outside;
    }
    let w = area.width as usize;
    let px = frame.pixels_mut();
    for sy in 0..size.1 as usize {
        for sx in 0..size.0 as usize {
            let (ax, ay) = (i64::from(at.0) + sx as i64, i64::from(at.1) + sy as i64);
            if ax < i64::from(area.x) || ax >= area.right() || ay < i64::from(area.y) || ay >= area.bottom() {
                continue;
            }
            if let Some(c) = pointer_sprite_pixel(sx, sy) {
                let i = ((ay - i64::from(area.y)) as usize * w + (ax - i64::from(area.x)) as usize) * 4;
                px[i..i + 4].copy_from_slice(&c);
            }
        }
    }
    PointerOutcome::Drawn(sprite)
}

#[cfg(test)]
mod tests;
