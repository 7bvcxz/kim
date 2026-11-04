//! Measures the capture overlay: first-frame latency, redraw time while dragging, and memory
//! (ADR 0026; NOEUL-NFR-002, NOEUL-NFR-003). Windows only.
//!
//! ```text
//! cargo run --release -p noeul --example overlay_measure [-- --runs 20]
//! cargo run --release -p noeul --example overlay_measure -- --gdi-stages
//! cargo run --release -p noeul --example overlay_measure -- --double-click [--runs 50]
//! ```
//!
//! `--double-click` measures NOEUL-NFR-004 through the Step 51 gesture: from the second press of
//! a double-click inside a 1920×1080 and a 3840×2160 selection to the pin shown (synthetic
//! off-screen monitors, off-screen never-activated pin windows).
//!
//! It runs the instance core's real path (`capture.region` → frozen picture → overlay window →
//! first frame painted; pointer drags; `capture.cancel`) on three layouts:
//!
//! - **real**: this machine's monitors. The whole virtual desktop is captured with GDI into
//!   memory, exactly as `F1` does, and shown in an overlay window of the same size moved to an
//!   off-screen origin (-20000,-20000).
//! - **4k-150**: one synthetic 3840×2160 monitor at 150 %, off-screen.
//! - **3x4k**: three synthetic 3840×2160 monitors side by side, off-screen.
//!
//! Safety on a machine in use: every overlay window is off-screen, never activated, never on
//! top and never changes the cursor; no input is synthesized; pictures stay in memory, nothing
//! is written, and only timings and sizes are printed (never a pixel). The synthetic layouts
//! "capture" a generated pattern, so their capture stage is not a screen capture: for them only
//! the render stage (window, back buffer, composition, paint) is meaningful.

#[cfg(windows)]
mod measure {
    use noeul::app::{App, Identity, OverlayEvent, OverlayWindow, Shell};
    use noeul::capture::{CaptureBackend, CaptureError, Desktop, Monitor, OFF_SCREEN, Synthetic};
    use noeul::error::{Code, NoeulError};
    use noeul::frame::{Frame, Ledger, Rect};
    use noeul::hotkeys::Manager;
    use noeul::overlay::{Button, Input, Mods, SystemValues};
    use noeul::paths::Paths;
    use noeul::pins::PinWindows;
    use noeul::win::overlay::{Placement, WinOverlay, system_values};
    use noeul::win::pinwnd::{Style, WinPins};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// The real GDI backend, shifted so that the overlay lands off-screen.
    struct Shifted {
        dx: i32,
        dy: i32,
    }

    impl Shifted {
        fn shift(&self, r: Rect) -> Rect {
            Rect::new(r.x + self.dx, r.y + self.dy, r.width, r.height)
        }
    }

