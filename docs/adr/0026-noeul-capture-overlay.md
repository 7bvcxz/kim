# ADR 0026 — Noeul capture overlay: one GDI freeze, a back-buffered Win32 window, a selection handed to its output

Status: **Accepted** (Step 50) · Date: 2026-11-04 · Builds on
[ADR 0011](0011-portability-boundary.md), [ADR 0012](0012-local-metrics-no-telemetry.md),
[ADR 0017](0017-universal-customizable-hotkeys.md), [ADR 0021](0021-noeul-product-definition.md)
and [ADR 0023](0023-noeul-app-foundation.md) (§2, §3, §7)

## Context

Step 50 adds `capture.region` (`F1`): a minimal, low-latency overlay over every monitor, built
from one frozen picture, where the user selects a rectangle
([NOEUL_UX_SPEC §4](../NOEUL_UX_SPEC.md#4-the-signature-flow-precise-rules)). The requirements
it serves: NOEUL-FR-010…014, NOEUL-FR-090, NOEUL-NFR-002, NOEUL-NFR-003, NOEUL-NFR-012,
NOEUL-SEC-003, NOEUL-SEC-005 and NOEUL-SEC-012. ADR 0023 fixed the frame: plain Win32 and GDI, no
GPU context or capture buffer while idle, the overlay exists only while a capture is open, and
`F1` is registered only once an action exists behind it (NOEUL-FR-114).

Three points of the Step's issue needed a reading:

- **"Enter confirms."** The spec binds `Enter` to `capture.pin`. An overlay whose only exit is
  "cancel" would be an action that does nothing, which NOEUL-FR-114 forbids, and would make
  `F1` steal a key for no use (ADR 0023 §7). Step 51 owns the double-click gesture; the pin engine
  exists since Step 49.
- **"Click-to-drag after a pointer pause."** A drag must start from the press point whenever the
  pointer later moves beyond the user's drag threshold, however long the pointer rests (with or
  without the button held), and the very first press works even if the pointer never moved after
  the overlay appeared.
- **"Shift+arrow resizes."** The spec's exact bindings are `Ctrl`+arrows to grow and `Shift`+arrows
  to shrink (UX spec §6, NOEUL-FR-042).

## Decision

### 1. Trigger: one overlay, one capture, only on the user's desktop

- `capture.region` joins the shipped manifest with the overlay keys of §4 (the final manifest
  filtered, as before). Its default `F1` is registered through `kim-hotkeys` like `F3`: a chord
  another program owns is reported `taken` with offers, never taken over and never retried in
  the background (KIM-SEC-020). `F1` and `F3` stay the only global defaults (NOEUL-FR-100).
- The same action runs from the tray's left click and menu item (NOEUL-FR-004) and from IPC
  `capture.region` (`noeul capture --interactive`). The CLI calls `AllowSetForegroundWindow` for
  the verified instance so its overlay can take focus from the console the user is in.
- **One at a time** (NOEUL-FR-013): while a capture session is open, every further request does
  nothing, captures nothing and is counted (`captures.ignored`). Rapid presses, a held `F1`, a tray
  click and ten concurrent CLI calls all end in exactly one overlay (tested).
- The desktop check of ADR 0023 §3 runs before anything is captured: on a locked, secure,
  disconnected or display-less desktop nothing is captured and no window opens (NOEUL-SEC-003).

### 2. One freeze

The whole virtual desktop is captured once with GDI into one [`Frame`](../../crates/noeul/src/frame.rs)
in device pixels (the process is per-monitor DPI aware v2), with every monitor's rectangle and
effective DPI (`GetDpiForMonitor`, now in `status.desktop.monitors[].dpi`). Everything the overlay
shows and every output is cut from that frame, never from the live screen (NOEUL-FR-010).

### 3. The overlay window

- A `WS_POPUP` tool window (no taskbar or Alt+Tab entry), `WS_EX_TOPMOST`, exactly over the
  virtual desktop; brought to the foreground with keyboard focus; the input method switched off;
  the crosshair cursor set as soon as it is shown, so a pointer that never moves still gets
  consistent feedback; the pointer captured during a press. `WM_DPICHANGED` is ignored (one
  device-pixel surface spans monitors of different scales); `WM_DISPLAYCHANGE`, `WM_CLOSE` and
  `Alt+F4` leave; activation of another program's window cancels (NOEUL-FR-013, default on; the
  setting is N5).
