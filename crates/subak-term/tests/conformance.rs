//! Terminal conformance fixtures (Step 15, ADR 0036, docs/TERMINAL_EMULATOR.md §6):
//! vttest-like sequences and golden screens for the escape sequences, cursor operations, the
//! alternate screen, scrolling regions, Unicode (Korean, wide, emoji, combining marks), colours,
//! hyperlinks, modes and device reports.
//!
//! Every expectation is written out independently of the code under test (no "bless whatever the
//! emulator prints"), and every fixture also checks Host–client agreement: the model's snapshot
//! fed into a fresh model — what a newly attached client does — reproduces the same screen,
//! attributes, links, modes and cursor (`SUBAK-FR-004`, `SUBAK-FR-031`, `KIM-NFR-013`).

// The expected screens are drawn with row and column indices; that reads closest to the picture.
#![allow(clippy::needless_range_loop)]

use subak_term::screen::{
    CellWidth, Color, CursorShape, HistoryRange, Limits, MouseEncoding, MouseTracking, Pos, Screen,
};

fn screen(cols: u16, rows: u16) -> Screen {
    Screen::new(cols, rows, Limits::default())
}

fn feed(cols: u16, rows: u16, bytes: &[u8]) -> Screen {
    let mut s = screen(cols, rows);
    s.feed(bytes);
    s
}

/// The screen rows as text, trailing blanks removed.
fn text(s: &Screen) -> Vec<String> {
    s.capture(Pos::Line(0), Pos::Edge, false, false).lines
}

fn history(s: &Screen) -> Vec<String> {
    s.capture(Pos::Edge, Pos::Line(-1), false, false).lines
}

/// Everything a viewer sees, in a comparable form: cells (character, marks, width, attributes,
/// link), cursor, modes, title, cursor shape.
fn dump(s: &Screen) -> String {
    let (cols, rows) = s.size();
    let c = s.cursor();
    let mut out = format!(
        "{cols}x{rows} cursor {},{} visible {} title {:?} shape {:?}\nmodes {:?}\n",
        c.x,
        c.y,
        c.visible,
        s.title(),
        s.cursor_shape(),
        s.modes()
    );
    for y in 0..rows {
        let row = s.row(y);
        out.push_str(&format!("{y:3} wrapped={} |", row.wrapped()));
        for x in 0..usize::from(cols) {
            let cell = row.cell(x);
            out.push_str(&format!(
                "{}{}{:?}{:?}{}",
                cell.ch,
                cell.combining,
                cell.width,
                cell.attrs,
                cell.link
                    .map_or(String::new(), |l| format!("<{}|{:?}>", l.uri, l.id))
            ));
        }
        out.push('\n');
    }
    out
}

/// A client attaching now gets the same screen as the Host model.
#[track_caller]
fn assert_replays(s: &Screen) {
    let mut c = Screen::new(s.size().0, s.size().1, Limits::default());
    c.feed(&s.snapshot(HistoryRange::Last(usize::MAX), false));
    assert_eq!(dump(s), dump(&c), "snapshot replay differs");
    assert_eq!(history(s), history(&c), "history replay differs");
}

/// An expected screen built from rows (shorter rows are padded with blanks).
fn grid(rows: &[&str]) -> Vec<String> {
    rows.iter().map(|r| r.trim_end().to_string()).collect()
}

// ---------------------------------------------------------------------------------------------
// vttest-like: cursor movements.

