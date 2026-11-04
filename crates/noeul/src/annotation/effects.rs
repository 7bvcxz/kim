//! The privacy tools' pixels: mosaic, Gaussian blur, the eraser and the solid redaction (Step 57;
//! NOEUL-FR-082; ADR 0041).
//!
//! [`super::draw`] calls these in drawing order over a straight-alpha BGRA buffer of the picture
//! part `rect`, changing pixels inside `clip` only:
//!
//! 1. [`redact`] fills **every solid redaction first** with its opaque colour, so every object
//!    drawn afterwards — a blur, a mosaic, an eraser — reads the redacted picture and can carry
//!    nothing of what a redaction covers anywhere. Each redaction is filled again at its own place
//!    in the drawing order, so annotations drawn before it are covered and later ones lie on top.
//! 2. [`restore`] keeps the picture under the erasers as it is now (redactions applied): an
//!    eraser brings that back, never the original under a redaction.
//! 3. A mosaic gives every cell of its own grid (anchored at the object, so it moves with it) the
//!    average of the cell's pixels inside the object; a blur is a Gaussian made of three box
//!    passes per direction, O(1) per pixel whatever the strength. Both read only the pixels inside
//!    the object ([`super::Object::bounds`]), average premultiplied colours (a transparent pixel
//!    adds no colour) and blend a brush stroke's soft edge by its coverage.
//!
//! Everything runs on the caller's thread when an edit or an output asks for pixels: no thread,
//! no timer, no cache that outlives the call. A blur allocates one copy of its area (4 bytes per
//! pixel) for the call; nothing touches the disk.

use super::{Area, Effect, Mask, Object, Shape};
use crate::frame::Rect;
use crate::overlay::intersection;
use std::cell::Cell;

thread_local! {
    static PASSES: Cell<u64> = const { Cell::new(0) };
}

/// How many effect areas this thread has rendered so far (tests and the measurement example use
/// it to prove nothing is processed while idle).
pub fn effect_passes() -> u64 {
    PASSES.with(Cell::get)
}

fn counted() {
    PASSES.with(|c| c.set(c.get() + 1));
}

/// The buffer [`super::draw`] works on: straight-alpha BGRA pixels of `rect`.
pub(super) struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub rect: Rect,
}

impl Canvas<'_> {
    fn at(&self, x: i64, y: i64) -> usize {
        ((y - i64::from(self.rect.y)) as usize * self.rect.width as usize
            + (x - i64::from(self.rect.x)) as usize)
            * 4
    }

    fn get(&self, x: i64, y: i64) -> [u8; 4] {
        let i = self.at(x, y);
        [
            self.buf[i],
            self.buf[i + 1],
            self.buf[i + 2],
            self.buf[i + 3],
        ]
    }

    fn set(&mut self, x: i64, y: i64, p: [u8; 4]) {
        let i = self.at(x, y);
        self.buf[i..i + 4].copy_from_slice(&p);
    }
}

/// The smallest rectangle holding both.
pub(super) fn union(a: Rect, b: Rect) -> Rect {
    let (l, t) = (a.x.min(b.x), a.y.min(b.y));
    let (r, bt) = (a.right().max(b.right()), a.bottom().max(b.bottom()));
    Rect::new(
        l,
        t,
        u32::try_from(r - i64::from(l)).unwrap_or(u32::MAX),
        u32::try_from(bt - i64::from(t)).unwrap_or(u32::MAX),
    )
}

/// The pixels an effect changes and reads, inside `rect`.
pub(super) fn region(o: &Object, rect: Rect) -> Option<Rect> {
    o.bounds().to_rect().and_then(|r| intersection(r, rect))
}

/// `area` grown until it holds every mosaic and blur it touches, whole (inside `within`): such an
/// effect is made from all of its pixels, so re-rendering part of it would read pixels it already
/// changed. Redactions and erasers read only their own pixels.
pub(super) fn reach(objects: &[Object], mut area: Rect, within: Rect) -> Rect {
    loop {
        let mut grown = area;
        for o in objects {
            if matches!(o.shape, Shape::Effect(Effect::Mosaic | Effect::Blur, _))
                && let Some(e) = region(o, within)
                && e.intersects(&grown)
            {
                grown = union(grown, e);
            }
        }
        if grown == area {
            return area;
        }
        area = grown;
    }
}

