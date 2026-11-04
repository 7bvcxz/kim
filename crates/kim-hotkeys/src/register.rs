//! The narrow `register` backend for global chords (docs/HOTKEY_CONTRACT.md §4.2, §9.2, §12).
//!
//! On Windows, [`Registrar`] owns one dedicated thread with a message queue and no window (the
//! hotkeys are registered with a null window handle, so `WM_HOTKEY` arrives as a thread
//! message). Every `RegisterHotKey`/`UnregisterHotKey` call runs on that thread. A chord that
//! another program already registered fails with `ERROR_HOTKEY_ALREADY_REGISTERED` and is
//! reported as [`Status::Taken`]: Kim never unregisters, re-registers over or hooks around
//! another program's hotkey, and never retries in the background (`KIM-SEC-020`). Dropping the
//! registrar (or [`Registrar::shutdown`]) unregisters everything it holds and ends the thread.
//! No hook is installed and no key other than a registered chord is ever seen (`KIM-SEC-022`).
//!
//! Other platforms get a stub that fails with [`RegisterError::PlatformUnsupported`]
//! (`hotkey_unsupported`); Linux support is not claimed before M4 (ADR 0011).

use crate::keys::{Chord, Key, Side};
use std::collections::BTreeMap;

/// Result of one registration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// The chord is now this tool's until it unregisters it or exits.
    Registered,
    /// Another program owns the chord; the binding stays inactive (`taken`).
    Taken,
}

impl Status {
    /// `registration` value of `hotkeys list` (§10.2).
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Registered => "registered",
            Status::Taken => "taken",
        }
    }
}

/// Why a chord could not be registered (other than `taken`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RegisterError {
    /// `RegisterHotKey` cannot express the chord (side-specific modifiers, `code:`/`char:` keys,
    /// AltGr, Win, F12, NumpadEnter, sequences).
    Unsupported(String),
    /// Global hotkeys are not implemented on this platform.
    PlatformUnsupported,
    /// The OS refused for another reason.
    Os { code: u32 },
    /// The registration thread is not running any more.
    Stopped,
    /// The id is not (or no longer) registered by this registrar.
    NotRegistered,
}

impl RegisterError {
    /// The registered CLI error code this maps to.
    pub fn code(&self) -> &'static str {
        match self {
            RegisterError::Unsupported(_) | RegisterError::PlatformUnsupported => {
                "hotkey_unsupported"
            }
            RegisterError::Os { .. } | RegisterError::Stopped | RegisterError::NotRegistered => {
                "internal"
            }
        }
    }
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterError::Unsupported(why) => write!(f, "cannot be registered globally: {why}"),
            RegisterError::PlatformUnsupported => {
                f.write_str("global hotkeys are not supported on this platform yet")
            }
            RegisterError::Os { code } => {
                write!(f, "the OS refused the registration (error {code})")
            }
            RegisterError::Stopped => f.write_str("the hotkey thread has stopped"),
            RegisterError::NotRegistered => f.write_str("that hotkey is not registered"),
        }
    }
}

impl std::error::Error for RegisterError {}

/// `MOD_ALT`, `MOD_CONTROL`, `MOD_SHIFT` (`RegisterHotKey`, documented values).
const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;

/// Maps a chord to `RegisterHotKey`'s modifiers and virtual key. Pure; the numbers never leave
/// this function (`KIM-NFR-022`). Fails for what `register` cannot do (§4.2): side-specific
/// modifiers, AltGr, `Win` (reserved for the OS), `code:`/`char:` keys, `F12` (reserved for
/// debuggers), `NumpadEnter` (indistinguishable from Enter) and IME keys.
pub fn win32_chord(c: &Chord) -> Result<(u32, u32), RegisterError> {
    let m = &c.mods;
    let unsupported = |why: &str| Err(RegisterError::Unsupported(why.to_string()));
    if m.side_specific() {
        return unsupported("left/right modifiers need a hook");
    }
    if m.altgr {
        return unsupported("AltGr chords need a hook");
    }
    if m.win != Side::None {
        return unsupported("Win chords are reserved for the OS");
    }
    let Key::Named(name) = c.key else {
        return unsupported("physical and character keys need a hook");
    };
    let Some(vk) = virtual_key(name) else {
        return unsupported(&format!("{name} cannot be registered"));
    };
    let mut mods = 0;
    if m.ctrl.on() {
        mods |= MOD_CONTROL;
    }
    if m.alt.on() {
        mods |= MOD_ALT;
    }
    if m.shift.on() {
        mods |= MOD_SHIFT;
    }
    Ok((mods, vk))
}