/// vttest's first screen, reduced to 40×12: the screen is filled with `E` (DECALN), everything
/// but a frame of `E`s in the middle is erased (EL, ECH), a border of `*` is drawn with absolute
/// positioning (CUP, including the last column, whose pending wrap the next CUP cancels), and a
/// border of `+` just inside it with relative movements only (CUF/CUD/CUB/BS/RI/IND/NEL). "The
/// screen should be cleared, and have an unbroken border of *'s and +'s around the edge, and
/// exactly in the middle there should be a frame of E's around this text with one free position
/// around it."
#[test]
fn vttest_cursor_movements_border_and_frame() {
    let (cols, rows) = (40usize, 12usize);
    let mut b = String::from("\x1b#8");
    // Erase all but the E rectangle rows 3..=8, columns 10..=29 (0-based).
    for r in 0..rows {
        if (3..=8).contains(&r) {
            b.push_str(&format!(
                "\x1b[{};1H\x1b[10X\x1b[{};31H\x1b[K",
                r + 1,
                r + 1
            ));
        } else {
            b.push_str(&format!("\x1b[{};1H\x1b[2K", r + 1));
        }
    }
    // Hollow out the rectangle's inside (rows 4..=7, columns 11..=28).
    for r in 4..=7 {
        b.push_str(&format!("\x1b[{};12H\x1b[18X", r + 1));
    }
    // The text, with one free position around it.
    b.push_str("\x1b[6;13HThe screen should");
    b.push_str("\x1b[7;13Hhave a frame of E");
    // `*` border by absolute positioning.
    for c in 1..=cols {
        b.push_str(&format!("\x1b[1;{c}H*\x1b[{rows};{c}H*"));
    }
    for r in 2..rows {
        b.push_str(&format!("\x1b[{r};1H*\x1b[{r};{cols}f*"));
    }
    // `+` border by relative movements: right along row 1 (CUF after each `+`, then BS), down
    // column 38 from the last column (CUD, CUB), left along row 10 (BS BS), up column 1 (RI).
    b.push_str("\x1b[2;2H");
    for _ in 0..(cols - 2) {
        b.push_str("+\x1b[C\x08");
    }
    b.push_str("\x1b[2;40H");
    for _ in 0..(rows - 3) {
        b.push_str("\x1b[B\x1b[D+");
    }
    b.push_str("\x1b[11;39H+");
    for _ in 0..(cols - 3) {
        b.push_str("\x08\x08+");
    }
    b.push_str("\x1b[11;2H");
    for _ in 0..(rows - 3) {
        b.push_str("\x1bM+\x08");
    }
    // Leave with NEL and IND from the last row of the region: the bottom row scrolls nothing
    // because the cursor is placed back first.
    b.push_str("\x1b[1;1H");
    let s = feed(40, 12, b.as_bytes());
    let mut expect = vec![vec![' '; cols]; rows];
    for c in 0..cols {
        expect[0][c] = '*';
        expect[rows - 1][c] = '*';
    }
    for r in 0..rows {
        expect[r][0] = '*';
        expect[r][cols - 1] = '*';
    }
    for c in 1..cols - 1 {
        expect[1][c] = '+';
        expect[rows - 2][c] = '+';
    }
    for r in 1..rows - 1 {
        expect[r][1] = '+';
        expect[r][cols - 2] = '+';
    }
    for r in 3..=8 {
        for c in 10..=29 {
            expect[r][c] = if (4..=7).contains(&r) && (11..=28).contains(&c) {
                ' '
            } else {
                'E'
            };
        }
    }
    for (i, ch) in "The screen should".chars().enumerate() {
        expect[5][12 + i] = ch;
    }
    for (i, ch) in "have a frame of E".chars().enumerate() {
        expect[6][12 + i] = ch;
    }
    let expect: Vec<String> = expect
        .iter()
        .map(|r| r.iter().collect::<String>().trim_end().to_string())
        .collect();
    assert_eq!(text(&s), expect, "\n{}", text(&s).join("\n"));
    assert_eq!((s.cursor().x, s.cursor().y), (0, 0));
    assert!(history(&s).is_empty(), "nothing scrolled");
    assert_replays(&s);
}

/// Autowrap on and off, pending wrap at the last column, and control characters mixed with
/// printing at the margin (vttest "autowrap, mixing control and print characters").
#[test]
fn autowrap_and_pending_wrap() {
    // Ten columns: the 10th character waits at the margin; CR then cancels the wrap.
    let s = feed(10, 4, b"0123456789\rX");
    assert_eq!(text(&s), grid(&["X123456789", "", "", ""]));
    // The 11th character wraps; the row is marked as soft-wrapped.
    let s = feed(10, 4, b"0123456789AB");
    assert_eq!(text(&s), grid(&["0123456789", "AB", "", ""]));
    assert!(s.row(0).wrapped() && !s.row(1).wrapped());
    // BS at the margin moves left from the last column (the pending wrap is dropped).
    let s = feed(10, 4, b"0123456789\x08Z");
    assert_eq!(text(&s)[0], "01234567Z9");
    // Autowrap off: everything past the margin overwrites the last column.
    let s = feed(10, 4, b"\x1b[?7l0123456789ABCD");
    assert_eq!(text(&s), grid(&["012345678D", "", "", ""]));
    assert!(!s.modes().autowrap);
    assert_replays(&s);
    // Tab at the margin stays there; LF in the middle of a line keeps the column.
    let s = feed(10, 4, b"ab\tc\nd\t\t\te");
    assert_eq!(text(&s), grid(&["ab      c", "         e", "", ""]));
    assert_replays(&s);
}

