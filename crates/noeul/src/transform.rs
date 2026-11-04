//! Pin transforms and what a pin window shows around its picture (Step 54, ADR 0044;
//! NOEUL-FR-061, NOEUL-FR-062, NOEUL-FR-063, NOEUL-FR-090, NOEUL-SEC-013).
//!
//! Platform independent and pure, so every transform and its inverse is tested headlessly:
//!
//! - [`Orientation`]: rotating by 90° and flipping. The eight orientations of a rectangle form a
//!   group: every operation has an inverse (`rotate_ccw` undoes `rotate_cw`, each flip undoes
//!   itself), so a pin can always be turned back exactly. The picture itself is never changed: an
//!   orientation is applied to what the window shows ([`Orientation::apply`], a pixel-exact
//!   remap, no resampling).
//! - **Opacity** in percent, 10–100 in steps of 10 ([`opacity_up`], [`opacity_down`]); never below
//!   [`OPACITY_MIN`] (NOEUL-FR-062), applied by the window as a constant alpha, so changing it
//!   renders nothing again.
//! - [`thumbnail_size`]: a thumbnail fits into 96 × 96 logical pixels of the pin's monitor.
//! - [`Edge`], [`edge_at`] and [`resize`]: dragging a pin's border or its shadow resizes it,
//!   keeping the aspect ratio (resizing a picture is zooming it, to any whole percent between the
//!   zoom limits); the opposite side stays where it was.
//! - [`Look`], [`View`] and [`paint`]: the window's surface: the picture scaled to its rectangle,
//!   a one-pixel `border.subtle` outline and the soft pin shadow outside it (UX spec §10, both
//!   switchable off), and the click-through badge in the picture's top-right corner. Border,
//!   shadow, badge and grab band scale with the DPI of the pin's monitor; the picture keeps one
//!   image pixel per device pixel at 100 % everywhere (NOEUL-FR-061).

use crate::capture::scale_px;
use crate::frame::{Frame, Rect};
use crate::zoom;
use serde_json::{Value, json};

/// A pin's orientation: the picture is mirrored left-right first (when `mirrored`), then turned
/// clockwise by `rotation` degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Orientation {
    quarter: u8,
    mirrored: bool,
}

impl Orientation {
    /// As captured.
    pub const IDENTITY: Orientation = Orientation {
        quarter: 0,
        mirrored: false,
    };

    /// `rotation` clockwise in degrees (0, 90, 180 or 270) after mirroring; `None` otherwise.
    pub fn new(rotation: u32, mirrored: bool) -> Option<Orientation> {
        (rotation % 90 == 0 && rotation < 360).then(|| Orientation {
            quarter: u8::try_from(rotation / 90).unwrap_or(0),
            mirrored,
        })
    }

    /// Degrees clockwise: 0, 90, 180 or 270.
    pub fn rotation(self) -> u32 {
        u32::from(self.quarter) * 90
    }

    pub fn mirrored(self) -> bool {
        self.mirrored
    }

    pub fn is_identity(self) -> bool {
        self == Orientation::IDENTITY
    }

    /// Turned a quarter clockwise (`pin.rotate-cw`); [`Orientation::rotate_ccw`] undoes it.
    #[must_use]
    pub fn rotate_cw(self) -> Orientation {
        Orientation {
            quarter: (self.quarter + 1) % 4,
            ..self
        }
    }

    /// Turned a quarter counter-clockwise (`pin.rotate-ccw`).
    #[must_use]
    pub fn rotate_ccw(self) -> Orientation {
        Orientation {
            quarter: (self.quarter + 3) % 4,
            ..self
        }
    }

    /// Mirrored left-right as shown on screen (`pin.flip-horizontal`); its own inverse.
    #[must_use]
    pub fn flip_horizontal(self) -> Orientation {
        // H ∘ R^q ∘ M^m = R^-q ∘ M^(m+1): a reflection conjugates a rotation into its inverse.
        Orientation {
            quarter: (4 - self.quarter) % 4,
            mirrored: !self.mirrored,
        }
    }

    /// Mirrored top-bottom as shown on screen (`pin.flip-vertical`); its own inverse.
    #[must_use]
    pub fn flip_vertical(self) -> Orientation {
        // V = R² ∘ H, so V ∘ R^q ∘ M^m = R^(2-q) ∘ M^(m+1).
        Orientation {
            quarter: (6 - self.quarter) % 4,
            mirrored: !self.mirrored,
        }
    }

    /// Whether width and height trade places (a quarter or three quarters turned).
    pub fn swaps(self) -> bool {
        self.quarter % 2 == 1
    }

    /// The shown size of a `w`×`h` picture.
    pub fn size(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps() { (h, w) } else { (w, h) }
    }

