//! Tests of the annotation model, its editing and its pixels (Step 56; NOEUL-FR-080, NOEUL-FR-081,
//! NOEUL-FR-083, NOEUL-NFR-014).

use super::*;
use crate::frame::Ledger;
use std::sync::Arc;

const D100: u32 = 96;
const D150: u32 = 144;

/// A drag with the current tool from `a` to `b` (press, two moves, release).
fn stroke(e: &mut Editor, a: (i32, i32), b: (i32, i32), dpi: u32) {
    assert!(e.press(a, dpi), "the press draws");
    let mid = ((a.0 + b.0) / 2, (a.1 + b.1) / 2);
    e.drag(mid, false);
    e.drag(b, false);
    e.release(b, false);
}

fn with_tool(t: Tool) -> Editor {
    let mut e = Editor::default();
    e.set_tool(Some(t));
    e
}

fn canvas(r: Rect, bg: [u8; 4]) -> (Frame, Arc<Ledger>) {
    let l = Ledger::new();
    (Frame::solid(&l, r, bg).unwrap(), l)
}

fn px(f: &Frame, x: i32, y: i32) -> [u8; 4] {
    let r = f.rect();
    let i = (((y - r.y) as u32 * r.width + (x - r.x) as u32) * 4) as usize;
    let p = &f.pixels()[i..i + 4];
    [p[0], p[1], p[2], p[3]]
}

/// The frame as text: `#` where the pixel is exactly the colour, `+` where it was changed but
/// blended, `.` where it is the background.
fn ascii(f: &Frame, c: Rgba, bg: [u8; 4]) -> Vec<String> {
    let r = f.rect();
    (r.y..r.y + r.height as i32)
        .map(|y| {
            (r.x..r.x + r.width as i32)
                .map(|x| {
                    let p = px(f, x, y);
                    if p == [c.b, c.g, c.r, 255] {
                        '#'
                    } else if p == bg {
                        '.'
                    } else {
                        '+'
                    }
                })
                .collect()
        })
        .collect()
}

#[test]
fn every_tool_creates_an_object_that_undo_and_redo_restore_exactly() {
    for tool in Tool::ALL {
        let mut e = with_tool(tool);
        if tool == Tool::Text {
            assert!(e.press((10, 10), D100));
            assert!(e.editing_text());
            for c in "Hi".chars() {
                e.type_char(c);
            }
            e.end_text();
        } else {
            stroke(&mut e, (10, 10), (40, 30), D100);
        }
        assert_eq!(e.objects().len(), 1, "{tool:?}");
        let o = e.objects()[0].clone();
        assert_eq!(o.shape.kind(), tool);
        assert_eq!(o.color, PALETTE[0].1, "the default colour");
        assert_eq!((o.level, o.dpi), (DEFAULT_LEVEL, D100));
        assert_eq!(
            e.doc().undo_steps(),
            1,
            "one drag (or one label) is one step"
        );
        assert!(e.undo());
        assert!(e.objects().is_empty(), "{tool:?}");
        assert!(e.redo());
        assert_eq!(e.objects(), [o], "redo brings back exactly the same object");
        assert!(!e.redo(), "nothing more to redo");
    }
}

#[test]
fn shapes_follow_the_drag_and_shift_squares_or_snaps() {
    let mut e = with_tool(Tool::Rectangle);
    stroke(&mut e, (10, 10), (20, 15), D100);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Rectangle(Bounds::new(10.0, 10.0, 21.0, 16.0)),
        "both pixels inclusive"
    );
    // Shift: a square (and a circle with the ellipse).
    let mut e = with_tool(Tool::Ellipse);
    e.press((10, 10), D100);
    e.drag((30, 15), true);
    e.release((30, 15), true);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Ellipse(Bounds::new(10.0, 10.0, 31.0, 31.0))
    );
    // Shift snaps a line to 45°.
    let mut e = with_tool(Tool::Line);
    e.press((0, 0), D100);
    e.drag((100, 4), true);
    e.release((100, 4), true);
    let Shape::Line(a, b) = e.objects()[0].shape.clone() else {
        panic!()
    };
    assert!((b.y - a.y).abs() < 0.01, "horizontal: {a:?} {b:?}");
    // The pen keeps every point; a click without a drag keeps a dot, a box needs a drag.
    let mut e = with_tool(Tool::Pen);
    e.press((5, 5), D100);
    for x in 6..15 {
        e.drag((x, 5 + x % 3), false);
    }
    e.release((14, 7), false);
    let Shape::Pen(pts) = &e.objects()[0].shape else {
        panic!()
    };
    assert_eq!(pts.len(), 10);
    e.press((50, 50), D100);
    e.release((50, 50), false);
    assert_eq!(e.objects().len(), 2, "a dot");
    let mut e = with_tool(Tool::Rectangle);
    e.press((5, 5), D100);
    e.release((5, 5), false);
    assert!(e.objects().is_empty(), "a click draws no box");
    assert_eq!(e.doc().undo_steps(), 0, "and records no step");
}

