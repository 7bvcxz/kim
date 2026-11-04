//! What Windows says about each display beyond its rectangle, and the mouse pointer drawn into a
//! capture (Step 58, ADR 0050, NOEUL-FR-016, NOEUL-FR-091, NOEUL-FR-092). Read-only: nothing here
//! changes a display setting, and the pointer is drawn into Noeul's own memory bitmap only.
//!
//! - **Details** ([`details`]): one `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)` call lists the
//!   active paths; for each, `DisplayConfigGetDeviceInfo` gives the GDI device name of its source
//!   (`\\.\DISPLAY1`, matched with `MONITORINFOEXW.szDevice`) and the advanced-colour state of its
//!   target (`DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO`: HDR, or wide colour enforced), and the path
//!   says how the target is turned (`DISPLAYCONFIG_ROTATION`). All *documented*; the names are
//!   used for the match only and never reported (NOEUL-SEC-008).
//! - **Pointer** ([`draw_pointer`]): `GetCursorInfo` gives the pointer's image and position (device
//!   pixels for a per-monitor-aware process) and whether it is shown; `GetIconInfo` its hotspot and
//!   size; `DrawIconEx` with `DI_NORMAL` at the image's own size draws it into the capture's DIB
//!   section, so monochrome pointers invert the pixels under them exactly as on screen. Animated
//!   pointers show their current frame (*documented*). The bitmaps `GetIconInfo` creates are freed
//!   at once.

use crate::capture::PointerOutcome;
use crate::frame::Rect;
use std::ffi::c_void;
use windows_sys::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Graphics::Gdi::{BITMAP, DeleteObject, GetObjectW, HDC};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, HICON,
    ICONINFO,
};

/// What the display configuration says about one active display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    /// The GDI device name of its source (`\\.\DISPLAY1`), to match `MONITORINFOEXW.szDevice`.
    pub device: String,
    /// Degrees clockwise: 0, 90, 180 or 270.
    pub rotation: u32,
    /// Advanced colour (HDR, or wide colour enforced) on; `None` when Windows did not say.
    pub hdr: Option<bool>,
}

/// `DISPLAYCONFIG_ROTATION` in degrees clockwise (identity and unknown values: 0).
pub fn rotation_degrees(r: i32) -> u32 {
    match r {
        2 => 90,
        3 => 180,
        4 => 270,
        _ => 0,
    }
}

/// The advanced-colour bits of `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO`: supported (bit 0),
/// enabled (bit 1), wide colour enforced (bit 2), force-disabled (bit 3). On when enabled and not
/// force-disabled.
pub fn advanced_colour_on(value: u32) -> bool {
    value & 0b10 != 0 && value & 0b1000 == 0
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// The active displays' details (empty when the configuration cannot be read; the caller then
/// reports rotation 0 and HDR unknown).
pub fn details() -> Vec<Details> {
    // The configuration can change between the two calls: retry a few times.
    let mut found = None;
    for _ in 0..3 {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: two writable u32 outputs.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &raw mut np, &raw mut nm) }
            != 0
        {
            return Vec::new();
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        // SAFETY: the arrays have the element counts passed; Windows writes at most that many and
        // updates the counts.
        let r = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &raw mut np,
                paths.as_mut_ptr(),
                &raw mut nm,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if r == 0 {
            paths.truncate(np as usize);
            found = Some(paths);
            break;
        }
        if r != ERROR_INSUFFICIENT_BUFFER {
            return Vec::new();
        }
    }
    let Some(paths) = found else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in &paths {
        let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: u32::try_from(size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>()).unwrap_or(0),
                adapterId: p.sourceInfo.adapterId,
                id: p.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: the header starts a DISPLAYCONFIG_SOURCE_DEVICE_NAME of the size it states.
        if unsafe { DisplayConfigGetDeviceInfo(&raw mut name.header) } != 0 {
            continue;
        }
        let mut colour = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: u32::try_from(size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>())
                    .unwrap_or(0),
                adapterId: p.targetInfo.adapterId,
                id: p.targetInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: the header starts a DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO of the size it states.
        let hdr = (unsafe { DisplayConfigGetDeviceInfo(&raw mut colour.header) } == 0).then(|| {
            // SAFETY: the union's plain `u32` view of the bit field Windows filled in.
            advanced_colour_on(unsafe { colour.Anonymous.value })
        });
        out.push(Details {
            device: wide_to_string(&name.viewGdiDeviceName),
            rotation: rotation_degrees(p.targetInfo.rotation),
            hdr,
        });
    }
    out
}

