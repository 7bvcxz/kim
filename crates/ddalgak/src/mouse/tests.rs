//! Mouse action tests (Step 71; `DDALGAK-FR-015`, `DDALGAK-SEC-005`). Synthetic monitor layouts
//! only (negative coordinates, mixed DPI, gaps between monitors) and a simulated pointer: no test
//! reads or moves the real pointer.

use super::*;
use crate::inject::Recorder;

/// Left: 2880 × 1620 at 150 % with negative x; primary: 1920 × 1080 at 100 %; right: 2560 × 1440 at
/// 125 %, its top 360 px above the primary (the corners beyond each monitor are dead zones).
fn three() -> Layout {
    Layout::new(vec![
        Monitor::new(Rect::sized(-2880, 0, 2880, 1620), 144, false),
        Monitor::new(Rect::sized(0, 0, 1920, 1080), 96, true),
        Monitor::new(Rect::sized(1920, -360, 2560, 1440), 120, false),
    ])
    .unwrap()
}

fn spec(x: i32, y: i32, anchor: Anchor) -> PointSpec {
    PointSpec { x, y, anchor }
}

#[test]
fn logical_pixels_scale_and_round_half_away_from_zero() {
    assert_eq!(to_physical(10, 96), 10);
    assert_eq!(to_physical(10, 120), 13, "12.5 rounds up");
    assert_eq!(to_physical(-10, 120), -13, "and down for negatives");
    assert_eq!(to_physical(10, 144), 15);
    assert_eq!(to_physical(-7, 192), -14);
    assert_eq!(to_physical(1, 144), 2, "1.5 rounds away from zero");
    assert_eq!(to_physical(0, 288), 0);
    assert_eq!(to_physical(10_000, 960), 100_000);
    assert_eq!(
        Monitor::new(Rect::sized(0, 0, 1, 1), 120, true).scale_percent(),
        125
    );
}

#[test]
fn normalized_coordinates_land_on_every_pixel_of_every_layout() {
    // Windows maps a normalized coordinate to `left + n × width / 65536`; every pixel's
    // normalized value must map back to that very pixel, across negative origins, odd sizes and
    // the widest desktops Windows allows.
    let layouts = [
        Layout::single(1920, 1080),
        three(),
        Layout::new(vec![
            Monitor::new(Rect::sized(0, -2160, 3840, 2160), 192, false),
            Monitor::new(Rect::sized(0, 0, 3, 7), 96, true),
        ])
        .unwrap(),
        Layout::new(vec![Monitor::new(
            Rect::sized(-16000, -100, 32000, 200),
            96,
            true,
        )])
        .unwrap(),
    ];
    for l in &layouts {
        let b = l.bounds();
        for x in b.left..b.right {
            let y = b.top + (x - b.left).rem_euclid(b.height());
            let n = l.normalize(Point::new(x, y));
            assert_eq!(l.denormalize(n), Point::new(x, y), "{b:?}");
        }
        assert_eq!(
            l.denormalize(l.normalize(Point::new(b.left, b.top))),
            Point::new(b.left, b.top)
        );
        assert_eq!(
            l.denormalize(l.normalize(Point::new(b.right - 1, b.bottom - 1))),
            Point::new(b.right - 1, b.bottom - 1)
        );
        // Beyond the virtual screen: the nearest edge, never outside 0–65535.
        let far = l.normalize(Point::new(i32::MAX / 2, i32::MIN / 2));
        assert_eq!(l.denormalize(far), Point::new(b.right - 1, b.top));
    }
    assert_eq!(three().bounds(), Rect::new(-2880, -360, 4480, 1620));
}

#[test]
fn targets_outside_the_desktop_land_on_the_nearest_monitor_edge() {
    // The playground example: (−200, 300) beside a single monitor lands on its left edge.
    let one = Layout::single(1920, 1080);
    assert_eq!(one.clamp(Point::new(-200, 300)), (Point::new(0, 300), true));
    assert_eq!(
        one.clamp(Point::new(960, 540)),
        (Point::new(960, 540), false)
    );
    assert_eq!(
        one.clamp(Point::new(5000, 5000)),
        (Point::new(1919, 1079), true)
    );
    let l = three();
    // Inside the left monitor with negative coordinates: unchanged.
    assert_eq!(
        l.clamp(Point::new(-2000, 1500)),
        (Point::new(-2000, 1500), false)
    );
    // Below the primary (its bottom is 1080, the left monitor goes to 1620): the dead zone. The
    // nearest pixel decides: the primary's bottom edge, or the left monitor when it is nearer.
    assert_eq!(
        l.clamp(Point::new(500, 1200)),
        (Point::new(500, 1079), true)
    );
    assert_eq!(l.clamp(Point::new(100, 1200)), (Point::new(-1, 1200), true));
    // Above the primary, left of the right monitor (which starts at y = −360): its left edge.
    assert_eq!(
        l.clamp(Point::new(1900, -300)),
        (Point::new(1920, -300), true)
    );
    // Above the left monitor: its top edge, not the virtual screen's.
    assert_eq!(
        l.clamp(Point::new(-1000, -300)),
        (Point::new(-1000, 0), true)
    );
    // Far beyond every corner.
    assert_eq!(
        l.clamp(Point::new(-99_999, 99_999)),
        (Point::new(-2880, 1619), true)
    );
    assert_eq!(
        l.clamp(Point::new(99_999, -99_999)),
        (Point::new(4479, -360), true)
    );
    assert_eq!(l.monitor_near(Point::new(-1, 0)).dpi, 144);
    assert!(Layout::new(vec![Monitor::new(Rect::sized(0, 0, 0, 10), 96, true)]).is_none());
}

