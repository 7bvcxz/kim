//! Off-screen tests of the Win32 pin windows (Steps 53 and 54).

use super::*;
use crate::frame::Ledger;
use std::time::Duration;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, SendMessageW, WS_EX_APPWINDOW,
};

/// Off-screen: far outside any real monitor arrangement (the user sees nothing).
const FAR: i32 = -20_000;

/// A view of `rect` without outline or shadow: the window is exactly the picture.
fn bare(rect: Rect) -> View {
    View {
        look: transform::Look {
            border: false,
            shadow: false,
        },
        ..View::plain(rect)
    }
}

fn spec(rect: Rect) -> WindowSpec {
    WindowSpec {
        view: bare(rect),
        focus: false,
        guard: None,
    }
}

/// Off-screen and never activated, but on top: z-order checks need real topmost windows,
/// and these lie where no monitor shows them.
const TOPMOST_TEST: Style = Style {
    topmost: true,
    activate: false,
};

fn ex_style(hwnd: HWND) -> u32 {
    // SAFETY: a query on our own window.
    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 }
}

fn rect_of(hwnd: HWND) -> (i32, i32, i32, i32) {
    let mut r = RECT::default();
    // SAFETY: a query on our own window; `r` is writable.
    unsafe { GetWindowRect(hwnd, &raw mut r) };
    (r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// Top-level windows from the top of the z-order down.
fn z_order() -> Vec<isize> {
    unsafe extern "system" fn each(h: HWND, data: LPARAM) -> i32 {
        // SAFETY: `data` is the `&mut Vec<isize>` passed below, alive during EnumWindows.
        unsafe { (*(data as *mut Vec<isize>)).push(h as isize) };
        1
    }
    let mut out: Vec<isize> = Vec::new();
    // SAFETY: the callback only touches `out`, passed by pointer for the duration.
    unsafe { EnumWindows(Some(each), (&raw mut out) as LPARAM) };
    out
}

/// A real pin window, created off-screen, never activated: the user sees nothing. Checks
/// styles, focus, key handling, double-click, the menu request, DPI changes and destruction.
#[test]
fn off_screen_pin_window_lifecycle() {
    let ledger = Ledger::new();
    let frame = Frame::solid(&ledger, Rect::new(FAR, FAR, 16, 12), [0, 128, 255, 255]).unwrap();
    let mut pin = create(PinId(77), &frame, &spec(frame.rect()), Style::INTERACTIVE).unwrap();
    let hwnd = pin.hwnd();
    assert_eq!(hwnd_of(PinId(77)), Some(hwnd));
    // SAFETY: queries on our own live window.
    unsafe {
        assert_ne!(IsWindow(hwnd), 0);
        assert_ne!(IsWindowVisible(hwnd), 0);
        let ex = ex_style(hwnd);
        assert_ne!(ex & WS_EX_TOOLWINDOW, 0, "no taskbar entry, not in Alt+Tab");
        assert_ne!(ex & WS_EX_TOPMOST, 0, "always on top");
        assert_ne!(ex & WS_EX_LAYERED, 0);
        assert_eq!(ex & WS_EX_APPWINDOW, 0);
        assert_eq!(
            ex & WS_EX_NOACTIVATE,
            0,
            "a click may give it focus (NOEUL-FR-060)"
        );
        assert_ne!(GetForegroundWindow(), hwnd, "a CLI pin never steals focus");
    }
    assert_eq!(rect_of(hwnd), (FAR, FAR, 16, 12));
    assert_eq!(activations(hwnd), Some(0));
    let _ = take_events();
    // SAFETY: synchronous messages to our own window on this thread.
    unsafe {
        SendMessageW(hwnd, WM_KEYDOWN, 0x1B, 0); // Escape
        SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
        SendMessageW(hwnd, WM_KEYDOWN, 0x25, 1 << 30); // Left, auto-repeat
        SendMessageW(hwnd, WM_KEYDOWN, 0xFF, 0); // no key name
        SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, -1); // the menu key
        let at = ((FAR as u16 as isize) & 0xFFFF) | ((FAR + 3) as u16 as isize) << 16;
        SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, at); // a right-click
        SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
        SendMessageW(
            hwnd,
            WM_MOUSEWHEEL,
            (120usize << 16) | MK_CONTROL as usize,
            0,
        );
        SendMessageW(hwnd, WM_MOUSEWHEEL, (0xFF88usize) << 16, 0); // -120
        SendMessageW(hwnd, WM_CLOSE, 0, 0);
        assert_ne!(
            IsWindow(hwnd),
            0,
            "WM_CLOSE leaves the decision to the core"
        );
        assert_eq!(SendMessageW(hwnd, WM_NCHITTEST, 0, 0), HTCAPTION as LRESULT);
        // A DPI change keeps the pixel size.
        let suggested = RECT {
            left: FAR,
            top: FAR,
            right: FAR + 24,
            bottom: FAR + 18,
        };
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            (144 << 16) | 144,
            (&raw const suggested) as LPARAM,
        );
    }
    assert_eq!(
        rect_of(hwnd),
        (FAR, FAR, 16, 12),
        "WM_DPICHANGED changes nothing"
    );
    let ev = take_events();
    let esc = chord_of("Escape", false, false, false).unwrap();
    let left = chord_of("Left", false, false, false).unwrap();
    assert_eq!(
        ev,
        [
            PinEvent::Key(PinId(77), esc, false),
            PinEvent::Key(PinId(77), left, true),
            PinEvent::Menu(PinId(77), None),
            PinEvent::Menu(PinId(77), Some((FAR, FAR + 3))),
            PinEvent::MiddleClick(PinId(77)),
            PinEvent::Wheel(PinId(77), 120, (0, 0), true),
            PinEvent::Wheel(PinId(77), -120, (0, 0), false),
            PinEvent::Closed(PinId(77)),
        ],
        "the double-click produced nothing (NOEUL-FR-026)"
    );
    pin.set_position(FAR - 100, FAR - 50);
    assert_eq!(rect_of(hwnd).0, FAR - 100);
    assert_eq!(pin.rect(), Some(Rect::new(FAR - 100, FAR - 50, 16, 12)));
    // Zoom: the window takes the new size and place, still not activated.
    pin.present(&frame, &bare(Rect::new(FAR, FAR, 32, 24)))
        .unwrap();
    assert_eq!(rect_of(hwnd), (FAR, FAR, 32, 24));
    assert!(
        pin.present(&frame, &bare(Rect::new(FAR, FAR, 20_000, 1)))
            .is_err(),
        "too large"
    );
    assert_eq!(activations(hwnd), Some(0), "nothing activated it");
    pin.destroy();
    // SAFETY: as above.
    assert_eq!(unsafe { IsWindow(hwnd) }, 0, "destroyed");
    assert_eq!(hwnd_of(PinId(77)), None);
    pin.destroy(); // idempotent
    drop(frame);
    assert_eq!(ledger.live(), (0, 0));
}