#[test]
fn drawing_stays_inside_the_selection() {
    let mut e = with_tool(Tool::Rectangle);
    e.set_clip(Rect::new(100, 100, 50, 40));
    stroke(&mut e, (90, 90), (500, 500), D100);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Rectangle(Bounds::new(100.0, 100.0, 150.0, 140.0))
    );
}

#[test]
fn objects_are_selected_moved_resized_reordered_and_deleted_with_undo() {
    let mut e = with_tool(Tool::Rectangle);
    stroke(&mut e, (10, 10), (50, 40), D100);
    stroke(&mut e, (100, 10), (140, 40), D100);
    let (a, b) = (e.objects()[0].id, e.objects()[1].id);
    let drawn = e.objects().to_vec();
    // A press on the first rectangle's outline picks it; a drag moves it.
    assert!(e.press((10, 25), D100));
    assert_eq!(e.selected(), Some(a));
    e.drag((15, 30), false);
    e.release((15, 30), false);
    assert_eq!(
        e.doc().get(a).unwrap().shape,
        Shape::Rectangle(Bounds::new(15.0, 15.0, 56.0, 46.0))
    );
    assert_eq!(
        e.measure().as_deref(),
        Some("41 \u{d7} 31"),
        "the size readout"
    );
    // Its bottom-right handle resizes it.
    let h = e.selected_handles();
    assert_eq!(h.len(), 8);
    assert_eq!(e.handle_at((56, 46), D100).map(|x| x.0), Some(a));
    assert!(e.press((56, 46), D100));
    e.drag((80, 70), false);
    e.release((80, 70), false);
    assert_eq!(
        e.doc().get(a).unwrap().shape,
        Shape::Rectangle(Bounds::new(15.0, 15.0, 81.0, 71.0))
    );
    // Reordering: the first goes on top, then back.
    assert!(e.reorder(Order::Forward));
    assert_eq!(e.objects()[1].id, a);
    assert!(!e.reorder(Order::Front), "already on top");
    assert!(e.reorder(Order::Back));
    assert_eq!(e.objects()[0].id, a);
    // Colour and width apply to the selected object.
    e.set_color(4);
    e.step_width(true);
    let o = e.doc().get(a).unwrap();
    assert_eq!((o.color, o.level), (PALETTE[4].1, DEFAULT_LEVEL + 1));
    assert_eq!(e.doc().get(b).unwrap().color, PALETTE[0].1, "not the other");
    // Delete, then clear all; both undo.
    assert!(e.delete_selected());
    assert_eq!(e.objects().len(), 1);
    assert!(e.clear_all());
    assert!(e.objects().is_empty());
    let mut steps = 0;
    while e.undo() {
        steps += 1;
    }
    assert_eq!(
        steps, 10,
        "two drawings, move, resize, two reorders, colour, width, delete, clear"
    );
    assert!(e.objects().is_empty(), "back to the start");
    while e.redo() {}
    assert!(e.objects().is_empty(), "redo to the end: cleared");
    e.undo();
    e.undo();
    assert_eq!(e.objects().len(), 2, "before delete and clear");
    let _ = drawn;
}

