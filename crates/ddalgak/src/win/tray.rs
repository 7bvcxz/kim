//! The tray icon: the engine is visible whenever the app runs (`DDALGAK-SEC-010`,
//! `DDALGAK-FR-002`).
//!
//! The state is shown by **shape and text**, never by color alone: a filled disc (on), two bars
//! (paused), a ring (off), and the tooltip "Ddalgak: engine on/paused/off". The menu turns the
//! engine on or off, pauses or resumes it, and quits. The icons are 16×16 monochrome images built
//! from the masks below (no image files, no GUI toolkit). Opening the menu is the one place where
//! Ddalgak brings its own (hidden) window to the foreground, as Windows requires for a popup menu
//! the user asked for with a click; nothing else ever takes focus.
//!
//! Step 73 refines the indicator (optional compact indicator, notices); Step 74 adds the editor.

use super::wide;
use crate::lifecycle::State;
use std::io;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, HICON,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
};

/// A menu choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuCmd {
    ToggleEngine,
    TogglePause,
    Quit,
}

const ON: [&str; 16] = [
    "................",
    ".....++++++.....",
    "...++######++...",
    "..+##########+..",
    "..+##########+..",
    ".+############+.",
    ".+############+.",
    ".+############+.",
    ".+############+.",
    ".+############+.",
    ".+############+.",
    "..+##########+..",
    "..+##########+..",
    "...++######++...",
    ".....++++++.....",
    "................",
];

const PAUSED: [&str; 16] = [
    "................",
    "..++++....++++..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..+##+....+##+..",
    "..++++....++++..",
    "................",
];

const OFF: [&str; 16] = [
    "................",
    ".....++++++.....",
    "...++######++...",
    "..+##++++++##+..",
    "..+#+......+#+..",
    ".+#+........+#+.",
    ".+#+........+#+.",
    ".+#+........+#+.",
    ".+#+........+#+.",
    ".+#+........+#+.",
    ".+#+........+#+.",
    "..+#+......+#+..",
    "..+##++++++##+..",
    "...++######++...",
    ".....++++++.....",
    "................",
];

/// AND and XOR masks of a 16×16 monochrome icon: `#` white, `+` black, `.` transparent.
pub fn masks(art: &[&str; 16]) -> ([u8; 32], [u8; 32]) {
    let mut and = [0u8; 32];
    let mut xor = [0u8; 32];
    for (y, row) in art.iter().enumerate() {
        for (x, c) in row.bytes().enumerate().take(16) {
            let (byte, bit) = (y * 2 + x / 8, 7 - (x % 8));
            match c {
                b'.' => and[byte] |= 1 << bit,
                b'#' => xor[byte] |= 1 << bit,
                _ => {}
            }
        }
    }
    (and, xor)
}

fn make_icon(art: &[&str; 16]) -> io::Result<HICON> {
    let (and, xor) = masks(art);
    // SAFETY: the module handle of this executable; masks hold 16 rows of 2 bytes each.
    let h = unsafe {
        CreateIcon(
            GetModuleHandleW(std::ptr::null()),
            16,
            16,
            1,
            1,
            and.as_ptr(),
            xor.as_ptr(),
        )
    };
    if h.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(h)
    }
}

/// The three state icons.
#[derive(Debug)]
pub struct Icons([HICON; 3]);

impl Icons {
    pub fn new() -> io::Result<Icons> {
        Ok(Icons([
            make_icon(&OFF)?,
            make_icon(&ON)?,
            make_icon(&PAUSED)?,
        ]))
    }

    pub fn for_state(&self, s: State) -> HICON {
        match s {
            State::Off => self.0[0],
            State::Running => self.0[1],
            State::Paused => self.0[2],
        }
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for h in self.0 {
            // SAFETY: icons created by CreateIcon above, destroyed once.
            unsafe { DestroyIcon(h) };
        }
    }
}

/// The tooltip text for a state.
pub fn tooltip(s: State) -> &'static str {
    match s {
        State::Off => "Ddalgak: engine off",
        State::Running => "Ddalgak: engine on",
        State::Paused => "Ddalgak: engine paused",
    }
}

