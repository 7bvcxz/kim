//! Typing on Windows (Step 69, ADR 0034): key translation for hotstrings, the focused-target
//! probe, the clipboard and the local clock. Every function here only reads, except the
//! clipboard's `set_text`/`restore`, which a `paste` run calls with the user's clipboard consent.
//!
//! - [`translate`]: `ToUnicodeEx` for the foreground thread's layout with flag 4 ("keyboard state
//!   is not changed", Windows 10 1607 and later, *documented*), so translating a key in the hook
//!   never disturbs a dead key the focused application is waiting on.
//! - [`WinProbe`]: the foreground window and its focused control (`GetGUIThreadInfo`), password
//!   edits (`ES_PASSWORD`), read-only edits (`ES_READONLY`) and disabled controls
//!   (`IsWindowEnabled`), the process integrity level, the session state and the IME. The IME
//!   mode is read from the focused window's default IME window (`WM_IME_CONTROL` with
//!   `IMC_GETOPENSTATUS` and `IMC_GETCONVERSIONMODE`, `SendMessageTimeoutW` with a 100 ms limit);
//!   when that cannot be read the mode counts as not native. Titles, text and keys are never read
//!   (`DDALGAK-SEC-011`).
//! - [`WinClipboard`]: opens the clipboard with the app's hidden window as owner (that window's
//!   thread pumps messages, so other programs are never blocked by Ddalgak owning the
//!   clipboard). See [`crate::run`] for what is copied and restored; formats Windows synthesizes
//!   (`CF_BITMAP` and `CF_PALETTE` from a DIB, `CF_METAFILEPICT` from an enhanced metafile) are
//!   left to Windows, enhanced metafiles are copied by their bits, memory formats byte for byte,
//!   and anything else (owner-drawn, private and other GDI formats, more than 32 MiB) makes the
//!   copy refuse (`clipboard_not_restorable`). Text Ddalgak puts on the clipboard, and the
//!   restored copy, carry the documented formats that keep them out of the clipboard history and
//!   the cloud clipboard.
//!
//! The clipboard and IME paths need a real desktop and are **not tested** by the automated tests
//! (they must not touch the maintainer's clipboard or IME); DDALGAK_ENGINE §12 lists the manual
//! checks.

use super::context::{is_elevated_window, is_password_window};
use super::hook::Shared;
use super::{own_integrity, wide};
use crate::hotstring::Typed;
use crate::run::{ClipData, ClipError, Clipboard, Probe, Snapshot, Target};
use crate::time::LocalTime;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND, SYSTEMTIME};
use windows_sys::Win32::Graphics::Gdi::{
    DeleteEnhMetaFile, GetEnhMetaFileBits, HENHMETAFILE, SetEnhMetaFileBits,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::Input::Ime::{IME_CMODE_NATIVE, ImmGetDefaultIMEWnd};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, IsWindowEnabled, ToUnicodeEx,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ES_READONLY, GUITHREADINFO, GWL_STYLE, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo,
    GetWindowLongW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    WM_IME_CONTROL,
};

const VK_SHIFT: usize = 0x10;
const VK_CONTROL: usize = 0x11;
const VK_MENU: usize = 0x12;
const VK_CAPITAL: usize = 0x14;
const VK_LSHIFT: usize = 0xA0;
const VK_LCONTROL: usize = 0xA2;
const VK_RMENU: usize = 0xA5;
/// `ToUnicodeEx` flag: do not change the keyboard state (Windows 10 1607 and later).
const NO_STATE_CHANGE: u32 = 4;
/// `imm.h`: `WM_IME_CONTROL` commands for an IME window.
const IMC_GETCONVERSIONMODE: usize = 0x0001;
const IMC_GETOPENSTATUS: usize = 0x0005;