    /// Where pixel (`x`, `y`) of a `w`×`h` picture is shown (relative to the shown top-left).
    pub fn map(self, x: u32, y: u32, w: u32, h: u32) -> (u32, u32) {
        let x = if self.mirrored { w - 1 - x } else { x };
        match self.quarter {
            0 => (x, y),
            1 => (h - 1 - y, x),
            2 => (w - 1 - x, h - 1 - y),
            _ => (y, w - 1 - x),
        }
    }

    /// Where the part `r` of a `w`×`h` picture (relative to its top-left) is shown.
    pub fn map_rect(self, r: Rect, w: u32, h: u32) -> Rect {
        if r.is_empty() {
            return r;
        }
        let (x0, y0) = (
            u32::try_from(r.x).unwrap_or(0),
            u32::try_from(r.y).unwrap_or(0),
        );
        let (x1, y1) = (
            (x0 + r.width - 1).min(w.saturating_sub(1)),
            (y0 + r.height - 1).min(h.saturating_sub(1)),
        );
        let (a, b) = (self.map(x0, y0, w, h), self.map(x1, y1, w, h));
        let (l, t) = (a.0.min(b.0), a.1.min(b.1));
        let (rr, bb) = (a.0.max(b.0), a.1.max(b.1));
        Rect::new(
            i32::try_from(l).unwrap_or(0),
            i32::try_from(t).unwrap_or(0),
            rr - l + 1,
            bb - t + 1,
        )
    }

    /// The picture as shown, in a new frame at the same origin; `None` for [`Orientation::IDENTITY`]
    /// (the picture itself is shown). Every pixel is moved, none is resampled.
    pub fn apply(self, frame: &Frame) -> Option<Frame> {
        if self.is_identity() {
            return None;
        }
        let (w, h) = (frame.width(), frame.height());
        let (sw, sh) = self.size(w, h);
        let src = frame.pixels().as_chunks::<4>().0;
        let mut out = vec![0u8; frame.pixels().len()];
        let dst = out.as_chunks_mut::<4>().0;
        let stride = sw as usize;
        for y in 0..h {
            let row = &src[y as usize * w as usize..(y as usize + 1) * w as usize];
            for (x, p) in row.iter().enumerate() {
                let (dx, dy) = self.map(x as u32, y, w, h);
                dst[dy as usize * stride + dx as usize] = *p;
            }
        }
        let r = frame.rect();
        frame.derived(Rect::new(r.x, r.y, sw, sh), out).ok()
    }

    pub fn to_json(self) -> Value {
        json!({"rotation": self.rotation(), "mirrored": self.mirrored})
    }
}

/// The lowest opacity a pin can have (percent, NOEUL-FR-062): a pin never becomes invisible.
pub const OPACITY_MIN: u32 = 10;
/// Fully opaque.
pub const OPACITY_FULL: u32 = 100;
/// One `pin.opacity-up`/`-down` press or `Ctrl`+wheel notch.
pub const OPACITY_STEP: u32 = 10;

/// The next opacity step above `o`; `None` at 100 %.
pub fn opacity_up(o: u32) -> Option<u32> {
    (o < OPACITY_FULL).then(|| ((o / OPACITY_STEP + 1) * OPACITY_STEP).min(OPACITY_FULL))
}

/// The next opacity step below `o`; `None` at [`OPACITY_MIN`].
pub fn opacity_down(o: u32) -> Option<u32> {
    (o > OPACITY_MIN).then(|| {
        (o.div_ceil(OPACITY_STEP) * OPACITY_STEP)
            .saturating_sub(OPACITY_STEP)
            .max(OPACITY_MIN)
    })
}

/// `o` within 10–100 %.
pub fn clamp_opacity(o: u32) -> u32 {
    o.clamp(OPACITY_MIN, OPACITY_FULL)
}

/// The constant alpha (0–255) of an opacity in percent.
pub fn alpha(o: u32) -> u8 {
    u8::try_from((clamp_opacity(o) * 255 + 50) / 100).unwrap_or(255)
}

/// The longest side of a thumbnail, in logical pixels of the pin's monitor.
pub const THUMBNAIL: u32 = 96;

/// The size of the thumbnail of a picture shown at `w`×`h` at 100 % on a monitor of `dpi`: the
/// whole picture, its longest side [`THUMBNAIL`] logical pixels (a smaller picture keeps its size).
pub fn thumbnail_size(w: u32, h: u32, dpi: u32) -> (u32, u32) {
    let t = scale_px(THUMBNAIL, dpi);
    if w <= t && h <= t {
        return (w.max(1), h.max(1));
    }
    let scale = |a: u32, b: u32| {
        u32::try_from((u64::from(a) * u64::from(t) + u64::from(b) / 2) / u64::from(b.max(1)))
            .unwrap_or(1)
            .max(1)
    };
    if w >= h {
        (t, scale(h, w))
    } else {
        (scale(w, h), t)
    }
}