/// Insert and delete characters and lines (vttest "VT102 insert/delete"), inside and outside a
/// scroll region; deleted lines never enter the history.
#[test]
fn insert_and_delete_characters_and_lines() {
    let mut b = String::new();
    for (i, l) in ["AAAA", "BBBB", "CCCC", "DDDD", "EEEE", "FFFF"]
        .iter()
        .enumerate()
    {
        b.push_str(&format!("\x1b[{};1H{l}", i + 1));
    }
    let mut s = feed(8, 6, b.as_bytes());
    // ICH and DCH on row 1.
    s.feed(b"\x1b[1;2H\x1b[2@xy\x1b[1;1H\x1b[P");
    assert_eq!(text(&s)[0], "xyAAA");
    // IL inside a region pushes the region's last row out; DL pulls blank rows in at its bottom.
    s.feed(b"\x1b[2;5r\x1b[3;1H\x1b[L");
    assert_eq!(
        text(&s),
        grid(&["xyAAA", "BBBB", "", "CCCC", "DDDD", "FFFF"])
    );
    s.feed(b"\x1b[2;1H\x1b[2M");
    assert_eq!(text(&s), grid(&["xyAAA", "CCCC", "DDDD", "", "", "FFFF"]));
    // Outside the region IL/DL do nothing.
    s.feed(b"\x1b[6;1H\x1b[L\x1b[M");
    assert_eq!(text(&s)[5], "FFFF");
    assert!(history(&s).is_empty());
    assert_replays(&s);
    // DCH past the end and ICH of more than the row has are clamped.
    let s = feed(6, 1, b"abcdef\x1b[1;3H\x1b[99P");
    assert_eq!(text(&s)[0], "ab");
    let s = feed(6, 1, b"abcdef\x1b[1;3H\x1b[99@");
    assert_eq!(text(&s)[0], "ab");
}

/// Scroll regions: IND at the bottom margin and RI at the top margin scroll only the region; SU
/// and SD; origin mode makes CUP relative to the region and keeps the cursor inside it; only
/// a full-screen region feeds the history.
#[test]
fn scroll_regions_and_origin_mode() {
    let mut s = screen(6, 6);
    for i in 0..6 {
        s.feed(format!("\x1b[{};1Hr{i}", i + 1).as_bytes());
    }
    s.feed(b"\x1b[2;4r\x1b[4;1H\nN");
    assert_eq!(text(&s), grid(&["r0", "r2", "r3", "N", "r4", "r5"]));
    s.feed(b"\x1b[2;1H\x1bMT");
    assert_eq!(text(&s), grid(&["r0", "T", "r2", "r3", "r4", "r5"]));
    s.feed(b"\x1b[S");
    assert_eq!(text(&s), grid(&["r0", "r2", "r3", "", "r4", "r5"]));
    s.feed(b"\x1b[2T");
    assert_eq!(text(&s), grid(&["r0", "", "", "r2", "r4", "r5"]));
    assert!(history(&s).is_empty(), "a partial region keeps no history");
    // Origin mode: row 1 is the region's top; the cursor cannot leave the region.
    s.feed(b"\x1b[?6h\x1b[1;1HO\x1b[9;1HB");
    assert_eq!(text(&s)[1], "O");
    assert_eq!(text(&s)[3], "B2", "row 9 clamps to the region's bottom");
    assert_replays(&s);
    // A full-screen region scrolls into the history.
    let mut f = screen(6, 3);
    f.feed(b"a\r\nb\r\nc\r\nd\x1b[S");
    assert_eq!(history(&f), ["a", "b"]);
    assert_eq!(text(&f), grid(&["c", "d", ""]));
}

