//! The instance core and the displays (Step 58, ADR 0050; NOEUL-FR-016, NOEUL-FR-017,
//! NOEUL-FR-067, NOEUL-FR-090…NOEUL-FR-093, NOEUL-SEC-004): monitor and window captures with exact
//! pixels and their logical mapping, the pointer, the capture keys, the debounce of display
//! notifications, and how pins and the overlay follow attach, detach, a new scale and a turned
//! monitor. Synthetic desktops and fake windows only: no screen, no real display setting.

use super::tests::{FakeShell, drag, overlay_app};
use super::*;
use crate::capture::{Monitor, Synthetic};
use crate::detect::{Affinity, WindowFacts};
use crate::displays::POINTER_SPRITE;

/// Every result of `capture` is checked against the IPC schema.
fn capture(a: &mut App<FakeShell>, params: &Value) -> Result<Value> {
    let r = a.request("capture", params);
    if let Ok(v) = &r {
        let e = crate::schema_errors("noeul-ipc-v1#capture_result", v);
        assert!(e.is_empty(), "{e:?}\n{v:#}");
    }
    r
}

/// The pin's picture equals the synthetic pattern over `r`, except where `except` says.
fn assert_pattern(
    a: &App<FakeShell>,
    id: PinId,
    r: Rect,
    except: impl Fn(i32, i32) -> Option<[u8; 4]>,
) {
    let f = a.pins.picture(id).expect("the pin's picture");
    assert_eq!((f.width(), f.height()), (r.width, r.height), "pixel size");
    let w = r.width as usize;
    for y in 0..r.height as i32 {
        for x in 0..r.width as i32 {
            let (ax, ay) = (r.x + x, r.y + y);
            let i = (y as usize * w + x as usize) * 4;
            let want = except(ax, ay).unwrap_or_else(|| Synthetic::pixel(ax, ay));
            assert_eq!(&f.pixels()[i..i + 4], want, "({ax}, {ay})");
        }
    }
}

fn set_pointer_setting(a: &mut App<FakeShell>, dir: &std::path::Path, on: bool) {
    let paths = Paths::under(dir);
    let edited = settings::set(
        &paths.config_file(),
        &[("capture.pointer", if on { "on" } else { "off" })],
    )
    .unwrap();
    settings::write(&paths.config_file(), &edited.text).unwrap();
    a.reload_settings();
}

#[test]
fn monitors_are_captured_whole_at_their_native_pixels_with_a_logical_mapping() {
    let (mut a, _syn, _dir) = overlay_app("display-monitor");
    // Monitor 1 lies left of the primary at negative coordinates, at 100 %.
    let v = capture(&mut a, &json!({"monitor": 1, "pin": true})).unwrap();
    assert_eq!(v["target"], "monitor");
    assert_eq!(
        v["region"],
        json!({"x": -640, "y": 0, "width": 640, "height": 360})
    );
    assert_eq!(v["monitor"]["number"], 1);
    assert_eq!(
        v["mapping"]["logical"],
        json!({"x": 0, "y": 0, "width": 640, "height": 360})
    );
    assert_eq!(v["warnings"], json!([]));
    assert_eq!(v["pin"]["id"], "pin-1");
    assert_pattern(&a, PinId(1), Rect::new(-640, 0, 640, 360), |_, _| None);
    assert_eq!(
        a.shell.windows.log.borrow().created[0],
        (PinId(1), Rect::new(-640, 0, 640, 360), false),
        "in place, at 100 %, without focus (a CLI request)"
    );
    assert!(a.shell.notices.last().unwrap().contains("command line"));
    // The primary at 150 %: 960 × 540 device pixels are 640 × 360 logical, nothing rescaled.
    let v = capture(&mut a, &json!({"monitor": "primary", "pin": true})).unwrap();
    assert_eq!(
        v["region"],
        json!({"x": 0, "y": 0, "width": 960, "height": 540})
    );
    assert_eq!(
        (
            v["mapping"]["scale_percent"].as_u64(),
            v["mapping"]["logical"]["width"].as_u64()
        ),
        (Some(150), Some(640))
    );
    assert_pattern(&a, PinId(2), Rect::new(0, 0, 960, 540), |_, _| None);
    // The monitor under the pointer (500, 300) is the primary too.
    let v = capture(
        &mut a,
        &json!({"monitor": "pointer", "pin": true, "dry_run": true}),
    )
    .unwrap();
    assert_eq!(
        (v["dry_run"].as_bool(), v["monitor"]["number"].as_u64()),
        (Some(true), Some(2))
    );
    assert_eq!(v["pin"], Value::Null);
    assert_eq!(a.pins.len(), 2, "a dry run pins nothing");
    // A monitor that is not attached, and arguments that name no single target.
    let e = capture(&mut a, &json!({"monitor": 3, "pin": true})).unwrap_err();
    assert_eq!((e.code, e.code.exit_code()), (Code::MonitorNotFound, 3));
    assert!(e.hint.unwrap().contains("2 monitor(s)"));
    for bad in [
        json!({"pin": true}),
        json!({"monitor": 1, "window": "active", "pin": true}),
        json!({"monitor": 0, "pin": true}),
        json!({"monitor": "left", "pin": true}),
        json!({"window": "focused", "pin": true}),
        json!({"monitor": 1}),
    ] {
        assert_eq!(
            capture(&mut a, &bad).unwrap_err().code,
            Code::Usage,
            "{bad}"
        );
    }
    assert_eq!(a.status()["captures"]["pinned"], 2);
}

