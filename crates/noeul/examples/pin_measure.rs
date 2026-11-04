//! Measures the pin engine: pin latency, memory per pin, zoomed pins, many pins and idle cost
//! (ADR 0029; NOEUL-NFR-001, NOEUL-NFR-002, NOEUL-NFR-004, NOEUL-NFR-007). Windows only.
//!
//! ```text
//! cargo run --release -p noeul --example pin_measure [-- --runs 50]
//! ```
//!
//! - **Pin latency** (NOEUL-NFR-004): the engine's part of `Enter`/double-click → visible pin:
//!   the selection is cut from a frozen picture of the virtual desktop's size (`Frame::crop`, what
//!   `CaptureSelection` does) and a pin window is created, rendered and shown
//!   (`win::pinwnd::create`: DIB, premultiply, `UpdateLayeredWindow`, `ShowWindow`). DWM puts
//!   the window on screen at its next composition, at most one refresh later.
//! - **Memory** (NOEUL-NFR-002, "a pin costs at most twice its pixel bytes plus 2 MB"): the
//!   process's private bytes before a pin, with it, zoomed to 200 %, and after it was destroyed;
//!   then twenty 1920×1080 pins at once (NOEUL-NFR-007).
//! - **Idle** (NOEUL-NFR-001): with twenty pins open, the process waits for messages for ten
//!   seconds; the CPU time it used and the messages it received are counted.
//!
//! Safety on a machine in use: every window lies at -20000,-20000 (no monitor there), is never
//! activated and never on top (`Style::OFF_SCREEN_TEST`); the pictures are generated patterns,
//! nothing reads the screen, nothing is written, and only timings and sizes are printed.

#[cfg(windows)]
#[allow(unsafe_code)]
mod measure {
    use noeul::frame::{Frame, Ledger, Rect};
    use noeul::pins::{PinId, PinWindow, WindowSpec};
    use noeul::win::pinwnd::{self, Style, WinPin};
    use std::time::{Duration, Instant};

    const FAR: i32 = -20_000;

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

