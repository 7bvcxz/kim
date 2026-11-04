# ADR 0044 — Noeul pin transforms and control: an orientation group, a picture that never changes, click-through only with a way back, groups in memory

Status: **Accepted** (Step 54) · Date: 2026-11-04 · Builds on
[ADR 0029](0029-noeul-pin-engine.md) (the pin engine), [ADR 0038](0038-noeul-annotation.md) and
[ADR 0041](0041-noeul-privacy-tools.md) (annotations and redactions on pins),
[ADR 0017](0017-universal-customizable-hotkeys.md) (shortcuts) and
[ADR 0021](0021-noeul-product-definition.md) (§3)

## Context

Step 54 (issue #59) gives pins their transforms and controls: zoom by wheel and keys with an
exact 100 % reset, rotation by 90°, flips, opacity, precise placement, resizing by dragging the
border (proposed by ADR 0029 §5), the pin outline and shadow (UX spec §10), mouse click-through
with a keyboard way back, thumbnail mode, hide and restore, groups with drag-and-drop
organisation, an honest answer for animated GIFs, conflict-free and customisable shortcuts, and
hidden pins that cost nothing. Every transform must be reversible and per pin, compose with
annotations and redactions (redactions stay irreversible in every output), and no state may trap
the user (NOEUL-SEC-013). The requirements: NOEUL-FR-061…064, NOEUL-FR-068, NOEUL-FR-100,
NOEUL-FR-112, NOEUL-NFR-001, NOEUL-NFR-007, NOEUL-SEC-013.

## Decision

### 1. Transforms are a view of an unchanged picture

A pin keeps its picture (the `Frame` at 100 %, as captured) and its annotations untouched. Its
transforms ([`pins::Transforms`](../../crates/noeul/src/pins.rs)) say how it is shown:

- **Orientation** ([`transform::Orientation`](../../crates/noeul/src/transform.rs)): the eight
  symmetries of a rectangle, stored as *mirror left-right, then turn clockwise by 0/90/180/270°*.
  Rotations and flips act in screen space (`flip_horizontal` = `R^-q ∘ M^(m+1)`,
  `flip_vertical` = `R^(2-q) ∘ M^(m+1)`), so every operation has an exact inverse: `1`/`2` undo
  each other, `3` and `4` undo themselves. The window shows `Orientation::apply` of the picture —
  a pixel-exact remap, never a resample. A turn keeps the pin's centre (integer halves, so a turn
  and its inverse land on the same pixel).
- **Zoom** stays per pin (10–800 %); the keys and wheel still step the ladder, and resizing makes
  any whole percent.
- **Opacity** is a percentage, 10–100 in steps of 10 (`Ctrl+Plus`/`Ctrl+Minus`, `Ctrl`+wheel);
  never below 10 % so a pin never vanishes. It is the constant alpha of `UpdateLayeredWindow`
  with no source DC: nothing is painted again.
- **Thumbnail**: the whole (turned) picture at most 96 × 96 logical pixels of its monitor at the
  pin's top-left; the zoom waits for when it ends. Zoom keys and border resizing do nothing
  meanwhile.
- **Click-through**: see §4.

What a pin shows, copies and saves is [`pins::shown`](../../crates/noeul/src/pins.rs): the picture,
its annotations drawn on it with **every redaction applied first** (ADR 0041 §2), *then* turned.
A redaction therefore covers the same image pixels in every orientation and never reveals the
original in an output; turning back shows the unturned output exactly. Copy and save output the
picture as shown at 100 % and fully opaque (opacity is a view of the desktop, not of the picture).
The annotation editor works on the picture as captured (unturned) at the pin's corner; a crop
keeps the kept part where it was shown, mapped through the orientation and zoom.

`zoom-reset` (`0`, middle-click) returns to 100 %, fully opaque and out of thumbnail mode, as
FR-061 says; the orientation stays (it has its own inverse keys).

### 2. Resizing by the border is zooming, in the window's own messages

