//! The OS capture boundary (ADR 0011, ADR 0023 §2, NOEUL-NFR-011).
//!
//! Everything Noeul knows about the screen goes through [`CaptureBackend`]: whether an
//! interactive desktop of this user is available right now, where the monitors are, and a
//! picture of a rectangle of it. Windows uses GDI (`BitBlt` from the screen into a DIB section,
//! ADR 0023); other platforms get [`Unsupported`], which reports `unsupported_platform` and never
//! pretends to capture (NOEUL-FR-112). Linux backends follow in N7.
//!
//! A capture happens only when a caller asks for one (NOEUL-SEC-001), re-checks the desktop
//! first (NOEUL_UX_SPEC §7), and returns the picture in memory only (NOEUL-SEC-005). Locked,
//! secure, disconnected, non-interactive and display-less sessions fail with
//! `desktop_unavailable`: Noeul never reaches into another session or desktop, and never invents
//! a picture where the OS provides none (NOEUL-SEC-003).
//!
//! Since Step 58 (ADR 0050) a monitor also says how it is turned and whether it shows HDR, a
//! capture can have the mouse pointer drawn into it ([`CaptureBackend::capture_with_pointer`],
//! NOEUL-FR-016), and the parts of a picture that no monitor shows are opaque black
//! ([`crate::displays::fill_gaps`]). GDI stays the only backend: the comparison with DXGI Desktop
//! Duplication, Windows.Graphics.Capture and `PrintWindow`, and what each does with protected
//! windows, HDR and colour profiles, is in ADR 0050.

pub use crate::displays::PointerOutcome;
use crate::error::{Code, NoeulError};
use crate::frame::{Frame, FrameError, Ledger, Rect};
use serde_json::{Value, json};
use std::sync::Arc;

/// Whether this process can see the user's interactive desktop right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopState {
    /// Unlocked, interactive, with at least one display.
    Available,
    /// The input desktop is not the user's: the lock screen, sign-in, UAC or Ctrl+Alt+Del.
    Locked,
    /// A remote session whose client is disconnected.
    Disconnected,
    /// No interactive window station (a service, an SSH session, a scheduled task).
    NonInteractive,
    /// No monitor is attached.
    NoDisplay,
    /// This platform has no capture backend yet.
    Unsupported,
}

impl DesktopState {
    pub fn as_str(self) -> &'static str {
        match self {
            DesktopState::Available => "available",
            DesktopState::Locked => "locked",
            DesktopState::Disconnected => "disconnected",
            DesktopState::NonInteractive => "non_interactive",
            DesktopState::NoDisplay => "no_display",
            DesktopState::Unsupported => "unsupported_platform",
        }
    }

    fn sentence(self) -> &'static str {
        match self {
            DesktopState::Available => "the desktop is available",
            DesktopState::Locked => {
                "the session is locked or a secure desktop (sign-in, UAC, Ctrl+Alt+Del) is shown"
            }
            DesktopState::Disconnected => "this remote session is disconnected",
            DesktopState::NonInteractive => {
                "this process has no interactive desktop (a service, SSH or scheduled task)"
            }
            DesktopState::NoDisplay => "no display is attached",
            DesktopState::Unsupported => "screen capture is not supported on this platform yet",
        }
    }

    /// The error a desktop command reports in this state (`None` when available).
    pub fn error(self) -> Option<NoeulError> {
        match self {
            DesktopState::Available => None,
            DesktopState::Unsupported => Some(NoeulError::unsupported_platform("screen capture")),
            s => Some(
                NoeulError::new(
                    Code::DesktopUnavailable,
                    format!("no interactive desktop is available: {}", s.sentence()),
                )
                .hint("unlock the session or reconnect, then try again; Noeul never captures a locked, secure or other user's desktop")
                .detail(format!("desktop state: {}", s.as_str())),
            ),
        }
    }
}

/// One monitor in virtual-desktop device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub rect: Rect,
    pub primary: bool,
    /// Effective DPI (96 = 100 %, 144 = 150 %). Noeul is per-monitor DPI aware (v2), so every
    /// rectangle is in device pixels and this only scales Noeul's own drawing (NOEUL-FR-090).
    pub dpi: u32,
    /// The work area: the monitor without the taskbar and docked app bars (`rcWork`). Pins are
    /// placed and brought back inside it (Step 53, ADR 0029).
    pub work: Rect,
    /// How the monitor is turned, in degrees clockwise (0, 90, 180, 270; Step 58). `rect` is
    /// already the turned size, and GDI delivers the picture upright, so nothing in Noeul turns
    /// pixels; it is reported so a turned layout can be recognised (ADR 0050).
    pub rotation: u32,
    /// Whether the monitor shows HDR (Windows "advanced colour"); `None` when unknown. A capture
    /// of an HDR monitor is converted to SDR by Windows and says so (NOEUL-FR-092, Step 58).
    pub hdr: Option<bool>,
}

