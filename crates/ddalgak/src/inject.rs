//! Injection with a ledger of held keys and buttons (`DDALGAK-SEC-004`, `DDALGAK-SEC-005`).
//!
//! Every event Ddalgak injects goes through an [`Injector`]: the backend tags it with
//! [`crate::event::OWN_MARKER`] (`dwExtraInfo`) and the injector records every key and mouse
//! button it holds down. [`Injector::release_all`] sends an up for each of them; the worker calls
//! it on pause, turn-off, emergency, session lock, a desktop switch, an elevated window taking
//! focus and shutdown, `Drop` calls it, and the panic hook of `ddalgak-gui` calls
//! [`release_all_global`]. A crash that kills the process cannot run any of these; Windows then
//! keeps the injected key state until the user presses the key (DDALGAK_SAFETY §10).
//!
//! Three kinds of output exist (Step 68): a **virtual key** (logical, what the active layout calls
//! it; the backend adds the scan code and the extended flag), a **scan code** (a `code:` output:
//! the key at that position whatever the layout) and a **mouse button**. [`Injector::adjust`]
//! sends events *without* recording them: the remap uses it to release the user's own trigger
//! modifiers while its target is held and to restore them afterwards (they are the user's keys,
//! not Ddalgak's, so pausing must not release them).

use std::sync::Mutex;

/// A mouse button as an output or a trigger. `Left` and `Right` are the logical primary and
/// secondary buttons: the backend follows the "swap buttons" setting (DDALGAK_RULE_MODEL §5.9).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl Button {
    pub const ALL: [Button; 5] = [
        Button::Left,
        Button::Right,
        Button::Middle,
        Button::X1,
        Button::X2,
    ];

    /// The rule syntax name (`MouseX1`).
    pub fn name(self) -> &'static str {
        match self {
            Button::Left => "MouseLeft",
            Button::Right => "MouseRight",
            Button::Middle => "MouseMiddle",
            Button::X1 => "MouseX1",
            Button::X2 => "MouseX2",
        }
    }

    pub fn from_name(name: &str) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.name() == name)
    }

    /// The documented `VK_*BUTTON` code. A keyboard never reports these, so the engine core uses
    /// them for mouse-button events from the low-level mouse hook.
    pub fn vk(self) -> u16 {
        match self {
            Button::Left => 0x01,
            Button::Right => 0x02,
            Button::Middle => 0x04,
            Button::X1 => 0x05,
            Button::X2 => 0x06,
        }
    }

    pub fn from_vk(vk: u16) -> Option<Button> {
        Button::ALL.into_iter().find(|b| b.vk() == vk)
    }
}

/// What one output event presses or releases.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum OutCode {
    /// A virtual key; the backend adds the layout's scan code and the extended flag.
    Vk(u16),
    /// A virtual key that must carry the extended flag although its virtual key does not say so
    /// (`NumpadEnter` is `VK_RETURN` with the flag).
    VkExtended(u16),
    /// A physical key by scan code (`code:` outputs).
    Scan(u16),
    /// A key exactly as the hook reported it (virtual key, scan code, extended flag): a key the
    /// engine held back while a dual role was decided and then sends on unchanged.
    Exact {
        vk: u16,
        scan: u16,
        ext: bool,
    },
    Button(Button),
}

impl OutCode {
    /// The virtual key, when the output is one.
    pub fn vk(self) -> Option<u16> {
        match self {
            OutCode::Vk(v) | OutCode::VkExtended(v) | OutCode::Exact { vk: v, .. } => Some(v),
            _ => None,
        }
    }
}

/// One output event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutEvent {
    pub code: OutCode,
    pub up: bool,
}

impl OutEvent {
    pub fn down(code: OutCode) -> OutEvent {
        OutEvent { code, up: false }
    }

    pub fn up(code: OutCode) -> OutEvent {
        OutEvent { code, up: true }
    }
}

/// One virtual-key transition (the shape tests and other-program injection use).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutKey {
    pub vk: u16,
    pub up: bool,
}

impl From<OutKey> for OutEvent {
    fn from(k: OutKey) -> OutEvent {
        OutEvent {
            code: OutCode::Vk(k.vk),
            up: k.up,
        }
    }
}

/// The platform's injection call. Returns how many events were inserted, in order.
pub trait Backend: Send {
    fn send(&mut self, events: &[OutEvent]) -> usize;
}

impl Backend for Box<dyn Backend> {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        (**self).send(events)
    }
}

/// Keys and buttons held down by any injector of this process (for the panic hook).
static HELD: Mutex<Vec<OutCode>> = Mutex::new(Vec::new());

/// The backend the panic hook uses (set by the host).
static PANIC_BACKEND: Mutex<Option<Box<dyn Backend>>> = Mutex::new(None);

fn global(f: impl FnOnce(&mut Vec<OutCode>)) {
    if let Ok(mut g) = HELD.lock() {
        f(&mut g);
    }
}

/// Injects keys and buttons and remembers what it holds.
pub struct Injector<B: Backend> {
    backend: B,
    held: Vec<OutCode>,
    /// Events the backend did not insert (`injection_failed`).
    pub failed: u64,
}

