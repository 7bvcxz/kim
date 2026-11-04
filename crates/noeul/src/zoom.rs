//! Pin zoom: the zoom levels, the size of a pin's window at a zoom, the fit of a large picture
//! into a monitor's work area, and the scaler that turns a pin's picture into the premultiplied
//! pixels of its layered window (NOEUL-FR-061, NOEUL-NFR-002; ADR 0029).
//!
//! **100 %** is one image pixel per device pixel on every monitor (NOEUL-FR-061); a pin's
//! picture is never resampled at 100 %. Zooming in repeats pixels (nearest neighbour, so pixels
//! stay sharp for inspection); zooming out averages the covered pixels (a box filter, so text
//! stays readable). The window surface is bounded by [`MAX_SIDE`] and [`MAX_BYTES`], so a zoomed
//! pin can never grow into an allocation the machine cannot make. Platform independent and pure:
//! the Win32 window (`win::pinwnd`) only uploads what [`render`] wrote.

use crate::frame::Frame;

/// One image pixel per device pixel.
pub const FULL: u32 = 100;
/// The smallest zoom (percent).
pub const MIN: u32 = 10;
/// The largest zoom (percent).
pub const MAX: u32 = 800;
/// The zoom levels the zoom keys, the wheel and the menu step through (percent).
pub const LADDER: &[u32] = &[
    10, 15, 20, 25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500, 600,
    800,
];
/// The largest side of a pin window (device pixels).
pub const MAX_SIDE: u32 = 16_384;
/// The largest pin window surface (BGRA bytes).
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// The window size of a `w`×`h` picture at `zoom` percent (rounded, at least one pixel).
pub fn size(w: u32, h: u32, zoom: u32) -> (u32, u32) {
    let s = |v: u32| {
        u32::try_from(((u64::from(v) * u64::from(zoom) + 50) / 100).max(1)).unwrap_or(u32::MAX)
    };
    (s(w), s(h))
}

/// Whether a window of `w`×`h` device pixels is within the surface limits.
pub fn allowed(w: u32, h: u32) -> bool {
    w >= 1
        && h >= 1
        && w <= MAX_SIDE
        && h <= MAX_SIDE
        && u64::from(w) * u64::from(h) * 4 <= MAX_BYTES
}

/// The next level above `zoom` for a `w`×`h` picture; `None` at [`MAX`] or when the window would
/// exceed the surface limits.
pub fn zoom_in(w: u32, h: u32, zoom: u32) -> Option<u32> {
    let next = LADDER.iter().copied().find(|z| *z > zoom)?;
    let (dw, dh) = size(w, h, next);
    allowed(dw, dh).then_some(next)
}

/// The next level below `zoom`; `None` at [`MIN`].
pub fn zoom_out(zoom: u32) -> Option<u32> {
    LADDER.iter().rev().copied().find(|z| *z < zoom)
}

/// The largest zoom of at most 100 % at which a `w`×`h` picture fits into `max_w`×`max_h`
/// (at least [`MIN`]; a picture that already fits gets 100 %).
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> u32 {
    let mut z = FULL;
    while z > MIN {
        let (dw, dh) = size(w, h, z);
        if dw <= max_w && dh <= max_h && allowed(dw, dh) {
            break;
        }
        z -= 1;
    }
    z
}

/// The new top-left corner when a pin at `origin` with zoom `from` is zoomed to `to` around the
/// screen point `anchor` (the point under it stays where it is).
pub fn around(origin: (i32, i32), anchor: (i32, i32), from: u32, to: u32) -> (i32, i32) {
    let f = |o: i32, a: i32| {
        let rel = i64::from(a) - i64::from(o);
        let moved = rel * i64::from(to) / i64::from(from.max(1));
        i32::try_from(i64::from(a) - moved).unwrap_or(o)
    };
    (f(origin.0, anchor.0), f(origin.1, anchor.1))
}

/// For each of `dst` cells along one axis, the source span `[start, end)` it covers.
fn spans(src: u32, dst: u32) -> Vec<(usize, usize)> {
    let (s, d) = (u64::from(src), u64::from(dst.max(1)));
    (0..d)
        .map(|i| {
            let a = (i * s / d).min(s - 1);
            let b = ((i + 1) * s / d).clamp(a + 1, s);
            (a as usize, b as usize)
        })
        .collect()
}

fn premultiply(p: &[u8; 4]) -> [u32; 4] {
    let a = u32::from(p[3]);
    let pm = |c: u8| (u32::from(c) * a + 127) / 255;
    [pm(p[0]), pm(p[1]), pm(p[2]), a]
}

/// Writes `frame` scaled to `w`×`h` into `dst` as premultiplied BGRA (top-down, `w × 4` bytes
/// per row), as `UpdateLayeredWindow` wants it. `dst` must hold `w × h × 4` bytes.
pub fn render(frame: &Frame, w: u32, h: u32, dst: &mut [u8]) {
    debug_assert_eq!(dst.len() as u64, u64::from(w) * u64::from(h) * 4);
    render_into(frame, w, h, dst, w as usize);
}