/// A brush stroke's coverage over `within` (anti-aliased edges).
fn brush(o: &Object, pts: &[super::P], within: Rect) -> Mask {
    let mut m = Mask::new(within);
    m.polyline(pts, o.width() / 2.0);
    m
}

impl Mask {
    fn cov_at(&self, x: i64, y: i64) -> u8 {
        let (rx, ry) = (i64::from(self.rect.x), i64::from(self.rect.y));
        if x < rx || y < ry || x >= self.rect.right() || y >= self.rect.bottom() {
            return 0;
        }
        self.cov[(y - ry) as usize * self.rect.width as usize + (x - rx) as usize]
    }
}

#[inline]
fn premul(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 255 {
        return p;
    }
    let m = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
    [m(p[0]), m(p[1]), m(p[2]), p[3]]
}

#[inline]
fn unpremul(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    match a {
        255 => p,
        0 => [0, 0, 0, 0],
        _ => {
            let u = |c: u8| ((u32::from(c) * 255 + a / 2) / a).min(255) as u8;
            [u(p[0]), u(p[1]), u(p[2]), p[3]]
        }
    }
}

/// `src` over `dst` by coverage `cov` (a linear mix, premultiplied when either is translucent).
#[inline]
fn mix(dst: [u8; 4], src: [u8; 4], cov: u8) -> [u8; 4] {
    match cov {
        255 => return src,
        0 => return dst,
        _ => {}
    }
    let t = u32::from(cov);
    let lerp = |d: u8, s: u8| ((u32::from(s) * t + u32::from(d) * (255 - t) + 127) / 255) as u8;
    if dst[3] == 255 && src[3] == 255 {
        return [
            lerp(dst[0], src[0]),
            lerp(dst[1], src[1]),
            lerp(dst[2], src[2]),
            255,
        ];
    }
    let (d, s) = (premul(dst), premul(src));
    unpremul([
        lerp(d[0], s[0]),
        lerp(d[1], s[1]),
        lerp(d[2], s[2]),
        lerp(d[3], s[3]),
    ])
}

/// Fills a solid redaction (opaque, whatever its colour's alpha): a box exactly, a brush on every
/// pixel it touches at all (no soft edge that would keep part of a pixel).
pub(super) fn redact(c: &mut Canvas, o: &Object, area: &Area, clip: Rect) {
    let Some(r) = region(o, c.rect).and_then(|r| intersection(r, clip)) else {
        return;
    };
    counted();
    let fill = [o.color.b, o.color.g, o.color.r, 255];
    let (x0, y0, x1, y1) = (i64::from(r.x), i64::from(r.y), r.right(), r.bottom());
    match area {
        Area::Box(_) => {
            for y in y0..y1 {
                let s = c.at(x0, y);
                for p in c.buf[s..s + r.width as usize * 4].as_chunks_mut::<4>().0 {
                    *p = fill;
                }
            }
        }
        Area::Brush(pts) => {
            let m = brush(o, pts, r);
            for y in y0..y1 {
                for x in x0..x1 {
                    if m.cov_at(x, y) > 0 {
                        c.set(x, y, fill);
                    }
                }
            }
        }
    }
}

/// The picture under the erasers as it is before any object but the redactions is drawn.
pub(super) struct Restore {
    rect: Rect,
    px: Vec<u8>,
}