/// The size of a pointer image from its bitmaps (a monochrome pointer's mask holds the AND and
/// XOR halves one above the other).
fn icon_size(info: &ICONINFO) -> Option<(i32, i32)> {
    let read = |h: *mut c_void| {
        let mut bm = BITMAP::default();
        // SAFETY: `bm` is a writable BITMAP of the size passed; `h` is a bitmap handle.
        let n = unsafe {
            GetObjectW(
                h,
                i32::try_from(size_of::<BITMAP>()).unwrap_or(0),
                (&raw mut bm).cast::<c_void>(),
            )
        };
        (n != 0).then_some((bm.bmWidth, bm.bmHeight))
    };
    if !info.hbmColor.is_null() {
        read(info.hbmColor)
    } else if !info.hbmMask.is_null() {
        read(info.hbmMask).map(|(w, h)| (w, h / 2))
    } else {
        None
    }
}

/// Draws `icon` with its hotspot at `at` (device pixels of the virtual desktop) into `dc`, whose
/// (0, 0) is `area`'s top-left corner; `area` is the captured rectangle.
pub fn draw_icon(dc: HDC, icon: HICON, at: (i32, i32), area: Rect) -> PointerOutcome {
    let mut info = ICONINFO::default();
    // SAFETY: `info` is writable; the bitmaps it receives are deleted below.
    if unsafe { GetIconInfo(icon, &raw mut info) } == 0 {
        return PointerOutcome::Unavailable;
    }
    let size = icon_size(&info);
    // SAFETY: the bitmaps GetIconInfo created for us (or null), each deleted once.
    unsafe {
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
    }
    let Some((w, h)) = size.filter(|(w, h)| *w > 0 && *h > 0) else {
        return PointerOutcome::Unavailable;
    };
    let hx = i32::try_from(info.xHotspot).unwrap_or(0);
    let hy = i32::try_from(info.yHotspot).unwrap_or(0);
    let drawn = Rect::new(
        at.0.saturating_sub(hx),
        at.1.saturating_sub(hy),
        w.unsigned_abs(),
        h.unsigned_abs(),
    );
    if crate::displays::intersect(drawn, area).is_none() {
        return PointerOutcome::Outside;
    }
    let (x, y) = (
        i64::from(drawn.x) - i64::from(area.x),
        i64::from(drawn.y) - i64::from(area.y),
    );
    // SAFETY: draws into the caller's memory DC at its own size; no brush, frame 0.
    let ok = unsafe {
        DrawIconEx(
            dc,
            i32::try_from(x).unwrap_or(i32::MAX),
            i32::try_from(y).unwrap_or(i32::MAX),
            icon,
            0,
            0,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        )
    };
    if ok == 0 {
        PointerOutcome::Unavailable
    } else {
        PointerOutcome::Drawn(drawn)
    }
}