The border band is the outline and shadow outside the picture plus 4 logical pixels inside it
(less on a small pin, so its middle still moves it; corners reach twice the band along each
side). `WM_NCHITTEST` answers `HTLEFT`…`HTBOTTOMRIGHT` there (the system shows the sizing
cursors) and `HTCAPTION` elsewhere (the system move loop, as before). A press on the band is
handled by the pin itself — no system sizing loop, which needs `WS_THICKFRAME` and cannot keep an
aspect ratio — with the mouse capture (real pins only; the test hook's windows never take it) and
`WM_MOUSEMOVE`/`WM_LBUTTONUP`, each turned into a `PinEvent::Resize(edge, start, delta)`. The core
computes the zoom that keeps the aspect ratio ([`transform::resize`]: a side drives its axis, a
corner the axis that changed more), keeps the opposite side, and clamps to the zoom and surface
limits. Being driven by the window's own messages, it is tested by posting them.

### 3. The outline and the pin shadow, drawn into the window

A pin window is larger than its picture by a margin: a one-pixel `border.subtle` outline (one
device pixel per 100 % of scale) and the soft **pin shadow** `0 2 10 rgba(0,0,0,0.40)`, a new
token `pin.shadow` in [design/tokens.json](../../design/tokens.json) (12 logical pixels: 13 device
pixels of margin at 100 %, 19 at 150 %). The picture keeps its exact place and pixels (a capture
pin is still exactly the selection, NOEUL-FR-020); everything in the model is about the picture's
rectangle, and the window adds the margin. The shadow is smaller than the menus' token because a
layered window is hit-tested by its pixels: the shadow *is* the resize band, and a 24-pixel blur
would make a 30-pixel ring around every pin catch clicks meant for the program below. Every
margin pixel has an alpha of at least 1 while the pin is resizable, so the whole band is hit;
without shadow and outline the band is only inside the picture. Both are settings,
`[pins] border` and `[pins] shadow` (`on`/`off`), applied to open pins at once. The outline,
shadow, badge and band scale with the DPI of the pin's monitor (re-read after a move or display
change); the picture never does.

### 4. Click-through only while there is a way back

`pin.click-through` (`Ctrl+Shift+T`, the menu) sets `WS_EX_TRANSPARENT` on the layered window: the
system hit-tests through it to the window below, and the pin shows a badge (an original accent
ring with a white centre) in its top-right corner. The ways back, all independent of the pin:

1. **The keyboard, pin scope**: a pin keeps keyboard focus when it becomes click-through, so the
   same `Ctrl+Shift+T` (or `Esc`) works at once.
2. **The tray**: "Make all pins clickable" appears while a pin lets clicks through.
3. **A global action, `pins.clickable`**, without a default key (HOTKEY_CONTRACT §7.5 allows only
   `F1` and `F3` as global defaults); the user binds it (`noeul hotkeys set pins.clickable …`).
4. **A second start** (`noeul start`, the Start menu, Win+R): until the settings window exists
   (N5), the running instance makes every pin clickable again and says so (`activate.clickable`).
5. **The CLI**: `noeul pin set --all --click-through off`.

The contract allows a pin-scope or a global action; Noeul uses both, because a pin-scope key alone
works only while the pin still has focus (pins are not in Alt+Tab), and a global key alone would
need a default that §7.5 forbids. Therefore, as NOEUL-FR-063 asks, **a pin's own key or menu
turns click-through on only while the tray icon is shown or `pins.clickable` is registered**
(otherwise the menu item is greyed and the key does nothing, `no_recovery_path`); the CLI may
always turn it on, because its caller holds the CLI way back, and the second start is always
there.

### 5. Groups live in memory, organised by menu, drop, tray and CLI