#[test]
fn keyboard_editing_selects_nudges_grows_and_shrinks() {
    let mut e = with_tool(Tool::Line);
    stroke(&mut e, (10, 10), (30, 10), D100);
    let mut r = with_tool(Tool::Rectangle);
    stroke(&mut r, (0, 0), (9, 9), D100);
    // A line: select it with the keyboard, move it, grow its end, shrink (move) its start.
    e.set_tool(None);
    assert!(e.select_step(true));
    assert!(e.nudge(1, 2));
    assert!(e.resize_by((1, 0), 5, true));
    assert!(e.resize_by((1, 0), 3, false));
    assert_eq!(
        e.objects()[0].shape,
        Shape::Line(P::new(14.5, 12.5), P::new(36.5, 12.5))
    );
    // A box: grow the right edge, shrink from the top (Shift+Down moves the top edge down).
    r.set_tool(None);
    r.select_step(false);
    r.resize_by((1, 0), 4, true);
    r.resize_by((0, 1), 3, false);
    assert_eq!(
        r.objects()[0].shape,
        Shape::Rectangle(Bounds::new(0.0, 3.0, 14.0, 10.0))
    );
    assert_eq!(r.doc().undo_steps(), 3);
}

#[test]
fn the_keyboard_draws_with_two_marks() {
    let mut e = with_tool(Tool::Arrow);
    assert!(e.mark((10, 10), D150, false));
    assert!(e.marking());
    e.drag((20, 10), false); // the pointer keys moved the pointer
    assert!(e.mark((30, 20), D150, false));
    assert!(!e.marking());
    assert_eq!(
        e.objects()[0].shape,
        Shape::Arrow(P::new(10.5, 10.5), P::new(30.5, 20.5))
    );
    assert_eq!(e.objects()[0].dpi, D150);
    // A release during a keyboard gesture does not end it.
    assert!(e.mark((0, 0), D150, false));
    e.release((5, 5), false);
    assert!(e.marking());
    e.done();
    assert_eq!(e.objects().len(), 1, "Escape aborts the shape being drawn");
}

#[test]
fn escape_ends_text_then_gestures_then_the_tool_and_then_nothing() {
    let mut e = with_tool(Tool::Text);
    e.press((0, 0), D100);
    e.type_char('a');
    assert!(e.done(), "ends the label");
    assert!(!e.editing_text());
    assert_eq!(e.tool(), Some(Tool::Text), "the tool stays");
    assert!(e.done(), "leaves the tool");
    assert_eq!(e.tool(), None);
    assert!(
        !e.done(),
        "nothing left: Escape belongs to the outer context"
    );
    assert_eq!(e.objects().len(), 1, "the drawing is kept");
}

#[test]
fn text_is_typed_edited_and_one_undo_step() {
    let mut e = with_tool(Tool::Text);
    e.press((100, 50), D100);
    for c in "Hello\u{7}".chars() {
        e.type_char(c); // the bell is a control character: ignored
    }
    e.text_key(TextKey::Newline);
    for c in "wrld".chars() {
        e.type_char(c);
    }
    e.text_key(TextKey::Left);
    e.text_key(TextKey::Left);
    e.text_key(TextKey::Left);
    e.type_char('o');
    e.text_key(TextKey::End);
    e.text_key(TextKey::Backspace);
    e.type_char('d');
    e.text_key(TextKey::Home);
    e.text_key(TextKey::Delete);
    e.type_char('W');
    e.typeset(&BlockType);
    let marks = e.text_marks().unwrap();
    assert_eq!(
        marks.caret.y,
        50 + 23,
        "on the second line (18 px text, 1.25 em lines, rounded up)"
    );
    e.end_text();
    let Shape::Text(t) = &e.objects()[0].shape else {
        panic!()
    };
    assert_eq!(t.text, "Hello\nWorld");
    assert_eq!(e.doc().undo_steps(), 1, "a whole label is one step");
    e.undo();
    assert!(e.objects().is_empty());
    // An empty label leaves nothing, not even an undo step.
    e.press((0, 0), D100);
    e.end_text();
    assert!(e.objects().is_empty());
    assert_eq!(e.doc().undo_steps(), 0);
}

