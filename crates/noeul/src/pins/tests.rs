//! Tests of the pin engine's model (Steps 53, 56 and 54).

use super::fake::FakeWindows;
use super::*;
use crate::capture::{DesktopState, Monitor};
use crate::frame::Ledger;

fn frame(ledger: &std::sync::Arc<Ledger>, x: i32) -> Frame {
    Frame::solid(ledger, Rect::new(x, 0, 2, 2), [0, 0, 0, 255]).unwrap()
}

fn exact(frame: Frame, source: Source, focus: bool) -> NewPin {
    NewPin {
        frame,
        source,
        placement: Placement::Exact,
        focus,
        guard: None,
        annotations: Vec::new(),
    }
}

/// A 1920×1080 primary with a 40-pixel taskbar at the bottom, and a 1280×1024 monitor left
/// of it whose taskbar is at the top.
fn desk() -> Desktop {
    Desktop {
        state: DesktopState::Available,
        virtual_rect: Some(Rect::new(-1280, 0, 3200, 1080)),
        monitors: vec![
            Monitor::new(Rect::new(0, 0, 1920, 1080), true).with_work(Rect::new(0, 0, 1920, 1040)),
            Monitor::new(Rect::new(-1280, 0, 1280, 1024), false)
                .with_work(Rect::new(-1280, 40, 1280, 984)),
        ],
    }
}

#[test]
fn ids_parse_and_print() {
    assert_eq!(PinId::parse("pin-12"), Some(PinId(12)));
    for bad in ["pin-", "pin-0", "pin-01", "p-1", "pin-1x", "pin--1"] {
        assert_eq!(PinId::parse(bad), None, "{bad}");
    }
    assert_eq!(PinId(7).to_string(), "pin-7");
}

#[test]
fn lifecycle_close_reopen_destroy_and_quit() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let a = pins
        .create(&mut w, &d, exact(frame(&ledger, 0), Source::Capture, true))
        .unwrap();
    let b = pins
        .create(
            &mut w,
            &d,
            exact(frame(&ledger, 5), Source::Clipboard, false),
        )
        .unwrap();
    assert_eq!((a, b), (PinId(1), PinId(2)));
    assert_eq!(ledger.live(), (2, 32));
    assert!(w.log.borrow().created[0].2, "focus only when asked");
    assert!(!w.log.borrow().created[1].2);

    pins.close(a).unwrap();
    assert_eq!(w.log.borrow().destroyed, [a]);
    assert_eq!(pins.reopenable(), Some(a));
    assert_eq!(
        ledger.live(),
        (2, 32),
        "a closed pin keeps its picture for reopen"
    );
    assert_eq!(pins.close(a), Err(PinError::NotFound(a)));

    assert_eq!(
        pins.reopen(&mut w, false).unwrap(),
        a,
        "same id after reopen"
    );
    assert_eq!(pins.reopen(&mut w, false), Err(PinError::NothingToReopen));
    assert_eq!(pins.list(&d)["pins"].as_array().unwrap().len(), 2);

    pins.destroy(b).unwrap();
    assert_eq!(ledger.live(), (1, 16), "destroy frees the picture");
    assert_eq!(pins.reopenable(), None);

    let c = pins
        .create(&mut w, &d, exact(frame(&ledger, 9), Source::Capture, false))
        .unwrap();
    assert_eq!(c, PinId(3), "ids are never reused");
    pins.close(c).unwrap();
    pins.close(a).unwrap();
    assert_eq!(
        pins.reopenable(),
        Some(a),
        "the memory keeps the last closed pin"
    );
    assert_eq!(ledger.live(), (1, 16), "the older closed picture was freed");

    let e = pins
        .create(&mut w, &d, exact(frame(&ledger, 1), Source::Capture, false))
        .unwrap();
    assert_eq!(pins.clear(), 1);
    assert!(
        w.log.borrow().alive.is_empty(),
        "quit destroys every window"
    );
    assert_eq!(ledger.live(), (0, 0), "quit releases every picture");
    assert!(!pins.contains(e));
}

