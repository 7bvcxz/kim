//! The attaching client's own console: raw VT input, VT output, size changes, and putting the
//! console back the way it was on every exit path the process can still act on.
//!
//! Only used when `subak attach` runs in a real console. With redirected stdin/stdout (scripts,
//! tests, agents) the client streams bytes instead and none of this applies.
//!
//! **Restore** ([`restore`]) puts back the input and output modes and the output code page saved
//! by [`RawMode::enable`], after resetting the terminal modes the pane turned on and never turned
//! off ([`ModeTracker`]: alternate screen, hidden cursor, mouse reporting, bracketed paste, focus
//! events, application cursor keys and keypad). It runs once, from whichever comes first: the
//! [`RawMode`] guard's drop (detach, the pane's end, an error), the panic hook, or the console
//! control handler (`CTRL_CLOSE_EVENT`, logoff, shutdown). A client ended with `TerminateProcess`
//! runs no code at all; docs/SUBAK_CLI.md §5 records what the shells around it do then.

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, CTRL_BREAK_EVENT, CTRL_C_EVENT,
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT,
    ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT, GetConsoleMode, GetConsoleOutputCP,
    GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;
use windows_sys::core::BOOL;

/// The console state [`RawMode::enable`] replaced, `-1` when nothing is to be restored.
static SAVED_INPUT: AtomicI64 = AtomicI64::new(-1);
static SAVED_OUTPUT: AtomicI64 = AtomicI64::new(-1);
static SAVED_CP: AtomicI64 = AtomicI64::new(-1);
/// Terminal modes the pane's output turned on and did not turn off ([`ModeTracker`]).
static ACTIVE_MODES: AtomicU32 = AtomicU32::new(0);
/// What a Ctrl+Break (or, with redirected input, Ctrl+C) signal does during an attach.
static ON_INTERRUPT: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

fn std_handle(which: u32) -> HANDLE {
    // SAFETY: plain query of a process-wide standard handle; may be null/invalid.
    unsafe { GetStdHandle(which) }
}

fn mode(h: HANDLE) -> Option<CONSOLE_MODE> {
    let mut m = 0;
    // SAFETY: `m` is writable; fails for non-console handles.
    (unsafe { GetConsoleMode(h, &raw mut m) } != 0).then_some(m)
}

pub fn stdin_is_console() -> bool {
    mode(std_handle(STD_INPUT_HANDLE)).is_some()
}

pub fn stdout_is_console() -> bool {
    mode(std_handle(STD_OUTPUT_HANDLE)).is_some()
}

/// The visible window size of the console stdout is attached to.
pub fn window_size() -> Option<(u16, u16)> {
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    // SAFETY: `info` is writable; fails for non-console handles.
    if unsafe { GetConsoleScreenBufferInfo(std_handle(STD_OUTPUT_HANDLE), &raw mut info) } == 0 {
        return None;
    }
    let w = info.srWindow;
    let cols = u16::try_from(i32::from(w.Right) - i32::from(w.Left) + 1).ok()?;
    let rows = u16::try_from(i32::from(w.Bottom) - i32::from(w.Top) + 1).ok()?;
    Some((cols.max(2), rows.max(2)))
}

/// Raw mode for the lifetime of the value; the previous modes and code page come back on drop
/// (and from the panic hook or the console control handler, whichever runs first: [`restore`]).
#[derive(Debug)]
pub struct RawMode {
    /// Whether this guard changed anything.
    pub input: bool,
    pub output: bool,
}

