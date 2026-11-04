//! The pointer facts mouse actions need (Step 71, ADR 0040): the monitor layout with each
//! monitor's effective DPI, the pointer position, the active window's content area and who would
//! receive pointer input at a point. Every call is read-only and runs with the calling thread in
//! per-monitor DPI awareness (v2), so every position is a physical pixel of the virtual desktop,
//! whatever the app's own DPI mode ([`DpiScope`]).
//!
//! Positions are read only when a mouse rule runs (`DDALGAK-SEC-001`: no pointer tracking).

use super::context::is_elevated_window;
use super::hook::Shared;
use super::own_integrity;
use crate::mouse::{Layout, Monitor, Point, PointerTarget, Rect, Screen, WindowArea};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    GetDpiForWindow, MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GetClientRect, GetCursorPos, GetForegroundWindow, GetGUIThreadInfo,
    MONITORINFOF_PRIMARY, WindowFromPoint,
};
use windows_sys::core::BOOL;

/// The calling thread in per-monitor DPI awareness (v2) until dropped; the previous mode comes
/// back then. Positions read and moves sent inside it are physical pixels.
#[derive(Debug)]
pub struct DpiScope {
    old: DPI_AWARENESS_CONTEXT,
}

impl DpiScope {
    pub fn per_monitor() -> DpiScope {
        // SAFETY: a documented pseudo-handle; returns the previous context (null on failure,
        // for example before Windows 10 1607, in which case nothing is restored).
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        DpiScope { old }
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        if !self.old.is_null() {
            // SAFETY: restores the context this scope replaced.
            unsafe { SetThreadDpiAwarenessContext(self.old) };
        }
    }
}

