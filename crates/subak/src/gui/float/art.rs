//! The floating and tray icon's picture, drawn from a small state (docs/SUBAK_GUI.md §11.3).
//!
//! **The state interface for Step 44.** The icon is a pure function of [`IconState`]: whether a
//! Host is reachable, how many Terminals it has and how many run or failed, and the aggregate
//! Claude indicator (`SUBAK-FR-051`; always [`Claude::None`] until the Host reports Claude states,
//! Steps 26–28). A new state redraws the icon once; nothing animates and no timer runs, so a
//! hidden icon costs nothing (the Step 44 addendum of issue #18). Step 44 owns the final watermelon
//! art and the per-state motion: it replaces [`render`]'s glyph and badge, not this interface.
//!
//! **The glyph** is Subak's identity, drawn procedurally until Step 44's assets exist: a
//! watermelon slice with a charcoal rind, a pale band and red flesh with dark seeds, on the
//! `bg.overlay` tile (40 × 40 logical pixels, radius 10, a 1 px `border.subtle` edge,
//! DESIGN_SYSTEM §3.4). The rind gets a light edge so it never reads black-on-black.
//!
//! **The badge** (10 logical pixels, bottom right) says the aggregate state in a shape, never in
//! colour alone (DESIGN_SYSTEM §3.8): a hollow ring when no Host runs, a ring with a gap while
//! connecting, a danger triangle when a Terminal failed, a Claude shape once there is one, and
//! nothing when all is well. The words are in [`tooltip`] (the tray tip, the icon's accessible
//! name).
//!
//! Pixels are premultiplied BGRA (`0xAARRGGBB` as a little-endian `u32`), what
//! `UpdateLayeredWindow` and `CreateIconIndirect` take; each pixel is 4 × 4 supersampled.

use subak_term::render::Rgb;

/// Whether the icon reaches a Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    /// Connecting (the first time, or a retry).
    Connecting,
    Connected,
    /// No Host serves the endpoint (nothing is started until the user asks for a Terminal).
    NoHost,
}

/// The aggregate Claude indicator (`SUBAK-FR-050`, `SUBAK-FR-051`), most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Claude {
    NeedInput,
    Error,
    Done,
    InProgress,
    Unknown,
    Idle,
    None,
}

impl Claude {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NeedInput => "need-input",
            Self::Error => "error",
            Self::Done => "done",
            Self::InProgress => "in-progress",
            Self::Unknown => "unknown",
            Self::Idle => "idle",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "need-input" => Self::NeedInput,
            "error" => Self::Error,
            "done" => Self::Done,
            "in-progress" => Self::InProgress,
            "unknown" => Self::Unknown,
            "idle" => Self::Idle,
            "none" => Self::None,
            _ => return None,
        })
    }
}

/// The aggregate of several Terminals' Claude states, by the priority of `SUBAK-FR-051`:
/// `need-input` > `error` > `done` > `in-progress` > `unknown` > `idle` > `none`.
pub fn aggregate(states: impl IntoIterator<Item = Claude>) -> Claude {
    states.into_iter().min().unwrap_or(Claude::None)
}

/// What the icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconState {
    pub host: HostState,
    /// Terminals the Host has (every state).
    pub terminals: usize,
    /// Of those, running.
    pub running: usize,
    /// Of those, failed (a start that failed and was kept).
    pub failed: usize,
    /// The aggregate Claude indicator.
    pub claude: Claude,
}

impl Default for IconState {
    fn default() -> Self {
        Self {
            host: HostState::Connecting,
            terminals: 0,
            running: 0,
            failed: 0,
            claude: Claude::None,
        }
    }
}

/// The badge an icon state shows (tests and the test hooks name it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    None,
    /// A hollow ring: no Host.
    NoHost,
    /// A ring with a gap: connecting.
    Connecting,
    /// A danger triangle: a Terminal failed.
    Failed,
    /// A Claude state's shape (Steps 26–28).
    Claude(Claude),
}

impl Badge {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NoHost => "no-host",
            Self::Connecting => "connecting",
            Self::Failed => "failed",
            Self::Claude(c) => c.as_str(),
        }
    }
}