#[test]
fn failed_windows_hand_the_picture_back_and_moves_are_tracked() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    *w.fail.borrow_mut() = true;
    let (back, why) = pins
        .create(&mut w, &d, exact(frame(&ledger, 0), Source::Capture, true))
        .unwrap_err();
    assert!(why.contains("failed"));
    assert_eq!(
        back.rect(),
        Rect::new(0, 0, 2, 2),
        "nothing is lost (NOEUL-FR-025)"
    );
    *w.fail.borrow_mut() = false;
    let id = pins
        .create(&mut w, &d, exact(back, Source::Capture, true))
        .unwrap();
    assert_eq!(id, PinId(1), "a failed create uses no id");
    assert!(pins.nudge(id, -1, 0));
    assert!(pins.nudge(id, 0, 1));
    assert_eq!(pins.rect(id), Some(Rect::new(-1, 1, 2, 2)));
    assert_eq!(w.log.borrow().moved, [(id, -1, 0), (id, -1, 1)]);
    assert!(pins.moved(id, 300, 200));
    assert_eq!(pins.rect(id), Some(Rect::new(300, 200, 2, 2)));
    assert!(!pins.nudge(PinId(99), 1, 1));
    assert_eq!(ledger.live(), (1, 16), "moving copies nothing");
    let listed = pins.list(&d);
    assert_eq!(listed["pins"][0]["id"], "pin-1");
    assert_eq!(listed["pins"][0]["rect"]["x"], 300);
    assert_eq!(listed["pins"][0]["zoom"], 100);
    assert_eq!(listed["pins"][0]["on_screen"], true);
}

#[test]
fn many_pins_move_zoom_hide_and_close_independently() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let ids: Vec<PinId> = (0..5)
        .map(|i| {
            let f = Frame::solid(&ledger, Rect::new(i * 100, 100, 40, 30), [9, 9, 9, 255]).unwrap();
            pins.create(&mut w, &d, exact(f, Source::Capture, false))
                .unwrap()
        })
        .collect();
    assert_eq!(pins.len(), 5);
    // Move the second, zoom the third around a point inside it, hide the fourth.
    assert!(pins.nudge(ids[1], 5, -3));
    assert!(pins.set_zoom(ids[2], 200, (210, 110)).unwrap());
    assert_eq!(
        pins.rect(ids[2]),
        Some(Rect::new(190, 90, 80, 60)),
        "zoomed around (210,110)"
    );
    assert!(pins.hide(ids[3]).unwrap());
    assert!(!pins.hide(ids[3]).unwrap(), "hiding twice changes nothing");
    assert_eq!(pins.hidden(), 1);
    // Close the first: every other window stays exactly as it was.
    pins.close(ids[0]).unwrap();
    assert_eq!(w.log.borrow().destroyed, [ids[0]]);
    assert_eq!(pins.rect(ids[1]), Some(Rect::new(105, 97, 40, 30)));
    assert_eq!(pins.info(ids[2]).unwrap().zoom, 200);
    assert!(!pins.info(ids[3]).unwrap().visible);
    assert_eq!(pins.rect(ids[4]), Some(Rect::new(400, 100, 40, 30)));
    assert_eq!(
        w.log.borrow().alive,
        ids[1..].to_vec(),
        "only the closed pin's window is gone"
    );
    assert!(pins.show(ids[3]).unwrap());
    assert_eq!(
        w.log.borrow().visible,
        [(ids[3], false), (ids[3], true)],
        "only the hidden pin's window was touched"
    );
    assert!(w.log.borrow().focused.is_empty(), "nothing took focus");
    // Zooming back to 100 % keeps the picture's pixels (the frame is never resampled).
    assert!(pins.set_zoom(ids[2], 100, (190, 90)).unwrap());
    assert_eq!(pins.rect(ids[2]), Some(Rect::new(190, 90, 40, 30)));
    assert!(!pins.set_zoom(ids[2], 100, (0, 0)).unwrap(), "no change");
    assert_eq!(
        ledger.live(),
        (5, 5 * 40 * 30 * 4),
        "the closed one is kept for reopen"
    );
    // A failed window update keeps the old zoom.
    *w.fail.borrow_mut() = true;
    assert!(matches!(
        pins.set_zoom(ids[4], 150, (0, 0)),
        Err(PinError::Window(_))
    ));
    assert_eq!(pins.info(ids[4]).unwrap().zoom, 100);
    assert_eq!(pins.hide(PinId(42)), Err(PinError::NotFound(PinId(42))));
}

