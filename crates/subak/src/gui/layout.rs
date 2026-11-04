//! Window geometry: DPI scale, the grid a client area holds, the client area a grid needs, and
//! whether a saved placement is still on a monitor (docs/SUBAK_GUI.md §4). Pure functions of
//! numbers, so they are tested with synthetic DPI values and monitor layouts; the window module
//! feeds them what Windows reports.

use super::prefs::Placement;

/// The DPI of 100 % scale.
pub const BASE_DPI: u32 = 96;
/// The grid a new window asks for when nothing was saved (Windows Terminal's default).
pub const DEFAULT_GRID: (u16, u16) = (120, 30);
/// The smallest grid the window can be resized to.
pub const MIN_GRID: (u16, u16) = (20, 5);
/// The part of a saved window that must lie on a monitor for the placement to be used, in
/// logical pixels: enough of the title bar to grab it.
const VISIBLE_W: i32 = 120;
const VISIBLE_H: i32 = 32;

/// Device pixels per logical pixel at `dpi` (1.0 at 96, 1.5 at 144, 2.0 at 192).
pub fn scale(dpi: u32) -> f32 {
    dpi.max(1) as f32 / BASE_DPI as f32
}

/// `logical` pixels at `dpi`, rounded to whole device pixels.
pub fn px(logical: f32, dpi: u32) -> i32 {
    (logical * scale(dpi)).round() as i32
}

/// A rectangle in screen pixels, `right`/`bottom` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    fn intersection(&self, o: &Rect) -> Option<Rect> {
        let r = Rect {
            left: self.left.max(o.left),
            top: self.top.max(o.top),
            right: self.right.min(o.right),
            bottom: self.bottom.min(o.bottom),
        };
        (r.width() > 0 && r.height() > 0).then_some(r)
    }
}

/// Cell metrics and padding of the renderer at the current scale, in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cells {
    pub width: u32,
    pub height: u32,
    pub pad: u32,
}

impl Cells {
    /// The client area a grid of `cols × rows` needs.
    pub fn client_for(&self, cols: u16, rows: u16) -> (u32, u32) {
        (
            u32::from(cols) * self.width + 2 * self.pad,
            u32::from(rows) * self.height + 2 * self.pad,
        )
    }

    /// The grid a client area holds (at least one cell each way). A minimized window reports a
    /// 0 × 0 client area: callers must not resize the pane for it (see [`client_is_usable`]).
    pub fn grid_for(&self, width: u32, height: u32) -> (u16, u16) {
        let fit = |px: u32, cell: u32| {
            u16::try_from((px.saturating_sub(2 * self.pad) / cell.max(1)).max(1))
                .unwrap_or(u16::MAX)
        };
        (fit(width, self.width), fit(height, self.height))
    }
}

/// Whether a client area is a real size to lay out for: a minimized window has none, and the
/// pane must keep its size while the window is minimized.
pub fn client_is_usable(width: u32, height: u32) -> bool {
    width > 0 && height > 0
}

/// The outer size a window must have to keep its logical size after moving from `old_dpi` to
/// `new_dpi` (what `WM_DPICHANGED` suggests, computed for tests and for a placement saved at
/// another DPI).
pub fn rescale(size: (i32, i32), old_dpi: u32, new_dpi: u32) -> (i32, i32) {
    let f = |v: i32| {
        (i64::from(v) * i64::from(new_dpi.max(1)) / i64::from(old_dpi.max(1)))
            .clamp(1, i64::from(i32::MAX)) as i32
    };
    (f(size.0), f(size.1))
}