/// The badge of a state: the Host first, then Claude (more urgent than a failure only for
/// `need-input` and `error`), then failed Terminals.
pub fn badge(s: &IconState) -> Badge {
    match s.host {
        HostState::NoHost => return Badge::NoHost,
        HostState::Connecting => return Badge::Connecting,
        HostState::Connected => {}
    }
    match s.claude {
        c @ (Claude::NeedInput | Claude::Error) => Badge::Claude(c),
        _ if s.failed > 0 => Badge::Failed,
        Claude::None | Claude::Idle => Badge::None,
        c => Badge::Claude(c),
    }
}

/// The icon's words: the tray tip and the floating icon's accessible name and tooltip, e.g.
/// "Subak — 3 Terminals, 2 running".
pub fn tooltip(s: &IconState) -> String {
    let base = match s.host {
        HostState::Connecting => return "Subak — connecting to the Host…".into(),
        HostState::NoHost => return "Subak — the Host is not running (no Terminals)".into(),
        HostState::Connected => {
            let n = s.terminals;
            let mut t = format!("Subak — {n} Terminal{}", if n == 1 { "" } else { "s" });
            if n > 0 {
                t.push_str(&format!(", {} running", s.running));
            }
            if s.failed > 0 {
                t.push_str(&format!(", {} failed", s.failed));
            }
            t
        }
    };
    match s.claude {
        Claude::None => base,
        Claude::NeedInput => format!("{base}, Claude needs input"),
        Claude::Error => format!("{base}, Claude error"),
        Claude::Done => format!("{base}, Claude done"),
        Claude::InProgress => format!("{base}, Claude working"),
        Claude::Unknown => format!("{base}, Claude state unknown"),
        Claude::Idle => format!("{base}, Claude idle"),
    }
}

/// The colours of the picture: the design tokens, or the system's under High Contrast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub tile: Rgb,
    pub edge: Rgb,
    pub rind: Rgb,
    pub rind_edge: Rgb,
    pub band: Rgb,
    pub flesh: Rgb,
    pub seed: Rgb,
    pub badge_ok: Rgb,
    pub badge_warn: Rgb,
    pub badge_danger: Rgb,
    pub badge_accent: Rgb,
    /// Under High Contrast the glyph is drawn as an outline in the text colour.
    pub outline_only: bool,
}

impl Palette {
    /// The Subak identity on the kim-dark tokens.
    pub fn subak() -> Self {
        use crate::gui::screens::tokens as t;
        Self {
            tile: t::BG_OVERLAY,
            edge: t::BORDER_SUBTLE,
            rind: Rgb(0x23, 0x22, 0x26),
            rind_edge: Rgb(0x6E, 0x6B, 0x7C),
            band: Rgb(0xF1, 0xE7, 0xD0),
            flesh: Rgb(0xE5, 0x38, 0x3B),
            seed: Rgb(0x16, 0x14, 0x18),
            badge_ok: t::SUCCESS,
            badge_warn: t::WARNING,
            badge_danger: t::DANGER,
            badge_accent: t::ACCENT,
            outline_only: false,
        }
    }

    /// High Contrast: the system's window and text colours, the highlight for the badge.
    pub fn high_contrast(window: Rgb, text: Rgb, highlight: Rgb) -> Self {
        Self {
            tile: window,
            edge: text,
            rind: text,
            rind_edge: text,
            band: window,
            flesh: window,
            seed: text,
            badge_ok: highlight,
            badge_warn: highlight,
            badge_danger: highlight,
            badge_accent: highlight,
            outline_only: true,
        }
    }
}

/// A premultiplied BGRA picture, row-major from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub size: u32,
    pub pixels: Vec<u32>,
}

