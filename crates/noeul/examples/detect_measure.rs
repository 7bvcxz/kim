//! Measures Step 52's detection costs on this machine (ADR 0035 §Measurements):
//!
//! - the window list ([`noeul::win::detect::snapshot`]) over `--windows N` of this process's own
//!   off-screen windows (the list is restricted to this process, so the user's windows are only
//!   enumerated as handles and their owner's process id is read — nothing else of them);
//! - one UI Automation element lookup on this process's own window with `--buttons N` child
//!   buttons (the deepest control under a point), answered by the worker thread while this
//!   thread pumps the window's messages.
//!
//! Everything lies far off-screen, is never activated and is destroyed at the end; no input is
//! synthesized. `cargo run --release -p noeul --example detect_measure -- --runs 50`.

#[cfg(not(windows))]
fn main() {
    eprintln!("detect_measure runs on Windows only");
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn main() {
    use noeul::detect::{ElementAnswer, ElementRequest};
    use noeul::frame::Rect;
    use noeul::win::detect::{ElementWorker, Scope, snapshot};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetWindowRect, MSG,
        PM_REMOVE, PeekMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage,
        WNDCLASSEXW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
    };

    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str, default: usize| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let (runs, count, buttons) = (
        arg("--runs", 50),
        arg("--windows", 100),
        arg("--buttons", 40),
    );
    noeul::win::per_monitor_dpi_awareness();
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let class = wide("NoeulDetectMeasure");
    let wc = WNDCLASSEXW {
        cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap(),
        lpfnWndProc: Some(DefWindowProcW),
        // SAFETY: this executable's module handle.
        hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: a fully initialized class description.
    unsafe { RegisterClassExW(&raw const wc) };
    let (x0, y0) = (-22_000, -22_000);
    let make = |x: i32, y: i32, w: i32, h: i32, style: u32, parent: HWND, cls: &[u16]| -> HWND {
        // SAFETY: plain values; our class or a system control class; no activation.
        unsafe {
            let h = CreateWindowExW(
                if parent.is_null() {
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE
                } else {
                    0
                },
                cls.as_ptr(),
                std::ptr::null(),
                style,
                x,
                y,
                w,
                h,
                parent,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            assert!(!h.is_null());
            if parent.is_null() {
                ShowWindow(h, SW_SHOWNOACTIVATE);
            }
            h
        }
    };
    let pump = || {
        // SAFETY: MSG is plain data; this thread's own messages are dispatched.
        unsafe {
            let mut m: MSG = std::mem::zeroed();
            while PeekMessageW(&raw mut m, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&raw const m);
                DispatchMessageW(&raw const m);
            }
        }
    };
    let pct = |v: &mut Vec<f64>, p: usize| {
        v.sort_by(f64::total_cmp);
        v[((p * v.len()).div_ceil(100)).max(1) - 1]
    };

    // The window list.
    let mut windows = Vec::new();
    for i in 0..count {
        let i = i32::try_from(i).unwrap();
        windows.push(make(
            x0 + (i % 20) * 60,
            y0 + (i / 20) * 60,
            50,
            50,
            WS_POPUP,
            std::ptr::null_mut(),
            &class,
        ));
    }
    pump();
    let picture = Rect::new(x0, y0, 2000, 2000);
    let scope = Scope::Processes(vec![std::process::id()]);
    let mut list = Vec::new();
    let mut offered = 0;
    for _ in 0..runs {
        let t = Instant::now();
        let s = snapshot(picture, &scope, None);
        list.push(t.elapsed().as_secs_f64() * 1000.0);
        offered = s.len();
    }
    println!(
        "window list: {offered} own windows offered; p50 {:.3} ms, p95 {:.3} ms over {runs} runs",
        pct(&mut list, 50),
        pct(&mut list, 95)
    );

    // One element lookup on our own window with child buttons.
    let top = make(
        x0,
        y0 + 1500,
        900,
        400,
        WS_POPUP,
        std::ptr::null_mut(),
        &class,
    );
    let button = wide("BUTTON");
    let mut kids = Vec::new();
    for i in 0..buttons {
        let i = i32::try_from(i).unwrap();
        kids.push(make(
            10 + (i % 8) * 110,
            10 + (i / 8) * 70,
            100,
            60,
            WS_CHILD | WS_VISIBLE,
            top,
            &button,
        ));
    }
    pump();
    let rect = |h: HWND| {
        let mut r = RECT::default();
        // SAFETY: our own window; `r` is writable.
        unsafe { GetWindowRect(h, &raw mut r) };
        Rect::new(
            r.left,
            r.top,
            u32::try_from(r.right - r.left).unwrap(),
            u32::try_from(r.bottom - r.top).unwrap(),
        )
    };
    let window = rect(top);
    let target = rect(*kids.last().unwrap());
    let worker = ElementWorker::start(Box::new(|| {})).unwrap();
    let mut lookups = Vec::new();
    let mut found = 0;
    for seq in 1..=u64::try_from(runs).unwrap() {
        let t = Instant::now();
        worker.ask(ElementRequest {
            seq,
            token: top as usize as u64,
            window,
            at: (target.x + 50, target.y + 30),
        });
        let until = t + Duration::from_secs(5);
        loop {
            pump();
            if let Some((_, a)) = worker
                .take_answers()
                .into_iter()
                .find(|(r, _)| r.seq == seq)
            {
                if a == ElementAnswer::Found(target) {
                    found += 1;
                }
                break;
            }
            assert!(Instant::now() < until, "no answer");
            std::hint::spin_loop();
        }
        lookups.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "element lookup: {found}/{runs} found the button among {buttons}; p50 {:.2} ms, p95 {:.2} ms (request to answer, including the first lookup's UI Automation start)",
        pct(&mut lookups, 50),
        pct(&mut lookups, 95)
    );
    drop(worker);
    for h in kids.into_iter().chain([top]).chain(windows) {
        // SAFETY: our own windows, destroyed once.
        unsafe { DestroyWindow(h) };
    }
}
