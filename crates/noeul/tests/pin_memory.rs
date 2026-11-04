//! Large-image memory of real pin windows (Step 53, ADR 0029; NOEUL-NFR-002: "a pin costs at
//! most twice its pixel bytes plus 2 MB", NOEUL-SEC-005).
//!
//! The only test of this binary, so the process's private bytes are not disturbed by other tests.
//! The windows lie at -20000,-20000 (no monitor there), are never activated and never on top; the
//! picture is a generated pattern, nothing reads the screen and nothing is written.
#![cfg(windows)]
#![allow(unsafe_code)]

use noeul::frame::{Frame, Ledger, Rect};
use noeul::pins::{PinId, PinWindow, WindowSpec};
use noeul::win::pinwnd::{self, Style};

const MIB: u64 = 1024 * 1024;

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

#[test]
fn a_4k_pin_costs_its_picture_once_and_leaves_nothing_behind() {
    let ledger = Ledger::new();
    let (w, h) = (3840u32, 2160u32);
    let bytes = u64::from(w) * u64::from(h) * 4;
    // Warm up the window class, GDI and the allocator with a small pin first.
    let small = Frame::solid(&ledger, Rect::new(-20_000, -20_000, 4, 4), [1, 2, 3, 255]).unwrap();
    drop(
        pinwnd::create(
            PinId(1),
            &small,
            &WindowSpec {
                rect: small.rect(),
                focus: false,
                guard: None,
            },
            Style::OFF_SCREEN_TEST,
        )
        .unwrap(),
    );
    drop(small);
    let before = private_bytes();

    let frame = Frame::solid(&ledger, Rect::new(-20_000, -20_000, w, h), [9, 8, 7, 255]).unwrap();
    let mut pin = pinwnd::create(
        PinId(2),
        &frame,
        &WindowSpec {
            rect: frame.rect(),
            focus: false,
            guard: None,
        },
        Style::OFF_SCREEN_TEST,
    )
    .unwrap();
    let with = private_bytes();
    let cost = with.saturating_sub(before);
    eprintln!(
        "4K pin: picture {:.1} MiB, process cost {:.1} MiB",
        bytes as f64 / MIB as f64,
        cost as f64 / MIB as f64
    );
    assert!(
        cost >= bytes,
        "the picture itself is held: {cost} < {bytes}"
    );
    assert!(
        cost <= 2 * bytes + 2 * MIB,
        "NOEUL-NFR-002: at most twice the pixel bytes plus 2 MB, got {cost}"
    );
    // Zooming renders a temporary surface of the new size and frees it again.
    pin.present(&frame, Rect::new(-20_000, -20_000, w * 2, h * 2))
        .unwrap();
    let zoomed = private_bytes();
    assert!(
        zoomed <= with + 2 * MIB,
        "the zoomed surface is not kept in the process: {zoomed} vs {with}"
    );
    assert_eq!(
        ledger.live(),
        (1, bytes),
        "one picture, never copied by zooming"
    );

    drop(pin);
    drop(frame);
    let after = private_bytes();
    assert!(
        after <= before + 2 * MIB,
        "closing gives the memory back: {after} vs {before}"
    );
    assert_eq!(ledger.live(), (0, 0));
}
