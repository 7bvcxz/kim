//! The Windows capture backend: GDI (ADR 0023 §2).
//!
//! - **Desktop state** (NOEUL-SEC-003, NOEUL_UX_SPEC §7), checked before every capture and on
//!   every hotkey: the process's window station must be interactive (`WSF_VISIBLE`), the session
//!   must not be a disconnected remote session (`WTSConnectState`), the input desktop must be
//!   this user's `Default` desktop (`OpenInputDesktop` fails or names `Winlogon` while the
//!   session is locked or a secure desktop is shown), and at least one monitor must exist.
//! - **Capture**: `BitBlt` with `CAPTUREBLT` from the screen DC into a 32-bit top-down DIB
//!   section, copied into an owned [`Frame`] and released at once; no DC, bitmap or buffer is
//!   kept between captures (NOEUL-NFR-002). Windows that exclude themselves from capture
//!   (display affinity) and DRM content come out exactly as GDI delivers them (black), and are
//!   never worked around (NOEUL-SEC-004).

use crate::capture::{CaptureBackend, CaptureError, Desktop, DesktopState, Monitor, check_region};
use crate::frame::{Frame, Ledger, Rect};
use std::ffi::c_void;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{GetLastError, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GdiFlush, GetDC, GetMonitorInfoW,
    HDC, HMONITOR, MONITORINFO, ReleaseDC, SRCCOPY, SelectObject,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSConnectState, WTSDisconnected,
    WTSFreeMemory, WTSQuerySessionInformationW,
};
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetUserObjectInformationW,
    OpenInputDesktop, UOI_FLAGS, UOI_NAME, USEROBJECTFLAGS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WSF_VISIBLE,
};

/// GDI screen capture. Holds nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct Gdi;

/// Whether this process runs on an interactive window station (not a service, SSH, task).
fn interactive_station() -> bool {
    // SAFETY: returns a borrowed handle of this process's window station (not to be closed).
    let station = unsafe { GetProcessWindowStation() };
    if station.is_null() {
        return false;
    }
    let mut flags = USEROBJECTFLAGS {
        fInherit: 0,
        fReserved: 0,
        dwFlags: 0,
    };
    let mut needed = 0u32;
    // SAFETY: `flags` is a writable USEROBJECTFLAGS of the size passed.
    let ok = unsafe {
        GetUserObjectInformationW(
            station,
            UOI_FLAGS,
            (&raw mut flags).cast::<c_void>(),
            u32::try_from(size_of::<USEROBJECTFLAGS>()).expect("small"),
            &raw mut needed,
        )
    };
    ok != 0 && flags.dwFlags & (WSF_VISIBLE as u32) != 0
}

/// Whether this remote session's client is disconnected.
fn session_disconnected() -> bool {
    let mut buf: *mut u16 = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: queries this process's own session; `buf` receives a WTS-allocated block.
    let ok = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSConnectState,
            &raw mut buf,
            &raw mut len,
        )
    };
    if ok == 0 || buf.is_null() {
        return false; // unknown: the other checks decide
    }
    let state = if len as usize >= size_of::<i32>() {
        // SAFETY: the documented result is one WTS_CONNECTSTATE_CLASS (an i32) value.
        unsafe { buf.cast::<i32>().read_unaligned() }
    } else {
        -1
    };
    // SAFETY: frees the block WTSQuerySessionInformationW allocated.
    unsafe { WTSFreeMemory(buf.cast::<c_void>()) };
    state == WTSDisconnected
}

/// Whether the input desktop is this session's `Default` desktop (not Winlogon or the
/// screen saver). A process cannot open the secure desktop; that failure means "locked".
fn input_desktop_is_default() -> bool {
    // SAFETY: plain open with read-only access; the handle is closed below.
    let desk = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    if desk.is_null() {
        return false;
    }
    let mut name = [0u16; 64];
    let mut needed = 0u32;
    // SAFETY: `name` is a writable buffer of the size passed (bytes).
    let ok = unsafe {
        GetUserObjectInformationW(
            desk,
            UOI_NAME,
            name.as_mut_ptr().cast::<c_void>(),
            u32::try_from(size_of_val(&name)).expect("small"),
            &raw mut needed,
        )
    };
    // SAFETY: closes the handle opened above.
    unsafe { CloseDesktop(desk) };
    if ok == 0 {
        return false;
    }
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end]).eq_ignore_ascii_case("Default")
}

