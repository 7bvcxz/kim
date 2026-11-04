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
                "rect": m.rect.to_json(), "primary": m.primary,
            })).collect::<Vec<_>>(),
        })
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

    /// A picture of `region` of the virtual desktop, after checking the desktop again.
    fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError>;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn desk() -> Desktop {
        Desktop {
            state: DesktopState::Available,
            virtual_rect: Some(Rect::new(-1920, 0, 3840, 1080)),
            monitors: vec![
                Monitor {
                    rect: Rect::new(-1920, 0, 1920, 1080),
                    primary: false,
                },
                Monitor {
                    rect: Rect::new(0, 0, 1920, 1080),
                    primary: true,
                },
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