impl RawMode {
    pub fn enable() -> Self {
        let mut raw = Self {
            input: false,
            output: false,
        };
        ACTIVE_MODES.store(0, Ordering::SeqCst);
        let hin = std_handle(STD_INPUT_HANDLE);
        if let Some(m) = mode(hin) {
            let new = (m & !(ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_MOUSE_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT
                | ENABLE_WINDOW_INPUT;
            SAVED_INPUT.store(i64::from(m), Ordering::SeqCst);
            // SAFETY: console input handle verified by GetConsoleMode.
            if unsafe { SetConsoleMode(hin, new) } != 0 {
                raw.input = true;
            } else {
                SAVED_INPUT.store(-1, Ordering::SeqCst);
            }
        }
        let hout = std_handle(STD_OUTPUT_HANDLE);
        if let Some(m) = mode(hout) {
            // SAFETY: plain query.
            let cp = unsafe { GetConsoleOutputCP() };
            let new = m
                | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN;
            SAVED_OUTPUT.store(i64::from(m), Ordering::SeqCst);
            SAVED_CP.store(i64::from(cp), Ordering::SeqCst);
            // SAFETY: console output handle verified by GetConsoleMode; 65001 = UTF-8, which is
            // what ConPTY emits.
            unsafe {
                SetConsoleMode(hout, new);
                SetConsoleOutputCP(65001);
            }
            raw.output = true;
        }
        raw
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        restore();
    }
}

/// Puts the console back (module docs): first the escape sequences that undo the terminal
/// modes the pane left on, while VT processing is still enabled, then the saved modes and code
/// page. Each part runs at most once, so it is safe from any thread, the panic hook and the
/// console control handler, also concurrently.
pub fn restore() {
    let hout = std_handle(STD_OUTPUT_HANDLE);
    let out_mode = SAVED_OUTPUT.swap(-1, Ordering::SeqCst);
    if out_mode >= 0 {
        let reset = reset_sequence(ACTIVE_MODES.swap(0, Ordering::SeqCst));
        let mut written = 0u32;
        // SAFETY: console output handle (verified when the mode was saved); `reset` is a live
        // buffer of the given length. A failed write is ignored: this is best effort.
        unsafe {
            WriteFile(
                hout,
                reset.as_ptr(),
                u32::try_from(reset.len()).unwrap_or(0),
                &raw mut written,
                std::ptr::null_mut(),
            );
            SetConsoleMode(hout, out_mode as CONSOLE_MODE);
        }
    }
    let cp = SAVED_CP.swap(-1, Ordering::SeqCst);
    if cp >= 0 {
        // SAFETY: restores the code page read in `enable`.
        unsafe { SetConsoleOutputCP(cp as u32) };
    }
    let in_mode = SAVED_INPUT.swap(-1, Ordering::SeqCst);
    if in_mode >= 0 {
        // SAFETY: console input handle (verified when the mode was saved).
        unsafe { SetConsoleMode(std_handle(STD_INPUT_HANDLE), in_mode as CONSOLE_MODE) };
    }
}

/// Console control signals during an attach. Ctrl+C with raw console input is a key (`0x03`) for
/// the pane and raises no signal at all; Ctrl+Break always raises one, and so does Ctrl+C when
/// input is redirected. Both run `on_interrupt` (the attach detaches cleanly) instead of the
/// default handler, which would end the client without restoring its console. Closing the
/// console window, logoff and shutdown restore the console and then let Windows end the client.
pub fn handle_signals(on_interrupt: Box<dyn Fn() + Send>) {
    *ON_INTERRUPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(on_interrupt);
    // SAFETY: `ctrl_handler` is a valid handler for the process lifetime.
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 1) };
}

/// Removes the handler [`handle_signals`] installed.
pub fn release_signals() {
    // SAFETY: removes the handler added in `handle_signals` (a no-op if it was not added).
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 0) };
    *ON_INTERRUPT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

unsafe extern "system" fn ctrl_handler(ctrl: u32) -> BOOL {
    if ctrl == CTRL_C_EVENT || ctrl == CTRL_BREAK_EVENT {
        if let Ok(guard) = ON_INTERRUPT.try_lock()
            && let Some(f) = guard.as_ref()
        {
            f();
        }
        return 1;
    }
    restore();
    0
}

/// Terminal modes a pane can switch on with DEC private mode sequences (`ESC [ ? n h`), each a
/// bit of [`ACTIVE_MODES`], with the sequence that switches it off again.
const MODES: [(&[u16], u32, &str); 8] = [
    (&[1049, 1047, 47], 1 << 0, "\x1b[?1049l"),
    (&[25], 1 << 1, "\x1b[?25h"), // tracked inverted: `?25l` hides the cursor
    (
        &[1000, 1001, 1002, 1003],
        1 << 2,
        "\x1b[?1003l\x1b[?1002l\x1b[?1000l",
    ),
    (&[1005, 1006, 1015, 1016], 1 << 3, "\x1b[?1006l\x1b[?1015l"),
    (&[2004], 1 << 4, "\x1b[?2004l"),
    (&[1004], 1 << 5, "\x1b[?1004l"),
    (&[1], 1 << 6, "\x1b[?1l"),
    (&[66], 1 << 7, "\x1b>"),
];
/// `ESC =` (keypad application mode) shares the bit of DECNKM (`?66`).
const KEYPAD: u32 = 1 << 7;

/// The bytes that switch off every mode in `active`, then reset colors and attributes.
pub fn reset_sequence(active: u32) -> Vec<u8> {
    let mut out = String::new();
    for (_, bit, off) in MODES {
        if active & bit != 0 {
            out.push_str(off);
        }
    }
    out.push_str("\x1b[0m");
    out.into_bytes()
}

