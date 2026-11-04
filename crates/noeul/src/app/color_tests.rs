//! The instance core's colour picker (Step 59, ADR 0053; NOEUL-FR-044, NOEUL-FR-071,
//! NOEUL-FR-092, NOEUL-NFR-013, NOEUL-SEC-002, NOEUL-SEC-008): `C` in the capture overlay copies
//! the exact text of a known synthetic pixel on mixed-DPI monitors, `Shift+C` and the `[color]`
//! settings choose the format, `F3` turns every copied text into a swatch, the `C` key never fires
//! while a label is typed or a tool draws, a selection is never lost, the clipboard is written
//! once and never read, and status and the accessible name never leak anything the overlay does
//! not show. Fake windows, a synthetic desktop and an in-memory clipboard only.

use super::tests::{
    FakeShell, checked_status, choose, drag, key, mouse, overlay_app, press_at, set_config, up,
};
use super::*;
use crate::capture::Synthetic;
use crate::overlay::{Button, HDR_NOTE, Mods};
use crate::paste::{Rgba, Snapshot};
use crate::pins::ContentKind;

/// The known colour of the synthetic desktop at `(x, y)`.
fn known(x: i32, y: i32) -> Rgba {
    let [b, g, r, a] = Synthetic::pixel(x, y);
    Rgba { r, g, b, a }
}

/// What the fake clipboard received last.
fn copied(a: &App<FakeShell>) -> Option<String> {
    a.shell.copied_text.last().cloned()
}

/// `F3` with the fake clipboard holding `text` (what the picker just copied): the new pin.
fn paste_back(a: &mut App<FakeShell>, text: &str) -> PinId {
    a.shell.paste = Some(Snapshot::default().with_text(text));
    let before = a.pins.ids();
    a.action("paste.float", None);
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "paste.float", "result": "pinned"}),
        "{:?}",
        a.shell.notices
    );
    *a.pins
        .ids()
        .iter()
        .find(|id| !before.contains(id))
        .expect("a new pin")
}

/// Whether the pin's picture shows `c` (the swatch's square of the colour).
fn shows(a: &App<FakeShell>, id: PinId, c: Rgba) -> bool {
    let f = a.pins.picture(id).unwrap();
    f.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .any(|p| p[..3] == [c.b, c.g, c.r])
}

/// Acceptance (issue #64): `C` copies the exact HEX of a known pixel on each monitor of a
/// mixed-DPI layout, the capture ends without keeping anything, nothing is read from the
/// clipboard, and `F3` pins the copied text as a swatch of exactly that colour; RGB and HSL too.
#[test]
fn c_copies_the_exact_pixel_text_and_f3_pins_it_as_a_swatch() {
    let (mut a, _syn, _dir) = overlay_app("color-copy");
    // The 100 % monitor left of the origin, the 150 % primary, and its last pixel.
    for (n, (x, y)) in [(-600, 20), (300, 200), (959, 539)].into_iter().enumerate() {
        let held = a.ledger.live();
        a.action("capture.region", None);
        mouse(&mut a, x, y);
        key(&mut a, "C", false, false);
        let c = known(x, y);
        let text = format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b);
        assert_eq!(copied(&a).as_deref(), Some(text.as_str()), "({x}, {y})");
        assert!(!a.overlay_open(), "nothing selected: the capture ended");
        let st = checked_status(&a);
        assert_eq!(
            st["last_action"],
            json!({"action": "color.copy", "result": "copied"})
        );
        assert_eq!(st["captures"]["colors"], n + 1);
        assert_eq!(st["captures"]["history"], 0, "no rectangle was captured");
        assert_eq!(st["captures"]["open"], false);
        assert_eq!(a.ledger.live(), held, "the frozen picture is freed");
        assert!(
            !serde_json::to_string(&st).unwrap().contains(&text[1..]),
            "status never names the colour (NOEUL-SEC-008)"
        );
        let id = paste_back(&mut a, &text);
        let content = a.pins.content(id).unwrap();
        assert_eq!(content.kind, ContentKind::Color);
        assert_eq!(content.text.as_deref(), Some(text.as_str()));
        assert!(shows(&a, id, c), "the swatch shows the picked colour");
    }
    assert_eq!(a.shell.clipboard_writes, 3, "one write per C");
    assert_eq!(
        a.shell.clipboard_reads, 3,
        "the clipboard was read only by the three F3 presses"
    );
    assert!(a.shell.copies.is_empty(), "no picture was copied");
    // RGB and HSL: Shift+C once and twice; each text pastes back as the same colour (HSL within
    // the rounding of whole degrees and percent).
    let (x, y) = (123, 45);
    let c = known(x, y);
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    key(&mut a, "C", false, true);
    key(&mut a, "C", false, false);
    let rgb = copied(&a).unwrap();
    assert_eq!(rgb, format!("rgb({}, {}, {})", c.r, c.g, c.b));
    let id = paste_back(&mut a, &rgb);
    assert!(shows(&a, id, c));
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    key(&mut a, "C", false, true);
    key(&mut a, "C", false, false);
    let hsl = copied(&a).unwrap();
    assert_eq!(
        hsl,
        crate::color::Style::default()
            .with_format(crate::color::Format::Hsl)
            .text(c)
    );
    let back = crate::paste::parse_color(&hsl).unwrap();
    for (p, q) in [(back.r, c.r), (back.g, c.g), (back.b, c.b)] {
        assert!(p.abs_diff(q) <= 3, "{hsl} → {back:?} vs {c:?}");
    }
    let id = paste_back(&mut a, &hsl);
    assert_eq!(a.pins.content(id).unwrap().kind, ContentKind::Color);
}