#[test]
fn points_resolve_against_their_anchor_with_its_scale() {
    let l = Layout::new(vec![
        Monitor::new(Rect::sized(-1920, 0, 1920, 1080), 96, false),
        Monitor::new(Rect::sized(0, 0, 2880, 1620), 144, true),
    ])
    .unwrap();
    assert_eq!(l.primary().dpi, 144);
    assert_eq!(
        resolve(spec(100, 50, Anchor::PrimaryMonitor), &l, None),
        Ok(Point::new(150, 75))
    );
    // Virtual-screen points are desktop pixels as they are.
    assert_eq!(
        resolve(spec(-1000, 10, Anchor::VirtualScreen), &l, None),
        Ok(Point::new(-1000, 10))
    );
    let w = WindowArea {
        origin: Point::new(-1500, 400),
        dpi: 120,
    };
    assert_eq!(
        resolve(spec(10, -10, Anchor::ActiveWindow), &l, Some(w)),
        Ok(Point::new(-1487, 387))
    );
    assert_eq!(
        resolve(spec(10, 10, Anchor::ActiveWindow), &l, None),
        Err(Unresolved::NoWindow)
    );
    // Without a primary flag the monitor at (0, 0) is primary; exactly one is.
    let l = Layout::new(vec![
        Monitor::new(Rect::sized(-100, 0, 100, 100), 96, false),
        Monitor::new(Rect::sized(0, 0, 100, 100), 120, false),
    ])
    .unwrap();
    assert_eq!(l.primary().dpi, 120);
    let l = Layout::new(vec![
        Monitor::new(Rect::sized(0, 0, 100, 100), 96, true),
        Monitor::new(Rect::sized(100, 0, 100, 100), 96, true),
    ])
    .unwrap();
    assert_eq!(l.monitors().iter().filter(|m| m.primary).count(), 1);
}

#[test]
fn relative_moves_scale_by_the_pointers_monitor_and_cross_monitors() {
    let l = three();
    // On the primary (100 %): 10 logical px are 10 physical px.
    assert_eq!(
        relative_target(&l, Point::new(500, 500), 10, -10),
        (Point::new(510, 490), false)
    );
    // On the left monitor (150 %): 15.
    assert_eq!(
        relative_target(&l, Point::new(-500, 500), 10, 0),
        (Point::new(-485, 500), false)
    );
    // On the right monitor (125 %): 12.5 → 13.
    assert_eq!(
        relative_target(&l, Point::new(2000, 0), -10, 0),
        (Point::new(1987, 0), false)
    );
    // From the left monitor across onto the primary.
    assert_eq!(
        relative_target(&l, Point::new(-10, 100), 20, 0),
        (Point::new(20, 100), false)
    );
    // Never off the desktop: a huge move stops at the edge.
    assert_eq!(
        relative_target(&l, Point::new(0, 0), -10_000, 0),
        (Point::new(-2880, 0), true)
    );
    assert_eq!(
        relative_target(&l, Point::new(100, 100), 0, 10_000),
        (Point::new(100, 1079), true)
    );
    // A pointer in a dead zone (impossible on Windows, but never trusted) starts from the edge.
    assert_eq!(
        relative_target(&l, Point::new(500, 1500), 0, 0),
        (Point::new(500, 1079), false)
    );
    // Straight down past the primary into the dead zone: it stops at the primary's bottom edge
    // (where a physical mouse stops), never jumping onto the left monitor's corner.
    assert_eq!(
        reach(&l, Point::new(100, 100), Point::new(100, 1500)),
        (Point::new(100, 1079), true)
    );
    // Across all three monitors in one move: the whole way, then the right edge.
    assert_eq!(
        relative_target(&l, Point::new(-2000, 100), 9_999, 0),
        (Point::new(4479, 100), true)
    );
}

#[test]
fn grid_moves_jump_between_cell_centres_and_stay_on_the_grid() {
    let l = Layout::single(1920, 1080);
    let g = Grid {
        columns: 16,
        rows: 9,
    };
    // Cells are 120 × 120; the pointer at (130, 10) is in cell (1, 0).
    assert_eq!(grid_cell(&l, Point::new(130, 10), g), (1, 0));
    assert_eq!(
        grid_target(&l, Point::new(130, 10), 1, 0, g),
        (Point::new(300, 60), false)
    );
    assert_eq!(
        grid_target(&l, Point::new(130, 10), 0, 0, g),
        (Point::new(180, 60), false),
        "snap"
    );
    assert_eq!(
        grid_target(&l, Point::new(130, 10), -5, 0, g),
        (Point::new(60, 60), true)
    );
    assert_eq!(
        grid_target(&l, Point::new(130, 10), 99, 99, g),
        (Point::new(1860, 1020), true)
    );
    // On the left monitor of three() (2880 wide, 4 columns of 720): per monitor.
    let g4 = Grid {
        columns: 4,
        rows: 2,
    };
    assert_eq!(
        grid_target(&three(), Point::new(-2000, 100), 1, 1, g4),
        (Point::new(-1080, 1215), false)
    );
}

