//! The cell-grid renderer, headless (Step 15, ADR 0036, docs/TERMINAL_EMULATOR.md §5): colours,
//! attributes, wide characters, cursor shapes and focus, selection, reverse video, box drawing,
//! themes, damage tracking and scaling, checked pixel by pixel; golden frames of the
//! deterministic built-in rasterizer (`tests/golden/render_*.txt`, refreshed with
//! `SUBAK_BLESS=1` after an intended change — review the diff); and the system-font rasterizer
//! on Windows. `SUBAK-FR-031`, `KIM-NFR-012`, `KIM-NFR-013`.

use std::collections::HashMap;
use subak_term::render::{Frame, RenderOptions, Renderer, Rgb, Theme};
use subak_term::{SelectMode, TermView};

fn view(cols: u16, rows: u16, bytes: &[u8]) -> TermView {
    let mut v = TermView::new(cols, rows, 100);
    v.feed(bytes);
    v
}

fn builtin(scale: f32) -> Renderer {
    Renderer::builtin(
        Theme::kim_dark(),
        RenderOptions {
            scale,
            ..RenderOptions::default()
        },
    )
}

/// The pixels of cell (`col`, `row`).
fn cell_pixels(r: &Renderer, col: u16, row: u16) -> Vec<Rgb> {
    let m = r.metrics();
    let (x0, y0) = (
        r.padding() + u32::from(col) * m.width,
        r.padding() + u32::from(row) * m.height,
    );
    let f = r.frame();
    let mut v = Vec::new();
    for y in y0..y0 + m.height {
        for x in x0..x0 + m.width {
            v.push(f.pixel(x, y));
        }
    }
    v
}

/// A cell's background (its top-left pixel; glyphs never reach it in these scenes) and the most
/// common other colour (its ink), if any.
fn cell_colors(r: &Renderer, col: u16, row: u16) -> (Rgb, Option<Rgb>) {
    let px = cell_pixels(r, col, row);
    let bg = px[0];
    let mut counts: HashMap<Rgb, usize> = HashMap::new();
    for p in &px {
        if *p != bg {
            *counts.entry(*p).or_default() += 1;
        }
    }
    let ink = counts
        .into_iter()
        .max_by_key(|(c, n)| (*n, c.0, c.1, c.2))
        .map(|(c, _)| c);
    (bg, ink)
}

/// Columns of a cell's pixel block that have any ink.
fn ink_columns(r: &Renderer, col: u16, row: u16) -> usize {
    let m = r.metrics();
    let px = cell_pixels(r, col, row);
    let bg = px[0];
    (0..m.width as usize)
        .filter(|&x| (0..m.height as usize).any(|y| px[y * m.width as usize + x] != bg))
        .count()
}

/// A golden of a frame: its size and hash, and per cell row a background letter and an ink
/// letter (`.` for none) from a legend of the colours that occur.
fn golden(name: &str, r: &Renderer, cols: u16, rows: u16) -> String {
    let f: &Frame = r.frame();
    let mut legend: Vec<Rgb> = Vec::new();
    let letter = |c: Rgb, legend: &mut Vec<Rgb>| -> char {
        let i = legend.iter().position(|l| *l == c).unwrap_or_else(|| {
            legend.push(c);
            legend.len() - 1
        });
        char::from(b'a' + u8::try_from(i % 26).unwrap())
    };
    let mut body = String::new();
    for row in 0..rows {
        let (mut bgs, mut inks) = (String::new(), String::new());
        for col in 0..cols {
            let (bg, ink) = cell_colors(r, col, row);
            bgs.push(letter(bg, &mut legend));
            inks.push(ink.map_or('.', |c| letter(c, &mut legend)));
        }
        body.push_str(&format!("{row:2} bg {bgs}\n{row:2} fg {inks}\n"));
    }
    let legend: Vec<String> = legend
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "{}=#{:02x}{:02x}{:02x}",
                char::from(b'a' + i as u8),
                c.0,
                c.1,
                c.2
            )
        })
        .collect();
    format!(
        "# {name}: {}x{} frame, cell {}x{}, hash {:016x}\n# legend {}\n{body}",
        f.width,
        f.height,
        r.metrics().width,
        r.metrics().height,
        f.hash(),
        legend.join(" ")
    )
}

