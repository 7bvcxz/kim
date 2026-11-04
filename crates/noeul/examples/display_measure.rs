//! Measures the display work of Step 58 and probes what the Windows capture APIs do with a
//! window that protects itself (ADR 0050; NOEUL-FR-016, NOEUL-FR-017, NOEUL-FR-091, NOEUL-FR-092,
//! NOEUL-SEC-004, NOEUL-NFR-001). Windows only.
//!
//! ```text
//! cargo run --release -p noeul --example display_measure [-- --runs 200]
//! ```
//!
//! - **Desktop query**: `win::gdi::desktop()` (session and input-desktop checks,
//!   `EnumDisplayMonitors`, per-monitor DPI) and the display configuration it adds since Step 58
//!   (`QueryDisplayConfig` for rotation and advanced colour), p50/p95. Geometry and states only.
//! - **Capture path without the screen**: the GDI copy and pointer drawing Noeul does after
//!   `BitBlt`, with a **memory DC as the source** (never the screen DC): `BitBlt` of 1920×1080 and
//!   3840×2160 into a DIB section, `DrawIconEx` of the system arrow, the copy out and the alpha fix;
//!   plus `fill_gaps` and the synthetic pointer over the same sizes, and the logical mapping.
//! - **Display change with twenty pins**: twenty 1920×1080 pins on real off-screen windows
//!   (`Style::OFF_SCREEN_TEST`), then what the core does per display change: follow the windows
//!   (`Pins::sync`) and redraw each outline for a new scale (`Pins::set_dpi`, 96 → 144 → 96).
//! - **Protected windows** (*measured* on this machine for ADR 0050): this example's own window,
//!   far off-screen, filled with a known colour, is read back with `PrintWindow` (with and without
//!   `PW_RENDERFULLCONTENT`) and with `BitBlt` from its own window DC, under each display affinity
//!   (`WDA_NONE`, `WDA_MONITOR`, `WDA_EXCLUDEFROMCAPTURE`). It shows which APIs would get around
//!   the protection — the reason Noeul never uses them (NOEUL-SEC-004). The screen DC is never read.
//!
//! Safety on a machine in use: every window lies at -30000,-30000 or on the synthetic desktop
//! around -20000,-20000 (no monitor there), is never activated and never on top; no display
//! setting is changed; nothing is written; only timings, sizes and yes/no results are printed.

