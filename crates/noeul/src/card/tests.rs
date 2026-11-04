//! Card layout and pixels (Step 55, NOEUL-FR-071) with block glyphs (`BlockText`), so every
//! size and pixel is exact; GDI's own glyphs are checked in `win::card`.

use super::*;

fn at(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width() + x) * 4) as usize;
    let p = &f.pixels()[i..i + 4];
    [p[0], p[1], p[2], p[3]]
}

fn bgra(c: Rgb) -> [u8; 4] {
    [c[2], c[1], c[0], 255]
}

/// Pixels of colour `c` in the frame.
fn count(f: &Frame, c: Rgb) -> usize {
    f.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| **p == bgra(c))
        .count()
}

#[test]
fn a_text_card_is_opaque_padded_and_on_the_kim_surface() {
    let ledger = Ledger::new();
    let doc = Doc::from_text("Hello");
    let f = render(&ledger, &doc, 96, &mut BlockText).unwrap();
    // 5 characters × 8 px (0.6 × 14) + 2 × 12 padding; one line of 11 + 2 px.
    assert_eq!(f.width(), 5 * 8 + 24);
    assert_eq!(f.height(), 11 + 2 + 24);
    assert!(
        f.pixels().as_chunks::<4>().0.iter().all(|p| p[3] == 255),
        "opaque"
    );
    assert_eq!(at(&f, 0, 0), bgra(SURFACE));
    assert_eq!(at(&f, 11, 20), bgra(SURFACE), "padding");
    assert!(count(&f, INK) > 100, "the text in text.primary");
    // 150 %: everything scales.
    let big = render(&ledger, &doc, 144, &mut BlockText).unwrap();
    assert_eq!(big.width(), 5 * 12 + 36);
    drop((f, big));
    assert_eq!(ledger.live(), (0, 0));
}

#[test]
fn long_lines_wrap_at_the_maximum_width_and_long_words_break() {
    let ledger = Ledger::new();
    let words = "lorem ".repeat(200);
    let f = render(&ledger, &Doc::from_text(&words), 96, &mut BlockText).unwrap();
    assert!(f.width() <= MAX_WIDTH + 2 * PADDING, "{}", f.width());
    assert!(f.width() >= MAX_WIDTH - 48, "a full line: {}", f.width());
    assert!(f.height() > 10 * 14, "many lines: {}", f.height());
    // One word longer than a line: broken between characters, never wider than the card.
    let word = "x".repeat(500);
    let f = render(&ledger, &Doc::from_text(&word), 96, &mut BlockText).unwrap();
    assert!(f.width() <= MAX_WIDTH + 2 * PADDING);
    assert!(f.height() >= 7 * 14, "{}", f.height());
}

#[test]
fn text_documents_keep_lines_and_detect_code() {
    let d = Doc::from_text("a\r\n\r\n  b\tc\n\n\n");
    assert_eq!(d.blocks.len(), 3, "trailing empty lines dropped");
    assert_eq!(d.blocks[2].runs[0].text, "  b    c", "tabs as four spaces");
    assert!(d.blocks[0].runs[0].style.mono, "a tab: code");
    let prose = Doc::from_text("One line.\nAnother line.");
    assert!(!prose.blocks[0].runs[0].style.mono);
    let code = Doc::from_text("fn x() {\n    y();\n}");
    assert!(code.blocks[0].runs[0].style.mono);
    assert_eq!(code.plain_text(), "fn x() {\n    y();\n}");
    let huge = "line\n".repeat(MAX_LINES + 10);
    let d = Doc::from_text(&huge);
    assert!(d.truncated && d.blocks.len() == MAX_LINES);
    // The card says it left something out.
    let ledger = Ledger::new();
    let f = render(&ledger, &d, 96, &mut BlockText).unwrap();
    assert!(count(&f, INK_SOFT) > 0, "the note in text.secondary");
    assert!(f.height() < 10_000);
}