impl<B: Backend> std::fmt::Debug for Injector<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injector")
            .field("held", &self.held.len())
            .field("failed", &self.failed)
            .finish()
    }
}

impl<B: Backend> Injector<B> {
    pub fn new(backend: B) -> Injector<B> {
        Injector {
            backend,
            held: Vec::new(),
            failed: 0,
        }
    }

    /// Sends `events` and records the downs and ups that were really inserted, in order.
    fn send_tracked(&mut self, events: &[OutEvent]) -> bool {
        if events.is_empty() {
            return true;
        }
        let n = self.backend.send(events).min(events.len());
        if n < events.len() {
            self.failed += (events.len() - n) as u64;
        }
        for e in &events[..n] {
            if e.up {
                self.held.retain(|c| *c != e.code);
                global(|g| g.retain(|c| *c != e.code));
            } else if !self.held.contains(&e.code) {
                self.held.push(e.code);
                global(|g| {
                    if !g.contains(&e.code) {
                        g.push(e.code);
                    }
                });
            }
        }
        n == events.len()
    }

    /// Sends events without recording them (the user's own keys: trigger modifiers released
    /// while a remap holds its target, and restored afterwards).
    pub fn adjust(&mut self, events: &[OutEvent]) -> bool {
        if events.is_empty() {
            return true;
        }
        let n = self.backend.send(events).min(events.len());
        if n < events.len() {
            self.failed += (events.len() - n) as u64;
        }
        n == events.len()
    }

    /// Sends a mix: `untracked` first (not recorded), then `tracked` (recorded), in one call so
    /// no other input comes between them (`SendInput` inserts serially, *documented*).
    pub fn send_mixed(&mut self, events: &[(OutEvent, bool)]) -> bool {
        if events.is_empty() {
            return true;
        }
        let plain: Vec<OutEvent> = events.iter().map(|(e, _)| *e).collect();
        let n = self.backend.send(&plain).min(plain.len());
        if n < plain.len() {
            self.failed += (plain.len() - n) as u64;
        }
        for (e, tracked) in &events[..n] {
            if !tracked {
                continue;
            }
            if e.up {
                self.held.retain(|c| *c != e.code);
                global(|g| g.retain(|c| *c != e.code));
            } else if !self.held.contains(&e.code) {
                self.held.push(e.code);
                global(|g| {
                    if !g.contains(&e.code) {
                        g.push(e.code);
                    }
                });
            }
        }
        n == plain.len()
    }

    /// Presses and holds an output.
    pub fn press_code(&mut self, code: OutCode) -> bool {
        self.send_tracked(&[OutEvent::down(code)])
    }

    /// Releases an output this injector holds (one it does not hold is never released: that
    /// would be an unmatched up for the application).
    pub fn release_code(&mut self, code: OutCode) -> bool {
        if !self.held.contains(&code) {
            return true;
        }
        self.send_tracked(&[OutEvent::up(code)])
    }

    /// Presses and holds a virtual key.
    pub fn press(&mut self, vk: u16) -> bool {
        self.press_code(OutCode::Vk(vk))
    }

    /// Releases a virtual key this injector holds.
    pub fn release(&mut self, vk: u16) -> bool {
        self.release_code(OutCode::Vk(vk))
    }

    /// Down and up of a virtual key.
    pub fn tap(&mut self, vk: u16) -> bool {
        self.send_tracked(&[
            OutEvent::down(OutCode::Vk(vk)),
            OutEvent::up(OutCode::Vk(vk)),
        ])
    }

    /// Releases every held key and button. Returns how many were released.
    pub fn release_all(&mut self) -> usize {
        let events: Vec<OutEvent> = self.held.iter().rev().map(|c| OutEvent::up(*c)).collect();
        if events.is_empty() {
            return 0;
        }
        self.send_tracked(&events);
        // A refused up (SendInput failed) is not retried forever: the ledger forgets it, and the
        // user's next press of that key resets Windows' state.
        for e in &events {
            self.held.retain(|c| *c != e.code);
            global(|g| g.retain(|c| *c != e.code));
        }
        events.len()
    }

    pub fn held(&self) -> &[OutCode] {
        &self.held
    }

    pub fn is_held(&self, code: OutCode) -> bool {
        self.held.contains(&code)
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }
}

impl<B: Backend> Drop for Injector<B> {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Registers the backend [`release_all_global`] uses (the host does this at start).
pub fn set_panic_backend(backend: Box<dyn Backend>) {
    if let Ok(mut g) = PANIC_BACKEND.lock() {
        *g = Some(backend);
    }
}

/// Releases every key and button any injector of this process still holds. Called from the
/// panic hook, so it only tries locks (a panic while one was held must not deadlock).
pub fn release_all_global() -> usize {
    let Ok(mut held) = HELD.try_lock() else {
        return 0;
    };
    let events: Vec<OutEvent> = held.iter().rev().map(|c| OutEvent::up(*c)).collect();
    if events.is_empty() {
        return 0;
    }
    let Ok(mut backend) = PANIC_BACKEND.try_lock() else {
        return 0;
    };
    let Some(b) = backend.as_mut() else {
        return 0;
    };
    let n = b.send(&events);
    held.clear();
    n
}

/// A backend that records what it would inject (tests, and `--dry-run` previews).
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    pub sent: Vec<OutEvent>,
    /// Insert at most this many events per call (simulates `SendInput` partial failures).
    pub limit: Option<usize>,
}

