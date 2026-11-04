//! Tests of the privacy tools (Step 57; NOEUL-FR-082; ADR 0041): mosaic, blur, the eraser and the
//! solid redaction as objects, their pixels, and what an output keeps of the picture.

use super::*;
use crate::frame::Ledger;
use std::sync::Arc;

const D100: u32 = 96;
const D150: u32 = 144;

/// A picture whose neighbouring pixels all differ (so a pixel that survives is recognisable),
/// with the alpha `alpha(x, y)`.
fn pattern(l: &Arc<Ledger>, r: Rect, alpha: impl Fn(i32, i32) -> u8) -> Frame {
    let mut px = Vec::with_capacity(r.bgra_bytes() as usize);
    for y in r.y..r.y + r.height as i32 {
        for x in r.x..r.x + r.width as i32 {
            px.extend_from_slice(&[
                (x * 7 + y * 13).rem_euclid(256) as u8,
                (x * 3 + y * 5 + 100).rem_euclid(256) as u8,
                (x * 11 + y * 2 + 50).rem_euclid(256) as u8,
                alpha(x, y),
            ]);
        }
    }
    Frame::new(l, r, px).unwrap()
}

fn opaque(l: &Arc<Ledger>, r: Rect) -> Frame {
    pattern(l, r, |_, _| 255)
}

fn px(f: &Frame, x: i32, y: i32) -> [u8; 4] {
    let r = f.rect();
    let i = (((y - r.y) as u32 * r.width + (x - r.x) as u32) * 4) as usize;
    let p = &f.pixels()[i..i + 4];
    [p[0], p[1], p[2], p[3]]
}

fn set_px(f: &mut Frame, x: i32, y: i32, p: [u8; 4]) {
    let r = f.rect();
    let i = (((y - r.y) as u32 * r.width + (x - r.x) as u32) * 4) as usize;
    f.pixels_mut()[i..i + 4].copy_from_slice(&p);
}

fn boxed(l: i32, t: i32, r: i32, b: i32) -> Area {
    Area::Box(Bounds::new(l as f32, t as f32, r as f32, b as f32))
}

fn obj(id: u32, effect: Effect, area: Area, level: u8) -> Object {
    Object {
        id,
        shape: Shape::Effect(effect, area),
        color: PALETTE[REDACT_COLOR].1,
        level,
        dpi: D100,
    }
}

fn pen(id: u32, pts: &[(i32, i32)], level: u8) -> Object {
    Object {
        id,
        shape: Shape::Pen(pts.iter().map(|p| P::of_pixel(*p)).collect()),
        color: PALETTE[4].1,
        level,
        dpi: D100,
    }
}

const BLACK: [u8; 4] = [0x11, 0x11, 0x11, 0xFF];

/// A drag with the current tool, `Ctrl` held or not.
fn drag(e: &mut Editor, a: (i32, i32), b: (i32, i32), ctrl: bool) {
    assert!(e.press_with(a, D100, ctrl), "the press draws");
    e.drag(((a.0 + b.0) / 2, (a.1 + b.1) / 2), false);
    e.drag(b, false);
    e.release(b, false);
}

fn tool(t: Tool) -> Editor {
    let mut e = Editor::default();
    e.set_tool(Some(t));
    e
}

