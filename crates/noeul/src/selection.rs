//! Capture sessions: the frozen picture and its selection (NOEUL-FR-010, NOEUL-FR-013,
//! NOEUL-FR-025; ADR 0023 §3).
//!
//! A session owns the frozen picture from the moment it is taken until the capture ends. At
//! most one session exists at a time: a second request while one is open is refused, so rapid
//! repeated presses never open a second overlay (NOEUL-FR-013). The selection is a rectangle in
//! device pixels that must lie inside the picture. Finishing a session with "pin" hands out the
//! selected part as a new frame and frees the frozen picture; cancelling frees it at once.
//! Pinning never depends on the clipboard, and a pin that cannot be created gives its picture
//! back ([`Captures::restore`]) so nothing is lost silently (NOEUL-FR-025).
//!
//! Two paths open sessions: `noeul capture --region … --pin` (Step 49: the region is captured
//! and selected as a whole, [`Captures::finish_pin`]) and the selection overlay of
//! `capture.region` (`F1`, Step 50: the picture of every monitor, the selection from the
//! pointer and the keyboard, `crate::overlay`).
//!
//! # The selection result: [`CaptureSelection`]
//!
//! An output action (pin now; copy and save in Step 51) asks the open session for its confirmed
//! selection with [`Captures::selection`]. The answer is a [`CaptureSelection`]: its own copy of
//! the selected pixels (a [`Frame`] cropped from the frozen picture), the rectangle in
//! virtual-desktop device pixels and the monitors it covers (position, DPI, primary).
//!
//! - **Ownership.** The `CaptureSelection` owns its frame; the session keeps the frozen picture
//!   until the action reports success with [`Captures::complete`], which frees it. So a failed
//!   output (a pin window that cannot be created) loses nothing: the overlay stays with its
//!   selection and the user can try again (NOEUL-FR-025). A pin takes the frame with
//!   [`CaptureSelection::into_frame`] and owns it from then on; a copy or save (Step 51) borrows
//!   it with [`CaptureSelection::frame`] and drops the selection afterwards.
//! - **Lifetime.** From the output action until that action ends, on the instance's UI thread;
//!   never stored, never sent over IPC (only its geometry is reported), never written anywhere.
//!   Nothing creates one automatically: confirming a selection in the overlay only records the
//!   rectangle; the pixels stay in the frozen picture until the user picks an output. In
//!   particular nothing is copied to the clipboard and nothing reaches the disk (NOEUL-SEC-002,
//!   NOEUL-SEC-005).
//! - Like every frame, it is counted in the instance's ledger and `Debug` never prints pixels.

use crate::capture::Monitor;
use crate::frame::{Frame, Rect};
use serde_json::{Value, json};

/// Who asked for the capture (NOEUL-SEC-001: always an explicit action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Hotkey,
    Tray,
    Cli,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Hotkey => "hotkey",
            Origin::Tray => "tray",
            Origin::Cli => "cli",
        }
    }
}

/// How a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pinned,
    /// Copied to the clipboard (Step 51).
    Copied,
    /// Saved to a file (Step 51).
    Saved,
    Cancelled,
}

#[derive(Debug)]
struct Session {
    id: u64,
    origin: Origin,
    frame: Frame,
    monitors: Vec<Monitor>,
    selection: Option<Rect>,
}

/// The confirmed selection handed to an output action. See the module documentation for its
/// ownership and lifetime.
pub struct CaptureSelection {
    session: u64,
    origin: Origin,
    monitors: Vec<Monitor>,
    frame: Frame,
}

impl std::fmt::Debug for CaptureSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Geometry only, never pixels (NOEUL-SEC-008).
        f.debug_struct("CaptureSelection")
            .field("session", &self.session)
            .field("rect", &self.frame.rect())
            .field("monitors", &self.monitors.len())
            .finish_non_exhaustive()
    }
}

impl CaptureSelection {
    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// Where the selection is, in virtual-desktop device pixels.
    pub fn rect(&self) -> Rect {
        self.frame.rect()
    }