#[cfg(windows)]
#[allow(unsafe_code)]
mod measure {
    use noeul::capture::{CaptureBackend, Desktop, DesktopState, Monitor, Synthetic};
    use noeul::frame::{Frame, Ledger, Rect};
    use noeul::pins::{NewPin, PinId, Pins, Placement, Source};
    use noeul::win::pinwnd::{Style, WinPins};
    use std::ffi::c_void;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleDC, CreateDIBSection,
        CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetWindowDC, HDC,
        ReleaseDC, SRCCOPY, SelectObject, UpdateWindow,
    };
    use windows_sys::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyWindow, DispatchMessageW, DrawIconEx,
        GetWindowRect, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PW_RENDERFULLCONTENT, PeekMessageW,
        RegisterClassExW, SW_SHOWNOACTIVATE, SetWindowDisplayAffinity, ShowWindow,
        TranslateMessage, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR, WDA_NONE, WNDCLASSEXW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    fn pct(v: &mut [Duration], p: usize) -> f64 {
        v.sort();
        let i = ((v.len() * p).div_ceil(100))
            .saturating_sub(1)
            .min(v.len() - 1);
        v[i].as_secs_f64() * 1000.0
    }

    fn time(runs: usize, mut f: impl FnMut()) -> (f64, f64) {
        let mut v: Vec<Duration> = (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect();
        (pct(&mut v, 50), pct(&mut v, 95))
    }

    /// A memory DC with a 32-bit top-down DIB section of `w`×`h`.
    struct Dib {
        dc: HDC,
        bitmap: *mut c_void,
        old: *mut c_void,
        bits: *mut u8,
        len: usize,
    }

    impl Dib {
        fn new(w: i32, h: i32) -> Dib {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap(),
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            // SAFETY: a memory DC and a DIB section of our own, released on drop.
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
                assert!(!bitmap.is_null() && !bits.is_null());
                let old = SelectObject(dc, bitmap);
                Dib {
                    dc,
                    bitmap,
                    old,
                    bits: bits.cast(),
                    len: (w * h * 4) as usize,
                }
            }
        }

        fn pixels(&self) -> &[u8] {
            // SAFETY: the DIB holds `len` bytes while it lives.
            unsafe { std::slice::from_raw_parts(self.bits, self.len) }
        }
    }

    impl Drop for Dib {
        fn drop(&mut self) {
            // SAFETY: our own objects, released once.
            unsafe {
                SelectObject(self.dc, self.old);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    fn desktop_query(runs: usize) {
        let (p50, p95) = time(runs, || {
            let _ = noeul::win::gdi::desktop();
        });
        let (c50, c95) = time(runs, || {
            let _ = noeul::win::displays::details();
        });
        let d = noeul::win::gdi::desktop();
        println!(
            "desktop query, {runs} runs ({} monitor(s), state {})",
            d.monitors.len(),
            d.state.as_str()
        );
        for (i, m) in d.monitors.iter().enumerate() {
            println!(
                "  monitor {}: {}×{} at {} %, rotation {}, hdr {:?}",
                i + 1,
                m.rect.width,
                m.rect.height,
                noeul::dpi::scale_percent(m.dpi),
                m.rotation,
                m.hdr
            );
        }
        println!("  gdi::desktop() p50 {p50:.3} ms p95 {p95:.3} ms");
        println!("  of which QueryDisplayConfig details() p50 {c50:.3} ms p95 {c95:.3} ms");
    }

    fn capture_path(runs: usize) {
        println!();
        println!("capture path with a memory DC as the source (never the screen), {runs} runs");
        println!(
            "size        | BitBlt+arrow+copy p50/p95 ms | fill_gaps p50/p95 ms | synthetic+pointer p50/p95 ms"
        );
        // SAFETY: a shared system cursor (never destroyed by us).
        let arrow = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
        let ledger = Ledger::new();
        for (w, h) in [(1920i32, 1080i32), (3840, 2160)] {
            let src = Dib::new(w, h);
            let (b50, b95) = time(runs, || {
                let dst = Dib::new(w, h);
                // SAFETY: two memory DCs of our own; the arrow at a fixed place inside.
                unsafe {
                    BitBlt(dst.dc, 0, 0, w, h, src.dc, 0, 0, SRCCOPY);
                    DrawIconEx(
                        dst.dc,
                        100,
                        100,
                        arrow,
                        0,
                        0,
                        0,
                        std::ptr::null_mut(),
                        DI_NORMAL,
                    );
                    GdiFlush();
                }
                let mut px = dst.pixels().to_vec();
                for p in px.as_chunks_mut::<4>().0 {
                    p[3] = 255;
                }
                std::hint::black_box(px);
            });
            // An L-shaped layout: the picture's right half has a gap below a shorter monitor.
            let (uw, uh) = (u32::try_from(w).unwrap(), u32::try_from(h).unwrap());
            let monitors = [
                Monitor::new(Rect::new(0, 0, uw / 2, uh), true),
                Monitor::new(Rect::new(w / 2, 0, uw / 2, uh / 2), false),
            ];
            let (g50, g95) = time(runs, || {
                let mut f = Frame::solid(&ledger, Rect::new(0, 0, uw, uh), [1, 2, 3, 255]).unwrap();
                std::hint::black_box(noeul::displays::fill_gaps(&mut f, &monitors));
            });
            let syn = Synthetic::new(vec![Monitor::new(Rect::new(0, 0, uw, uh), true)]);
            syn.set_pointer(Some((100, 100)));
            let (s50, s95) = time(runs.min(30), || {
                std::hint::black_box(
                    syn.capture_with_pointer(&ledger, Rect::new(0, 0, uw, uh))
                        .unwrap(),
                );
            });
            println!(
                "{w}×{h}  | {b50:.2} / {b95:.2}                | {g50:.2} / {g95:.2}          | {s50:.2} / {s95:.2}"
            );
        }
        let layout: Vec<Monitor> = vec![
            Monitor::new(Rect::new(-2880, 0, 2880, 1620), false).with_dpi(144),
            Monitor::new(Rect::new(0, 0, 1920, 1080), true),
            Monitor::new(Rect::new(1920, -840, 1350, 2400), false).with_dpi(120),
        ];
        let (m50, m95) = time(runs, || {
            std::hint::black_box(noeul::displays::mapping(
                &layout,
                Rect::new(-100, 100, 300, 50),
            ));
        });
        println!(
            "logical mapping (3 monitors) p50 {:.4} ms p95 {:.4} ms",
            m50, m95
        );
    }

    fn display_change_with_pins(ledger: &std::sync::Arc<Ledger>, runs: usize) {
        const FAR: i32 = -20_000;
        let desk = Desktop {
            state: DesktopState::Available,
            virtual_rect: Some(Rect::new(FAR, FAR, 7680, 4320)),
            monitors: vec![Monitor::new(Rect::new(FAR, FAR, 7680, 4320), true)],
        };
        let mut windows = WinPins {
            style: Style::OFF_SCREEN_TEST,
        };
        let mut pins = Pins::new();
        let mut ids: Vec<PinId> = Vec::new();
        for i in 0..20 {
            let r = Rect::new(FAR + (i % 4) * 1920, FAR + (i / 4) * 200, 1920, 1080);
            let f = Frame::solid(ledger, r, [40, 80, 120, 255]).unwrap();
            ids.push(
                pins.create(
                    &mut windows,
                    &desk,
                    NewPin {
                        frame: f,
                        source: Source::Capture,
                        placement: Placement::Exact,
                        focus: false,
                        guard: None,
                        annotations: Vec::new(),
                    },
                )
                .map_err(|(_, e)| e)
                .unwrap(),
            );
        }
        let (y50, y95) = time(runs, || {
            std::hint::black_box(pins.sync());
        });
        let mut dpi = 96;
        let (d50, d95) = time(runs.min(20), || {
            dpi = if dpi == 96 { 144 } else { 96 };
            for id in &ids {
                let _ = pins.set_dpi(*id, dpi);
            }
        });
        println!();
        println!("display change with twenty 1920×1080 pins (real off-screen windows)");
        println!("  Pins::sync (follow the windows) p50 {y50:.3} ms p95 {y95:.3} ms");
        println!(
            "  redraw every outline for a new scale (96 ↔ 144) p50 {d50:.1} ms p95 {d95:.1} ms"
        );
        pins.clear();
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn pump() {
        // SAFETY: MSG is plain data; this thread's own messages are dispatched.
        unsafe {
            let mut m: MSG = std::mem::zeroed();
            while PeekMessageW(&raw mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&raw const m);
                DispatchMessageW(&raw const m);
            }
        }
    }

    /// Whether `dib` holds the probe colour (BGR 30, 200, 10) in most pixels.
    fn shows_probe(dib: &Dib) -> bool {
        let px = dib.pixels();
        let hits = px
            .chunks(4)
            .filter(|p| p[0] == 30 && p[1] == 200 && p[2] == 10)
            .count();
        hits * 2 > px.len() / 4
    }

    fn protected_windows() {
        let class = wide("NoeulDisplayMeasureProbe");
        // SAFETY: a class of our own with a solid background brush (RGB 10, 200, 30).
        unsafe {
            let wc = WNDCLASSEXW {
                cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap(),
                lpfnWndProc: Some(DefWindowProcW),
                hInstance: GetModuleHandleW(std::ptr::null()),
                lpszClassName: class.as_ptr(),
                hbrBackground: CreateSolidBrush(0x001E_C80A),
                ..Default::default()
            };
            RegisterClassExW(&raw const wc);
        }
        let (w, h) = (64i32, 48i32);
        // SAFETY: our own popup, far off-screen, never activated.
        let hwnd: HWND = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                std::ptr::null(),
                WS_POPUP,
                -30_000,
                -30_000,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        // SAFETY: as above.
        unsafe {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(hwnd);
        }
        pump();
        std::thread::sleep(Duration::from_millis(100));
        pump();
        let mut r = RECT::default();
        // SAFETY: our own window.
        unsafe { GetWindowRect(hwnd, &raw mut r) };
        println!();
        println!(
            "protected windows: this example's own {w}×{h} window at ({}, {}), off-screen",
            r.left, r.top
        );
        println!(
            "affinity               | PrintWindow 0 | PrintWindow FULLCONTENT | BitBlt window DC"
        );
        for (name, aff) in [
            ("WDA_NONE", WDA_NONE),
            ("WDA_MONITOR", WDA_MONITOR),
            ("WDA_EXCLUDEFROMCAPTURE", WDA_EXCLUDEFROMCAPTURE),
        ] {
            // SAFETY: our own top-level window.
            let set = unsafe { SetWindowDisplayAffinity(hwnd, aff) } != 0;
            pump();
            std::thread::sleep(Duration::from_millis(100));
            pump();
            let print = |flags: PRINT_WINDOW_FLAGS| {
                let d = Dib::new(w, h);
                // SAFETY: our own window into our own memory DC.
                let ok = unsafe { PrintWindow(hwnd, d.dc, flags) } != 0;
                // SAFETY: GDI finished before the read.
                unsafe { GdiFlush() };
                if ok {
                    if shows_probe(&d) {
                        "content"
                    } else {
                        "black/blank"
                    }
                } else {
                    "refused"
                }
            };
            let plain = print(0);
            let full = print(PW_RENDERFULLCONTENT);
            let d = Dib::new(w, h);
            // SAFETY: our own window's DC into our own memory DC; released at once.
            let blt = unsafe {
                let wdc = GetWindowDC(hwnd);
                let ok = BitBlt(d.dc, 0, 0, w, h, wdc, 0, 0, SRCCOPY) != 0;
                ReleaseDC(hwnd, wdc);
                GdiFlush();
                if !ok {
                    "refused"
                } else if shows_probe(&d) {
                    "content"
                } else {
                    "black/blank"
                }
            };
            println!(
                "{name:<22} | {plain:<13} | {full:<23} | {blt}{}",
                if set { "" } else { "  (affinity not set)" }
            );
        }
        // SAFETY: our own window, destroyed once.
        unsafe { DestroyWindow(hwnd) };
        pump();
    }

    pub fn main() {
        noeul::win::per_monitor_dpi_awareness();
        let runs: usize = std::env::args()
            .skip_while(|a| a != "--runs")
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(200);
        desktop_query(runs);
        capture_path(runs.min(50));
        let ledger = Ledger::new();
        display_change_with_pins(&ledger, runs.min(100));
        protected_windows();
        assert_eq!(ledger.live(), (0, 0), "nothing kept");
    }
}

fn main() {
    #[cfg(windows)]
    measure::main();
    #[cfg(not(windows))]
    eprintln!("display_measure: Windows only");
}
