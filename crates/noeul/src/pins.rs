//! Pins and their lifecycle (NOEUL-FR-060, NOEUL-FR-065, NOEUL-FR-025, NOEUL-FR-026; ADR 0023 §3).
//!
//! A pin is a floating window that owns one [`Frame`]. The registry gives every pin a stable ID
//! (`pin-<n>`, never reused while the instance runs), keeps the windows behind the narrow
//! [`PinWindow`]/[`PinWindows`] traits (the Win32 implementation is `win::pinwnd`; tests use
//! fakes), and enforces the lifecycle:
//!
//! ```text
//!   create ──► open ──close──► closed (reopen memory: the last one) ──reopen──► open (same id)
//!                │                     │
//!                └──destroy──► gone ◄──┘ (a newer close replaces it; quit clears it)
//! ```
//!
//! Closing destroys the window but keeps the picture so "Reopen closed pin" can bring it back
//! (default memory: the last pin, NOEUL-FR-065); destroying frees it for good. Closing the
//! capture overlay never closes pins. Quitting releases every picture. Nothing is written to
//! disk (NOEUL-SEC-005).

use crate::frame::{Frame, Rect};
use crate::util::rfc3339;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::SystemTime;

/// A pin's stable identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PinId(pub u64);

impl PinId {
    pub fn parse(s: &str) -> Option<PinId> {
        let n = s.strip_prefix("pin-")?;
        if n.is_empty() || n.starts_with('0') || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        n.parse().ok().map(PinId)
    }
}

impl std::fmt::Display for PinId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pin-{}", self.0)
    }
}

/// Where a pin's picture came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Capture,
    Clipboard,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Capture => "capture",
            Source::Clipboard => "clipboard",
        }
    }
}

/// One pin window. Implementations own an OS window and destroy it in [`PinWindow::destroy`]
/// (and on drop).
pub trait PinWindow {
    /// Moves the window (device pixels).
    fn set_position(&mut self, x: i32, y: i32);
    /// Gives the window keyboard focus; `false` when the OS refused.
    fn focus(&mut self) -> bool;
    /// Destroys the window now.
    fn destroy(&mut self);
}

/// Creates pin windows.
pub trait PinWindows {
    /// A window showing `frame` at its rectangle, at 100 % (one image pixel per device pixel).
    /// `focus` gives it keyboard focus (only when the user created it, NOEUL-FR-060).
    fn create(
        &mut self,
        id: PinId,
        frame: &Frame,
        focus: bool,
    ) -> Result<Box<dyn PinWindow>, String>;
}

struct Pin {
    frame: Frame,
    window: Box<dyn PinWindow>,
    source: Source,
    created: SystemTime,
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pin")
            .field("rect", &self.frame.rect())
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Closed {
    id: PinId,
    frame: Frame,
    source: Source,
    created: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    NotFound(PinId),
    NothingToReopen,
    /// The window could not be created; the picture is still in the caller's hands.
    Window(String),
}

/// The pins of one instance.
#[derive(Debug, Default)]
pub struct Pins {
    open: BTreeMap<PinId, Pin>,
    closed: Option<Closed>,
    next: u64,
}

impl Pins {
    pub fn new() -> Pins {
        Pins::default()
    }