/// [`render`] into a larger surface whose rows are `stride` pixels apart (the picture inside a
/// pin window's outline and shadow, Step 54); `dst` starts at the picture's top-left pixel.
pub fn render_into(frame: &Frame, w: u32, h: u32, dst: &mut [u8], stride: usize) {
    let (fw, fh) = (frame.width(), frame.height());
    let src = frame.pixels().as_chunks::<4>().0;
    let out = dst.as_chunks_mut::<4>().0;
    let wu = w as usize;
    debug_assert!(stride >= wu && out.len() >= (h as usize).saturating_sub(1) * stride + wu);
    if (w, h) == (fw, fh) {
        for (y, row) in src.chunks(wu).enumerate() {
            for (d, s) in out[y * stride..y * stride + wu].iter_mut().zip(row) {
                let p = premultiply(s);
                *d = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
            }
        }
        return;
    }
    let cols = spans(fw, w);
    let rows = spans(fh, h);
    let fw = fw as usize;
    for (y, (y0, y1)) in rows.iter().enumerate() {
        let line = &mut out[y * stride..y * stride + wu];
        for (d, (x0, x1)) in line.iter_mut().zip(&cols) {
            if y1 - y0 == 1 && x1 - x0 == 1 {
                let p = premultiply(&src[y0 * fw + x0]);
                *d = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
                continue;
            }
            let mut sum = [0u32; 4];
            for yy in *y0..*y1 {
                for s in &src[yy * fw + x0..yy * fw + x1] {
                    let p = premultiply(s);
                    for (acc, v) in sum.iter_mut().zip(p) {
                        *acc += v;
                    }
                }
            }
            let n = u32::try_from((y1 - y0) * (x1 - x0)).unwrap_or(u32::MAX);
            *d = sum.map(|v| ((v + n / 2) / n).min(255) as u8);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Ledger, Rect};

    #[test]
    fn levels_sizes_and_limits() {
        assert_eq!(size(400, 300, 100), (400, 300));
        assert_eq!(size(400, 300, 33), (132, 99));
        assert_eq!(size(1, 1, 10), (1, 1), "never empty");
        assert_eq!(zoom_in(400, 300, 100), Some(110));
        assert_eq!(
            zoom_in(400, 300, 105),
            Some(110),
            "between levels: the next one"
        );
        assert_eq!(zoom_in(400, 300, 800), None, "the top");
        assert_eq!(zoom_out(100), Some(90));
        assert_eq!(zoom_out(10), None, "the bottom");
        // A 4K picture zooms in only while its window stays within the surface limits.
        assert_eq!(zoom_in(3840, 2160, 200), Some(250));
        assert_eq!(zoom_in(3840, 2160, 250), None, "300 % would need 285 MiB");
        assert_eq!(zoom_in(100, 20_000, 80), None, "16384 px at most");
        assert!(!allowed(16_385, 1));
        assert!(!allowed(0, 1));
    }

    #[test]
    fn fit_shrinks_only_what_does_not_fit() {
        assert_eq!(fit(400, 300, 1920, 1040), 100);
        assert_eq!(
            fit(3840, 2160, 1920, 1040),
            48,
            "a 4K picture on a 1080p work area"
        );
        let (w, h) = size(3840, 2160, 48);
        assert!(w <= 1920 && h <= 1040);
        assert_eq!(
            fit(100_000 / 4, 10, 100, 100),
            MIN,
            "never below the smallest zoom"
        );
    }

    #[test]
    fn zoom_keeps_the_anchor_still() {
        assert_eq!(around((100, 100), (150, 120), 100, 200), (50, 80));
        assert_eq!(around((100, 100), (100, 100), 100, 50), (100, 100));
        assert_eq!(around((-500, 0), (-400, 50), 200, 100), (-450, 25));
    }

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> (std::sync::Arc<Ledger>, Frame) {
        let ledger = Ledger::new();
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&f(x, y));
            }
        }
        let f = Frame::new(&ledger, Rect::new(0, 0, w, h), px).unwrap();
        (ledger, f)
    }

    #[test]
    fn render_premultiplies_repeats_and_averages() {
        let (_l, f) = frame(2, 2, |x, y| match (x, y) {
            (0, 0) => [200, 100, 50, 255],
            (1, 0) => [255, 255, 255, 128],
            (0, 1) => [0, 0, 0, 255],
            _ => [10, 20, 30, 0],
        });
        let mut one = vec![0u8; 16];
        render(&f, 2, 2, &mut one);
        assert_eq!(&one[..4], &[200, 100, 50, 255], "100 %: the picture itself");
        assert_eq!(&one[4..8], &[128, 128, 128, 128], "premultiplied");
        assert_eq!(&one[12..16], &[0, 0, 0, 0]);
        // 200 %: every pixel becomes a 2×2 block.
        let mut big = vec![0u8; 4 * 4 * 4];
        render(&f, 4, 4, &mut big);
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let i = ((y * 4 + x) * 4) as usize;
            assert_eq!(&big[i..i + 4], &[200, 100, 50, 255], "({x},{y})");
        }
        let i = ((3 * 4 + 3) * 4) as usize;
        assert_eq!(&big[i..i + 4], &[0, 0, 0, 0]);
        // 50 %: the average of the four.
        let mut small = vec![0u8; 4];
        render(&f, 1, 1, &mut small);
        assert_eq!(small, [82, 57, 45, 160]);
    }

    #[test]
    fn render_covers_odd_ratios_without_gaps() {
        let (_l, f) = frame(7, 5, |x, y| [x as u8 * 30, y as u8 * 50, 7, 255]);
        for (w, h) in [(3, 2), (10, 7), (1, 5), (7, 1), (13, 9)] {
            let mut out = vec![0u8; (w * h * 4) as usize];
            render(&f, w, h, &mut out);
            assert!(
                out.chunks(4).all(|p| p[3] == 255 && p[2] == 7),
                "{w}×{h}: every pixel written from the picture, opaque"
            );
        }
        let mut out = vec![0u8; 13 * 9 * 4];
        render(&f, 13, 9, &mut out);
        assert_eq!(
            &out[..4],
            &[0, 0, 7, 255],
            "zooming in keeps the top-left pixel"
        );
        let i = (8 * 13 + 12) * 4;
        assert_eq!(
            &out[i..i + 4],
            &[180, 200, 7, 255],
            "and the bottom-right one"
        );
    }
}
