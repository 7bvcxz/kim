//! The Win32 notification-area icon (NOEUL-FR-004): `Shell_NotifyIconW` with version 4
//! callbacks, a popup menu rendered from [`crate::tray::menu`], and balloon notices.
//!
//! **Not exercised by automated tests**: creating a real icon would show it on the user's
//! taskbar. The menu model and the core's reactions are tested headlessly (`tray`, `app`); the
//! manual check is in docs/adr/0023-noeul-app-foundation.md. The icon is drawn in code (an
//! original evening-glow disc); no image asset is shipped.

use crate::tray::{Click, Entry, MenuItem};
use crate::win::wide;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_NOSOUND, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    HICON, ICONINFO, MF_CHECKED, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_CONTEXTMENU, WM_LBUTTONUP,
    WM_MBUTTONUP, WM_NULL,
};

const ICON_ID: u32 = 1;
/// `NIN_SELECT | NINF_KEY` (the icon chosen with the keyboard; documented value).
const NIN_KEYSELECT: u32 = NIN_SELECT | 0x1;

/// One icon owned by the instance's control window.
#[derive(Debug)]
pub struct TrayIcon {
    hwnd: HWND,
    icon: HICON,
    message: u32,
    added: bool,
}

fn copy_text(dst: &mut [u16], text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

/// Draws the 32×32 icon: a warm disc fading from gold to rose, transparent outside.
fn make_icon() -> HICON {
    const N: i32 = 32;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
            biWidth: N,
            biHeight: -N,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    // SAFETY: a 32×32 32-bit DIB; `bits` receives its pixels.
    let color = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if color.is_null() || bits.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: the DIB holds N×N 32-bit pixels.
    let px = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), (N * N * 4) as usize) };
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (f64::from(x) - 15.5, f64::from(y) - 15.5);
            let d = (dx * dx + dy * dy).sqrt();
            let a = (14.5 - d).clamp(0.0, 1.0);
            let t = f64::from(y) / f64::from(N - 1);
            // Gold (#F6B44B) at the top to rose (#E0566B) at the bottom.
            let (r, g, b) = (
                246.0 + (224.0 - 246.0) * t,
                180.0 + (86.0 - 180.0) * t,
                75.0 + (107.0 - 75.0) * t,
            );
            let i = ((y * N + x) * 4) as usize;
            let pm = |c: f64| (c * a).round() as u8;
            px[i..i + 4].copy_from_slice(&[pm(b), pm(g), pm(r), (a * 255.0).round() as u8]);
        }
    }
    // SAFETY: an all-zero monochrome mask (the color bitmap's alpha decides).
    let mask = unsafe { CreateBitmap(N, N, 1, 1, std::ptr::null()) };
    let ii = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    // SAFETY: both bitmaps are valid; the icon copies them, so they are deleted right after.
    let icon = unsafe { CreateIconIndirect(&raw const ii) };
    // SAFETY: as above.
    unsafe {
        DeleteObject(color);
        DeleteObject(mask);
    }
    icon
}

impl TrayIcon {
    fn data(&self) -> NOTIFYICONDATAW {
        let mut d = NOTIFYICONDATAW {
            cbSize: u32::try_from(size_of::<NOTIFYICONDATAW>()).expect("small"),
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        d.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        d
    }

    /// Adds the icon (`message` arrives at `hwnd` for clicks).
    pub fn add(hwnd: HWND, message: u32) -> Result<TrayIcon, String> {
        let mut t = TrayIcon {
            hwnd,
            icon: make_icon(),
            message,
            added: false,
        };
        t.show()?;
        Ok(t)
    }

    fn show(&mut self) -> Result<(), String> {
        let mut d = self.data();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = self.message;
        d.hIcon = self.icon;
        copy_text(&mut d.szTip, "Noeul — F3 pins the clipboard");
        // SAFETY: `d` is fully initialized and names our window.
        if unsafe { Shell_NotifyIconW(NIM_ADD, &raw const d) } == 0 {
            return Err("Shell_NotifyIconW(NIM_ADD) failed".into());
        }
        // SAFETY: as above; selects version 4 callbacks.
        unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw const d) };
        self.added = true;
        Ok(())
    }

    /// Explorer restarted (`TaskbarCreated`): add the icon again.
    pub fn readd(&mut self) {
        self.added = false;
        let _ = self.show();
    }

    pub fn shown(&self) -> bool {
        self.added
    }

    /// A short notice next to the icon, without sound.
    pub fn balloon(&self, text: &str) {
        if !self.added {
            return;
        }
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_NOSOUND;
        copy_text(&mut d.szInfoTitle, "Noeul");
        copy_text(&mut d.szInfo, text);
        // SAFETY: `d` is fully initialized and names our icon.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw const d) };
    }

    pub fn remove(&mut self) {
        if self.added {
            let d = self.data();
            // SAFETY: removes our own icon.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &raw const d) };
            self.added = false;
        }
    }

    /// Decodes a version-4 callback (`lParam` low word is the event).
    pub fn click(lparam: isize) -> Option<Click> {
        match (lparam & 0xFFFF) as u32 {
            WM_LBUTTONUP | NIN_SELECT | NIN_KEYSELECT => Some(Click::Left),
            WM_MBUTTONUP => Some(Click::Middle),
            WM_CONTEXTMENU => Some(Click::Right),
            _ => None,
        }
    }

    /// Shows the menu at the pointer and returns the chosen item (modal; call it while the
    /// instance core is not borrowed).
    pub fn menu(&self, items: &[Entry]) -> Option<MenuItem> {
        // SAFETY: a new popup menu, destroyed below.
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            return None;
        }
        let mut groups = false;
        for (i, e) in items.iter().enumerate() {
            let is_group = matches!(e.item, MenuItem::Group(_));
            if (e.item == MenuItem::Quit && i > 0) || (is_group && !groups) {
                // SAFETY: valid menu handle.
                unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null()) };
            }
            groups |= is_group;
            let label = wide(&e.label);
            let flags = if e.checked {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            // SAFETY: valid menu; the label is copied.
            unsafe { AppendMenuW(menu, flags, e.item.command_id() as usize, label.as_ptr()) };
        }
        let mut pt = POINT::default();
        // SAFETY: documented requirement for tray menus: our window is foreground while the menu
        // is open, and a WM_NULL afterwards lets it close correctly.
        let cmd = unsafe {
            GetCursorPos(&raw mut pt);
            SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                std::ptr::null(),
            );
            PostMessageW(self.hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
            cmd
        };
        let cmd = u32::try_from(cmd).ok()?;
        items.iter().map(|e| e.item).find(|i| i.command_id() == cmd)
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        self.remove();
        if !self.icon.is_null() {
            // SAFETY: the icon created by `make_icon`, destroyed once.
            unsafe { DestroyIcon(self.icon) };
        }
    }
}