Every pin belongs to a named group (`default` first); new pins go to the current group. Switching
to a group (tray entry, `noeul pin group switch`) shows its pins and hides every other one;
`pin hide|show|close --group` act on a group, and closing several pins at once needs `--yes`
(NOEUL-FR-064). Organisation by mouse: the pin menu's "Move to a new group" (`pin.group-new`,
bindable, no default) and **dropping a pin with `Ctrl` held onto another pin**, which joins that
pin's group (the system move loop ends in `WM_EXITSIZEMOVE`, read with the `Ctrl` state). Groups
are kept with a closed pin for reopen and are lost with the pins at quit (ADR 0029 §8). A global
`pins.toggle-hidden` (no default) hides every pin, or shows them when one is hidden.

### 6. Animated GIFs: honest, not flattened silently

This build has no GIF decoder and no animated pins: frame navigation, pause and speed are **not
implemented** and no action exists for them (NOEUL-FR-114). Programs that copy a GIF put the
registered `GIF` clipboard format next to a still bitmap of its first frame; Noeul counts the GIF's
image descriptors ([`gif::frame_count`](../../crates/noeul/src/gif.rs), bounds-checked, nothing
decoded) during the same single clipboard read, pins the bitmap, and says so: a notice, the pin's
menu ("first of N frames only") and `pin.list` (`animation: {frames, shown: "first_frame"}`).
NOEUL-FR-068 stays open (P2).

### 7. Shortcuts: pin context versus global, customisable

Pin-focused actions live in the `pin` context (they exist only while a pin has focus, so they
never take another program's keys): `1`/`2`/`3`/`4`, `Ctrl+Plus`/`Ctrl+Minus` (distinct from the
zoom keys `Plus`/`Minus` by the modifier), `T`, `Ctrl+Shift+T`, plus `pin.group-new` unbound. The
global actions `pins.clickable` and `pins.toggle-hidden` ship without keys; nothing new is
registered by default, so no other program's input is swallowed. All are in the manifest (the
final contract example and the shipped subset), so `noeul hotkeys set|disable|reset` changes them
and conflicts are checked; the catalog's shortcut summary lists the two global actions (catalog
v5). Every gesture keeps a keyboard twin: border drag ↔ zoom keys, `Ctrl`+wheel ↔
`Ctrl+Plus`/`Minus`, `Shift`+double-click ↔ `T`, drag ↔ arrows and `noeul pin set --at`.

### 8. CLI and IPC

`noeul pin set (<id> | --group <name> | --all)` with `--at`, `--zoom`, `--opacity`, `--rotate
cw|ccw`, `--rotation`, `--flip`, `--mirrored`, `--thumbnail`, `--click-through`,
`--move-to-group` and `--dry-run`; `noeul pin group list|switch`; `--group` on `pin
hide|show|close`. IPC v1 gains `pin.set`, `group.list` and `group.switch` additively; `pin.list`
and `status` report each pin's orientation, opacity, thumbnail, click-through, group and
animation, never pixels (NOEUL-SEC-008). Every value is checked before anything changes.

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, 2026-11-04. Windows lie at
-20000,-20000, never activated, not topmost; the pictures are generated patterns.

- **Hidden pins** (NOEUL-NFR-001, NOEUL-NFR-007): `tests/pin_transform.rs`
  `hidden_pins_use_no_cpu` (debug build, real instance): twelve hidden pins (turned, faded, one
  thumbnail) — **0.00 ms CPU in 3.0 s**. `cargo run --release -p noeul --example pin_measure --
  --runs 30`: twenty 1920×1080 pins, shown, then hidden, each for 10.0 s — **0.0 ms CPU and 0
  messages** both times: a hidden pin renders nothing and wakes nothing.
- **Transform latency** (release, `pin_measure`, the pin engine over real off-screen windows,
  each step and its inverse):

| Pin size | Turn p50 / p95 | Flip p50 / p95 | Opacity p50 / p95 | Thumbnail p50 / p95 | Resize step p50 / p95 |
| --- | --- | --- | --- | --- | --- |
| 1920×1080 | 11.3 / 20.7 ms | 10.9 / 17.2 ms | 0.0 / 0.1 ms | 13.5 / 17.7 ms | 17.5 / 22.7 ms |
| 3840×2160 | 45.7 / 97.7 ms | 45.8 / 82.0 ms | 0.0 / 0.1 ms | 59.1 / 65.9 ms | 66.5 / 82.1 ms |