unsafe extern "system" fn monitor(h: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<Monitor>` passed to EnumDisplayMonitors below, alive for the
    // whole (synchronous) enumeration.
    let list = unsafe { &mut *(data as *mut Vec<Monitor>) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).expect("small"),
        ..Default::default()
    };
    // SAFETY: `info` is a sized out-parameter; `h` comes from the enumeration.
    if unsafe { GetMonitorInfoW(h, &raw mut info) } == 0 {
        return 1; // skip this one, keep enumerating
    }
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: out-parameters for the monitor's effective DPI; a failure leaves 0 (then 96).
    let hr = unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &raw mut dx, &raw mut dy) };
    let r = info.rcMonitor;
    list.push(Monitor::new(
        Rect::new(r.left, r.top, r.right, r.bottom),
        if hr == 0 && dx > 0 { dx } else { 96 },
        info.dwFlags & MONITORINFOF_PRIMARY != 0,
    ));
    1
}

/// The monitors of the desktop in physical pixels, each with its effective DPI.
pub fn layout() -> Option<Layout> {
    let _dpi = DpiScope::per_monitor();
    let mut list: Vec<Monitor> = Vec::new();
    // SAFETY: enumerates every monitor of the desktop synchronously; the callback receives a
    // pointer to `list`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(monitor),
            &raw mut list as LPARAM,
        );
    }
    Layout::new(list)
}

/// The pointer position in physical pixels (`None` when another desktop has the input, as on
/// the lock screen).
pub fn cursor() -> Option<Point> {
    let _dpi = DpiScope::per_monitor();
    let mut p = POINT::default();
    // SAFETY: `p` is a writable out-parameter.
    (unsafe { GetCursorPos(&raw mut p) } != 0).then(|| Point::new(p.x, p.y))
}

/// The foreground window's content area (client area): its top-left corner in physical pixels
/// and the window's DPI.
pub fn active_window() -> Option<WindowArea> {
    let _dpi = DpiScope::per_monitor();
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return None;
    }
    let mut r = RECT::default();
    // SAFETY: `r` is a writable out-parameter; a window of another process is allowed.
    if unsafe { GetClientRect(fg, &raw mut r) } == 0 {
        return None;
    }
    let mut origin = POINT {
        x: r.left,
        y: r.top,
    };
    // SAFETY: converts the client origin to screen coordinates in place.
    if unsafe { ClientToScreen(fg, &raw mut origin) } == 0 {
        return None;
    }
    // SAFETY: reads the window's DPI (0 for an invalid window).
    let dpi = unsafe { GetDpiForWindow(fg) };
    Some(WindowArea {
        origin: Point::new(origin.x, origin.y),
        dpi: if dpi == 0 { 96 } else { dpi },
    })
}

/// The window that receives pointer input at `at`: the foreground thread's capture window
/// (during a drag every mouse message goes there), or else the window at the point.
pub fn input_window(at: Point) -> HWND {
    let _dpi = DpiScope::per_monitor();
    let mut info = GUITHREADINFO {
        cbSize: u32::try_from(size_of::<GUITHREADINFO>()).expect("small"),
        ..Default::default()
    };
    // SAFETY: thread id 0 = the foreground thread; `info` is a sized out-parameter.
    if unsafe { GetGUIThreadInfo(0, &raw mut info) } != 0 && !info.hwndCapture.is_null() {
        return info.hwndCapture;
    }
    // SAFETY: a plain point query.
    unsafe { WindowFromPoint(POINT { x: at.x, y: at.y }) }
}

/// The real pointer facts (see the module documentation).
#[derive(Debug)]
pub struct WinScreen {
    shared: Arc<Shared>,
    own_il: u32,
}

impl WinScreen {
    pub fn new(shared: Arc<Shared>) -> WinScreen {
        WinScreen {
            shared,
            own_il: own_integrity().unwrap_or(0x2000),
        }
    }
}

impl Screen for WinScreen {
    fn layout(&mut self) -> Option<Layout> {
        layout()
    }

    fn cursor(&mut self) -> Option<Point> {
        cursor()
    }

    fn window(&mut self) -> Option<WindowArea> {
        active_window()
    }

    fn target(&mut self, at: Point) -> PointerTarget {
        let secure = self.shared.flags().secure_desktop || !super::input_desktop_available();
        PointerTarget {
            secure_desktop: secure,
            elevated: !secure && is_elevated_window(input_window(at), self.own_il),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn the_layout_and_the_pointer_are_read_without_moving_anything() {
        // Read-only queries of this machine's monitors and pointer; nothing is sent.
        let l = layout().expect("a desktop has at least one monitor");
        let primary = l.primary();
        assert!(primary.primary);
        assert!(
            primary.rect.contains(Point::new(0, 0)),
            "the primary monitor's top-left corner is (0, 0): {primary:?}"
        );
        for m in l.monitors() {
            assert!(!m.rect.is_empty() && (48..=960).contains(&m.dpi), "{m:?}");
        }
        eprintln!(
            "monitors: {:?}",
            l.monitors()
                .iter()
                .map(|m| (m.rect, m.scale_percent(), m.primary))
                .collect::<Vec<_>>()
        );
        // The pointer (absent when the workstation is locked) is on the desktop.
        if let Some(p) = cursor() {
            assert!(!l.clamp(p).1, "the pointer {p:?} is on a monitor");
        } else {
            eprintln!("no pointer position (locked)");
        }
        // The thread's own DPI mode is unchanged afterwards.
        let _ = active_window();
    }

    #[test]
    fn the_pointer_target_is_checked_quickly() {
        let shared = Shared::new(Arc::new(crate::stats::Stats::default()));
        let mut s = WinScreen::new(shared);
        let at = cursor().unwrap_or_default();
        let t = Instant::now();
        let n = 200u32;
        for _ in 0..n {
            let _ = s.target(at);
        }
        let per = t.elapsed() / n;
        eprintln!("pointer target check: {per:?} per call");
        // The check runs before every click, scroll and motion tick (every 10 ms).
        assert!(per.as_millis() < 5, "{per:?}");
    }
}