/// The tail of a double-click: for the guard time every press is swallowed and the window
/// refuses activation; afterwards a click may activate it again.
#[test]
fn a_new_pin_swallows_the_rest_of_the_gesture() {
    let ledger = Ledger::new();
    let frame = Frame::solid(&ledger, Rect::new(FAR, FAR, 8, 8), [1, 2, 3, 255]).unwrap();
    let guarded_spec = WindowSpec {
        guard: Some(Duration::from_millis(300)),
        ..spec(frame.rect())
    };
    let pin = create(PinId(91), &frame, &guarded_spec, Style::INTERACTIVE).unwrap();
    let hwnd = pin.hwnd();
    let _ = take_events();
    // SAFETY: synchronous messages to our own window on this thread.
    unsafe {
        assert_eq!(
            SendMessageW(hwnd, WM_MOUSEACTIVATE, hwnd as usize, 0),
            MA_NOACTIVATEANDEAT as LRESULT,
            "the third click of a triple click neither activates nor reaches the pin"
        );
        assert_eq!(
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0),
            0
        );
        SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
        SendMessageW(hwnd, WM_CONTEXTMENU, hwnd as usize, -1);
    }
    assert!(take_events().is_empty(), "nothing reached the core");
    std::thread::sleep(Duration::from_millis(350));
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            SendMessageW(hwnd, WM_MOUSEACTIVATE, hwnd as usize, 0),
            MA_ACTIVATE as LRESULT,
            "after the guard a click may activate it"
        );
        SendMessageW(hwnd, WM_NCMBUTTONUP, HTCAPTION as usize, 0);
    }
    assert_eq!(take_events(), [PinEvent::MiddleClick(PinId(91))]);
    drop(pin);
}