    /// The monitors the selection covers, in layout order.
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// The monitor that holds most of the selection (the first of equals).
    pub fn main_monitor(&self) -> Option<Monitor> {
        let r = self.rect();
        let area = |m: &Monitor| {
            crate::overlay::intersection(m.rect, r)
                .map_or(0, |i| u64::from(i.width) * u64::from(i.height))
        };
        let mut best: Option<Monitor> = None;
        for m in &self.monitors {
            if best.is_none_or(|b| area(m) > area(&b)) {
                best = Some(*m);
            }
        }
        best
    }

    /// The selected pixels, for an action that only reads them (copy, save).
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The selected pixels, moved into a pin.
    pub fn into_frame(self) -> Frame {
        self.frame
    }

    /// Geometry only (NOEUL-SEC-007, NOEUL-SEC-008).
    pub fn to_json(&self) -> Value {
        json!({
            "session": self.session,
            "rect": self.rect().to_json(),
            "monitors": self.monitors.iter().map(|m| json!({
                "rect": m.rect.to_json(), "primary": m.primary, "dpi": m.dpi,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Refusals of the session rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// Another capture is open (`busy`).
    Busy { open: u64 },
    /// No session with this id is open.
    NotOpen,
    /// The selection is empty or not inside the frozen picture.
    BadSelection(Rect),
    /// Nothing is selected yet.
    NoSelection,
}

/// The capture sessions of one instance.
#[derive(Debug, Default)]
pub struct Captures {
    open: Option<Session>,
    next_id: u64,
    last: Option<Rect>,
    started: u64,
    pinned: u64,
    cancelled: u64,
    copied: u64,
    saved: u64,
    ignored: u64,
}

impl Captures {
    pub fn new() -> Captures {
        Captures::default()
    }

    /// Opens a session with its frozen picture and the monitors it shows. Refused while another
    /// one is open; the picture is then dropped at once.
    pub fn begin(
        &mut self,
        origin: Origin,
        frame: Frame,
        monitors: Vec<Monitor>,
    ) -> Result<u64, SessionError> {
        if let Some(s) = &self.open {
            return Err(SessionError::Busy { open: s.id });
        }
        self.next_id += 1;
        self.started += 1;
        self.open = Some(Session {
            id: self.next_id,
            origin,
            frame,
            monitors,
            selection: None,
        });
        Ok(self.next_id)
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The id of the open session.
    pub fn open_id(&self) -> Option<u64> {
        self.open.as_ref().map(|s| s.id)
    }

    /// The open session's frozen picture (the overlay draws from it).
    pub fn frozen(&self) -> Option<&Frame> {
        self.open.as_ref().map(|s| &s.frame)
    }

    /// A capture request that found a session open and did nothing (rapid repeated presses,
    /// NOEUL-FR-013); counted for `noeul status`.
    pub fn note_ignored(&mut self) {
        self.ignored += 1;
    }

    pub fn ignored(&self) -> u64 {
        self.ignored
    }

    /// The confirmed selection of the open session as a [`CaptureSelection`] (a copy of its
    /// pixels). The session stays open until [`Captures::complete`].
    pub fn selection(&self, id: u64) -> Result<CaptureSelection, SessionError> {
        let s = match &self.open {
            Some(s) if s.id == id => s,
            _ => return Err(SessionError::NotOpen),
        };
        let rect = s.selection.ok_or(SessionError::NoSelection)?;
        let frame = s.frame.crop(rect).ok_or(SessionError::BadSelection(rect))?;
        Ok(CaptureSelection {
            session: s.id,
            origin: s.origin,
            monitors: s
                .monitors
                .iter()
                .filter(|m| m.rect.intersects(&rect))
                .copied()
                .collect(),
            frame,
        })
    }

    /// Ends the open session after its output succeeded: frees the frozen picture and makes the
    /// selection the "last capture" (`selection.last`, NOEUL-FR-014).
    pub fn complete(&mut self, id: u64, how: Outcome) -> Result<(), SessionError> {
        let s = self.session(id)?;
        let rect = s.selection;
        self.open = None;
        match how {
            Outcome::Pinned | Outcome::Copied | Outcome::Saved => {
                match how {
                    Outcome::Pinned => self.pinned += 1,
                    Outcome::Copied => self.copied += 1,
                    _ => self.saved += 1,
                }
                self.last = rect.or(self.last);
            }
            Outcome::Cancelled => self.cancelled += 1,
        }
        Ok(())
    }

    pub fn origin(&self) -> Option<Origin> {
        self.open.as_ref().map(|s| s.origin)
    }

    fn session(&mut self, id: u64) -> Result<&mut Session, SessionError> {
        match &mut self.open {
            Some(s) if s.id == id => Ok(s),
            _ => Err(SessionError::NotOpen),
        }
    }

    /// Sets (or replaces) the confirmed selection.
    pub fn select(&mut self, id: u64, rect: Rect) -> Result<(), SessionError> {
        let s = self.session(id)?;
        if rect.is_empty() || !s.frame.rect().contains(&rect) {
            return Err(SessionError::BadSelection(rect));
        }
        s.selection = Some(rect);
        Ok(())
    }

    /// Ends the session with "pin": the selected part as a new frame, and the frozen picture is
    /// freed. The rectangle becomes the "last capture" (`selection.last`, NOEUL-FR-014).
    pub fn finish_pin(&mut self, id: u64) -> Result<Frame, SessionError> {
        let s = self.session(id)?;
        let rect = s.selection.ok_or(SessionError::NoSelection)?;
        let out = if rect == s.frame.rect() {
            // The whole picture: move it instead of copying.
            let s = self.open.take().expect("checked above");
            s.frame
        } else {
            let out = s.frame.crop(rect).ok_or(SessionError::BadSelection(rect))?;
            self.open = None;
            out
        };
        self.last = Some(rect);
        self.pinned += 1;
        Ok(out)
    }

    /// A pin could not be created from `frame`: reopen the session with it so the user can try
    /// again or copy and save instead (NOEUL-FR-025). The counters stay honest.
    pub fn restore(&mut self, origin: Origin, frame: Frame) -> u64 {
        self.pinned = self.pinned.saturating_sub(1);
        self.started = self.started.saturating_sub(1);
        let rect = frame.rect();
        let id = self.begin(origin, frame, Vec::new()).unwrap_or(0);
        let _ = self.select(id, rect);
        id
    }

    /// Cancels the open session (any id); its picture is freed. `false` when none was open.
    pub fn cancel(&mut self) -> bool {
        let was = self.open.take().is_some();
        if was {
            self.cancelled += 1;
        }
        was
    }

    /// The rectangle of the last successful capture.
    pub fn last(&self) -> Option<Rect> {
        self.last
    }

    /// `(started, pinned, cancelled)` since the instance started (a local metric, ADR 0012);
    /// ignored requests are [`Captures::ignored`].
    pub fn counts(&self) -> (u64, u64, u64) {
        (self.started, self.pinned, self.cancelled)
    }

    /// `(copied, saved)` since the instance started (Step 51).
    pub fn output_counts(&self) -> (u64, u64) {
        (self.copied, self.saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Ledger;

    #[test]
    fn one_session_at_a_time_and_pictures_are_released() {
        let ledger = Ledger::new();
        let mut c = Captures::new();
        let f = Frame::solid(&ledger, Rect::new(0, 0, 10, 10), [0, 0, 0, 255]).unwrap();
        let id = c.begin(Origin::Hotkey, f, Vec::new()).unwrap();
        // Rapid repeated presses: refused, and their pictures are dropped at once.
        for _ in 0..5 {
            let again = Frame::solid(&ledger, Rect::new(0, 0, 10, 10), [0; 4]).unwrap();
            assert_eq!(
                c.begin(Origin::Hotkey, again, Vec::new()).unwrap_err(),
                SessionError::Busy { open: id }
            );
        }
        assert_eq!(
            ledger.live(),
            (1, 400),
            "only the open session's picture is alive"
        );
        assert!(c.cancel());
        assert_eq!(ledger.live(), (0, 0), "cancel frees the frozen picture");
        assert!(!c.cancel());
        assert_eq!(c.counts(), (1, 0, 1));
    }

    #[test]
    fn pinning_hands_out_the_selection_and_frees_the_rest() {
        let ledger = Ledger::new();
        let mut c = Captures::new();
        let f = Frame::solid(&ledger, Rect::new(-20, -20, 40, 40), [9, 9, 9, 255]).unwrap();
        let id = c.begin(Origin::Cli, f, Vec::new()).unwrap();
        assert_eq!(c.finish_pin(id).unwrap_err(), SessionError::NoSelection);
        assert_eq!(
            c.select(id, Rect::new(10, 10, 20, 20)),
            Err(SessionError::BadSelection(Rect::new(10, 10, 20, 20)))
        );
        assert_eq!(
            c.select(id + 1, Rect::new(0, 0, 1, 1)),
            Err(SessionError::NotOpen)
        );
        c.select(id, Rect::new(-10, -10, 5, 6)).unwrap();
        let pin = c.finish_pin(id).unwrap();
        assert_eq!(pin.rect(), Rect::new(-10, -10, 5, 6));
        assert!(!c.is_open());
        assert_eq!(ledger.live(), (1, 120), "only the pinned part is kept");
        assert_eq!(c.last(), Some(Rect::new(-10, -10, 5, 6)));
        drop(pin);
        assert_eq!(ledger.live(), (0, 0));
    }

    #[test]
    fn a_failed_pin_gives_the_picture_back() {
        let ledger = Ledger::new();
        let mut c = Captures::new();
        let f = Frame::solid(&ledger, Rect::new(0, 0, 8, 8), [1; 4]).unwrap();
        let id = c.begin(Origin::Cli, f, Vec::new()).unwrap();
        c.select(id, Rect::new(0, 0, 8, 8)).unwrap();
        let whole = c.finish_pin(id).unwrap();
        assert_eq!(
            ledger.live(),
            (1, 256),
            "a whole-picture pin moves, not copies"
        );
        let id = c.restore(Origin::Cli, whole);
        assert!(c.is_open());
        assert_eq!(c.counts(), (1, 0, 0));
        let again = c.finish_pin(id).unwrap();
        assert_eq!(again.rect(), Rect::new(0, 0, 8, 8));
    }

    #[test]
    fn a_capture_selection_copies_the_selection_and_keeps_the_session_until_complete() {
        let ledger = Ledger::new();
        let mut c = Captures::new();
        let mons = vec![
            Monitor::new(Rect::new(0, 0, 20, 10), true),
            Monitor::new(Rect::new(20, 0, 20, 10), false).with_dpi(144),
        ];
        let f = Frame::solid(&ledger, Rect::new(0, 0, 40, 10), [3; 4]).unwrap();
        let id = c.begin(Origin::Hotkey, f, mons).unwrap();
        assert_eq!(c.selection(id).unwrap_err(), SessionError::NoSelection);
        c.select(id, Rect::new(15, 2, 10, 4)).unwrap();
        let sel = c.selection(id).unwrap();
        assert_eq!(sel.rect(), Rect::new(15, 2, 10, 4));
        assert_eq!(sel.monitors().len(), 2, "it crosses the seam");
        assert_eq!(sel.main_monitor().unwrap().dpi, 96, "a tie keeps the first");
        assert_eq!(sel.origin(), Origin::Hotkey);
        assert!(!format!("{sel:?}").contains("[3"), "no pixels in Debug");
        assert_eq!(sel.to_json()["monitors"][1]["dpi"], 144);
        assert_eq!(
            ledger.live(),
            (2, 40 * 10 * 4 + 10 * 4 * 4),
            "the frozen picture stays while the output runs"
        );
        assert!(c.is_open());
        // The output failed: nothing is lost, the session is still there.
        drop(sel);
        assert_eq!(c.frozen().unwrap().rect(), Rect::new(0, 0, 40, 10));
        // It succeeded this time: the pin owns the pixels, the frozen picture is freed.
        let pin = c.selection(id).unwrap().into_frame();
        c.complete(id, Outcome::Pinned).unwrap();
        assert!(!c.is_open());
        assert_eq!(ledger.live(), (1, 160));
        assert_eq!(c.last(), Some(Rect::new(15, 2, 10, 4)));
        assert_eq!(c.counts(), (1, 1, 0));
        drop(pin);
        assert_eq!(ledger.live(), (0, 0));
        assert_eq!(c.complete(id, Outcome::Pinned), Err(SessionError::NotOpen));
        c.note_ignored();
        assert_eq!(c.ignored(), 1);
    }
}
