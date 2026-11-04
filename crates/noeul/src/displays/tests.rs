//! Display logic over synthetic layouts (Step 58, ADR 0050): mixed DPI, negative origins,
//! turned monitors, gaps, attach and detach, the debounce of display notifications, monitor
//! numbers and the logical mapping of captured rectangles. Pure: no window, no screen.

use super::*;
use crate::capture::{CaptureBackend, DesktopState, Synthetic};
use crate::frame::Ledger;

/// A layout with everything at once: a 150 % monitor at negative coordinates left of the 100 %
/// primary, a portrait (90°) 125 % monitor right of it reaching higher than the others, and a
/// 4K HDR monitor below the primary. Its bounding rectangle has gaps.
fn hard() -> Vec<Monitor> {
    vec![
        Monitor::new(Rect::new(-2880, 0, 2880, 1620), false).with_dpi(144),
        Monitor::new(Rect::new(0, 0, 1920, 1080), true)
            .with_work(Rect::new(0, 0, 1920, 1040)),
        Monitor::new(Rect::new(1920, -840, 1350, 2400), false)
            .with_dpi(120)
            .with_rotation(90),
        Monitor::new(Rect::new(0, 1560, 3840, 2160), false)
            .with_dpi(192)
            .with_hdr(Some(true)),
    ]
}

fn desktop(monitors: Vec<Monitor>) -> Desktop {
    Synthetic::new(monitors).desktop()
}

#[test]
fn the_debounce_coalesces_a_burst_and_never_waits_forever() {
    let t0 = Instant::now();
    let mut d = Debounce::default();
    assert!(!d.pending());
    assert!(!d.settle(), "a timer with nothing pending does nothing");
    // A burst of ten notifications 20 ms apart: every one re-arms the timer while the burst is
    // shorter than MAX_WAIT - SETTLE.
    let mut armed = 0;
    for i in 0..10u64 {
        if d.note(t0 + Duration::from_millis(20 * i)).is_some() {
            armed += 1;
        }
    }
    assert_eq!(armed, 10);
    assert!(d.pending());
    assert!(d.settle(), "the timer fires once");
    assert!(!d.settle(), "and only once");
    assert_eq!(d.counts(), (10, 1));
    // A storm that never stops: after MAX_WAIT - SETTLE the timer is no longer re-armed, so it
    // fires at most MAX_WAIT after the first notification.
    let t1 = t0 + Duration::from_secs(10);
    let mut last_arm = None;
    for i in 0..100u64 {
        let at = t1 + Duration::from_millis(10 * i);
        if let Some(delay) = d.note(at) {
            assert_eq!(delay, SETTLE);
            last_arm = Some(at);
        }
    }
    let fires_at = last_arm.unwrap() + SETTLE;
    assert!(fires_at <= t1 + MAX_WAIT, "fires within MAX_WAIT");
    assert!(d.settle());
    assert_eq!(d.counts(), (110, 2));
    assert_eq!(
        d.to_json(),
        json!({"changes": 110, "handled": 2, "pending": false})
    );
}

#[test]
fn layouts_differ_in_geometry_or_only_in_details() {
    let a = hard();
    assert_eq!(compare(&a, &a), LayoutChange::Same);
    let mut reordered = a.clone();
    reordered.reverse();
    assert_eq!(compare(&a, &reordered), LayoutChange::Same, "order is not a change");
    // A new scale, a moved taskbar, HDR switched off, another primary: the picture still matches.
    let mut scale = a.clone();
    scale[1].dpi = 144;
    assert_eq!(compare(&a, &scale), LayoutChange::Details);
    let mut work = a.clone();
    work[1].work = Rect::new(0, 40, 1920, 1040);
    assert_eq!(compare(&a, &work), LayoutChange::Details);
    let mut hdr = a.clone();
    hdr[3].hdr = Some(false);
    assert_eq!(compare(&a, &hdr), LayoutChange::Details);
    // Detach, attach, move, resize, turn: it does not.
    assert_eq!(compare(&a, &a[..3]), LayoutChange::Geometry);
    let mut more = a.clone();
    more.push(Monitor::new(Rect::new(-1920, -1080, 1920, 1080), false));
    assert_eq!(compare(&a, &more), LayoutChange::Geometry);
    let mut moved = a.clone();
    moved[0].rect.x -= 1;
    assert_eq!(compare(&a, &moved), LayoutChange::Geometry);
    let mut upside_down = a.clone();
    upside_down[1].rotation = 180; // same size, the picture is turned
    assert_eq!(compare(&a, &upside_down), LayoutChange::Geometry);
}