/// Several real topmost pins, off-screen: each is independent, the newest is on top, hiding
/// and showing one leaves the others alone, a shown one returns to the top of the topmost
/// band, and no window is ever activated or made the foreground window.
#[test]
fn independent_topmost_pins_keep_their_z_order_and_never_take_focus() {
    let ledger = Ledger::new();
    // SAFETY: a plain query.
    let fg_before = unsafe { GetForegroundWindow() };
    let frames: Vec<Frame> = (0..3)
        .map(|i| {
            Frame::solid(
                &ledger,
                Rect::new(FAR + i * 40, FAR, 30, 20),
                [i as u8, 0, 0, 255],
            )
            .unwrap()
        })
        .collect();
    let mut pins: Vec<WinPin> = frames
        .iter()
        .enumerate()
        .map(|(i, f)| create(PinId(200 + i as u64), f, &spec(f.rect()), TOPMOST_TEST).unwrap())
        .collect();
    let h: Vec<HWND> = pins.iter().map(WinPin::hwnd).collect();
    // A normal (not topmost) window of ours, also off-screen.
    let normal_frame = Frame::solid(&ledger, Rect::new(FAR, FAR + 100, 10, 10), [0; 4]).unwrap();
    let normal = create(
        PinId(299),
        &normal_frame,
        &spec(normal_frame.rect()),
        Style::OFF_SCREEN_TEST,
    )
    .unwrap();
    let pos = |order: &[isize], w: HWND| order.iter().position(|x| *x == w as isize).unwrap();
    let z = z_order();
    assert!(
        pos(&z, h[2]) < pos(&z, h[1]) && pos(&z, h[1]) < pos(&z, h[0]),
        "newest on top"
    );
    assert!(
        pos(&z, h[0]) < pos(&z, normal.hwnd()),
        "pins lie above normal windows"
    );
    assert_eq!(ex_style(normal.hwnd()) & WS_EX_TOPMOST, 0);
    assert_ne!(
        ex_style(normal.hwnd()) & WS_EX_NOACTIVATE,
        0,
        "the test style never activates"
    );
    // Hide the oldest: only it disappears.
    pins[0].set_visible(false);
    // SAFETY: queries on our own windows.
    unsafe {
        assert_eq!(IsWindowVisible(h[0]), 0);
        assert_ne!(IsWindowVisible(h[1]), 0);
        assert_ne!(IsWindowVisible(h[2]), 0);
    }
    // Show it again: it comes back on top of the other pins, without activation.
    pins[0].set_visible(true);
    let z = z_order();
    assert!(
        pos(&z, h[0]) < pos(&z, h[2]),
        "a shown pin returns to the top"
    );
    assert_ne!(ex_style(h[0]) & WS_EX_TOPMOST, 0, "still topmost");
    // Move and zoom the middle one; the others keep their rectangles.
    pins[1].set_position(FAR + 500, FAR + 7);
    pins[1]
        .present(&frames[1], &bare(Rect::new(FAR + 500, FAR + 7, 60, 40)))
        .unwrap();
    assert_eq!(rect_of(h[1]), (FAR + 500, FAR + 7, 60, 40));
    assert_eq!(rect_of(h[0]), (FAR, FAR, 30, 20));
    assert_eq!(rect_of(h[2]), (FAR + 80, FAR, 30, 20));
    // Close the middle one: the others stay alive and visible.
    pins[1].destroy();
    // SAFETY: queries on our own windows.
    unsafe {
        assert_eq!(IsWindow(h[1]), 0);
        assert_ne!(IsWindowVisible(h[0]), 0);
        assert_ne!(IsWindowVisible(h[2]), 0);
    }
    for w in [h[0], h[2]] {
        assert_eq!(activations(w), Some(0), "never activated");
    }
    // SAFETY: a plain query.
    let fg_after = unsafe { GetForegroundWindow() };
    assert!(
        !h.contains(&fg_after) && fg_after != normal.hwnd(),
        "no pin became the foreground window"
    );
    if fg_after != fg_before {
        eprintln!("the foreground window changed during the test (the user switched apps)");
    }
    drop(pins);
    drop(normal);
    drop(frames);
    drop(normal_frame);
    assert_eq!(ledger.live(), (0, 0));
}