/// Windows virtual-key codes of the contract's logical key names (documented `VK_*` values).
fn virtual_key(name: &str) -> Option<u32> {
    let b = name.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(u32::from(b[0])); // VK_A…VK_Z = 'A'…'Z', VK_0…VK_9 = '0'…'9'
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return (n != 12).then_some(0x6F + n); // VK_F1 = 0x70
    }
    if let Some(n) = name
        .strip_prefix("Numpad")
        .and_then(|n| n.parse::<u32>().ok())
    {
        return Some(0x60 + n); // VK_NUMPAD0
    }
    Some(match name {
        "Escape" => 0x1B,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Space" => 0x20,
        "Backspace" => 0x08,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "Up" => 0x26,
        "Down" => 0x28,
        "Left" => 0x25,
        "Right" => 0x27,
        "Plus" => 0xBB,
        "Minus" => 0xBD,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        "PrintScreen" => 0x2C,
        "Pause" => 0x13,
        "ScrollLock" => 0x91,
        "ContextMenu" => 0x5D,
        "MediaNext" => 0xB0,
        "MediaPrevious" => 0xB1,
        "MediaStop" => 0xB2,
        "MediaPlayPause" => 0xB3,
        "VolumeMute" => 0xAD,
        "VolumeDown" => 0xAE,
        "VolumeUp" => 0xAF,
        _ => return None, // NumpadEnter, IME keys
    })
}

/// An identifier of one registration held by a [`Registrar`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct HotkeyId(pub i32);

/// Owns the registration thread. See the module documentation.
#[derive(Debug)]
pub struct Registrar {
    inner: imp::Inner,
}

impl Registrar {
    /// Starts the registration thread. Registers nothing.
    pub fn start() -> Result<Registrar, RegisterError> {
        Ok(Registrar {
            inner: imp::Inner::start(None)?,
        })
    }

    /// Like [`Registrar::start`], and `waker` runs on the registration thread right after each
    /// press is queued, so an event loop that also waits for other things (a GUI message loop)
    /// learns about a press without polling and then takes it with
    /// `next_press(Duration::ZERO)`. The waker must not block.
    pub fn start_with_waker(waker: Box<dyn Fn() + Send>) -> Result<Registrar, RegisterError> {
        Ok(Registrar {
            inner: imp::Inner::start(Some(waker))?,
        })
    }

    /// Registers one global chord (`MOD_NOREPEAT`, so auto-repeat does not fire it again).
    /// `Ok((None, Taken))` when another program owns it; nothing is retried.
    pub fn register(&self, chord: &Chord) -> Result<(Option<HotkeyId>, Status), RegisterError> {
        let (mods, vk) = win32_chord(chord)?;
        self.inner.register(mods, vk)
    }

    /// Releases one registration.
    pub fn unregister(&self, id: HotkeyId) -> Result<(), RegisterError> {
        self.inner.unregister(id)
    }

    /// The next pressed hotkey, waiting at most `timeout` (no polling: a blocking receive).
    pub fn next_press(&self, timeout: std::time::Duration) -> Option<HotkeyId> {
        self.inner.next_press(timeout)
    }

    /// Unregisters everything and ends the thread (also done on drop).
    #[allow(clippy::drop_non_drop)] // only the Windows backend holds state to release
    pub fn shutdown(self) {
        drop(self);
    }
}

/// The global bindings of one tool, kept in sync with its effective bindings (§8.3 5): what is
/// no longer wanted is released, what is new is registered, and what already works is left
/// alone. A chord that turns out to be taken stays inactive until the next explicit sync.
#[derive(Debug)]
pub struct GlobalHotkeys {
    registrar: Registrar,
    held: BTreeMap<String, (Chord, HotkeyId)>,
}

impl GlobalHotkeys {
    pub fn new(registrar: Registrar) -> GlobalHotkeys {
        GlobalHotkeys {
            registrar,
            held: BTreeMap::new(),
        }
    }

    /// Makes the held registrations equal `wanted` (action → chord) and returns the status of
    /// every wanted action.
    pub fn sync(
        &mut self,
        wanted: &[(String, Chord)],
    ) -> Vec<(String, Result<Status, RegisterError>)> {
        let stale: Vec<String> = self
            .held
            .iter()
            .filter(|(a, (c, _))| !wanted.iter().any(|(w, wc)| w == *a && wc == c))
            .map(|(a, _)| a.clone())
            .collect();
        for a in stale {
            if let Some((_, id)) = self.held.remove(&a) {
                let _ = self.registrar.unregister(id);
            }
        }
        let mut out = Vec::new();
        for (action, chord) in wanted {
            if self.held.contains_key(action) {
                out.push((action.clone(), Ok(Status::Registered)));
                continue;
            }
            let r = self.registrar.register(chord).map(|(id, status)| {
                if let Some(id) = id {
                    self.held.insert(action.clone(), (*chord, id));
                }
                status
            });
            out.push((action.clone(), r));
        }
        out
    }