#[test]
fn monitors_are_named_by_number_primary_or_pointer() {
    let d = desktop(hard());
    assert_eq!(MonitorSpec::parse("primary"), Some(MonitorSpec::Primary));
    assert_eq!(MonitorSpec::parse(" pointer "), Some(MonitorSpec::Pointer));
    assert_eq!(MonitorSpec::parse("2"), Some(MonitorSpec::Number(2)));
    for bad in ["0", "02", "-1", "65", "first", "", "1.5"] {
        assert_eq!(MonitorSpec::parse(bad), None, "{bad}");
    }
    assert_eq!(resolve_monitor(&d, MonitorSpec::Primary, (0, 0)), Some(1));
    assert_eq!(resolve_monitor(&d, MonitorSpec::Number(1), (0, 0)), Some(0));
    assert_eq!(resolve_monitor(&d, MonitorSpec::Number(4), (0, 0)), Some(3));
    assert_eq!(resolve_monitor(&d, MonitorSpec::Number(5), (0, 0)), None);
    // The pointer at negative coordinates, on the turned monitor, and in a gap (nearest).
    assert_eq!(resolve_monitor(&d, MonitorSpec::Pointer, (-10, 10)), Some(0));
    assert_eq!(resolve_monitor(&d, MonitorSpec::Pointer, (2000, -800)), Some(2));
    assert_eq!(resolve_monitor(&d, MonitorSpec::Pointer, (-100, 1700)), Some(0));
    for s in [MonitorSpec::Primary, MonitorSpec::Pointer, MonitorSpec::Number(3)] {
        assert_eq!(MonitorSpec::from_json(&s.to_json()), Some(s));
    }
    assert_eq!(MonitorSpec::from_json(&json!(0)), None);
    assert_eq!(MonitorSpec::from_json(&json!("3")), None, "numbers are numbers");
    let empty = desktop(Vec::new());
    assert_eq!(resolve_monitor(&empty, MonitorSpec::Primary, (0, 0)), None);
    assert_eq!(resolve_monitor(&empty, MonitorSpec::Pointer, (0, 0)), None);
}

#[test]
fn captured_rectangles_map_to_the_logical_pixels_of_their_monitor() {
    let l = hard();
    // A whole monitor at 150 %: 2880 × 1620 device pixels are 1920 × 1080 logical.
    let m = mapping(&l, l[0].rect).unwrap();
    assert_eq!((m.monitor, m.dpi, m.logical), (0, 144, (0, 0, 1920, 1080)));
    assert_eq!((m.spans, m.mixed_dpi), (1, false));
    // A window at negative coordinates on that monitor.
    let w = mapping(&l, Rect::new(-2880 + 300, 150, 900, 600)).unwrap();
    assert_eq!(w.logical, (200, 100, 600, 400));
    // The turned 125 % monitor, measured from its own top-left corner (above the primary).
    let t = mapping(&l, Rect::new(1920, -840, 1350, 2400)).unwrap();
    assert_eq!((t.monitor, t.logical), (2, (0, 0, 1080, 1920)));
    // 200 %: the 4K HDR monitor is 1920 × 1080 logical.
    let h = mapping(&l, Rect::new(0, 1560, 3840, 2160)).unwrap();
    assert_eq!(h.logical, (0, 0, 1920, 1080));
    // Across the seam of 150 % and 100 %: the centre decides the monitor, and it says so.
    let seam = mapping(&l, Rect::new(-100, 100, 300, 50)).unwrap();
    assert_eq!((seam.monitor, seam.spans, seam.mixed_dpi), (1, 2, true));
    assert_eq!(seam.logical, (-100, 100, 300, 50), "logical of the 100 % primary");
    // Edges round on their own, so neighbours still touch at every scale.
    for dpi in [120u32, 144, 168] {
        let one = [Monitor::new(Rect::new(0, 0, 3000, 3000), true).with_dpi(dpi)];
        let a = mapping(&one, Rect::new(0, 0, 7, 7)).unwrap();
        let b = mapping(&one, Rect::new(7, 0, 7, 7)).unwrap();
        assert_eq!(a.logical.0 + a.logical.2, b.logical.0, "{dpi}");
    }
    assert_eq!(
        m.to_json(),
        json!({"monitor": 1, "dpi": 144, "scale_percent": 150,
               "logical": {"x": 0, "y": 0, "width": 1920, "height": 1080},
               "spans": 1, "mixed_dpi": false})
    );
    assert_eq!(mapping(&[], Rect::new(0, 0, 1, 1)), None);
    assert_eq!(monitor_json(&l, 2)["rotation"], 90);
    assert_eq!(monitor_json(&l, 3)["hdr"], true);
    assert_eq!(monitor_json(&l, 3)["scale_percent"], 200);
    assert_eq!(monitor_json(&l, 9), Value::Null);
}