fn copy_into(dst: &mut [u16], text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// The notification-area icon of the app window `hwnd`.
#[derive(Debug)]
pub struct Tray {
    hwnd: HWND,
    callback: u32,
    icons: Icons,
    state: State,
    added: bool,
}

const ID: u32 = 1;

impl Tray {
    /// Adds the icon. `callback` is the window message the shell sends for mouse events.
    pub fn new(hwnd: HWND, callback: u32, state: State) -> io::Result<Tray> {
        let mut t = Tray {
            hwnd,
            callback,
            icons: Icons::new()?,
            state,
            added: false,
        };
        t.add()?;
        Ok(t)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).expect("small"),
            hWnd: self.hwnd,
            uID: ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: self.callback,
            hIcon: self.icons.for_state(self.state),
            ..Default::default()
        };
        copy_into(&mut d.szTip, tooltip(self.state));
        d
    }

    fn add(&mut self) -> io::Result<()> {
        let d = self.data();
        // SAFETY: `d` is a fully initialized NOTIFYICONDATAW of the declared size.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &raw const d) } == 0 {
            return Err(io::Error::other("the notification area refused the icon"));
        }
        self.added = true;
        Ok(())
    }

    /// Shows the new state.
    pub fn update(&mut self, state: State) {
        self.state = state;
        let d = self.data();
        // SAFETY: as in `add`.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) };
    }

    /// Adds the icon again after Explorer restarted (`TaskbarCreated`).
    pub fn readd(&mut self) {
        let _ = self.add();
    }

    /// A non-modal notice in the notification area (no window, no focus change).
    pub fn notify(&self, title: &str, text: &str) {
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        // SAFETY: as in `add`.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) };
    }

    /// Shows the menu at the pointer and returns the choice.
    pub(crate) fn menu(hwnd: HWND, state: State) -> Option<MenuCmd> {
        // SAFETY: a new popup menu, destroyed below.
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return None;
        }
        let item = |id: usize, text: &str, enabled: bool| {
            let w = wide(text);
            let flags = MF_STRING | if enabled { 0 } else { MF_GRAYED };
            // SAFETY: valid menu; NUL-terminated text copied by the call.
            unsafe { AppendMenuW(menu, flags, id, w.as_ptr()) };
        };
        item(9, tooltip(state), false);
        // SAFETY: valid menu; a separator has no text.
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
        item(
            1,
            if state == State::Off {
                "Turn the engine on"
            } else {
                "Turn the engine off"
            },
            true,
        );
        item(
            2,
            if state == State::Paused {
                "Resume rules"
            } else {
                "Pause rules"
            },
            state != State::Off,
        );
        // SAFETY: as above.
        unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
        item(3, "Quit Ddalgak", true);
        let mut pt = POINT::default();
        // SAFETY: `pt` is writable; the menu needs its owner in the foreground (documented for
        // TrackPopupMenu), and WM_NULL afterwards lets it close correctly.
        let cmd = unsafe {
            GetCursorPos(&raw mut pt);
            SetForegroundWindow(hwnd);
            let c = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            c
        };
        match cmd {
            1 => Some(MenuCmd::ToggleEngine),
            2 => Some(MenuCmd::TogglePause),
            3 => Some(MenuCmd::Quit),
            _ => None,
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.added {
            let d = self.data();
            // SAFETY: removes the icon this value added.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const d) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_differ_by_shape_and_are_created_without_showing_anything() {
        let shapes = [masks(&ON), masks(&PAUSED), masks(&OFF)];
        for (i, a) in shapes.iter().enumerate() {
            for b in &shapes[i + 1..] {
                assert_ne!(a, b, "each state has its own shape");
            }
            // Every icon has transparent, white and black pixels.
            assert!(a.0.iter().any(|b| *b != 0) && a.1.iter().any(|b| *b != 0));
        }
        for art in [ON, PAUSED, OFF] {
            assert!(art.iter().all(|r| r.len() == 16));
        }
        // CreateIcon only builds the images; no icon is added to the notification area here.
        let icons = Icons::new().unwrap();
        assert_ne!(icons.for_state(State::Off), icons.for_state(State::Running));
        assert_eq!(tooltip(State::Paused), "Ddalgak: engine paused");
    }
}
