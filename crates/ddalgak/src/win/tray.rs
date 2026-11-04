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
//! **Modes** (Step 72, ADR 0043): when the mode indicator is visible (`settings.mode_indicator`:
//! by default only for a profile with more than one mode), the tooltip names the active mode
//! ("Ddalgak: engine on · mode N Navigation"), a running engine outside its default mode has its
//! own icon shape (the disc with a notch), and the menu has a `Mode` submenu with every mode (the
//! active one checked) and "Back to <default>", the safe reset. The tray is never shown by the
//! automated tests (the test host has no tray); its texts and shapes are unit-tested.
//!
//! **Step 73** (ADR 0046): the icon and tooltip come from [`crate::indicator`]: after an
//! emergency off, a lost hook or an unclean exit the ring is struck through until the engine is
//! turned on again, and the tooltip names what needs attention in words (a disabled profile, a
//! shortcut another program owns, a locked session). The menu adds *Release stuck keys*. Step 74
//! adds the editor and draws the optional floating badge.

use super::wide;
use crate::indicator::{self, Shape};
use crate::lifecycle::State;
use crate::modes::MenuEntry;
use std::io;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, HICON,
    MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_NULL,
};

/// A menu choice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MenuCmd {
    ToggleEngine,
    TogglePause,
    Quit,
    /// Switch to this mode (Step 72).
    Mode(String),
    /// Back to the default mode (the safe reset).
    ResetMode,
    /// Step 73: release every key Ddalgak holds and stale keys Windows still considers pressed.
    ReleaseKeys,
    /// Step 74: start the rule editor (`ddalgak-editor.exe`, ADR 0049).
    OpenEditor,
}

/// Step 73: off after an emergency, a lost hook or an unclean exit: the ring struck through
/// (DDALGAK_GUI_JOURNEYS J8), so it differs from a plain "off" by shape.
const OFF_ALERT: [&str; 16] = [
    "................",
    ".....++++++..++.",
    "...++######++##+",
    "..+##++++++#####",
    "..+#+.....+###+.",
    ".+#+.....+###+#+",
    ".+#+....+###+.#+",
    ".+#+...+###+..#+",
    ".+#+..+###+...#+",
    ".+#+.+###+....#+",
    ".+#++###+.....#+",
    "..+####+.....+#+",
    ".+####+++++++#+.",
    "+##++######++...",
    ".++..++++++.....",
    "................",
];

/// The menu IDs of mode entries start here (one per mode, at most 32).
const MODE_ID: usize = 100;
const RESET_MODE_ID: usize = 99;