- **Drawing.** A back buffer (a 32-bit DIB section of the picture's size) is written by the
  platform-independent [`overlay::compose`](../../crates/noeul/src/overlay.rs): the frozen
  picture dimmed by a black veil of about 45 % outside the selection and untouched inside it, the
  one-device-pixel `accent` border with a dark halo just outside the selection (inside it on a
  picture edge), 7-logical-pixel handles, and `bg.overlay` label boxes. GDI draws the labels' text
  into the same buffer (Cascadia Mono → Consolas at caption size, Segoe UI at body size, scaled
  by the DPI of the monitor they are on). [`overlay::dirty`](../../crates/noeul/src/overlay.rs)
  computes which pixels changed between two scenes; only those are recomposed and invalidated,
  and `WM_PAINT` copies them with one `BitBlt`. No timer, no animation (UX spec §10).
- **Memory while open:** the frozen picture plus the back buffer, 8 bytes per virtual-desktop
  pixel; both are freed when the overlay closes. Nothing is created before `F1` or kept after it.

### 4. The selection model and its keys

[`overlay::Overlay`](../../crates/noeul/src/overlay.rs) is a pure state machine fed with inputs
in virtual-desktop device pixels: `Hover` → `Pressed` → `Dragging` → `Selected`, and from
`Selected` through `Pressed` to `Moving` (interior) or `Resizing` (handle zone 9×9 or border band
3 logical pixels, moved outside a selection under 27 logical pixels). A drag starts only beyond
the user's drag rectangle (`SM_CXDRAG`/`SM_CYDRAG`, read when the overlay opens, NOEUL-NFR-012);
`Shift` makes a square; a press that never left the threshold is a click and selects nothing in
this build (window detection is Step 52; the double-click gesture Step 51). Right-click cancels
without a selection, clears outside one, undoes a drag in progress, and does nothing inside one
(its menu is Step 51). Keys wait while a button is held.

Overlay keys shipped in Step 50 (context `capture-overlay`, every one rebindable):
`capture.pin` (`Enter`), `capture.cancel` (`Escape`), `selection.full` (`Ctrl+A`: the monitor
under the pointer, again all monitors), `selection.last` (`R`), `selection.move-*` (arrows),
`selection.grow-*` (`Ctrl`+arrows), `selection.shrink-*` (`Shift`+arrows: `Shift+Up` shrinks from
the bottom edge, as the manifest titles say), one device pixel per press and per auto-repeat.
`Escape` also leaves as the reserved navigation key `nav.cancel` when no overlay binding uses
it, so the overlay never traps the user, even with `--no-hotkeys` (NOEUL-SEC-013). Keyboard
acceleration, `W A S D` pointer keys and `Space` marking are Step 52.

### 5. The output: `capture.pin` through a `CaptureSelection`

`Enter` (or the IPC/tray paths of later Steps) asks the session for its
[`CaptureSelection`](../../crates/noeul/src/selection.rs): an owned copy of the selected pixels,
the rectangle and the monitors it covers. A pin takes the pixels at the same place and size with
keyboard focus; then the session ends and frees the frozen picture. If the pin cannot be created,
the copy is dropped, the overlay stays `Selected` with a banner and the user can retry or cancel
(NOEUL-FR-025). Nothing opens the clipboard or the disk (NOEUL-SEC-005); the selection is never
created automatically: confirming a rectangle only records it. Step 51 adds the double-click and
middle-click gestures, the toolbar and copy/save, all through the same type.

### 6. First-run keyboard help

A small box at the top of the monitor the pointer was on (moved to its bottom when the selection
is in the way, hidden while dragging) lists the keys from the **effective** bindings, so a
changed key reads as such and missing bindings drop out. `[capture] keyboard_help` is
`"first-run"` (default), `"always"` or `"never"`. "Seen" is remembered after the first pin from the
overlay in `%LOCALAPPDATA%\mouv\noeul\ui-state.json` — the only file Step 50 writes on its own,
written atomically, holding no picture.

### 7. Status, IPC and the local metric

`noeul status` reports `captures.ignored`, `captures.overlay` (session, phase, selection,
picture, monitor count, back-buffer bytes, help shown) and `captures.first_frame` (count, last
sample split into capture and render, p50/p95 of the last 100), measured from the request reaching
the instance's UI thread to the first frame painted into the window. It is a local metric only
(ADR 0012). IPC v1 gains `capture.region` and `capture.cancel` (both with `dry_run`); the schema,
its examples and the capabilities document change in the same commit.

### 8. A debug-build test hook

With `NOEUL_DEV_SYNTHETIC_DESKTOP=1`, a **debug** build of `noeul run` captures from a synthetic
two-monitor desktop (100 % and 150 %) placed at (-20000, -20000), and its overlay and pins never
take focus, never stay on top and never touch the cursor. Release builds do not contain it, and it
switches itself off if a real monitor ever overlapped that area. It lets the integration tests
and the e2e suite drive the real binary's overlay without anything appearing on the user's desktop
and without reading the screen (`capture_backend: "synthetic"` makes it visible in `status`).

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, one 5120×1440 monitor at 100 %, release
build, 2026-11-04. `cargo run --release -p noeul --example overlay_measure -- --runs 20` runs the
instance core's real path (`capture.region` → frozen picture → overlay window → first frame
painted; a 60-step drag; `capture.cancel`) with the overlay window **off-screen, never activated,
not topmost**; for the real layout the picture comes from GDI and is moved to an off-screen origin.
The synthetic layouts "capture" a generated pattern, so only their render stage stands for real
work. Nothing was written or printed but timings and sizes.