#[test]
fn a_huge_paste_fits_its_monitor_and_avoids_the_taskbar() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    // A 4K picture pasted near the bottom right of the 1080p primary.
    let big = Frame::solid(&ledger, Rect::new(0, 0, 3840, 2160), [1, 2, 3, 255]).unwrap();
    let id = pins
        .create(
            &mut w,
            &d,
            NewPin {
                frame: big,
                source: Source::Clipboard,
                placement: Placement::At(1900, 1000),
                focus: true,
                guard: None,
                annotations: Vec::new(),
            },
        )
        .unwrap();
    let info = pins.info(id).unwrap();
    assert_eq!(info.zoom, 48, "zoomed out to fit the work area");
    assert_eq!(info.image, (3840, 2160), "the picture keeps every pixel");
    let r = info.rect;
    assert!(
        r.right() <= 1920 && r.bottom() <= 1040,
        "{r:?} inside the work area"
    );
    assert!(r.x >= 0 && r.y >= 0);
    assert_eq!(w.log.borrow().created[0].1, r, "the window is that size");
    // A small one near the left monitor's top lands below its top taskbar.
    let small = Frame::solid(&ledger, Rect::new(0, 0, 50, 50), [1, 2, 3, 255]).unwrap();
    let id2 = pins
        .create(
            &mut w,
            &d,
            NewPin {
                frame: small,
                source: Source::Clipboard,
                placement: Placement::At(-600, 5),
                focus: false,
                guard: None,
                annotations: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(pins.rect(id2), Some(Rect::new(-600, 40, 50, 50)));
    // A point off every monitor goes to the nearest one.
    assert_eq!(place_at(&d, 10, 10, (5000, 500)), (1910, 500, 100));
    // Without monitors the point is kept (nothing to fit).
    let none = Desktop {
        state: DesktopState::NoDisplay,
        virtual_rect: None,
        monitors: Vec::new(),
    };
    assert_eq!(place_at(&none, 10, 10, (5, 6)), (5, 6, 100));
}

#[test]
fn a_monitor_disconnect_keeps_pins_in_place_until_they_are_brought_back() {
    let ledger = Ledger::new();
    let mut d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let on_left = pins
        .create(
            &mut w,
            &d,
            exact(
                Frame::solid(&ledger, Rect::new(-1000, 300, 800, 600), [0; 4]).unwrap(),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let on_primary = pins
        .create(
            &mut w,
            &d,
            exact(
                Frame::solid(&ledger, Rect::new(10, 10, 20, 20), [0; 4]).unwrap(),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    assert_eq!(pins.off_screen(&d), 0);
    // The left monitor is unplugged.
    d.monitors.truncate(1);
    d.virtual_rect = Some(Rect::new(0, 0, 1920, 1080));
    assert_eq!(pins.sync(), 0, "nothing moved by itself");
    assert_eq!(pins.off_screen(&d), 1);
    let l = pins.list(&d);
    assert_eq!(l["pins"][0]["on_screen"], false);
    assert_eq!(l["pins"][1]["on_screen"], true);
    assert_eq!(
        pins.rect(on_left),
        Some(Rect::new(-1000, 300, 800, 600)),
        "it keeps its place (NOEUL-FR-067)"
    );
    // The system moved the other window: the model follows.
    w.log.borrow_mut().rects.get_mut(&on_primary).unwrap().x = 50;
    assert_eq!(pins.sync(), 1);
    assert_eq!(pins.rect(on_primary).unwrap().x, 50);
    // "Bring pins on screen": only the lost one moves, into the work area.
    assert!(pins.bring_on_screen(on_left, &d).unwrap());
    assert!(!pins.bring_on_screen(on_primary, &d).unwrap());
    assert_eq!(pins.rect(on_left), Some(Rect::new(0, 300, 800, 600)));
    assert_eq!(pins.off_screen(&d), 0);
    // A larger pin brought onto a smaller monitor is zoomed out to fit.
    let big = pins
        .create(
            &mut w,
            &d,
            exact(
                Frame::solid(&ledger, Rect::new(-4000, 0, 2560, 1440), [0; 4]).unwrap(),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    assert!(pins.bring_on_screen(big, &d).unwrap());
    let info = pins.info(big).unwrap();
    assert!(info.zoom < 100 && info.rect.width <= 1920 && info.rect.height <= 1040);
    assert_eq!((info.rect.x, info.rect.y), (0, 0));
    // While the desktop is locked nobody knows what is on screen.
    d.state = DesktopState::Locked;
    assert_eq!(pins.list(&d)["pins"][0]["on_screen"], Value::Null);
}

#[test]
fn on_screen_needs_a_grabbable_part() {
    let d = desk();
    assert!(
        on_screen(&d, Rect::new(1890, 500, 100, 100)),
        "30 px wide on screen"
    );
    assert!(!on_screen(&d, Rect::new(1910, 500, 100, 100)), "only 10 px");
    assert!(
        on_screen(&d, Rect::new(1915, 500, 5, 5)),
        "a tiny pin is whole"
    );
    assert!(!on_screen(&d, Rect::new(-5000, 0, 100, 100)));
}

/// Step 56: a pin keeps its picture and its annotations apart; its window shows them drawn
/// over a transparent picture without making it opaque; a commit crops where it was shown.
#[test]
fn annotations_over_a_transparent_picture_and_crop_commits() {
    use crate::annotation::{Bounds, Object, PALETTE, Shape};
    let ledger = Ledger::new();
    let mut w = FakeWindows::default();
    let d = desk();
    let mut pins = Pins::new();
    let red = PALETTE[0].1;
    let outline = Object {
        id: 1,
        shape: Shape::Rectangle(Bounds::new(0.0, 0.0, 10.0, 10.0)),
        color: red,
        level: 0,
        dpi: 96,
    };
    let frame = Frame::solid(&ledger, Rect::new(100, 100, 40, 30), [0, 0, 0, 0]).unwrap();
    let mut new = exact(frame, Source::Clipboard, false);
    new.annotations = vec![outline.clone()];
    let id = pins.create(&mut w, &d, new).unwrap();
    let px = |f: &Frame, x: i32, y: i32| {
        let r = f.rect();
        let i = (((y - r.y) as u32 * r.width + (x - r.x) as u32) * 4) as usize;
        [
            f.pixels()[i],
            f.pixels()[i + 1],
            f.pixels()[i + 2],
            f.pixels()[i + 3],
        ]
    };
    let shown = pins.rendered(id).unwrap();
    assert_eq!(px(&shown, 100, 100), [red.b, red.g, red.r, 255]);
    assert_eq!(
        px(&shown, 105, 105),
        [0, 0, 0, 0],
        "transparent inside the outline"
    );
    assert_eq!(px(&shown, 130, 120), [0, 0, 0, 0]);
    drop(shown);
    assert!(
        pins.picture(id).unwrap().pixels().iter().all(|b| *b == 0),
        "unchanged"
    );
    assert_eq!(pins.info(id).unwrap().annotations, 1);
    // Editing hides the window and shows it again; nothing else changes.
    pins.set_editing(id, true).unwrap();
    assert!(pins.info(id).unwrap().editing);
    pins.set_editing(id, false).unwrap();
    assert_eq!(w.log.borrow().visible.as_slice(), [(id, false), (id, true)]);
    // Commit a crop that keeps x 105..140: the pin moves to where that part was shown.
    let screen = annotation::translate_all(&[outline], 100, 100);
    pins.commit_edit(id, &screen, Rect::new(105, 100, 35, 30))
        .unwrap();
    let info = pins.info(id).unwrap();
    assert_eq!(
        (info.rect, info.image),
        (Rect::new(105, 100, 35, 30), (35, 30))
    );
    assert_eq!(
        pins.annotations(id).unwrap().len(),
        1,
        "still shows in the kept part"
    );
    assert_eq!(
        w.log.borrow().presented.last(),
        Some(&(id, Rect::new(105, 100, 35, 30)))
    );
    // At 200 % the kept part goes where it was shown at that zoom.
    pins.set_zoom(id, 200, (105, 100)).unwrap();
    pins.commit_edit(id, &[], Rect::new(115, 110, 25, 20))
        .unwrap();
    let info = pins.info(id).unwrap();
    assert_eq!(info.rect, Rect::new(125, 120, 50, 40));
    assert_eq!(info.annotations, 0);
    assert!(
        pins.commit_edit(PinId(99), &[], Rect::new(0, 0, 1, 1))
            .is_err()
    );
    // Closing and reopening keeps annotations.
    let mut new = exact(
        Frame::solid(&ledger, Rect::new(0, 0, 20, 20), [9, 9, 9, 255]).unwrap(),
        Source::Capture,
        false,
    );
    new.annotations = annotation::translate_all(&screen, -100, -100);
    let two = pins.create(&mut w, &d, new).unwrap();
    pins.close(two).unwrap();
    pins.reopen(&mut w, false).unwrap();
    assert_eq!(pins.annotations(two).unwrap().len(), 1);
    assert_eq!(pins.list(&d)["pins"][1]["annotations"], 1);
}

// ---- Step 54: transforms, thumbnails, click-through, groups (ADR 0044) ----

/// A picture whose pixels all differ, at `r`.
fn pattern(ledger: &std::sync::Arc<Ledger>, r: Rect) -> Frame {
    let mut px = Vec::new();
    for y in 0..r.height {
        for x in 0..r.width {
            px.extend_from_slice(&[(x * 7 + y * 13) as u8, (x * 3) as u8, (y * 5) as u8, 255]);
        }
    }
    Frame::new(ledger, r, px).unwrap()
}

fn pixel(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width() + x) * 4) as usize;
    f.pixels()[i..i + 4].try_into().unwrap()
}

type Op = fn(Orientation) -> Orientation;

/// Every transform of one pin is undone by its inverse — position, size and the very pixels it
/// shows — while the other pins' windows are never touched (NOEUL-FR-062).
#[test]
fn every_transform_is_reversible_and_touches_only_its_pin() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let a = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(100, 100, 40, 20)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let b = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(400, 100, 30, 30)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let before = pins.list(&d);
    let start = pins.rect(a).unwrap();
    assert!(
        pins.rendered(a).is_none(),
        "an untransformed pin shows its picture itself"
    );
    let touched = |w: &FakeWindows| {
        let l = w.log.borrow();
        l.views.iter().filter(|(i, _)| *i == b).count()
            + l.opacity.iter().filter(|(i, _)| *i == b).count()
            + l.visible.iter().filter(|(i, _)| *i == b).count()
            + l.moved.iter().filter(|(i, _, _)| *i == b).count()
    };
    let b_before = touched(&w);

    // Rotate right: the shown picture is 20×40, around the same centre.
    assert!(pins.orient(a, Orientation::rotate_cw).unwrap());
    let r = pins.rect(a).unwrap();
    assert_eq!((r.width, r.height), (20, 40));
    assert_eq!(
        (r.x + 10, r.y + 20),
        (start.x + 20, start.y + 10),
        "the centre stays"
    );
    assert_eq!(
        w.log.borrow().sizes.last(),
        Some(&(a, 20, 40)),
        "the window got the turned picture"
    );
    let turned = pins
        .rendered(a)
        .expect("copy and save output what is shown");
    assert_eq!((turned.width(), turned.height()), (20, 40));
    assert_eq!(pixel(&turned, 19, 0), pixel(pins.picture(a).unwrap(), 0, 0));
    assert_eq!(
        pins.picture(a).unwrap().width(),
        40,
        "the picture itself is untouched"
    );
    drop(turned);
    // Each inverse brings back the same place, size and pixels.
    assert!(pins.orient(a, Orientation::rotate_ccw).unwrap());
    assert_eq!(pins.rect(a), Some(start));
    let pairs: [(Op, Op); 3] = [
        (Orientation::flip_horizontal, Orientation::flip_horizontal),
        (Orientation::flip_vertical, Orientation::flip_vertical),
        (Orientation::rotate_ccw, Orientation::rotate_cw),
    ];
    for (there, back) in pairs {
        assert!(pins.orient(a, there).unwrap());
        assert!(pins.rendered(a).is_some());
        assert!(pins.orient(a, back).unwrap());
        assert_eq!(pins.rect(a), Some(start));
        assert!(pins.rendered(a).is_none(), "back to the picture itself");
    }
    // Opacity: no rendering, only the constant alpha; never below 10 %.
    let presents = w.log.borrow().presented.len();
    assert!(pins.set_opacity(a, 50).unwrap());
    assert!(pins.set_opacity(a, 3).unwrap());
    assert_eq!(pins.info(a).unwrap().transforms.opacity, 10);
    assert!(!pins.set_opacity(a, 10).unwrap(), "unchanged");
    assert!(pins.set_opacity(a, 100).unwrap());
    assert_eq!(w.log.borrow().opacity, [(a, 50), (a, 10), (a, 100)]);
    assert_eq!(
        w.log.borrow().presented.len(),
        presents,
        "opacity renders nothing again"
    );
    // Zoom in and back, click-through on and off, thumbnail on and off.
    assert!(pins.set_zoom(a, 200, (start.x, start.y)).unwrap());
    assert!(pins.set_zoom(a, 100, (start.x, start.y)).unwrap());
    assert!(pins.set_click_through(a, true).unwrap());
    assert!(w.log.borrow().views.last().unwrap().1.click_through);
    assert_eq!(pins.click_through(), 1);
    assert!(pins.set_click_through(a, false).unwrap());
    assert!(pins.set_thumbnail(a, true).unwrap());
    assert_eq!(pins.thumbnails(), 1);
    assert!(pins.set_thumbnail(a, false).unwrap());
    let mut after = pins.list(&d);
    let mut was = before;
    for l in [&mut after, &mut was] {
        l["pins"][0]["created_at"] = Value::Null;
    }
    assert_eq!(after, was, "every transform undone: the same pin as before");
    assert_eq!(
        touched(&w),
        b_before,
        "the other pin's window was never touched"
    );
    assert_eq!(ledger.live().0, 2, "no shown copy is kept");
}

/// Annotations and redactions are drawn on the picture before it is turned: the shown and
/// exported pixels under a redaction are its fill wherever the pin is turned, and turning back
/// shows exactly the unturned output (ADR 0041, ADR 0044).
#[test]
fn transforms_compose_with_annotations_and_redactions() {
    use crate::annotation::{Area, Bounds, Effect, PALETTE, REDACT_COLOR, Shape};
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let mut new = exact(
        pattern(&ledger, Rect::new(0, 0, 8, 6)),
        Source::Capture,
        false,
    );
    // A redaction over the picture's top-left 3×2 pixels (image pixels).
    new.annotations = vec![Object {
        id: 1,
        shape: Shape::Effect(Effect::Redact, Area::Box(Bounds::new(0.0, 0.0, 3.0, 2.0))),
        color: PALETTE[REDACT_COLOR].1,
        level: 0,
        dpi: 96,
    }];
    let id = pins.create(&mut w, &d, new).unwrap();
    let flat = pins.rendered(id).unwrap();
    let fill = pixel(&flat, 0, 0);
    assert_ne!(fill, pixel(pins.picture(id).unwrap(), 0, 0), "redacted");
    assert_eq!(pixel(&flat, 1, 1), fill);
    for o in [
        Orientation::IDENTITY.rotate_cw(),
        Orientation::IDENTITY.rotate_cw().rotate_cw(),
        Orientation::IDENTITY.rotate_ccw(),
        Orientation::IDENTITY.flip_horizontal(),
        Orientation::IDENTITY.flip_vertical(),
        Orientation::IDENTITY.rotate_cw().flip_horizontal(),
    ] {
        pins.orient(id, |_| o).unwrap();
        let out = pins.rendered(id).unwrap();
        for y in 0..6 {
            for x in 0..8 {
                let (sx, sy) = o.map(x, y, 8, 6);
                assert_eq!(pixel(&out, sx, sy), pixel(&flat, x, y), "{o:?} ({x},{y})");
                if x < 3 && y < 2 {
                    assert_eq!(
                        pixel(&out, sx, sy),
                        fill,
                        "the fill and nothing of the original"
                    );
                }
            }
        }
    }
    pins.orient(id, |_| Orientation::IDENTITY).unwrap();
    assert_eq!(
        pins.rendered(id).unwrap().pixels(),
        flat.pixels(),
        "turned back exactly"
    );
    assert_eq!(pins.annotations(id).unwrap().len(), 1, "still editable");
}

/// An edit's crop on a turned, zoomed pin keeps the kept part where it was shown.
#[test]
fn a_crop_of_a_turned_pin_stays_where_it_was_shown() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let id = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(100, 100, 40, 20)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    pins.orient(id, Orientation::rotate_cw).unwrap();
    let r = pins.rect(id).unwrap();
    pins.set_zoom(id, 200, (r.x, r.y)).unwrap();
    let r = pins.rect(id).unwrap();
    assert_eq!((r.width, r.height), (40, 80));
    // Keep the picture's right half (image x 20..40): turned right it is the shown bottom half.
    let p = pins.picture(id).unwrap().rect();
    pins.commit_edit(id, &[], Rect::new(p.x + 20, p.y, 20, 20))
        .unwrap();
    let after = pins.rect(id).unwrap();
    assert_eq!(
        after,
        Rect::new(r.x, r.y + 40, 40, 40),
        "the bottom half, where it was"
    );
    assert_eq!(pins.info(id).unwrap().transforms.orientation.rotation(), 90);
}

/// Thumbnails, zoom limits and border-drag resizing (NOEUL-FR-061, NOEUL-FR-062).
#[test]
fn thumbnails_extreme_zoom_and_border_resizing() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let id = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(10, 10, 400, 200)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    pins.set_zoom(id, 150, (10, 10)).unwrap();
    assert!(pins.set_thumbnail(id, true).unwrap());
    let t = pins.info(id).unwrap();
    assert_eq!(
        t.rect,
        Rect::new(10, 10, 96, 48),
        "the whole picture in 96 logical pixels"
    );
    assert_eq!(t.zoom, 150, "the zoom waits");
    assert!(
        !pins.view(id).unwrap().resizable,
        "a thumbnail is not resized by its border"
    );
    assert!(
        !pins.set_zoom(id, 300, (10, 10)).unwrap(),
        "zoom keys wait while it is a thumbnail"
    );
    assert!(!pins.resize(id, Edge::Right, t.rect, (50, 0)).unwrap());
    assert!(pins.orient(id, Orientation::rotate_cw).unwrap());
    let turned = pins.rect(id).unwrap();
    assert_eq!(
        (turned.width, turned.height),
        (48, 96),
        "a turned thumbnail"
    );
    pins.orient(id, Orientation::rotate_ccw).unwrap();
    assert!(pins.set_thumbnail(id, false).unwrap());
    assert_eq!(
        pins.rect(id),
        Some(Rect::new(10, 10, 600, 300)),
        "back at 150 %"
    );
    // Dragging the right border by 200 px: 800×400, 200 %, the left side stays.
    let start = pins.rect(id).unwrap();
    assert!(pins.resize(id, Edge::Right, start, (200, 0)).unwrap());
    assert_eq!(pins.info(id).unwrap().zoom, 200);
    assert_eq!(pins.rect(id), Some(Rect::new(10, 10, 800, 400)));
    // Further along the same drag: from the same start, a smaller delta shrinks it back.
    assert!(pins.resize(id, Edge::Right, start, (-300, 0)).unwrap());
    assert_eq!(pins.rect(id), Some(Rect::new(10, 10, 300, 150)));
    // The top-left corner keeps the bottom-right one.
    let s = pins.rect(id).unwrap();
    pins.resize(id, Edge::TopLeft, s, (-100, -10)).unwrap();
    let r = pins.rect(id).unwrap();
    assert_eq!((r.right(), r.bottom()), (s.right(), s.bottom()));
    assert_eq!(r.width / r.height, 2, "the aspect ratio is kept");
    // Extreme zoom: 800 % of a small picture, never beyond; a 4K picture stops at the surface
    // limits (and keeps its last good zoom).
    let small = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(0, 0, 10, 10)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    assert!(pins.set_zoom(small, 5000, (0, 0)).unwrap());
    assert_eq!(pins.info(small).unwrap().zoom, zoom::MAX);
    assert!(pins.set_zoom(small, 1, (0, 0)).unwrap());
    assert_eq!(
        pins.rect(small),
        Some(Rect::new(0, 0, 1, 1)),
        "10 %: never empty"
    );
    let big = pins
        .create(
            &mut w,
            &d,
            exact(
                Frame::solid(&ledger, Rect::new(0, 0, 3840, 2160), [1, 2, 3, 255]).unwrap(),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    assert!(
        !pins.set_zoom(big, 800, (0, 0)).unwrap(),
        "a 4K window at 800 % is refused"
    );
    assert_eq!(pins.info(big).unwrap().zoom, 100);
    let r = pins.rect(big).unwrap();
    pins.resize(big, Edge::BottomRight, r, (30_000, 30_000))
        .unwrap();
    let r = pins.rect(big).unwrap();
    assert!(zoom::allowed(r.width, r.height), "{r:?}");
}

/// A window that refuses an update leaves the pin exactly as it was.
#[test]
fn a_failed_window_update_keeps_every_transform() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let id = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(0, 0, 30, 10)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let before = pins.info(id).unwrap();
    *w.fail.borrow_mut() = true;
    assert!(matches!(
        pins.orient(id, Orientation::rotate_cw),
        Err(PinError::Window(_))
    ));
    assert!(pins.set_thumbnail(id, true).is_err());
    assert!(pins.set_click_through(id, true).is_err());
    assert!(pins.resize(id, Edge::Right, before.rect, (30, 0)).is_err());
    assert_eq!(pins.info(id).unwrap(), before);
    assert_eq!(
        pins.set_look(Look {
            border: false,
            shadow: true
        }),
        1,
        "reported, kept"
    );
    *w.fail.borrow_mut() = false;
    assert_eq!(
        pins.orient(PinId(9), Orientation::rotate_cw),
        Err(PinError::NotFound(PinId(9)))
    );
}

/// High DPI on synthetic layouts: the outline, shadow and badge follow the monitor's DPI, the
/// picture keeps one image pixel per device pixel, and a thumbnail scales with the monitor.
#[test]
fn high_dpi_layouts_scale_the_look_never_the_picture() {
    let ledger = Ledger::new();
    let d = Desktop {
        state: DesktopState::Available,
        virtual_rect: Some(Rect::new(0, 0, 3840, 1080)),
        monitors: vec![
            Monitor::new(Rect::new(0, 0, 1920, 1080), true),
            Monitor::new(Rect::new(1920, 0, 1920, 1080), false).with_dpi(144),
        ],
    };
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let a = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(100, 100, 500, 400)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let b = pins
        .create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(2000, 100, 500, 400)),
                Source::Capture,
                false,
            ),
        )
        .unwrap();
    let (va, vb) = (pins.view(a).unwrap(), pins.view(b).unwrap());
    assert_eq!((va.dpi, vb.dpi), (96, 144));
    assert_eq!(
        (va.rect.width, vb.rect.width),
        (500, 500),
        "100 % is one image pixel per device pixel"
    );
    assert!(
        vb.margin() > va.margin(),
        "{} > {}",
        vb.margin(),
        va.margin()
    );
    assert_eq!(
        vb.band().1,
        6,
        "the grab band scales: 4 logical pixels at 150 %"
    );
    pins.set_thumbnail(a, true).unwrap();
    pins.set_thumbnail(b, true).unwrap();
    assert_eq!(pins.rect(a).unwrap().width, 96);
    assert_eq!(
        pins.rect(b).unwrap().width,
        144,
        "96 logical pixels at 150 %"
    );
    // Dragged onto the 150 % monitor: the look follows, the picture's pixel size does not.
    pins.set_thumbnail(a, false).unwrap();
    assert!(pins.moved(a, 2500, 300));
    assert!(pins.set_dpi(a, dpi_at(&d, pins.rect(a).unwrap())).unwrap());
    assert_eq!(pins.view(a).unwrap().dpi, 144);
    assert_eq!(pins.rect(a).unwrap().width, 500);
    assert!(!pins.set_dpi(a, 144).unwrap(), "unchanged");
    // The look is a setting: switching the shadow off redraws every pin without it.
    assert_eq!(
        pins.set_look(Look {
            border: true,
            shadow: false
        }),
        0
    );
    assert_eq!(pins.view(b).unwrap().shadow(), 0);
    assert_eq!(pins.view(b).unwrap().border(), 1);
}