#[test]
fn each_privacy_tool_draws_its_form_and_undo_redo_restore_it_exactly() {
    for t in [Tool::Mosaic, Tool::Blur, Tool::Eraser, Tool::Redact] {
        for ctrl in [false, true] {
            let mut e = tool(t);
            drag(&mut e, (10, 10), (40, 30), ctrl);
            assert_eq!(e.objects().len(), 1, "{t:?}");
            let o = e.objects()[0].clone();
            assert_eq!(o.shape.kind(), t);
            // The eraser is a brush by default, the others a box; Ctrl swaps.
            let brush = (t == Tool::Eraser) != ctrl;
            match &o.shape {
                Shape::Effect(eff, Area::Box(b)) => {
                    assert!(!brush, "{t:?} {ctrl}");
                    assert_eq!(Some(*eff), t.effect());
                    assert_eq!(
                        *b,
                        Bounds::new(10.0, 10.0, 41.0, 31.0),
                        "both pixels inclusive"
                    );
                    assert_eq!(o.measure(), "31 \u{d7} 21");
                }
                Shape::Effect(eff, Area::Brush(p)) => {
                    assert!(brush, "{t:?} {ctrl}");
                    assert_eq!(Some(*eff), t.effect());
                    assert_eq!(p.len(), 3, "every point of the stroke");
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(e.doc().undo_steps(), 1, "one drag is one step");
            assert!(e.undo());
            assert!(e.objects().is_empty());
            assert!(e.redo());
            assert_eq!(e.objects(), [o], "redo restores exactly the same object");
        }
    }
    // A click without a drag draws no box (and no step); a brush keeps a dot.
    let mut e = tool(Tool::Redact);
    e.press((5, 5), D100);
    e.release((5, 5), false);
    assert!(e.objects().is_empty());
    assert_eq!(e.doc().undo_steps(), 0);
    let mut e = tool(Tool::Eraser);
    e.press((5, 5), D100);
    e.release((5, 5), false);
    assert_eq!(e.objects().len(), 1);
}

#[test]
fn strength_and_brush_follow_the_level_and_the_monitor_dpi() {
    assert_eq!(strength_px(Tool::Mosaic, DEFAULT_LEVEL, D100), 8);
    assert_eq!(strength_px(Tool::Mosaic, DEFAULT_LEVEL, D150), 12);
    assert_eq!(strength_px(Tool::Blur, DEFAULT_LEVEL, D100), 4);
    assert_eq!(strength_px(Tool::Blur, DEFAULT_LEVEL, D150), 6);
    assert_eq!(strength_px(Tool::Mosaic, 0, D100), 4);
    assert_eq!(
        strength_px(Tool::Mosaic, 99, D100),
        30,
        "levels are clamped"
    );
    assert_eq!(strength_px(Tool::Pen, DEFAULT_LEVEL, D100), 0);
    for t in [Tool::Mosaic, Tool::Blur, Tool::Eraser, Tool::Redact] {
        assert_eq!(stroke_px(t, DEFAULT_LEVEL, D100), 16, "{t:?}");
        assert_eq!(stroke_px(t, DEFAULT_LEVEL, D150), 24, "{t:?}");
    }
    // The width keys change a selected effect's strength (a live preview: nothing is applied
    // to the picture until an output).
    let mut e = tool(Tool::Blur);
    drag(&mut e, (10, 10), (40, 30), false);
    e.select_step(true);
    assert!(e.step_width(true));
    assert_eq!(e.objects()[0].strength(), 6);
    assert!(e.undo());
    assert_eq!(e.objects()[0].strength(), 4);
}

#[test]
fn a_redaction_starts_black_its_colour_stays_opaque_and_the_chip_says_what_protects() {
    let mut e = tool(Tool::Redact);
    assert_eq!(e.color().0, "black");
    assert_eq!(e.note().as_deref(), Some(REDACT_NOTE));
    e.set_color(7);
    assert_eq!(e.color().0, "white", "the redaction's own colour");
    drag(&mut e, (10, 10), (40, 30), false);
    assert_eq!(e.objects()[0].color, PALETTE[7].1);
    e.set_tool(Some(Tool::Pen));
    assert_eq!(e.color().0, "red", "the other tools keep theirs");
    // A selected redaction takes a chosen colour, always opaque.
    e.set_tool(Some(Tool::Redact));
    e.select_step(true);
    e.step_color(true);
    let c = e.objects()[0].color;
    assert_eq!((c, c.a), (PALETTE[0].1, 255));
    assert_eq!(
        e.note().as_deref(),
        Some(format!("31 \u{d7} 21  {REDACT_NOTE}").as_str()),
        "the readout and the note"
    );
    // Mosaic and blur say they are weak protection; the eraser that it never erases a redaction.
    for (t, note) in [
        (Tool::Mosaic, WEAK_NOTE),
        (Tool::Blur, WEAK_NOTE),
        (Tool::Eraser, ERASER_NOTE),
    ] {
        let mut e = tool(t);
        assert_eq!(e.note().as_deref(), Some(note), "{t:?}");
        assert!(e.press((10, 10), D100));
        e.drag((40, 30), false);
        assert!(e.note().unwrap().ends_with(note), "while drawing: {t:?}");
        e.release((40, 30), false);
    }
    assert!(WEAK_NOTE.contains("weak protection") && WEAK_NOTE.contains("Redact"));
    assert_eq!(tool(Tool::Pen).note(), None);
}

#[test]
fn boxes_are_picked_on_their_edge_erasers_never_by_the_mouse_and_ctrl_always_draws() {
    let mut e = tool(Tool::Redact);
    drag(&mut e, (10, 10), (60, 40), false);
    let id = e.objects()[0].id;
    // On its edge: picked and moved as one step.
    assert!(e.press((10, 25), D100));
    assert_eq!(e.selected(), Some(id));
    e.drag((15, 30), false);
    e.release((15, 30), false);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Effect(
            Effect::Redact,
            Area::Box(Bounds::new(15.0, 15.0, 66.0, 46.0))
        )
    );
    // Its handles resize it, keeping whole-pixel edges.
    assert!(e.press((66, 46), D100));
    e.drag((80, 70), false);
    e.release((80, 70), false);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Effect(
            Effect::Redact,
            Area::Box(Bounds::new(15.0, 15.0, 81.0, 71.0))
        )
    );
    // Inside it: a new redaction over it.
    e.done();
    e.set_tool(Some(Tool::Redact));
    drag(&mut e, (40, 40), (50, 50), false);
    assert_eq!(e.objects().len(), 2);
    // Ctrl on its edge: a brush stroke, never a pick.
    drag(&mut e, (15, 30), (30, 30), true);
    assert_eq!(e.objects().len(), 3);
    assert!(matches!(
        e.objects()[2].shape,
        Shape::Effect(Effect::Redact, Area::Brush(_))
    ));
    // An eraser stroke shows nothing of its own: the mouse never picks it, the keyboard does.
    let mut e = tool(Tool::Eraser);
    drag(&mut e, (10, 10), (60, 10), false);
    assert!(!e.objects()[0].hit(P::of_pixel((30, 10)), 4.0));
    e.set_tool(Some(Tool::Rectangle));
    drag(&mut e, (30, 10), (50, 30), false);
    assert_eq!(
        e.objects().len(),
        2,
        "the press drew, it did not pick the eraser"
    );
    e.set_tool(None);
    assert!(e.select_step(true));
    assert!(matches!(
        e.doc().get(e.selected().unwrap()).unwrap().shape,
        Shape::Effect(Effect::Erase, _)
    ));
    assert!(e.delete_selected());
    assert_eq!(e.objects().len(), 1);
}

