//! Client view conformance (Step 15, ADR 0036, docs/TERMINAL_EMULATOR.md §3–§4): copy and paste,
//! keys, mouse reporting and local mouse behaviour, focus, hyperlinks and the scrollback
//! viewport — all without a window. `SUBAK-FR-013`, `SUBAK-FR-031`, `SUBAK-SEC-013`,
//! `KIM-NFR-013`.

use subak_term::input::{Win32Key, encode_key, encode_mouse, encode_paste, encode_win32_key};
use subak_term::screen::{Limits, ModeState, Screen};
use subak_term::{
    Key, Mods, MouseAction, MouseButton, MouseEvent, MouseOutcome, SelectMode, TermView,
};

fn view(cols: u16, rows: u16, bytes: &[u8]) -> TermView {
    let mut v = TermView::new(cols, rows, 1000);
    v.feed(bytes);
    v
}

fn modes(bytes: &[u8]) -> ModeState {
    let mut s = Screen::new(10, 2, Limits::default());
    s.feed(bytes);
    s.modes()
}

fn rows(v: &TermView) -> Vec<String> {
    (0..v.size().1)
        .map(|r| v.row_at(r).text().trim_end().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Copy.

#[test]
fn copy_cells_wide_characters_and_combining_marks() {
    let v0 = view(20, 3, "abc 한글 e\u{301}nd   \r\nsecond".as_bytes());
    let mut v = v0;
    // From the middle of "abc" to the tail cell of 글: the whole wide character is copied once.
    v.select(1, 0, SelectMode::Cell);
    v.extend_selection(7, 0);
    assert_eq!(v.selected_text().as_deref(), Some("bc 한글"));
    // Starting on a tail cell includes its character.
    v.select(5, 0, SelectMode::Cell);
    v.extend_selection(12, 0);
    assert_eq!(v.selected_text().as_deref(), Some("한글 e\u{301}nd"));
    // Across lines: trailing blanks of a line are dropped, lines end with \n; reversed drag.
    v.select(5, 1, SelectMode::Cell);
    v.extend_selection(9, 0);
    assert_eq!(v.selected_text().as_deref(), Some("e\u{301}nd\nsecond"));
    assert_eq!(v.selection_cols(0), Some((9, 19)));
    assert_eq!(v.selection_cols(1), Some((0, 5)));
    assert_eq!(v.selection_cols(2), None);
    v.clear_selection();
    assert_eq!(v.selected_text(), None);
}

#[test]
fn copy_joins_soft_wrapped_rows_but_not_hard_line_breaks() {
    let mut v = view(10, 4, b"0123456789abcdef\r\nnext line");
    v.select(0, 0, SelectMode::Line);
    assert_eq!(v.selected_text().as_deref(), Some("0123456789abcdef"));
    // Line mode expands to the whole logical line from its second row too.
    v.select(3, 1, SelectMode::Line);
    assert_eq!(v.selected_text().as_deref(), Some("0123456789abcdef"));
    v.select(0, 0, SelectMode::Cell);
    v.extend_selection(3, 2);
    assert_eq!(v.selected_text().as_deref(), Some("0123456789abcdef\nnext"));
}

#[test]
fn word_and_block_selection() {
    let mut v = view(
        40,
        3,
        "cd C:\\src\\subak; echo 수박-터미널 done\r\nabcdef\r\nghijkl".as_bytes(),
    );
    // A word ends at the delimiters ` \ ; -`; Korean is a word.
    v.select(7, 0, SelectMode::Word);
    assert_eq!(v.selected_text().as_deref(), Some("src"));
    // On a delimiter, the run of delimiters.
    v.select(5, 0, SelectMode::Word);
    assert_eq!(v.selected_text().as_deref(), Some(":\\"));
    v.select(23, 0, SelectMode::Word);
    assert_eq!(v.selected_text().as_deref(), Some("수박"));
    v.select(8, 0, SelectMode::Word);
    v.extend_selection(13, 0);
    assert_eq!(v.selected_text().as_deref(), Some("src\\subak"));
    // A block is the same columns on every line.
    v.select(1, 1, SelectMode::Block);
    v.extend_selection(3, 2);
    assert_eq!(v.selected_text().as_deref(), Some("bcd\nhij"));
    assert_eq!(v.selection_cols(1), Some((1, 3)));
    assert_eq!(v.selection_cols(2), Some((1, 3)));
}

#[test]
fn selection_in_the_history_survives_output_and_scrolling() {
    let mut v = TermView::new(12, 3, 100);
    for i in 0..10 {
        v.feed(format!("line {i}\r\n").as_bytes());
    }
    // The screen shows "line 8", "line 9" and the prompt row; the history holds lines 0–7.
    // Scroll back five lines to see lines 3–5 and select "line 4".
    v.scroll_lines(5);
    assert_eq!(rows(&v), ["line 3", "line 4", "line 5"]);
    v.select(0, 1, SelectMode::Line);
    assert_eq!(v.selected_text().as_deref(), Some("line 4"));
    // Output arrives: the scrolled-back viewport stays on the same lines, the selection too.
    v.feed(b"more 1\r\nmore 2\r\n");
    assert_eq!(rows(&v), ["line 3", "line 4", "line 5"]);
    assert_eq!(v.scroll_offset(), 7);
    assert_eq!(v.selected_text().as_deref(), Some("line 4"));
    assert_eq!(v.selection_cols(1), Some((0, 11)));
    // Typing returns to the live screen and clears the selection.
    let bytes = v.key(Key::Char('x'), Mods::NONE);
    assert_eq!(bytes, b"x");
    assert_eq!(v.scroll_offset(), 0);
    assert!(!v.has_selection());
    assert_eq!(rows(&v), ["more 1", "more 2", ""]);
}

#[test]
fn the_viewport_is_bounded_and_the_alternate_screen_has_none() {
    let mut v = TermView::new(10, 2, 3);
    for i in 0..8 {
        v.feed(format!("{i}\r\n").as_bytes());
    }
    v.scroll_lines(100);
    assert_eq!(v.scroll_offset(), 3, "only the kept history");
    assert_eq!(rows(&v), ["4", "5"]);
    v.scroll_lines(-1);
    assert_eq!(rows(&v), ["5", "6"]);
    // Lines evicted from the history take a selection on them away.
    v.select(0, 0, SelectMode::Line);
    v.feed(b"a\r\nb\r\nc\r\nd\r\n");
    assert!(!v.has_selection());
    // The alternate screen: no scrollback; switching screens resets the viewport.
    v.scroll_lines(2);
    v.feed(b"\x1b[?1049hTUI");
    assert_eq!(v.scroll_offset(), 0);
    v.scroll_lines(5);
    assert_eq!(v.scroll_offset(), 0);
    assert_eq!(
        v.cursor_in_view(),
        Some((3, 1)),
        "1049 keeps the cursor's position"
    );
}

#[test]
fn the_cursor_leaves_the_view_when_scrolled_back() {
    let mut v = TermView::new(10, 3, 100);
    v.feed(b"1\r\n2\r\n3\r\n4\r\n$ ");
    assert_eq!(v.cursor_in_view(), Some((2, 2)));
    v.scroll_lines(1);
    assert_eq!(
        v.cursor_in_view(),
        None,
        "the cursor row is below the viewport"
    );
    v.scroll_to_bottom();
    assert_eq!(v.cursor_in_view(), Some((2, 2)));
}

// ---------------------------------------------------------------------------------------------
// Paste.

#[test]
fn paste_is_sanitized_and_bracketed_when_asked() {
    assert_eq!(
        encode_paste("ls -la\r\npwd\nwhoami", false),
        b"ls -la\rpwd\rwhoami"
    );
    assert_eq!(
        encode_paste("echo 한글\ttab", true),
        "\x1b[200~echo 한글\ttab\x1b[201~".as_bytes()
    );
    // A pasted text can neither end the bracket early nor smuggle sequences or controls.
    let evil = "safe\x1b[201~rm -rf /\x1b]52;c;x\x07\x03\u{9b}31m\x7f";
    assert_eq!(
        encode_paste(evil, true),
        b"\x1b[200~safe[201~rm -rf /]52;c;x31m\x1b[201~"
    );
    assert_eq!(encode_paste(evil, false), b"safe[201~rm -rf /]52;c;x31m");
    // Through the view: bracketed when the program set 2004, and back to the live screen.
    let mut v = view(10, 2, b"\x1b[?2004h");
    for i in 0..5 {
        v.feed(format!("{i}\r\n").as_bytes());
    }
    v.scroll_lines(2);
    assert_eq!(v.paste("a\nb"), b"\x1b[200~a\rb\x1b[201~");
    assert_eq!(v.scroll_offset(), 0);
    v.feed(b"\x1b[?2004l");
    assert_eq!(v.paste("a\nb"), b"a\rb");
}

// ---------------------------------------------------------------------------------------------
// Keys.

#[test]
fn xterm_key_encoding() {
    let n = modes(b"");
    let app = modes(b"\x1b[?1h");
    let lnm = modes(b"\x1b[20h");
    let k = |key, mods, m: &ModeState| encode_key(key, mods, m);
    let ctrl_shift = Mods {
        shift: true,
        ctrl: true,
        alt: false,
    };
    let all = Mods {
        shift: true,
        alt: true,
        ctrl: true,
    };
    let cases: Vec<(Key, Mods, &ModeState, &[u8])> = vec![
        (Key::Char('a'), Mods::NONE, &n, b"a"),
        (Key::Char('한'), Mods::NONE, &n, "한".as_bytes()),
        (Key::Char('c'), Mods::CTRL, &n, b"\x03"),
        (Key::Char('C'), ctrl_shift, &n, b"\x03"),
        (Key::Char(' '), Mods::CTRL, &n, b"\x00"),
        (Key::Char('['), Mods::CTRL, &n, b"\x1b"),
        (Key::Char('\\'), Mods::CTRL, &n, b"\x1c"),
        (Key::Char('6'), Mods::CTRL, &n, b"\x1e"),
        (Key::Char('?'), Mods::CTRL, &n, b"\x7f"),
        (Key::Char('1'), Mods::CTRL, &n, b"1"),
        (Key::Char('x'), Mods::ALT, &n, b"\x1bx"),
        (
            Key::Char('d'),
            Mods {
                alt: true,
                ctrl: true,
                shift: false,
            },
            &n,
            b"\x1b\x04",
        ),
        (Key::Enter, Mods::NONE, &n, b"\r"),
        (Key::Enter, Mods::NONE, &lnm, b"\r\n"),
        (Key::Enter, Mods::ALT, &n, b"\x1b\r"),
        (Key::Tab, Mods::NONE, &n, b"\t"),
        (Key::Tab, Mods::SHIFT, &n, b"\x1b[Z"),
        (Key::Backspace, Mods::NONE, &n, b"\x7f"),
        (Key::Backspace, Mods::CTRL, &n, b"\x08"),
        (Key::Backspace, Mods::ALT, &n, b"\x1b\x7f"),
        (Key::Escape, Mods::NONE, &n, b"\x1b"),
        (Key::Up, Mods::NONE, &n, b"\x1b[A"),
        (Key::Up, Mods::NONE, &app, b"\x1bOA"),
        (Key::Down, Mods::SHIFT, &app, b"\x1b[1;2B"),
        (Key::Right, Mods::CTRL, &n, b"\x1b[1;5C"),
        (Key::Left, all, &n, b"\x1b[1;8D"),
        (Key::Home, Mods::NONE, &n, b"\x1b[H"),
        (Key::End, Mods::NONE, &app, b"\x1bOF"),
        (Key::Insert, Mods::NONE, &n, b"\x1b[2~"),
        (Key::Delete, Mods::NONE, &n, b"\x1b[3~"),
        (Key::Delete, Mods::CTRL, &n, b"\x1b[3;5~"),
        (Key::PageUp, Mods::NONE, &n, b"\x1b[5~"),
        (Key::PageDown, Mods::ALT, &n, b"\x1b[6;3~"),
        (Key::F(1), Mods::NONE, &n, b"\x1bOP"),
        (Key::F(4), Mods::SHIFT, &n, b"\x1b[1;2S"),
        (Key::F(5), Mods::NONE, &n, b"\x1b[15~"),
        (Key::F(6), Mods::NONE, &n, b"\x1b[17~"),
        (Key::F(11), Mods::NONE, &n, b"\x1b[23~"),
        (Key::F(12), Mods::CTRL, &n, b"\x1b[24;5~"),
        (Key::F(13), Mods::NONE, &n, b""),
    ];
    for (key, mods, m, want) in cases {
        assert_eq!(
            k(key, mods, m),
            want,
            "{key:?} {mods:?} app_cursor={} newline={}",
            m.app_cursor,
            m.newline
        );
    }
    // win32-input-mode: the KEY_EVENT_RECORD fields.
    let w = Win32Key {
        vk: 0x41,
        scan: 0x1e,
        unit: u16::from(b'a'),
        down: true,
        control: 0,
        repeat: 1,
    };
    assert_eq!(encode_win32_key(&w), b"\x1b[65;30;97;1;0;1_");
}

// ---------------------------------------------------------------------------------------------
// Mouse.

fn ev(action: MouseAction, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        action,
        col,
        row,
        x_px: u32::from(col) * 9 + 4,
        y_px: u32::from(row) * 17 + 8,
        mods: Mods::NONE,
    }
}

#[test]
fn mouse_reports_for_every_tracking_mode_and_encoding() {
    use MouseAction::{Move, Press, Release};
    use MouseButton::{Left, Middle, Right, WheelDown, WheelUp};
    let enc = |setup: &[u8], e: MouseEvent, held: Option<MouseButton>| {
        encode_mouse(&e, held, &modes(setup)).map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let none = None;
    // Off: nothing.
    assert_eq!(enc(b"", ev(Press(Left), 0, 0), none), None);
    // X10 (9): presses of the three buttons, no modifiers, no release or wheel.
    let x10 = b"\x1b[?9h";
    assert_eq!(
        enc(x10, ev(Press(Left), 0, 0), none).as_deref(),
        Some("\x1b[M !!")
    );
    let mut shifted = ev(Press(Right), 4, 2);
    shifted.mods = Mods::SHIFT;
    assert_eq!(enc(x10, shifted, none).as_deref(), Some("\x1b[M\"%#"));
    assert_eq!(enc(x10, ev(Release(Left), 0, 0), none), None);
    assert_eq!(enc(x10, ev(Press(WheelUp), 0, 0), none), None);
    // Normal (1000), default encoding: press, release as button 3, wheel 64/65, modifiers.
    let n = b"\x1b[?1000h";
    assert_eq!(
        enc(n, ev(Press(Middle), 9, 4), none).as_deref(),
        Some("\x1b[M!*%")
    );
    assert_eq!(
        enc(n, ev(Release(Middle), 9, 4), none).as_deref(),
        Some("\x1b[M#*%")
    );
    assert_eq!(
        enc(n, ev(Press(WheelUp), 0, 0), none).as_deref(),
        Some("\x1b[M`!!")
    );
    assert_eq!(
        enc(n, ev(Press(WheelDown), 0, 0), none).as_deref(),
        Some("\x1b[Ma!!")
    );
    let mut ctrl = ev(Press(Left), 0, 0);
    ctrl.mods = Mods {
        ctrl: true,
        alt: true,
        shift: false,
    };
    assert_eq!(
        enc(n, ctrl, none).as_deref(),
        Some("\x1b[M8!!"),
        "16 + 8 = 24 + 32"
    );
    assert_eq!(
        enc(n, ev(Move, 1, 1), Some(Left)),
        None,
        "1000 has no motion"
    );
    // Coordinates past 223 cannot be encoded by default.
    assert_eq!(enc(n, ev(Press(Left), 223, 0), none), None);
    assert!(enc(n, ev(Press(Left), 222, 0), none).is_some());
    // Button events (1002): motion only while a button is held.
    let b = b"\x1b[?1002h";
    assert_eq!(
        enc(b, ev(Move, 2, 3), Some(Left)).as_deref(),
        Some("\x1b[M@#$")
    );
    assert_eq!(enc(b, ev(Move, 2, 3), None), None);
    // Any event (1003): motion without a button is button 3 + 32.
    let a = b"\x1b[?1003h";
    assert_eq!(enc(a, ev(Move, 0, 0), None).as_deref(), Some("\x1b[MC!!"));
    // SGR (1006): decimal, M for press, m for release with the real button.
    let sgr = b"\x1b[?1000h\x1b[?1006h";
    assert_eq!(
        enc(sgr, ev(Press(Left), 299, 99), none).as_deref(),
        Some("\x1b[<0;300;100M")
    );
    assert_eq!(
        enc(sgr, ev(Release(Right), 0, 0), none).as_deref(),
        Some("\x1b[<2;1;1m")
    );
    assert_eq!(
        enc(sgr, ev(Press(WheelDown), 3, 3), none).as_deref(),
        Some("\x1b[<65;4;4M")
    );
    assert_eq!(
        enc(b"\x1b[?1002h\x1b[?1006h", ev(Move, 5, 6), Some(Right)).as_deref(),
        Some("\x1b[<34;6;7M")
    );
    // SGR pixels (1016): pixel coordinates.
    assert_eq!(
        enc(b"\x1b[?1000h\x1b[?1016h", ev(Press(Left), 2, 1), none).as_deref(),
        Some("\x1b[<0;23;26M")
    );
    // urxvt (1015): decimal with the +32 code; release as 3.
    let ux = b"\x1b[?1000h\x1b[?1015h";
    assert_eq!(
        enc(ux, ev(Press(Left), 0, 0), none).as_deref(),
        Some("\x1b[32;1;1M")
    );
    assert_eq!(
        enc(ux, ev(Release(Left), 0, 0), none).as_deref(),
        Some("\x1b[35;1;1M")
    );
    // UTF-8 (1005): coordinates above 95 as two-byte characters, up to 2015.
    let u8m = b"\x1b[?1000h\x1b[?1005h";
    let s = enc(u8m, ev(Press(Left), 299, 0), none).unwrap();
    assert_eq!(s, "\x1b[M \u{14c}!", "300 + 32 = 332 = U+014C");
    assert_eq!(enc(u8m, ev(Press(Left), 2015, 0), none), None);
}

#[test]
fn the_view_routes_the_mouse_to_the_program_or_to_selection_and_scrolling() {
    // No tracking: the wheel scrolls the view, clicks are the window's (selection).
    let mut v = TermView::new(10, 2, 100);
    for i in 0..9 {
        v.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(
        v.mouse(&ev(MouseAction::Press(MouseButton::WheelUp), 0, 0)),
        MouseOutcome::Scrolled
    );
    assert_eq!(v.scroll_offset(), 3);
    assert_eq!(
        v.mouse(&ev(MouseAction::Press(MouseButton::WheelDown), 0, 0)),
        MouseOutcome::Scrolled
    );
    assert_eq!(v.scroll_offset(), 0);
    assert_eq!(
        v.mouse(&ev(MouseAction::Press(MouseButton::Left), 0, 0)),
        MouseOutcome::Local
    );
    // Tracking: the program gets reports; motion is reported once per cell; Shift keeps the
    // mouse for selection.
    v.feed(b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        v.mouse(&ev(MouseAction::Press(MouseButton::Left), 1, 1)),
        MouseOutcome::Send(b"\x1b[<0;2;2M".to_vec())
    );
    assert_eq!(
        v.mouse(&ev(MouseAction::Move, 2, 1)),
        MouseOutcome::Send(b"\x1b[<32;3;2M".to_vec())
    );
    assert_eq!(
        v.mouse(&ev(MouseAction::Move, 2, 1)),
        MouseOutcome::Send(Vec::new())
    );
    assert_eq!(
        v.mouse(&ev(MouseAction::Release(MouseButton::Left), 2, 1)),
        MouseOutcome::Send(b"\x1b[<0;3;2m".to_vec())
    );
    assert_eq!(
        v.mouse(&ev(MouseAction::Move, 4, 1)),
        MouseOutcome::Send(Vec::new())
    );
    let mut shift = ev(MouseAction::Press(MouseButton::Left), 0, 0);
    shift.mods = Mods::SHIFT;
    assert_eq!(v.mouse(&shift), MouseOutcome::Local);
    // Alternate screen with alternate scroll (1007): the wheel sends cursor keys.
    let mut t = view(10, 2, b"\x1b[?1049h\x1b[?1007h\x1b[?1h");
    assert_eq!(
        t.mouse(&ev(MouseAction::Press(MouseButton::WheelDown), 0, 0)),
        MouseOutcome::Send(b"\x1bOB\x1bOB\x1bOB".to_vec())
    );
    let mut t = view(10, 2, b"\x1b[?1049h");
    assert_eq!(
        t.mouse(&ev(MouseAction::Press(MouseButton::WheelDown), 0, 0)),
        MouseOutcome::Local
    );
}

#[test]
fn focus_reports_and_hyperlink_hit_test() {
    let mut v = view(20, 2, b"\x1b[?1004h");
    assert_eq!(v.focus(false), Some(b"\x1b[O".to_vec()));
    assert_eq!(v.focus(false), None, "no change, no report");
    assert_eq!(v.focus(true), Some(b"\x1b[I".to_vec()));
    let mut q = view(20, 2, b"");
    assert_eq!(q.focus(false), None, "the program did not ask");
    // Hyperlinks: the view finds them; it never opens them.
    let v = view(
        20,
        2,
        b"see \x1b]8;;https://example.com/x\x1b\\here\x1b]8;;\x1b\\.",
    );
    assert_eq!(
        v.link_at(5, 0).map(|l| l.uri.to_string()).as_deref(),
        Some("https://example.com/x")
    );
    assert!(v.link_at(2, 0).is_none() && v.link_at(8, 0).is_none());
}

#[test]
fn reports_are_answered_by_the_size_owning_view_only() {
    let mut v = view(20, 3, b"");
    v.feed(b"\x1b[6n");
    assert!(v.take_replies().is_empty());
    v.set_answer_reports(true);
    v.feed(b"\x1b[2;5H\x1b[6n");
    assert_eq!(v.take_replies(), b"\x1b[2;5R");
}

#[test]
fn resize_keeps_content_and_clears_the_selection() {
    let mut v = view(10, 3, b"hello\r\nworld");
    v.select(0, 0, SelectMode::Line);
    v.resize(8, 2);
    assert!(!v.has_selection());
    assert_eq!(v.size(), (8, 2));
    assert_eq!(rows(&v), ["hello", "world"]);
}