/// Groups: new pins go to the current group; a switch shows its pins and hides the others; a pin
/// can be moved between groups or dropped onto another pin's; reopening keeps the group and the
/// transforms (NOEUL-FR-064, NOEUL-FR-065).
#[test]
fn groups_switch_show_hide_and_survive_a_reopen() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let mut mk = |pins: &mut Pins, x: i32| {
        pins.create(
            &mut w,
            &d,
            exact(
                pattern(&ledger, Rect::new(x, 0, 20, 20)),
                Source::Capture,
                false,
            ),
        )
        .unwrap()
    };
    let a = mk(&mut pins, 0);
    let b = mk(&mut pins, 100);
    assert_eq!(pins.group(a), Some(DEFAULT_GROUP));
    assert_eq!(pins.new_group_name(), "group-2");
    let (shown, hidden) = pins.switch_group("work");
    assert_eq!(
        (shown, hidden),
        (vec![], vec![a, b]),
        "the new group is empty: the others hide"
    );
    let c = mk(&mut pins, 200);
    assert_eq!(
        pins.group(c),
        Some("work"),
        "new pins go to the current group"
    );
    assert_eq!(
        pins.groups(),
        [
            Group {
                name: "default".into(),
                pins: 2,
                hidden: 2
            },
            Group {
                name: "work".into(),
                pins: 1,
                hidden: 0
            },
        ]
    );
    let (shown, hidden) = pins.switch_group(DEFAULT_GROUP);
    assert_eq!((shown, hidden), (vec![a, b], vec![c]));
    assert!(pins.set_group(b, "work").unwrap());
    assert!(!pins.set_group(b, "work").unwrap());
    assert_eq!(pins.in_group("work"), [b, c]);
    // Drop target: the shown pin under a point, never the dropped pin itself.
    assert_eq!(pins.pin_under(b, (5, 5)), Some(a));
    assert_eq!(pins.pin_under(a, (5, 5)), None);
    assert_eq!(
        pins.pin_under(a, (205, 5)),
        None,
        "a hidden pin is no drop target"
    );
    // Close and reopen: the group and the transforms come back.
    pins.orient(a, Orientation::flip_horizontal).unwrap();
    pins.set_opacity(a, 40).unwrap();
    pins.set_group(a, "kept").unwrap();
    let t = pins.info(a).unwrap().transforms;
    pins.close(a).unwrap();
    pins.reopen(&mut w, false).unwrap();
    assert_eq!(pins.info(a).unwrap().transforms, t);
    assert_eq!(pins.group(a), Some("kept"));
    let reopened = w.log.borrow().views.last().unwrap().1;
    assert_eq!(
        (reopened.opacity, reopened.rect),
        (40, pins.rect(a).unwrap())
    );
    // Names.
    assert_eq!(group_name("  Work 2 "), Some("Work 2".into()));
    assert_eq!(group_name("노을"), Some("노을".into()));
    for bad in ["", " ", "-x", "a/b", &"x".repeat(33)] {
        assert_eq!(group_name(bad), None, "{bad:?}");
    }
}

/// An animated picture is never flattened silently: the pin says it shows the first frame.
#[test]
fn an_animated_picture_says_it_shows_its_first_frame() {
    let ledger = Ledger::new();
    let d = desk();
    let mut w = FakeWindows::default();
    let mut pins = Pins::new();
    let id = pins
        .create(
            &mut w,
            &d,
            exact(frame(&ledger, 0), Source::Clipboard, false),
        )
        .unwrap();
    assert_eq!(pins.list(&d)["pins"][0]["animation"], Value::Null);
    assert!(!pins.mark_animation(id, 1), "one frame is no animation");
    assert!(pins.mark_animation(id, 12));
    assert_eq!(
        pins.list(&d)["pins"][0]["animation"],
        json!({"frames": 12, "shown": "first_frame"})
    );
    assert_eq!(pins.info(id).unwrap().animation, Some(12));
}