#[test]
fn a_solid_redaction_keeps_nothing_of_any_picture_odd_sizes_and_alpha_included() {
    let l = Ledger::new();
    // Odd sizes, a negative origin; opaque, translucent and fully transparent pictures whose
    // transparent pixels still carry colour values.
    let r = Rect::new(-5, -3, 37, 23);
    let pictures = [
        opaque(&l, r),
        pattern(&l, r, |x, y| ((x * 17 + y * 29).rem_euclid(256)) as u8),
        pattern(&l, r, |_, _| 0),
    ];
    let box_ = obj(1, Effect::Redact, boxed(-2, 1, 19, 14), 0);
    let brush = Object {
        level: 1,
        ..obj(
            2,
            Effect::Redact,
            Area::Brush(vec![P::of_pixel((3, 16)), P::of_pixel((27, 9))]),
            0,
        )
    };
    for base in &pictures {
        let f = flatten(base, &[box_.clone(), brush.clone()], r).unwrap();
        let inside_box = |x: i32, y: i32| (-2..19).contains(&x) && (1..14).contains(&y);
        let mut brushed = 0;
        for y in r.y..r.y + r.height as i32 {
            for x in r.x..r.x + r.width as i32 {
                let (out, was) = (px(&f, x, y), px(base, x, y));
                if inside_box(x, y) {
                    assert_eq!(out, BLACK, "({x}, {y}) inside the box");
                } else if out == BLACK {
                    brushed += 1;
                } else {
                    // A brush redaction has no soft edge: a pixel is filled or untouched.
                    assert_eq!(out, was, "({x}, {y}) untouched");
                }
            }
        }
        assert!(brushed > 60, "the brush filled its stroke: {brushed}");
        // Next to the stroke's line every pixel is filled.
        assert_eq!(px(&f, 15, 12), BLACK);
        assert_eq!(px(&f, 26, 9), BLACK);
    }
}

