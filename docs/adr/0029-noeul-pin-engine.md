# ADR 0029 — Noeul floating pin engine: one creation path, retained layered windows, focus hand-back, pins in memory only

Status: **Accepted** (Step 53) · Date: 2026-11-04 · Builds on
[ADR 0011](0011-portability-boundary.md), [ADR 0012](0012-local-metrics-no-telemetry.md),
[ADR 0021](0021-noeul-product-definition.md) (§3), [ADR 0023](0023-noeul-app-foundation.md)
(§2, §4, §5) and [ADR 0026](0026-noeul-capture-overlay.md) (§5, §8)

## Context

Step 53 (issue #58) turns Step 49's pin basics into the pin engine every later Step builds on:
several independent, always-on-top, borderless, draggable picture windows with their own ID,
position, size, lifetime and context menu; close, hide and show without disturbing the user's
input focus; safe z-order and placement on several monitors, taskbars and DPIs; no pin that can
cover every screen without a way out; local IPC to create, list, hide, show and close pins; a
documented restart policy and monitor-disconnect handling; memory measured against the budget.
Step 51 must be able to pin from the `F1` double-click through this engine with no clipboard in
between.

The rules it serves: [NOEUL_UX_SPEC](../NOEUL_UX_SPEC.md) §3.3 (the pin menu), §4.2 rules 8–10
(the pin of a capture, its focus, a new pin ignoring the rest of the gesture), §6 (pin keys and
gestures) and §8 (pins live in memory); NOEUL-FR-060, NOEUL-FR-061, NOEUL-FR-065, NOEUL-FR-067,
NOEUL-FR-026, NOEUL-NFR-001, NOEUL-NFR-002, NOEUL-NFR-004, NOEUL-NFR-007, NOEUL-SEC-005 and
NOEUL-SEC-013. ADR 0023 fixed the frame: plain Win32 layered windows uploaded with
`UpdateLayeredWindow`, no timer, no GPU context, a picture moved (never copied) from capture to
pin, nothing on disk.

## Decision

### 1. One creation path, and the entry point for Step 51

Every pin is made through one request, [`pins::NewPin`](../../crates/noeul/src/pins.rs): the
picture (a `Frame`, moved in), its source, a placement, whether it takes focus, and a *guard*
time. Placement is `Exact` (the picture's own rectangle at 100 %: a capture stays at its place
and pixel size, NOEUL-FR-020) or `At(point)` (§5). [`App::pin_selection`](../../crates/noeul/src/app.rs)
takes the overlay's `CaptureSelection` straight into a pin — no clipboard, no file
(NOEUL-FR-025, NOEUL-SEC-005). `Enter` uses it today; Step 51's double-click calls it with
`guard` set to the system double-click time, so for that long the new pin swallows every press
(`WM_MOUSEACTIVATE` → `MA_NOACTIVATEANDEAT`, button messages dropped): the remaining clicks of a
triple or quadruple click can neither move, zoom, open the menu of nor close the new pin (UX
spec §4.2 rule 10, NOEUL-FR-026). On failure the selection's copy is dropped and the session
keeps its frozen picture, so the overlay stays open.

### 2. Windows: retained layered windows, the picture held once by Noeul

A pin window stays a `WS_POPUP` layered tool window (no taskbar or Alt+Tab entry). Its picture is
rendered per change — creation, zoom — by the platform-independent
[`zoom::render`](../../crates/noeul/src/zoom.rs) into a **temporary** premultiplied DIB section,
handed to `UpdateLayeredWindow` and freed at once: the system keeps the window's surface itself.
This changes ADR 0023 §2/§4, where the DIB lived as long as the window and a pin cost its pixels
twice inside Noeul: it now costs them once (the `Frame`), plus the surface the system holds for
any layered window (outside Noeul's process; measured below). A test renders an off-screen pin
back through DWM (`PrintWindow` with `PW_RENDERFULLCONTENT`) long after the DIB was freed and
finds the picture's exact pixels, at 100 % and zoomed. Nothing runs while a pin is idle: no
`WM_PAINT`, no timer (NOEUL-NFR-001).

### 3. Focus: only the user's own gesture, and handed back

- A pin takes keyboard focus only when the user made it (overlay `Enter`, `F3`, the tray) or
  clicks it (NOEUL-FR-060), so `Esc`, the arrows and the zoom keys work at once. Real pins are
  therefore **not** `WS_EX_NOACTIVATE`: that style would make a clicked pin deaf to `Esc`.
- Every other operation never activates anything: CLI and IPC pins are created with
  `SW_SHOWNOACTIVATE`; show is `SetWindowPos(…, SWP_NOACTIVATE | SWP_SHOWWINDOW)`; move and
  zoom are `SWP_NOACTIVATE` and `UpdateLayeredWindow`.
- **Hand-back.** Each pin remembers the window the user was in before it took focus (from
  `WM_MOUSEACTIVATE`, `WM_ACTIVATE` or its own `focus()`; never another pin — a chain of pins
  resolves to the program behind them). Hiding or closing the focused pin first gives focus back
  to that window, then hides or destroys the now inactive pin. Without this Windows would
  activate the next window in the z-order — usually another topmost pin — and a second `Esc`
  would close that one too.
- **Test hook.** Under `NOEUL_DEV_SYNTHETIC_DESKTOP` pins are `WS_EX_NOACTIVATE` and not topmost
  (`Style::OFF_SCREEN_TEST`), as ADR 0026 §8 already promised; in Steps 49–50 they were still
  topmost (off-screen, so harmless). A pin's context menu is never opened under the hook: a popup
  menu is always kept on a monitor by Windows and would appear on the user's screen; the request
  is recorded (`last_action: pin.menu / not_shown`) instead.

### 4. Z-order: once to the top, never fighting

New pins, and pins shown again, go to the top of the topmost band once (newest on top). Pins
never re-assert themselves against other topmost windows: no timer, no `HWND_TOPMOST` on
activation of others, no hook. The capture overlay is created later and lies above every pin;
`paste.float` does nothing while the overlay is open (a pin would cover it or take its focus).

### 5. Placement, zoom and the way out of a huge pin

- **Captures** keep their exact place and pixel size, even a selection covering every monitor:
  that is what the user selected, and FR-020 promises it.
- **Pastes** (`F3`, `pin.create` from the clipboard) go to the pointer (or `at`), moved inside
  the **work area** of the monitor there — never under a taskbar or a docked bar — and a picture
  larger than that work area is zoomed out until it fits (`zoom::fit`), keeping every pixel.
- **Zoom** is per pin, 10–800 % on a fixed ladder (wheel, `Plus`/`Minus`, the menu), around the
  pointer when it is over the pin and around the pin's corner otherwise; `0` and a middle-click
  return to 100 %. 100 % is one picture pixel per device pixel on every monitor, and
  `WM_DPICHANGED` is ignored so a pin keeps its pixel size across monitors (NOEUL-FR-061). A
  window surface is limited to 16,384 device pixels per side and 256 MiB. Resizing by dragging
  the border (also NOEUL-FR-061) is **not in this build**: zooming covers resizing until it lands
  with the transforms (proposed for Step 54).
- **Always a way out** (NOEUL-SEC-013): `Esc` closes the focused pin even with every shortcut
  off (the reserved navigation key, like the overlay's `nav.cancel`); `Shift+Esc` destroys it; a
  right-click, `Shift+F10` or the menu key opens its menu (Zoom in, Zoom out, Zoom 100 %, Hide,
  Close, Destroy — only implemented actions, with their effective keys); the tray offers Hide all
  pins / Show all pins and, when a pin is lost, Bring pins on screen; scripts have
  `noeul pin hide|show|close --all` and `noeul quit --yes`. A pin covering every monitor is
  closed by clicking it and pressing `Esc`, or from its menu.

### 6. IPC and CLI

Noeul IPC v1 gains, additively: `pin.create` (`source: "region"` captures that region and pins it
in place; `source: "clipboard"` reads the clipboard once and pins its image at `at` or the
pointer — pictures never cross the pipe), `pin.hide` and `pin.show` (`id` or `all`; `on_screen`
brings lost pins into the nearest work area). `pin.list` reports each pin's on-screen rectangle,
picture size, zoom, visibility and whether a monitor shows it; `status.pins` adds `hidden` and
`off_screen`; every monitor reports its work area. The CLI adds `noeul pin --clipboard
[--at x,y]`, `noeul pin hide` and `noeul pin show [--on-screen]`. Pins made over IPC never take
focus and are announced with a notice (the default of NOEUL-SEC-010). There is no central Kim
daemon: the pipe is Noeul's own (ADR 0023 §5).

### 7. Display changes

`WM_DISPLAYCHANGE` and a work-area change (`WM_SETTINGCHANGE` with `SPI_SETWORKAREA`) reach the
core, which reads every pin window's real rectangle (the system may move windows) and **moves
nothing by itself** (NOEUL-FR-067: a pin on an unplugged monitor comes back when the monitor
does). A pin counts as on screen when at least 24 × 24 device pixels of it (or all of it, when
smaller) lie on one monitor; when a display change leaves pins off-screen, a notice names the way
back: the tray's Bring pins on screen or `noeul pin show --all --on-screen`, which moves each lost
pin into the nearest monitor's work area (zoomed out if it is larger).

### 8. Restart and restore: pins live in memory only

Pins, their zoom and the reopen memory live in the instance's memory and nowhere else
(NOEUL-SEC-005, UX spec §8). Quitting closes them (the tray asks once, `noeul quit` needs
`--yes`); a crash, sign-out or restart loses them; the next instance starts with no pins, no
reopen memory and IDs from `pin-1` again (IDs belong to one instance; scripts compare
`status.instance.started_at`). Nothing is written to restore from. Opt-in backup and restore is
NOEUL-FR-066 (N4) and will be its own decision.

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, release build, 2026-11-04.
`cargo run --release -p noeul --example pin_measure -- --runs 50`: the selection is cut from an
11,520×2,160 frozen picture (`Frame::crop`, as `CaptureSelection` does) and a pin window is
created, rendered and shown; windows lie at -20000,-20000, never activated, not topmost; the
pictures are generated patterns.

| Pin size | Crop p50 / p95 | Window shown p50 / p95 | Total p50 / p95 | Budget (NOEUL-NFR-004) |
| --- | --- | --- | --- | --- |
| 1920×1080 | 1.6 / 1.8 ms | 8.0 / 9.1 ms | **9.6 / 10.9 ms** | p95 ≤ 50 ms |
| 3840×2160 | 6.6 / 7.1 ms | 27.2 / 29.6 ms | **34.0 / 36.6 ms** | p95 ≤ 100 ms |

DWM shows a window at its next composition, at most one refresh after this (*documented*).

| Memory (private bytes of the process) | Picture | Pin cost | Budget (NOEUL-NFR-002: 2× + 2 MB) |
| --- | --- | --- | --- |
| one 1920×1080 pin | 7.9 MiB | +8.8 MiB (zoomed to 200 %: +0.1 MiB more) | 17.8 MiB |
| one 3840×2160 pin | 31.6 MiB | +32.7 MiB (zoomed to 200 %: +0.1 MiB more) | 65.3 MiB |
| twenty 1920×1080 pins (NOEUL-NFR-007) | 158.2 MiB | +158.9 MiB (1.00×) | 355.9 MiB |
| after closing them | — | back to 1.1 MiB, 0 image bytes held | — |

The system keeps each layered window's surface outside Noeul's process: the whole machine's
commit grew by about the surface's size when a pin appeared (+86 MiB around a 31.6 MiB 4K pin,
+147 MiB at 200 %, whose surface is four times larger; a noisy machine-wide number taken while
the PC was in use). That copy exists for every layered window; Noeul no longer keeps a second one
(Steps 49–50 did), and the 256 MiB surface limit bounds what a zoomed pin can ask of the system.
`tests/pin_memory.rs` asserts the per-pin budget on every test run (a 4K pin cost 31.8 MiB).

Idle (NOEUL-NFR-001): with twenty pins open, the measuring process waited for messages for
10.0 s and used **0.0 ms CPU and received 0 messages**: pins wake nothing.

## Not tested here (and why)

- **Visible, on-desktop behaviour**: a pin staying on top of the user's programs, dragging it
  with a real mouse, the real context menu, the wheel over a pin, focus hand-back to a real
  program after `Esc` or Hide, the taskbar and Alt+Tab staying free of pins. Automated runs must
  not show a pin or menu on the maintainer's screen or synthesize input; they assert the styles,
  z-order among Noeul's own off-screen topmost windows, rectangles, visibility, activation counts
  and the foreground window instead. The maintainer's numbered script is
  [NOEUL_UX_SPEC §4.6](../NOEUL_UX_SPEC.md#46-step-53-manual-check-of-real-pins).
- Real multi-monitor and mixed-DPI hardware, unplugging a monitor, a taskbar on another edge,
  Windows 11 and ARM64: tested with synthetic layouts (a monitor removed, monitors replaced, work
  areas with a taskbar) and the runtime's real `WM_DISPLAYCHANGE` path on the test hook's
  unchanged layout.
- Reading the real clipboard through `noeul pin --clipboard`: tests never read the maintainer's
  clipboard; the core path is tested with a fake clipboard and the real path's dry run.

## Consequences

- Pins cost half the memory inside Noeul they did in Steps 49–50; a zoomed pin asks the system
  for a larger surface, bounded by the zoom limits.
- Real pins can be activated, so a click on a pin moves keyboard focus to it (as the spec wants);
  everything Noeul does on its own never does.
- Pins never fight other topmost windows; a program that puts itself above pins stays there until
  a pin is shown again or newly created.
- Losing every pin on quit or crash remains the documented behaviour until the opt-in backup of
  N4.

## Alternatives considered

- **`WS_EX_NOACTIVATE` for every pin**: no focus could ever move, but a clicked pin could not
  receive `Esc` or the arrows, against NOEUL-FR-060/061. Kept for the test hook only.
- **Keeping the DIB alive with the window** (Steps 49–50): twice the picture in Noeul for nothing,
  since `UpdateLayeredWindow` retains the surface. Rejected by measurement.
- **`StretchBlt`/`HALFTONE` scaling**: alpha handling of 32-bit stretching is not defined for
  transparent pictures; the box/nearest scaler is exact, testable and fast enough (≤ 30 ms for 4K).
- **Clamping capture pins to the work area**: would break "same place, same size" (NOEUL-FR-020).
- **Moving pins on display changes**: would lose their place when a monitor comes back
  (NOEUL-FR-067); recovery is explicit instead.
- **A timer or hook keeping pins above everything**: wake-ups while idle (NOEUL-NFR-001) and an
  unsafe z-order fight with other topmost windows.
- **Saving pins for the next start by default**: against memory-first privacy (ADR 0021 §6);
  opt-in backup is N4.

## Requirements

`NOEUL-FR-020`, `NOEUL-FR-025`, `NOEUL-FR-026`, `NOEUL-FR-060`, `NOEUL-FR-061`, `NOEUL-FR-062`,
`NOEUL-FR-064`, `NOEUL-FR-065`, `NOEUL-FR-067`, `NOEUL-FR-070`, `NOEUL-FR-091`, `NOEUL-FR-112`,
`NOEUL-FR-114`, `NOEUL-NFR-001`, `NOEUL-NFR-002`, `NOEUL-NFR-004`, `NOEUL-NFR-007`,
`NOEUL-NFR-014`, `NOEUL-SEC-002`, `NOEUL-SEC-005`, `NOEUL-SEC-008`, `NOEUL-SEC-010`,
`NOEUL-SEC-012`, `NOEUL-SEC-013`.