#[test]
fn warnings_are_honest_about_scales_hdr_protection_clipping_and_the_pointer() {
    let l = hard();
    assert!(warnings(&l, Rect::new(10, 10, 100, 100), None, None).is_empty());
    let w = warnings(&l, Rect::new(-100, 100, 300, 50), None, None);
    assert_eq!(w.len(), 1);
    assert!(w[0].contains("different scales") && w[0].contains("monitor 2"), "{w:?}");
    let w = warnings(&l, Rect::new(100, 1500, 100, 200), None, None);
    assert!(w.iter().any(|w| w.contains("monitor 4 shows HDR")), "{w:?}");
    let mut facts = WindowFacts::plain(1, Rect::new(-50, 10, 200, 100));
    facts.affinity = Affinity::Excluded;
    let w = warnings(&l, Rect::new(-50, 10, 200, 100), Some((&facts, Rect::new(-50, 10, 200, 100))), None);
    assert!(w.iter().any(|w| w.contains("excludes itself")), "{w:?}");
    facts.affinity = Affinity::Monitor;
    let w = warnings(&l, Rect::new(0, 10, 150, 100), Some((&facts, Rect::new(0, 10, 150, 100))), None);
    assert!(w.iter().any(|w| w.contains("black")), "{w:?}");
    assert!(w.iter().any(|w| w.contains("only the part")), "{w:?}");
    for (p, word) in [
        (PointerOutcome::Hidden, "hidden"),
        (PointerOutcome::Outside, "outside"),
        (PointerOutcome::Unavailable, "could not be read"),
    ] {
        let w = warnings(&l, Rect::new(10, 10, 10, 10), None, Some(p));
        assert!(w[0].contains(word), "{w:?}");
    }
    assert!(warnings(&l, Rect::new(10, 10, 10, 10), None, Some(PointerOutcome::Drawn(Rect::new(10, 10, 12, 19)))).is_empty());
    // No warning names a window title, a program or a pixel: geometry and states only.
    assert_eq!(PointerOutcome::Hidden.to_json(), json!({"drawn": false, "rect": null, "reason": "hidden"}));
}