#[test]
fn nothing_drawn_before_or_after_a_redaction_reveals_or_spreads_what_it_covers() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 96, 64);
    let a = opaque(&l, r);
    // `b` differs from `a` only under the redaction.
    let mut b = opaque(&l, r);
    for y in 20..40 {
        for x in 30..60 {
            let p = px(&b, x, y);
            set_px(&mut b, x, y, [255 - p[0], 255 - p[1], 255 - p[2], 255]);
        }
    }
    let redaction = obj(3, Effect::Redact, boxed(30, 20, 60, 40), 0);
    let objects = vec![
        // Before it: a blur and a mosaic reaching far beyond it (they read the redacted picture).
        obj(1, Effect::Blur, boxed(10, 5, 90, 60), 6),
        obj(2, Effect::Mosaic, boxed(25, 15, 70, 50), 4),
        redaction.clone(),
        // After it: a mosaic brush, an eraser through it, another blur, a pen stroke on top.
        obj(
            4,
            Effect::Mosaic,
            Area::Brush(vec![P::of_pixel((20, 30)), P::of_pixel((80, 30))]),
            3,
        ),
        obj(
            5,
            Effect::Erase,
            Area::Brush(vec![P::of_pixel((45, 10)), P::of_pixel((45, 55))]),
            2,
        ),
        obj(6, Effect::Blur, boxed(35, 25, 75, 45), 2),
        pen(7, &[(32, 22), (58, 38)], 2),
    ];
    let fa = flatten(&a, &objects, r).unwrap();
    let fb = flatten(&b, &objects, r).unwrap();
    assert!(
        fa.pixels() == fb.pixels(),
        "the output does not depend on what lies under the redaction, anywhere"
    );
    // The eraser brings back the redacted picture, not the original: under it, inside the
    // redaction, the fill (the later blur and pen stroke are elsewhere).
    let only = [redaction.clone(), objects[4].clone()];
    let f = flatten(&a, &only, r).unwrap();
    assert_eq!(px(&f, 45, 22), BLACK);
    assert_eq!(px(&f, 45, 50), px(&a, 45, 50), "outside it: the picture");
    // Its place in the drawing order changes nothing: sent to the back it still covers.
    let mut back = objects.clone();
    back.retain(|o| o.id != 3);
    back.insert(0, redaction);
    assert!(flatten(&a, &back, r).unwrap().pixels() == flatten(&b, &back, r).unwrap().pixels());
    // Control: without the redaction the two pictures do show.
    let none: Vec<Object> = objects.into_iter().filter(|o| o.id != 3).collect();
    assert!(flatten(&a, &none, r).unwrap().pixels() != flatten(&b, &none, r).unwrap().pixels());
}

