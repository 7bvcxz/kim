//! Where the floating icon and the Terminal manager go (docs/SUBAK_GUI.md §11): the icon's size
//! at a DPI, its default place, snapping to the work area's edges while it is dragged, keeping it
//! on a monitor when monitors change, and the manager's rectangle anchored to the icon. Pure
//! functions of numbers, tested with synthetic DPI values and monitor layouts; the window module
//! feeds them what Windows reports (or, in the off-screen test mode, a synthetic layout).

use crate::gui::layout::{Rect, px};

/// The icon's edge in logical pixels (DESIGN_SYSTEM §3.4: 40 × 40).
pub const ICON: f32 = 40.0;
/// Distance at which a dragged icon snaps to a work area's edge, logical pixels (§3.4: 8).
pub const SNAP: f32 = 8.0;
/// The default gap between the icon and the work area's bottom-right corner, logical pixels.
pub const MARGIN: f32 = 24.0;
/// The gap between the icon and the manager, logical pixels.
pub const GAP: f32 = 8.0;
/// The compact manager's size in logical pixels (DESIGN_SYSTEM §3.6).
pub const MANAGER: (f32, f32) = (380.0, 440.0);
/// The manager's smallest height (a short work area), logical pixels.
pub const MANAGER_MIN_H: f32 = 240.0;

/// One monitor: its work area (without the taskbar) in physical screen pixels and its DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub work: Rect,
    pub dpi: u32,
}

/// The icon's edge in device pixels at `dpi`.
pub fn icon_size(dpi: u32) -> i32 {
    px(ICON, dpi).max(16)
}

fn overlap(a: &Rect, b: &Rect) -> i64 {
    let w = (a.right.min(b.right) - a.left.max(b.left)).max(0);
    let h = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0);
    i64::from(w) * i64::from(h)
}

/// The squared distance from a point to a rectangle (0 inside).
fn distance2(x: i32, y: i32, r: &Rect) -> i64 {
    let dx = if x < r.left {
        r.left - x
    } else if x >= r.right {
        x - (r.right - 1)
    } else {
        0
    };
    let dy = if y < r.top {
        r.top - y
    } else if y >= r.bottom {
        y - (r.bottom - 1)
    } else {
        0
    };
    i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy)
}

/// The monitor a rectangle belongs to: the one it overlaps most, else the nearest one to its
/// centre. `None` only without monitors.
pub fn monitor_of(r: &Rect, monitors: &[Monitor]) -> Option<Monitor> {
    let best = monitors
        .iter()
        .map(|m| (overlap(r, &m.work), m))
        .filter(|(o, _)| *o > 0)
        .max_by_key(|(o, _)| *o)
        .map(|(_, m)| *m);
    best.or_else(|| {
        let (cx, cy) = (r.left + r.width() / 2, r.top + r.height() / 2);
        monitors
            .iter()
            .min_by_key(|m| distance2(cx, cy, &m.work))
            .copied()
    })
}

/// Moves a `size × size` square at `(x, y)` fully inside `work` (a work area smaller than the
/// icon keeps its top-left corner inside).
pub fn clamp(x: i32, y: i32, size: i32, work: &Rect) -> (i32, i32) {
    let cx = x.min(work.right - size).max(work.left);
    let cy = y.min(work.bottom - size).max(work.top);
    (cx, cy)
}

/// Snaps a square within `snap` pixels of a work-area edge onto that edge.
pub fn snap(x: i32, y: i32, size: i32, work: &Rect, snap: i32) -> (i32, i32) {
    let near = |v: i32, edge: i32| (v - edge).abs() <= snap;
    let sx = if near(x, work.left) {
        work.left
    } else if near(x + size, work.right) {
        work.right - size
    } else {
        x
    };
    let sy = if near(y, work.top) {
        work.top
    } else if near(y + size, work.bottom) {
        work.bottom - size
    } else {
        y
    };
    (sx, sy)
}

/// Where a dragged icon goes: its origin when the drag started moved by the cursor's travel,
/// snapped to the edges of the monitor it is over and kept inside that monitor's work area.
/// Returns the origin and that monitor's DPI (the icon's size follows the monitor).
pub fn drag_to(
    origin: (i32, i32),
    from: (i32, i32),
    to: (i32, i32),
    size: i32,
    monitors: &[Monitor],
) -> ((i32, i32), u32) {
    let x = origin.0.saturating_add(to.0 - from.0);
    let y = origin.1.saturating_add(to.1 - from.1);
    settle(x, y, size, monitors)
}