#[test]
fn continuous_motion_accelerates_and_extreme_speed_stays_bounded() {
    assert!((speed_at(600, 3, 0) - 600.0).abs() < 1e-9);
    assert!(
        (speed_at(600, 3, 500) - 1500.0).abs() < 1e-9,
        "half way up the ramp"
    );
    assert!((speed_at(600, 3, 1000) - 2400.0).abs() < 1e-9);
    assert!(
        (speed_at(600, 3, 60_000) - 2400.0).abs() < 1e-9,
        "then it stays"
    );
    assert!(
        (speed_at(5000, 10, 2000) - f64::from(MAX_SPEED)).abs() < 1e-9,
        "clamped"
    );
    // 50 px/s for one second in 10 ms ticks: exactly 50 px, carried as fractions.
    let mut m = Motion::new(1, 0, 50, 0).unwrap();
    let mut x = 0;
    for _ in 0..100 {
        x += m.advance(0, 10, 96).0;
    }
    assert_eq!(x, 50);
    // A diagonal is as fast as a straight line (unit direction), up-left is negative.
    let mut d = Motion::new(-1, -1, 1000, 0).unwrap();
    let (mut sx, mut sy) = (0, 0);
    for _ in 0..100 {
        let (a, b) = d.advance(0, 10, 96);
        sx += a;
        sy += b;
    }
    assert!((-708..=-706).contains(&sx) && sx == sy, "{sx} {sy}");
    // 150 % moves 1.5 physical px per logical px.
    let mut h = Motion::new(0, 1, 1000, 0).unwrap();
    assert_eq!(h.advance(0, 10, 144), (0, 15));
    // A late tick (a stalled worker) moves at most MAX_TICK_MS of distance: no jump.
    let mut f = Motion::new(1, 0, 5000, 10).unwrap();
    let (jx, _) = f.advance(10_000, 10_000, 96);
    assert_eq!(jx, i32::try_from(MAX_SPEED * MAX_TICK_MS / 1000).unwrap());
    assert!(
        Motion::new(0, 0, 100, 0).is_none(),
        "no direction, no motion"
    );
}

#[test]
fn held_motions_tick_and_stepped_holds_repeat_after_their_delay() {
    let l = Layout::single(1920, 1080);
    let stepped = MotionSpec {
        dx: 10,
        dy: 0,
        unit: Unit::Pixels,
        kind: MotionKind::Stepped {
            repeat_ms: 100,
            delay_ms: 300,
        },
    };
    let mut h = [HeldMotion::new(stepped, 1000)];
    assert_eq!(h[0].due_ms, 1000, "the first step at once");
    let (p, _) = tick_motions(&mut h, &l, Point::new(0, 0), 1000);
    assert_eq!(p, Point::new(10, 0));
    assert_eq!(h[0].due_ms, 1300, "then after the delay");
    assert_eq!(tick_motions(&mut h, &l, p, 1200).0, p, "not due yet");
    let (p, _) = tick_motions(&mut h, &l, p, 1300);
    assert_eq!(
        (p, h[0].due_ms),
        (Point::new(20, 0), 1400),
        "then every repeat"
    );
    let (p, _) = tick_motions(&mut h, &l, p, 1400);
    assert_eq!((p, h[0].due_ms), (Point::new(30, 0), 1500));
    assert!(!h[0].expired(1000 + MAX_HOLD_MS - 1) && h[0].expired(1000 + MAX_HOLD_MS));
    // Two continuous motions (Up and Right held) add up.
    let c = |dx, dy| MotionSpec {
        dx,
        dy,
        unit: Unit::Pixels,
        kind: MotionKind::Continuous {
            speed: 1000,
            acceleration: 0,
        },
    };
    let mut both = [HeldMotion::new(c(0, -1), 0), HeldMotion::new(c(1, 0), 0)];
    assert_eq!(both[0].due_ms, u64::from(TICK_MS));
    let (p, clamped) = tick_motions(&mut both, &l, Point::new(500, 500), 10);
    assert_eq!((p, clamped), (Point::new(510, 490), false));
    // At the edge the pointer stays on the desktop.
    let (p, clamped) = tick_motions(&mut both, &l, Point::new(1919, 0), 20);
    assert_eq!((p, clamped), (Point::new(1919, 0), true));
}

