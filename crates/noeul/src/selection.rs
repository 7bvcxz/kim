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
//! Step 49 drives sessions from `noeul capture --region … --pin` (the region is captured and
//! selected as a whole). The selection overlay of `capture.region` (`F1`) is Step 50: it will
//! open a session with the picture of every monitor and update the selection from the pointer.

use crate::frame::{Frame, Rect};

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
    Cancelled,
}

#[derive(Debug)]
struct Session {
    id: u64,
    origin: Origin,
    frame: Frame,
    selection: Option<Rect>,
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
}

impl Captures {
    pub fn new() -> Captures {
        Captures::default()
    }

    /// Opens a session with its frozen picture. Refused while another one is open; the
    /// picture is then dropped at once.
    pub fn begin(&mut self, origin: Origin, frame: Frame) -> Result<u64, SessionError> {
        if let Some(s) = &self.open {
            return Err(SessionError::Busy { open: s.id });
        }
        self.next_id += 1;
        self.started += 1;
        self.open = Some(Session {
            id: self.next_id,
            origin,
            frame,
            selection: None,
        });
        Ok(self.next_id)
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
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
        let id = self.begin(origin, frame).unwrap_or(0);
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

    /// `(started, pinned, cancelled)` since the instance started (a local metric, ADR 0012).
    pub fn counts(&self) -> (u64, u64, u64) {
        (self.started, self.pinned, self.cancelled)
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
        let id = c.begin(Origin::Hotkey, f).unwrap();
        // Rapid repeated presses: refused, and their pictures are dropped at once.
        for _ in 0..5 {
            let again = Frame::solid(&ledger, Rect::new(0, 0, 10, 10), [0; 4]).unwrap();
            assert_eq!(
                c.begin(Origin::Hotkey, again).unwrap_err(),
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
        let id = c.begin(Origin::Cli, f).unwrap();
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
        let id = c.begin(Origin::Cli, f).unwrap();
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
}