/// The alternate screens: 47 switches without clearing or saving, 1047 clears when leaving,
/// 1048 saves/restores the cursor, 1049 saves the cursor, clears and restores; the alternate
/// screen has no history.
#[test]
fn alternate_screen_variants() {
    // 1049: the shell screen and cursor come back exactly.
    let mut s = feed(10, 3, b"shell\r\n$ ");
    s.feed(b"\x1b[?1049hFULL\x1b[3;1Hbar\r\n\r\n");
    assert!(s.modes().alternate_screen);
    // 1049 keeps the cursor where the shell left it: FULL on row 1, bar on row 2, then two
    // line feeds at the bottom scroll the alternate screen twice.
    assert_eq!(
        text(&s),
        grid(&["bar", "", ""]),
        "scrolled inside the alt screen"
    );
    assert!(
        history(&s).is_empty(),
        "the alternate screen keeps no history"
    );
    assert_replays(&s);
    s.feed(b"\x1b[?1049l");
    assert_eq!(text(&s), grid(&["shell", "$", ""]));
    assert_eq!((s.cursor().x, s.cursor().y), (2, 1));
    // 1049 entered twice keeps the first saved cursor and clears again.
    let mut s = feed(10, 3, b"keep");
    s.feed(b"\x1b[?1049hA\x1b[?1049hB\x1b[?1049l");
    assert_eq!(text(&s)[0], "keep");
    assert_eq!(s.cursor().x, 4);
    // 47 neither clears nor saves the cursor.
    let mut s = feed(10, 3, b"one");
    s.feed(b"\x1b[?47hX\x1b[?47l");
    assert_eq!(text(&s)[0], "one");
    assert_eq!(
        s.cursor().x,
        4,
        "the cursor moved on the alternate screen and stays"
    );
    // A documented difference from xterm (docs/TERMINAL_EMULATOR.md §8): the model frees the
    // alternate screen when a program leaves it, so 47 shows it blank again, where xterm would
    // show what was left there. Programs use 1049, which clears on entry anyway.
    s.feed(b"\x1b[?47h");
    assert_eq!(text(&s)[0], "", "the alternate screen starts blank");
    s.feed(b"\x1b[?47l");
    // 1047 clears the alternate screen when leaving it; 1048 saves and restores the cursor.
    let mut s = feed(10, 3, b"main");
    s.feed(b"\x1b[?1048h\x1b[?1047hALT\x1b[?1047l\x1b[?1048l");
    assert_eq!(text(&s)[0], "main");
    assert_eq!(s.cursor().x, 4);
    s.feed(b"\x1b[?1047h");
    assert_eq!(
        text(&s),
        grid(&["", "", ""]),
        "1047 cleared it on the way out"
    );
    assert_replays(&s);
}

/// Erasing in display and line, ECH, and background colour erase (BCE): erased cells take the
/// current background colour, nothing else.
#[test]
fn erase_with_background_colour() {
    let mut s = feed(8, 3, b"abcdefgh\r\nijklmnop\r\nqrstuvwx");
    s.feed(b"\x1b[2;4H\x1b[1;44m\x1b[K");
    assert_eq!(text(&s)[1], "ijk");
    let e = s.row(1).cell(5);
    assert_eq!(e.attrs.bg, Color::Indexed(4), "BCE");
    assert!(!e.attrs.bold(), "erased cells keep only the background");
    s.feed(b"\x1b[0m\x1b[1;3H\x1b[1K");
    assert_eq!(text(&s)[0], "   defgh");
    s.feed(b"\x1b[3;2H\x1b[3X");
    assert_eq!(text(&s)[2], "q   uvwx");
    // ED 1 erases from the top through the cursor cell (row 1, column 0) only.
    s.feed(b"\x1b[2;1H\x1b[1J");
    assert_eq!(text(&s), grid(&["", " jk", "q   uvwx"]));
    s.feed(b"\x1b[3;5H\x1b[J");
    assert_eq!(text(&s)[2], "q");
    assert_replays(&s);
    // ED 3 clears the history only.
    let mut h = feed(4, 2, b"1\r\n2\r\n3\r\n4");
    assert_eq!(history(&h), ["1", "2"]);
    h.feed(b"\x1b[3J");
    assert!(history(&h).is_empty());
    assert_eq!(text(&h), grid(&["3", "4"]));
}