#[test]
fn korean_composition_shows_inline_and_becomes_text_only_when_committed() {
    let mut e = with_tool(Tool::Text);
    e.press((0, 0), D100);
    let text = |e: &Editor| match &e.objects()[0].shape {
        Shape::Text(t) => t.text.clone(),
        _ => panic!(),
    };
    // 한 = ㅎ + ㅏ + ㄴ, then 글 = ㄱ + ㅡ + ㄹ (what the Microsoft Korean IME reports).
    e.ime(Ime::Start);
    e.ime(Ime::Compose("\u{314e}".into())); // ㅎ
    assert!(e.composing());
    e.ime(Ime::Compose("\u{d558}".into())); // 하
    e.ime(Ime::Compose("\u{d55c}".into())); // 한
    assert_eq!(text(&e), "\u{d55c}", "shown while composing");
    e.typeset(&BlockType);
    let marks = e.text_marks().unwrap();
    assert_eq!(marks.composition.len(), 1, "underlined");
    // Keys go to the IME during a composition.
    e.text_key(TextKey::Backspace);
    assert_eq!(text(&e), "\u{d55c}");
    e.ime(Ime::Commit("\u{d55c}".into()));
    assert!(!e.composing());
    e.ime(Ime::Compose("\u{3131}".into())); // ㄱ
    e.ime(Ime::Compose("\u{ae00}".into())); // 글
    e.ime(Ime::Commit("\u{ae00}".into()));
    e.ime(Ime::End);
    assert_eq!(text(&e), "\u{d55c}\u{ae00}", "한글");
    // A composition that ends without a commit is dropped.
    e.ime(Ime::Start);
    e.ime(Ime::Compose("\u{3131}".into()));
    e.ime(Ime::End);
    assert_eq!(text(&e), "\u{d55c}\u{ae00}");
    // Ending the label in the middle of a composition drops it too.
    e.ime(Ime::Compose("\u{3131}".into()));
    e.end_text();
    assert_eq!(text(&e), "\u{d55c}\u{ae00}");
    // Characters outside the BMP (emoji) and right-to-left text are just text.
    e.edit_text(e.objects()[0].id);
    for c in " \u{1F600} \u{5e9}\u{5dc}\u{5d5}\u{5dd}".chars() {
        e.type_char(c);
    }
    e.end_text();
    assert_eq!(
        text(&e).chars().count(),
        2 + 3 + 4,
        "한글, two spaces, one emoji, four Hebrew letters"
    );
}

#[test]
fn a_double_click_selects_a_word_and_typing_replaces_it() {
    let mut e = with_tool(Tool::Text);
    e.press((0, 0), D100);
    for c in "one two three".chars() {
        e.type_char(c);
    }
    e.typeset(&BlockType);
    // "two" starts at character 4; BlockType advances 11 px per character at 18 px.
    assert!(e.select_word_at((4 * 11 + 3, 5)));
    let marks = e.text_marks().unwrap();
    assert_eq!(marks.selection, [Rect::new(44, 0, 33, 23)]);
    e.type_char('2');
    e.end_text();
    let Shape::Text(t) = &e.objects()[0].shape else {
        panic!()
    };
    assert_eq!(t.text, "one 2 three");
}

#[test]
fn long_text_and_many_lines_typeset_and_stay_editable() {
    let mut e = with_tool(Tool::Text);
    e.press((0, 0), D100);
    let long: String = "annotation ".repeat(200);
    for c in long.chars() {
        e.type_char(c);
    }
    for _ in 0..30 {
        e.text_key(TextKey::Newline);
        e.type_char('x');
    }
    e.typeset(&BlockType);
    e.end_text();
    let o = &e.objects()[0];
    let b = o.bounds();
    assert_eq!(b.width(), (2200 * 11) as f32);
    assert_eq!(b.height(), (31 * 23) as f32);
    assert!(o.hit(P::new(10.0, 10.0), 0.0));
}