/// The DPI of a 100 % monitor.
pub const BASE_DPI: u32 = 96;

impl Monitor {
    /// A monitor at 100 %.
    pub const fn new(rect: Rect, primary: bool) -> Monitor {
        Monitor {
            rect,
            primary,
            dpi: BASE_DPI,
            work: rect,
            rotation: 0,
            hdr: None,
        }
    }

    /// The same monitor turned by `degrees` clockwise (`rect` must already be the turned size).
    #[must_use]
    pub const fn with_rotation(mut self, degrees: u32) -> Monitor {
        self.rotation = degrees;
        self
    }

    #[must_use]
    pub const fn with_hdr(mut self, hdr: Option<bool>) -> Monitor {
        self.hdr = hdr;
        self
    }

    /// The same monitor with a work area (e.g. without a taskbar at its bottom).
    #[must_use]
    pub const fn with_work(mut self, work: Rect) -> Monitor {
        self.work = work;
        self
    }

    #[must_use]
    pub const fn with_dpi(mut self, dpi: u32) -> Monitor {
        self.dpi = dpi;
        self
    }

    /// Device pixels for `logical` pixels of Noeul's drawing on this monitor (at least 1).
    pub fn px(&self, logical: u32) -> u32 {
        scale_px(logical, self.dpi)
    }
}

/// `logical` pixels at `dpi`, rounded to the nearest device pixel and at least 1.
pub fn scale_px(logical: u32, dpi: u32) -> u32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    ((u64::from(logical) * u64::from(dpi) + u64::from(BASE_DPI) / 2) / u64::from(BASE_DPI))
        .max(1)
        .min(u64::from(u32::MAX)) as u32
}

/// What the backend sees now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desktop {
    pub state: DesktopState,
    /// The bounding rectangle of all monitors (`None` without a display).
    pub virtual_rect: Option<Rect>,
    pub monitors: Vec<Monitor>,
}

impl Desktop {
    pub fn unsupported() -> Desktop {
        Desktop {
            state: DesktopState::Unsupported,
            virtual_rect: None,
            monitors: Vec::new(),
        }
    }

    /// Geometry only (NOEUL-SEC-007): no window, program or content information.
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "virtual_rect": self.virtual_rect.map(|r| r.to_json()),
            "monitors": self.monitors.iter().map(|m| json!({
                "rect": m.rect.to_json(), "primary": m.primary, "dpi": m.dpi,
                "work": m.work.to_json(), "rotation": m.rotation, "hdr": m.hdr,
            })).collect::<Vec<_>>(),
        })
    }

    /// The monitor that contains the point, else the one nearest to it (`None` without a
    /// monitor).
    pub fn monitor_near(&self, x: i32, y: i32) -> Option<&Monitor> {
        let dist = |m: &Monitor| {
            let r = m.rect;
            let dx = if i64::from(x) < i64::from(r.x) {
                i64::from(r.x) - i64::from(x)
            } else if i64::from(x) >= r.right() {
                i64::from(x) - r.right() + 1
            } else {
                0
            };
            let dy = if i64::from(y) < i64::from(r.y) {
                i64::from(r.y) - i64::from(y)
            } else if i64::from(y) >= r.bottom() {
                i64::from(y) - r.bottom() + 1
            } else {
                0
            };
            dx * dx + dy * dy
        };
        self.monitors.iter().min_by_key(|m| dist(m))
    }

    /// Whether any part of `r` lies on a monitor.
    pub fn shows(&self, r: Rect) -> bool {
        self.monitors.iter().any(|m| m.rect.intersects(&r))
    }
}

/// Why a capture failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The desktop is not available (see [`DesktopState`]).
    Desktop(DesktopState),
    /// The region is not inside the virtual desktop.
    Outside { region: Rect, desktop: Option<Rect> },
    /// The region is larger than a picture may be.
    TooLarge,
    /// The OS refused (GDI call failed); `what` names the call, never content.
    Os { what: &'static str, code: u32 },
}