impl Restore {
    fn get(&self, x: i64, y: i64) -> [u8; 4] {
        let i = ((y - i64::from(self.rect.y)) as usize * self.rect.width as usize
            + (x - i64::from(self.rect.x)) as usize)
            * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
}

/// Keeps the pixels the erasers inside `clip` will bring back (`None` without an eraser there).
pub(super) fn restore(c: &Canvas, objects: &[Object], clip: Rect) -> Option<Restore> {
    let rect = objects
        .iter()
        .filter(|o| matches!(o.shape, Shape::Effect(Effect::Erase, _)))
        .filter_map(|o| region(o, c.rect).and_then(|r| intersection(r, clip)))
        .reduce(union)?;
    let mut px = Vec::with_capacity(rect.bgra_bytes() as usize);
    for y in i64::from(rect.y)..rect.bottom() {
        let s = c.at(i64::from(rect.x), y);
        px.extend_from_slice(&c.buf[s..s + rect.width as usize * 4]);
    }
    Some(Restore { rect, px })
}

/// Draws one effect object at its place in the drawing order.
pub(super) fn apply(
    c: &mut Canvas,
    o: &Object,
    effect: Effect,
    area: &Area,
    clip: Rect,
    restore: Option<&Restore>,
) {
    match effect {
        Effect::Redact => redact(c, o, area, clip),
        Effect::Mosaic => mosaic(c, o, area, clip),
        Effect::Blur => blur(c, o, area, clip),
        Effect::Erase => erase(c, o, area, clip, restore),
    }
}

fn erase(c: &mut Canvas, o: &Object, area: &Area, clip: Rect, restore: Option<&Restore>) {
    let (Some(r), Some(s)) = (
        region(o, c.rect).and_then(|r| intersection(r, clip)),
        restore,
    ) else {
        return;
    };
    counted();
    let m = match area {
        Area::Box(_) => None,
        Area::Brush(pts) => Some(brush(o, pts, r)),
    };
    for y in i64::from(r.y)..r.bottom() {
        for x in i64::from(r.x)..r.right() {
            let back = s.get(x, y);
            match &m {
                None => c.set(x, y, back),
                Some(m) => {
                    let cov = m.cov_at(x, y);
                    if cov > 0 {
                        let d = c.get(x, y);
                        c.set(x, y, mix(d, back, cov));
                    }
                }
            }
        }
    }
}

/// Where an effect's own grid starts: the box's corner, or a stroke's first pixel.
fn anchor(area: &Area) -> (i64, i64) {
    match area {
        Area::Box(b) => (b.l.floor() as i64, b.t.floor() as i64),
        Area::Brush(pts) => pts
            .first()
            .map_or((0, 0), |p| (p.x.floor() as i64, p.y.floor() as i64)),
    }
}

fn mosaic(c: &mut Canvas, o: &Object, area: &Area, clip: Rect) {
    let Some(e) = region(o, c.rect) else {
        return;
    };
    let Some(out) = intersection(e, clip) else {
        return;
    };
    counted();
    let cell = i64::from(o.strength().max(2));
    let (ax, ay) = anchor(area);
    let m = match area {
        Area::Box(_) => None,
        Area::Brush(pts) => Some(brush(o, pts, out)),
    };
    let (ex0, ey0, ex1, ey1) = (i64::from(e.x), i64::from(e.y), e.right(), e.bottom());
    let (ox0, oy0, ox1, oy1) = (
        i64::from(out.x),
        i64::from(out.y),
        out.right(),
        out.bottom(),
    );
    let first = |v: i64, a: i64| a + (v - a).div_euclid(cell) * cell;
    let mut cy = first(oy0, ay);
    while cy < oy1 {
        let mut cx = first(ox0, ax);
        while cx < ox1 {
            // The cell's average over its pixels inside the object (premultiplied).
            let (mut n, mut sa) = (0u64, 0u64);
            let mut sc = [0u64; 3];
            for y in cy.max(ey0)..(cy + cell).min(ey1) {
                for x in cx.max(ex0)..(cx + cell).min(ex1) {
                    let p = c.get(x, y);
                    let a = u64::from(p[3]);
                    n += 1;
                    sa += a;
                    for k in 0..3 {
                        sc[k] += u64::from(p[k]) * a;
                    }
                }
            }
            if let Some(alpha) = (sa + n / 2).checked_div(n) {
                let avg = if sa == 0 {
                    [0, 0, 0, 0]
                } else {
                    let ch = |k: usize| ((sc[k] + sa / 2) / sa).min(255) as u8;
                    [ch(0), ch(1), ch(2), alpha.min(255) as u8]
                };
                for y in cy.max(oy0)..(cy + cell).min(oy1) {
                    for x in cx.max(ox0)..(cx + cell).min(ox1) {
                        match &m {
                            None => c.set(x, y, avg),
                            Some(m) => {
                                let cov = m.cov_at(x, y);
                                if cov > 0 {
                                    let d = c.get(x, y);
                                    c.set(x, y, mix(d, avg, cov));
                                }
                            }
                        }
                    }
                }
            }
            cx += cell;
        }
        cy += cell;
    }
}

/// The radii of three box blurs whose sum approximates a Gaussian of `sigma` (the classic
/// construction: box widths `wl` and `wl + 2` around √(12σ²/n + 1)).
pub(super) fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0_f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl = wl.max(1);
    let wu = wl + 2;
    let (wlf, s2) = (wl as f32, sigma * sigma);
    let m = ((12.0 * s2 - n * wlf * wlf - 4.0 * n * wlf - 3.0 * n) / (-4.0 * wlf - 4.0)).round();
    let m = m.clamp(0.0, 3.0) as usize;
    let mut out = [0; 3];
    for (i, r) in out.iter_mut().enumerate() {
        let w = if i < m { wl } else { wu };
        *r = usize::try_from((w - 1) / 2).unwrap_or(0);
    }
    out
}