/// What a key types in the foreground window's layout (the core's [`crate::hotstring::Translate`]).
pub fn translate(vk: u16, scan: u16, shift: bool, altgr: bool) -> Typed {
    let mut state = [0u8; 256];
    if shift {
        state[VK_SHIFT] = 0x80;
        state[VK_LSHIFT] = 0x80;
    }
    if altgr {
        for k in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
            state[k] = 0x80;
        }
    }
    // SAFETY: no preconditions; the toggle bit of Caps Lock.
    if unsafe { GetKeyState(VK_CAPITAL as i32) } & 1 != 0 {
        state[VK_CAPITAL] = 0x01;
    }
    // SAFETY: plain queries; a null pid pointer is allowed.
    let hkl = unsafe {
        GetKeyboardLayout(GetWindowThreadProcessId(
            GetForegroundWindow(),
            std::ptr::null_mut(),
        ))
    };
    let mut buf = [0u16; 8];
    // SAFETY: `state` has the 256 bytes the function reads; `buf` has room for 8 units; flag 4
    // leaves the keyboard state (pending dead keys) unchanged.
    let n = unsafe {
        ToUnicodeEx(
            u32::from(vk),
            u32::from(scan),
            state.as_ptr(),
            buf.as_mut_ptr(),
            8,
            NO_STATE_CHANGE,
            hkl,
        )
    };
    match n {
        n if n < 0 => Typed::Dead,
        1 | 2 => {
            let len = usize::try_from(n).unwrap_or(0);
            let mut chars = char::decode_utf16(buf[..len].iter().copied());
            match (chars.next(), chars.next()) {
                (Some(Ok(c)), None) => Typed::Char(c),
                _ => Typed::Nothing, // two characters (a ligature): not text the buffer models
            }
        }
        _ => Typed::Nothing,
    }
}

/// The local wall-clock time (`{date}`, `{time}`).
pub fn local_time() -> LocalTime {
    let mut st = SYSTEMTIME::default();
    // SAFETY: `st` is a writable SYSTEMTIME.
    unsafe { GetLocalTime(&raw mut st) };
    LocalTime {
        year: i32::from(st.wYear),
        month: u8::try_from(st.wMonth).unwrap_or(1),
        day: u8::try_from(st.wDay).unwrap_or(1),
        hour: u8::try_from(st.wHour).unwrap_or(0),
        minute: u8::try_from(st.wMinute).unwrap_or(0),
        second: u8::try_from(st.wSecond).unwrap_or(0),
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut class = [0u16; 64];
    // SAFETY: `class` has room for the given number of UTF-16 units.
    let n = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 64) };
    String::from_utf16_lossy(&class[..usize::try_from(n).unwrap_or(0)])
}

/// Whether `hwnd` can take typed text: it exists, is enabled, and is not a read-only edit.
pub(crate) fn is_editable(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    // SAFETY: reads a window's state; works for windows of other processes (documented).
    if unsafe { IsWindowEnabled(hwnd) } == 0 {
        return false;
    }
    let name = class_name(hwnd);
    let editlike =
        name.eq_ignore_ascii_case("Edit") || name.to_ascii_uppercase().starts_with("RICHEDIT");
    // SAFETY: as above.
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) };
    !(editlike && (style as u32) & (ES_READONLY as u32) != 0)
}

fn ime_query(ime: HWND, cmd: usize) -> Option<usize> {
    let mut out = 0usize;
    // SAFETY: a documented IME window message without pointers; the timeout bounds the wait and
    // SMTO_ABORTIFHUNG returns at once for a hung window.
    let ok = unsafe {
        SendMessageTimeoutW(
            ime,
            WM_IME_CONTROL,
            cmd,
            0,
            SMTO_ABORTIFHUNG,
            100,
            &raw mut out,
        )
    };
    (ok != 0).then_some(out)
}

/// Whether the IME of `hwnd` is open in a native-language mode (Hangul). Unknown counts as no.
fn ime_native(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    // SAFETY: no preconditions; returns null when the window has no IME window.
    let ime = unsafe { ImmGetDefaultIMEWnd(hwnd) };
    if ime.is_null() {
        return false;
    }
    ime_query(ime, IMC_GETOPENSTATUS).is_some_and(|open| open != 0)
        && ime_query(ime, IMC_GETCONVERSIONMODE).is_some_and(|m| m & IME_CMODE_NATIVE as usize != 0)
}