#[track_caller]
fn check_golden(name: &str, actual: &str) {
    let path = format!(
        "{}/tests/golden/render_{name}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    if std::env::var_os("SUBAK_BLESS").is_some() {
        std::fs::create_dir_all(format!("{}/tests/golden", env!("CARGO_MANIFEST_DIR"))).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing golden {path}; run with SUBAK_BLESS=1 and review it"))
        .replace("\r\n", "\n");
    assert_eq!(
        actual, want,
        "golden {name} differs (SUBAK_BLESS=1 refreshes it)"
    );
}

const SCENE: &[u8] = "\x1b[31mR\x1b[1mB\x1b[0m \x1b[7mI\x1b[0m \x1b[2mD\x1b[0m \x1b[8mH\x1b[0m \
\x1b[4mu\x1b[24m\x1b[21md\x1b[24m\x1b[9ms\x1b[29m\x1b[53mo\x1b[55m \x1b[44m  \x1b[0m \
\x1b[38;2;255;0;0;48;2;0;0;255mT\x1b[0m\r\n한e\u{301}😀 \x1b]8;;https://e.example\x1b\\L\x1b]8;;\x1b\\\r\n\
┌─┬─┐█▀\r\n└─┴─┘░▄"
    .as_bytes();

#[test]
fn colours_attributes_and_golden_frames() {
    let t = Theme::kim_dark();
    let v = view(24, 5, SCENE);
    let mut r = builtin(1.0);
    let d = r.render(&v);
    assert!(d.full, "the first frame is drawn in full");
    let bg = t.background;
    // Red; bold red uses the bright red (bold is bright).
    assert_eq!(cell_colors(&r, 0, 0), (bg, Some(t.ansi[1])));
    assert_eq!(cell_colors(&r, 1, 0), (bg, Some(t.ansi[9])));
    // Inverse: the default foreground becomes the background.
    assert_eq!(cell_colors(&r, 3, 0), (t.foreground, Some(bg)));
    // Dim mixes toward the background; hidden draws nothing.
    let dim = cell_colors(&r, 5, 0).1.unwrap();
    assert!(dim != t.foreground && dim.0 < t.foreground.0, "{dim:?}");
    assert_eq!(cell_colors(&r, 7, 0), (bg, None));
    // Decorations put ink below/through/above the glyph.
    for col in [9, 10, 11, 12] {
        assert!(cell_colors(&r, col, 0).1.is_some(), "decorated cell {col}");
    }
    // Background colour fills whole cells; 24-bit colours are exact.
    assert_eq!(cell_colors(&r, 14, 0).0, t.ansi[4]);
    assert_eq!(cell_colors(&r, 15, 0).0, t.ansi[4]);
    assert_eq!(
        cell_colors(&r, 17, 0),
        (Rgb(0, 0, 255), Some(Rgb(255, 0, 0)))
    );
    // A wide glyph spans both of its cells; the combining mark changes the glyph.
    assert!(
        ink_columns(&r, 0, 1) > 0 && ink_columns(&r, 1, 1) > 0,
        "한 spans two cells"
    );
    assert!(
        ink_columns(&r, 3, 1) > 0 && ink_columns(&r, 4, 1) > 0,
        "😀 spans two cells"
    );
    // Box drawing joins edge to edge: ─ inks every column of its cell, █ fills it.
    let m = r.metrics();
    assert_eq!(ink_columns(&r, 1, 2), m.width as usize);
    assert!(
        cell_pixels(&r, 5, 2).iter().all(|p| *p == t.foreground),
        "█ fills its cell"
    );
    // The link has its dotted underline.
    assert!(cell_colors(&r, 6, 1).1.is_some());
    check_golden("scene_100", &golden("scene_100", &r, 24, 5));
    let mut r2 = builtin(2.0);
    r2.render(&v);
    // 26 px em at 200 %: 16 × 32 px cells (8 × 16 at 100 %).
    assert_eq!((m.width, m.height), (8, 16));
    assert_eq!((r2.metrics().width, r2.metrics().height), (16, 32));
    check_golden("scene_200", &golden("scene_200", &r2, 24, 5));
}

#[test]
fn cursor_shapes_focus_and_visibility() {
    let t = Theme::kim_dark();
    // Block cursor (default) on "x": the cell inverts (cursor colour behind, background ink).
    let v = view(6, 2, b"x\x1b[1;1H");
    let mut r = builtin(1.0);
    r.render(&v);
    assert_eq!(cell_colors(&r, 0, 0), (t.cursor, Some(t.background)));
    // Unfocused: a hollow box in the cursor colour around the normal cell.
    r.set_focused(false);
    let d = r.render(&v);
    assert!(d.full);
    let px = cell_pixels(&r, 0, 0);
    assert_eq!(px[0], t.cursor, "the outline's corner");
    assert!(px.contains(&t.foreground), "the glyph is drawn normally");
    r.set_focused(true);
    // Bar (DECSCUSR 6) and underline (4) shapes.
    let v = view(6, 2, b"x\x1b[1;1H\x1b[6 q");
    r.render(&v);
    let m = r.metrics();
    let px = cell_pixels(&r, 0, 0);
    assert_eq!(px[0], t.cursor, "a bar on the left edge");
    assert_eq!(
        px[m.width as usize - 1],
        t.background,
        "not on the right edge"
    );
    let v = view(6, 2, b"x\x1b[1;1H\x1b[4 q");
    r.render(&v);
    let px = cell_pixels(&r, 0, 0);
    let last = ((m.height - 1) * m.width) as usize;
    assert_eq!(px[last], t.cursor, "an underline at the bottom");
    assert_eq!(px[0], t.background);
    // A cursor on a wide character covers both cells; a hidden cursor draws nothing.
    let v = view(6, 2, "한\x1b[1;1H".as_bytes());
    r.render(&v);
    assert_eq!(cell_colors(&r, 0, 0).0, t.cursor);
    assert_eq!(cell_colors(&r, 1, 0).0, t.cursor);
    let v = view(6, 2, b"x\x1b[1;1H\x1b[?25l");
    r.render(&v);
    assert_eq!(cell_colors(&r, 0, 0).0, t.background);
}

#[test]
fn selection_reverse_video_and_themes() {
    let t = Theme::kim_dark();
    let mut v = view(10, 2, b"hello world");
    v.select(0, 0, SelectMode::Word);
    let mut r = builtin(1.0);
    r.render(&v);
    assert_eq!(cell_colors(&r, 0, 0), (t.selection, Some(t.foreground)));
    assert_eq!(cell_colors(&r, 4, 0).0, t.selection);
    assert_eq!(cell_colors(&r, 6, 0).0, t.background, "outside the word");
    // DECSCNM swaps the default colours, padding included.
    let v = view(10, 2, b"\x1b[?5hab\x1b[?25l");
    r.render(&v);
    assert_eq!(cell_colors(&r, 0, 0), (t.foreground, Some(t.background)));
    assert_eq!(r.frame().pixel(0, 0), t.foreground, "the padding follows");
    // A theme change redraws everything in the new colours.
    let hc = Theme::high_contrast(Rgb(0, 0, 0), Rgb(255, 255, 255), Rgb(26, 235, 255));
    let v = view(10, 2, b"ab\x1b[?25l");
    r.render(&v);
    r.set_theme(hc);
    let d = r.render(&v);
    assert!(d.full && d.row_count() == 2);
    assert_eq!(
        cell_colors(&r, 0, 0),
        (Rgb(0, 0, 0), Some(Rgb(255, 255, 255)))
    );
    assert_eq!(r.frame().pixel(0, 0), Rgb(0, 0, 0));
}

/// Step 19: search matches are tinted, the current one is dark text on a light fill (it wins
/// over the selection), and changing the highlights redraws only the rows involved.
#[test]
fn search_highlights_and_the_current_match() {
    let t = Theme::kim_dark();
    let mut v = view(12, 3, b"foo bar foo\r\nnext foo\x1b[?25l");
    let s = subak_term::search::Search::new(
        &v,
        &subak_term::search::Query {
            text: "foo".into(),
            ..Default::default()
        },
        subak_term::search::Budget::default(),
    )
    .unwrap();
    assert_eq!(s.matches().len(), 3);
    let mut r = builtin(1.0);
    r.render(&v);
    v.set_highlights(s.matches().to_vec(), Some(1));
    v.select_span(s.matches()[1]);
    let d = r.render(&v);
    assert_eq!(d.rows, vec![(0, 2)], "only the rows with matches");
    assert_eq!(cell_colors(&r, 0, 0).0, t.search_match);
    assert_eq!(cell_colors(&r, 3, 0).0, t.background, "between matches");
    assert_eq!(
        cell_colors(&r, 8, 0),
        (t.search_current, Some(t.search_current_text)),
        "the current match over the selection"
    );
    assert_eq!(cell_colors(&r, 5, 1).0, t.search_match);
    v.clear_highlights();
    v.clear_selection();
    let d = r.render(&v);
    assert_eq!(d.rows, vec![(0, 2)]);
    assert_eq!(cell_colors(&r, 0, 0).0, t.background);
}

#[test]
fn only_changed_rows_are_drawn() {
    let mut v = view(20, 5, b"one\r\ntwo\r\nthree");
    let mut r = builtin(1.0);
    assert!(r.render(&v).full);
    assert!(r.render(&v).is_empty(), "nothing changed, nothing drawn");
    // Typing changes one row (the cursor moves within it).
    v.feed(b"!");
    let d = r.render(&v);
    assert!(!d.full);
    assert_eq!(d.rows, vec![(2, 3)]);
    // A line feed moves the cursor to the next row: two rows.
    v.feed(b"\r\n");
    assert_eq!(r.render(&v).rows, vec![(2, 4)]);
    // Scrolling the whole screen changes every row.
    v.feed(b"a\r\nb\r\nc\r\nd\r\n");
    assert_eq!(r.render(&v).row_count(), 5);
    // Scrolling back in the view redraws it from the history.
    v.scroll_lines(2);
    let d = r.render(&v);
    assert!(d.row_count() >= 2);
    let before = r.frame().hash();
    v.scroll_to_bottom();
    r.render(&v);
    assert_ne!(before, r.frame().hash());
    let s = r.stats();
    assert!(s.frames >= 7 && s.rows_drawn >= 15);
}

#[test]
fn geometry_scales_with_dpi() {
    let v = view(80, 24, b"x");
    let mut r = builtin(1.0);
    r.render(&v);
    let m1 = r.metrics();
    let (w1, h1) = (r.frame().width, r.frame().height);
    assert_eq!(
        (w1, h1),
        (80 * m1.width + 16, 24 * m1.height + 16),
        "8 px padding"
    );
    assert_eq!(r.grid_size_for(w1, h1), (80, 24));
    assert_eq!(r.cell_at(8, 8), Some((0, 0)));
    assert_eq!(r.cell_at(w1 - 9, h1 - 9), Some((79, 23)));
    assert_eq!(r.cell_at(2, 2), None, "the padding is not a cell");
    r.set_scale(2.0);
    let d = r.render(&v);
    assert!(d.full);
    let m2 = r.metrics();
    assert!(
        m2.height >= 2 * m1.height - 1 && m2.width >= 2 * m1.width - 1,
        "{m1:?} {m2:?}"
    );
    assert_eq!(r.padding(), 16);
    assert_eq!(r.grid_size_for(r.frame().width, r.frame().height), (80, 24));
    assert!(r.memory_bytes() >= r.frame().bytes());
}

#[test]
fn font_size_changes_the_cells_and_redraws() {
    // The Subak window's zoom (Step 16): a larger font, larger cells, the same padding.
    let v = view(10, 2, b"zoom");
    let mut r = builtin(1.0);
    r.render(&v);
    let small = r.metrics();
    r.set_font_size(20.0);
    assert!(r.render(&v).full, "a size change redraws everything");
    let big = r.metrics();
    assert_eq!(big.height, (20.0f32 * 1.2).ceil() as u32);
    assert!(big.width > small.width, "{small:?} {big:?}");
    assert_eq!(r.padding(), 8);
    assert_eq!(r.options().font_size, 20.0);
    assert_eq!(r.frame().height, 2 * big.height + 16);
}

/// Step 20 (`appearance.line_height`, `padding`, `bold_is_bright`, theme `kim-light`): the cell
/// height follows the line height, the padding moves the grid, bold ANSI colours stay or turn
/// bright, and the light theme draws its own tokens.
#[test]
fn line_height_padding_bold_and_the_light_theme() {
    let v = view(10, 2, b"ab\x1b[1;31mR\x1b[0m\x1b[?25l");
    let mut r = builtin(1.0);
    r.render(&v);
    let normal = r.metrics();
    assert_eq!(normal.height, (13.0f32 * 1.2).ceil() as u32);
    let opts = |line_height: f32, padding: f32, bold_is_bright: bool| RenderOptions {
        line_height,
        padding,
        bold_is_bright,
        ..RenderOptions::default()
    };
    r.set_options(opts(1.6, 8.0, true));
    assert!(r.render(&v).full, "a layout change redraws everything");
    assert_eq!(r.metrics().height, (13.0f32 * 1.6).ceil() as u32);
    assert_eq!(r.metrics().width, normal.width, "the advance is unchanged");
    // The same options again: nothing to redraw.
    r.set_options(opts(1.6, 8.0, true));
    assert!(r.render(&v).is_empty());
    // No padding: the first cell starts at the frame's corner.
    r.set_options(opts(1.2, 0.0, true));
    r.render(&v);
    assert_eq!(r.padding(), 0);
    assert_eq!(r.frame().width, 10 * r.metrics().width);
    let t = Theme::kim_dark();
    assert_eq!(
        cell_colors(&r, 2, 0).1,
        Some(t.ansi[9]),
        "bold red is bright"
    );
    r.set_options(opts(1.2, 0.0, false));
    r.render(&v);
    assert_eq!(
        cell_colors(&r, 2, 0).1,
        Some(t.ansi[1]),
        "bold red stays red"
    );
    // The light theme: its own background, foreground and ANSI red.
    let light = Theme::kim_light();
    r.set_theme(light.clone());
    r.render(&v);
    assert_eq!(
        cell_colors(&r, 0, 0),
        (light.background, Some(light.foreground))
    );
    assert_eq!(cell_colors(&r, 2, 0).1, Some(light.ansi[1]));
    assert_eq!(r.frame().pixel(r.frame().width - 1, 0), light.background);
    assert_ne!(light.background, t.background);
}

#[test]
fn the_font_chain_puts_the_users_family_first() {
    use subak_term::render::{MONO_FAMILY, font_chain};
    assert_eq!(font_chain(""), MONO_FAMILY.to_vec());
    let c = font_chain("Courier New");
    assert_eq!(c[0], "Courier New");
    assert_eq!(&c[1..], &MONO_FAMILY[..]);
    // A family of the default chain moves to the front instead of appearing twice.
    let c = font_chain(" consolas ");
    assert_eq!(c[0], "consolas");
    assert_eq!(c.len(), MONO_FAMILY.len());
    assert!(!c[1..].contains(&"Consolas"));
}

/// Step 20 (`appearance.font_family`): the user's family draws Latin text when it is installed
/// (Courier New and Consolas ship with Windows 10 and 11), the chain still sends Korean to
/// Malgun Gothic, and a family that is not installed is skipped.
#[cfg(windows)]
#[test]
fn a_chosen_font_family_draws_and_falls_back() {
    use subak_term::render::font_chain;
    let v = view(10, 1, "Aa 한\x1b[?25l".as_bytes());
    let mut glyphs = Vec::new();
    for family in ["Courier New", "Consolas"] {
        let mut r = Renderer::with_font_chain(
            Theme::kim_dark(),
            RenderOptions::default(),
            &font_chain(family),
        )
        .expect("system fonts");
        assert_eq!(r.face_for('A').family, family);
        assert!(!r.face_for('A').fallback);
        assert_eq!(r.face_for('한').family, "Malgun Gothic");
        r.render(&v);
        glyphs.push(cell_pixels(&r, 0, 0));
    }
    assert_ne!(glyphs[0], glyphs[1], "two families draw `A` differently");
    let mut r = Renderer::with_font_chain(
        Theme::kim_dark(),
        RenderOptions::default(),
        &font_chain("No Such Font Family 20"),
    )
    .expect("the rest of the chain");
    let primary = r.face_for('A').family;
    assert!(
        primary == "Cascadia Mono" || primary == "Consolas",
        "{primary}"
    );
    assert!(
        Renderer::with_font_chain(Theme::kim_dark(), RenderOptions::default(), &["Nope"]).is_err()
    );
}

#[cfg(windows)]
#[test]
fn system_fonts_draw_korean_emoji_and_symbols_in_their_cells() {
    let t = Theme::kim_dark();
    let v = view(30, 3, "한글 수박 😀 ✓ → ┼ é\r\nabc".as_bytes());
    let mut r = Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
        .expect("system fonts");
    assert_eq!(r.rasterizer_name(), "gdi");
    r.render(&v);
    assert_eq!(r.face_for('한').family, "Malgun Gothic");
    assert_eq!(r.face_for('😀').family, "Segoe UI Emoji");
    assert_eq!(r.face_for('┼').family, "procedural");
    // Every visible character has ink in its own cells; the wide ones in both halves; nothing
    // spills into the row below.
    for (col, wide) in [
        (0, true),
        (2, true),
        (5, true),
        (7, true),
        (10, true),
        (13, false),
        (15, false),
        (17, false),
        (19, false),
    ] {
        assert!(ink_columns(&r, col, 0) > 0, "cell {col}");
        if wide {
            assert!(ink_columns(&r, col + 1, 0) > 0, "second half of cell {col}");
        }
    }
    assert_eq!(
        cell_colors(&r, 25, 0),
        (t.background, None),
        "blank cells stay blank"
    );
    assert!(ink_columns(&r, 0, 1) > 0);
    // The same at 200 %: re-rasterized, not scaled.
    let before = r.glyph_cache().len();
    r.set_scale(2.0);
    r.render(&v);
    assert!(r.metrics().height >= 30);
    assert!(ink_columns(&r, 0, 0) > 0 && ink_columns(&r, 1, 0) > 0);
    assert!(r.glyph_cache().len() >= before.min(5));
}
