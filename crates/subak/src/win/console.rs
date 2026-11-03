//! The attaching client's own console: raw VT input, VT output, size changes.
//!
//! Only used when `subak host attach` runs in a real console. With redirected stdin/stdout
//! (scripts, tests, agents) the client streams bytes instead and none of this applies.

use std::io;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT,
    ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleMode, GetConsoleOutputCP, GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD,
    KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    SetConsoleOutputCP,
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

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

/// Raw mode for the lifetime of the value; the previous modes and code page come back on drop.
#[derive(Debug)]
pub struct RawMode {
    input: Option<(HANDLE, CONSOLE_MODE)>,
    output: Option<(HANDLE, CONSOLE_MODE, u32)>,
}

impl RawMode {
    pub fn enable() -> Self {
        let mut raw = Self {
            input: None,
            output: None,
        };
        let hin = std_handle(STD_INPUT_HANDLE);
        if let Some(m) = mode(hin) {
            let new = (m
                & !(ENABLE_LINE_INPUT
                    | ENABLE_ECHO_INPUT
                    | ENABLE_PROCESSED_INPUT
                    | ENABLE_MOUSE_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT
                | ENABLE_WINDOW_INPUT;
            // SAFETY: console input handle verified by GetConsoleMode.
            if unsafe { SetConsoleMode(hin, new) } != 0 {
                raw.input = Some((hin, m));
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
            // SAFETY: console output handle verified by GetConsoleMode; 65001 = UTF-8, which is
            // what ConPTY emits.
            unsafe {
                SetConsoleMode(hout, new);
                SetConsoleOutputCP(65001);
            }
            raw.output = Some((hout, m, cp));
        }
        raw
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring values read from the same handles in `enable`.
        unsafe {
            if let Some((h, m)) = self.input {
                SetConsoleMode(h, m);
            }
            if let Some((h, m, cp)) = self.output {
                SetConsoleMode(h, m);
                SetConsoleOutputCP(cp);
            }
        }
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
pub fn read_input(size: &mut (u16, u16), pending_high: &mut Option<u16>, poll: Duration) -> io::Result<Vec<Input>> {
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
                let ch = unsafe { key.uChar.UnicodeChar };
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