#[test]
fn the_active_window_is_cut_from_the_screen_and_its_limits_are_said() {
    let (mut a, _syn, _dir) = overlay_app("display-window");
    // No window has the keyboard focus.
    let e = capture(&mut a, &json!({"window": "active", "pin": true})).unwrap_err();
    assert_eq!((e.code, e.code.exit_code()), (Code::WindowNotFound, 3));
    // A window across the seam of the 100 % and the 150 % monitor.
    a.shell.active = Some(WindowFacts::plain(7, Rect::new(-100, 100, 300, 200)));
    let v = capture(&mut a, &json!({"window": "active", "pin": true})).unwrap();
    assert_eq!(
        v["window"],
        json!({"rect": {"x": -100, "y": 100, "width": 300, "height": 200}, "affinity": "none"})
    );
    assert_eq!(v["region"], v["window"]["rect"]);
    assert_eq!(
        (
            v["mapping"]["spans"].as_u64(),
            v["mapping"]["mixed_dpi"].as_bool()
        ),
        (Some(2), Some(true))
    );
    assert_eq!(v["warnings"].as_array().unwrap().len(), 1, "{v:#}");
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .contains("different scales")
    );
    assert_pattern(&a, PinId(1), Rect::new(-100, 100, 300, 200), |_, _| None);
    // Partly beyond the desktop: only the part on the monitors, and it says so; the part in the
    // gap below the left monitor is opaque black.
    a.shell.active = Some(WindowFacts::plain(8, Rect::new(-700, 300, 800, 300)));
    let v = capture(&mut a, &json!({"window": "active", "pin": true})).unwrap();
    let r = Rect::new(-640, 300, 740, 240);
    assert_eq!(Rect::from_json(&v["region"]), Some(r));
    assert!(
        v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("only the part")),
        "{v:#}"
    );
    assert_pattern(&a, PinId(2), r, |x, y| {
        (x < 0 && y >= 360).then_some([0, 0, 0, 255])
    });
    // A window that protects itself is captured as Windows delivers it, with the reason.
    let mut protected = WindowFacts::plain(9, Rect::new(10, 10, 50, 40));
    protected.affinity = Affinity::Monitor;
    a.shell.active = Some(protected);
    let v = capture(&mut a, &json!({"window": "active", "pin": true})).unwrap();
    assert_eq!(v["window"]["affinity"], "monitor");
    assert!(v["warnings"][0].as_str().unwrap().contains("black"));
    protected.affinity = Affinity::Excluded;
    a.shell.active = Some(protected);
    let v = capture(
        &mut a,
        &json!({"window": "active", "pin": true, "dry_run": true}),
    )
    .unwrap();
    assert!(v["warnings"][0].as_str().unwrap().contains("leaves it out"));
    // Minimized, hidden by Windows, or off every monitor: nothing to capture.
    for f in [
        WindowFacts {
            minimized: true,
            ..WindowFacts::plain(10, Rect::new(0, 0, 10, 10))
        },
        WindowFacts {
            cloaked: true,
            ..WindowFacts::plain(11, Rect::new(0, 0, 10, 10))
        },
        WindowFacts {
            visible: false,
            ..WindowFacts::plain(12, Rect::new(0, 0, 10, 10))
        },
        WindowFacts::plain(13, Rect::new(5000, 5000, 100, 100)),
        WindowFacts::plain(14, Rect::new(-640, 400, 100, 100)),
    ] {
        a.shell.active = Some(f);
        let e = capture(&mut a, &json!({"window": "active", "pin": true})).unwrap_err();
        assert_eq!(e.code, Code::WindowNotFound, "{f:?}");
    }
    // Geometry only: every result above passed the schema, whose window object holds a rectangle
    // and the affinity and nothing else (no title, class or program; NOEUL-SEC-007, -008).
    assert_eq!(a.pins.len(), 3);
}