#[test]
fn gaps_of_the_virtual_desktop_are_opaque_black_and_monitors_keep_their_pixels() {
    let l = hard();
    let syn = Synthetic::new(l.clone());
    let d = syn.desktop();
    let v = d.virtual_rect.unwrap();
    assert_eq!(v, Rect::new(-2880, -840, 2880 + 3840, 840 + 1560 + 2160));
    let ledger = Ledger::new();
    // A band across the top-left gap (above the 150 % monitor) into the turned monitor.
    let region = Rect::new(-10, -20, 1960, 40);
    let f = syn.capture(&ledger, region).unwrap();
    let px = |x: i32, y: i32| {
        let i = (((y - region.y) as usize) * region.width as usize + (x - region.x) as usize) * 4;
        [f.pixels()[i], f.pixels()[i + 1], f.pixels()[i + 2], f.pixels()[i + 3]]
    };
    assert_eq!(px(-10, -20), [0, 0, 0, 255], "no monitor above the 150 % one");
    assert_eq!(px(1919, -1), [0, 0, 0, 255], "nor above the primary");
    assert_eq!(px(1920, -20), Synthetic::pixel(1920, -20), "the turned monitor reaches up");
    assert_eq!(px(-10, 0), Synthetic::pixel(-10, 0), "the 150 % monitor");
    assert_eq!(px(5, 19), Synthetic::pixel(5, 19), "the primary");
    // Exactly the uncovered pixels were filled: (1920 + 10 columns) × 20 rows.
    let mut g = Frame::solid(&ledger, region, [9, 9, 9, 9]).unwrap();
    assert_eq!(fill_gaps(&mut g, &l), 1930 * 20);
    // Without gaps nothing changes; with no monitor everything is black.
    let mut inside = Frame::solid(&ledger, Rect::new(0, 0, 100, 100), [1, 2, 3, 4]).unwrap();
    assert_eq!(fill_gaps(&mut inside, &l), 0);
    assert!(inside.pixels().chunks(4).all(|p| p == [1, 2, 3, 4]));
    let mut none = Frame::solid(&ledger, Rect::new(0, 0, 3, 2), [1, 2, 3, 4]).unwrap();
    assert_eq!(fill_gaps(&mut none, &[]), 6);
    // Overlapping monitors (a mirrored pair listed twice) leave no gap and are not counted twice.
    let twice = [Monitor::new(Rect::new(0, 0, 10, 10), true), Monitor::new(Rect::new(0, 0, 10, 10), false)];
    let mut o = Frame::solid(&ledger, Rect::new(0, 0, 12, 10), [1, 2, 3, 4]).unwrap();
    assert_eq!(fill_gaps(&mut o, &twice), 20);
}

#[test]
fn the_synthetic_pointer_is_drawn_exactly_at_its_hotspot_or_says_why_not() {
    let syn = Synthetic::new(hard());
    let ledger = Ledger::new();
    let region = Rect::new(-40, -10, 80, 60);
    // Hidden.
    let (f, p) = syn.capture_with_pointer(&ledger, region).unwrap();
    assert_eq!(p, PointerOutcome::Hidden);
    drop(f);
    // Across the seam of the 150 % and the 100 % monitor, at negative coordinates.
    syn.set_pointer(Some((-5, 3)));
    let (f, p) = syn.capture_with_pointer(&ledger, region).unwrap();
    assert_eq!(p, PointerOutcome::Drawn(Rect::new(-5, 3, 12, 19)));
    let w = region.width as usize;
    for y in 0..region.height as i32 {
        for x in 0..region.width as i32 {
            let (ax, ay) = (region.x + x, region.y + y);
            let i = (y as usize * w + x as usize) * 4;
            let got = &f.pixels()[i..i + 4];
            let sprite = (0..12).contains(&(ax + 5)) && (0..19).contains(&(ay - 3));
            let want = if sprite {
                pointer_sprite_pixel((ax + 5) as usize, (ay - 3) as usize)
            } else {
                None
            }
            .unwrap_or(if ay < 0 { [0, 0, 0, 255] } else { Synthetic::pixel(ax, ay) });
            assert_eq!(got, want, "({ax}, {ay})");
        }
    }
    // Partly outside the picture: drawn, clipped; wholly outside: not drawn.
    syn.set_pointer(Some((35, 45)));
    let (_, p) = syn.capture_with_pointer(&ledger, region).unwrap();
    assert_eq!(p, PointerOutcome::Drawn(Rect::new(35, 45, 12, 19)));
    syn.set_pointer(Some((500, 500)));
    let (_, p) = syn.capture_with_pointer(&ledger, region).unwrap();
    assert_eq!(p, PointerOutcome::Outside);
}