impl From<FrameError> for CaptureError {
    fn from(_: FrameError) -> CaptureError {
        CaptureError::TooLarge
    }
}

impl From<CaptureError> for NoeulError {
    fn from(e: CaptureError) -> NoeulError {
        match e {
            CaptureError::Desktop(s) => s.error().unwrap_or_else(|| {
                NoeulError::new(
                    Code::Internal,
                    "the desktop became available during the check",
                )
            }),
            CaptureError::Outside { region, desktop } => {
                let mut err = NoeulError::new(
                    Code::Usage,
                    format!("the region {region} is not inside the desktop"),
                );
                if let Some(d) = desktop {
                    err = err.hint(format!(
                        "the virtual desktop is {d} (x,y,width,height in device pixels)"
                    ));
                }
                err
            }
            CaptureError::TooLarge => {
                NoeulError::new(Code::Usage, FrameError::TooLarge.to_string())
            }
            CaptureError::Os { what, code } => NoeulError::new(
                Code::Internal,
                format!("screen capture failed: {what} (error {code})"),
            ),
        }
    }
}

/// A screen capture backend. Implementations hold no picture and no device context between
/// calls, so an idle Noeul owns no capture memory (NOEUL-NFR-002).
pub trait CaptureBackend: Send + Sync {
    /// `gdi` on Windows (ADR 0023), `unsupported` elsewhere.
    fn name(&self) -> &'static str;

    /// The current desktop state and monitor geometry.
    fn desktop(&self) -> Desktop;

    /// A picture of `region` of the virtual desktop, after checking the desktop again. Parts no
    /// monitor shows are opaque black.
    fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError>;

    /// The same picture with the mouse pointer drawn where it is now (NOEUL-FR-016, Step 58).
    /// A backend that cannot read the pointer captures without it and says so.
    fn capture_with_pointer(
        &self,
        ledger: &Arc<Ledger>,
        region: Rect,
    ) -> Result<(Frame, PointerOutcome), CaptureError> {
        self.capture(ledger, region)
            .map(|f| (f, PointerOutcome::Unavailable))
    }

    /// The displays changed (the instance's debounced display-change handling, Step 58): a
    /// backend that keeps anything about them reads it again. GDI keeps nothing.
    fn refresh(&self) {}
}

/// Checks `region` against `desktop` the way every backend must before capturing.
pub fn check_region(desktop: &Desktop, region: Rect) -> Result<(), CaptureError> {
    if desktop.state != DesktopState::Available {
        return Err(CaptureError::Desktop(desktop.state));
    }
    crate::frame::check_size(region.width, region.height)?;
    let inside = desktop.virtual_rect.is_some_and(|v| v.contains(&region))
        && desktop.monitors.iter().any(|m| m.rect.intersects(&region));
    if !inside {
        return Err(CaptureError::Outside {
            region,
            desktop: desktop.virtual_rect,
        });
    }
    Ok(())
}

/// The backend of a platform without one (ADR 0011).
#[derive(Debug, Default)]
pub struct Unsupported;

impl CaptureBackend for Unsupported {
    fn name(&self) -> &'static str {
        "unsupported"
    }

    fn desktop(&self) -> Desktop {
        Desktop::unsupported()
    }

    fn capture(&self, _: &Arc<Ledger>, _: Rect) -> Result<Frame, CaptureError> {
        Err(CaptureError::Desktop(DesktopState::Unsupported))
    }
}

/// This platform's backend.
pub fn system() -> Box<dyn CaptureBackend> {
    #[cfg(windows)]
    {
        Box::new(crate::win::gdi::Gdi)
    }
    #[cfg(not(windows))]
    {
        Box::new(Unsupported)
    }
}

/// A made-up desktop that "captures" a generated test pattern: synthetic monitor layouts
/// (several monitors, mixed DPI, negative coordinates, turned monitors, gaps) for headless tests,
/// and the debug-build test hook of `noeul run` (ADR 0026), which places it far off-screen so
/// nothing Noeul draws from it is visible. It never reads the real screen. Its pointer is a
/// position a test sets and a fixed sprite ([`crate::displays::POINTER_SPRITE`]). Since Step 58
/// the test hook can also read its layout from a file and read it again on every display change
/// ([`Synthetic::with_layout_file`]), so tests attach, remove, rescale and turn monitors of a
/// running instance without touching the real displays.
#[derive(Debug)]
pub struct Synthetic {
    monitors: std::sync::Mutex<Vec<Monitor>>,
    state: std::sync::Mutex<DesktopState>,
    captures: std::sync::atomic::AtomicU64,
    pointer: std::sync::Mutex<Option<(i32, i32)>>,
    layout_file: Option<std::path::PathBuf>,
    /// The real virtual desktop: a layout file that would touch it is refused.
    avoid: Option<Rect>,
}