/// SGR: every attribute on and off, the 16/256/24-bit colour forms with semicolons and colons.
#[test]
fn sgr_attributes_and_colours() {
    let s = feed(
        40,
        3,
        b"\x1b[1;2;3;4;5;7;8;9;53mA\x1b[22;23;24;25;27;28;29;55mB\x1b[21mC\x1b[0m\
          \x1b[31;42mD\x1b[91;102mE\x1b[38;5;208;48;5;17mF\x1b[38;2;1;2;3;48;2;4;5;6mG\
          \x1b[38:2::7:8:9mH\x1b[38:5:99mI\x1b[4:3mJ\x1b[4:0mK\x1b[39;49mL",
    );
    let c = |x| s.row(0).cell(x).attrs;
    let a = c(0);
    assert!(a.bold() && a.dim() && a.italic() && a.blink() && a.inverse());
    assert!(a.hidden() && a.strike() && a.overline());
    assert_eq!(a.underline(), 1);
    let b = c(1);
    assert!(!b.bold() && !b.dim() && !b.italic() && !b.blink() && !b.inverse());
    assert!(!b.hidden() && !b.strike() && !b.overline());
    assert_eq!(b.underline(), 0);
    assert_eq!(c(2).underline(), 2, "SGR 21 is a double underline");
    assert_eq!((c(3).fg, c(3).bg), (Color::Indexed(1), Color::Indexed(2)));
    assert_eq!((c(4).fg, c(4).bg), (Color::Indexed(9), Color::Indexed(10)));
    assert_eq!(
        (c(5).fg, c(5).bg),
        (Color::Indexed(208), Color::Indexed(17))
    );
    assert_eq!(
        (c(6).fg, c(6).bg),
        (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6))
    );
    assert_eq!(c(7).fg, Color::Rgb(7, 8, 9), "colon form with colour space");
    assert_eq!(c(8).fg, Color::Indexed(99));
    assert_eq!(c(9).underline(), 1, "4:3 (curly) draws as an underline");
    assert_eq!(c(10).underline(), 0);
    assert_eq!((c(11).fg, c(11).bg), (Color::Default, Color::Default));
    assert_replays(&s);
}

/// Tab stops: defaults every 8 columns, HTS sets, TBC clears one or all, CHT and CBT move.
#[test]
fn tab_stops() {
    let mut s = feed(30, 2, b"a\tb\tc");
    assert_eq!(text(&s)[0], "a       b       c");
    s.feed(b"\r\n\x1b[3g\x1b[2;5H\x1bH\x1b[2;12H\x1bH\x1b[2;1H\tX\tY\x1b[2I");
    assert_eq!(text(&s)[1], "    X      Y");
    assert_eq!(s.cursor().x, 29, "no stop left: the last column");
    s.feed(b"\x1b[Z!");
    assert_eq!(text(&s)[1], "    X      !");
    s.feed(b"\x1b[2;12H\x1b[g\x1b[2;1H\t\tZ");
    assert_eq!(text(&s)[1], "    X      !                 Z");
    assert_replays(&s);
}

/// DEC special graphics in G0 and G1 (SI/SO), and the UK set.
#[test]
fn charsets() {
    let s = feed(20, 2, b"\x1b(0lqqk\x1b(B x \x1b)0\x0emqj\x0f!\x1b(A#");
    assert_eq!(text(&s)[0], "┌──┐ x └─┘!£");
    assert_replays(&s);
}