    /// Releases everything (the tool-wide switch, §12.4).
    pub fn release_all(&mut self) {
        let _ = self.sync(&[]);
    }

    /// The action a pressed hotkey belongs to.
    pub fn action_for(&self, id: HotkeyId) -> Option<&str> {
        self.held
            .iter()
            .find(|(_, (_, h))| *h == id)
            .map(|(a, _)| a.as_str())
    }

    pub fn registrar(&self) -> &Registrar {
        &self.registrar
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use super::{HotkeyId, RegisterError, Status};
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY,
    };

    /// Wakes the thread to run queued commands.
    const WM_KIM_COMMAND: u32 = WM_APP + 0x4B; // 'K'

    enum Cmd {
        Register(i32, u32, u32, Sender<Result<Status, RegisterError>>),
        Unregister(i32, Sender<Result<(), RegisterError>>),
        Shutdown,
    }

    #[derive(Debug)]
    pub(super) struct Inner {
        thread_id: u32,
        cmds: Sender<Cmd>,
        presses: Receiver<i32>,
        next_id: AtomicI32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl std::fmt::Debug for Cmd {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Cmd")
        }
    }

    /// How long a caller waits for the thread to answer one command.
    const REPLY: Duration = Duration::from_secs(5);

    impl Inner {
        pub(super) fn start(waker: Option<Box<dyn Fn() + Send>>) -> Result<Inner, RegisterError> {
            let (cmd_tx, cmd_rx) = channel::<Cmd>();
            let (press_tx, press_rx) = channel::<i32>();
            let (ready_tx, ready_rx) = channel::<u32>();
            let join = std::thread::Builder::new()
                .name("kim-hotkeys".into())
                .spawn(move || run(&cmd_rx, &press_tx, &ready_tx, waker.as_deref()))
                .map_err(|_| RegisterError::Stopped)?;
            let thread_id = ready_rx
                .recv_timeout(REPLY)
                .map_err(|_| RegisterError::Stopped)?;
            Ok(Inner {
                thread_id,
                cmds: cmd_tx,
                presses: press_rx,
                next_id: AtomicI32::new(1),
                join: Some(join),
            })
        }

        fn send(&self, cmd: Cmd) -> Result<(), RegisterError> {
            self.cmds.send(cmd).map_err(|_| RegisterError::Stopped)?;
            // SAFETY: posts a message without pointers to a thread id this struct created; the
            // call fails harmlessly (returns 0) when the thread has ended.
            let ok = unsafe { PostThreadMessageW(self.thread_id, WM_KIM_COMMAND, 0, 0) };
            if ok == 0 {
                return Err(RegisterError::Stopped);
            }
            Ok(())
        }

        pub(super) fn register(
            &self,
            mods: u32,
            vk: u32,
        ) -> Result<(Option<HotkeyId>, Status), RegisterError> {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = channel();
            self.send(Cmd::Register(id, mods, vk, tx))?;
            let status = rx
                .recv_timeout(REPLY)
                .map_err(|_| RegisterError::Stopped)??;
            Ok((
                (status == Status::Registered).then_some(HotkeyId(id)),
                status,
            ))
        }

        pub(super) fn unregister(&self, id: HotkeyId) -> Result<(), RegisterError> {
            let (tx, rx) = channel();
            self.send(Cmd::Unregister(id.0, tx))?;
            rx.recv_timeout(REPLY).map_err(|_| RegisterError::Stopped)?
        }