#[test]
fn layout_files_stay_far_off_screen_and_follow_attach_and_detach() {
    let ok = json!({
        "monitors": [
            {"x": 0, "y": 0, "width": 640, "height": 400, "primary": true,
             "work": {"x": 0, "y": 0, "width": 640, "height": 368}},
            {"x": -480, "y": 50, "width": 480, "height": 300, "dpi": 144},
            {"x": 640, "y": -200, "width": 300, "height": 480, "dpi": 120, "rotation": 90, "hdr": true}
        ],
        "pointer": {"x": 10, "y": 20}
    });
    let (ms, p) = Synthetic::parse_layout(&ok).unwrap();
    let (ox, oy) = crate::capture::OFF_SCREEN;
    assert_eq!(ms[0].rect, Rect::new(ox, oy, 640, 400));
    assert_eq!(ms[0].work, Rect::new(ox, oy, 640, 368));
    assert_eq!(ms[1].rect, Rect::new(ox - 480, oy + 50, 480, 300));
    assert_eq!((ms[2].dpi, ms[2].rotation, ms[2].hdr), (120, 90, Some(true)));
    assert_eq!((ms[1].hdr, ms[1].primary), (None, false));
    assert_eq!(p, Some((ox + 10, oy + 20)));
    for bad in [
        json!({"monitors": [{"x": 20000, "y": 0, "width": 10, "height": 10}]}),
        json!({"monitors": [{"x": 0, "y": 0, "width": 0, "height": 10}]}),
        json!({"monitors": [{"x": 0, "y": 0, "width": 10, "height": 10, "dpi": 50}]}),
        json!({"monitors": [{"x": 0, "y": 0, "width": 10, "height": 10, "rotation": 45}]}),
        json!({"monitors": "none"}),
        json!({"monitors": [], "pointer": {"x": 99999, "y": 0}}),
    ] {
        assert!(Synthetic::parse_layout(&bad).is_err(), "{bad}");
    }
    // A running desktop follows its file on refresh; a broken file changes nothing.
    let dir = std::env::temp_dir().join(format!("noeul-layout-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("layout.json");
    std::fs::write(&file, serde_json::to_vec(&ok).unwrap()).unwrap();
    let syn = Synthetic::with_layout_file(file.clone(), Some(Rect::new(-3840, 0, 7680, 2160)));
    assert_eq!(syn.desktop().monitors.len(), 3);
    assert_eq!(syn.pointer(), Some((ox + 10, oy + 20)));
    let detached = json!({"monitors": [ok["monitors"][0]], "pointer": null});
    std::fs::write(&file, serde_json::to_vec(&detached).unwrap()).unwrap();
    syn.refresh();
    let d = syn.desktop();
    assert_eq!((d.monitors.len(), syn.pointer()), (1, None));
    assert_eq!(compare(&ms, &d.monitors), LayoutChange::Geometry);
    std::fs::write(&file, b"not json").unwrap();
    syn.refresh();
    assert_eq!(syn.desktop().monitors.len(), 1, "kept");
    // Every allowed layout lies between -29 000 and -11 000: it never reaches a real monitor.
    let reach = json!({"monitors": [{"x": 8000, "y": 8000, "width": 1000, "height": 1000}]});
    let (m, _) = Synthetic::parse_layout(&reach).unwrap();
    assert_eq!((m[0].rect.right(), m[0].rect.bottom()), (-11_000, -11_000));
    assert!(Synthetic::parse_layout(&json!({"monitors": [
        {"x": 8000, "y": 0, "width": 1001, "height": 10}]})).is_err());
    // A layout that would touch the real desktop it must avoid is refused.
    let guarded = Synthetic::with_layout_file(file.clone(), Some(Rect::new(ox, oy, 10, 10)));
    std::fs::write(&file, serde_json::to_vec(&ok).unwrap()).unwrap();
    assert!(!guarded.reload(), "touches the avoided rectangle");
    assert!(syn.reload(), "the other one takes it");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(syn.desktop().state, DesktopState::Available);
}