#[test]
fn mosaic_cells_average_their_pixels_with_odd_sizes_alpha_edges_and_a_grid_that_moves_along() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 10, 7);
    let base = opaque(&l, r);
    // Cells of 4 from the box's corner; the right and bottom cells are partial.
    let m = obj(1, Effect::Mosaic, boxed(0, 0, 10, 7), 0);
    let f = flatten(&base, std::slice::from_ref(&m), r).unwrap();
    for (cx, cy) in [(0, 0), (4, 0), (8, 0), (0, 4), (4, 4), (8, 4)] {
        let (mut n, mut s) = (0u32, [0u32; 3]);
        for y in cy..(cy + 4).min(7) {
            for x in cx..(cx + 4).min(10) {
                let p = px(&base, x, y);
                n += 1;
                for k in 0..3 {
                    s[k] += u32::from(p[k]);
                }
            }
        }
        let want = [
            ((s[0] + n / 2) / n) as u8,
            ((s[1] + n / 2) / n) as u8,
            ((s[2] + n / 2) / n) as u8,
            255,
        ];
        for y in cy..(cy + 4).min(7) {
            for x in cx..(cx + 4).min(10) {
                assert_eq!(px(&f, x, y), want, "cell ({cx}, {cy}) at ({x}, {y})");
            }
        }
    }
    // Alpha edge: transparent pixels add no colour (their green never shows), only less alpha.
    let mut half = Frame::solid(&l, Rect::new(0, 0, 4, 4), [0, 255, 0, 0]).unwrap();
    for y in 0..4 {
        for x in 2..4 {
            set_px(&mut half, x, y, [0, 0, 200, 255]);
        }
    }
    let f = flatten(
        &half,
        &[obj(1, Effect::Mosaic, boxed(0, 0, 4, 4), 0)],
        half.rect(),
    )
    .unwrap();
    assert_eq!(
        px(&f, 0, 0),
        [0, 0, 200, 128],
        "red at half opacity, no green"
    );
    // The grid is the object's own: moving picture and object together moves the result along
    // (a pin shows the same cells as the overlay did).
    let wide = Rect::new(0, 0, 40, 30);
    let moved = Rect::new(5, 3, 40, 30);
    let p0 = opaque(&l, wide);
    let mut p1 = Frame::new(&l, moved, p0.pixels().to_vec()).unwrap();
    let brush = obj(
        1,
        Effect::Mosaic,
        Area::Brush(vec![P::of_pixel((3, 4)), P::of_pixel((30, 20))]),
        2,
    );
    let f0 = flatten(&p0, std::slice::from_ref(&brush), wide).unwrap();
    let f1 = flatten(&p1, &translate_all(&[brush], 5, 3), moved).unwrap();
    assert!(f0.pixels() == f1.pixels());
    assert!(f0.pixels() != p0.pixels(), "the brush changed something");
    p1.pixels_mut()[0] = 1;
}