/// A side or corner of a pin that dragging resizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    pub fn left(self) -> bool {
        matches!(self, Edge::Left | Edge::TopLeft | Edge::BottomLeft)
    }

    pub fn right(self) -> bool {
        matches!(self, Edge::Right | Edge::TopRight | Edge::BottomRight)
    }

    pub fn top(self) -> bool {
        matches!(self, Edge::Top | Edge::TopLeft | Edge::TopRight)
    }

    pub fn bottom(self) -> bool {
        matches!(self, Edge::Bottom | Edge::BottomLeft | Edge::BottomRight)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::TopLeft => "top-left",
            Edge::TopRight => "top-right",
            Edge::BottomLeft => "bottom-left",
            Edge::BottomRight => "bottom-right",
        }
    }
}

/// Which border of a pin whose picture is `r` the screen point `p` grabs: anything within
/// `outside` device pixels outside the picture (its outline and shadow) or `inside` pixels inside
/// it. `inside` shrinks on a small pin, so its middle always moves it; a corner wins where two
/// edges meet (its zone runs twice the band along each side). `None` elsewhere (a drag moves the
/// pin) and outside the band.
pub fn edge_at(r: Rect, outside: u32, inside: u32, p: (i32, i32)) -> Option<Edge> {
    let (w, h) = (i64::from(r.width), i64::from(r.height));
    let (x, y) = (
        i64::from(p.0) - i64::from(r.x),
        i64::from(p.1) - i64::from(r.y),
    );
    let out = i64::from(outside);
    if x < -out || y < -out || x >= w + out || y >= h + out {
        return None;
    }
    let inner = i64::from(inside).min(w / 4).min(h / 4);
    let (l, rr, t, b) = (x < inner, x >= w - inner, y < inner, y >= h - inner);
    if !(l || rr || t || b) {
        return None;
    }
    let corner = 2 * (out + inner).max(1) - out;
    let (near_t, near_b) = (y < corner, y >= h - corner);
    let (near_l, near_r) = (x < corner, x >= w - corner);
    Some(
        match (l || (near_l && (t || b)), rr || (near_r && (t || b))) {
            (true, _) if t || near_t => Edge::TopLeft,
            (true, _) if b || near_b => Edge::BottomLeft,
            (true, _) => Edge::Left,
            (_, true) if t || near_t => Edge::TopRight,
            (_, true) if b || near_b => Edge::BottomRight,
            (_, true) => Edge::Right,
            _ if t => Edge::Top,
            _ => Edge::Bottom,
        },
    )
}

/// The zoom and the picture's rectangle after `edge` of a pin whose picture was `start` when the
/// drag began (`base`: its shown size at 100 %) was dragged by `delta` device pixels. The aspect
/// ratio is kept: a side drives the scale along its axis, a corner the axis that changed more.
/// The opposite side (and, for a side, the top or left) stays where it was; the zoom stays
/// within the zoom limits and the window surface limits, so a drag past them stops there.
pub fn resize(start: Rect, base: (u32, u32), edge: Edge, delta: (i32, i32)) -> (u32, Rect) {
    let (bw, bh) = (f64::from(base.0.max(1)), f64::from(base.1.max(1)));
    let grow = |len: u32, d: i32, plus: bool, minus: bool| {
        let d = f64::from(d);
        let v = f64::from(len)
            + if plus {
                d
            } else if minus {
                -d
            } else {
                0.0
            };
        v.max(1.0)
    };
    let pw = grow(start.width, delta.0, edge.right(), edge.left());
    let ph = grow(start.height, delta.1, edge.bottom(), edge.top());
    let (zx, zy) = (pw * 100.0 / bw, ph * 100.0 / bh);
    let z = match edge {
        Edge::Left | Edge::Right => zx,
        Edge::Top | Edge::Bottom => zy,
        _ => {
            let rx = (pw / f64::from(start.width.max(1)) - 1.0).abs();
            let ry = (ph / f64::from(start.height.max(1)) - 1.0).abs();
            if rx >= ry { zx } else { zy }
        }
    };
    let mut z = (z.round() as u32).clamp(zoom::MIN, zoom::MAX);
    while z > zoom::MIN && {
        let (w, h) = zoom::size(base.0, base.1, z);
        !zoom::allowed(w, h)
    } {
        z -= 1;
    }
    let (nw, nh) = zoom::size(base.0, base.1, z);
    let x = if edge.left() {
        i32::try_from(start.right() - i64::from(nw)).unwrap_or(start.x)
    } else {
        start.x
    };
    let y = if edge.top() {
        i32::try_from(start.bottom() - i64::from(nh)).unwrap_or(start.y)
    } else {
        start.y
    };
    (z, Rect::new(x, y, nw, nh))
}

