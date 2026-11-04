//! What the capture overlay can detect under the pointer: windows and UI elements, as
//! rectangles only (NOEUL-FR-040, NOEUL-FR-043, NOEUL-SEC-004, NOEUL-SEC-007; NOEUL_UX_SPEC
//! §4.8; ADR 0035).
//!
//! **Privacy.** Detection reads geometry and a few flags, nothing else: a window's rectangle, its
//! visibility, whether it is minimized, cloaked, layered, click-through, its constant opacity and
//! its capture affinity; a UI element's bounding rectangle. It never reads a title, class name,
//! process name, element name, value, help text or any other text, and it stores nothing beyond
//! the open capture session (the [`Snapshot`] lives in the overlay and is dropped with it). The
//! types below cannot hold text: there is no field for it.
//!
//! **Windows.** When a capture starts, the platform lists the top-level windows of the desktop in
//! Z-order, topmost first, as [`WindowFacts`] — taken right after the frozen picture, so the list
//! matches what the picture shows. [`classify`] decides which of them detection offers:
//!
//! | Window | Detection | Why |
//! | --- | --- | --- |
//! | hidden, minimized, cloaked (another virtual desktop), empty or outside the picture | skipped | not in the picture |
//! | layered and click-through (`WS_EX_LAYERED` + `WS_EX_TRANSPARENT`) | skipped | the pointer and the eye go through it |
//! | layered with a constant opacity of 0 | skipped | invisible |
//! | excluded from capture (`WDA_EXCLUDEFROMCAPTURE`) | skipped | absent from the frozen picture: what lies below is what the user sees |
//! | protected (`WDA_MONITOR`) | **offered as protected**: not highlighted, a click does not select it, a note says Windows shows it black | the picture holds black where it is (NOEUL-SEC-004) |
//! | every other window, including other layered ones | offered | its extended frame bounds, clipped to the picture |
//!
//! [`Snapshot::window_at`] is the topmost offered window that contains a point. A protected window
//! still hides the windows below it, so the plain region selection is what remains there.
//!
//! **Elements.** After `detect.cycle` (`E`) the overlay asks for the UI element under the pointer
//! inside the window found there ([`ElementRequest`]); the platform answers off the UI thread
//! with an [`ElementAnswer`]. [`check_element`] accepts a rectangle only if it contains the point
//! and lies inside its window (after one DPI reinterpretation for providers that report logical
//! pixels under fractional scaling); anything else falls back to the window rectangle, and a
//! window that refused or did not answer in time is not asked again during that capture
//! ([`Fallback`]).

use crate::capture::Monitor;
use crate::dpi;
use crate::frame::Rect;
use crate::overlay::{grow, intersection};

/// What hover detection looks for (`detect.cycle`, `E`; `[capture] detection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetectMode {
    /// The window under the pointer (the default, NOEUL-FR-040).
    #[default]
    Window,
    /// The UI element under the pointer, through UI Automation rectangles.
    Element,
    /// Nothing: plain region selection only.
    Off,
}

impl DetectMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectMode::Window => "window",
            DetectMode::Element => "element",
            DetectMode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<DetectMode> {
        [DetectMode::Window, DetectMode::Element, DetectMode::Off]
            .into_iter()
            .find(|m| m.as_str() == s)
    }

    /// The next mode of `detect.cycle`: window, element, off, window…
    #[must_use]
    pub fn next(self) -> DetectMode {
        match self {
            DetectMode::Window => DetectMode::Element,
            DetectMode::Element => DetectMode::Off,
            DetectMode::Off => DetectMode::Window,
        }
    }
}

/// How Windows treats a window in screen captures (`GetWindowDisplayAffinity`, *documented*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Affinity {
    #[default]
    None,
    /// `WDA_MONITOR`: captured as black.
    Monitor,
    /// `WDA_EXCLUDEFROMCAPTURE`: left out of captures (Windows 10 2004 and later).
    Excluded,
}

/// What the platform read about one top-level window: geometry and flags, never text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowFacts {
    /// An opaque token for the window (its handle on Windows), used only to ask UI Automation for
    /// elements inside it during this capture. Never reported or stored.
    pub token: u64,
    /// The extended frame bounds (the visible frame, without the invisible resize border), in
    /// device pixels.
    pub rect: Rect,
    pub visible: bool,
    pub minimized: bool,
    /// Hidden by the compositor (another virtual desktop, a suspended app).
    pub cloaked: bool,
    pub layered: bool,
    /// `WS_EX_TRANSPARENT`.
    pub transparent: bool,
    /// The constant opacity of a layered window (`LWA_ALPHA`), when it has one.
    pub alpha: Option<u8>,
    pub affinity: Affinity,
}