/// `[color]` sets the case, the alpha and the starting format; `Shift+C` chooses another one,
/// which the next captures keep until the settings change; status names the format only.
#[test]
fn the_format_follows_shift_c_and_the_settings_and_is_remembered() {
    let (mut a, _syn, _dir) = overlay_app("color-format");
    set_config(
        &mut a,
        &[("color.hex_case", "lower"), ("color.alpha", "on")],
    );
    let (x, y) = (-10, 300);
    let c = known(x, y);
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    let st = checked_status(&a);
    assert_eq!(
        st["captures"]["overlay"]["color"],
        json!({"format": "hex", "hex_case": "lower", "alpha": true})
    );
    assert_eq!(
        st["settings"]["color"],
        json!({"format": "hex", "hex_case": "lower", "alpha": "on"})
    );
    key(&mut a, "C", false, false);
    assert_eq!(
        copied(&a).unwrap(),
        format!("#{:02x}{:02x}{:02x}ff", c.r, c.g, c.b)
    );
    // Shift+C twice: HSL, kept for the next capture.
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    key(&mut a, "C", false, true);
    key(&mut a, "C", false, true);
    assert_eq!(
        checked_status(&a)["captures"]["overlay"]["color"]["format"],
        "hsl"
    );
    key(&mut a, "Escape", false, false);
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    assert_eq!(
        checked_status(&a)["captures"]["overlay"]["color"]["format"],
        "hsl"
    );
    key(&mut a, "C", false, false);
    let t = copied(&a).unwrap();
    assert!(t.starts_with("hsla(") && t.ends_with(", 1)"), "{t}");
    // A settings change applies again from the next capture.
    set_config(&mut a, &[("color.format", "rgb"), ("color.alpha", "off")]);
    a.action("capture.region", None);
    mouse(&mut a, x, y);
    key(&mut a, "C", false, false);
    assert_eq!(
        copied(&a).unwrap(),
        format!("rgb({}, {}, {})", c.r, c.g, c.b)
    );
}