/// The focused target as the platform reports it (see the module documentation).
#[derive(Debug)]
pub struct WinProbe {
    shared: Arc<Shared>,
    own_il: u32,
}

impl WinProbe {
    pub fn new(shared: Arc<Shared>) -> WinProbe {
        WinProbe {
            shared,
            own_il: own_integrity().unwrap_or(0x2000),
        }
    }
}

impl Probe for WinProbe {
    fn sample(&mut self, ime_mode: bool) -> Target {
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        let mut info = GUITHREADINFO {
            cbSize: u32::try_from(size_of::<GUITHREADINFO>()).expect("small"),
            ..Default::default()
        };
        // SAFETY: thread id 0 = the foreground thread; `info` is a sized out-parameter.
        let focus = if unsafe { GetGUIThreadInfo(0, &raw mut info) } != 0 {
            info.hwndFocus
        } else {
            std::ptr::null_mut()
        };
        let flags = self.shared.flags();
        Target {
            focus: ((fg as usize as u128) << 64) | focus as usize as u128,
            secure_desktop: flags.secure_desktop || !super::input_desktop_available(),
            password_field: flags.password_field || is_password_window(focus),
            elevated: is_elevated_window(fg, self.own_il),
            ime_composing: flags.ime_composing,
            ime_native: ime_mode && ime_native(if focus.is_null() { fg } else { focus }),
            editable: is_editable(focus),
        }
    }
}

const CF_BITMAP: u32 = 2;
const CF_METAFILEPICT: u32 = 3;
const CF_DIB: u32 = 8;
const CF_PALETTE: u32 = 9;
const CF_ENHMETAFILE: u32 = 14;
const CF_DIBV5: u32 = 17;
const CF_UNICODETEXT: u32 = 13;
const CF_OWNERDISPLAY: u32 = 0x80;
const CF_DSPBITMAP: u32 = 0x82;
const CF_DSPMETAFILEPICT: u32 = 0x83;
const CF_DSPENHMETAFILE: u32 = 0x8E;
/// The most clipboard data a copy keeps.
pub const MAX_SNAPSHOT: usize = 32 * 1024 * 1024;

/// The real clipboard (see the module documentation).
#[derive(Debug)]
pub struct WinClipboard {
    owner: isize,
}

impl WinClipboard {
    /// `owner`: the app's hidden window (its thread pumps messages).
    pub fn new(owner: HWND) -> WinClipboard {
        WinClipboard {
            owner: owner as isize,
        }
    }
}

fn available(f: u32) -> bool {
    // SAFETY: no preconditions.
    unsafe { IsClipboardFormatAvailable(f) != 0 }
}

/// Copies a memory block into a new movable global for `SetClipboardData`.
fn global_from(bytes: &[u8]) -> Option<HANDLE> {
    // SAFETY: allocates `len` bytes (at least one); freed below on failure, or owned by the
    // clipboard after a successful SetClipboardData.
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) };
    if h.is_null() {
        return None;
    }
    // SAFETY: `h` is a valid movable global of at least `bytes.len()` bytes.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        // SAFETY: frees the global allocated above.
        unsafe { GlobalFree(h) };
        return None;
    }
    // SAFETY: `p` points to at least `bytes.len()` writable bytes that do not overlap `bytes`.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        GlobalUnlock(h);
    }
    Some(h)
}

fn set(format: u32, bytes: &[u8]) -> bool {
    let Some(h) = global_from(bytes) else {
        return false;
    };
    // SAFETY: the clipboard is open by this thread; on success the clipboard owns `h`.
    if unsafe { SetClipboardData(format, h) }.is_null() {
        // SAFETY: not taken by the clipboard; freed here.
        unsafe { GlobalFree(h) };
        return false;
    }
    true
}