impl WindowFacts {
    /// A visible, plain window at `rect` (tests and synthetic desktops).
    pub fn plain(token: u64, rect: Rect) -> WindowFacts {
        WindowFacts {
            token,
            rect,
            visible: true,
            minimized: false,
            cloaked: false,
            layered: false,
            transparent: false,
            alpha: None,
            affinity: Affinity::None,
        }
    }
}

/// Why a window is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Hidden,
    Minimized,
    Cloaked,
    Empty,
    OffPicture,
    ClickThrough,
    Invisible,
    ExcludedFromCapture,
}

/// A window detection offers: its rectangle clipped to the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectWindow {
    pub token: u64,
    pub rect: Rect,
    /// Protected from capture (`WDA_MONITOR`): shown black, never highlighted or selected.
    pub protected: bool,
    pub layered: bool,
}

/// Decides whether detection offers a window (the table in the module documentation).
pub fn classify(f: &WindowFacts, picture: Rect) -> Result<DetectWindow, Skip> {
    if !f.visible {
        return Err(Skip::Hidden);
    }
    if f.minimized {
        return Err(Skip::Minimized);
    }
    if f.cloaked {
        return Err(Skip::Cloaked);
    }
    if f.rect.is_empty() {
        return Err(Skip::Empty);
    }
    if f.layered && f.transparent {
        return Err(Skip::ClickThrough);
    }
    if f.layered && f.alpha == Some(0) {
        return Err(Skip::Invisible);
    }
    if f.affinity == Affinity::Excluded {
        return Err(Skip::ExcludedFromCapture);
    }
    let rect = intersection(f.rect, picture).ok_or(Skip::OffPicture)?;
    Ok(DetectWindow {
        token: f.token,
        rect,
        protected: f.affinity == Affinity::Monitor,
        layered: f.layered,
    })
}

/// The windows detection offers during one capture, topmost first. Dropped with the overlay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    windows: Vec<DetectWindow>,
    skipped: usize,
    /// Whether the list was read at all (`false`: the platform could not list windows, so
    /// detection falls back to the plain region selection).
    pub taken: bool,
}

impl Snapshot {
    /// The offered windows of `facts` (Z-order, topmost first) over `picture`.
    pub fn from_facts(facts: impl IntoIterator<Item = WindowFacts>, picture: Rect) -> Snapshot {
        let mut s = Snapshot {
            taken: true,
            ..Snapshot::default()
        };
        for f in facts {
            match classify(&f, picture) {
                Ok(w) => s.windows.push(w),
                Err(_) => s.skipped += 1,
            }
        }
        s
    }

    /// No list: detection is unavailable for this capture.
    pub fn unavailable() -> Snapshot {
        Snapshot::default()
    }

    pub fn windows(&self) -> &[DetectWindow] {
        &self.windows
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// How many listed windows were not offered.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// The topmost offered window that contains `(x, y)`.
    pub fn window_at(&self, (x, y): (i32, i32)) -> Option<&DetectWindow> {
        let at = Rect::new(x, y, 1, 1);
        self.windows.iter().find(|w| w.rect.contains(&at))
    }
}

/// A question for UI Automation: the element under `at` inside the window `token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementRequest {
    /// Increases with every request of a capture; the answer to an older one is used only if it
    /// still fits the pointer.
    pub seq: u64,
    pub token: u64,
    /// The window's rectangle (clipped to the picture).
    pub window: Rect,
    pub at: (i32, i32),
}

/// Why the element lookup fell back to the window rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The window belongs to a process with higher rights (an elevated program, UIPI) or refused.
    Denied,
    /// The window did not answer within the budget (an unresponsive program).
    Timeout,
    /// UI Automation is not available, or the window exposes no elements.
    Unavailable,
    /// The reported rectangle did not fit the window or the point, even as logical pixels.
    InvalidRect,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::Denied => "denied",
            Fallback::Timeout => "timeout",
            Fallback::Unavailable => "unavailable",
            Fallback::InvalidRect => "invalid_rect",
        }
    }

    /// Whether the window is not asked again during this capture.
    pub fn refuses_window(self) -> bool {
        !matches!(self, Fallback::InvalidRect)
    }
}

