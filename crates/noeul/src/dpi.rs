//! Logical and device pixels on mixed-DPI layouts (NOEUL-FR-090, NOEUL-FR-091; Step 52,
//! ADR 0035).
//!
//! Noeul is per-monitor DPI aware (v2): every selection, readout, pin, window rectangle and UI
//! Automation rectangle it handles is in **device pixels** of the virtual desktop, and only its
//! own drawing (handles, toolbar, magnifier) is sized in logical pixels of the monitor it is
//! drawn on ([`Monitor::px`]). This module is the one place where the two meet:
//!
//! - [`to_logical`] and [`to_device`] convert lengths at one DPI with integer arithmetic, rounding
//!   half away from zero. For every DPI Windows offers (100 % and up), logical → device → logical
//!   is the identity, and device → logical → device → logical gives the same logical value as
//!   one conversion: repeated conversions never drift.
//! - [`point_to_logical`] / [`point_to_device`] convert a point relative to the top-left corner of
//!   the monitor that holds it (or the nearest one), so a point keeps its monitor and a round trip
//!   lands on the same device pixel whenever that pixel starts a logical pixel.
//! - [`rect_from_logical`] reinterprets a rectangle given in a monitor's logical pixels (as a
//!   DPI-unaware program reports it) as device pixels on that monitor. Detection uses it to
//!   recover an element rectangle that a provider reported unscaled under fractional scaling, and
//!   falls back to the window rectangle when that does not fit either (detect::check_element).
//!
//! Windows lays out the logical space of DPI-unaware programs per monitor; this module keeps every
//! monitor's top-left corner fixed, which is exact for the primary monitor (at 0,0) and for
//! monitors whose origin Windows keeps. It never guesses beyond that.

use crate::capture::{BASE_DPI, Monitor};
use crate::frame::Rect;

/// The DPI Noeul assumes for a monitor that reports none or less than 100 %.
fn dpi_of(dpi: u32) -> i64 {
    i64::from(dpi.max(BASE_DPI))
}

/// `n / d` rounded half away from zero (`d` > 0).
fn round_div(n: i64, d: i64) -> i64 {
    if n >= 0 {
        (n + d / 2) / d
    } else {
        -((-n + d / 2) / d)
    }
}

/// Device pixels of `logical` pixels at `dpi`.
pub fn to_device(logical: i64, dpi: u32) -> i64 {
    round_div(logical * dpi_of(dpi), i64::from(BASE_DPI))
}

/// Logical pixels of `device` pixels at `dpi`.
pub fn to_logical(device: i64, dpi: u32) -> i64 {
    round_div(device * i64::from(BASE_DPI), dpi_of(dpi))
}

/// The scale of `dpi` in percent (96 → 100, 120 → 125, 144 → 150, 168 → 175).
pub fn scale_percent(dpi: u32) -> u32 {
    u32::try_from(round_div(dpi_of(dpi) * 100, i64::from(BASE_DPI))).unwrap_or(100)
}

/// A point in the logical pixels of one monitor, relative to that monitor's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalPoint {
    /// Index into the layout.
    pub monitor: usize,
    pub x: i64,
    pub y: i64,
}

/// The monitor that holds `(x, y)`, else the nearest one (`None` for an empty layout).
pub fn monitor_index(monitors: &[Monitor], (x, y): (i32, i32)) -> Option<usize> {
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
    monitors
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| dist(m))
        .map(|(i, _)| i)
}

/// `pt` (device pixels of the virtual desktop) in the logical pixels of its monitor.
pub fn point_to_logical(monitors: &[Monitor], pt: (i32, i32)) -> Option<LogicalPoint> {
    let i = monitor_index(monitors, pt)?;
    let m = monitors[i];
    Some(LogicalPoint {
        monitor: i,
        x: to_logical(i64::from(pt.0) - i64::from(m.rect.x), m.dpi),
        y: to_logical(i64::from(pt.1) - i64::from(m.rect.y), m.dpi),
    })
}

/// A logical point of one monitor in device pixels of the virtual desktop.
pub fn point_to_device(monitors: &[Monitor], p: LogicalPoint) -> Option<(i32, i32)> {
    let m = monitors.get(p.monitor)?;
    let x = i64::from(m.rect.x) + to_device(p.x, m.dpi);
    let y = i64::from(m.rect.y) + to_device(p.y, m.dpi);
    Some((i32::try_from(x).ok()?, i32::try_from(y).ok()?))
}