/// Follows the DEC private modes in a pane's output (sequences may be split across frames), so
/// a detach can undo what the pane left switched on in this client's terminal.
#[derive(Debug, Default)]
pub struct ModeTracker {
    state: Scan,
    params: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Esc,
    Csi,
    Private,
}

impl ModeTracker {
    /// Scans `bytes` and publishes the modes that are on now.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut active = ACTIVE_MODES.load(Ordering::SeqCst);
        for &b in bytes {
            self.state = match (self.state, b) {
                (_, 0x1b) => Scan::Esc,
                (Scan::Esc, b'[') => Scan::Csi,
                (Scan::Esc, b'=') => {
                    active |= KEYPAD;
                    Scan::Ground
                }
                (Scan::Esc, b'>') => {
                    active &= !KEYPAD;
                    Scan::Ground
                }
                (Scan::Csi, b'?') => {
                    self.params.clear();
                    Scan::Private
                }
                (Scan::Private, b'0'..=b'9' | b';') if self.params.len() < 64 => {
                    self.params.push(b);
                    Scan::Private
                }
                (Scan::Private, b'h' | b'l') => {
                    let on = b == b'h';
                    for p in self.params.split(|c| *c == b';') {
                        let Some(n) = std::str::from_utf8(p).ok().and_then(|s| s.parse().ok())
                        else {
                            continue;
                        };
                        if let Some((_, bit, _)) = MODES.iter().find(|(ns, ..)| ns.contains(&n)) {
                            // `?25h` shows the cursor: the mode to undo is "hidden".
                            if on != (n == 25) {
                                active |= bit;
                            } else {
                                active &= !bit;
                            }
                        }
                    }
                    Scan::Ground
                }
                _ => Scan::Ground,
            };
        }
        ACTIVE_MODES.store(active, Ordering::SeqCst);
    }
}

/// What the console delivered.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