impl Picture {
    /// A stable hash of the pixels (test reports).
    pub fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for p in &self.pixels {
            h ^= u64::from(*p);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    /// The pixel at `(x, y)` as straight (not premultiplied) `(r, g, b, a)`.
    pub fn rgba(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let p = self.pixels[(y * self.size + x) as usize];
        let a = (p >> 24) as u8;
        let un = |c: u32| {
            if a == 0 {
                0
            } else {
                ((c * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
            }
        };
        (un((p >> 16) & 0xFF), un((p >> 8) & 0xFF), un(p & 0xFF), a)
    }
}

/// Coverage-weighted colour accumulation of one pixel.
#[derive(Default, Clone, Copy)]
struct Acc {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Acc {
    /// Paints `c` with coverage `alpha` (0–1) over what is there (source-over).
    fn over(&mut self, c: Rgb, alpha: f32) {
        let k = 1.0 - alpha;
        self.r = f32::from(c.0) * alpha + self.r * k;
        self.g = f32::from(c.1) * alpha + self.g * k;
        self.b = f32::from(c.2) * alpha + self.b * k;
        self.a = alpha + self.a * k;
    }
}

/// Signed distance to a rounded square of half-size `h` and radius `r` centred at the origin.
fn rounded_box(x: f32, y: f32, h: f32, r: f32) -> f32 {
    let qx = x.abs() - (h - r);
    let qy = y.abs() - (h - r);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// What a sample point of the glyph is (in units of the 40 px tile, origin at the centre).
fn glyph_layer(x: f32, y: f32, outline: bool) -> Option<u8> {
    // The slice: a half-disc, flat edge on top, centred a little above the middle.
    let (cx, cy, r) = (0.0, -4.0, 14.0);
    let (dx, dy) = (x - cx, y - cy);
    let d = (dx * dx + dy * dy).sqrt();
    if dy < 0.0 || d > r {
        return None;
    }
    if outline {
        // High Contrast: the slice's outline and the seeds, 1.6 units wide.
        let edge = (r - d < 1.6) || dy < 1.6;
        return edge.then_some(1);
    }
    if d > r - 2.6 {
        return Some(if d > r - 0.9 { 0 } else { 1 }); // rind edge, rind
    }
    if d > r - 4.0 {
        return Some(2); // pale band
    }
    // Seeds: small dark drops in the flesh.
    for (sx, sy) in [(-5.0, 3.0), (0.0, 6.0), (5.0, 3.0), (-2.5, 8.5), (2.5, 8.5)] {
        let (ex, ey) = (x - sx, y - cy - sy);
        if (ex * ex) / (0.9 * 0.9) + (ey * ey) / (1.5 * 1.5) <= 1.0 {
            return Some(4);
        }
    }
    Some(3) // flesh
}

/// The badge's centre and radius in tile units (10 across, at the bottom right, inside the
/// tile's rounded corner).
const BADGE: (f32, f32, f32) = (11.5, 11.5, 5.0);

/// What a sample point of the badge is: `Some(filled)` within the badge's shape.
fn badge_hit(b: Badge, x: f32, y: f32) -> Option<bool> {
    let (cx, cy, r) = BADGE;
    let (dx, dy) = (x - cx, y - cy);
    let d = (dx * dx + dy * dy).sqrt();
    match b {
        Badge::None => None,
        Badge::NoHost => (d <= r && d >= r - 1.8).then_some(false),
        Badge::Connecting => {
            // A ring with a gap at the top right.
            (d <= r && d >= r - 1.8 && !(dx > 0.0 && dy < 0.0)).then_some(false)
        }
        Badge::Failed => {
            // A triangle pointing up.
            let t = (dy + r) / (2.0 * r);
            (dy.abs() <= r && dx.abs() <= t * r).then_some(true)
        }
        Badge::Claude(Claude::NeedInput) => (d <= r).then_some(true),
        Badge::Claude(Claude::Error) => {
            // A square.
            (dx.abs() <= r * 0.8 && dy.abs() <= r * 0.8).then_some(true)
        }
        Badge::Claude(Claude::Done) => (d <= r && (d <= r * 0.45 || d >= r - 1.6)).then_some(true),
        Badge::Claude(Claude::Unknown) => (d <= r && d >= r - 1.6).then_some(false),
        Badge::Claude(_) => {
            // in-progress (and idle, never badged): a half-filled disc.
            (d <= r && (dx <= 0.0 || d >= r - 1.6)).then_some(true)
        }
    }
}

fn badge_colour(b: Badge, p: &Palette) -> Rgb {
    match b {
        Badge::NoHost | Badge::Connecting | Badge::Claude(Claude::Unknown) => p.badge_warn,
        Badge::Failed | Badge::Claude(Claude::Error) => p.badge_danger,
        Badge::Claude(Claude::Done) => p.badge_ok,
        _ => p.badge_accent,
    }
}

/// The icon at `size × size` device pixels (40 logical pixels at the DPI; 16–48 for the tray).
pub fn render(state: &IconState, size: u32, palette: &Palette) -> Picture {
    let size = size.clamp(8, 512);
    let b = badge(state);
    let unit = 40.0 / size as f32;
    let mut pixels = Vec::with_capacity((size * size) as usize);
    const N: u32 = 4;
    for py in 0..size {
        for px in 0..size {
            let mut sum = Acc::default();
            for sy in 0..N {
                for sx in 0..N {
                    // Sample position in tile units, origin at the centre.
                    let x = (px as f32 + (sx as f32 + 0.5) / N as f32) * unit - 20.0;
                    let y = (py as f32 + (sy as f32 + 0.5) / N as f32) * unit - 20.0;
                    let tile = rounded_box(x, y, 20.0, 10.0);
                    if tile > 0.0 {
                        continue; // outside the rounded tile: transparent
                    }
                    let mut acc = Acc::default();
                    // A one-device-pixel edge (`unit` tile units), then the tile.
                    acc.over(
                        if tile > -unit {
                            palette.edge
                        } else {
                            palette.tile
                        },
                        1.0,
                    );
                    if let Some(layer) = glyph_layer(x, y, palette.outline_only) {
                        let c = match layer {
                            0 => palette.rind_edge,
                            1 => palette.rind,
                            2 => palette.band,
                            3 => palette.flesh,
                            _ => palette.seed,
                        };
                        acc.over(c, 1.0);
                    }
                    if badge_hit(b, x, y).is_some() {
                        acc.over(badge_colour(b, palette), 1.0);
                    } else if b != Badge::None {
                        // A tile-coloured halo keeps the badge apart from the glyph.
                        let (dx, dy) = (x - BADGE.0, y - BADGE.1);
                        if (dx * dx + dy * dy).sqrt() <= BADGE.2 + 1.6 {
                            acc.over(palette.tile, 1.0);
                        }
                    }
                    sum.r += acc.r;
                    sum.g += acc.g;
                    sum.b += acc.b;
                    sum.a += acc.a;
                }
            }
            let n = (N * N) as f32;
            // `acc` channels are straight colour × coverage of each sample (coverage 0 or 1),
            // so the averages are premultiplied colour and alpha.
            let a = (sum.a / n * 255.0).round().clamp(0.0, 255.0) as u32;
            let ch = |v: f32| (v / n).round().clamp(0.0, 255.0) as u32;
            let (r, g, bl) = (ch(sum.r).min(a), ch(sum.g).min(a), ch(sum.b).min(a));
            pixels.push((a << 24) | (r << 16) | (g << 8) | bl);
        }
    }
    Picture { size, pixels }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(host: HostState, terminals: usize, running: usize, failed: usize) -> IconState {
        IconState {
            host,
            terminals,
            running,
            failed,
            claude: Claude::None,
        }
    }

    #[test]
    fn claude_states_aggregate_by_urgency() {
        use Claude::*;
        assert_eq!(aggregate([Idle, InProgress, Done]), Done);
        assert_eq!(aggregate([Idle, NeedInput, Error]), NeedInput);
        assert_eq!(aggregate([None, Unknown, Idle]), Unknown);
        assert_eq!(aggregate([None, Idle]), Idle);
        assert_eq!(aggregate([]), None);
        for c in [NeedInput, Error, Done, InProgress, Unknown, Idle, None] {
            assert_eq!(Claude::parse(c.as_str()), Some(c));
        }
    }

    #[test]
    fn badges_and_words_say_the_state_never_in_colour_alone() {
        let ok = st(HostState::Connected, 3, 2, 0);
        assert_eq!(badge(&ok), Badge::None);
        assert_eq!(tooltip(&ok), "Subak — 3 Terminals, 2 running");
        assert_eq!(
            tooltip(&st(HostState::Connected, 1, 1, 0)),
            "Subak — 1 Terminal, 1 running"
        );
        assert_eq!(
            tooltip(&st(HostState::Connected, 0, 0, 0)),
            "Subak — 0 Terminals"
        );
        let none = st(HostState::NoHost, 0, 0, 0);
        assert_eq!(badge(&none), Badge::NoHost);
        assert!(tooltip(&none).contains("not running"));
        assert_eq!(badge(&IconState::default()), Badge::Connecting);
        let failed = st(HostState::Connected, 2, 1, 1);
        assert_eq!(badge(&failed), Badge::Failed);
        assert!(tooltip(&failed).ends_with("1 failed"));
        let mut c = ok;
        c.claude = Claude::NeedInput;
        assert_eq!(badge(&c), Badge::Claude(Claude::NeedInput));
        assert!(tooltip(&c).ends_with("Claude needs input"));
        c.claude = Claude::Done;
        c.failed = 1;
        assert_eq!(
            badge(&c),
            Badge::Failed,
            "a failure outranks an unread done"
        );
        c.claude = Claude::Idle;
        c.failed = 0;
        assert_eq!(badge(&c), Badge::None);
    }

    #[test]
    fn the_picture_is_a_rounded_tile_with_a_red_watermelon_and_a_badge() {
        let p = render(&st(HostState::Connected, 1, 1, 0), 40, &Palette::subak());
        assert_eq!(p.pixels.len(), 1600);
        // Corners are transparent (radius 10), the middle of the edges is the border.
        assert_eq!(p.rgba(0, 0).3, 0);
        assert_eq!(p.rgba(39, 39).3, 0);
        assert_eq!(p.rgba(20, 0), (0x26, 0x25, 0x2F, 255));
        // Tile colour away from the glyph.
        assert_eq!(p.rgba(4, 20), (0x1C, 0x1B, 0x24, 255));
        // Red flesh in the slice, the charcoal rind at its bottom, a pale band in between.
        let (r, g, b, a) = p.rgba(20, 19);
        assert!(
            a == 255 && r > 200 && g < 80 && b < 80,
            "flesh {:?}",
            (r, g, b)
        );
        let rind = p.rgba(20, 28);
        assert!(rind.0 < 0x70 && rind.1 < 0x70, "rind {rind:?}");
        let band = p.rgba(20, 26);
        assert!(band.0 > 0xC0 && band.1 > 0xC0, "band {band:?}");
        // No badge when all is well; a badge for no Host, at the bottom right.
        assert_eq!(p.rgba(33, 33), (0x1C, 0x1B, 0x24, 255));
        let q = render(&st(HostState::NoHost, 0, 0, 0), 40, &Palette::subak());
        assert_ne!(p.hash(), q.hash());
        let ring = q.rgba(35, 31);
        assert_eq!(ring.3, 255);
        assert!(ring.0 > 0xC0 && ring.1 > 0x90, "warning ring {ring:?}");
    }

    #[test]
    fn every_size_and_high_contrast_render_and_differ_by_state() {
        for size in [16, 20, 24, 32, 40, 48, 60, 80, 120] {
            let p = render(&IconState::default(), size, &Palette::subak());
            assert_eq!(p.pixels.len(), (size * size) as usize);
            // Premultiplied: no channel exceeds alpha.
            for px in &p.pixels {
                let a = px >> 24;
                assert!((px >> 16 & 0xFF) <= a && (px >> 8 & 0xFF) <= a && (px & 0xFF) <= a);
            }
        }
        let hc = Palette::high_contrast(Rgb(0, 0, 0), Rgb(255, 255, 255), Rgb(0, 255, 255));
        let p = render(&st(HostState::Connected, 1, 1, 0), 40, &hc);
        // The slice is an outline in the text colour; no red anywhere.
        assert!(p.pixels.iter().all(|px| {
            let (r, g, b) = ((px >> 16) & 0xFF, (px >> 8) & 0xFF, px & 0xFF);
            !(r > 150 && g < 80 && b < 80)
        }));
        let states = [
            st(HostState::Connected, 1, 1, 0),
            st(HostState::NoHost, 0, 0, 0),
            st(HostState::Connecting, 0, 0, 0),
            st(HostState::Connected, 2, 1, 1),
        ];
        let hashes: std::collections::HashSet<u64> = states
            .iter()
            .map(|s| render(s, 40, &Palette::subak()).hash())
            .collect();
        assert_eq!(hashes.len(), states.len(), "each badge is its own picture");
    }
}