#[test]
fn blur_is_gaussian_like_reads_only_its_box_and_keeps_colours_at_alpha_edges() {
    // Three boxes whose variances add up to about σ² (σ = 3 … 60).
    let mut last = 0.0;
    for sigma in 3..=60 {
        let var: f32 = effects::box_radii(sigma as f32)
            .iter()
            .map(|r| (((2 * r + 1) * (2 * r + 1)) as f32 - 1.0) / 12.0)
            .sum();
        let want = (sigma * sigma) as f32;
        assert!(
            (var - want).abs() / want < 0.2,
            "σ {sigma}: variance {var} for {want}"
        );
        assert!(var >= last, "stronger blur for a larger σ");
        last = var;
    }
    let l = Ledger::new();
    // A flat picture stays exactly flat, up to its edges (no dark border from outside).
    let flat = Frame::solid(&l, Rect::new(0, 0, 31, 17), [40, 90, 200, 255]).unwrap();
    for level in [0, 3, 6] {
        let f = flatten(
            &flat,
            &[obj(1, Effect::Blur, boxed(0, 0, 31, 17), level)],
            flat.rect(),
        )
        .unwrap();
        assert!(f.pixels() == flat.pixels(), "level {level}");
    }
    // A busy picture gets smoother and stays within its range.
    let r = Rect::new(0, 0, 41, 29);
    let busy = opaque(&l, r);
    let f = flatten(&busy, &[obj(1, Effect::Blur, boxed(0, 0, 41, 29), 2)], r).unwrap();
    let rough = |f: &Frame| -> u64 {
        let mut s = 0;
        for y in 0..29 {
            for x in 0..40 {
                s += u64::from(px(f, x, y)[0].abs_diff(px(f, x + 1, y)[0]));
            }
        }
        s
    };
    assert!(
        rough(&f) * 3 < rough(&busy),
        "{} vs {}",
        rough(&f),
        rough(&busy)
    );
    // It reads only its own box: a bright strip just outside changes nothing inside.
    let mut striped = opaque(&l, r);
    for y in 0..29 {
        set_px(&mut striped, 30, y, [255, 255, 255, 255]);
    }
    let blur = [obj(1, Effect::Blur, boxed(5, 5, 30, 25), 5)];
    let a = flatten(&busy, &blur, r).unwrap();
    let b = flatten(&striped, &blur, r).unwrap();
    for y in 5..25 {
        for x in 5..30 {
            assert_eq!(px(&a, x, y), px(&b, x, y), "({x}, {y})");
        }
    }
    // Alpha edge: opaque red next to transparent pixels carrying green; no green fringe.
    let mut half = Frame::solid(&l, Rect::new(0, 0, 20, 6), [0, 255, 0, 0]).unwrap();
    for y in 0..6 {
        for x in 0..10 {
            set_px(&mut half, x, y, [0, 0, 220, 255]);
        }
    }
    let f = flatten(
        &half,
        &[obj(1, Effect::Blur, boxed(0, 0, 20, 6), 2)],
        half.rect(),
    )
    .unwrap();
    for x in 0..20 {
        let p = px(&f, x, 3);
        // (8-bit premultiplication rounds the colour of nearly transparent pixels, below 16
        // of 255 alpha; nothing there is visible.)
        if p[3] >= 16 {
            assert!(p[1] <= 1 && p[0] <= 1 && p[2] >= 200, "x {x}: {p:?}");
        }
        assert!(p[1] <= 1, "never green: x {x}: {p:?}");
    }
    assert!(
        px(&f, 9, 3)[3] < 255 && px(&f, 10, 3)[3] > 0,
        "a soft alpha edge"
    );
}

#[test]
fn the_eraser_wipes_drawings_and_effects_back_to_the_picture_but_never_a_redaction() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 80, 40);
    let base = opaque(&l, r);
    let stroke = pen(1, &[(5, 20), (75, 20)], 6);
    let mosaic = obj(2, Effect::Mosaic, boxed(50, 0, 80, 40), 3);
    let redaction = obj(3, Effect::Redact, boxed(60, 25, 75, 35), 0);
    let eraser = obj(
        4,
        Effect::Erase,
        Area::Brush(vec![P::of_pixel((30, 2)), P::of_pixel((30, 38))]),
        2,
    );
    let box_eraser = obj(5, Effect::Erase, boxed(55, 5, 70, 30), 0);
    let later = pen(6, &[(28, 10), (32, 10)], 2);
    let f = flatten(
        &base,
        &[stroke, mosaic, redaction, eraser, box_eraser, later.clone()],
        r,
    )
    .unwrap();
    assert_eq!(px(&f, 30, 20), px(&base, 30, 20), "the pen wiped away");
    assert_ne!(px(&f, 10, 20), px(&base, 10, 20), "the pen elsewhere stays");
    assert_eq!(
        px(&f, 56, 6),
        px(&base, 56, 6),
        "the mosaic wiped by the box"
    );
    assert_ne!(
        px(&f, 77, 6),
        px(&base, 77, 6),
        "the mosaic elsewhere stays"
    );
    assert_eq!(px(&f, 65, 28), BLACK, "the redaction is never erased");
    let pen_only = flatten(&base, &[later], r).unwrap();
    assert_eq!(px(&f, 30, 10), px(&pen_only, 30, 10), "drawn after: on top");
}