#[test]
fn undo_and_redo_are_lossless_over_a_long_session() {
    let mut e = Editor::default();
    let mut states = vec![e.objects().to_vec()];
    for (i, t) in Tool::ALL.iter().cycle().take(21).enumerate() {
        e.set_tool(Some(*t));
        // Apart, so every press draws (a press on an object would pick it).
        let base = i32::try_from(i).unwrap() * 45;
        if *t == Tool::Text {
            e.press((base, 0), D100);
            e.type_char('x');
            e.end_text();
        } else {
            stroke(&mut e, (base, 0), (base + 30, 20), D150);
        }
        states.push(e.objects().to_vec());
    }
    assert_eq!(e.objects().len(), 21);
    e.set_tool(None);
    e.select_step(true);
    e.nudge(3, 3);
    states.push(e.objects().to_vec());
    e.clear_all();
    states.push(e.objects().to_vec());
    for want in states.iter().rev().skip(1) {
        assert!(e.undo());
        assert_eq!(e.objects(), want.as_slice());
    }
    assert!(!e.undo());
    for want in states.iter().skip(1) {
        assert!(e.redo());
        assert_eq!(e.objects(), want.as_slice());
    }
}

#[test]
fn widths_scale_with_the_monitor_dpi() {
    for (tool, at100, at150) in [
        (Tool::Rectangle, 3, 5),
        (Tool::Pen, 3, 5),
        (Tool::Marker, 14, 21),
        (Tool::Text, 18, 27),
    ] {
        assert_eq!(stroke_px(tool, DEFAULT_LEVEL, D100), at100, "{tool:?}");
        assert_eq!(stroke_px(tool, DEFAULT_LEVEL, D150), at150, "{tool:?}");
    }
    assert_eq!(stroke_px(Tool::Line, 0, D100), 1);
    assert_eq!(stroke_px(Tool::Line, 99, D100), 12, "levels are clamped");
}

// ----- pixels: golden images -----

const BG: [u8; 4] = [0x40, 0x30, 0x20, 0xFF];

fn rect_object(b: Bounds, level: u8, dpi: u32) -> Object {
    Object {
        id: 1,
        shape: Shape::Rectangle(b),
        color: PALETTE[0].1,
        level,
        dpi,
    }
}

#[test]
fn a_rectangle_is_crisp_at_100_and_150_percent() {
    let red = PALETTE[0].1;
    // Level 1: 2 logical pixels → 2 device pixels at 100 %, 3 at 150 %.
    let (base, _l) = canvas(Rect::new(0, 0, 12, 9), BG);
    let o = rect_object(Bounds::new(1.0, 1.0, 11.0, 8.0), 1, D100);
    let f = flatten(&base, &[o], base.rect()).unwrap();
    assert_eq!(
        ascii(&f, red, BG),
        [
            "............",
            ".##########.",
            ".##########.",
            ".##......##.",
            ".##......##.",
            ".##......##.",
            ".##########.",
            ".##########.",
            "............",
        ]
    );
    let o = rect_object(Bounds::new(1.0, 1.0, 11.0, 8.0), 1, D150);
    let f = flatten(&base, &[o], base.rect()).unwrap();
    assert_eq!(
        ascii(&f, red, BG),
        [
            "............",
            ".##########.",
            ".##########.",
            ".##########.",
            ".###....###.",
            ".##########.",
            ".##########.",
            ".##########.",
            "............",
        ]
    );
}