/// Korean and other wide characters take two cells, emoji too; combining marks join the
/// character before them; invalid UTF-8 becomes U+FFFD; a wide character never splits across
/// the margin; overwriting half of one blanks the other half.
#[test]
fn unicode_wide_korean_emoji_and_combining() {
    let s = feed(
        12,
        4,
        "한글 테스트\r\n😀a🍉\r\ne\u{301}a\u{308}\u{fe0f}".as_bytes(),
    );
    assert_eq!(text(&s)[0], "한글 테스트");
    let r = s.row(0);
    assert_eq!(r.cell(0).ch, '한');
    assert_eq!(r.cell(0).width, CellWidth::Wide);
    assert_eq!(r.cell(1).width, CellWidth::Tail);
    assert_eq!(r.cell(4).ch, ' ');
    assert_eq!(r.cell(5).ch, '테');
    assert_eq!(s.row(1).cell(0).width, CellWidth::Wide, "emoji are wide");
    assert_eq!(s.row(1).cell(2).ch, 'a');
    assert_eq!(s.row(1).cell(3).ch, '🍉');
    let r2 = s.row(2);
    assert_eq!((r2.cell(0).ch, r2.cell(0).combining), ('e', "\u{301}"));
    assert_eq!(
        (r2.cell(1).ch, r2.cell(1).combining),
        ('a', "\u{308}\u{fe0f}")
    );
    assert_eq!(s.cursor().x, 2, "combining marks take no column");
    assert_replays(&s);
    // A wide character that does not fit in the last column wraps (autowrap) or is placed in
    // the last two columns (no autowrap).
    let s = feed(5, 2, "abcd한".as_bytes());
    assert_eq!(text(&s), grid(&["abcd", "한"]));
    assert!(s.row(0).wrapped());
    let s = feed(5, 2, "\x1b[?7labcd한".as_bytes());
    assert_eq!(text(&s), grid(&["abc한", ""]));
    // Overwriting either half of a wide character blanks the other.
    let s = feed(6, 1, "한글\x1b[1;2Hx".as_bytes());
    assert_eq!(text(&s)[0], " x글");
    let s = feed(6, 1, "한글\x1b[1;3Hx".as_bytes());
    assert_eq!(text(&s)[0], "한x");
    // Invalid UTF-8: a lone continuation byte, a truncated sequence, a byte that can never
    // start a character — one U+FFFD each.
    let s = feed(10, 1, b"\x80a\xe2\x82b\xc0c");
    assert_eq!(text(&s)[0], "\u{fffd}a\u{fffd}b\u{fffd}c");
    // A UTF-8 character split across two reads (ConPTY chunks) is one character.
    let mut s = screen(10, 1);
    let bytes = "가".as_bytes();
    s.feed(&bytes[..1]);
    s.feed(&bytes[1..]);
    assert_eq!(text(&s)[0], "가");
}

/// OSC 8 hyperlinks: on the cells printed while active (a wide character's both cells), an `id`
/// joins a link across rows, an empty URI ends it, invalid targets are not links, and links
/// survive the snapshot and the history.
#[test]
fn osc8_hyperlinks() {
    let mut s = screen(10, 3);
    s.feed(b"a\x1b]8;id=x;https://example.com/a\x1b\\link\x1b]8;;\x1b\\b");
    let r = s.row(0);
    assert!(r.cell(0).link.is_none());
    let l = r.cell(1).link.expect("linked");
    assert_eq!(&*l.uri, "https://example.com/a");
    assert_eq!(l.id.as_deref(), Some("x"));
    assert!(r.cell(4).link.is_some() && r.cell(5).link.is_none());
    // A link across a wrap and on a wide character (BEL-terminated form).
    s.feed("\r\n\x1b]8;;file:///C:/notes.txt\x07파일-long-name\x1b]8;;\x07".as_bytes());
    assert!(
        s.row(1).cell(1).link.is_some(),
        "the tail cell reports the link"
    );
    assert!(s.row(2).cell(0).link.is_some(), "continues after the wrap");
    assert!(s.row(1).wrapped(), "a soft wrap");
    assert_replays(&s);
    // Invalid targets: control characters, spaces, too long.
    let mut bad = screen(20, 1);
    bad.feed(b"\x1b]8;;http://a b\x1b\\x\x1b]8;;\x1b\\");
    bad.feed(format!("\x1b]8;;http://{}\x1b\\y", "a".repeat(3000)).as_bytes());
    assert!(bad.row(0).cell(0).link.is_none());
    assert!(bad.row(0).cell(1).link.is_none());
    // Into the history and back: the decoded row has the link.
    let mut h = screen(10, 2);
    h.feed(b"\x1b]8;;https://h.example\x1b\\HIST\x1b]8;;\x1b\\\r\n\r\n\r\n");
    assert_eq!(
        history(&h),
        ["HIST", ""],
        "capture text has no escape sequences"
    );
    let row = h.history_row(0, 10).expect("history row");
    assert_eq!(row.text().trim_end(), "HIST");
    assert_eq!(
        row.cell(0).link.map(|l| l.uri.to_string()).as_deref(),
        Some("https://h.example")
    );
    let vt = h.capture(Pos::Edge, Pos::Line(-1), true, false).lines;
    assert!(vt[0].contains("\x1b]8;;https://h.example\x1b\\"), "{vt:?}");
    // A link still open when the snapshot is taken stays open on the client.
    let mut o = screen(10, 2);
    o.feed(b"\x1b]8;;https://open.example\x1b\\ab");
    assert!(o.active_link().is_some());
    assert_replays(&o);
}