/// A position made valid: on the monitor the icon overlaps most (or the nearest one), snapped to
/// its edges, inside its work area. The size is rescaled to that monitor's DPI from the size at
/// the DPI it was given at (`size` is in device pixels at the current DPI, so a move across
/// monitors keeps 40 logical pixels).
pub fn settle(x: i32, y: i32, size: i32, monitors: &[Monitor]) -> ((i32, i32), u32) {
    let r = Rect {
        left: x,
        top: y,
        right: x.saturating_add(size),
        bottom: y.saturating_add(size),
    };
    let Some(m) = monitor_of(&r, monitors) else {
        return ((x, y), crate::gui::layout::BASE_DPI);
    };
    let s = icon_size(m.dpi);
    let (sx, sy) = snap(x, y, s, &m.work, px(SNAP, m.dpi));
    (clamp(sx, sy, s, &m.work), m.dpi)
}

/// The icon's default place: the bottom-right corner of the first monitor's work area (the
/// primary one), `MARGIN` away from both edges.
pub fn default_place(monitors: &[Monitor]) -> Option<((i32, i32), u32)> {
    let m = monitors.first()?;
    let s = icon_size(m.dpi);
    let g = px(MARGIN, m.dpi);
    Some((
        clamp(m.work.right - s - g, m.work.bottom - s - g, s, &m.work),
        m.dpi,
    ))
}

/// A saved position `(x, y)` at `dpi` restored on the current monitors: kept where it was when
/// it still lies on a monitor (moved fully inside if it stuck out), else — a monitor was
/// unplugged, the resolution shrank, a saved test position — brought onto the nearest monitor,
/// so the icon can never be lost off-screen. Also what a display change re-applies.
pub fn restore(x: i32, y: i32, dpi: u32, monitors: &[Monitor]) -> Option<((i32, i32), u32)> {
    if monitors.is_empty() {
        return None;
    }
    let size = icon_size(dpi);
    Some(settle(x, y, size, monitors))
}

/// Whether the icon at `(x, y)` lies fully on some monitor's work area.
pub fn fully_visible(x: i32, y: i32, size: i32, monitors: &[Monitor]) -> bool {
    monitors.iter().any(|m| {
        x >= m.work.left && y >= m.work.top && x + size <= m.work.right && y + size <= m.work.bottom
    })
}

/// The manager's rectangle for an icon at `icon` (its screen rectangle): beside the icon on the
/// side with room — above it when the icon is in the lower half of its work area, else below;
/// right-aligned with it when it is in the right half, else left-aligned — kept inside the work
/// area, and shortened (down to its minimum) when the work area is short. The size is the
/// compact manager's at the monitor's DPI.
pub fn manager_rect(icon: &Rect, monitors: &[Monitor]) -> Option<(Rect, u32)> {
    let m = monitor_of(icon, monitors)?;
    let w_area = m.work;
    let gap = px(GAP, m.dpi);
    let w = px(MANAGER.0, m.dpi).min(w_area.width());
    let h = px(MANAGER.1, m.dpi)
        .min(w_area.height() - 2 * gap)
        .max(px(MANAGER_MIN_H, m.dpi).min(w_area.height()));
    let lower = icon.top + icon.height() / 2 > w_area.top + w_area.height() / 2;
    let right = icon.left + icon.width() / 2 > w_area.left + w_area.width() / 2;
    let x = if right { icon.right - w } else { icon.left };
    let mut y = if lower {
        icon.top - gap - h
    } else {
        icon.bottom + gap
    };
    // No room on the chosen side: overlap the icon rather than leave the work area.
    if y < w_area.top || y + h > w_area.bottom {
        y = y.clamp(w_area.top, (w_area.bottom - h).max(w_area.top));
    }
    let x = x.clamp(w_area.left, (w_area.right - w).max(w_area.left));
    Some((
        Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        },
        m.dpi,
    ))
}