#[test]
fn lines_arrows_and_ellipses_cover_the_expected_pixels() {
    let red = PALETTE[0].1;
    let (base, _l) = canvas(Rect::new(0, 0, 20, 9), BG);
    // A horizontal 3-pixel line through row 4, from x 3 to 16 (pixel centres).
    let o = Object {
        id: 1,
        shape: Shape::Line(P::of_pixel((3, 4)), P::of_pixel((16, 4))),
        color: red,
        level: DEFAULT_LEVEL,
        dpi: D100,
    };
    let f = flatten(&base, &[o], base.rect()).unwrap();
    let rows = ascii(&f, red, BG);
    // Round caps: half the width beyond each end.
    assert_eq!(rows[4], "..################..", "{rows:#?}");
    assert_eq!(rows[3], rows[5], "symmetric");
    assert_eq!(rows[3], "..+##############+..", "{rows:#?}");
    assert_eq!(rows[1], "....................", "nothing two rows away");
    // At 150 % the same line is 5 pixels wide.
    let o = Object {
        id: 1,
        shape: Shape::Line(P::of_pixel((3, 4)), P::of_pixel((16, 4))),
        color: red,
        level: DEFAULT_LEVEL,
        dpi: D150,
    };
    let f = flatten(&base, &[o], base.rect()).unwrap();
    let rows = ascii(&f, red, BG);
    for y in 2..=6 {
        assert_eq!(&rows[y][5..15], "##########", "row {y}: {rows:#?}");
    }
    assert!(!rows[1].contains('#') && !rows[7].contains('#'));
    // An arrow: the head is wider than the shaft and ends at the second point.
    let (base, _l) = canvas(Rect::new(0, 0, 60, 41), BG);
    let o = Object {
        id: 1,
        shape: Shape::Arrow(P::of_pixel((2, 20)), P::of_pixel((55, 20))),
        color: red,
        level: DEFAULT_LEVEL,
        dpi: D100,
    };
    let f = flatten(&base, &[o], base.rect()).unwrap();
    let col = |x: i32| {
        (0..41)
            .filter(|y| px(&f, x, *y) == [red.b, red.g, red.r, 255])
            .count()
    };
    assert_eq!(col(10), 3, "the shaft");
    // 16 px long, 7.2 px half width at the base (x 39.5), narrowing to the tip (x 55.5).
    assert!(col(41) > 9, "the head's base: {}", col(41));
    assert!(
        col(41) > col(52) && col(52) >= 1,
        "it narrows: {} {}",
        col(41),
        col(52)
    );
    assert_eq!(px(&f, 58, 20), BG, "nothing beyond the tip");
    // An ellipse is symmetric and its centre is untouched.
    let (base, _l) = canvas(Rect::new(0, 0, 41, 31), BG);
    let o = Object {
        id: 1,
        shape: Shape::Ellipse(Bounds::new(0.0, 0.0, 41.0, 31.0)),
        color: red,
        level: DEFAULT_LEVEL,
        dpi: D100,
    };
    let f = flatten(&base, &[o], base.rect()).unwrap();
    for y in 0..31 {
        for x in 0..41 {
            assert_eq!(px(&f, x, y), px(&f, 40 - x, y), "mirror x at {x},{y}");
            assert_eq!(px(&f, x, y), px(&f, x, 30 - y), "mirror y at {x},{y}");
        }
    }
    assert_eq!(px(&f, 20, 15), BG, "the inside");
    assert_eq!(
        px(&f, 20, 1),
        [red.b, red.g, red.r, 255],
        "the top of the ring"
    );
    assert_eq!(px(&f, 0, 0), BG, "the box corner");
}