#[test]
fn the_pointer_is_drawn_into_the_picture_only_when_asked_and_says_why_not() {
    let (mut a, syn, dir) = overlay_app("display-pointer");
    syn.set_pointer(Some((-5, 3)));
    let v = capture(&mut a, &json!({"monitor": 1, "pin": true})).unwrap();
    assert_eq!(v["pointer"], Value::Null, "not asked");
    assert_pattern(&a, PinId(1), Rect::new(-640, 0, 640, 360), |_, _| None);
    // Asked: the sprite's hotspot sits exactly on the pointer's device pixel; on the left monitor
    // only the sprite's first five columns are inside the picture.
    let v = capture(&mut a, &json!({"monitor": 1, "pin": true, "pointer": true})).unwrap();
    assert_eq!(
        v["pointer"],
        json!({"drawn": true, "rect": {"x": -5, "y": 3, "width": 12, "height": 19}, "reason": null})
    );
    let sprite = |x: i32, y: i32| {
        let (sx, sy) = (x + 5, y - 3);
        if sx < 0
            || sy < 0
            || sy >= POINTER_SPRITE.len() as i32
            || sx >= POINTER_SPRITE[0].len() as i32
        {
            return None;
        }
        crate::displays::pointer_sprite_pixel(sx as usize, sy as usize)
    };
    assert_pattern(&a, PinId(2), Rect::new(-640, 0, 640, 360), sprite);
    // On the 150 % primary the same sprite, at its own size.
    let v = capture(&mut a, &json!({"region": {"x": -20, "y": 0, "width": 60, "height": 40}, "pin": true, "pointer": true})).unwrap();
    assert_eq!(v["pointer"]["drawn"], true);
    assert_pattern(&a, PinId(3), Rect::new(-20, 0, 60, 40), sprite);
    // Hidden, or outside the picture: no pointer, and a warning says why.
    let v = capture(&mut a, &json!({"region": {"x": 100, "y": 100, "width": 10, "height": 10}, "pin": true, "pointer": true})).unwrap();
    assert_eq!(v["pointer"]["reason"], "outside");
    assert!(v["warnings"][0].as_str().unwrap().contains("outside"));
    syn.set_pointer(None);
    let v = capture(&mut a, &json!({"monitor": 2, "pin": true, "pointer": true})).unwrap();
    assert_eq!(v["pointer"]["reason"], "hidden");
    let v = capture(
        &mut a,
        &json!({"monitor": 2, "pin": true, "pointer": true, "dry_run": true}),
    )
    .unwrap();
    assert_eq!(v["pointer"]["reason"], "dry_run");
    // `[capture] pointer` puts it into the frozen picture of F1 too.
    syn.set_pointer(Some((100, 50)));
    set_pointer_setting(&mut a, &dir, true);
    assert_eq!(a.status()["settings"]["pointer"], "on");
    a.action("capture.region", None);
    let frozen = a.captures.frozen().unwrap();
    let at = |x: i32, y: i32| {
        let r = frozen.rect();
        let i = (((y - r.y) as usize) * r.width as usize + (x - r.x) as usize) * 4;
        [
            frozen.pixels()[i],
            frozen.pixels()[i + 1],
            frozen.pixels()[i + 2],
            frozen.pixels()[i + 3],
        ]
    };
    assert_eq!(at(100, 50), [0, 0, 0, 255], "the sprite's tip");
    assert_eq!(at(101, 52), [255, 255, 255, 255], "its white fill");
    assert_eq!(at(120, 50), Synthetic::pixel(120, 50), "and nothing else");
}