/// UI Automation's answer: a rectangle only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementAnswer {
    /// The deepest element under the point (device pixels, as reported).
    Found(Rect),
    /// No element smaller than the window lies under the point.
    WindowOnly,
    Failed(Fallback),
}

/// What a process may ask of a window's process (UIPI, *documented*): a process cannot drive the
/// UI of a process with higher rights, and Noeul never elevates (NOEUL-SEC-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRights {
    /// The target's token could be read: whether it is elevated.
    Elevated(bool),
    /// The target's token could not even be opened (an elevated or protected process).
    Denied,
    /// Unknown (the process is gone, or the check is not available).
    Unknown,
}

/// Whether UI Automation is asked at all for a window of a process with `target` rights, when
/// Noeul itself runs with `own_elevated`; `Some(Denied)` falls back to the window rectangle at
/// once instead of waiting for a refusal.
pub fn uia_permission(own_elevated: bool, target: TargetRights) -> Option<Fallback> {
    match target {
        TargetRights::Denied => Some(Fallback::Denied),
        TargetRights::Elevated(true) if !own_elevated => Some(Fallback::Denied),
        _ => None,
    }
}

/// Checks a reported element rectangle against its request: it must contain the point and lie
/// inside the window (one pixel of tolerance for frames). A rectangle that fails is tried once as
/// logical pixels of the window's monitor (a provider that reports unscaled coordinates under
/// fractional scaling); if that fails too, `None` (the window rectangle is used). An element as
/// large as its window is the window itself.
pub fn check_element(r: Rect, req: &ElementRequest, monitors: &[Monitor]) -> Option<Rect> {
    let fits = |r: Rect| {
        let at = Rect::new(req.at.0, req.at.1, 1, 1);
        !r.is_empty() && r.contains(&at) && grow(req.window, 1).contains(&r)
    };
    let found = if fits(r) {
        Some(r)
    } else {
        let centre = (
            i32::try_from(i64::from(req.window.x) + i64::from(req.window.width / 2))
                .unwrap_or(req.window.x),
            i32::try_from(i64::from(req.window.y) + i64::from(req.window.height / 2))
                .unwrap_or(req.window.y),
        );
        dpi::monitor_index(monitors, centre)
            .map(|i| dpi::rect_from_logical(&monitors[i], r))
            .filter(|m| fits(*m))
    }?;
    let clipped = intersection(found, req.window)?;
    (clipped != req.window).then_some(clipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIC: Rect = Rect::new(0, 0, 1920, 1080);

    fn facts(token: u64, rect: Rect) -> WindowFacts {
        WindowFacts::plain(token, rect)
    }

    #[test]
    fn modes_cycle_window_element_off() {
        let mut m = DetectMode::default();
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(m.as_str());
            m = m.next();
        }
        assert_eq!(seen, ["window", "element", "off", "window"]);
        assert_eq!(DetectMode::parse("element"), Some(DetectMode::Element));
        assert_eq!(DetectMode::parse("Element"), None);
    }

    #[test]
    fn classification_follows_the_table_layered_protected_and_hidden_windows() {
        let r = Rect::new(100, 100, 400, 300);
        let ok = |f: WindowFacts| classify(&f, PIC);
        assert_eq!(
            ok(facts(1, r)),
            Ok(DetectWindow {
                token: 1,
                rect: r,
                protected: false,
                layered: false
            })
        );
        let mut f = facts(2, r);
        f.visible = false;
        assert_eq!(ok(f), Err(Skip::Hidden));
        let mut f = facts(2, r);
        f.minimized = true;
        assert_eq!(ok(f), Err(Skip::Minimized));
        let mut f = facts(2, r);
        f.cloaked = true;
        assert_eq!(ok(f), Err(Skip::Cloaked));
        assert_eq!(ok(facts(2, Rect::new(5, 5, 0, 10))), Err(Skip::Empty));
        assert_eq!(
            ok(facts(2, Rect::new(5000, 5, 10, 10))),
            Err(Skip::OffPicture)
        );
        // Layered windows: detected unless click-through or fully transparent.
        let mut f = facts(3, r);
        f.layered = true;
        f.alpha = Some(200);
        assert!(ok(f).is_ok_and(|w| w.layered));
        f.alpha = None; // per-pixel alpha (UpdateLayeredWindow), like Noeul's own pins
        assert!(ok(f).is_ok());
        f.alpha = Some(0);
        assert_eq!(ok(f), Err(Skip::Invisible));
        f.alpha = Some(255);
        f.transparent = true;
        assert_eq!(ok(f), Err(Skip::ClickThrough));
        // WS_EX_TRANSPARENT alone does not make clicks pass through.
        let mut f = facts(4, r);
        f.transparent = true;
        assert!(ok(f).is_ok());
        // Capture affinity: excluded windows are absent from the picture; protected ones are
        // there, black, and are offered as protected only.
        let mut f = facts(5, r);
        f.affinity = Affinity::Excluded;
        assert_eq!(ok(f), Err(Skip::ExcludedFromCapture));
        f.affinity = Affinity::Monitor;
        assert!(ok(f).is_ok_and(|w| w.protected));
        // Clipped to the picture.
        assert_eq!(
            ok(facts(6, Rect::new(-10, -10, 50, 50))).map(|w| w.rect),
            Ok(Rect::new(0, 0, 40, 40))
        );
    }

    #[test]
    fn the_topmost_offered_window_wins_and_skipped_ones_let_the_one_below_through() {
        let mut excluded = facts(1, Rect::new(0, 0, 300, 300));
        excluded.affinity = Affinity::Excluded;
        let mut through = facts(2, Rect::new(0, 0, 300, 300));
        through.layered = true;
        through.transparent = true;
        let s = Snapshot::from_facts(
            [
                excluded,
                through,
                facts(3, Rect::new(50, 50, 100, 100)),
                facts(4, Rect::new(0, 0, 400, 400)),
            ],
            PIC,
        );
        assert!(s.taken);
        assert_eq!((s.len(), s.skipped()), (2, 2));
        assert_eq!(s.window_at((60, 60)).map(|w| w.token), Some(3), "above 4");
        assert_eq!(s.window_at((10, 10)).map(|w| w.token), Some(4));
        assert_eq!(s.window_at((149, 149)).map(|w| w.token), Some(3));
        assert_eq!(s.window_at((150, 150)).map(|w| w.token), Some(4));
        assert_eq!(s.window_at((500, 500)), None);
        let none = Snapshot::unavailable();
        assert!(!none.taken && none.is_empty());
        assert_eq!(none.window_at((10, 10)), None);
    }

    fn req(window: Rect, at: (i32, i32)) -> ElementRequest {
        ElementRequest {
            seq: 1,
            token: 7,
            window,
            at,
        }
    }

    #[test]
    fn element_rectangles_must_fit_the_point_and_the_window_with_one_dpi_retry() {
        let mons = vec![Monitor::new(PIC, true).with_dpi(144)];
        let w = Rect::new(150, 150, 900, 600);
        let r = req(w, (300, 300));
        assert_eq!(
            check_element(Rect::new(250, 250, 100, 100), &r, &mons),
            Some(Rect::new(250, 250, 100, 100))
        );
        // One pixel of frame tolerance, clipped to the window.
        assert_eq!(
            check_element(Rect::new(149, 149, 200, 200), &r, &mons),
            Some(Rect::new(150, 150, 199, 199))
        );
        // As large as the window: the window itself.
        assert_eq!(check_element(w, &r, &mons), None);
        // Does not contain the point.
        assert_eq!(check_element(Rect::new(400, 400, 50, 50), &r, &mons), None);
        // Reported in logical pixels at 150 % (a provider unaware of the scale): recovered.
        assert_eq!(
            check_element(Rect::new(180, 180, 40, 40), &r, &mons),
            Some(Rect::new(270, 270, 60, 60))
        );
        // Outside the window even as logical pixels: rejected.
        assert_eq!(check_element(Rect::new(0, 0, 90, 90), &r, &mons), None);
        assert_eq!(check_element(Rect::new(0, 0, 0, 0), &r, &mons), None);
    }

    #[test]
    fn uipi_skips_elevated_windows_unless_noeul_is_elevated_too() {
        use TargetRights::*;
        assert_eq!(uia_permission(false, Denied), Some(Fallback::Denied));
        assert_eq!(
            uia_permission(false, Elevated(true)),
            Some(Fallback::Denied)
        );
        assert_eq!(uia_permission(false, Elevated(false)), None);
        assert_eq!(uia_permission(true, Elevated(true)), None);
        assert_eq!(uia_permission(false, Unknown), None);
        assert!(Fallback::Timeout.refuses_window());
        assert!(!Fallback::InvalidRect.refuses_window());
        assert_eq!(
            [
                Fallback::Denied,
                Fallback::Timeout,
                Fallback::Unavailable,
                Fallback::InvalidRect
            ]
            .map(Fallback::as_str),
            ["denied", "timeout", "unavailable", "invalid_rect"]
        );
    }
}