/// What a pin window draws around its picture (UX spec §10; `[pins] border` and `[pins] shadow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// A one-pixel `border.subtle` outline.
    pub border: bool,
    /// The soft pin shadow (`pin.shadow`, design/tokens.json).
    pub shadow: bool,
}

impl Default for Look {
    fn default() -> Look {
        Look {
            border: true,
            shadow: true,
        }
    }
}

/// `border.subtle` (design/tokens.json), BGRA.
pub const BORDER_BGRA: [u8; 4] = [0x2F, 0x25, 0x26, 0xFF];
/// `accent` (design/tokens.json), BGRA: the click-through badge.
pub const ACCENT_BGRA: [u8; 4] = [0xF5, 0xA6, 0xB7, 0xFF];
/// The pin shadow `0 2 10 rgba(0,0,0,0.40)` (token `pin.shadow`): offset down, blur radius and
/// alpha. Smaller than the menus' shadow because a pin window is hit-tested by its pixels: the
/// shadow is the grab band for resizing, not a 30-pixel ring that catches clicks meant for the
/// program below (ADR 0044).
pub const SHADOW_OFFSET: u32 = 2;
pub const SHADOW_BLUR: u32 = 10;
pub const SHADOW_ALPHA: f64 = 0.40;
/// The band inside the picture that grabs its border (logical pixels).
pub const GRAB_INSIDE: u32 = 4;
/// The click-through badge's diameter and inset (logical pixels).
pub const BADGE: u32 = 14;
pub const BADGE_INSET: u32 = 4;

/// Everything a pin window needs to draw itself, besides its picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    /// The picture on screen, in device pixels (its window is larger by [`View::margin`]).
    pub rect: Rect,
    pub look: Look,
    /// The effective DPI of the pin's monitor: border, shadow, badge and grab band scale with it.
    pub dpi: u32,
    /// Percent, 10–100.
    pub opacity: u32,
    /// The pin lets mouse input through to the window below, and shows the badge.
    pub click_through: bool,
    /// Dragging its border resizes it (not a thumbnail).
    pub resizable: bool,
}

impl View {
    /// A plain view of `rect`: default look, 100 %, opaque, clickable, resizable.
    pub fn plain(rect: Rect) -> View {
        View {
            rect,
            look: Look::default(),
            dpi: crate::capture::BASE_DPI,
            opacity: OPACITY_FULL,
            click_through: false,
            resizable: true,
        }
    }

    /// The outline's width: one device pixel per 100 % of scale.
    pub fn border(&self) -> u32 {
        if self.look.border {
            (self.dpi / crate::capture::BASE_DPI).max(1)
        } else {
            0
        }
    }

    /// How far the shadow reaches outside the outline.
    pub fn shadow(&self) -> u32 {
        if self.look.shadow {
            scale_px(SHADOW_BLUR + SHADOW_OFFSET, self.dpi)
        } else {
            0
        }
    }

    /// The window's extra size on each side of the picture.
    pub fn margin(&self) -> u32 {
        self.border() + self.shadow()
    }

    /// The window: the picture plus the margin on every side.
    pub fn window(&self) -> Rect {
        let m = self.margin();
        let shift = |v: i32| i32::try_from(i64::from(v) - i64::from(m)).unwrap_or(v);
        Rect::new(
            shift(self.rect.x),
            shift(self.rect.y),
            self.rect.width.saturating_add(2 * m),
            self.rect.height.saturating_add(2 * m),
        )
    }

    /// The resize grab band: device pixels outside and inside the picture (none for a thumbnail).
    pub fn band(&self) -> (u32, u32) {
        if self.resizable {
            (self.margin(), scale_px(GRAB_INSIDE, self.dpi))
        } else {
            (0, 0)
        }
    }

    /// The click-through badge's square, relative to the picture's top-left, while shown.
    pub fn badge(&self) -> Option<Rect> {
        if !self.click_through {
            return None;
        }
        let (w, h) = (self.rect.width, self.rect.height);
        let d = scale_px(BADGE, self.dpi).min(w).min(h).max(1);
        let inset = scale_px(BADGE_INSET, self.dpi)
            .min((w - d) / 2)
            .min((h - d) / 2);
        Some(Rect::new(
            i32::try_from(w - d - inset).unwrap_or(0),
            i32::try_from(inset).unwrap_or(0),
            d,
            d,
        ))
    }
}

/// erf(x), Abramowitz–Stegun 7.1.26 (absolute error below 1.5e-7).
fn erf(x: f64) -> f64 {
    let s = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    s * y
}