#[test]
fn the_capture_keys_pin_with_focus_and_explain_failures() {
    let (mut a, syn, dir) = overlay_app("display-keys");
    // capture.monitor takes the monitor under the pointer (500, 300): the 150 % primary.
    a.action(CAPTURE_MONITOR, None);
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "capture.monitor", "result": "pinned"})
    );
    let created = a.shell.windows.log.borrow().created.clone();
    assert_eq!(
        created,
        [(PinId(1), Rect::new(0, 0, 960, 540), true)],
        "with focus: the user pressed the key"
    );
    assert!(
        a.shell.notices.is_empty(),
        "the user's own key needs no notice"
    );
    // capture.window without a focused window: a notice, nothing pinned.
    a.action(CAPTURE_WINDOW, None);
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "capture.window", "result": "window_not_found"})
    );
    assert!(
        a.shell.notices[0].contains("no window has the keyboard focus"),
        "{:?}",
        a.shell.notices
    );
    a.shell.active = Some(WindowFacts::plain(5, Rect::new(-600, 20, 200, 100)));
    syn.set_pointer(Some((-550, 30)));
    set_pointer_setting(&mut a, &dir, true);
    a.action(CAPTURE_WINDOW, None);
    assert_eq!(a.status()["last_action"]["result"], "pinned");
    let f = a.pins.picture(PinId(2)).unwrap();
    let i = ((30 - 20) as usize * 200 + 50) * 4;
    assert_eq!(
        &f.pixels()[i..i + 4],
        [0, 0, 0, 255],
        "the pointer is in it"
    );
    // While the overlay is open a direct capture waits its turn.
    a.action("capture.region", None);
    a.action(CAPTURE_MONITOR, None);
    assert_eq!(a.status()["last_action"]["result"], "busy");
    // A locked desktop captures nothing and says nothing.
    a.cancel_overlay();
    syn.set_state(DesktopState::Locked);
    let notices = a.shell.notices.len();
    a.action(CAPTURE_MONITOR, None);
    assert_eq!(a.status()["last_action"]["result"], "desktop_unavailable");
    assert_eq!(a.shell.notices.len(), notices);
}

#[test]
fn display_notifications_are_coalesced_and_handled_once() {
    let (mut a, _syn, _dir) = overlay_app("display-debounce");
    let t0 = Instant::now();
    for i in 0..10 {
        a.note_display_change(t0 + Duration::from_millis(15 * i));
    }
    assert_eq!(a.take_display_timer(), Some(crate::displays::SETTLE));
    assert_eq!(a.take_display_timer(), None, "armed once per request");
    assert_eq!(
        a.status()["displays"],
        json!({"changes": 10, "handled": 0, "pending": true})
    );
    assert_eq!(
        a.status()["last_action"],
        Value::Null,
        "nothing was looked at yet"
    );
    a.display_settled();
    a.display_settled();
    assert_eq!(
        a.status()["displays"],
        json!({"changes": 10, "handled": 1, "pending": false})
    );
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "display.change", "result": "ok"})
    );
    // A pin's WM_DPICHANGED and the overlay's display messages are notifications too.
    a.request("capture", &json!({"monitor": 1, "pin": true}))
        .unwrap();
    a.pin_event(PinEvent::DpiChanged(PinId(1)));
    a.overlay_event(OverlayEvent::DisplayChanged);
    assert!(a.take_display_timer().is_some());
    a.display_settled();
    assert_eq!(a.display_counts(), (12, 2));
}