/// Applies the editor's changed pixels to `layer` as the overlay does and checks that it equals a
/// fresh export (what the overlay shows is what an output holds).
fn check_refresh(e: &mut Editor, layer: &mut Frame, base: &Frame, what: &str) {
    if let Some(d) = e.take_dirty() {
        refresh(layer, base, e.objects(), d);
    }
    let fresh = flatten(base, e.objects(), base.rect()).unwrap();
    assert!(layer.pixels() == fresh.pixels(), "after {what}");
}

#[test]
fn refreshing_the_layer_after_every_edit_equals_a_fresh_export() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 120, 80);
    let base = opaque(&l, r);
    let mut layer = flatten(&base, &[], r).unwrap();
    let mut e = Editor::default();
    e.set_clip(r);
    e.set_tool(Some(Tool::Blur));
    drag(&mut e, (20, 20), (60, 50), false);
    check_refresh(&mut e, &mut layer, &base, "a blur");
    e.set_tool(Some(Tool::Pen));
    drag(&mut e, (40, 10), (90, 70), false);
    check_refresh(&mut e, &mut layer, &base, "a pen stroke into the blur");
    e.set_tool(Some(Tool::Mosaic));
    drag(&mut e, (5, 60), (100, 40), true);
    check_refresh(&mut e, &mut layer, &base, "a mosaic brush across both");
    // Move the blur by its edge, resize it, change its strength.
    e.set_tool(Some(Tool::Blur));
    assert!(e.press((20, 35), D100));
    e.drag((26, 31), false);
    e.release((26, 31), false);
    check_refresh(&mut e, &mut layer, &base, "moving the blur");
    let h = e.selected_handles();
    assert_eq!(h.len(), 8);
    let corner = (66, 46);
    assert!(e.press(corner, D100));
    e.drag((80, 70), false);
    e.release((80, 70), false);
    check_refresh(&mut e, &mut layer, &base, "resizing the blur");
    e.step_width(true);
    check_refresh(&mut e, &mut layer, &base, "a stronger blur");
    e.set_tool(Some(Tool::Redact));
    drag(&mut e, (50, 30), (70, 45), false);
    check_refresh(&mut e, &mut layer, &base, "a redaction over the blur");
    e.set_tool(Some(Tool::Eraser));
    drag(&mut e, (10, 40), (110, 40), false);
    check_refresh(&mut e, &mut layer, &base, "an eraser across everything");
    for i in 0..3 {
        e.undo();
        check_refresh(&mut e, &mut layer, &base, &format!("undo {i}"));
    }
    for i in 0..2 {
        e.redo();
        check_refresh(&mut e, &mut layer, &base, &format!("redo {i}"));
    }
    e.set_tool(None);
    e.select_step(true);
    e.delete_selected();
    check_refresh(&mut e, &mut layer, &base, "delete");
    e.select_step(true);
    e.reorder(Order::Front);
    check_refresh(&mut e, &mut layer, &base, "reorder");
    e.clear_all();
    check_refresh(&mut e, &mut layer, &base, "clear all");
    e.undo();
    check_refresh(&mut e, &mut layer, &base, "undo clear all");
    // Idle: nothing changed, nothing is processed.
    let passes = effect_passes();
    assert_eq!(e.take_dirty(), None);
    assert_eq!(effect_passes(), passes, "no image processing while idle");
}

#[test]
fn repeated_undo_and_redo_restore_the_exact_pixels() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 500, 60);
    let base = opaque(&l, r);
    let mut e = Editor::default();
    let mut shots = vec![flatten(&base, e.objects(), r).unwrap().pixels().to_vec()];
    let tools = [
        Tool::Mosaic,
        Tool::Blur,
        Tool::Redact,
        Tool::Eraser,
        Tool::Pen,
    ];
    for i in 0..12 {
        let t = tools[i % tools.len()];
        e.set_tool(Some(t));
        // Apart, so every press draws (a press on an object's edge would pick it).
        let x = 5 + 40 * i32::try_from(i).unwrap();
        drag(
            &mut e,
            (x, 5 + (x % 7)),
            (x + 20, 40 + (x % 11)),
            i % 3 == 0,
        );
        assert_eq!(e.doc().undo_steps(), i + 1, "one step per drag");
        if i % 4 == 1 {
            e.step_width(true);
        }
        shots.push(flatten(&base, e.objects(), r).unwrap().pixels().to_vec());
    }
    for round in 0..3 {
        for want in shots.iter().rev().skip(1) {
            assert!(e.undo());
            assert!(flatten(&base, e.objects(), r).unwrap().pixels() == want.as_slice());
        }
        assert!(!e.undo(), "round {round}: back at the picture");
        assert!(flatten(&base, e.objects(), r).unwrap().pixels() == base.pixels());
        for want in shots.iter().skip(1) {
            assert!(e.redo());
            assert!(flatten(&base, e.objects(), r).unwrap().pixels() == want.as_slice());
        }
    }
}