unsafe extern "system" fn monitor_proc(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
    // SAFETY: `data` is the `&mut Vec<Monitor>` passed to EnumDisplayMonitors below, alive for
    // the whole enumeration, which runs on this thread.
    let out = unsafe { &mut *(data as *mut Vec<Monitor>) };
    let mut info = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).expect("small"),
        ..Default::default()
    };
    // SAFETY: `m` comes from the enumeration; `info` is writable with cbSize set.
    if unsafe { GetMonitorInfoW(m, &raw mut info) } != 0 {
        let r = info.rcMonitor;
        if let (Ok(w), Ok(h)) = (
            u32::try_from(r.right - r.left),
            u32::try_from(r.bottom - r.top),
        ) && w > 0
            && h > 0
        {
            out.push(Monitor {
                rect: Rect::new(r.left, r.top, w, h),
                primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }
    1
}

fn monitors() -> Vec<Monitor> {
    let mut out: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only touches `out`, passed by pointer for the duration of the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(monitor_proc),
            (&raw mut out) as LPARAM,
        )
    };
    out
}

fn virtual_rect() -> Option<Rect> {
    // SAFETY: plain metric queries.
    let (x, y, w, h) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    match (u32::try_from(w), u32::try_from(h)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Some(Rect::new(x, y, w, h)),
        _ => None,
    }
}

/// The current desktop as GDI sees it.
pub fn desktop() -> Desktop {
    let unavailable = |state| Desktop {
        state,
        virtual_rect: None,
        monitors: Vec::new(),
    };
    if !interactive_station() {
        return unavailable(DesktopState::NonInteractive);
    }
    if session_disconnected() {
        return unavailable(DesktopState::Disconnected);
    }
    if !input_desktop_is_default() {
        return unavailable(DesktopState::Locked);
    }
    let monitors = monitors();
    let virtual_rect = virtual_rect();
    if monitors.is_empty() || virtual_rect.is_none() {
        return unavailable(DesktopState::NoDisplay);
    }
    Desktop {
        state: DesktopState::Available,
        virtual_rect,
        monitors,
    }
}

fn last_error() -> u32 {
    // SAFETY: trivially safe thread-local read.
    unsafe { GetLastError() }
}