/// The share of a Gaussian-blurred box `[a, b)` that covers pixel `i` (centre `i + 0.5`).
fn coverage(i: i64, a: f64, b: f64, sigma: f64) -> f64 {
    let c = i as f64 + 0.5;
    let phi = |z: f64| 0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2));
    (phi((c - a) / sigma) - phi((c - b) / sigma)).clamp(0.0, 1.0)
}

/// Premultiplied `src` over premultiplied `dst`.
fn over(dst: &mut [u8; 4], src: [u8; 4]) {
    let inv = 255 - u32::from(src[3]);
    for i in 0..4 {
        dst[i] = (u32::from(src[i]) + (u32::from(dst[i]) * inv + 127) / 255).min(255) as u8;
    }
}

/// `bgra` (straight alpha) with `cover` (0–1) as a premultiplied pixel.
fn premul(bgra: [u8; 4], cover: f64) -> [u8; 4] {
    let a = (f64::from(bgra[3]) * cover.clamp(0.0, 1.0)).round();
    let c = |v: u8| (f64::from(v) * a / 255.0).round() as u8;
    [c(bgra[0]), c(bgra[1]), c(bgra[2]), a as u8]
}

/// Paints the window's surface (`view.window()`'s size, premultiplied BGRA, top-down) into
/// `dst`: the shadow and outline outside the picture, `frame` scaled to `view.rect`'s size, and
/// the click-through badge. While resizable, every pixel of the margin has an alpha of at least 1
/// so the whole band is hit by the mouse (a layered window lets clicks through where alpha is 0).
pub fn paint(frame: &Frame, view: &View, dst: &mut [u8]) {
    let win = view.window();
    let (ww, wh) = (win.width as usize, win.height as usize);
    let m = view.margin() as usize;
    let (pw, ph) = (view.rect.width as usize, view.rect.height as usize);
    debug_assert_eq!(dst.len(), ww * wh * 4);
    dst.fill(0);
    let px = dst.as_chunks_mut::<4>().0;
    let b = view.border() as usize;
    if view.look.shadow {
        let sigma = f64::from(scale_px(SHADOW_BLUR, view.dpi)) / 2.0;
        let off = f64::from(scale_px(SHADOW_OFFSET, view.dpi));
        // The shadow of the outlined picture, moved down.
        let (x0, x1) = ((m - b) as f64, (m + pw + b) as f64);
        let (y0, y1) = ((m - b) as f64 + off, (m + ph + b) as f64 + off);
        let cx: Vec<f64> = (0..ww).map(|i| coverage(i as i64, x0, x1, sigma)).collect();
        let cy: Vec<f64> = (0..wh).map(|i| coverage(i as i64, y0, y1, sigma)).collect();
        for (y, fy) in cy.iter().enumerate() {
            for (x, fx) in cx.iter().enumerate() {
                let a = (SHADOW_ALPHA * fx * fy * 255.0).round() as u8;
                px[y * ww + x] = [0, 0, 0, a];
            }
        }
    }
    if view.resizable && m > 0 {
        for p in px.iter_mut() {
            p[3] = p[3].max(1);
        }
    }
    if b > 0 {
        let (l, t, r, bt) = (m - b, m - b, m + pw + b, m + ph + b);
        for y in t..bt {
            for x in l..r {
                if x < m || x >= m + pw || y < m || y >= m + ph {
                    px[y * ww + x] = BORDER_BGRA;
                }
            }
        }
    }
    // The picture inside, clear of the shadow (a transparent picture shows what is behind it).
    for y in m..m + ph {
        px[y * ww + m..y * ww + m + pw].fill([0, 0, 0, 0]);
    }
    let start = (m * ww + m) * 4;
    zoom::render_into(
        frame,
        view.rect.width,
        view.rect.height,
        &mut dst[start..],
        ww,
    );
    if let Some(badge) = view.badge() {
        let px = dst.as_chunks_mut::<4>().0;
        let d = f64::from(badge.width);
        let r = d / 2.0;
        let ring = f64::from((view.dpi / crate::capture::BASE_DPI).max(1));
        let (bx, by) = (
            m + usize::try_from(badge.x).unwrap_or(0),
            m + usize::try_from(badge.y).unwrap_or(0),
        );
        for y in 0..badge.height as usize {
            for x in 0..badge.width as usize {
                let (dx, dy) = (x as f64 + 0.5 - r, y as f64 + 0.5 - r);
                let dist = (dx * dx + dy * dy).sqrt();
                let p = &mut px[(by + y) * ww + bx + x];
                over(p, premul([0, 0, 0, 220], r - dist + 0.5));
                over(p, premul(ACCENT_BGRA, r - ring - dist + 0.5));
                over(p, premul([255, 255, 255, 255], r * 0.38 - dist + 0.5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Ledger;

    const ALL: [Orientation; 8] = [
        Orientation {
            quarter: 0,
            mirrored: false,
        },
        Orientation {
            quarter: 1,
            mirrored: false,
        },
        Orientation {
            quarter: 2,
            mirrored: false,
        },
        Orientation {
            quarter: 3,
            mirrored: false,
        },
        Orientation {
            quarter: 0,
            mirrored: true,
        },
        Orientation {
            quarter: 1,
            mirrored: true,
        },
        Orientation {
            quarter: 2,
            mirrored: true,
        },
        Orientation {
            quarter: 3,
            mirrored: true,
        },
    ];

    fn pattern(ledger: &std::sync::Arc<Ledger>, w: u32, h: u32) -> Frame {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as u8, y as u8, (x * 7 + y * 13) as u8, 255]);
            }
        }
        Frame::new(ledger, Rect::new(5, 6, w, h), px).unwrap()
    }

    fn at(f: &Frame, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * f.width() + x) * 4) as usize;
        f.pixels()[i..i + 4].try_into().unwrap()
    }

    /// Every operation has an inverse, from every one of the eight orientations (NOEUL-FR-062:
    /// each transform is reversible).
    #[test]
    fn every_transform_is_undone_by_its_inverse() {
        for o in ALL {
            assert_eq!(o.rotate_cw().rotate_ccw(), o);
            assert_eq!(o.rotate_ccw().rotate_cw(), o);
            assert_eq!(o.flip_horizontal().flip_horizontal(), o);
            assert_eq!(o.flip_vertical().flip_vertical(), o);
            assert_eq!(o.rotate_cw().rotate_cw().rotate_cw().rotate_cw(), o);
            // Two flips are half a turn.
            assert_eq!(
                o.flip_horizontal().flip_vertical(),
                o.rotate_cw().rotate_cw()
            );
            assert_eq!(Orientation::new(o.rotation(), o.mirrored()), Some(o));
        }
        assert_eq!(Orientation::new(45, false), None);
        assert_eq!(Orientation::new(360, false), None);
        assert!(Orientation::IDENTITY.is_identity());
    }

    /// The orientations act on the pixels as their names say, every pixel lands exactly once,
    /// and the inverse brings back the very same picture.
    #[test]
    fn orientations_move_every_pixel_exactly() {
        let ledger = Ledger::new();
        let f = pattern(&ledger, 5, 3);
        let cw = Orientation::IDENTITY.rotate_cw().apply(&f).unwrap();
        assert_eq!(
            (cw.width(), cw.height()),
            (3, 5),
            "a quarter turn swaps the sides"
        );
        assert_eq!(cw.rect().x, 5, "the origin stays");
        assert_eq!(
            at(&cw, 2, 0),
            at(&f, 0, 0),
            "top-left goes to the top-right"
        );
        assert_eq!(
            at(&cw, 0, 4),
            at(&f, 4, 2),
            "bottom-right goes to the bottom-left"
        );
        let h = Orientation::IDENTITY.flip_horizontal().apply(&f).unwrap();
        assert_eq!(at(&h, 4, 0), at(&f, 0, 0));
        assert_eq!(at(&h, 0, 2), at(&f, 4, 2));
        let v = Orientation::IDENTITY.flip_vertical().apply(&f).unwrap();
        assert_eq!(at(&v, 0, 2), at(&f, 0, 0));
        assert_eq!(at(&v, 4, 0), at(&f, 4, 2));
        for o in ALL {
            let shown = o.apply(&f).unwrap_or_else(|| f.crop(f.rect()).unwrap());
            let mut seen = vec![false; 15];
            for y in 0..3 {
                for x in 0..5 {
                    let (dx, dy) = o.map(x, y, 5, 3);
                    assert_eq!(at(&shown, dx, dy), at(&f, x, y), "{o:?} ({x},{y})");
                    let (sw, _) = o.size(5, 3);
                    let i = (dy * sw + dx) as usize;
                    assert!(!seen[i], "{o:?}: two pixels on one");
                    seen[i] = true;
                }
            }
        }
        // A transform followed by its inverse shows the original pixels again.
        let there = Orientation::IDENTITY.rotate_cw().flip_vertical();
        let back = there.flip_vertical().rotate_ccw();
        assert!(back.apply(&f).is_none(), "back to the identity");
        // Rotating four times shows the picture unchanged.
        let mut o = Orientation::IDENTITY;
        for _ in 0..4 {
            o = o.rotate_cw();
        }
        assert!(o.apply(&f).is_none());
        // A part of the picture maps to where it is shown.
        let part = Rect::new(1, 0, 2, 1);
        assert_eq!(
            Orientation::IDENTITY.rotate_cw().map_rect(part, 5, 3),
            Rect::new(2, 1, 1, 2)
        );
        assert_eq!(
            Orientation::IDENTITY.flip_horizontal().map_rect(part, 5, 3),
            Rect::new(2, 0, 2, 1)
        );
        drop((cw, h, v));
        drop(f);
        assert_eq!(
            ledger.live(),
            (0, 0),
            "the shown copies are accounted and freed"
        );
    }

    #[test]
    fn opacity_steps_stay_within_ten_and_a_hundred_percent() {
        assert_eq!(opacity_up(100), None);
        assert_eq!(opacity_up(90), Some(100));
        assert_eq!(opacity_up(25), Some(30), "back onto the steps");
        assert_eq!(opacity_down(100), Some(90));
        assert_eq!(opacity_down(25), Some(20));
        assert_eq!(opacity_down(20), Some(10));
        assert_eq!(opacity_down(10), None, "never below 10 %");
        assert_eq!(opacity_down(15), Some(10));
        let mut o = 100;
        let mut downs = 0;
        while let Some(n) = opacity_down(o) {
            o = n;
            downs += 1;
        }
        assert_eq!((o, downs), (10, 9));
        while let Some(n) = opacity_up(o) {
            o = n;
        }
        assert_eq!(o, 100, "every step down has a step up");
        assert_eq!(
            (alpha(100), alpha(10), alpha(0), alpha(500)),
            (255, 26, 26, 255)
        );
        assert_eq!(clamp_opacity(3), 10);
    }

    #[test]
    fn thumbnails_fit_ninety_six_logical_pixels_of_the_monitor() {
        assert_eq!(thumbnail_size(1920, 1080, 96), (96, 54));
        assert_eq!(thumbnail_size(1080, 1920, 96), (54, 96));
        assert_eq!(thumbnail_size(1920, 1080, 144), (144, 81), "150 %");
        assert_eq!(
            thumbnail_size(40, 30, 96),
            (40, 30),
            "small pictures keep their size"
        );
        assert_eq!(thumbnail_size(10_000, 1, 96), (96, 1), "never empty");
    }

    #[test]
    fn the_grab_band_finds_sides_and_corners_and_leaves_the_middle_to_moving() {
        let r = Rect::new(100, 100, 200, 100);
        let at = |x, y| edge_at(r, 13, 4, (x, y));
        assert_eq!(at(150, 150), None, "the middle moves the pin");
        assert_eq!(at(95, 150), Some(Edge::Left), "the shadow grabs");
        assert_eq!(at(102, 150), Some(Edge::Left), "so does the band inside");
        assert_eq!(at(299, 150), Some(Edge::Right));
        assert_eq!(at(200, 99), Some(Edge::Top));
        assert_eq!(at(200, 210), Some(Edge::Bottom));
        assert_eq!(at(90, 90), Some(Edge::TopLeft));
        assert_eq!(at(305, 205), Some(Edge::BottomRight));
        assert_eq!(
            at(100, 106),
            Some(Edge::TopLeft),
            "a corner reaches along the side"
        );
        assert_eq!(at(320, 150), None, "beyond the band");
        assert_eq!(at(150, 80), None);
        // A tiny pin keeps a middle to move it by; without a margin only the inside band grabs.
        let tiny = Rect::new(0, 0, 8, 8);
        assert_eq!(edge_at(tiny, 0, 4, (4, 4)), None);
        assert_eq!(edge_at(tiny, 0, 4, (0, 4)), Some(Edge::Left));
        assert_eq!(edge_at(tiny, 0, 0, (0, 4)), None, "no band, no resizing");
    }

    /// Dragging keeps the aspect ratio, the opposite side and the zoom limits; dragging back
    /// returns to the start exactly.
    #[test]
    fn resizing_keeps_the_aspect_ratio_and_the_opposite_side() {
        let start = Rect::new(100, 100, 200, 100);
        let base = (200, 100);
        assert_eq!(
            resize(start, base, Edge::Right, (100, 0)),
            (150, Rect::new(100, 100, 300, 150))
        );
        assert_eq!(
            resize(start, base, Edge::Left, (100, 0)),
            (50, Rect::new(200, 100, 100, 50)),
            "the right side stays"
        );
        assert_eq!(
            resize(start, base, Edge::Top, (0, -50)),
            (150, Rect::new(100, 50, 300, 150)),
            "the bottom stays"
        );
        assert_eq!(
            resize(start, base, Edge::BottomRight, (20, 60)),
            (160, Rect::new(100, 100, 320, 160)),
            "a corner follows the axis that changed more"
        );
        assert_eq!(
            resize(start, base, Edge::TopLeft, (0, 0)),
            (100, start),
            "no movement, no change"
        );
        assert_eq!(resize(start, base, Edge::Right, (-1000, 0)).0, zoom::MIN);
        assert_eq!(resize(start, base, Edge::Right, (100_000, 0)).0, zoom::MAX);
        // A 4K picture stops where its window would exceed the surface limits.
        let big = Rect::new(0, 0, 3840, 2160);
        let (z, r) = resize(big, (3840, 2160), Edge::BottomRight, (20_000, 20_000));
        assert!(zoom::allowed(r.width, r.height) && z < 300, "{z} {r:?}");
        // Rotated pins resize from their shown size.
        let tall = Rect::new(0, 0, 100, 200);
        assert_eq!(resize(tall, (100, 200), Edge::Bottom, (0, 200)).0, 200);
    }

    fn view(rect: Rect, dpi: u32) -> View {
        View {
            rect,
            dpi,
            ..View::plain(rect)
        }
    }

    /// The window shows the picture's own pixels at 100 % inside the outline and the shadow; the
    /// margin scales with the DPI and every margin pixel can be grabbed.
    #[test]
    fn the_surface_has_the_picture_inside_its_outline_and_shadow() {
        let ledger = Ledger::new();
        let f = pattern(&ledger, 6, 4);
        for dpi in [96, 144, 192] {
            let v = view(Rect::new(0, 0, 6, 4), dpi);
            let m = v.margin();
            assert_eq!(m, v.border() + scale_px(12, dpi));
            let win = v.window();
            assert_eq!((win.x, win.width), (-(m as i32), 6 + 2 * m));
            let mut buf = vec![0u8; (win.width * win.height * 4) as usize];
            paint(&f, &v, &mut buf);
            let px = |x: u32, y: u32| {
                let i = ((y * win.width + x) * 4) as usize;
                [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
            };
            for y in 0..4 {
                for x in 0..6 {
                    assert_eq!(
                        px(m + x, m + y),
                        at(&f, x, y),
                        "picture pixel ({x},{y}) at {dpi}"
                    );
                }
            }
            let b = v.border();
            assert_eq!(px(m - b, m + 1), BORDER_BGRA, "the outline");
            assert_eq!(px(m + 6, m + 1), BORDER_BGRA);
            assert!(px(0, 0)[3] >= 1, "the shadow's corner can be grabbed");
            let below = px(m + 3, m + 4 + b + 1)[3];
            let above = px(m + 3, m - b - 2)[3];
            assert!(
                below > above,
                "the shadow falls downwards: {below} > {above}"
            );
            assert!(below < 255 && px(m + 3, m + 4 + b + 1)[..3] == [0, 0, 0]);
        }
        // Without border and shadow the window is the picture.
        let mut bare = view(Rect::new(3, 4, 6, 4), 96);
        bare.look = Look {
            border: false,
            shadow: false,
        };
        assert_eq!((bare.margin(), bare.window()), (0, Rect::new(3, 4, 6, 4)));
        let mut buf = vec![0u8; 6 * 4 * 4];
        paint(&f, &bare, &mut buf);
        assert_eq!(&buf[..4], &at(&f, 0, 0));
        // A thumbnail has no grab band.
        bare.resizable = false;
        assert_eq!(bare.band(), (0, 0));
    }

    /// The click-through badge sits in the picture's top-right corner, visible on light and dark
    /// pictures, and scales with the DPI; it never leaves the picture of a tiny pin.
    #[test]
    fn the_click_through_badge_marks_the_corner() {
        let ledger = Ledger::new();
        for (bgra, dpi) in [([255, 255, 255, 255], 96), ([0, 0, 0, 255], 144)] {
            let f = Frame::solid(&ledger, Rect::new(0, 0, 60, 40), bgra).unwrap();
            let mut v = view(Rect::new(0, 0, 60, 40), dpi);
            v.look = Look {
                border: false,
                shadow: false,
            };
            v.click_through = true;
            let b = v.badge().unwrap();
            assert_eq!(b.width, scale_px(BADGE, dpi));
            assert_eq!(b.right(), i64::from(60 - scale_px(BADGE_INSET, dpi)));
            let mut buf = vec![0u8; 60 * 40 * 4];
            paint(&f, &v, &mut buf);
            let c = ((b.y as u32 + b.height / 2) * 60 + b.x as u32 + b.width / 2) as usize * 4;
            assert_eq!(&buf[c..c + 4], &[255, 255, 255, 255], "white centre");
            let ring = ((b.y as u32 + b.height / 2) * 60 + b.x as u32 + 3 * dpi / 96) as usize * 4;
            assert_ne!(
                &buf[ring..ring + 4],
                &bgra,
                "the accent ring differs from the picture"
            );
            assert_eq!(&buf[..4], &bgra, "the rest of the picture is untouched");
        }
        let mut v = view(Rect::new(0, 0, 5, 3), 96);
        v.click_through = true;
        let b = v.badge().unwrap();
        assert!(b.right() <= 5 && b.bottom() <= 3 && b.width == 3);
        v.click_through = false;
        assert_eq!(v.badge(), None);
    }
}