#[test]
fn effects_outside_the_picture_are_clipped_and_cancelling_a_drag_leaves_nothing() {
    let l = Ledger::new();
    let r = Rect::new(0, 0, 50, 40);
    let base = opaque(&l, r);
    // Drawing is clamped to the selection.
    let mut e = tool(Tool::Redact);
    e.set_clip(Rect::new(10, 10, 30, 20));
    drag(&mut e, (0, 0), (500, 500), false);
    assert_eq!(
        e.objects()[0].shape,
        Shape::Effect(
            Effect::Redact,
            Area::Box(Bounds::new(10.0, 10.0, 40.0, 30.0))
        )
    );
    // Objects moved partly off the picture are clipped to it; wholly off, they do nothing.
    let partly = [
        obj(1, Effect::Blur, boxed(-30, -20, 20, 15), 4),
        obj(2, Effect::Mosaic, boxed(40, 30, 90, 70), 1),
        obj(3, Effect::Redact, boxed(-10, 35, 5, 60), 0),
    ];
    let f = flatten(&base, &partly, r).unwrap();
    assert_eq!(px(&f, 2, 38), BLACK);
    assert_ne!(px(&f, 45, 35), px(&base, 45, 35));
    assert_eq!(px(&f, 30, 20), px(&base, 30, 20), "between them: untouched");
    let passes = effect_passes();
    let off = [obj(4, Effect::Blur, boxed(200, 200, 260, 240), 4)];
    assert!(flatten(&base, &off, r).unwrap().pixels() == base.pixels());
    assert_eq!(
        effect_passes(),
        passes,
        "an effect off the picture costs nothing"
    );
    // A one-pixel picture and a one-pixel box.
    let dot = opaque(&l, Rect::new(3, 3, 1, 1));
    for o in [
        obj(1, Effect::Mosaic, boxed(0, 0, 9, 9), 6),
        obj(1, Effect::Blur, boxed(3, 3, 4, 4), 6),
    ] {
        let f = flatten(&dot, &[o], dot.rect()).unwrap();
        assert_eq!(px(&f, 3, 3), px(&dot, 3, 3));
    }
    // Escape while drawing a redaction aborts it: no object, no step.
    let mut e = tool(Tool::Redact);
    assert!(e.press((5, 5), D100));
    e.drag((30, 30), false);
    assert!(e.done());
    assert!(e.objects().is_empty());
    assert_eq!(e.doc().undo_steps(), 0);
    e.press((5, 5), D100);
    e.drag((30, 30), false);
    e.abort();
    assert!(e.objects().is_empty());
}

#[test]
fn a_large_blur_and_mosaic_stay_exact_on_a_flat_picture() {
    // A 1280 × 720 picture at the strongest levels (the measurement example times 4K in a
    // release build; here only correctness on a large area).
    let l = Ledger::new();
    let r = Rect::new(0, 0, 1280, 720);
    let flat = Frame::solid(&l, r, [12, 34, 56, 255]).unwrap();
    let objects = [
        obj(1, Effect::Blur, boxed(0, 0, 1280, 720), 6),
        obj(2, Effect::Mosaic, boxed(100, 100, 1181, 621), 6),
    ];
    let f = flatten(&flat, &objects, r).unwrap();
    assert!(f.pixels() == flat.pixels());
}