/// The picture really reaches the window, and stays there after Noeul's temporary DIB was
/// freed: DWM renders the layered window's retained surface into a bitmap of ours
/// (`PrintWindow` with `PW_RENDERFULLCONTENT`), at 100 % and zoomed.
#[test]
fn the_window_keeps_showing_the_pictures_pixels_after_the_upload() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> i32;
    }
    const PW_RENDERFULLCONTENT: u32 = 2;
    let ledger = Ledger::new();
    let (w, h) = (8u32, 6u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[x as u8 * 30, y as u8 * 40, 200, 255]);
        }
    }
    let frame = Frame::new(&ledger, Rect::new(FAR, FAR, w, h), px).unwrap();
    let mut pin = create(
        PinId(301),
        &frame,
        &spec(frame.rect()),
        Style::OFF_SCREEN_TEST,
    )
    .unwrap();
    let shot = |pin: &WinPin, w: u32, h: u32| -> Option<Vec<u8>> {
        let mut s = Surface::new(w, h).unwrap();
        // SAFETY: our window renders into our own DIB section of its size.
        let ok = unsafe { PrintWindow(pin.hwnd(), s.dc, PW_RENDERFULLCONTENT) } != 0;
        ok.then(|| s.pixels().to_vec())
    };
    let Some(got) = shot(&pin, w, h) else {
        eprintln!("SKIP: PrintWindow could not render the window on this system");
        return;
    };
    let rgb = |b: &[u8], i: usize| [b[i * 4], b[i * 4 + 1], b[i * 4 + 2]];
    for i in 0..(w * h) as usize {
        assert_eq!(rgb(&got, i), rgb(frame.pixels(), i), "pixel {i} at 100 %");
    }
    // 200 %: every picture pixel is a 2×2 block of the window.
    pin.present(&frame, &bare(Rect::new(FAR, FAR, w * 2, h * 2)))
        .unwrap();
    let got = shot(&pin, w * 2, h * 2).expect("rendered");
    for (x, y) in [(0u32, 0u32), (3, 2), (7, 5)] {
        let src = (y * w + x) as usize;
        for (dx, dy) in [(0, 0), (1, 1)] {
            let dst = ((y * 2 + dy) * w * 2 + x * 2 + dx) as usize;
            assert_eq!(
                rgb(&got, dst),
                rgb(frame.pixels(), src),
                "({x},{y}) at 200 %"
            );
        }
    }
    drop(pin);
}

#[test]
fn key_names_follow_the_contract() {
    assert_eq!(vk_name(0x41).as_deref(), Some("A"));
    assert_eq!(vk_name(0x37).as_deref(), Some("7"));
    assert_eq!(vk_name(0x70).as_deref(), Some("F1"));
    assert_eq!(vk_name(0x87).as_deref(), Some("F24"));
    assert_eq!(vk_name(0x1B).as_deref(), Some("Escape"));
    assert_eq!(vk_name(0x60).as_deref(), Some("Numpad0"));
    assert_eq!(vk_name(0x5B), None, "Win keys are not names");
    for vk in (0x08..=0xFE).filter_map(vk_name) {
        assert!(chord_of(&vk, false, false, false).is_some(), "{vk} parses");
    }
    assert_eq!(point_of(0xFFFF_FFFFu32 as isize), (-1, -1));
    assert_eq!(point_of(((-5i16 as u16 as isize) << 16) | 7), (7, -5));
}

// ---- Step 54 (ADR 0044): outline, shadow, opacity, click-through, border resizing ----

/// Packs a point into a mouse message's `lParam`.
fn lp(x: i32, y: i32) -> LPARAM {
    ((x as u16 as isize) & 0xFFFF) | ((y as u16 as isize) << 16)
}