/// One box pass of radius `r` over `n` pixels of `src` (edges repeated) into `dst`.
fn box_line(src: &[u8], dst: &mut [u8], n: usize, r: usize) {
    let win = (2 * r + 1) as u32;
    let last = n - 1;
    for ch in 0..4 {
        let v = |i: usize| u32::from(src[i.min(last) * 4 + ch]);
        let mut sum = (r as u32 + 1) * v(0) + (1..=r).map(v).sum::<u32>();
        for i in 0..n {
            dst[i * 4 + ch] = ((sum + win / 2) / win) as u8;
            sum = sum + v(i + r + 1) - v(i.saturating_sub(r));
        }
    }
}

fn blur(c: &mut Canvas, o: &Object, area: &Area, clip: Rect) {
    let Some(e) = region(o, c.rect) else {
        return;
    };
    let Some(out) = intersection(e, clip) else {
        return;
    };
    counted();
    let (w, h) = (e.width as usize, e.height as usize);
    // A premultiplied copy of the object's pixels: the passes read nothing outside it.
    let mut px = Vec::with_capacity(w * h * 4);
    for y in i64::from(e.y)..e.bottom() {
        let s = c.at(i64::from(e.x), y);
        for p in c.buf[s..s + w * 4].as_chunks::<4>().0 {
            px.extend_from_slice(&premul(*p));
        }
    }
    let mut a = vec![0u8; w.max(h) * 4];
    let mut b = vec![0u8; w.max(h) * 4];
    for r in box_radii(o.strength().max(1) as f32) {
        if r == 0 {
            continue;
        }
        for row in px.chunks_exact_mut(w * 4) {
            a[..w * 4].copy_from_slice(row);
            box_line(&a, row, w, r);
        }
        for x in 0..w {
            for y in 0..h {
                a[y * 4..y * 4 + 4].copy_from_slice(&px[(y * w + x) * 4..(y * w + x) * 4 + 4]);
            }
            box_line(&a, &mut b, h, r);
            for y in 0..h {
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&b[y * 4..y * 4 + 4]);
            }
        }
    }
    let m = match area {
        Area::Box(_) => None,
        Area::Brush(pts) => Some(brush(o, pts, out)),
    };
    for y in i64::from(out.y)..out.bottom() {
        for x in i64::from(out.x)..out.right() {
            let i = ((y - i64::from(e.y)) as usize * w + (x - i64::from(e.x)) as usize) * 4;
            let q = unpremul([px[i], px[i + 1], px[i + 2], px[i + 3]]);
            match &m {
                None => c.set(x, y, q),
                Some(m) => {
                    let cov = m.cov_at(x, y);
                    if cov > 0 {
                        let d = c.get(x, y);
                        c.set(x, y, mix(d, q, cov));
                    }
                }
            }
        }
    }
}