/// `r`, given in `m`'s logical pixels with `m`'s top-left corner kept in place, as device pixels.
pub fn rect_from_logical(m: &Monitor, r: Rect) -> Rect {
    let (ox, oy) = (i64::from(m.rect.x), i64::from(m.rect.y));
    let l = ox + to_device(i64::from(r.x) - ox, m.dpi);
    let t = oy + to_device(i64::from(r.y) - oy, m.dpi);
    let rr = ox + to_device(r.right() - ox, m.dpi);
    let b = oy + to_device(r.bottom() - oy, m.dpi);
    Rect::new(
        i32::try_from(l).unwrap_or(i32::MAX),
        i32::try_from(t).unwrap_or(i32::MAX),
        u32::try_from((rr - l).max(0)).unwrap_or(0),
        u32::try_from((b - t).max(0)).unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scale the Windows display settings offer, and a few custom ones.
    const DPIS: &[u32] = &[96, 120, 144, 168, 192, 216, 240, 288, 336, 384, 110, 133];

    #[test]
    fn lengths_round_trip_without_drift_at_every_scale() {
        for &dpi in DPIS {
            for logical in -2000..2000 {
                let d = to_device(logical, dpi);
                assert_eq!(to_logical(d, dpi), logical, "{logical} at {dpi}");
            }
            for device in -3000..3000 {
                let l = to_logical(device, dpi);
                let again = to_logical(to_device(l, dpi), dpi);
                assert_eq!(again, l, "device {device} at {dpi} is stable");
            }
        }
        assert_eq!(to_device(7, 96), 7);
        assert_eq!(to_device(7, 144), 11, "10.5 rounds away from zero");
        assert_eq!(
            to_device(-7, 144),
            -11,
            "symmetric for negative coordinates"
        );
        assert_eq!(to_logical(600, 144), 400);
        assert_eq!(to_device(9, 120), 11);
        assert_eq!(
            to_device(5, 0),
            5,
            "a monitor without a DPI counts as 100 %"
        );
        assert_eq!(
            [96, 120, 144, 168, 192].map(scale_percent),
            [100, 125, 150, 175, 200]
        );
    }

    /// A synthetic mixed layout: a 150 % monitor at negative coordinates left of a 100 % primary,
    /// and a 125 % monitor above the primary.
    fn mixed() -> Vec<Monitor> {
        vec![
            Monitor::new(Rect::new(-2880, 0, 2880, 1620), false).with_dpi(144),
            Monitor::new(Rect::new(0, 0, 1920, 1080), true),
            Monitor::new(Rect::new(0, -1350, 2400, 1350), false).with_dpi(120),
        ]
    }

    #[test]
    fn points_keep_their_monitor_and_round_trip_on_mixed_layouts() {
        let l = mixed();
        let p = point_to_logical(&l, (-2880 + 300, 150)).unwrap();
        assert_eq!(
            p,
            LogicalPoint {
                monitor: 0,
                x: 200,
                y: 100
            }
        );
        assert_eq!(point_to_device(&l, p), Some((-2880 + 300, 150)));
        let q = point_to_logical(&l, (1000, -1000)).unwrap();
        assert_eq!((q.monitor, q.x, q.y), (2, 800, 280));
        assert_eq!(point_to_device(&l, q), Some((1000, -1000)));
        // Every device pixel of every monitor maps into that monitor, and the logical point is
        // stable under a second round trip.
        for (i, m) in l.iter().enumerate() {
            for (x, y) in [
                (m.rect.x, m.rect.y),
                (m.rect.x + 1, m.rect.y + 2),
                (
                    i32::try_from(m.rect.right() - 1).unwrap(),
                    i32::try_from(m.rect.bottom() - 1).unwrap(),
                ),
            ] {
                let lp = point_to_logical(&l, (x, y)).unwrap();
                assert_eq!(lp.monitor, i);
                let back = point_to_device(&l, lp).unwrap();
                assert_eq!(point_to_logical(&l, back), Some(lp), "stable at ({x}, {y})");
                assert_eq!(monitor_index(&l, back), Some(i), "same monitor");
            }
        }
        // A point in a gap belongs to the nearest monitor.
        assert_eq!(monitor_index(&l, (1950, 500)), Some(1));
        assert_eq!(
            monitor_index(&l, (1950, 10)),
            Some(2),
            "11 above beats 31 right"
        );
        assert_eq!(monitor_index(&[], (0, 0)), None);
        assert_eq!(point_to_logical(&[], (0, 0)), None);
    }

    #[test]
    fn logical_rectangles_become_device_pixels_on_their_monitor() {
        let l = mixed();
        // 400 × 300 logical at (100, 100) of the primary is unchanged at 100 %.
        assert_eq!(
            rect_from_logical(&l[1], Rect::new(100, 100, 400, 300)),
            Rect::new(100, 100, 400, 300)
        );
        // On the 150 % monitor, measured from its top-left corner.
        assert_eq!(
            rect_from_logical(&l[0], Rect::new(-2880 + 100, 10, 400, 300)),
            Rect::new(-2880 + 150, 15, 600, 450)
        );
        // At 125 % the edges round independently, so adjacent rectangles still touch.
        let a = rect_from_logical(&l[2], Rect::new(0, -1350, 3, 3));
        let b = rect_from_logical(&l[2], Rect::new(3, -1350, 3, 3));
        assert_eq!(a.right(), i64::from(b.x));
    }
}