| Layout | Picture | First frame p50 / p95 (ms) | Capture p50 / p95 | Render p50 / p95 | Drag redraw p50 / p95 | Private MiB before / peak / after / +10 s |
| --- | --- | --- | --- | --- | --- | --- |
| real (GDI) | 5120×1440, 1 monitor, 28.1 MiB | 106.6 / 114.8 | 94.4 / 101.7 | 11.8 / 13.5 | 0.1 / 0.2 | 1.2 / 57.8 / 1.5 / 1.5 |
| synthetic 4K at 150 % | 3840×2160, 1 monitor, 31.6 MiB | — | (pattern) | 20.3 / 24.1 | 0.2 / 0.3 | 1.5 / 64.9 / 1.5 / 1.5 |
| synthetic 3 × 4K | 11520×2160, 3 monitors, 94.9 MiB | — | (pattern) | 53.0 / 63.6 | 0.3 / 0.4 | 1.6 / 192.0 / 1.7 / 1.7 |

`--gdi-stages` on the same monitor (20 runs): `BitBlt` of the whole desktop into a DIB section
87.4 / 99.0 ms with `CAPTUREBLT` and 87.2 / 91.4 ms without it; the full `Gdi::capture` (blit,
copy, opaque alpha) 85.2 / 100.3 ms. The capture stage is the screen read-back itself.

The resident instance after this Step (`scripts/noeul-measure-idle.ps1`, `capture.region` and
`paste.float` registered on `Ctrl+Alt+Shift+F21`/`F22`): binary 2,375,168 bytes, start to answer
66 ms, **0 ms CPU in 60 s idle, 1.64 MiB private, 7.63 MiB working set**, 6 threads, 121 handles,
0 image bytes held.

Against the budgets:

- NOEUL-NFR-003, one 4K monitor (p95 ≤ 150 ms, target 100): the real 7.4-megapixel monitor gives
  **114.8 ms p95**, inside the budget and above the target; a real 3840×2160 monitor (8.3 MP) was
  not available.
- NOEUL-NFR-003, three monitors (p95 ≤ 250 ms): **not measured** (one monitor here). The render
  stage alone is 63.6 ms p95 for three 4K monitors; *if* GDI's read-back scaled with pixels as on
  this machine (about 12 ms per megapixel — an estimate, not a measurement), the capture would take
  about 300 ms and miss the budget. Step 58 must measure GDI on real multi-monitor hardware and
  compare DXGI Desktop Duplication behind the same `CaptureBackend` (ADR 0023 §3).
- NOEUL-NFR-002: memory falls back to the baseline (+0.3 MiB) as soon as the overlay closes and
  stays there 10 s later; idle private memory is 1.64 MiB of 20 MB.
- Responsiveness: redrawing a drag step costs 0.1–0.4 ms, because only the changed strips are
  recomposed.

## Not tested here (and why)