/// Mouse tracking and encoding modes are each one mode at a time (setting one replaces the
/// others, resetting any turns it off, as in xterm) and are replayed to new clients.
#[test]
fn mouse_modes() {
    let mut s = screen(10, 2);
    assert_eq!(s.modes().mouse, MouseTracking::Off);
    s.feed(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(s.modes().mouse, MouseTracking::Normal);
    assert_eq!(s.modes().mouse_encoding, MouseEncoding::Sgr);
    s.feed(b"\x1b[?1003h");
    assert_eq!(s.modes().mouse, MouseTracking::AnyEvent);
    assert_replays(&s);
    s.feed(b"\x1b[?1000l");
    assert_eq!(
        s.modes().mouse,
        MouseTracking::Off,
        "resetting any turns tracking off"
    );
    s.feed(b"\x1b[?9h\x1b[?1015h");
    assert_eq!(s.modes().mouse, MouseTracking::X10);
    assert_eq!(s.modes().mouse_encoding, MouseEncoding::Urxvt);
    s.feed(b"\x1b[?1002h\x1b[?1005h");
    assert_eq!(s.modes().mouse, MouseTracking::ButtonEvent);
    assert_eq!(s.modes().mouse_encoding, MouseEncoding::Utf8);
    s.feed(b"\x1b[?1016h");
    assert_eq!(s.modes().mouse_encoding, MouseEncoding::SgrPixels);
    assert_replays(&s);
    // Other modes a view needs.
    s.feed(b"\x1b[?2004h\x1b[?1004h\x1b[?1007h\x1b[?2026h\x1b[?9001h\x1b[?1h\x1b=");
    let m = s.modes();
    assert!(m.bracketed_paste && m.focus_events && m.alternate_scroll);
    assert!(m.synchronized_output && m.win32_input && m.app_cursor && m.app_keypad);
    assert_replays(&s);
    s.feed(b"\x1b[?2026l\x1b[?2004l");
    assert!(!s.modes().synchronized_output && !s.modes().bracketed_paste);
}

/// Device reports are answered only by a view that is the pane's terminal: never by default
/// (the Host), and correctly once enabled.
#[test]
fn device_reports_only_when_enabled() {
    let mut host = screen(20, 5);
    host.feed(b"\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[?2004$p");
    assert!(host.take_replies().is_empty(), "the Host never answers");
    let mut v = screen(20, 5);
    v.set_answer_reports(true);
    v.feed(b"\x1b[3;7Hx\x1b[6n");
    assert_eq!(v.take_replies(), b"\x1b[3;8R");
    v.feed(b"\x1b[c\x1b[0c\x1b[>c\x1b[5n");
    assert_eq!(
        v.take_replies(),
        b"\x1b[?62;22c\x1b[?62;22c\x1b[>1;10;0c\x1b[0n"
    );
    v.feed(b"\x1b[?2004h\x1b[?2004$p\x1b[?1004$p\x1b[?7$p\x1b[?4242$p\x1b[4$p");
    assert_eq!(
        String::from_utf8(v.take_replies()).unwrap(),
        "\x1b[?2004;1$y\x1b[?1004;2$y\x1b[?7;1$y\x1b[?4242;0$y\x1b[4;2$y"
    );
    // CPR is relative to the region in origin mode; DECXCPR adds the page.
    v.feed(b"\x1b[2;4r\x1b[?6h\x1b[2;3H\x1b[6n\x1b[?6n");
    assert_eq!(v.take_replies(), b"\x1b[2;3R\x1b[?2;3;1R");
    // Replies are bounded if nobody collects them.
    for _ in 0..10_000 {
        v.feed(b"\x1b[6n");
    }
    assert!(v.take_replies().len() <= 4096);
}

/// Resize: rows above the cursor that no longer fit go to the history, columns are cut or
/// padded without reflow, the region resets, the alternate screen loses rows instead.
#[test]
fn resize() {
    let mut s = feed(10, 4, b"1\r\n2\r\n3\r\n4 wide-row");
    s.resize(6, 2);
    assert_eq!(history(&s), ["1", "2"]);
    assert_eq!(text(&s), grid(&["3", "4 wide"]));
    assert_eq!(
        (s.cursor().x, s.cursor().y),
        (5, 1),
        "clamped to the new width"
    );
    s.resize(8, 4);
    assert_eq!(text(&s), grid(&["3", "4 wide", "", ""]));
    assert_replays(&s);
    // A wide character cut in half by a narrower width is blanked.
    let mut w = feed(4, 1, "ab한".as_bytes());
    w.resize(3, 1);
    assert_eq!(text(&w)[0], "ab");
    let mut a = feed(10, 3, b"shell");
    a.feed(b"\x1b[?1049h\x1b[3;1Hlast");
    a.resize(10, 1);
    assert_eq!(text(&a), ["last"]);
    assert!(
        history(&a).is_empty(),
        "the alternate screen keeps no history"
    );
    a.feed(b"\x1b[?1049l");
    assert_eq!(text(&a), ["shell"]);
}

/// Window title (OSC 0 and 2, controls removed), cursor shape (DECSCUSR), REP, DECALN, the soft
/// and hard resets, and sequences a terminal must consume without effect.
#[test]
fn title_cursor_shape_repeat_and_resets() {
    let mut s = feed(
        10,
        2,
        b"\x1b]0;first\x07\x1b]2;sec\x01ond\x1b\\-\x1b[3b\x1b[4 q",
    );
    assert_eq!(s.title(), "second");
    assert_eq!(text(&s)[0], "----");
    assert_eq!(s.cursor_shape(), (CursorShape::Underline, false));
    s.feed(b"\x1b[5 q");
    assert_eq!(s.cursor_shape(), (CursorShape::Bar, true));
    assert_replays(&s);
    s.feed(b"\x1b#8");
    assert_eq!(text(&s), grid(&["EEEEEEEEEE", "EEEEEEEEEE"]));
    s.feed(b"\x1b[1m\x1b[?25l\x1b[!p");
    assert!(s.cursor().visible, "DECSTR shows the cursor");
    s.feed(b"\x1bc");
    assert_eq!(text(&s), grid(&["", ""]));
    assert_eq!(s.title(), "");
    // Consumed safely: DCS, APC, PM, SOS, OSC 52 (never replayed), unknown CSI.
    let mut c = screen(10, 1);
    c.feed(b"\x1bP1$qm\x1b\\a\x1b_x\x1b\\b\x1b^y\x1b\\c\x1bXz\x1b\\d\x1b]52;c;c2VjcmV0\x07e\x1b[?1;2;3zf");
    assert_eq!(text(&c)[0], "abcdef");
    assert!(!String::from_utf8_lossy(&c.snapshot(HistoryRange::None, false)).contains("52;"));
}

/// A long random byte stream never panics and always replays exactly (the client view and the
/// Host agree on every intermediate state that a snapshot captures).
#[test]
fn fuzz_never_panics_and_always_replays() {
    let alphabet: &[u8] =
        b"\x1b[]?;:0123456789hlmHJKr@PXLMSTb\n\r\t\x08\x07\\ABCDEFGHIJKLMNOPQ8()#7 qc$p";
    let utf8 = ["한", "😀", "e\u{301}", "─", "\u{fe0f}"];
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut s = screen(9, 4);
    for i in 0..300_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        if seed.is_multiple_of(11) {
            s.feed(utf8[(seed / 11 % utf8.len() as u64) as usize].as_bytes());
        } else if seed.is_multiple_of(13) {
            s.feed(b"\x1b]8;;https://x.example\x1b\\");
        } else {
            s.feed(&[alphabet[(seed % alphabet.len() as u64) as usize]]);
        }
        if i % 7919 == 0 {
            s.resize((seed % 12 + 1) as u16, (seed % 6 + 1) as u16);
        }
        if i % 4999 == 0 {
            let mut c = Screen::new(s.size().0, s.size().1, Limits::default());
            c.feed(&s.snapshot(HistoryRange::Last(usize::MAX), false));
            assert_eq!(dump(&s), dump(&c), "after {i} bytes");
        }
    }
}