        pub(super) fn next_press(&self, timeout: Duration) -> Option<HotkeyId> {
            self.presses.recv_timeout(timeout).ok().map(HotkeyId)
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let _ = self.send(Cmd::Shutdown);
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    /// The registration thread: a plain message loop that sleeps in `GetMessageW` (no polling,
    /// no idle wake-ups, `KIM-NFR-020`).
    fn run(
        cmds: &Receiver<Cmd>,
        presses: &Sender<i32>,
        ready: &Sender<u32>,
        waker: Option<&(dyn Fn() + Send)>,
    ) {
        // SAFETY: MSG is plain data; all-zero is a valid value for an out-parameter.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: creates this thread's message queue (documented PeekMessage behavior) so that
        // PostThreadMessageW cannot fail before the first GetMessageW; `msg` is a valid pointer.
        unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
        // SAFETY: no preconditions.
        let _ = ready.send(unsafe { GetCurrentThreadId() });
        let mut held: Vec<i32> = Vec::new();
        'outer: loop {
            // SAFETY: `msg` is a valid out-pointer; a null window handle retrieves this thread's
            // messages. Returns 0 for WM_QUIT and -1 on error.
            let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if r == 0 || r == -1 {
                break;
            }
            match msg.message {
                WM_HOTKEY => {
                    let _ = presses.send(i32::try_from(msg.wParam).unwrap_or(-1));
                    if let Some(w) = waker {
                        w();
                    }
                }
                WM_KIM_COMMAND => {
                    while let Ok(cmd) = cmds.try_recv() {
                        match cmd {
                            Cmd::Register(id, mods, vk, reply) => {
                                // SAFETY: plain values; a null window handle associates the
                                // hotkey with this thread, which unregisters it below.
                                let ok = unsafe {
                                    RegisterHotKey(
                                        std::ptr::null_mut(),
                                        id,
                                        mods | MOD_NOREPEAT,
                                        vk,
                                    )
                                };
                                let result = if ok != 0 {
                                    held.push(id);
                                    Ok(Status::Registered)
                                } else {
                                    // SAFETY: no preconditions; read right after the failure.
                                    match unsafe { GetLastError() } {
                                        ERROR_HOTKEY_ALREADY_REGISTERED => Ok(Status::Taken),
                                        code => Err(RegisterError::Os { code }),
                                    }
                                };
                                let _ = reply.send(result);
                            }
                            Cmd::Unregister(id, reply) => {
                                let result = if held.contains(&id) {
                                    held.retain(|h| *h != id);
                                    // SAFETY: unregisters an id this thread registered.
                                    let ok = unsafe { UnregisterHotKey(std::ptr::null_mut(), id) };
                                    if ok != 0 {
                                        Ok(())
                                    } else {
                                        // SAFETY: no preconditions; read right after the failure.
                                        let code = unsafe { GetLastError() };
                                        Err(RegisterError::Os { code })
                                    }
                                } else {
                                    Err(RegisterError::NotRegistered)
                                };
                                let _ = reply.send(result);
                            }
                            Cmd::Shutdown => break 'outer,
                        }
                    }
                }
                _ => {}
            }
        }
        for id in held {
            // SAFETY: releases this thread's own registrations before it ends.
            unsafe { UnregisterHotKey(std::ptr::null_mut(), id) };
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{HotkeyId, RegisterError, Status};
    use std::time::Duration;

    /// Not implemented outside Windows (ADR 0011); every call fails with `hotkey_unsupported`.
    #[derive(Debug)]
    pub(super) struct Inner;

    impl Inner {
        pub(super) fn start(_waker: Option<Box<dyn Fn() + Send>>) -> Result<Inner, RegisterError> {
            Err(RegisterError::PlatformUnsupported)
        }
        pub(super) fn register(
            &self,
            _mods: u32,
            _vk: u32,
        ) -> Result<(Option<HotkeyId>, Status), RegisterError> {
            Err(RegisterError::PlatformUnsupported)
        }
        pub(super) fn unregister(&self, _id: HotkeyId) -> Result<(), RegisterError> {
            Err(RegisterError::PlatformUnsupported)
        }
        pub(super) fn next_press(&self, _timeout: Duration) -> Option<HotkeyId> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> Chord {
        Chord::parse(s).unwrap()
    }

    #[test]
    fn chords_map_to_register_hotkey_or_are_refused() {
        assert_eq!(
            win32_chord(&chord("Ctrl+Alt+Shift+F24")).unwrap(),
            (7, 0x87)
        );
        assert_eq!(win32_chord(&chord("F1")).unwrap(), (0, 0x70));
        assert_eq!(
            win32_chord(&chord("Ctrl+Shift+T")).unwrap(),
            (6, u32::from(b'T'))
        );
        assert_eq!(win32_chord(&chord("Alt+Numpad7")).unwrap(), (1, 0x67));
        for bad in [
            "LCtrl+F5",
            "AltGr+Q",
            "Win+F5",
            "Ctrl+code:KeyZ",
            "F12",
            "NumpadEnter",
            "HangulMode",
        ] {
            assert!(
                matches!(win32_chord(&chord(bad)), Err(RegisterError::Unsupported(_))),
                "{bad}"
            );
        }
        assert_eq!(
            RegisterError::PlatformUnsupported.code(),
            "hotkey_unsupported"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_report_unsupported() {
        assert_eq!(
            Registrar::start().unwrap_err(),
            RegisterError::PlatformUnsupported
        );
    }
}