#[test]
fn clicks_pair_their_downs_and_ups_and_the_wheel_counts_notches() {
    for b in Button::ALL {
        let c = click_events(b, false);
        assert_eq!(
            c,
            [
                OutEvent::down(OutCode::Button(b)),
                OutEvent::up(OutCode::Button(b))
            ]
        );
        let d = click_events(b, true);
        assert_eq!(d.len(), 4);
        assert_eq!(&d[..2], &c[..]);
        assert_eq!(&d[2..], &c[..]);
    }
    let w = wheel_events(ScrollDir::Down, 3);
    assert_eq!(
        w,
        vec![
            OutEvent::down(OutCode::Wheel {
                horizontal: false,
                delta: -120
            });
            3
        ]
    );
    assert_eq!(
        wheel_events(ScrollDir::Up, 1)[0].code,
        OutCode::Wheel {
            horizontal: false,
            delta: 120
        }
    );
    assert_eq!(
        wheel_events(ScrollDir::Right, 1)[0].code,
        OutCode::Wheel {
            horizontal: true,
            delta: 120
        }
    );
    assert_eq!(
        wheel_events(ScrollDir::Left, 1)[0].code,
        OutCode::Wheel {
            horizontal: true,
            delta: -120
        }
    );
    assert_eq!(wheel_events(ScrollDir::Down, 999).len(), 50, "at most 50");
    assert_eq!(wheel_events(ScrollDir::Down, 0).len(), 1, "at least one");
    // Moves and notches never enter the injector's ledger; buttons do until their up.
    let mut inj = Injector::new(Recorder::default());
    inj.send_balanced(&w);
    inj.send_balanced(&[move_event(&three(), Point::new(-5, -5))]);
    assert!(inj.held().is_empty());
    inj.press_code(OutCode::Button(Button::X2));
    assert_eq!(inj.held(), [OutCode::Button(Button::X2)]);
    assert_eq!(inj.release_all(), 1);
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../docs/contracts/examples/ddalgak-rules-v1")
                .join(name),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn every_mouse_step_of_the_examples_compiles_and_broken_ones_are_refused() {
    let mut n = 0;
    for name in ["05-mouse.json", "14-mouse-navigation.json"] {
        let doc = fixture(name);
        for r in doc["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|p| p["rules"].as_array().unwrap())
        {
            for s in r["steps"].as_array().unwrap() {
                if s["type"].as_str().unwrap().starts_with("mouse_") {
                    compile_step(s).unwrap_or_else(|e| panic!("{name} {}: {e}", r["id"]));
                    n += 1;
                }
            }
        }
    }
    assert!(n >= 20, "{n}");
    let c = |v: Value| compile_step(&v);
    assert_eq!(
        c(
            json!({"type":"mouse_move","mode":"relative","dx":0,"dy":-1,"while_held":{"speed":600,"acceleration":3}})
        ),
        Ok(Compiled::Motion(MotionSpec {
            dx: 0,
            dy: -1,
            unit: Unit::Pixels,
            kind: MotionKind::Continuous {
                speed: 600,
                acceleration: 3
            }
        }))
    );
    assert_eq!(
        c(
            json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid","grid":{"columns":8,"rows":4},"while_held":{"repeat_ms":150}})
        ),
        Ok(Compiled::Motion(MotionSpec {
            dx: 1,
            dy: 0,
            unit: Unit::Grid(Grid {
                columns: 8,
                rows: 4
            }),
            kind: MotionKind::Stepped {
                repeat_ms: 150,
                delay_ms: DEFAULT_DELAY_MS
            }
        }))
    );
    assert_eq!(
        c(
            json!({"type":"mouse_click","button":"x2","op":"hold","at":{"x":1,"y":2,"relative_to":"active_window"}})
        ),
        Ok(Compiled::Hold {
            button: Button::X2,
            at: Some(spec(1, 2, Anchor::ActiveWindow))
        })
    );
    assert_eq!(
        c(json!({"type":"mouse_scroll","direction":"left"})),
        Ok(Compiled::Run(MouseStep::Scroll {
            dir: ScrollDir::Left,
            notches: 1
        }))
    );
    for bad in [
        json!({"type":"mouse_move","mode":"absolute","dx":5,"dy":0,"to":{"x":1,"y":1}}),
        json!({"type":"mouse_move","mode":"relative","dx":5}),
        json!({"type":"mouse_move","mode":"relative","dx":0,"dy":0,"while_held":{"speed":100}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid"}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"grid":{"columns":4,"rows":4}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid","grid":{"columns":4,"rows":4},"while_held":{"speed":100}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":100,"repeat_ms":100}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":10}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":100,"delay_ms":5}}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"repeat_ms":100,"acceleration":1}}),
        json!({"type":"mouse_move","mode":"absolute","to":{"x":1,"y":99999}}),
        json!({"type":"mouse_move","mode":"absolute","to":{"x":1,"y":1,"relative_to":"cursor"}}),
        json!({"type":"mouse_scroll","direction":"in"}),
        json!({"type":"mouse_scroll","direction":"up","notches":51}),
        json!({"type":"mouse_click","button":"x3"}),
        json!({"type":"mouse_click","button":"left","op":"down"}),
    ] {
        assert!(c(bad.clone()).is_err(), "{bad}");
    }
}

fn sim(layout: Layout, at: Point) -> (SimPointer, Injector<SimBackend>) {
    let s = SimPointer::new(layout, at);
    let inj = Injector::new(s.backend());
    (s, inj)
}

#[test]
fn a_plan_runs_its_steps_in_order_with_exactly_these_events() {
    // The fixture's "Home double-clicks the middle of the primary monitor" on three monitors.
    let l = three();
    let (s, mut inj) = sim(l.clone(), Point::new(-1000, 500));
    let plan = MousePlan {
        steps: vec![
            MouseStep::Move(MoveTo::At(spec(960, 540, Anchor::PrimaryMonitor))),
            MouseStep::Click {
                button: Button::Left,
                double: true,
                at: None,
            },
            MouseStep::Scroll {
                dir: ScrollDir::Down,
                notches: 3,
            },
            MouseStep::Click {
                button: Button::Middle,
                double: false,
                at: Some(spec(-2000, 100, Anchor::VirtualScreen)),
            },
        ],
        stop_on_error: true,
    };
    assert_eq!(run_plan(&plan, &mut s.screen(), &mut inj), "ok");
    let (nx, ny) = l.normalize(Point::new(960, 540));
    let (mx, my) = l.normalize(Point::new(-2000, 100));
    let mut want = vec![OutEvent::down(OutCode::MoveTo { x: nx, y: ny })];
    want.extend(click_events(Button::Left, true));
    want.extend(wheel_events(ScrollDir::Down, 3));
    want.push(OutEvent::down(OutCode::MoveTo { x: mx, y: my }));
    want.extend(click_events(Button::Middle, false));
    assert_eq!(s.events(), want);
    assert_eq!(s.cursor(), Point::new(-2000, 100));
    assert_eq!(s.moves(), [Point::new(960, 540), Point::new(-2000, 100)]);
    s.with(|st| {
        assert!(st.buttons.is_empty(), "every click released");
        assert_eq!(st.wheel, (-3, 0));
    });
    assert!(inj.held().is_empty());
    // A relative nudge on the 150 % monitor and a click where the pointer is: no extra move.
    s.clear();
    let nudge = MouseStep::Move(MoveTo::By {
        dx: 10,
        dy: 0,
        unit: Unit::Pixels,
    });
    assert_eq!(run_step(&nudge, &mut s.screen(), &mut inj), "ok");
    assert_eq!(s.cursor(), Point::new(-1985, 100));
    let click = MouseStep::Click {
        button: Button::Right,
        double: false,
        at: None,
    };
    assert_eq!(run_step(&click, &mut s.screen(), &mut inj), "ok");
    assert_eq!(s.events()[1..], click_events(Button::Right, false)[..]);
}

#[test]
fn targets_that_refuse_input_stop_the_run_and_nothing_stays_pressed() {
    let l = Layout::single(1920, 1080);
    let click = MouseStep::Click {
        button: Button::Left,
        double: false,
        at: None,
    };
    let to_window = MouseStep::Move(MoveTo::At(spec(10, 10, Anchor::ActiveWindow)));
    let (s, mut inj) = sim(l, Point::new(100, 100));
    s.with(|st| st.target.secure_desktop = true);
    assert_eq!(
        run_step(&click, &mut s.screen(), &mut inj),
        "skipped_secure_desktop"
    );
    s.with(|st| {
        st.target = PointerTarget {
            secure_desktop: false,
            elevated: true,
        }
    });
    assert_eq!(
        run_step(&click, &mut s.screen(), &mut inj),
        "skipped_elevated_target"
    );
    assert!(s.events().is_empty(), "nothing was sent");
    s.with(|st| st.target = PointerTarget::default());
    assert_eq!(
        run_step(&to_window, &mut s.screen(), &mut inj),
        "skipped_disabled_field",
        "no window"
    );
    s.with(|st| {
        st.window = Some(WindowArea {
            origin: Point::new(300, 200),
            dpi: 144,
        });
    });
    assert_eq!(run_step(&to_window, &mut s.screen(), &mut inj), "ok");
    assert_eq!(s.cursor(), Point::new(315, 215));
    // on_error: stop ends at the first failure; continue runs the rest and reports it.
    s.with(|st| st.window = None);
    let scroll = MouseStep::Scroll {
        dir: ScrollDir::Up,
        notches: 1,
    };
    let stop = MousePlan {
        steps: vec![to_window, scroll],
        stop_on_error: true,
    };
    s.clear();
    assert_eq!(
        run_plan(&stop, &mut s.screen(), &mut inj),
        "skipped_disabled_field"
    );
    assert!(s.events().is_empty());
    let cont = MousePlan {
        steps: vec![to_window, scroll],
        stop_on_error: false,
    };
    assert_eq!(
        run_plan(&cont, &mut s.screen(), &mut inj),
        "skipped_disabled_field"
    );
    assert_eq!(s.events(), wheel_events(ScrollDir::Up, 1));
    // SendInput inserting only part of a click: the button that went down goes up at once.
    s.clear();
    s.with(|st| st.limit = Some(1));
    assert_eq!(
        run_step(&click, &mut s.screen(), &mut inj),
        "injection_failed"
    );
    s.with(|st| st.limit = None);
    assert!(inj.held().is_empty(), "the ledger holds nothing");
    s.with(|st| assert!(st.buttons.is_empty(), "the pointer holds nothing"));
    // A recording backend that refuses everything: nothing is held either.
    let mut dead = Injector::new(Recorder {
        sent: Vec::new(),
        limit: Some(0),
    });
    assert_eq!(
        run_step(&click, &mut s.screen(), &mut dead),
        "injection_failed"
    );
    assert!(dead.held().is_empty());
}

fn sim_schema(def: &str) -> Value {
    let sim: Value = serde_json::from_str(include_str!(
        "../../../../docs/contracts/schemas/ddalgak-simulation-v1.schema.json"
    ))
    .unwrap();
    let mut shape = sim["$defs"][def].clone();
    shape["$defs"] = sim["$defs"].clone();
    shape
}

#[test]
fn previews_fit_the_playground_contract_and_match_its_examples() {
    let one = Layout::single(1920, 1080);
    let at = Point::new(400, 300);
    // 05-mouse--clamped-to-desktop: the back button at (−200, 300) clicks at the left edge.
    let back = compile_step(&json!({"type":"mouse_click","button":"x1","at":{"x":-200,"y":300,"relative_to":"virtual_screen"}})).unwrap();
    assert_eq!(
        preview(&back, &one, at, None).unwrap(),
        json!({"button":"x1","op":"click","at":{"x":0,"y":300}})
    );
    // 05-mouse--pointer-right-held: from (400, 300) heading right at 600 px/s.
    let right = compile_step(&json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":600,"acceleration":3}})).unwrap();
    assert_eq!(
        preview(&right, &one, at, None).unwrap(),
        json!({"from":{"x":400,"y":300},"to":{"x":401,"y":300},"clamped":false,"while_held":true,"speed":600})
    );
    // 05-mouse--scroll-and-click: the middle of the primary monitor, then a double click.
    let centre = compile_step(&json!({"type":"mouse_move","mode":"absolute","to":{"x":960,"y":540,"relative_to":"primary_monitor"}})).unwrap();
    assert_eq!(
        preview(&centre, &one, at, None).unwrap(),
        json!({"to":{"x":960,"y":540},"clamped":false})
    );
    // 14-mouse-navigation--cell-up-repeats: the published example is what the code computes.
    let example: Value = serde_json::from_str(include_str!(
        "../../../../docs/contracts/examples/ddalgak-simulation-v1/14-mouse-navigation--cell-up-repeats.json"
    ))
    .unwrap();
    let nav = fixture("14-mouse-navigation.json");
    let cell_up = nav["profiles"][0]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "cell-up")
        .unwrap()["steps"][0]
        .clone();
    let cell_up = compile_step(&cell_up).unwrap();
    let effect = &example["effects"][0];
    assert_eq!(
        preview(&cell_up, &one, Point::new(130, 400), None).unwrap(),
        effect["preview"]
    );
    assert_eq!(
        format!("{}.", summary(&cell_up, "Shift+I")),
        effect["summary"].as_str().unwrap()
    );
    let steps = [
        json!({"type":"mouse_move","mode":"relative","dx":-9999,"dy":5}),
        json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid","grid":{"columns":16,"rows":9},"while_held":{"repeat_ms":120}}),
        json!({"type":"mouse_move","mode":"relative","dx":2,"dy":1,"unit":"grid","grid":{"columns":16,"rows":9}}),
        json!({"type":"mouse_scroll","direction":"right","notches":4}),
        json!({"type":"mouse_click","button":"left","op":"double_click"}),
        json!({"type":"mouse_click","button":"middle","op":"hold","at":{"x":5,"y":5,"relative_to":"active_window"}}),
        json!({"type":"mouse_click","button":"right","op":"hold"}),
    ];
    let w = Some(WindowArea {
        origin: Point::new(100, 100),
        dpi: 96,
    });
    for s in &steps {
        let c = compile_step(s).unwrap();
        let p = preview(&c, &three(), at, w).unwrap();
        let def = format!("preview_{}", s["type"].as_str().unwrap());
        let shape = sim_schema(&def);
        let v = crate::jsonschema::Validator::new(&shape);
        assert!(v.validate(&p).unwrap().is_empty(), "{def}: {p}");
    }
    let far = compile_step(&steps[0]).unwrap();
    assert_eq!(preview(&far, &three(), at, None).unwrap()["clamped"], true);
    let grid = preview(&compile_step(&steps[1]).unwrap(), &one, at, None).unwrap();
    assert_eq!(
        grid["to"],
        json!({"x":540,"y":300}),
        "the next cell's centre"
    );
    assert_eq!(
        (grid["unit"].as_str(), grid["repeat_ms"].as_u64()),
        (Some("grid"), Some(120))
    );
    let hold_w = compile_step(&steps[5]).unwrap();
    assert_eq!(
        preview(&hold_w, &three(), at, None),
        Err(Unresolved::NoWindow)
    );
    // Summaries name keys, directions and numbers only.
    assert_eq!(
        summary(&right, "Right"),
        "Moves the pointer right at 600 px/s while Right is held"
    );
    assert_eq!(
        summary(&back, ""),
        "Clicks the back button (X1) at (-200, 300) on the desktop"
    );
    assert_eq!(
        summary(&centre, ""),
        "Moves the pointer to (960, 540) on the primary monitor"
    );
    assert_eq!(
        summary(&far, ""),
        "Moves the pointer 9999 px left and 5 px down"
    );
    assert_eq!(
        summary(&compile_step(&steps[1]).unwrap(), "L"),
        "Moves the pointer one cell right on a 16×9 grid, again every 120 ms while L is held"
    );
    assert_eq!(
        summary(&compile_step(&steps[3]).unwrap(), ""),
        "Scrolls right 4 notches"
    );
    assert_eq!(
        summary(&compile_step(&steps[4]).unwrap(), ""),
        "Double-clicks the left button"
    );
    assert_eq!(
        summary(&compile_step(&steps[6]).unwrap(), "Space"),
        "Holds the right button while Space is held"
    );
}

#[test]
fn the_playground_moves_only_a_simulated_pointer() {
    let mut p = Playground::new(three(), Point::new(500, 500), None);
    // A continuous move right at 600 px/s with acceleration 3 for one second: ~1500 px (the
    // average speed of the linear ramp), stopped at the primary's right edge? No: it continues
    // onto the right monitor.
    let right = compile_step(&json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"while_held":{"speed":600,"acceleration":3}})).unwrap();
    let e = p.press(&right);
    assert_eq!(
        (e.code, e.to),
        ("ok", Point::new(500, 500)),
        "a continuous move waits a tick"
    );
    let e = p.advance(1000);
    assert!((1950..=2100).contains(&e.to.x) && e.to.y == 500, "{e:?}");
    let e = p.release();
    let stopped = e.to;
    assert_eq!(p.advance(1000).to, stopped, "released: no more movement");
    // A stepped grid move: once at press, then after the delay, then every repeat.
    p.reset(Point::new(130, 10));
    let grid = compile_step(&json!({"type":"mouse_move","mode":"relative","dx":1,"dy":0,"unit":"grid","grid":{"columns":16,"rows":9},"while_held":{"repeat_ms":100,"delay_ms":300}})).unwrap();
    assert_eq!(p.press(&grid).to, Point::new(300, 60));
    assert_eq!(p.advance(299).to, Point::new(300, 60));
    assert_eq!(p.advance(1).to, Point::new(420, 60));
    assert_eq!(p.advance(200).to, Point::new(660, 60));
    p.release();
    // A held button with a point: moved there, held, released on the trigger's up.
    let hold = compile_step(&json!({"type":"mouse_click","button":"left","op":"hold","at":{"x":10,"y":20,"relative_to":"primary_monitor"}})).unwrap();
    let e = p.press(&hold);
    assert_eq!((e.to, e.buttons), (Point::new(10, 20), vec![Button::Left]));
    let e = p.release();
    assert!(e.buttons.is_empty());
    // Clicks and the wheel.
    let e = p.press(
        &compile_step(&json!({"type":"mouse_scroll","direction":"down","notches":3})).unwrap(),
    );
    assert_eq!(e.wheel, (-3, 0));
    p.reset(Point::new(0, 0));
    assert_eq!(p.pointer().cursor(), Point::new(0, 0));
    assert!(p.pointer().events().is_empty());
}

fn rule_with(step: &Value) -> Value {
    json!({ "id": "mouse-rule", "title": "Mouse", "enabled": true, "trigger": { "keys": "F13" },
            "steps": [step],
            "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } })
}

#[test]
fn the_editor_draft_saves_valid_canonical_steps() {
    let drafts = [
        MouseAction::MoveBy {
            dx: 0,
            dy: -10,
            grid: None,
            hold: HoldChoice::Once,
        },
        MouseAction::MoveBy {
            dx: 1,
            dy: 0,
            grid: None,
            hold: HoldChoice::Continuous {
                speed: 600,
                acceleration: 3,
            },
        },
        MouseAction::MoveBy {
            dx: -1,
            dy: 0,
            grid: Some(Grid {
                columns: 16,
                rows: 9,
            }),
            hold: HoldChoice::Repeat {
                repeat_ms: 120,
                delay_ms: 300,
            },
        },
        MouseAction::MoveBy {
            dx: 0,
            dy: 1,
            grid: None,
            hold: HoldChoice::Repeat {
                repeat_ms: 50,
                delay_ms: 0,
            },
        },
        MouseAction::MoveTo(spec(-200, 300, Anchor::VirtualScreen)),
        MouseAction::MoveTo(spec(960, 540, Anchor::PrimaryMonitor)),
        MouseAction::Scroll {
            dir: ScrollDir::Left,
            notches: 1,
        },
        MouseAction::Scroll {
            dir: ScrollDir::Down,
            notches: 5,
        },
        MouseAction::Click {
            button: Button::Left,
            op: "click",
            at: None,
        },
        MouseAction::Click {
            button: Button::X2,
            op: "double_click",
            at: Some(spec(1, 2, Anchor::ActiveWindow)),
        },
        MouseAction::Click {
            button: Button::Middle,
            op: "hold",
            at: None,
        },
    ];
    for a in drafts {
        let d = MouseDraft::new("Mouse rule", "F13", a);
        assert!(d.problems().is_empty(), "{a:?}: {:?}", d.problems());
        let step = d.step();
        assert!(compile_step(&step).is_ok(), "{step}");
        let problems = crate::schema::rule_problems(&rule_with(&step), "rule");
        assert!(problems.is_empty(), "{step}: {problems:?}");
        assert!(d.can_save().is_ok() && d.can_test());
        assert!(!d.summary().starts_with("Not ready"), "{}", d.summary());
        let map = d.screen_map(&three(), Point::new(500, 500), None, 240, 80);
        assert_eq!(map.monitors.len(), 3);
        assert!(
            map.monitors
                .iter()
                .all(|m| m.rect.left >= 0 && m.rect.right <= 240)
        );
    }
    // Defaults are left out; held actions say so.
    let d = MouseDraft::new(
        "Scroll",
        "PageDown",
        MouseAction::Scroll {
            dir: ScrollDir::Down,
            notches: 1,
        },
    );
    assert_eq!(d.step(), json!({"type":"mouse_scroll","direction":"down"}));
    assert!(!d.holds_trigger());
    let h = MouseDraft::new(
        "Drag",
        "Space",
        MouseAction::Click {
            button: Button::Left,
            op: "hold",
            at: None,
        },
    );
    assert!(h.holds_trigger());
    assert_eq!(h.summary(), "Holds the left button while Space is held");
    // Problems name the field, and Save waits for a change.
    let bad = MouseDraft::new(
        "",
        "F13",
        MouseAction::MoveBy {
            dx: 20_000,
            dy: 0,
            grid: Some(Grid {
                columns: 1,
                rows: 9,
            }),
            hold: HoldChoice::Continuous {
                speed: 9000,
                acceleration: 11,
            },
        },
    );
    let paths: Vec<&str> = bad.problems().iter().map(|p| p.path).collect();
    assert_eq!(
        paths,
        [
            "title",
            "steps[0].dx",
            "steps[0].grid",
            "steps[0].while_held.speed",
            "steps[0].while_held.speed",
            "steps[0].while_held.acceleration"
        ]
    );
    assert!(bad.can_save().is_err() && !bad.can_test());
    let mut saved = MouseDraft::new(
        "Scroll",
        "F13",
        MouseAction::Scroll {
            dir: ScrollDir::Up,
            notches: 2,
        },
    );
    saved.saved = Some(Box::new(saved.clone()));
    assert!(!saved.dirty() && saved.can_save().is_err());
    // The screen map shows a clamped target at the edge of the map.
    let far = MouseDraft::new(
        "Far",
        "F13",
        MouseAction::MoveTo(spec(-30_000, 0, Anchor::VirtualScreen)),
    );
    let map = far.screen_map(&three(), Point::new(500, 500), None, 240, 80);
    assert!(map.clamped && map.to == Some(Point::new(0, 14)), "{map:?}");
}

#[test]
fn the_form_fields_are_fields_of_the_published_schema() {
    let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
    let defs = &schema["$defs"];
    for f in MOUSE_FIELDS {
        let name = f.path.strip_prefix("steps[0].").expect("a step field");
        let known = ["step_mouse_move", "step_mouse_scroll", "step_mouse_click"]
            .iter()
            .any(|d| defs[*d]["properties"].get(name).is_some());
        assert!(known, "{} is not a mouse step field", f.path);
        assert!(!f.label.is_empty() && !f.help.is_empty());
    }
}

#[test]
fn a_motion_tick_costs_far_below_its_interval() {
    // The worker's own share of a 10 ms tick: two motions, the path check over three monitors and
    // the normalization (the platform's position read and target check are measured on Windows,
    // win::mouse::tests).
    let l = three();
    let c = |dx, dy| MotionSpec {
        dx,
        dy,
        unit: Unit::Pixels,
        kind: MotionKind::Continuous {
            speed: 3000,
            acceleration: 5,
        },
    };
    let mut held = [HeldMotion::new(c(1, 0), 0), HeldMotion::new(c(0, 1), 0)];
    let mut p = Point::new(-2000, 100);
    let n = 20_000u64;
    let t = std::time::Instant::now();
    for i in 1..=n {
        let (q, _) = tick_motions(&mut held, &l, p, i * u64::from(TICK_MS));
        let _ = move_event(&l, q);
        p = q;
    }
    let per = t.elapsed() / u32::try_from(n).unwrap();
    eprintln!("motion tick (compute only): {per:?}");
    assert!(per.as_micros() < 500, "{per:?}");
    assert!(!l.clamp(p).1, "always on the desktop: {p:?}");
}