/// Whether a press-move distance is a drag (more than the system's drag threshold) rather than a
/// click.
pub fn is_drag(from: (i32, i32), to: (i32, i32), threshold: i32) -> bool {
    (to.0 - from.0).abs() > threshold || (to.1 - from.1).abs() > threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(l: i32, t: i32, r: i32, b: i32) -> Rect {
        Rect {
            left: l,
            top: t,
            right: r,
            bottom: b,
        }
    }

    /// A 1920 × 1080 primary (taskbar 40 px) and a 150 % 2560 × 1440 monitor on its right.
    fn two() -> Vec<Monitor> {
        vec![
            Monitor {
                work: rect(0, 0, 1920, 1040),
                dpi: 96,
            },
            Monitor {
                work: rect(1920, 0, 4480, 1400),
                dpi: 144,
            },
        ]
    }

    #[test]
    fn the_icon_is_40_logical_pixels_at_every_dpi() {
        assert_eq!(icon_size(96), 40);
        assert_eq!(icon_size(120), 50);
        assert_eq!(icon_size(144), 60);
        assert_eq!(icon_size(192), 80);
        assert_eq!(icon_size(288), 120);
    }

    #[test]
    fn default_place_is_the_primary_bottom_right_corner() {
        assert_eq!(
            default_place(&two()),
            Some(((1920 - 40 - 24, 1040 - 40 - 24), 96))
        );
        assert_eq!(default_place(&[]), None);
        // At 200 % the size and the margin scale.
        let hi = [Monitor {
            work: rect(0, 0, 3840, 2080),
            dpi: 192,
        }];
        assert_eq!(
            default_place(&hi),
            Some(((3840 - 80 - 48, 2080 - 80 - 48), 192))
        );
    }

    #[test]
    fn a_drag_follows_the_cursor_snaps_to_edges_and_stays_on_screen() {
        let m = two();
        // A plain move inside the primary monitor.
        assert_eq!(
            drag_to((500, 500), (520, 520), (620, 470), 40, &m),
            ((600, 450), 96)
        );
        // Within 8 px of the left edge: snapped onto it.
        assert_eq!(
            drag_to((100, 300), (110, 310), (16, 310), 40, &m),
            ((0, 300), 96)
        );
        // Dragged past the bottom (into the taskbar): kept inside the work area.
        assert_eq!(
            drag_to((100, 900), (110, 910), (110, 1200), 40, &m),
            ((100, 1000), 96)
        );
        // Far off the left of every monitor: back on the nearest one, at its edge.
        assert_eq!(
            drag_to((100, 300), (110, 310), (-5000, 310), 40, &m),
            ((0, 300), 96)
        );
        // Onto the 150 % monitor: its DPI, and the size there (60 px) stays inside.
        let ((x, y), dpi) = drag_to((1800, 300), (1810, 310), (2810, 1390), 40, &m);
        assert_eq!(dpi, 144);
        assert_eq!((x, y), (2800, 1400 - 60));
        // Snapping at the 150 % monitor uses 12 px (8 logical).
        let ((x, _), _) = drag_to((3000, 300), (3000, 300), (4480 - 60 - 11, 300), 40, &m);
        assert_eq!(x, 4480 - 60);
    }

    #[test]
    fn a_saved_position_on_a_monitor_that_disappeared_comes_back_on_screen() {
        let m = two();
        // Still on the right monitor: kept.
        assert_eq!(restore(3000, 700, 144, &m), Some(((3000, 700), 144)));
        // The right monitor was unplugged: onto the primary, at its right edge.
        let only = [m[0]];
        assert_eq!(restore(3000, 700, 144, &only), Some(((1920 - 40, 700), 96)));
        // The resolution shrank below the saved place: inside again.
        let small = [Monitor {
            work: rect(0, 0, 1280, 680),
            dpi: 96,
        }];
        assert_eq!(restore(1856, 976, 96, &small), Some(((1240, 640), 96)));
        // A monitor left of the primary (negative coordinates) is a real place.
        let left = [
            m[0],
            Monitor {
                work: rect(-1920, 0, 0, 1040),
                dpi: 96,
            },
        ];
        assert_eq!(restore(-900, 200, 96, &left), Some(((-900, 200), 96)));
        // The off-screen test position on a real layout: recovered.
        assert_eq!(restore(-20000, -20000, 96, &left), Some(((-1920, 0), 96)));
        assert!(fully_visible(-1920, 0, 40, &left));
        assert!(!fully_visible(-20000, -20000, 40, &left));
        assert_eq!(restore(10, 10, 96, &[]), None);
    }

    #[test]
    fn the_manager_opens_beside_the_icon_with_room_and_inside_the_work_area() {
        let m = two();
        // Default icon place (bottom right of the primary): manager above it, right-aligned.
        let icon = rect(1856, 976, 1896, 1016);
        let (r, dpi) = manager_rect(&icon, &m).unwrap();
        assert_eq!(dpi, 96);
        assert_eq!((r.width(), r.height()), (380, 440));
        assert_eq!(r.right, 1896);
        assert_eq!(r.bottom, 976 - 8);
        // Top-left icon: below it, left-aligned.
        let (r, _) = manager_rect(&rect(0, 0, 40, 40), &m).unwrap();
        assert_eq!((r.left, r.top), (0, 48));
        // On the 150 % monitor: scaled.
        let (r, dpi) = manager_rect(&rect(2000, 100, 2060, 160), &m).unwrap();
        assert_eq!(dpi, 144);
        assert_eq!((r.width(), r.height()), (570, 660));
        assert!(r.left >= 1920 && r.right <= 4480 && r.top >= 0 && r.bottom <= 1400);
        // A short work area: shortened, still inside.
        let short = [Monitor {
            work: rect(0, 0, 800, 300),
            dpi: 96,
        }];
        let (r, _) = manager_rect(&rect(700, 200, 740, 240), &short).unwrap();
        assert!(r.top >= 0 && r.bottom <= 300, "{r:?}");
        // 440 does not fit: the work area less the gaps (300 - 16), above the minimum of 240.
        assert_eq!(r.height(), 284);
    }

    #[test]
    fn clicks_and_drags_are_told_apart_by_the_threshold() {
        assert!(!is_drag((10, 10), (12, 13), 4));
        assert!(is_drag((10, 10), (15, 10), 4));
        assert!(is_drag((10, 10), (10, 5), 4));
    }
}