impl Backend for Recorder {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        let n = self.limit.map_or(events.len(), |l| l.min(events.len()));
        self.sent.extend_from_slice(&events[..n]);
        n
    }
}

/// A backend that injects nothing and keeps nothing: the debug test host's, so a test host
/// never types for the user even when a remapped key is pressed on the real keyboard while a
/// test runs (DDALGAK-SEC-010).
#[derive(Clone, Copy, Debug, Default)]
pub struct Discard;

impl Backend for Discard {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        events.len()
    }
}

/// A backend that appends to a shared log (worker tests read what the worker thread sent).
#[derive(Clone, Debug, Default)]
pub struct SharedRecorder(pub std::sync::Arc<Mutex<Vec<OutEvent>>>);

impl Backend for SharedRecorder {
    fn send(&mut self, events: &[OutEvent]) -> usize {
        if let Ok(mut g) = self.0.lock() {
            g.extend_from_slice(events);
        }
        events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(v: u16, up: bool) -> OutEvent {
        OutEvent {
            code: OutCode::Vk(v),
            up,
        }
    }

    #[test]
    fn every_pressed_key_is_released_on_release_all_and_drop() {
        let mut i = Injector::new(Recorder::default());
        i.press(0x7C);
        i.press(0x7D);
        i.tap(0x7E);
        assert_eq!(i.held().len(), 2);
        assert_eq!(i.release_all(), 2);
        assert!(i.held().is_empty());
        let sent = &i.backend().sent;
        assert_eq!(sent.len(), 6);
        assert!(sent[4].up && sent[5].up);
        assert_eq!(i.release_all(), 0, "nothing left to release");
        // A key that is not held is never released.
        i.release(0x7F);
        assert_eq!(i.backend().sent.len(), 6);
    }

    #[test]
    fn drop_releases_held_keys() {
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        {
            let mut i = Injector::new(SharedRecorder(log.clone()));
            i.press(0x7F);
        }
        let log = log.lock().unwrap();
        assert_eq!(log.as_slice(), &[vk(0x7F, false), vk(0x7F, true)]);
    }

    #[test]
    fn partial_failures_are_counted_and_not_recorded_as_held() {
        let mut i = Injector::new(Recorder {
            limit: Some(1),
            ..Recorder::default()
        });
        assert!(!i.tap(0x7C), "only the down was inserted");
        assert_eq!(i.failed, 1);
        assert!(
            i.is_held(OutCode::Vk(0x7C)),
            "the inserted down is held and will be released"
        );
        assert_eq!(i.release_all(), 1);
        assert!(i.held().is_empty());
    }

    #[test]
    fn scan_codes_and_buttons_are_held_and_released_like_keys() {
        let mut i = Injector::new(Recorder::default());
        i.press_code(OutCode::Scan(0x23));
        i.press_code(OutCode::Button(Button::X1));
        i.press_code(OutCode::VkExtended(0x0D));
        assert_eq!(i.held().len(), 3);
        // Pressing again (auto-repeat) keeps one entry.
        i.press_code(OutCode::Scan(0x23));
        assert_eq!(i.held().len(), 3);
        i.release_code(OutCode::Button(Button::X1));
        assert_eq!(i.release_all(), 2);
        let ups: Vec<OutCode> = i
            .backend()
            .sent
            .iter()
            .filter(|e| e.up)
            .map(|e| e.code)
            .collect();
        assert_eq!(
            ups,
            [
                OutCode::Button(Button::X1),
                OutCode::VkExtended(0x0D),
                OutCode::Scan(0x23)
            ],
            "release_all lets go in reverse order of pressing"
        );
    }

    #[test]
    fn adjustments_are_never_recorded() {
        let mut i = Injector::new(Recorder::default());
        // The user's own LCtrl is logically released and restored around a remap.
        i.adjust(&[vk(0xA2, true)]);
        i.adjust(&[vk(0xA2, false)]);
        assert!(
            i.held().is_empty(),
            "pausing must not release the user's keys"
        );
        assert_eq!(i.release_all(), 0);
        // A mixed call records only the tracked part.
        i.send_mixed(&[(vk(0xA2, true), false), (vk(0x7F, false), true)]);
        assert_eq!(i.held(), &[OutCode::Vk(0x7F)]);
        assert_eq!(i.backend().sent.len(), 4);
    }

    #[test]
    fn button_names_and_codes_round_trip() {
        for b in Button::ALL {
            assert_eq!(Button::from_name(b.name()), Some(b));
            assert_eq!(Button::from_vk(b.vk()), Some(b));
        }
        assert_eq!(Button::from_vk(0x03), None, "VK_CANCEL is not a button");
        assert_eq!(OutCode::VkExtended(0x0D).vk(), Some(0x0D));
        assert_eq!(OutCode::Scan(1).vk(), None);
    }
}