/// Click-through: the system hit-tests through the window (`WS_EX_TRANSPARENT`) to whatever is
/// below it, and stops again when it is turned off; the keyboard still reaches the pin, which
/// shows the badge meanwhile. Only this test's own off-screen window is involved.
#[test]
fn a_click_through_window_lets_the_mouse_pass_and_comes_back() {
    use windows_sys::Win32::UI::WindowsAndMessaging::WindowFromPoint;
    let ledger = Ledger::new();
    let (x0, y0) = (FAR - 3_000, FAR + 50);
    let frame = Frame::solid(&ledger, Rect::new(x0, y0, 40, 30), [9, 9, 9, 255]).unwrap();
    let mut pin = create(
        PinId(401),
        &frame,
        &spec(frame.rect()),
        Style::OFF_SCREEN_TEST,
    )
    .unwrap();
    let hwnd = pin.hwnd();
    let probe = POINT {
        x: x0 + 20,
        y: y0 + 15,
    };
    // SAFETY: a plain query of the window under a point (off every monitor).
    let hit = || unsafe { WindowFromPoint(probe) };
    assert_eq!(hit(), hwnd, "the pin is hit where it is");
    let mut view = bare(frame.rect());
    view.click_through = true;
    pin.present(&frame, &view).unwrap();
    assert_ne!(ex_style(hwnd) & WS_EX_TRANSPARENT, 0);
    assert_ne!(hit(), hwnd, "the mouse goes through the pin");
    assert!(view_of(hwnd).unwrap().click_through, "it shows the badge");
    let _ = take_events();
    // SAFETY: a synchronous message to our own window on this thread.
    unsafe { SendMessageW(hwnd, WM_KEYDOWN, 0x54, 0) }; // T
    assert_eq!(
        take_events(),
        [PinEvent::Key(
            PinId(401),
            chord_of("T", false, false, false).unwrap(),
            false
        )],
        "the keyboard still reaches it"
    );
    view.click_through = false;
    pin.present(&frame, &view).unwrap();
    assert_eq!(ex_style(hwnd) & WS_EX_TRANSPARENT, 0);
    assert_eq!(hit(), hwnd, "clickable again");
    assert_eq!(activations(hwnd), Some(0));
    drop(pin);
}

/// The border band: hit-testing finds the sides and corners of the outline and shadow while the
/// picture's middle moves the pin; pressing the border and moving resizes through the window's
/// own messages (no system sizing loop, and no mouse capture under the test hook); a thumbnail
/// has no band.
#[test]
fn the_border_band_resizes_through_the_windows_own_messages() {
    let ledger = Ledger::new();
    let r = Rect::new(FAR, FAR + 300, 100, 60);
    let frame = Frame::solid(&ledger, r, [1, 2, 3, 255]).unwrap();
    let view = View::plain(r);
    let m = view.margin() as i32;
    assert_eq!(m, 13, "one outline pixel and the 12-pixel shadow at 100 %");
    let mut pin = create(
        PinId(402),
        &frame,
        &WindowSpec {
            view,
            focus: false,
            guard: None,
        },
        Style::OFF_SCREEN_TEST,
    )
    .unwrap();
    let hwnd = pin.hwnd();
    assert_eq!(
        rect_of(hwnd),
        (r.x - m, r.y - m, 100 + 2 * m, 60 + 2 * m),
        "the window holds the outline and the shadow"
    );
    assert_eq!(pin.rect(), Some(r), "the pin's rectangle is the picture's");
    // SAFETY: synchronous messages to our own window on this thread.
    let ht = |x: i32, y: i32| unsafe { SendMessageW(hwnd, WM_NCHITTEST, 0, lp(x, y)) };
    let code = |c: u32| c as LRESULT;
    assert_eq!(ht(r.x + 50, r.y + 30), code(HTCAPTION), "the middle moves");
    assert_eq!(ht(r.x - 5, r.y + 30), code(HTLEFT), "the shadow resizes");
    assert_eq!(
        ht(r.x + 98, r.y + 30),
        code(HTRIGHT),
        "so does the band inside"
    );
    assert_eq!(ht(r.x + 50, r.y + 62), code(HTBOTTOM));
    assert_eq!(ht(r.x - 10, r.y - 10), code(HTTOPLEFT));
    assert_eq!(ht(r.x + 108, r.y + 66), code(HTBOTTOMRIGHT));
    let _ = take_events();
    let (ox, oy) = (r.x - m, r.y - m);
    // SAFETY: synchronous messages to our own window on this thread.
    unsafe {
        SendMessageW(
            hwnd,
            WM_NCLBUTTONDOWN,
            HTRIGHT as usize,
            lp(r.x + 101, r.y + 30),
        );
        // Client coordinates while the border is held.
        SendMessageW(hwnd, WM_MOUSEMOVE, 0, lp(r.x + 151 - ox, r.y + 30 - oy));
        SendMessageW(hwnd, WM_LBUTTONUP, 0, lp(r.x + 161 - ox, r.y + 32 - oy));
        SendMessageW(hwnd, WM_MOUSEMOVE, 0, lp(5, 5));
        assert_ne!(
            windows_sys::Win32::UI::Input::KeyboardAndMouse::GetCapture(),
            hwnd,
            "the test hook's windows never take the mouse"
        );
    }
    assert_eq!(
        take_events(),
        [
            PinEvent::Resize(PinId(402), Edge::Right, r, (50, 0)),
            PinEvent::Resize(PinId(402), Edge::Right, r, (60, 2)),
        ],
        "after the release a move is just a move"
    );
    // The core answers with the new size: the window follows, the band with it.
    let bigger = Rect::new(r.x, r.y, 160, 96);
    pin.present(&frame, &View::plain(bigger)).unwrap();
    assert_eq!(pin.rect(), Some(bigger));
    assert_eq!(ht(bigger.x + 158, bigger.y + 50), code(HTRIGHT));
    // A thumbnail has no grab band: its shadow moves it like its picture.
    let thumb = View {
        resizable: false,
        ..View::plain(Rect::new(r.x, r.y, 48, 30))
    };
    pin.present(&frame, &thumb).unwrap();
    assert_eq!(ht(r.x - 5, r.y + 10), code(HTCAPTION));
    // SAFETY: as above.
    unsafe {
        SendMessageW(
            hwnd,
            WM_NCLBUTTONDOWN,
            HTLEFT as usize,
            lp(r.x - 5, r.y + 10),
        )
    };
    drop(pin);
}