    /// Creates a pin of `frame` at the frame's position. On failure the frame is handed back.
    pub fn create(
        &mut self,
        windows: &mut dyn PinWindows,
        frame: Frame,
        source: Source,
        focus: bool,
    ) -> Result<PinId, (Frame, String)> {
        let id = PinId(self.next + 1);
        match windows.create(id, &frame, focus) {
            Ok(window) => {
                self.next += 1;
                self.open.insert(
                    id,
                    Pin {
                        frame,
                        window,
                        source,
                        created: SystemTime::now(),
                    },
                );
                Ok(id)
            }
            Err(e) => Err((frame, e)),
        }
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn contains(&self, id: PinId) -> bool {
        self.open.contains_key(&id)
    }

    pub fn ids(&self) -> Vec<PinId> {
        self.open.keys().copied().collect()
    }

    /// The pin closed last (reopen memory), if any.
    pub fn reopenable(&self) -> Option<PinId> {
        self.closed.as_ref().map(|c| c.id)
    }

    /// Closes a pin: the window goes, the picture stays in the reopen memory (replacing the
    /// previously closed one, which is freed).
    pub fn close(&mut self, id: PinId) -> Result<(), PinError> {
        let mut pin = self.open.remove(&id).ok_or(PinError::NotFound(id))?;
        pin.window.destroy();
        self.closed = Some(Closed {
            id,
            frame: pin.frame,
            source: pin.source,
            created: pin.created,
        });
        Ok(())
    }

    /// Removes a pin for good (`pin.destroy`): window and picture are freed.
    pub fn destroy(&mut self, id: PinId) -> Result<(), PinError> {
        let mut pin = self.open.remove(&id).ok_or(PinError::NotFound(id))?;
        pin.window.destroy();
        Ok(())
    }

    /// Reopens the last closed pin with its old ID and position.
    pub fn reopen(&mut self, windows: &mut dyn PinWindows, focus: bool) -> Result<PinId, PinError> {
        let c = self.closed.take().ok_or(PinError::NothingToReopen)?;
        match windows.create(c.id, &c.frame, focus) {
            Ok(window) => {
                self.open.insert(
                    c.id,
                    Pin {
                        frame: c.frame,
                        window,
                        source: c.source,
                        created: c.created,
                    },
                );
                Ok(c.id)
            }
            Err(e) => {
                self.closed = Some(c);
                Err(PinError::Window(e))
            }
        }
    }

    /// The window was moved (by the user dragging it, or a key); keeps the model in sync.
    pub fn moved(&mut self, id: PinId, x: i32, y: i32) -> bool {
        match self.open.get_mut(&id) {
            Some(p) => {
                p.frame.set_origin(x, y);
                true
            }
            None => false,
        }
    }

    /// Moves a pin by a delta (`pin.move-*`, one device pixel per press).
    pub fn nudge(&mut self, id: PinId, dx: i32, dy: i32) -> bool {
        let Some(p) = self.open.get_mut(&id) else {
            return false;
        };
        let r = p.frame.rect();
        let (x, y) = (r.x.saturating_add(dx), r.y.saturating_add(dy));
        p.window.set_position(x, y);
        self.moved(id, x, y)
    }

    pub fn focus(&mut self, id: PinId) -> bool {
        self.open.get_mut(&id).is_some_and(|p| p.window.focus())
    }

    /// Closes every pin and forgets everything, including the reopen memory (quit).
    pub fn clear(&mut self) -> usize {
        let n = self.open.len();
        for (_, mut p) in std::mem::take(&mut self.open) {
            p.window.destroy();
        }
        self.closed = None;
        n
    }

    pub fn rect(&self, id: PinId) -> Option<Rect> {
        self.open.get(&id).map(|p| p.frame.rect())
    }

    /// The pin list (`pin.list`): IDs, geometry and origin only, never pixels (NOEUL-SEC-008).
    pub fn list(&self) -> Value {
        json!({
            "pins": self.open.iter().map(|(id, p)| json!({
                "id": id.to_string(),
                "rect": p.frame.rect().to_json(),
                "source": p.source.as_str(),
                "created_at": rfc3339(p.created),
            })).collect::<Vec<_>>(),
            "reopenable": self.reopenable().map(|id| id.to_string()),
        })
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What the fake windows saw, shared with the test.
    #[derive(Debug, Default)]
    pub struct Log {
        pub created: Vec<(PinId, Rect, bool)>,
        pub destroyed: Vec<PinId>,
        pub moved: Vec<(PinId, i32, i32)>,
        pub alive: Vec<PinId>,
    }

    #[derive(Debug, Default, Clone)]
    pub struct FakeWindows {
        pub log: Rc<RefCell<Log>>,
        pub fail: Rc<RefCell<bool>>,
    }

    struct FakeWindow {
        id: PinId,
        log: Rc<RefCell<Log>>,
        destroyed: bool,
    }

    impl PinWindow for FakeWindow {
        fn set_position(&mut self, x: i32, y: i32) {
            self.log.borrow_mut().moved.push((self.id, x, y));
        }
        fn focus(&mut self) -> bool {
            true
        }
        fn destroy(&mut self) {
            if !self.destroyed {
                self.destroyed = true;
                let mut l = self.log.borrow_mut();
                l.destroyed.push(self.id);
                l.alive.retain(|a| *a != self.id);
            }
        }
    }

    impl Drop for FakeWindow {
        fn drop(&mut self) {
            self.destroy();
        }
    }

    impl PinWindows for FakeWindows {
        fn create(
            &mut self,
            id: PinId,
            frame: &Frame,
            focus: bool,
        ) -> Result<Box<dyn PinWindow>, String> {
            if *self.fail.borrow() {
                return Err("window creation failed (test)".into());
            }
            let mut l = self.log.borrow_mut();
            l.created.push((id, frame.rect(), focus));
            l.alive.push(id);
            Ok(Box::new(FakeWindow {
                id,
                log: Rc::clone(&self.log),
                destroyed: false,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeWindows;
    use super::*;
    use crate::frame::Ledger;

    fn frame(ledger: &std::sync::Arc<Ledger>, x: i32) -> Frame {
        Frame::solid(ledger, Rect::new(x, 0, 2, 2), [0, 0, 0, 255]).unwrap()
    }

    #[test]
    fn ids_parse_and_print() {
        assert_eq!(PinId::parse("pin-12"), Some(PinId(12)));
        for bad in ["pin-", "pin-0", "pin-01", "p-1", "pin-1x", "pin--1"] {
            assert_eq!(PinId::parse(bad), None, "{bad}");
        }
        assert_eq!(PinId(7).to_string(), "pin-7");
    }

    #[test]
    fn lifecycle_close_reopen_destroy_and_quit() {
        let ledger = Ledger::new();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        let a = pins
            .create(&mut w, frame(&ledger, 0), Source::Capture, true)
            .unwrap();
        let b = pins
            .create(&mut w, frame(&ledger, 5), Source::Clipboard, false)
            .unwrap();
        assert_eq!((a, b), (PinId(1), PinId(2)));
        assert_eq!(ledger.live(), (2, 32));
        assert!(w.log.borrow().created[0].2, "focus only when asked");
        assert!(!w.log.borrow().created[1].2);

        pins.close(a).unwrap();
        assert_eq!(w.log.borrow().destroyed, [a]);
        assert_eq!(pins.reopenable(), Some(a));
        assert_eq!(
            ledger.live(),
            (2, 32),
            "a closed pin keeps its picture for reopen"
        );
        assert_eq!(pins.close(a), Err(PinError::NotFound(a)));

        assert_eq!(
            pins.reopen(&mut w, false).unwrap(),
            a,
            "same id after reopen"
        );
        assert_eq!(pins.reopen(&mut w, false), Err(PinError::NothingToReopen));
        assert_eq!(pins.list()["pins"].as_array().unwrap().len(), 2);

        pins.destroy(b).unwrap();
        assert_eq!(ledger.live(), (1, 16), "destroy frees the picture");
        assert_eq!(pins.reopenable(), None);

        let c = pins
            .create(&mut w, frame(&ledger, 9), Source::Capture, false)
            .unwrap();
        assert_eq!(c, PinId(3), "ids are never reused");
        pins.close(c).unwrap();
        pins.close(a).unwrap();
        assert_eq!(
            pins.reopenable(),
            Some(a),
            "the memory keeps the last closed pin"
        );
        assert_eq!(ledger.live(), (1, 16), "the older closed picture was freed");

        let d = pins
            .create(&mut w, frame(&ledger, 1), Source::Capture, false)
            .unwrap();
        assert_eq!(pins.clear(), 1);
        assert!(
            w.log.borrow().alive.is_empty(),
            "quit destroys every window"
        );
        assert_eq!(ledger.live(), (0, 0), "quit releases every picture");
        assert!(!pins.contains(d));
    }

    #[test]
    fn failed_windows_hand_the_picture_back_and_moves_are_tracked() {
        let ledger = Ledger::new();
        let mut w = FakeWindows::default();
        let mut pins = Pins::new();
        *w.fail.borrow_mut() = true;
        let (back, why) = pins
            .create(&mut w, frame(&ledger, 0), Source::Capture, true)
            .unwrap_err();
        assert!(why.contains("failed"));
        assert_eq!(
            back.rect(),
            Rect::new(0, 0, 2, 2),
            "nothing is lost (NOEUL-FR-025)"
        );
        *w.fail.borrow_mut() = false;
        let id = pins.create(&mut w, back, Source::Capture, true).unwrap();
        assert_eq!(id, PinId(1), "a failed create uses no id");
        assert!(pins.nudge(id, -1, 0));
        assert!(pins.nudge(id, 0, 1));
        assert_eq!(pins.rect(id), Some(Rect::new(-1, 1, 2, 2)));
        assert_eq!(w.log.borrow().moved, [(id, -1, 0), (id, -1, 1)]);
        assert!(pins.moved(id, 300, 200));
        assert_eq!(pins.rect(id), Some(Rect::new(300, 200, 2, 2)));
        assert!(!pins.nudge(PinId(99), 1, 1));
        assert_eq!(ledger.live(), (1, 16), "moving copies nothing");
        let listed = pins.list();
        assert_eq!(listed["pins"][0]["id"], "pin-1");
        assert_eq!(listed["pins"][0]["rect"]["x"], 300);
    }
}