    impl CaptureBackend for Shifted {
        fn name(&self) -> &'static str {
            "gdi"
        }
        fn desktop(&self) -> Desktop {
            let mut d = noeul::win::gdi::desktop();
            d.virtual_rect = d.virtual_rect.map(|r| self.shift(r));
            for m in &mut d.monitors {
                m.rect = self.shift(m.rect);
            }
            d
        }
        fn capture(&self, ledger: &Arc<Ledger>, region: Rect) -> Result<Frame, CaptureError> {
            let real = Rect::new(
                region.x - self.dx,
                region.y - self.dy,
                region.width,
                region.height,
            );
            let f = noeul::win::gdi::Gdi.capture(ledger, real)?;
            Ok(f.moved_to(region.x, region.y))
        }
    }

    struct Quiet {
        windows: WinPins,
        pointer: (i32, i32),
        text: noeul::card::BlockText,
    }

    impl Shell for Quiet {
        fn pin_windows(&mut self) -> &mut dyn PinWindows {
            &mut self.windows
        }
        fn clipboard_take(
            &mut self,
            _: Option<noeul::paste::Kind>,
            _: noeul::paste::Options,
        ) -> noeul::error::Result<noeul::paste::Taken> {
            Err(NoeulError::new(Code::NothingToPin, "not used"))
        }
        fn clipboard_write_text(&mut self, _: &str) -> noeul::error::Result<()> {
            Err(NoeulError::new(
                Code::ClipboardUnavailable,
                "never touches the clipboard",
            ))
        }
        fn read_image_file(
            &mut self,
            _: &std::path::Path,
            _: u64,
        ) -> noeul::error::Result<Vec<u8>> {
            Err(NoeulError::new(Code::NothingToPin, "not used"))
        }
        fn text_engine(&mut self) -> &mut dyn noeul::card::TextEngine {
            &mut self.text
        }
        fn clipboard_write(&mut self, _: &[u8], _: &[u8]) -> noeul::error::Result<()> {
            Err(NoeulError::new(
                Code::ClipboardUnavailable,
                "never touches the clipboard",
            ))
        }
        fn pictures_dir(&self) -> Option<std::path::PathBuf> {
            None
        }
        fn local_time(&self) -> [u16; 6] {
            [2026, 1, 1, 0, 0, 0]
        }
        fn pointer(&self) -> (i32, i32) {
            self.pointer
        }
        fn notice(&mut self, _: &str) {}
        fn tray_shown(&self) -> bool {
            false
        }
        fn confirm_quit(&mut self, _: usize) -> bool {
            false
        }
        fn open_overlay(
            &mut self,
            picture: Rect,
            layered: bool,
        ) -> Result<Box<dyn OverlayWindow>, String> {
            WinOverlay::create(picture, Placement::OFF_SCREEN_TEST, layered)
                .map(|w| Box::new(w) as Box<dyn OverlayWindow>)
        }
        fn system_values(&self) -> SystemValues {
            system_values()
        }
        fn interactive(&self) -> bool {
            false
        }
        // Detection lists only this process's windows here: a measurement never reads the
        // user's windows (Step 52).
        fn window_snapshot(&mut self, picture: Rect) -> noeul::detect::Snapshot {
            noeul::win::detect::snapshot(
                picture,
                &noeul::win::detect::Scope::Processes(vec![std::process::id()]),
                noeul::win::overlay::current(),
            )
        }
        fn request_element(&mut self, _: noeul::detect::ElementRequest) {}
        fn end_detection(&mut self) {}
    }

    /// Private bytes of this process (`GetProcessMemoryInfo`).
    #[allow(unsafe_code)]
    fn private_bytes() -> u64 {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut c = PROCESS_MEMORY_COUNTERS_EX {
            cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: our own process; `c` is writable with `cb` set.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
                c.cb,
            )
        };
        c.PrivateUsage as u64
    }

    fn pct(v: &mut [u64], p: usize) -> u64 {
        v.sort_unstable();
        let rank = (p * v.len()).div_ceil(100).max(1);
        v[rank - 1]
    }

    fn mib(b: u64) -> String {
        format!("{:.1}", b as f64 / (1024.0 * 1024.0))
    }

    fn ms(us: u64) -> String {
        format!("{:.1}", us as f64 / 1000.0)
    }

    struct Row {
        name: &'static str,
        size: String,
        total: (u64, u64),
        capture: (u64, u64),
        render: (u64, u64),
        drag: (u64, u64),
        base: u64,
        peak: u64,
        after: u64,
        after10: u64,
    }

    fn run(name: &'static str, backend: Box<dyn CaptureBackend>, runs: usize) -> Option<Row> {
        let d = backend.desktop();
        let Some(pic) = d.virtual_rect else {
            eprintln!("{name}: SKIP, desktop state is {}", d.state.as_str());
            return None;
        };
        let dir = std::env::temp_dir().join(format!("noeul-measure-{}", std::process::id()));
        let paths = Paths::under(&dir);
        let mgr = Manager::start(&paths, Some("--no-hotkeys"), || {
            unreachable!("no hotkey is registered")
        });
        let center = (
            pic.x + i32::try_from(pic.width / 2).unwrap_or(0),
            pic.y + i32::try_from(pic.height / 2).unwrap_or(0),
        );
        let mut app = App::new(
            Quiet {
                windows: WinPins {
                    style: Style::OFF_SCREEN_TEST,
                },
                pointer: center,
                text: noeul::card::BlockText,
            },
            backend,
            mgr,
            noeul::settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: std::process::id(),
                endpoint: r"\\.\pipe\noeul-measure".into(),
                session: 0,
            },
        );
        let base = private_bytes();
        let mut peak = base;
        let (mut totals, mut caps, mut renders, mut drags) = (vec![], vec![], vec![], vec![]);
        for _ in 0..runs {
            let r: Value = match app.request("capture.region", &json!({})) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{name}: SKIP, {} ({})", e.message, e.code);
                    return None;
                }
            };
            assert_eq!(r["opened"], true);
            let last = app.status()["captures"]["first_frame"]["last"].clone();
            totals.push(last["total_us"].as_u64().unwrap_or(0));
            caps.push(last["capture_us"].as_u64().unwrap_or(0));
            renders.push(last["render_us"].as_u64().unwrap_or(0));
            peak = peak.max(private_bytes());
            // A drag from a third of the picture to two thirds, in 60 pointer moves.
            let (x0, y0) = (
                pic.x + i32::try_from(pic.width / 3).unwrap_or(0),
                pic.y + i32::try_from(pic.height / 3).unwrap_or(0),
            );
            app.overlay_event(OverlayEvent::Input(Input::Press {
                button: Button::Left,
                x: x0,
                y: y0,
                mods: Mods::NONE,
                time_ms: 0,
            }));
            for i in 1..=60i32 {
                let x = x0 + i * i32::try_from(pic.width / 180).unwrap_or(1);
                let y = y0 + i * i32::try_from(pic.height / 180).unwrap_or(1);
                let t = Instant::now();
                app.overlay_event(OverlayEvent::Input(Input::Move {
                    x,
                    y,
                    mods: Mods::NONE,
                }));
                drags.push(u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX));
            }
            peak = peak.max(private_bytes());
            app.request("capture.cancel", &json!({})).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        let after = private_bytes();
        std::thread::sleep(Duration::from_secs(10));
        let after10 = private_bytes();
        let _ = std::fs::remove_dir_all(&dir);
        let st = app.status();
        assert_eq!(st["memory"]["bytes"], 0, "nothing kept");
        Some(Row {
            name,
            size: format!(
                "{}×{} ({} monitor(s), {} MiB per picture)",
                pic.width,
                pic.height,
                d.monitors.len(),
                mib(pic.bgra_bytes())
            ),
            total: (pct(&mut totals.clone(), 50), pct(&mut totals, 95)),
            capture: (pct(&mut caps.clone(), 50), pct(&mut caps, 95)),
            render: (pct(&mut renders.clone(), 50), pct(&mut renders, 95)),
            drag: (pct(&mut drags.clone(), 50), pct(&mut drags, 95)),
            base,
            peak,
            after,
            after10,
        })
    }

    fn uhd(n: i32) -> Vec<Monitor> {
        (0..n)
            .map(|i| {
                Monitor::new(
                    Rect::new(OFF_SCREEN.0 + i * 3840, OFF_SCREEN.1, 3840, 2160),
                    i == 0,
                )
                .with_dpi(144)
            })
            .collect()
    }

    /// One `BitBlt` of the whole virtual desktop into a DIB section (memory only), in µs.
    #[allow(unsafe_code)]
    fn blit_once(r: Rect, captureblt: bool) -> u64 {
        use windows_sys::Win32::Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
            CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC,
            SRCCOPY, SelectObject,
        };
        let (w, h) = (
            i32::try_from(r.width).unwrap_or(1),
            i32::try_from(r.height).unwrap_or(1),
        );
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let t = Instant::now();
        // SAFETY: the objects are created, used and released here in order; the pixels are
        // never read.
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            let mem = CreateCompatibleDC(screen);
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(
                mem,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                std::ptr::null_mut(),
                0,
            );
            let old = SelectObject(mem, bmp);
            let rop = if captureblt {
                SRCCOPY | CAPTUREBLT
            } else {
                SRCCOPY
            };
            BitBlt(mem, 0, 0, w, h, screen, r.x, r.y, rop);
            GdiFlush();
            SelectObject(mem, old);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(std::ptr::null_mut(), screen);
        }
        u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    /// Where the capture stage's time goes on this machine (GDI, ADR 0023 §3).
    fn gdi_stages(runs: usize) {
        let d = noeul::win::gdi::desktop();
        let Some(r) = d.virtual_rect else {
            println!("gdi stages: SKIP, desktop state is {}", d.state.as_str());
            return;
        };
        let ledger = Ledger::new();
        let (mut with, mut without, mut full) = (vec![], vec![], vec![]);
        for _ in 0..runs {
            with.push(blit_once(r, true));
            without.push(blit_once(r, false));
            let t = Instant::now();
            let f = noeul::win::gdi::Gdi.capture(&ledger, r).expect("capture");
            full.push(u64::try_from(t.elapsed().as_micros()).unwrap_or(0));
            drop(f);
            std::thread::sleep(Duration::from_millis(50));
        }
        println!(
            "gdi stages for {r} (p50 / p95 ms): BitBlt with CAPTUREBLT {} / {}; without {} / {}; Gdi::capture (BitBlt, copy, opaque alpha) {} / {}",
            ms(pct(&mut with.clone(), 50)),
            ms(pct(&mut with, 95)),
            ms(pct(&mut without.clone(), 50)),
            ms(pct(&mut without, 95)),
            ms(pct(&mut full.clone(), 50)),
            ms(pct(&mut full, 95)),
        );
    }

    /// NOEUL-NFR-004 through the double-click (Step 51): on a synthetic off-screen monitor of
    /// the selection's size, `capture.region`, a drag over the whole monitor, a first click, and
    /// the time from handing the second press to the core until it returns — the selection cut
    /// from the frozen picture, the pin window created, rendered and shown, the overlay window
    /// destroyed and the frozen picture freed. Pins are closed between runs.
    fn double_click_latency(w: u32, h: u32, runs: usize) {
        let mon = vec![Monitor::new(
            Rect::new(OFF_SCREEN.0, OFF_SCREEN.1, w, h),
            true,
        )];
        let dir = std::env::temp_dir().join(format!("noeul-measure-dc-{}", std::process::id()));
        let paths = Paths::under(&dir);
        let mgr = Manager::start(&paths, Some("--no-hotkeys"), || {
            unreachable!("no hotkey is registered")
        });
        let mut app = App::new(
            Quiet {
                windows: WinPins {
                    style: Style::OFF_SCREEN_TEST,
                },
                pointer: (OFF_SCREEN.0 + 10, OFF_SCREEN.1 + 10),
                text: noeul::card::BlockText,
            },
            Box::new(Synthetic::new(mon)),
            mgr,
            noeul::settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: std::process::id(),
                endpoint: r"\\.\pipe\noeul-measure".into(),
                session: 0,
            },
        );
        let (x0, y0) = OFF_SCREEN;
        let (x1, y1) = (
            x0 + i32::try_from(w).unwrap_or(0) - 1,
            y0 + i32::try_from(h).unwrap_or(0) - 1,
        );
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        let ev = |app: &mut App<Quiet>, i: Input| app.overlay_event(OverlayEvent::Input(i));
        let press = |x, y, t| Input::Press {
            button: Button::Left,
            x,
            y,
            mods: Mods::NONE,
            time_ms: t,
        };
        let release = |x, y| Input::Release {
            button: Button::Left,
            x,
            y,
        };
        let mut samples = Vec::new();
        for i in 0..runs {
            app.request("capture.region", &json!({})).expect("overlay");
            ev(&mut app, press(x0, y0, 0));
            ev(
                &mut app,
                Input::Move {
                    x: x1,
                    y: y1,
                    mods: Mods::NONE,
                },
            );
            ev(&mut app, release(x1, y1));
            let t0 = u32::try_from(i).unwrap_or(0) * 10_000 + 1_000;
            ev(&mut app, press(cx, cy, t0));
            ev(&mut app, release(cx, cy));
            let t = Instant::now();
            ev(&mut app, press(cx, cy, t0 + 100));
            samples.push(u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX));
            assert!(!app.overlay_open(), "the double-click pinned");
            assert_eq!(app.pins().len(), 1);
            app.request("pin.close", &json!({"all": true})).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(&dir);
        println!(
            "| {w}×{h} | {} / {} ms |",
            ms(pct(&mut samples.clone(), 50)),
            ms(pct(&mut samples, 95))
        );
    }

    pub fn main() {
        noeul::win::per_monitor_dpi_awareness();
        if std::env::args().any(|a| a == "--gdi-stages") {
            gdi_stages(20);
            return;
        }
        if std::env::args().any(|a| a == "--double-click") {
            let runs = std::env::args()
                .skip_while(|a| a != "--runs")
                .nth(1)
                .and_then(|n| n.parse().ok())
                .unwrap_or(50usize);
            println!("Second press of the double-click → pin shown, {runs} runs (NOEUL-NFR-004)");
            println!("| Selection | p50 / p95 |");
            println!("| --- | --- |");
            double_click_latency(1920, 1080, runs);
            double_click_latency(3840, 2160, runs);
            return;
        }
        let runs = std::env::args()
            .skip_while(|a| a != "--runs")
            .nth(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(20usize);
        let real = noeul::win::gdi::desktop();
        let shift = real
            .virtual_rect
            .map(|r| (OFF_SCREEN.0 - r.x, OFF_SCREEN.1 - r.y))
            .unwrap_or((0, 0));
        println!(
            "Noeul overlay measurement, {} build, {runs} runs per layout",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        for m in &real.monitors {
            println!(
                "real monitor: {} at {} DPI{}",
                m.rect,
                m.dpi,
                if m.primary { " (primary)" } else { "" }
            );
        }
        let rows: Vec<Row> = [
            run(
                "real",
                Box::new(Shifted {
                    dx: shift.0,
                    dy: shift.1,
                }),
                runs,
            ),
            run("4k-150", Box::new(Synthetic::new(uhd(1))), runs),
            run("3x4k", Box::new(Synthetic::new(uhd(3))), runs),
        ]
        .into_iter()
        .flatten()
        .collect();
        println!();
        println!(
            "| Layout | Picture | First frame p50 / p95 (ms) | Capture p50 / p95 | Render p50 / p95 | Drag redraw p50 / p95 | Private MiB: before / peak / after / +10 s |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- |");
        for r in rows {
            println!(
                "| {} | {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} / {} / {} |",
                r.name,
                r.size,
                ms(r.total.0),
                ms(r.total.1),
                ms(r.capture.0),
                ms(r.capture.1),
                ms(r.render.0),
                ms(r.render.1),
                ms(r.drag.0),
                ms(r.drag.1),
                mib(r.base),
                mib(r.peak),
                mib(r.after),
                mib(r.after10)
            );
        }
    }
}

fn main() {
    #[cfg(windows)]
    measure::main();
    #[cfg(not(windows))]
    eprintln!("overlay_measure: Windows only (the overlay is not implemented elsewhere yet)");
}