/// The modifiers come from this test thread's own key-state table (`SetKeyboardState` changes
/// only the calling thread's view of the keys; no input is generated): `Shift`+double-click
/// toggles thumbnail mode, a plain double-click does nothing (NOEUL-FR-026), a move that ends
/// with `Ctrl` held is a drop onto whatever pin lies there.
#[test]
fn shift_double_click_and_ctrl_drop_are_reported() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState};
    let ledger = Ledger::new();
    let r = Rect::new(FAR, FAR + 600, 30, 20);
    let frame = Frame::solid(&ledger, r, [1, 2, 3, 255]).unwrap();
    let pin = create(PinId(403), &frame, &spec(r), Style::OFF_SCREEN_TEST).unwrap();
    let hwnd = pin.hwnd();
    let _ = take_events();
    let mut keys = [0u8; 256];
    // SAFETY: this thread's key-state table, read, changed and restored below; our window.
    unsafe {
        GetKeyboardState(keys.as_mut_ptr());
        let saved = keys;
        keys[usize::from(VK_SHIFT)] = 0x80;
        SetKeyboardState(keys.as_ptr());
        SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
        keys[usize::from(VK_SHIFT)] = 0;
        keys[usize::from(VK_CONTROL)] = 0x80;
        SetKeyboardState(keys.as_ptr());
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
        SetKeyboardState(saved.as_ptr());
        SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
        // The debug-build test hook's instance reads no key state: a Shift and Ctrl the user
        // holds elsewhere change nothing.
        ignore_key_state(true);
        keys[usize::from(VK_SHIFT)] = 0x80;
        SetKeyboardState(keys.as_ptr());
        SendMessageW(hwnd, WM_NCLBUTTONDBLCLK, HTCAPTION as usize, 0);
        SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0);
        ignore_key_state(false);
        SetKeyboardState(saved.as_ptr());
    }
    assert_eq!(
        take_events(),
        [
            PinEvent::ShiftDoubleClick(PinId(403)),
            PinEvent::Dropped(PinId(403), r.x, r.y),
            PinEvent::Moved(PinId(403), r.x, r.y),
            PinEvent::Moved(PinId(403), r.x, r.y),
        ]
    );
    drop(pin);
}

/// Opacity changes only the constant alpha of the retained surface: the call succeeds without a
/// new picture, and a later present keeps the new opacity.
#[test]
fn opacity_changes_only_the_constant_alpha() {
    let ledger = Ledger::new();
    let r = Rect::new(FAR, FAR + 900, 20, 10);
    let frame = Frame::solid(&ledger, r, [200, 100, 50, 255]).unwrap();
    let mut pin = create(PinId(404), &frame, &spec(r), Style::OFF_SCREEN_TEST).unwrap();
    let hwnd = pin.hwnd();
    pin.set_opacity(40);
    assert_eq!(
        view_of(hwnd).unwrap().opacity,
        40,
        "UpdateLayeredWindow accepted it"
    );
    pin.set_opacity(10);
    assert_eq!(view_of(hwnd).unwrap().opacity, 10);
    let mut v = bare(r);
    v.opacity = 60;
    pin.present(&frame, &v).unwrap();
    assert_eq!(view_of(hwnd).unwrap().opacity, 60);
    assert_eq!(rect_of(hwnd), (r.x, r.y, 20, 10), "nothing moved");
    drop(pin);
}