/// How far from [`OFF_SCREEN`] a layout file may reach (device pixels, each side): every
/// synthetic monitor lies between -29 000 and -11 000 on both axes, far from real monitors.
pub const LAYOUT_SPAN: i64 = 9_000;

/// A synthetic layout: monitors and the pointer.
pub type Layout = (Vec<Monitor>, Option<(i32, i32)>);

impl Synthetic {
    pub fn new(monitors: Vec<Monitor>) -> Synthetic {
        Synthetic {
            monitors: std::sync::Mutex::new(monitors),
            state: std::sync::Mutex::new(DesktopState::Available),
            captures: std::sync::atomic::AtomicU64::new(0),
            pointer: std::sync::Mutex::new(None),
            layout_file: None,
            avoid: None,
        }
    }

    /// The off-screen test desktop whose layout comes from `path` (the debug-build test hook's
    /// `NOEUL_DEV_SYNTHETIC_LAYOUT`): read now and on every [`CaptureBackend::refresh`]. A file
    /// that is missing or invalid, or whose monitors would touch `avoid` (the real virtual
    /// desktop), leaves the layout as it was.
    pub fn with_layout_file(path: std::path::PathBuf, avoid: Option<Rect>) -> Synthetic {
        let mut s = Synthetic::off_screen();
        s.layout_file = Some(path);
        s.avoid = avoid;
        s.reload();
        s
    }