/// A saved placement to restore, or `None` when not enough of it lies on any of `monitors`
/// (their work areas): a monitor was removed, or the window was off-screen (a test run). The
/// size is rescaled from the DPI it was saved at to `dpi`, the DPI of the monitor it lands on.
pub fn restore(p: &Placement, monitors: &[(Rect, u32)]) -> Option<(Rect, bool)> {
    let saved = Rect {
        left: p.x,
        top: p.y,
        right: p.x.saturating_add(p.width),
        bottom: p.y.saturating_add(p.height),
    };
    let (_, dpi) = monitors.iter().find(|(m, dpi)| {
        saved.intersection(m).is_some_and(|i| {
            i.width() >= px(VISIBLE_W as f32, *dpi) && i.height() >= px(VISIBLE_H as f32, *dpi)
        })
    })?;
    let (w, h) = rescale((p.width, p.height), p.dpi, *dpi);
    Some((
        Rect {
            left: p.x,
            top: p.y,
            right: p.x + w,
            bottom: p.y + h,
        },
        p.maximized,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use subak_term::render::{RenderOptions, Renderer, Theme};

    fn cells_at(dpi: u32) -> Cells {
        let mut r = Renderer::builtin(Theme::kim_dark(), RenderOptions::default());
        r.set_scale(scale(dpi));
        let m = r.metrics();
        Cells {
            width: m.width,
            height: m.height,
            pad: r.padding(),
        }
    }

    #[test]
    fn synthetic_dpi_scales_cells_and_padding_with_the_monitor() {
        let mut last = None;
        for dpi in [96, 120, 144, 168, 192, 288] {
            let c = cells_at(dpi);
            let s = scale(dpi);
            // Padding is 8 logical pixels; cells are 13 px × 1.2 at the scale (DESIGN_SYSTEM §2.3).
            assert_eq!(c.pad, (8.0 * s).round() as u32, "dpi {dpi}");
            assert_eq!(c.height, (13.0 * 1.2 * s).ceil() as u32, "dpi {dpi}");
            if let Some(prev) = last {
                let p: Cells = prev;
                assert!(c.width >= p.width && c.height > p.height, "dpi {dpi}");
            }
            last = Some(c);
            // A grid round-trips through its client area at every scale.
            for (cols, rows) in [(120, 30), (80, 24), (20, 5), (311, 97)] {
                let (w, h) = c.client_for(cols, rows);
                assert_eq!(c.grid_for(w, h), (cols, rows), "dpi {dpi}");
                // One pixel short of a cell drops that column/row.
                assert_eq!(c.grid_for(w - 1, h - 1), (cols - 1, rows - 1), "dpi {dpi}");
            }
        }
        assert_eq!(scale(144), 1.5);
        assert_eq!(px(8.0, 120), 10);
        assert_eq!(px(13.0, 168), 23);
    }

    #[test]
    fn a_minimized_window_is_not_laid_out() {
        let c = cells_at(96);
        assert!(!client_is_usable(0, 0));
        assert!(!client_is_usable(800, 0));
        assert!(client_is_usable(1, 1));
        // A tiny client area still yields a 1 × 1 grid, never 0 (the pane cannot be 0 wide).
        assert_eq!(c.grid_for(0, 0), (1, 1));
    }

    #[test]
    fn moving_between_monitors_keeps_the_logical_size() {
        // 100 % → 150 %: Windows suggests 1.5× the outer size; back again restores it.
        assert_eq!(rescale((1000, 600), 96, 144), (1500, 900));
        assert_eq!(rescale((1500, 900), 144, 96), (1000, 600));
        assert_eq!(rescale((1001, 601), 96, 120), (1251, 751));
    }

    #[test]
    fn saved_placements_are_restored_only_on_a_monitor() {
        let primary = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let right_150 = Rect {
            left: 1920,
            top: 0,
            right: 1920 + 2560,
            bottom: 1400,
        };
        let monitors = [(primary, 96), (right_150, 144)];
        let p = |x, y, dpi, maximized| Placement {
            x,
            y,
            width: 1000,
            height: 600,
            dpi,
            maximized,
        };
        // On the primary monitor at its DPI: as saved.
        assert_eq!(
            restore(&p(100, 100, 96, false), &monitors),
            Some((
                Rect {
                    left: 100,
                    top: 100,
                    right: 1100,
                    bottom: 700
                },
                false
            ))
        );
        // Saved at 100 % but now on the 150 % monitor: rescaled; maximized kept.
        assert_eq!(
            restore(&p(2000, 50, 96, true), &monitors),
            Some((
                Rect {
                    left: 2000,
                    top: 50,
                    right: 3500,
                    bottom: 950
                },
                true
            ))
        );
        // The off-screen placement of a test run, and a monitor that was unplugged: ignored.
        assert_eq!(restore(&p(-20000, -20000, 96, false), &monitors), None);
        assert_eq!(restore(&p(5000, 100, 144, false), &monitors), None);
        // Only a sliver left on screen (less than a title bar to grab): ignored.
        assert_eq!(restore(&p(1850, 100, 96, false), &[(primary, 96)]), None);
    }
}