#[test]
fn a_marker_is_one_translucent_coat_and_text_uses_its_mask() {
    let yellow = PALETTE[2].1;
    let (base, _l) = canvas(Rect::new(0, 0, 60, 30), [0xFF, 0xFF, 0xFF, 0xFF]);
    // A marker stroke that doubles back over itself.
    let o = Object {
        id: 1,
        shape: Shape::Marker(vec![
            P::of_pixel((5, 15)),
            P::of_pixel((50, 15)),
            P::of_pixel((10, 15)),
        ]),
        color: yellow,
        level: 0,
        dpi: D100,
    };
    let f = flatten(&base, &[o], base.rect()).unwrap();
    let (a, b) = (px(&f, 20, 15), px(&f, 45, 15));
    assert_eq!(a, b, "overlapping parts are not darker");
    assert_ne!(a, [0xFF, 0xFF, 0xFF, 0xFF], "it is visible");
    assert!(a[0] > 0x90, "and translucent over white: {a:?}");
    // Text: exactly the BlockType mask in the label's colour.
    let mut e = with_tool(Tool::Text);
    e.press((2, 2), D100);
    e.type_char('A');
    e.typeset(&BlockType);
    e.end_text();
    let f = flatten(&base, e.objects(), base.rect()).unwrap();
    let red = PALETTE[0].1;
    // 18 px: ink from row 4 to 15 of the label, 10 px wide.
    assert_eq!(px(&f, 2 + 5, 2 + 8), [red.b, red.g, red.r, 255]);
    assert_eq!(px(&f, 2 + 5, 2 + 2), [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn transparent_pictures_keep_their_alpha_where_nothing_is_drawn() {
    let (base, _l) = canvas(Rect::new(0, 0, 10, 10), [0, 0, 0, 0]);
    let o = rect_object(Bounds::new(0.0, 0.0, 5.0, 5.0), 0, D100);
    let f = flatten(&base, &[o], base.rect()).unwrap();
    let red = PALETTE[0].1;
    assert_eq!(px(&f, 0, 0), [red.b, red.g, red.r, 255], "drawn: opaque");
    assert_eq!(px(&f, 2, 2), [0, 0, 0, 0], "inside the outline: untouched");
    assert_eq!(px(&f, 8, 8), [0, 0, 0, 0]);
    // A translucent marker over a transparent pixel: its own colour at its own opacity.
    let m = Object {
        id: 2,
        shape: Shape::Marker(vec![P::of_pixel((7, 7))]),
        color: red,
        level: 0,
        dpi: D100,
    };
    let f = flatten(&base, &[m], base.rect()).unwrap();
    assert_eq!(px(&f, 7, 7), [red.b, red.g, red.r, MARKER_ALPHA]);
    // Half-transparent picture under an opaque stroke: the stroke wins.
    let (half, _l2) = canvas(Rect::new(0, 0, 4, 4), [10, 20, 30, 128]);
    let o = rect_object(Bounds::new(0.0, 0.0, 4.0, 4.0), 0, D100);
    let f = flatten(&half, &[o], half.rect()).unwrap();
    assert_eq!(px(&f, 0, 0), [red.b, red.g, red.r, 255]);
    assert_eq!(px(&f, 1, 1), [10, 20, 30, 128]);
}

#[test]
fn flatten_crops_clips_and_leaves_the_picture_untouched() {
    let (base, l) = canvas(Rect::new(-100, -50, 200, 100), BG);
    let o = rect_object(Bounds::new(-10.0, -10.0, 300.0, 300.0), 0, D100);
    let area = Rect::new(-20, -20, 40, 40);
    let f = flatten(&base, std::slice::from_ref(&o), area).unwrap();
    assert_eq!(f.rect(), area);
    assert_eq!(
        px(&f, -10, 5),
        [PALETTE[0].1.b, PALETTE[0].1.g, PALETTE[0].1.r, 255]
    );
    assert_eq!(px(&f, 19, 19), BG, "the far edges lie outside the area");
    assert_eq!(px(&base, -10, 5), BG, "the frozen picture keeps its pixels");
    assert!(flatten(&base, std::slice::from_ref(&o), Rect::new(90, 0, 20, 10)).is_none());
    assert_eq!(
        inside(std::slice::from_ref(&o), Rect::new(500, 500, 5, 5)).len(),
        0
    );
    assert_eq!(inside(&[o], area).len(), 1);
    drop(f);
    assert_eq!(l.live().0, 1, "only the base frame is alive");
}

#[test]
fn translated_objects_move_by_whole_pixels() {
    let mut e = with_tool(Tool::Pen);
    stroke(&mut e, (10, 10), (20, 20), D100);
    let o = e.objects()[0].clone();
    let t = translate_all(std::slice::from_ref(&o), -10, -10);
    assert_eq!(translate_all(&t, 10, 10), [o]);
    let Shape::Pen(p) = &t[0].shape else { panic!() };
    assert_eq!(p[0], P::new(0.5, 0.5));
}

#[test]
fn the_block_typesetter_gives_carets_and_lines() {
    let g = BlockType.typeset("ab\nc", 20);
    assert_eq!((g.width, g.height, g.line_height), (24, 50, 25));
    assert_eq!(g.carets, [vec![0, 12, 24], vec![0, 12]]);
    assert_eq!(
        caret_pos(&g, 3),
        (0, 25),
        "after the newline: the second line"
    );
}