    /// Reads the layout file again (see [`Synthetic::with_layout_file`]); whether it applied.
    pub fn reload(&self) -> bool {
        let Some(path) = &self.layout_file else {
            return false;
        };
        let Some(layout) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| Synthetic::parse_layout(&v).ok())
        else {
            return false;
        };
        if let Some(real) = self.avoid
            && layout.0.iter().any(|m| m.rect.intersects(&real))
        {
            return false;
        }
        self.set_monitors(layout.0);
        self.set_pointer(layout.1);
        true
    }

    /// A layout document: `{"monitors": [{"x", "y", "width", "height", "primary", "dpi",
    /// "rotation", "hdr", "work": {"x", "y", "width", "height"}}], "pointer": {"x", "y"} | null}`,
    /// every rectangle relative to [`OFF_SCREEN`] and within [`LAYOUT_SPAN`] of it on each side.
    pub fn parse_layout(v: &Value) -> Result<Layout, String> {
        let (ox, oy) = (i64::from(OFF_SCREEN.0), i64::from(OFF_SCREEN.1));
        let place = |r: &Value, what: &str| -> Result<Rect, String> {
            let n = |k: &str| {
                r.get(k)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| format!("{what}: `{k}` must be a whole number"))
            };
            let (x, y, w, h) = (n("x")?, n("y")?, n("width")?, n("height")?);
            let fits = |p: i64, len: i64| {
                len >= 1 && p >= -LAYOUT_SPAN && p.saturating_add(len) <= LAYOUT_SPAN
            };
            if !fits(x, w) || !fits(y, h) {
                return Err(format!("{what} lies too far from the off-screen origin"));
            }
            let int = |v: i64| i32::try_from(v).map_err(|_| format!("{what} is out of range"));
            let side = |v: i64| u32::try_from(v).map_err(|_| format!("{what} is out of range"));
            Ok(Rect::new(int(ox + x)?, int(oy + y)?, side(w)?, side(h)?))
        };
        let mut monitors = Vec::new();
        for (i, m) in v["monitors"]
            .as_array()
            .ok_or("`monitors` must be a list")?
            .iter()
            .enumerate()
        {
            let what = format!("monitor {}", i + 1);
            let rect = place(m, &what)?;
            let work = match m.get("work") {
                None | Some(Value::Null) => rect,
                Some(w) => place(w, &what)?,
            };
            let dpi = m.get("dpi").and_then(Value::as_u64).unwrap_or(96);
            let rotation = m.get("rotation").and_then(Value::as_u64).unwrap_or(0);
            if !(96..=960).contains(&dpi) || ![0, 90, 180, 270].contains(&rotation) {
                return Err(format!("{what}: dpi 96–960, rotation 0, 90, 180 or 270"));
            }
            monitors.push(Monitor {
                rect,
                primary: m.get("primary").and_then(Value::as_bool).unwrap_or(false),
                dpi: u32::try_from(dpi).unwrap_or(BASE_DPI),
                work,
                rotation: u32::try_from(rotation).unwrap_or(0),
                hdr: m.get("hdr").and_then(Value::as_bool),
            });
        }
        let pointer = match v.get("pointer") {
            None | Some(Value::Null) => None,
            Some(p) => {
                let r = place(
                    &json!({"x": p["x"], "y": p["y"], "width": 1, "height": 1}),
                    "the pointer",
                )?;
                Some((r.x, r.y))
            }
        };
        Ok((monitors, pointer))
    }

    /// Where the synthetic pointer's hotspot is (`None`: hidden).
    pub fn set_pointer(&self, at: Option<(i32, i32)>) {
        *self
            .pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = at;
    }

    pub fn pointer(&self) -> Option<(i32, i32)> {
        *self
            .pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Two monitors far off-screen: a primary 640×400 at 100 % with a 32-pixel taskbar at its
    /// bottom, and a 480×300 one at 150 % to its right (the debug-build test hook, ADR 0026).
    pub fn off_screen() -> Synthetic {
        let (x, y) = OFF_SCREEN;
        Synthetic::new(vec![
            Monitor::new(Rect::new(x, y, 640, 400), true).with_work(Rect::new(x, y, 640, 368)),
            Monitor::new(Rect::new(x + 640, y, 480, 300), false).with_dpi(144),
        ])
    }

    /// Pretends the session was locked (or unlocked again).
    pub fn set_state(&self, s: DesktopState) {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = s;
    }

    /// Changes the monitor layout, as attaching, removing or rearranging monitors does (the
    /// runtime then reports a display change to the core).
    pub fn set_monitors(&self, monitors: Vec<Monitor>) {
        *self
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = monitors;
    }

    /// How many pictures were taken.
    pub fn captures(&self) -> u64 {
        self.captures.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The pattern's pixel at an absolute position (BGRA).
    pub fn pixel(x: i32, y: i32) -> [u8; 4] {
        [x as u8, y as u8, ((x >> 8) ^ (y >> 8)) as u8 | 0x40, 255]
    }
}

/// Where the debug-build test desktop lies: far outside any real monitor arrangement.
pub const OFF_SCREEN: (i32, i32) = (-20_000, -20_000);

impl CaptureBackend for Synthetic {
    fn name(&self) -> &'static str {
        "synthetic"
    }

    fn desktop(&self) -> Desktop {
        let state = *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let monitors = self
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if state != DesktopState::Available || monitors.is_empty() {
            return Desktop {
                state: if state == DesktopState::Available {
                    DesktopState::NoDisplay
                } else {
                    state
                },
                virtual_rect: None,
                monitors: Vec::new(),
            };
        }
        let x0 = monitors.iter().map(|m| m.rect.x).min().unwrap_or(0);
        let y0 = monitors.iter().map(|m| m.rect.y).min().unwrap_or(0);
        let x1 = monitors.iter().map(|m| m.rect.right()).max().unwrap_or(0);
        let y1 = monitors.iter().map(|m| m.rect.bottom()).max().unwrap_or(0);
        Desktop {
            state,
            virtual_rect: Some(Rect::new(
                x0,
                y0,
                u32::try_from(x1 - i64::from(x0)).unwrap_or(0),
                u32::try_from(y1 - i64::from(y0)).unwrap_or(0),
            )),
            monitors,
        }
    }

    fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError> {
        let desktop = self.desktop();
        check_region(&desktop, region)?;
        let mut pixels = Vec::with_capacity(region.bgra_bytes() as usize);
        for y in 0..region.height {
            let ay = region.y + i32::try_from(y).unwrap_or(i32::MAX);
            for x in 0..region.width {
                let ax = region.x + i32::try_from(x).unwrap_or(i32::MAX);
                pixels.extend_from_slice(&Synthetic::pixel(ax, ay));
            }
        }
        self.captures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut frame = Frame::new(ledger, region, pixels)?;
        crate::displays::fill_gaps(&mut frame, &desktop.monitors);
        Ok(frame)
    }

    fn capture_with_pointer(
        &self,
        ledger: &Arc<Ledger>,
        region: Rect,
    ) -> Result<(Frame, PointerOutcome), CaptureError> {
        let mut frame = self.capture(ledger, region)?;
        let outcome = match self.pointer() {
            Some(at) => crate::displays::draw_pointer_sprite(&mut frame, at),
            None => PointerOutcome::Hidden,
        };
        Ok((frame, outcome))
    }

    fn refresh(&self) {
        self.reload();
    }
}