/// What reaches the screen: DWM renders the window (`PrintWindow`) with the picture's exact
/// pixels inside the one-pixel `border.subtle` outline, at 100 % and at 150 % DPI.
#[test]
fn the_outline_surrounds_the_exact_picture_on_screen() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> i32;
    }
    const PW_RENDERFULLCONTENT: u32 = 2;
    let ledger = Ledger::new();
    let (w, h) = (8u32, 6u32);
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[x as u8 * 30, y as u8 * 40, 200, 255]);
        }
    }
    let frame = Frame::new(&ledger, Rect::new(FAR + 1_000, FAR, w, h), px).unwrap();
    for dpi in [96, 144] {
        let view = View {
            dpi,
            look: transform::Look {
                border: true,
                shadow: false,
            },
            ..View::plain(frame.rect())
        };
        let pin = create(
            PinId(405),
            &frame,
            &WindowSpec {
                view,
                focus: false,
                guard: None,
            },
            Style::OFF_SCREEN_TEST,
        )
        .unwrap();
        let win = view.window();
        let mut s = Surface::new(win.width, win.height).unwrap();
        // SAFETY: our window renders into our own DIB section of its size.
        let ok = unsafe { PrintWindow(pin.hwnd(), s.dc, PW_RENDERFULLCONTENT) } != 0;
        if !ok {
            eprintln!("SKIP: PrintWindow could not render the window on this system");
            return;
        }
        let got = s.pixels().to_vec();
        let m = view.margin();
        let rgb = |b: &[u8], i: usize| [b[i * 4], b[i * 4 + 1], b[i * 4 + 2]];
        for y in 0..h {
            for x in 0..w {
                let i = ((y + m) * win.width + x + m) as usize;
                let src = (y * w + x) as usize;
                assert_eq!(rgb(&got, i), rgb(frame.pixels(), src), "({x},{y}) at {dpi}");
            }
        }
        let edge = (2 * win.width) as usize; // row 2, column 0: the outline
        assert_eq!(
            rgb(&got, edge),
            [
                transform::BORDER_BGRA[0],
                transform::BORDER_BGRA[1],
                transform::BORDER_BGRA[2]
            ],
            "the outline at {dpi}"
        );
        assert_eq!(m, (dpi / 96).max(1), "one outline pixel per 100 % of scale");
        drop(pin);
    }
}

/// A drop on a pin (Step 55, NOEUL-FR-073) through the real drop handler, with a synthetic
/// `HDROP` built in memory (a `DROPFILES` header and the wide paths): no drag, no input. The
/// handler reports every path and the drop point in screen coordinates, and frees the drop.
#[test]
fn files_dropped_on_a_pin_are_reported_with_the_point() {
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    let ledger = Ledger::new();
    let r = Rect::new(FAR, FAR + 1200, 40, 30);
    let frame = Frame::solid(&ledger, r, [5, 6, 7, 255]).unwrap();
    let pin = create(PinId(405), &frame, &spec(r), Style::OFF_SCREEN_TEST).unwrap();
    let hwnd = pin.hwnd();
    // SAFETY: a query on our own window.
    assert_ne!(
        ex_style(hwnd) & windows_sys::Win32::UI::WindowsAndMessaging::WS_EX_ACCEPTFILES,
        0
    );
    let _ = take_events();
    let files = [r"C:\shots\a.png", r"C:\shots\노을 b.gif"];
    let mut wide: Vec<u16> = Vec::new();
    for f in files {
        wide.extend(f.encode_utf16());
        wide.push(0);
    }
    wide.push(0);
    // DROPFILES: pFiles, pt, fNC, fWide.
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(&20u32.to_le_bytes());
    bytes.extend_from_slice(&7i32.to_le_bytes());
    bytes.extend_from_slice(&9i32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for u in &wide {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    // SAFETY: a movable block of our own, filled while locked, handed to our own window, which
    // frees it with DragFinish.
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        assert!(!h.is_null());
        let p = GlobalLock(h);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
        GlobalUnlock(h);
        SendMessageW(hwnd, WM_DROPFILES, h as usize, 0);
    }
    assert_eq!(
        take_events(),
        [PinEvent::Files(
            PinId(405),
            files.iter().map(std::path::PathBuf::from).collect(),
            (r.x + 7, r.y + 9),
        )]
    );
    drop(pin);
}