/// No colour can hide text: below 3:1 against what is behind it, the card's text colour is used.
#[test]
fn illegible_colours_fall_back_to_the_text_colour() {
    assert!(contrast([0, 0, 0], [255, 255, 255]) > 20.0);
    assert!((contrast([10, 10, 10], [10, 10, 10]) - 1.0).abs() < 1e-9);
    assert_eq!(legible(SURFACE, SURFACE), INK);
    assert_eq!(legible([255, 0, 0], SURFACE), [255, 0, 0]);
    assert_eq!(ink_on([255, 255, 255]), [0x14, 0x13, 0x1A]);
    assert_eq!(ink_on([0x1E, 0x1E, 0x1E]), [0xF4, 0xF3, 0xF8]);
    let ledger = Ledger::new();
    // White text on a white card background: drawn dark instead.
    let mut doc = crate::html::parse(
        "<div style=\"background-color:#ffffff\"><span style=\"color:#fefefe\">ghost</span></div>",
    );
    assert_eq!(doc.background, Some([255, 255, 255]));
    let f = render(&ledger, &doc, 96, &mut BlockText).unwrap();
    assert_eq!(at(&f, 0, 0), [255, 255, 255, 255]);
    assert!(count(&f, [0x14, 0x13, 0x1A]) > 50, "dark text on white");
    assert_eq!(
        count(&f, [0xFE, 0xFE, 0xFE]),
        0,
        "never the invisible colour"
    );
    // The Kim surface's own colour as text: drawn in text.primary.
    doc = crate::html::parse("<p><span style=\"color:#1C1B24\">ghost</span></p>");
    let f = render(&ledger, &doc, 96, &mut BlockText).unwrap();
    assert!(count(&f, INK) > 50);
    // A highlight is drawn behind its run.
    doc = crate::html::parse("<p><mark>hi</mark></p>");
    let f = render(&ledger, &doc, 96, &mut BlockText).unwrap();
    assert!(count(&f, [0xF5, 0xD7, 0x9C]) > 20);
}

#[test]
fn rules_quotes_items_and_links_are_drawn() {
    let ledger = Ledger::new();
    let doc = crate::html::parse(
        "<p>a</p><hr><blockquote>q</blockquote><ul><li>i</li></ul><p><a href=x>link</a></p>",
    );
    let f = render(&ledger, &doc, 96, &mut BlockText).unwrap();
    assert!(count(&f, LINE) > 20, "the rule and the quote bar");
    assert!(count(&f, ACCENT) > 20, "the link in accent, underlined");
    assert_eq!(doc.plain_text(), "a\nq\n\u{2022} i\nlink");
}

#[test]
fn a_file_card_lists_names_only() {
    let d = Doc::files(&["a.docx".into(), "b\u{202E}txt.exe".into()], 3);
    assert_eq!(
        d.plain_text(),
        "5 copied files\n\u{2022} a.docx\n\u{2022} btxt.exe"
    );
    assert!(d.truncated);
    let ledger = Ledger::new();
    assert!(render(&ledger, &d, 96, &mut BlockText).is_ok());
    assert_eq!(
        Doc::files(&["x".into()], 0).blocks[0].runs[0].text,
        "1 copied file"
    );
}

#[test]
fn a_colour_card_shows_the_colour_exactly_and_its_values() {
    let ledger = Ledger::new();
    let c = Rgba::rgb(0x1E, 0x90, 0xFF);
    let f = swatch(&ledger, c, 96, &mut BlockText).unwrap();
    // The square at the padding.
    assert_eq!(at(&f, 12, 12), [0xFF, 0x90, 0x1E, 255]);
    assert_eq!(at(&f, 12 + 111, 12 + 111), [0xFF, 0x90, 0x1E, 255]);
    assert_eq!(at(&f, 5, 5), bgra(SURFACE));
    assert_eq!(count(&f, [0x1E, 0x90, 0xFF]), 112 * 112);
    assert!(
        count(&f, INK) > 0 && count(&f, INK_SOFT) > 0,
        "HEX and RGB drawn"
    );
    assert!(f.width() >= 112 + 24);
    // Translucent: a checkerboard shows through.
    let half = Rgba { a: 128, ..c };
    let f = swatch(&ledger, half, 96, &mut BlockText).unwrap();
    assert_ne!(at(&f, 12, 12), at(&f, 12 + 8, 12), "the checkerboard");
    assert!(f.pixels().as_chunks::<4>().0.iter().all(|p| p[3] == 255));
}