    /// User plus kernel CPU time of this process (100 ns units).
    fn cpu_time() -> u64 {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: our own process; four writable FILETIMEs.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &raw mut c,
                &raw mut e,
                &raw mut k,
                &raw mut u,
            )
        };
        let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
        t(k) + t(u)
    }

    /// Waits for messages for `d`, dispatching them; returns how many arrived.
    fn idle(d: Duration) -> u64 {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
            TranslateMessage,
        };
        let until = Instant::now() + d;
        let mut n = 0;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return n;
            }
            let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: no handles; blocks until a message arrives or the time is up.
            unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, ms, QS_ALLINPUT) };
            // SAFETY: MSG is plain data; all-zero is a valid out-parameter.
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            loop {
                // SAFETY: `msg` is writable; drains this thread's queue without blocking.
                let got =
                    unsafe { PeekMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) };
                if got == 0 {
                    break;
                }
                n += 1;
                // SAFETY: a message just retrieved.
                unsafe {
                    TranslateMessage(&raw const msg);
                    DispatchMessageW(&raw const msg);
                }
            }
        }
    }

    fn pct(v: &mut [u64], p: usize) -> u64 {
        v.sort_unstable();
        let rank = (p * v.len()).div_ceil(100).max(1);
        v[rank - 1]
    }

    fn mib(b: u64) -> String {
        format!("{:.1}", b as f64 / (1024.0 * 1024.0))
    }

    fn signed_mib(a: u64, b: u64) -> String {
        format!("{:+.1}", (b as f64 - a as f64) / (1024.0 * 1024.0))
    }

    /// A frozen picture of `w`×`h` at the off-screen origin (a pattern, not the screen).
    fn picture(ledger: &std::sync::Arc<Ledger>, w: u32, h: u32) -> Frame {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                px.extend_from_slice(&[x as u8, y as u8, (x ^ y) as u8, 255]);
            }
        }
        Frame::new(ledger, Rect::new(FAR, FAR, w, h), px).expect("a valid picture")
    }

    fn spec(rect: Rect) -> WindowSpec {
        WindowSpec {
            rect,
            focus: false,
            guard: None,
        }
    }

    pub fn main() {
        noeul::win::per_monitor_dpi_awareness();
        let runs: usize = std::env::args()
            .skip_while(|a| a != "--runs")
            .nth(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or(50);
        let ledger = Ledger::new();
        // The frozen picture of a 3×4K desktop, as large as the overlay ever holds here.
        let frozen = picture(&ledger, 11_520, 2160);
        println!(
            "pin latency (crop from an 11520×2160 frozen picture + window shown), {runs} runs"
        );
        println!("size        | crop p50/p95 ms | window p50/p95 ms | total p50/p95 ms");
        for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
            let (mut crops, mut wins, mut totals) = (vec![], vec![], vec![]);
            for i in 0..runs {
                let t0 = Instant::now();
                let sel = frozen
                    .crop(Rect::new(FAR + 100, FAR, w, h))
                    .expect("inside the picture");
                let t1 = Instant::now();
                let pin = pinwnd::create(
                    PinId(i as u64 + 1),
                    &sel,
                    &spec(sel.rect()),
                    Style::OFF_SCREEN_TEST,
                )
                .expect("a pin window");
                let t2 = Instant::now();
                crops.push(u64::try_from((t1 - t0).as_micros()).unwrap_or(u64::MAX));
                wins.push(u64::try_from((t2 - t1).as_micros()).unwrap_or(u64::MAX));
                totals.push(u64::try_from((t2 - t0).as_micros()).unwrap_or(u64::MAX));
                drop(pin);
                drop(sel);
            }
            let ms = |v: u64| format!("{:.1}", v as f64 / 1000.0);
            println!(
                "{w}×{h} | {} / {} | {} / {} | {} / {}",
                ms(pct(&mut crops, 50)),
                ms(pct(&mut crops, 95)),
                ms(pct(&mut wins, 50)),
                ms(pct(&mut wins, 95)),
                ms(pct(&mut totals, 50)),
                ms(pct(&mut totals, 95)),
            );
        }
        drop(frozen);

        println!();
        println!("memory per pin (private MiB of this process; picture = its BGRA bytes)");
        println!(
            "size        | picture | before | with pin | at 200 % | destroyed | pin cost | budget (2× + 2 MB)"
        );
        for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
            idle(Duration::from_millis(300));
            let before = private_bytes();
            let frame = picture(&ledger, w, h);
            let mut pin: WinPin = pinwnd::create(
                PinId(1000),
                &frame,
                &spec(frame.rect()),
                Style::OFF_SCREEN_TEST,
            )
            .expect("a pin window");
            idle(Duration::from_millis(300));
            let with = private_bytes();
            pin.present(&frame, Rect::new(FAR, FAR, w * 2, h * 2))
                .expect("a zoomed window");
            idle(Duration::from_millis(300));
            let zoomed = private_bytes();
            drop(pin);
            drop(frame);
            idle(Duration::from_millis(300));
            let after = private_bytes();
            let bytes = u64::from(w) * u64::from(h) * 4;
            println!(
                "{w}×{h} | {} | {} | {} | {} | {} | {} | {}",
                mib(bytes),
                mib(before),
                mib(with),
                mib(zoomed),
                mib(after),
                signed_mib(before, with),
                mib(2 * bytes + 2 * 1024 * 1024),
            );
        }

        println!();
        idle(Duration::from_millis(300));
        let before = private_bytes();
        let frames: Vec<Frame> = (0..20).map(|_| picture(&ledger, 1920, 1080)).collect();
        let pins: Vec<WinPin> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| {
                pinwnd::create(
                    PinId(2000 + i as u64),
                    f,
                    &spec(f.rect()),
                    Style::OFF_SCREEN_TEST,
                )
                .expect("a pin window")
            })
            .collect();
        idle(Duration::from_millis(500));
        let with = private_bytes();
        let bytes = 20 * 1920 * 1080 * 4u64;
        println!(
            "twenty 1920×1080 pins: pictures {} MiB, private {} → {} MiB ({} MiB, {:.2}× the pictures)",
            mib(bytes),
            mib(before),
            mib(with),
            signed_mib(before, with),
            (with as f64 - before as f64) / bytes as f64
        );
        let cpu0 = cpu_time();
        let t0 = Instant::now();
        let messages = idle(Duration::from_secs(10));
        let cpu = cpu_time() - cpu0;
        println!(
            "idle with twenty pins for {:.1} s: {:.1} ms CPU, {messages} message(s) received",
            t0.elapsed().as_secs_f64(),
            cpu as f64 / 10_000.0
        );
        drop(pins);
        drop(frames);
        idle(Duration::from_millis(300));
        println!(
            "after closing them: private {} MiB; image bytes held: {}",
            mib(private_bytes()),
            ledger.live().1
        );
    }
}

fn main() {
    #[cfg(windows)]
    measure::main();
    #[cfg(not(windows))]
    eprintln!("pin_measure: Windows only (pins are not implemented elsewhere yet)");
}