/// The `C` conflicts (issue #64): while a label is typed `C` is a character; while a tool draws
/// inside the selection the magnifier is hidden and `C` copies nothing; next to a selection `C`
/// copies but the capture stays with its selection and annotations, and `Enter` still pins them.
#[test]
fn c_never_fires_while_typing_or_drawing_and_never_loses_a_selection() {
    let (mut a, _syn, _dir) = overlay_app("color-conflict");
    a.action("capture.region", None);
    drag(&mut a, (-600, 20), (-201, 219));
    choose(&mut a, annotation::Tool::Text);
    press_at(&mut a, Button::Left, -550, 60, Mods::NONE, 0);
    up(&mut a, -550, 60);
    assert_eq!(
        checked_status(&a)["captures"]["overlay"]["annotation"]["editing_text"],
        true
    );
    // What WM_KEYDOWN and WM_CHAR of a `C` report while typing.
    key(&mut a, "C", false, false);
    a.overlay_event(OverlayEvent::Char('c', false));
    key(&mut a, "C", false, true);
    a.overlay_event(OverlayEvent::Char('C', false));
    assert_eq!(a.shell.clipboard_writes, 0, "typing never copies");
    key(&mut a, "Escape", false, false);
    let text = match &a.overlay.as_ref().unwrap().model.annotations()[0].shape {
        annotation::Shape::Text(t) => t.text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(text, "cC", "C typed into the label");
    assert_eq!(
        checked_status(&a)["captures"]["overlay"]["color"]["format"],
        "hex",
        "Shift+C did not switch the format while typing"
    );
    // A drawing tool, the pointer inside the selection: the magnifier is hidden, C does nothing.
    choose(&mut a, annotation::Tool::Rectangle);
    mouse(&mut a, -400, 120);
    assert_eq!(
        checked_status(&a)["captures"]["overlay"]["magnifier"]["shown"],
        false
    );
    key(&mut a, "C", false, false);
    assert_eq!(a.shell.clipboard_writes, 0);
    // Outside the selection the magnifier shows a pixel: C copies it and the capture stays.
    mouse(&mut a, 300, 300);
    key(&mut a, "C", false, false);
    let c = known(300, 300);
    assert_eq!(
        copied(&a).unwrap(),
        format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
    );
    assert!(a.overlay_open(), "a selection is never lost to C");
    let st = checked_status(&a);
    assert_eq!(st["captures"]["colors"], 0);
    assert_eq!(
        st["captures"]["overlay"]["selection"],
        Rect::new(-600, 20, 400, 200).to_json()
    );
    assert_eq!(st["captures"]["overlay"]["annotation"]["objects"], 1);
    let banner = a
        .shell
        .overlays
        .log
        .borrow()
        .scene
        .clone()
        .unwrap()
        .help
        .unwrap();
    assert!(
        banner.lines[0].starts_with(&format!("Copied #{:02X}", c.r))
            && banner.lines[0].ends_with("After the capture, F3 pins it as a swatch."),
        "{banner:?}"
    );
    // Enter still pins the selection with its annotation.
    key(&mut a, "Escape", false, false); // leave the tool
    key(&mut a, "Enter", false, false);
    assert!(!a.overlay_open());
    assert_eq!(a.pins.len(), 1);
    assert_eq!(checked_status(&a)["captures"]["pinned"], 1);
}

/// The keys stay the user's: `color.copy` rebound to `X` copies and `C` no longer does; turned
/// off, nothing copies. The keyboard help names the effective keys.
#[test]
fn the_picker_keys_are_rebindable_and_named_in_the_help() {
    let (mut a, _syn, dir) = overlay_app("color-keys");
    let paths = Paths::under(&dir);
    std::fs::create_dir_all(paths.hotkeys_file().parent().unwrap()).unwrap();
    std::fs::write(
        paths.hotkeys_file(),
        serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [
                {"action": "color.copy", "bindings": ["X"]},
                {"action": "color.format-next", "bindings": ["K"]}]}))
        .unwrap(),
    )
    .unwrap();
    a.hotkeys().reload();
    assert_eq!(a.hotkeys.keys_for("color.copy"), Some("X"));
    a.action("capture.region", None);
    let help = a.help_lines().unwrap();
    assert_eq!(
        help.last().map(String::as_str),
        Some("X copies the colour \u{b7} K switches HEX, RGB, HSL")
    );
    mouse(&mut a, 40, 40);
    key(&mut a, "C", false, false);
    assert_eq!(a.shell.clipboard_writes, 0, "C is free now");
    key(&mut a, "K", false, false);
    key(&mut a, "X", false, false);
    let c = known(40, 40);
    assert_eq!(
        copied(&a).unwrap(),
        format!("rgb({}, {}, {})", c.r, c.g, c.b)
    );
    std::fs::write(
        paths.hotkeys_file(),
        serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
            "tool": "noeul", "overrides": [{"action": "color.copy", "disabled": true}]}))
        .unwrap(),
    )
    .unwrap();
    a.hotkeys().reload();
    a.action("capture.region", None);
    mouse(&mut a, 40, 40);
    key(&mut a, "C", false, false);
    assert_eq!(a.shell.clipboard_writes, 1, "disabled: nothing copies");
    assert!(a.overlay_open());
}