#[test]
fn pins_keep_their_pixels_and_follow_the_scale_of_their_monitor() {
    let (mut a, syn, _dir) = overlay_app("display-pins");
    let v = a
        .request(
            "capture",
            &json!({"region": {"x": 100, "y": 100, "width": 40, "height": 30}, "pin": true}),
        )
        .unwrap();
    assert_eq!(v["pin"]["id"], "pin-1");
    let view = |a: &App<FakeShell>| a.pins.view(PinId(1)).unwrap();
    assert_eq!(view(&a).dpi, 144, "drawn for the 150 % primary");
    let margin_150 = view(&a).margin();
    // The primary is set to 200 %: the pin keeps its picture's pixel size and place, its outline
    // and shadow (and the band that resizes it) follow the new scale.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(-640, 0, 640, 360), false),
        Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(192),
    ]);
    a.display_changed();
    assert_eq!(view(&a).dpi, 192);
    assert_eq!(view(&a).rect, Rect::new(100, 100, 40, 30));
    assert!(
        view(&a).margin() > margin_150,
        "{} > {margin_150}",
        view(&a).margin()
    );
    assert_eq!(a.pins.picture(PinId(1)).unwrap().width(), 40);
    // The left monitor turns to portrait and a new one is attached above: nothing moves by itself.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(-360, 0, 360, 640), false).with_rotation(90),
        Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(192),
        Monitor::new(Rect::new(0, -1080, 1920, 1080), false).with_hdr(Some(true)),
    ]);
    a.display_changed();
    assert_eq!(view(&a).rect, Rect::new(100, 100, 40, 30));
    let st = a.status();
    assert_eq!(st["desktop"]["monitors"][0]["rotation"], 90);
    assert_eq!(st["desktop"]["monitors"][2]["hdr"], true);
    // A capture of the HDR monitor says what Windows does to it.
    let v = capture(&mut a, &json!({"monitor": 3, "pin": true})).unwrap();
    assert!(v["warnings"][0].as_str().unwrap().contains("HDR"), "{v:#}");
    assert_eq!(v["monitor"]["hdr"], true);
    // The primary is detached: the pin stays where it was, off-screen, with the way back.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(-360, 0, 360, 640), true).with_rotation(90),
    ]);
    a.display_changed();
    assert_eq!(a.status()["pins"]["off_screen"], 2);
    a.request("pin.show", &json!({"all": true, "on_screen": true}))
        .unwrap();
    assert_eq!(a.status()["pins"]["off_screen"], 0);
    let r = a.pins.rect(PinId(1)).unwrap();
    assert!(
        r.x >= -360 && r.right() <= 0,
        "brought onto the portrait monitor: {r}"
    );
    assert_eq!(view(&a).dpi, 96, "and drawn for its scale");
}

#[test]
fn an_open_overlay_follows_a_new_scale_and_ends_when_the_monitors_move() {
    let (mut a, syn, _dir) = overlay_app("display-overlay");
    a.action("capture.region", None);
    drag(&mut a, (100, 100), (300, 200));
    let sel = a.overlay.as_ref().unwrap().model.selection().unwrap();
    let presents = a.shell.overlays.log.borrow().presents;
    let ledger = a.ledger.live();
    // Only the primary's scale changes (150 % → 200 %): the selection and the frozen picture
    // stay, the overlay is drawn again for the new scale.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(-640, 0, 640, 360), false),
        Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(192),
    ]);
    a.display_changed();
    let ov = a.overlay.as_ref().expect("still open");
    assert_eq!(ov.model.selection(), Some(sel));
    assert_eq!(ov.model.monitors()[1].dpi, 192);
    assert!(
        a.shell.overlays.log.borrow().presents > presents,
        "repainted"
    );
    assert_eq!(a.ledger.live(), ledger, "the same frozen picture");
    assert_eq!(a.status()["last_action"]["result"], "ok");
    // A work area changes (the taskbar moved): still the same picture.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(-640, 0, 640, 360), false),
        Monitor::new(Rect::new(0, 0, 960, 540), true)
            .with_dpi(192)
            .with_work(Rect::new(0, 40, 960, 500)),
    ]);
    a.display_changed();
    assert!(a.overlay_open());
    // The left monitor is unplugged: the frozen picture no longer matches, the capture ends and
    // says why; nothing was pinned, copied or kept.
    syn.set_monitors(vec![
        Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(192),
    ]);
    a.display_changed();
    assert!(!a.overlay_open());
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "display.change", "result": "capture_cancelled"})
    );
    assert!(
        a.shell.notices.last().unwrap().contains("displays changed"),
        "{:?}",
        a.shell.notices
    );
    assert_eq!(a.ledger.live(), (0, 0), "the frozen picture is freed");
    assert_eq!(a.status()["captures"]["cancelled"], 1);
    assert_eq!(a.pins.len(), 0);
    // The next capture covers the new layout.
    a.action("capture.region", None);
    assert_eq!(
        a.overlay.as_ref().unwrap().model.picture(),
        Rect::new(0, 0, 960, 540)
    );
}