fn registered(name: &str) -> u32 {
    // SAFETY: a NUL-terminated name.
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

/// The documented formats that keep content out of clipboard history and the cloud clipboard.
fn mark_private() {
    let zero = 0u32.to_le_bytes();
    let _ = set(
        registered("ExcludeClipboardContentFromMonitorProcessing"),
        &zero,
    );
    let _ = set(registered("CanIncludeInClipboardHistory"), &zero);
    let _ = set(registered("CanUploadToCloudClipboard"), &zero);
}

fn copy_global(h: HANDLE) -> Option<Vec<u8>> {
    // SAFETY: `h` came from GetClipboardData while the clipboard is open; GlobalSize returns 0
    // for a handle that is not a global.
    let size = unsafe { GlobalSize(h) };
    if size == 0 || size > MAX_SNAPSHOT {
        return None;
    }
    // SAFETY: as above.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` points to `size` readable bytes until GlobalUnlock.
    let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size).to_vec() };
    // SAFETY: unlocks the lock taken above.
    unsafe { GlobalUnlock(h) };
    Some(bytes)
}

impl Clipboard for WinClipboard {
    fn open(&mut self) -> bool {
        // SAFETY: the app's own window; fails while another program has the clipboard open.
        unsafe { OpenClipboard(self.owner as HWND) != 0 }
    }

    fn close(&mut self) {
        // SAFETY: closes the clipboard this thread opened.
        unsafe { CloseClipboard() };
    }

    fn snapshot(&mut self) -> Result<Snapshot, ClipError> {
        let has_dib = available(CF_DIB) || available(CF_DIBV5);
        let has_emf = available(CF_ENHMETAFILE);
        let mut out = Snapshot::default();
        let mut total = 0usize;
        // SAFETY: enumerates the open clipboard's formats (0 starts, 0 ends).
        let mut f = unsafe { EnumClipboardFormats(0) };
        while f != 0 {
            match f {
                // Windows makes these from the DIB or the metafile again.
                CF_BITMAP | CF_PALETTE if has_dib => {}
                CF_METAFILEPICT if has_emf => {}
                CF_ENHMETAFILE => {
                    // SAFETY: a metafile handle owned by the clipboard while it is open.
                    let h = unsafe { GetClipboardData(f) } as HENHMETAFILE;
                    if h.is_null() {
                        return Err(ClipError::NotRestorable);
                    }
                    // SAFETY: size query, then a copy into a buffer of that size.
                    let size = unsafe { GetEnhMetaFileBits(h, 0, std::ptr::null_mut()) };
                    let n = usize::try_from(size).unwrap_or(0);
                    if n == 0 || total + n > MAX_SNAPSHOT {
                        return Err(ClipError::NotRestorable);
                    }
                    let mut buf = vec![0u8; n];
                    // SAFETY: `buf` has `size` writable bytes.
                    unsafe { GetEnhMetaFileBits(h, size, buf.as_mut_ptr()) };
                    total += n;
                    out.formats.push((f, ClipData::Metafile(buf)));
                }
                CF_BITMAP
                | CF_METAFILEPICT
                | CF_PALETTE
                | CF_OWNERDISPLAY
                | CF_DSPBITMAP
                | CF_DSPMETAFILEPICT
                | CF_DSPENHMETAFILE
                | 0x0200..=0x03FF => {
                    return Err(ClipError::NotRestorable);
                }
                _ => {
                    // SAFETY: the clipboard is open; delayed formats are rendered by their owner.
                    let h = unsafe { GetClipboardData(f) };
                    let bytes = (!h.is_null())
                        .then(|| copy_global(h))
                        .flatten()
                        .ok_or(ClipError::NotRestorable)?;
                    total += bytes.len();
                    if total > MAX_SNAPSHOT {
                        return Err(ClipError::NotRestorable);
                    }
                    out.formats.push((f, ClipData::Bytes(bytes)));
                }
            }
            // SAFETY: continues the enumeration of the open clipboard.
            f = unsafe { EnumClipboardFormats(f) };
        }
        Ok(out)
    }