/// The running engine outside its default mode: the disc with a notch at the top right, so the
/// state is visible by shape, not by color (Step 72).
const ON_MODE: [&str; 16] = [
    "................",
    ".....++++++.....",
    "...++#####+.....",
    "..+######+..+...",
    "..+#####+..+#+..",
    ".+######+.+##+..",
    ".+#######+###+..",
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

/// The state icons (off, on, paused, on outside the default mode, off needing attention).
#[derive(Debug)]
pub struct Icons([HICON; 5]);

impl Icons {
    pub fn new() -> io::Result<Icons> {
        Ok(Icons([
            make_icon(&OFF)?,
            make_icon(&ON)?,
            make_icon(&PAUSED)?,
            make_icon(&ON_MODE)?,
            make_icon(&OFF_ALERT)?,
        ]))
    }

    pub fn for_state(&self, s: State) -> HICON {
        self.for_state_in_mode(s, false)
    }

    /// `away`: a mode other than the default mode is active and the indicator is visible.
    pub fn for_state_in_mode(&self, s: State, away: bool) -> HICON {
        match s {
            State::Off => self.0[0],
            State::Running if away => self.0[3],
            State::Running => self.0[1],
            State::Paused => self.0[2],
        }
    }

    /// The icon of an indicator shape (Step 73).
    pub fn for_shape(&self, s: Shape) -> HICON {
        match s {
            Shape::Off => self.0[0],
            Shape::Running => self.0[1],
            Shape::Paused => self.0[2],
            Shape::RunningAway => self.0[3],
            Shape::OffAlert => self.0[4],
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
    /// What the icon and tooltip show (Step 73: [`crate::indicator`], which includes the mode
    /// of Step 72).
    status: indicator::Status,
    added: bool,
}

const ID: u32 = 1;

impl Tray {
    /// Adds the icon. `callback` is the window message the shell sends for mouse events.
    pub fn new(hwnd: HWND, callback: u32, status: indicator::Status) -> io::Result<Tray> {
        let mut t = Tray {
            hwnd,
            callback,
            icons: Icons::new()?,
            status,
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
            hIcon: self.icons.for_shape(self.status.shape),
            ..Default::default()
        };
        copy_into(&mut d.szTip, &self.status.tooltip);
        d
    }

    /// Shows a new state, mode or notice (event-driven: the host calls it after a change).
    pub fn set(&mut self, status: indicator::Status) {
        if self.status == status {
            return;
        }
        self.status = status;
        let d = self.data();
        // SAFETY: as in `add`.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) };
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

    /// Shows the menu at the pointer and returns the choice. `modes`: the mode menu entries and
    /// the default mode's title, when the engine is on and the indicator is visible (Step 72).
    pub(crate) fn menu(
        hwnd: HWND,
        state: State,
        title: &str,
        modes: Option<(&[MenuEntry], &str)>,
    ) -> Option<MenuCmd> {
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
        item(9, title, false);
        if let Some((entries, default_title)) = modes
            && !entries.is_empty()
        {
            // SAFETY: a new popup menu; owned by `menu` once appended, destroyed with it.
            let sub = unsafe { CreatePopupMenu() };
            if !sub.is_null() {
                for (i, e) in entries.iter().enumerate().take(32) {
                    let w = wide(&e.label);
                    let flags = MF_STRING | if e.checked { MF_CHECKED } else { 0 };
                    // SAFETY: valid submenu; NUL-terminated text copied by the call.
                    unsafe { AppendMenuW(sub, flags, MODE_ID + i, w.as_ptr()) };
                }
                let w = wide("Mode");
                // SAFETY: valid menus; the submenu handle is passed as the item ID (MF_POPUP).
                unsafe { AppendMenuW(menu, MF_STRING | MF_POPUP, sub as usize, w.as_ptr()) };
            }
            item(
                RESET_MODE_ID,
                &format!("Back to {default_title}"),
                entries
                    .iter()
                    .any(|e| e.checked && !e.label.ends_with("(default)")),
            );
        }
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
        // Step 73: the way out of a stuck key, also while the engine is off.
        item(4, "Release stuck keys", true);
        // Step 74: the rule editor, its own process (ADR 0049).
        item(5, "Open rule editor", true);
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
        let cmd = usize::try_from(cmd).unwrap_or(0);
        match cmd {
            1 => Some(MenuCmd::ToggleEngine),
            2 => Some(MenuCmd::TogglePause),
            3 => Some(MenuCmd::Quit),
            4 => Some(MenuCmd::ReleaseKeys),
            5 => Some(MenuCmd::OpenEditor),
            RESET_MODE_ID => Some(MenuCmd::ResetMode),
            n if n >= MODE_ID => modes
                .and_then(|(e, _)| e.get(n - MODE_ID))
                .map(|e| MenuCmd::Mode(e.id.clone())),
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
        let shapes = [
            masks(&ON),
            masks(&PAUSED),
            masks(&OFF),
            masks(&ON_MODE),
            masks(&OFF_ALERT),
        ];
        for (i, a) in shapes.iter().enumerate() {
            for b in &shapes[i + 1..] {
                assert_ne!(a, b, "each state has its own shape");
            }
            // Every icon has transparent, white and black pixels.
            assert!(a.0.iter().any(|b| *b != 0) && a.1.iter().any(|b| *b != 0));
        }
        for art in [ON, PAUSED, OFF, ON_MODE, OFF_ALERT] {
            assert!(art.iter().all(|r| r.len() == 16));
        }
        // Step 73: every indicator shape has its own icon.
        let icons = Icons::new().unwrap();
        let all = [
            Shape::Off,
            Shape::OffAlert,
            Shape::Running,
            Shape::RunningAway,
            Shape::Paused,
        ]
        .map(|s| icons.for_shape(s));
        for (i, a) in all.iter().enumerate() {
            assert!(!all[i + 1..].contains(a));
        }
        // CreateIcon only builds the images; no icon is added to the notification area here.
        let icons = Icons::new().unwrap();
        assert_ne!(icons.for_state(State::Off), icons.for_state(State::Running));
        assert_ne!(
            icons.for_state_in_mode(State::Running, true),
            icons.for_state(State::Running),
            "outside the default mode the running icon has its own shape"
        );
        assert_eq!(
            icons.for_state_in_mode(State::Paused, true),
            icons.for_state(State::Paused)
        );
        assert_eq!(tooltip(State::Paused), "Ddalgak: engine paused");
    }
}