- **Pin latency with the outline and shadow** (NOEUL-NFR-004, the same run): a new 1920×1080 pin
  10.7 / 11.4 ms p50/p95 (ADR 0029: 9.6 / 10.9), 3840×2160 38.0 / 40.9 ms (34.0 / 36.6), within
  the 50 / 100 ms budgets; only the margin is painted besides the picture. A first version painted
  the shadow over the whole window and then the picture again, which took 17.9 / 63.2 ms p95; it
  was changed before this Step was finished.
- **Memory** (NOEUL-NFR-002, the same run): a 1920×1080 pin +7.9 MiB (picture 7.9 MiB, budget
  17.8), a 3840×2160 pin +31.8 MiB (31.6, budget 65.3), twenty 1080p pins 1.00× their pictures:
  the transforms keep no copy (a turned picture is a temporary of the window's paint).

Opacity renders nothing (only the constant alpha changes); turns, flips, thumbnails and resize
steps paint the window surface again, which costs about as much as showing a new pin of that size
(ADR 0029).

## Not tested here (and why)

- **Visible behaviour on the real desktop**: dragging the border with a real mouse and the sizing
  cursors, the outline and shadow as seen, the opacity as seen, click-through with real programs
  underneath, the badge, the tray item, a `Shift`+double-click and a `Ctrl`-drop with a real mouse.
  Automated runs must not show a pin on the maintainer's screen or synthesize input: they assert
  styles, hit-testing (`WindowFromPoint` on Noeul's own off-screen windows), rectangles, the
  pixels DWM renders (`PrintWindow`) and the events of the windows' own messages. The script is
  [NOEUL_UX_SPEC §4.11](../NOEUL_UX_SPEC.md#411-step-54-manual-check-of-pin-transforms-and-controls).
- **Animated GIFs from real programs** on the real clipboard: tests never read the maintainer's
  clipboard; the frame counter is tested on generated GIFs, the notice through the core.
- Real mixed-DPI hardware: synthetic layouts (100 % and 150 % monitors) only.

## Consequences

- A pin window is 13 device pixels larger on each side at 100 % (with outline and shadow);
  `pin.list` still reports the picture's rectangle.
- Rotated pins copy and save rotated; opacity never reaches an output.
- Click-through can be lost only if the user turns off the tray, binds no `pins.clickable`, and
  then forgets both the second start and the CLI — and even then the key refuses to turn it on.
- Groups and transforms are lost at quit with the pins until opt-in backup (NOEUL-FR-066, N4).

## Alternatives considered

- **Baking a rotation into the picture**: simpler windows, but repeated turns would compound
  edits and an annotation drawn before a turn would need transforming; rejected for exact
  reversibility.
- **The system sizing loop** (`WS_THICKFRAME`, `WM_SIZING`): no aspect-ratio control without
  fighting it, a visible frame, and untestable without synthesized input.
- **A separate shadow window**: no hit-testing question, but twice the windows, z-order coupling
  and moves; rejected.
- **A default global key for click-through recovery**: forbidden by HOTKEY_CONTRACT §7.5 and would
  take a key from every program.
- **Decoding GIFs now**: a new dependency and an animation timer belong to their own Step with
  NOEUL-FR-072; until then the honest notice.

## Requirements

`NOEUL-FR-020`, `NOEUL-FR-061`, `NOEUL-FR-062`, `NOEUL-FR-063`, `NOEUL-FR-064`, `NOEUL-FR-065`,
`NOEUL-FR-068`, `NOEUL-FR-100`, `NOEUL-FR-112`, `NOEUL-FR-114`, `NOEUL-NFR-001`, `NOEUL-NFR-007`,
`NOEUL-NFR-012`, `NOEUL-SEC-008`, `NOEUL-SEC-012`, `NOEUL-SEC-013`.