/// Draws the user's pointer as it is now into `dc` (see [`draw_icon`]).
pub fn draw_pointer(dc: HDC, area: Rect) -> PointerOutcome {
    let mut ci = CURSORINFO {
        cbSize: u32::try_from(size_of::<CURSORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `ci` is writable with cbSize set.
    if unsafe { GetCursorInfo(&raw mut ci) } == 0 {
        return PointerOutcome::Unavailable;
    }
    if ci.flags & CURSOR_SHOWING == 0 || ci.hCursor.is_null() {
        return PointerOutcome::Hidden;
    }
    draw_icon(dc, ci.hCursor, (ci.ptScreenPos.x, ci.ptScreenPos.y), area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, GdiFlush, SelectObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{IDC_ARROW, IDC_IBEAM, LoadCursorW};

    #[test]
    fn rotations_and_colour_bits_follow_the_documented_values() {
        assert_eq!([1, 2, 3, 4, 0, 99].map(rotation_degrees), [0, 90, 180, 270, 0, 0]);
        assert!(!advanced_colour_on(0b0001), "supported only");
        assert!(advanced_colour_on(0b0011));
        assert!(advanced_colour_on(0b0110), "wide colour enforced");
        assert!(!advanced_colour_on(0b1011), "force-disabled");
    }

    /// Reads the configuration of this machine's displays (read-only) and checks it is
    /// consistent with the monitors GDI lists; prints geometry only.
    #[test]
    fn the_display_configuration_matches_the_monitors() {
        crate::win::per_monitor_dpi_awareness();
        let d = details();
        let desk = crate::win::gdi::desktop();
        eprintln!("display paths: {}", d.len());
        for x in &d {
            eprintln!("rotation {} hdr {:?}", x.rotation, x.hdr);
            assert!([0, 90, 180, 270].contains(&x.rotation));
            assert!(x.device.starts_with(r"\\.\DISPLAY"), "a GDI device name");
        }
        if desk.state == crate::capture::DesktopState::Available {
            assert!(
                d.len() >= desk.monitors.len() || d.is_empty(),
                "every monitor has an active path (clones share one monitor)"
            );
        }
    }

    /// A 64 × 64 memory DIB of one colour, its DC and pixels (the test's own surface).
    struct Surface {
        dc: HDC,
        bitmap: *mut c_void,
        old: *mut c_void,
        bits: *mut u8,
    }

    impl Surface {
        fn new(fill: [u8; 4]) -> Surface {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap(),
                    biWidth: 64,
                    biHeight: -64,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            // SAFETY: a memory DC and a 32-bit top-down DIB section of our own, freed on drop.
            unsafe {
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                let bitmap = CreateDIBSection(
                    dc,
                    &raw const info,
                    DIB_RGB_COLORS,
                    &raw mut bits,
                    std::ptr::null_mut(),
                    0,
                );
                assert!(!dc.is_null() && !bitmap.is_null() && !bits.is_null());
                let old = SelectObject(dc, bitmap);
                let mut s = Surface {
                    dc,
                    bitmap,
                    old,
                    bits: bits.cast(),
                };
                for p in s.pixels_mut().chunks_mut(4) {
                    p.copy_from_slice(&fill);
                }
                s
            }
        }

        fn pixels_mut(&mut self) -> &mut [u8] {
            // SAFETY: the DIB holds 64 × 64 × 4 bytes while the surface lives.
            unsafe { std::slice::from_raw_parts_mut(self.bits, 64 * 64 * 4) }
        }
    }

    impl Drop for Surface {
        fn drop(&mut self) {
            // SAFETY: our own objects, released once in reverse order.
            unsafe {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    /// The real drawing path on the test's own memory surface (never the screen): the system
    /// arrow and I-beam are drawn with their hotspot at the requested device pixel, only inside
    /// the predicted box, and clipped or refused at the picture's edge.
    #[test]
    fn system_pointers_are_drawn_at_their_hotspot_into_a_memory_bitmap() {
        // SAFETY: shared system cursors (never destroyed by us).
        let arrow = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
        // SAFETY: as above.
        let ibeam = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_IBEAM) };
        assert!(!arrow.is_null() && !ibeam.is_null());
        // The picture is at negative coordinates, as on a monitor left of the primary.
        let area = Rect::new(-1000, -500, 64, 64);
        for (cursor, bg) in [(arrow, [40u8, 80, 120, 255]), (ibeam, [200, 200, 200, 255])] {
            let mut s = Surface::new(bg);
            let at = (area.x + 20, area.y + 10);
            let outcome = draw_icon(s.dc, cursor, at, area);
            // SAFETY: GDI finished drawing before the pixels are read.
            unsafe { GdiFlush() };
            let PointerOutcome::Drawn(r) = outcome else {
                panic!("{outcome:?}")
            };
            assert!(r.x <= at.0 && r.y <= at.1, "the hotspot lies inside the image");
            assert!(r.width >= 16 && r.height >= 16 && r.width <= 256, "{r}");
            let px = s.pixels_mut();
            let mut changed = 0;
            for y in 0..64i32 {
                for x in 0..64i32 {
                    let i = ((y * 64 + x) * 4) as usize;
                    if px[i..i + 3] != bg[..3] {
                        changed += 1;
                        let (ax, ay) = (area.x + x, area.y + y);
                        assert!(
                            i64::from(ax) >= i64::from(r.x)
                                && i64::from(ax) < r.right()
                                && i64::from(ay) >= i64::from(r.y)
                                && i64::from(ay) < r.bottom(),
                            "only the pointer's box changes ({ax}, {ay})"
                        );
                    }
                }
            }
            assert!(changed > 10, "the pointer is visible: {changed} pixels");
        }
        // Wholly outside the picture: nothing is drawn.
        let mut s = Surface::new([1, 2, 3, 255]);
        assert_eq!(
            draw_icon(s.dc, arrow, (area.x + 500, area.y), area),
            PointerOutcome::Outside
        );
        assert!(s.pixels_mut().chunks(4).all(|p| p == [1, 2, 3, 255]));
    }
}