/// Copies `region` of the screen into a new frame.
fn blit(ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError> {
    let (w, h) = (
        i32::try_from(region.width).map_err(|_| CaptureError::TooLarge)?,
        i32::try_from(region.height).map_err(|_| CaptureError::TooLarge)?,
    );
    // SAFETY: the DC of the whole screen; released at the end of this function.
    let screen = unsafe { GetDC(std::ptr::null_mut()) };
    if screen.is_null() {
        return Err(CaptureError::Os {
            what: "GetDC",
            code: last_error(),
        });
    }
    struct Guard {
        screen: HDC,
        mem: HDC,
        bitmap: *mut c_void,
        old: *mut c_void,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: each object was created (or is null) by `blit` and is released once, in
            // reverse order of creation; the original bitmap is selected back first.
            unsafe {
                if !self.old.is_null() {
                    SelectObject(self.mem, self.old);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                if !self.mem.is_null() {
                    DeleteDC(self.mem);
                }
                ReleaseDC(std::ptr::null_mut(), self.screen);
            }
        }
    }
    let mut g = Guard {
        screen,
        mem: std::ptr::null_mut(),
        bitmap: std::ptr::null_mut(),
        old: std::ptr::null_mut(),
    };
    // SAFETY: a memory DC compatible with the screen DC above.
    g.mem = unsafe { CreateCompatibleDC(screen) };
    if g.mem.is_null() {
        return Err(CaptureError::Os {
            what: "CreateCompatibleDC",
            code: last_error(),
        });
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `info` describes a 32-bit top-down DIB; `bits` receives its pixel pointer.
    g.bitmap = unsafe {
        CreateDIBSection(
            g.mem,
            &raw const info,
            DIB_RGB_COLORS,
            &raw mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if g.bitmap.is_null() || bits.is_null() {
        return Err(CaptureError::Os {
            what: "CreateDIBSection",
            code: last_error(),
        });
    }
    // SAFETY: selects our bitmap into our memory DC; the previous object is restored on drop.
    g.old = unsafe { SelectObject(g.mem, g.bitmap) };
    // SAFETY: copies from the screen DC into the DIB of exactly `w`×`h` pixels.
    let ok = unsafe {
        BitBlt(
            g.mem,
            0,
            0,
            w,
            h,
            screen,
            region.x,
            region.y,
            SRCCOPY | CAPTUREBLT,
        )
    };
    if ok == 0 {
        return Err(CaptureError::Os {
            what: "BitBlt",
            code: last_error(),
        });
    }
    // SAFETY: makes sure GDI finished writing the DIB before it is read.
    unsafe { GdiFlush() };
    let len = region.bgra_bytes() as usize;
    // SAFETY: the DIB section holds `w`×`h` 32-bit pixels (`len` bytes) at `bits` while it is
    // alive (until the guard drops, after the copy).
    let src = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), len) };
    let mut pixels = src.to_vec();
    drop(g);
    // GDI leaves the alpha byte undefined; a screen picture is opaque.
    for p in pixels.as_chunks_mut::<4>().0 {
        p[3] = 255;
    }
    Ok(Frame::new(ledger, region, pixels)?)
}

impl CaptureBackend for Gdi {
    fn name(&self) -> &'static str {
        "gdi"
    }

    fn desktop(&self) -> Desktop {
        desktop()
    }

    fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError> {
        check_region(&desktop(), region)?;
        blit(ledger, region).map_err(|e| {
            // The desktop may have been locked between the check and the copy.
            let d = desktop();
            if d.state == DesktopState::Available {
                e
            } else {
                CaptureError::Desktop(d.state)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real backend, real screen: captures a 4×3 region into memory, checks its size and format,
    /// and drops it at once. No pixel is printed, compared or written anywhere (privacy rule).
    /// On a locked or non-interactive desktop the test says so and checks the honest error.
    #[test]
    fn tiny_real_capture_or_an_honest_refusal() {
        let ledger = Ledger::new();
        let d = desktop();
        let Some(m) = d
            .monitors
            .iter()
            .find(|m| m.primary)
            .or(d.monitors.first())
            .copied()
        else {
            eprintln!("SKIP real capture: desktop state is {}", d.state.as_str());
            let e = Gdi.capture(&ledger, Rect::new(0, 0, 4, 3)).unwrap_err();
            assert!(
                matches!(e, CaptureError::Desktop(s) if s != DesktopState::Available),
                "{e:?}"
            );
            assert_eq!(ledger.live(), (0, 0));
            return;
        };
        if d.state != DesktopState::Available {
            eprintln!("SKIP real capture: desktop state is {}", d.state.as_str());
            return;
        }
        let region = Rect::new(m.rect.x, m.rect.y, 4, 3);
        let f = Gdi.capture(&ledger, region).unwrap();
        assert_eq!(f.rect(), region);
        assert_eq!(f.pixels().len(), 4 * 3 * 4, "BGRA, no row padding");
        assert!(f.pixels().chunks(4).all(|p| p[3] == 255), "opaque");
        assert_eq!(ledger.live(), (1, 48));
        drop(f);
        assert_eq!(ledger.live(), (0, 0), "nothing kept after the capture");
        // Outside the desktop: refused before any GDI call.
        let v = d.virtual_rect.unwrap();
        let outside = Rect::new(i32::try_from(v.right()).unwrap(), v.y, 2, 2);
        assert!(matches!(
            Gdi.capture(&ledger, outside),
            Err(CaptureError::Outside { .. })
        ));
    }

    #[test]
    fn desktop_geometry_is_consistent() {
        let d = desktop();
        eprintln!("desktop state: {}", d.state.as_str());
        if d.state == DesktopState::Available {
            let v = d.virtual_rect.unwrap();
            assert!(d.monitors.iter().all(|m| v.contains(&m.rect)));
            assert_eq!(d.monitors.iter().filter(|m| m.primary).count(), 1);
        } else {
            assert!(d.monitors.is_empty());
        }
    }
}