    fn set_text(&mut self, text: &str) -> bool {
        // Clipboard text uses CRLF line ends and a terminating NUL.
        let mut units: Vec<u16> = text.replace('\n', "\r\n").encode_utf16().collect();
        units.push(0);
        let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
        // SAFETY: the clipboard is open by this thread (with an owner window).
        if unsafe { EmptyClipboard() } == 0 {
            return false;
        }
        let ok = set(CF_UNICODETEXT, &bytes);
        mark_private();
        ok
    }

    fn restore(&mut self, s: &Snapshot) -> bool {
        // SAFETY: the clipboard is open by this thread.
        if unsafe { EmptyClipboard() } == 0 {
            return false;
        }
        let mut ok = true;
        for (f, data) in &s.formats {
            ok &= match data {
                ClipData::Bytes(b) => set(*f, b),
                ClipData::Metafile(b) => {
                    // SAFETY: builds a metafile from the copied bits.
                    let h = unsafe {
                        SetEnhMetaFileBits(u32::try_from(b.len()).unwrap_or(0), b.as_ptr())
                    };
                    if h.is_null() {
                        false
                    } else if
                    // SAFETY: the clipboard is open; on success it owns the metafile.
                    unsafe { SetClipboardData(*f, h as HANDLE) }.is_null() {
                        // SAFETY: the clipboard did not take the metafile; it is deleted here.
                        unsafe { DeleteEnhMetaFile(h) };
                        false
                    } else {
                        true
                    }
                }
            };
        }
        if !s.formats.is_empty() {
            mark_private();
        }
        ok
    }

    fn sequence(&mut self) -> u32 {
        // SAFETY: no preconditions.
        unsafe { GetClipboardSequenceNumber() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::wide;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, ES_PASSWORD, HWND_MESSAGE, WS_CHILD, WS_DISABLED,
    };

    fn window(class: &str, style: u32) -> (HWND, HWND) {
        // SAFETY: a hidden message-only parent and a child without WS_VISIBLE: never shown.
        unsafe {
            let parent = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            let h = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                std::ptr::null(),
                WS_CHILD | style,
                0,
                0,
                10,
                10,
                parent,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            assert!(!parent.is_null() && !h.is_null());
            (parent, h)
        }
    }

    #[test]
    fn disabled_read_only_and_password_fields_are_recognized_without_showing_anything() {
        let edit = window("EDIT", 0);
        let ro = window("EDIT", ES_READONLY as u32);
        let off = window("EDIT", WS_DISABLED);
        let pw = window("EDIT", ES_PASSWORD as u32);
        assert!(is_editable(edit.1));
        assert!(!is_editable(ro.1), "read-only");
        assert!(!is_editable(off.1), "disabled");
        assert!(!is_editable(std::ptr::null_mut()), "nothing has focus");
        assert!(is_editable(pw.1) && is_password_window(pw.1));
        // SAFETY: windows created by this thread.
        unsafe {
            for (p, h) in [edit, ro, off, pw] {
                DestroyWindow(h);
                DestroyWindow(p);
            }
        }
    }

    #[test]
    fn the_local_clock_reads_a_plausible_time() {
        let t = local_time();
        assert!((2024..2200).contains(&t.year));
        assert!((1..=12).contains(&t.month) && (1..=31).contains(&t.day));
        assert!(t.hour < 24 && t.minute < 60);
    }

    #[test]
    fn translation_needs_no_window_and_never_types() {
        // A pure table lookup for the foreground layout: Space is a space on every layout, a
        // navigation key is nothing. Nothing is injected and no keyboard state changes.
        assert_eq!(translate(0x20, 0x39, false, false), Typed::Char(' '));
        assert_eq!(translate(0x25, 0x4B, false, false), Typed::Nothing);
        assert_eq!(translate(0x0D, 0x1C, false, false), Typed::Char('\r'));
    }
}