/// A busy clipboard keeps the overlay with a banner and copies nothing; no notice, no read.
#[test]
fn a_busy_clipboard_keeps_the_capture_and_says_so() {
    let (mut a, _syn, _dir) = overlay_app("color-busy");
    a.shell.clipboard_busy = true;
    a.action("capture.region", None);
    mouse(&mut a, 10, 10);
    key(&mut a, "C", false, false);
    assert!(a.overlay_open());
    assert_eq!(
        checked_status(&a)["last_action"],
        json!({"action": "color.copy", "result": "clipboard_unavailable"})
    );
    let banner = a
        .shell
        .overlays
        .log
        .borrow()
        .scene
        .clone()
        .unwrap()
        .help
        .unwrap();
    assert!(
        banner.lines[0].starts_with("The clipboard is in use"),
        "{banner:?}"
    );
    assert_eq!(a.shell.clipboard_reads, 0);
    assert!(a.shell.notices.is_empty());
    a.shell.clipboard_busy = false;
    key(&mut a, "C", false, false);
    assert!(!a.overlay_open(), "the retry copied and ended the capture");
    assert_eq!(a.shell.copied_text.len(), 1);
}

/// The overlay window's accessible name (UI Automation's Name) says the magnifier's position, the
/// logical position on a scaled monitor, the colour values and the format `C` copies; it changes
/// only when that changes, and a display with HDR adds its note.
#[test]
fn the_accessible_name_follows_the_magnifier() {
    let (mut a, syn, _dir) = overlay_app("color-name");
    a.action("capture.region", None);
    mouse(&mut a, 300, 200);
    let c = known(300, 200);
    let want = format!(
        "Capture overlay. Magnifier at 300, 200 (200, 133 at 150 %): #{:02X}{:02X}{:02X}, rgb({}, {}, {}); copy format HEX",
        c.r, c.g, c.b, c.r, c.g, c.b
    );
    assert_eq!(
        a.shell.overlays.log.borrow().names.last(),
        Some(&want),
        "{:?}",
        a.shell.overlays.log.borrow().names
    );
    let n = a.shell.overlays.log.borrow().names.len();
    mouse(&mut a, 300, 200);
    assert_eq!(
        a.shell.overlays.log.borrow().names.len(),
        n,
        "set only when it changes"
    );
    key(&mut a, "C", false, true);
    assert!(
        a.shell
            .overlays
            .log
            .borrow()
            .names
            .last()
            .unwrap()
            .ends_with("; copy format RGB")
    );
    key(&mut a, "M", false, false);
    assert_eq!(
        a.shell
            .overlays
            .log
            .borrow()
            .names
            .last()
            .map(String::as_str),
        Some("Capture overlay"),
        "the magnifier is hidden"
    );
    key(&mut a, "Escape", false, false);
    // An HDR monitor: the values are the SDR picture's, and the name says so.
    syn.set_monitors(vec![
        crate::capture::Monitor::new(Rect::new(0, 0, 800, 600), true).with_hdr(Some(true)),
    ]);
    a.action("capture.region", None);
    mouse(&mut a, 20, 20);
    let names = a.shell.overlays.log.borrow().names.clone();
    assert!(
        names
            .last()
            .unwrap()
            .ends_with(&format!("; copy format RGB ({HDR_NOTE})")),
        "{names:?}"
    );
    for n in &names {
        assert!(!n.contains("NOEUL"), "only what the overlay shows");
    }
}