impl CaptureBackend for Arc<Synthetic> {
    fn name(&self) -> &'static str {
        Synthetic::name(self)
    }

    fn desktop(&self) -> Desktop {
        Synthetic::desktop(self)
    }

    fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError> {
        Synthetic::capture(self, ledger, region)
    }

    fn capture_with_pointer(
        &self,
        ledger: &Arc<Ledger>,
        region: Rect,
    ) -> Result<(Frame, PointerOutcome), CaptureError> {
        Synthetic::capture_with_pointer(self, ledger, region)
    }

    fn refresh(&self) {
        Synthetic::reload(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_synthetic_desktop_spans_its_monitors_and_counts_captures() {
        let s = Synthetic::off_screen();
        let d = s.desktop();
        assert_eq!(d.state, DesktopState::Available);
        assert_eq!(
            d.virtual_rect,
            Some(Rect::new(OFF_SCREEN.0, OFF_SCREEN.1, 1120, 400))
        );
        assert_eq!(d.monitors[1].dpi, 144);
        assert_eq!(d.monitors[1].px(9), 14, "9 logical px at 150 %");
        let ledger = Ledger::new();
        let f = s
            .capture(&ledger, Rect::new(OFF_SCREEN.0 + 1, OFF_SCREEN.1 + 2, 2, 1))
            .unwrap();
        assert_eq!(
            &f.pixels()[..4],
            &Synthetic::pixel(OFF_SCREEN.0 + 1, OFF_SCREEN.1 + 2)
        );
        assert_eq!(s.captures(), 1);
        s.set_state(DesktopState::Locked);
        assert_eq!(
            s.capture(&ledger, Rect::new(OFF_SCREEN.0, OFF_SCREEN.1, 1, 1))
                .unwrap_err(),
            CaptureError::Desktop(DesktopState::Locked)
        );
        assert_eq!(s.captures(), 1, "nothing is taken on a locked desktop");
        assert_eq!(scale_px(7, 96), 7);
        assert_eq!(scale_px(7, 120), 9);
        assert_eq!(scale_px(1, 0), 1);
    }

    fn desk() -> Desktop {
        Desktop {
            state: DesktopState::Available,
            virtual_rect: Some(Rect::new(-1920, 0, 3840, 1080)),
            monitors: vec![
                Monitor::new(Rect::new(-1920, 0, 1920, 1080), false),
                Monitor::new(Rect::new(0, 0, 1920, 1080), true).with_dpi(144),
            ],
        }
    }

    #[test]
    fn regions_must_lie_on_the_desktop() {
        assert_eq!(check_region(&desk(), Rect::new(-100, 10, 200, 20)), Ok(()));
        assert!(matches!(
            check_region(&desk(), Rect::new(1900, 0, 40, 10)),
            Err(CaptureError::Outside { .. })
        ));
        let mut locked = desk();
        locked.state = DesktopState::Locked;
        assert_eq!(
            check_region(&locked, Rect::new(0, 0, 1, 1)),
            Err(CaptureError::Desktop(DesktopState::Locked))
        );
    }

    #[test]
    fn unavailable_desktops_map_to_registered_errors() {
        for s in [
            DesktopState::Locked,
            DesktopState::Disconnected,
            DesktopState::NonInteractive,
            DesktopState::NoDisplay,
        ] {
            let e = s.error().unwrap();
            assert_eq!(e.code, Code::DesktopUnavailable);
            assert_eq!(e.code.exit_code(), 6);
            assert!(e.details[0].ends_with(s.as_str()));
        }
        assert_eq!(
            DesktopState::Unsupported.error().unwrap().code,
            Code::UnsupportedPlatform
        );
        assert!(DesktopState::Available.error().is_none());
    }

    #[test]
    fn the_stub_never_pretends() {
        let u = Unsupported;
        assert_eq!(u.desktop().state, DesktopState::Unsupported);
        let ledger = Ledger::new();
        let e: NoeulError = u
            .capture(&ledger, Rect::new(0, 0, 1, 1))
            .unwrap_err()
            .into();
        assert_eq!(e.code, Code::UnsupportedPlatform);
        assert_eq!(ledger.live(), (0, 0));
    }
}