/// Reads console input for up to `poll`; returns key bytes (UTF-8, VT sequences included thanks to
/// `ENABLE_VIRTUAL_TERMINAL_INPUT`) and a resize when the window size differs from `*size`.
/// The size is also re-checked on every timeout, because conhost does not report window-height
/// changes as events.
pub fn read_input(
    size: &mut (u16, u16),
    pending_high: &mut Option<u16>,
    poll: Duration,
    verbose: bool,
) -> io::Result<Vec<Input>> {
    let hin = std_handle(STD_INPUT_HANDLE);
    let mut out = Vec::new();
    let ms = u32::try_from(poll.as_millis()).unwrap_or(250);
    // SAFETY: console input handle (checked by the caller); a timed wait.
    if unsafe { WaitForSingleObject(hin, ms) } == WAIT_OBJECT_0 {
        let mut recs = [INPUT_RECORD::default(); 64];
        let mut n = 0u32;
        // SAFETY: `recs` has room for 64 records.
        if unsafe { ReadConsoleInputW(hin, recs.as_mut_ptr(), 64, &raw mut n) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut units: Vec<u16> = Vec::new();
        for rec in &recs[..n as usize] {
            if u32::from(rec.EventType) == KEY_EVENT {
                // SAFETY: EventType says the union holds a KEY_EVENT_RECORD.
                let key = unsafe { rec.Event.KeyEvent };
                // SAFETY: both union members are plain u16/char data.
                let raw_ch = unsafe { key.uChar.UnicodeChar };
                let ch = key_char(raw_ch, key.wVirtualKeyCode, key.dwControlKeyState);
                if verbose && (ch < 0x20 || raw_ch != ch) {
                    eprint!(
                        "subak: key down={} vk=0x{:02X} char=0x{raw_ch:04X} state=0x{:X} -> 0x{ch:04X}\r\n",
                        key.bKeyDown, key.wVirtualKeyCode, key.dwControlKeyState
                    );
                }
                if key.bKeyDown != 0 && ch != 0 {
                    for _ in 0..key.wRepeatCount.max(1) {
                        units.push(ch);
                    }
                }
            }
        }
        if let Some(h) = pending_high.take() {
            units.insert(0, h);
        }
        if let Some(&last) = units.last()
            && (0xD800..0xDC00).contains(&last)
        {
            *pending_high = units.pop();
        }
        if !units.is_empty() {
            out.push(Input::Bytes(String::from_utf16_lossy(&units).into_bytes()));
        }
    }
    if let Some(now) = window_size()
        && now != *size
    {
        *size = now;
        out.push(Input::Resize(now.0, now.1));
    }
    Ok(out)
}

const LEFT_ALT: u32 = 0x0002;
const RIGHT_ALT: u32 = 0x0001;
const LEFT_CTRL: u32 = 0x0008;
const RIGHT_CTRL: u32 = 0x0004;

/// The character a key event stands for. Ctrl+letter is the C0 control (`Ctrl+B` = `0x02`,
/// needed for the `Ctrl+B d` detach key) even when the console reports the plain letter or no
/// character at all, as some pseudo console versions do. AltGr (Ctrl+Alt) is left untouched.
pub fn key_char(ch: u16, vk: u16, state: u32) -> u16 {
    let ctrl = state & (LEFT_CTRL | RIGHT_CTRL) != 0;
    let alt = state & (LEFT_ALT | RIGHT_ALT) != 0;
    let letter = (0x41..=0x5A).contains(&vk); // VK_A..VK_Z
    let plain = ch == 0
        || char::from_u32(u32::from(ch))
            .is_some_and(|c| c.eq_ignore_ascii_case(&char::from(vk as u8)));
    if ctrl && !alt && letter && plain {
        vk - 0x40
    } else {
        ch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test owns the process-wide mode bits (tests run in parallel threads).
    #[test]
    fn modes_left_on_by_the_pane_are_tracked_across_frames_and_undone() {
        ACTIVE_MODES.store(0, Ordering::SeqCst);
        let mut t = ModeTracker::default();
        // vim-like: alternate screen, hidden cursor, bracketed paste and mouse, split mid-sequence.
        let stream = b"\x1b[?1049h\x1b[?25l\x1b[1mhello\x1b[?2004;1002h\x1b[?100";
        t.feed(stream);
        t.feed(b"6h\x1b=");
        let on = ACTIVE_MODES.load(Ordering::SeqCst);
        let reset = String::from_utf8(reset_sequence(on)).unwrap();
        for off in [
            "\x1b[?1049l",
            "\x1b[?25h",
            "\x1b[?2004l",
            "\x1b[?1003l\x1b[?1002l\x1b[?1000l",
            "\x1b[?1006l",
            "\x1b>",
        ] {
            assert!(reset.contains(off), "{off:?} missing from {reset:?}");
        }
        assert!(!reset.contains("\x1b[?1004l") && !reset.contains("\x1b[?1l"));
        assert!(reset.ends_with("\x1b[0m"));
        // The pane switched everything off again (`?25h` shows the cursor): nothing to undo.
        t.feed(b"\x1b[?1049l\x1b[?25h\x1b[?2004l\x1b[?1002;1006l\x1b>");
        assert_eq!(ACTIVE_MODES.load(Ordering::SeqCst), 0);
        assert_eq!(reset_sequence(0), b"\x1b[0m");
        // Ordinary sequences and text change nothing; overlong parameters are ignored.
        t.feed(b"\x1b[2J\x1b[12;5H\x1b]0;title\x07plain ? h l");
        t.feed(format!("\x1b[?{}h", "9".repeat(100)).as_bytes());
        assert_eq!(ACTIVE_MODES.load(Ordering::SeqCst), 0);
        ACTIVE_MODES.store(0, Ordering::SeqCst);
    }

    #[test]
    fn ctrl_letters_become_c0_controls() {
        const B: u16 = 0x42;
        assert_eq!(key_char(0x02, B, LEFT_CTRL), 0x02, "already a control char");
        assert_eq!(key_char(0, B, LEFT_CTRL), 0x02, "no char reported");
        assert_eq!(
            key_char(u16::from(b'b'), B, RIGHT_CTRL),
            0x02,
            "plain letter reported"
        );
        assert_eq!(key_char(u16::from(b'B'), B, LEFT_CTRL), 0x02);
        assert_eq!(key_char(u16::from(b'b'), B, 0), u16::from(b'b'), "no Ctrl");
        // AltGr = Ctrl+Alt produces real characters (e.g. `@` on German layouts): untouched.
        assert_eq!(
            key_char(u16::from(b'@'), 0x51, LEFT_CTRL | RIGHT_ALT),
            u16::from(b'@')
        );
        assert_eq!(key_char(0, 0x51, LEFT_CTRL | RIGHT_ALT), 0);
        assert_eq!(
            key_char(u16::from(b'1'), 0x31, LEFT_CTRL),
            u16::from(b'1'),
            "not a letter"
        );
    }
}