- **The real `F1` on the visible desktop**, the visible topmost overlay, focus and cursor on real
  input, and the time until DWM shows the first frame: an automated run must not cover the
  maintainer's desktop, register `F1` or synthesize input. The first frame is measured until it is
  painted into the window; DWM presents it at its next composition (at most one refresh later,
  *documented*). The maintainer's manual script is in
  [NOEUL_UX_SPEC §4.5](../NOEUL_UX_SPEC.md#45-step-50-manual-check-of-the-real-f1-overlay).
- Mixed-DPI and multi-monitor real hardware, high-contrast themes (the overlay keeps the Kim
  colors; NOEUL-NFR-013 is N5), Windows 11 and ARM64. Multi-monitor and mixed-DPI geometry is
  tested only with synthetic layouts (two and three monitors, negative coordinates, 100/150 %).
- The locked desktop: tested with the synthetic backend and the desktop check of ADR 0023; this
  machine was unlocked during the runs.

## Consequences

- `F1` is now taken from every other program while Noeul runs (the user can rebind or disable it;
  `hotkeys check --all-tools` reports `global_shadows_app` against Subak's help, HOTKEY_CONTRACT
  §13.3).
- A capture holds about twice its picture in memory while the overlay is open (190 MiB for three
  4K monitors), freed at once when it closes.
- The overlay draws with the Kim dark colors only; contrast themes and reduced motion follow in
  N5.
- The capture API decision of ADR 0023 stands for one monitor; multi-monitor latency is an open
  risk for Step 58.

## Alternatives considered

- **A layered window (`UpdateLayeredWindow`)**, as for pins: every update re-uploads the whole
  surface (28–95 MiB per pointer move here); rejected for a surface that changes on every move.
- **A pre-dimmed copy of the picture**: a third full-size buffer for no visible gain; dimming the
  dirty strips on the fly costs well under a millisecond.
- **egui or a GPU swap chain for the overlay**: ADR 0023 rejected a resident GPU context; one
  created per capture would add device creation to the first frame without a measured need.
- **Showing the window before the capture finishes**: the capture would then include the overlay.
- **No output in Step 50** (cancel only): an action that does nothing (NOEUL-FR-114).
- **A test-only IPC or overlay mode in release builds**: an off-screen overlay holding a real
  capture would be a hidden capture (NOEUL-SEC-001); the test hook is debug-only and synthetic.

## Requirements

`NOEUL-FR-004`, `NOEUL-FR-010`…`NOEUL-FR-014`, `NOEUL-FR-022`, `NOEUL-FR-025`, `NOEUL-FR-042`,
`NOEUL-FR-090`, `NOEUL-FR-100`, `NOEUL-FR-112`, `NOEUL-FR-114`, `NOEUL-NFR-001`…`NOEUL-NFR-003`,
`NOEUL-NFR-012`, `NOEUL-NFR-014`, `NOEUL-SEC-001`, `NOEUL-SEC-003`, `NOEUL-SEC-005`,
`NOEUL-SEC-008`, `NOEUL-SEC-012`, `NOEUL-SEC-013`, `KIM-SEC-020`.

## Notes

- 2026-11-04 (Step 53): §8 said the test hook's pins "never stay on top"; until Step 53 they were
  still topmost (far off-screen, so nothing showed). Since [ADR 0029](0029-noeul-pin-engine.md)
  they are created not topmost and `WS_EX_NOACTIVATE`, and a pin's context menu is never opened
  under the hook. §5's `capture.pin` now goes through `App::pin_selection`, the engine's entry
  point that Step 51's double-click also uses.
- 2026-11-04 (Step 51): [ADR 0032](0032-noeul-double-click-pin-and-outputs.md) adds what §4 and
  §5 left to Step 51. A click without a drag inside a confirmed selection may now be the first
  press of the double-click gesture (Noeul counts presses itself; the window class still has no
  `CS_DBLCLKS`); a middle-click inside runs `[gestures] middle_click`; a right-click inside opens
  the selection's context menu instead of doing nothing; the overlay gains the toolbar and
  `capture.copy`, `capture.save` and `capture.quick-save`, all through the same
  `CaptureSelection`. §6's first-run help names the double-click and the copy and save keys. §8's
  test hook now also keeps the outputs off the user's clipboard, screen and Pictures folder
  (ADR 0032 §5). `noeul status` reports `captures.copied`, `captures.saved` and the overlay's
  keyboard `focus`.
