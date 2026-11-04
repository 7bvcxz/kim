//! Injection with a ledger of held keys (`DDALGAK-SEC-004`, `DDALGAK-SEC-005`).
//!
//! Every key Ddalgak injects goes through an [`Injector`]: it tags the event with
//! [`crate::event::OWN_MARKER`] (the backend sets `dwExtraInfo`) and records every key it holds
//! down. [`Injector::release_all`] sends a key-up for each of them; the host calls it on pause,
//! turn-off, emergency, session lock and shutdown, `Drop` calls it, and the panic hook of
//! `ddalgak-gui` calls [`release_all_global`]. A crash that kills the process cannot run any of
//! these; Windows then keeps the injected key state until the user presses the key (documented
//! in DDALGAK_SAFETY §10; verified with a killed process in Step 73).
//!
//! Step 66 has no injecting rule yet (remaps arrive in Step 68). The engine tests use the
//! injector with obscure keys, and later Steps inject only through it.

use crate::event::KeySet;
use std::sync::Mutex;

/// One key transition to inject.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutKey {
    pub vk: u16,
    pub up: bool,
}

/// The platform's injection call. Returns how many events were inserted.
pub trait Backend: Send {
    fn send(&mut self, keys: &[OutKey]) -> usize;
}

/// Keys held down by any injector of this process (for the panic hook).
static HELD: Mutex<KeySet> = Mutex::new(KeySet::empty());

/// The backend the panic hook uses (set by the host).
static PANIC_BACKEND: Mutex<Option<Box<dyn Backend>>> = Mutex::new(None);

/// Injects keys and remembers what it holds.
pub struct Injector<B: Backend> {
    backend: B,
    held: KeySet,
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
            held: KeySet::default(),
            failed: 0,
        }
    }

    fn send(&mut self, keys: &[OutKey]) -> bool {
        let n = self.backend.send(keys);
        if n < keys.len() {
            self.failed += (keys.len() - n) as u64;
        }
        // Record what was really inserted, in order.
        for k in &keys[..n.min(keys.len())] {
            if k.up {
                self.held.remove(k.vk);
                if let Ok(mut g) = HELD.lock() {
                    g.remove(k.vk);
                }
            } else {
                self.held.insert(k.vk);
                if let Ok(mut g) = HELD.lock() {
                    g.insert(k.vk);
                }
            }
        }
        n == keys.len()
    }

    /// Presses and holds a key.
    pub fn press(&mut self, vk: u16) -> bool {
        self.send(&[OutKey { vk, up: false }])
    }

    /// Releases a key this injector holds (a key it does not hold is never released: that
    /// would be an unmatched key-up for the application).
    pub fn release(&mut self, vk: u16) -> bool {
        if !self.held.contains(vk) {
            return true;
        }
        self.send(&[OutKey { vk, up: true }])
    }

    /// Down and up.
    pub fn tap(&mut self, vk: u16) -> bool {
        self.send(&[OutKey { vk, up: false }, OutKey { vk, up: true }])
    }

    /// Releases every held key. Returns how many were released.
    pub fn release_all(&mut self) -> usize {
        let keys: Vec<OutKey> = self.held.iter().map(|vk| OutKey { vk, up: true }).collect();
        if keys.is_empty() {
            return 0;
        }
        self.send(&keys);
        keys.len()
    }

    pub fn held(&self) -> &KeySet {
        &self.held
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

/// Releases every key any injector of this process still holds. Called from the panic hook, so
/// it only tries locks (a panic while one was held must not deadlock).
pub fn release_all_global() -> usize {
    let Ok(mut held) = HELD.try_lock() else {
        return 0;
    };
    let keys: Vec<OutKey> = held.iter().map(|vk| OutKey { vk, up: true }).collect();
    if keys.is_empty() {
        return 0;
    }
    let Ok(mut backend) = PANIC_BACKEND.try_lock() else {
        return 0;
    };
    let Some(b) = backend.as_mut() else {
        return 0;
    };
    let n = b.send(&keys);
    held.clear();
    n
}

/// A backend that records what it would inject (tests, and `--dry-run` previews).
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    pub sent: Vec<OutKey>,
    /// Insert at most this many events per call (simulates `SendInput` partial failures).
    pub limit: Option<usize>,
}

impl Backend for Recorder {
    fn send(&mut self, keys: &[OutKey]) -> usize {
        let n = self.limit.map_or(keys.len(), |l| l.min(keys.len()));
        self.sent.extend_from_slice(&keys[..n]);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        struct Shared(std::sync::Arc<Mutex<Vec<OutKey>>>);
        impl Backend for Shared {
            fn send(&mut self, keys: &[OutKey]) -> usize {
                self.0.lock().unwrap().extend_from_slice(keys);
                keys.len()
            }
        }
        let log = std::sync::Arc::new(Mutex::new(Vec::new()));
        {
            let mut i = Injector::new(Shared(log.clone()));
            i.press(0x7F);
        }
        let log = log.lock().unwrap();
        assert_eq!(
            log.as_slice(),
            &[
                OutKey {
                    vk: 0x7F,
                    up: false
                },
                OutKey { vk: 0x7F, up: true }
            ]
        );
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
            i.held().contains(0x7C),
            "the inserted down is held and will be released"
        );
        assert_eq!(i.release_all(), 1);
        assert!(i.held().is_empty());
    }
}
