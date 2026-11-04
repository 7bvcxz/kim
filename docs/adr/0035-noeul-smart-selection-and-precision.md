# ADR 0035 — Noeul smart selection: a window list taken with the picture, UI Automation rectangles off the UI thread, keyboard precision and in-memory history

Status: **Accepted** (Step 52) · Date: 2026-11-04 · Builds on
[ADR 0021](0021-noeul-product-definition.md) (§5), [ADR 0026](0026-noeul-capture-overlay.md)
(§3, §4, §8) and [ADR 0032](0032-noeul-double-click-pin-and-outputs.md) (§1, §5)

## Context

Step 52 (issue #57) adds what makes a region capture quick and exact: the window or UI element
under the pointer is detected and selected with a click (NOEUL-FR-040), a magnifier shows the
pixels and their colour (NOEUL-FR-041), the keyboard moves the selection and the pointer pixel by
pixel with acceleration (NOEUL-FR-042), `Space` marks corners (NOEUL-FR-043), selection edges
snap, earlier rectangles come back with `Comma`/`Period` (NOEUL-FR-050, in memory), and layered,
capture-protected, fractionally scaled and undiscoverable windows fall back to the plain region
selection. Detection must read geometry only (NOEUL-SEC-007), never bypass capture protection
(NOEUL-SEC-004), never elevate (NOEUL-SEC-003), and never block the overlay on another program.
ADR 0026 deferred detection and the magnifier to this Step; the spec left open how windows are
found under a full-screen overlay, where UI Automation runs, what `Space` does over a highlighted
window, and how the keyboard pointer interacts with detection.

## Decision

### 1. Windows: one list, taken right after the frozen picture

`EnumWindows` lists the top-level windows in Z-order, topmost first (*documented*), **right after
the picture is frozen and before the overlay is shown** ([`win::detect::snapshot`](../../crates/noeul/src/win/detect.rs)).
Each window gives only [`WindowFacts`](../../crates/noeul/src/detect.rs): visibility, minimized,
cloaked (`DWMWA_CLOAKED`: other virtual desktops), `WS_EX_LAYERED` and `WS_EX_TRANSPARENT`, the
constant opacity of a layered window, the capture affinity (`GetWindowDisplayAffinity`) and the
extended frame bounds (`DWMWA_EXTENDED_FRAME_BOUNDS`, else `GetWindowRect`) — device pixels for a
per-monitor-aware process, whatever the window's own awareness. The platform-independent
[`detect::classify`](../../crates/noeul/src/detect.rs) decides what is offered:

| Window | Detection |
| --- | --- |
| hidden, minimized, cloaked, empty, outside the picture | skipped |
| layered and click-through, or layered with opacity 0 | skipped (the eye and the pointer go through it) |
| `WDA_EXCLUDEFROMCAPTURE` | skipped: absent from the frozen picture, so what lies below is what the user sees |
| `WDA_MONITOR` (or DRM that shows black) | **offered as protected**: never highlighted, a click does not select it, the size chip says "Protected window: Windows captures it as black" and `status` reports `protected: true`; the plain region selection remains (NOEUL-SEC-004) |
| any other window, including other layered ones and Noeul's own pins | offered, its frame bounds clipped to the picture |

Hover hit-testing is then pure geometry ([`Snapshot::window_at`](../../crates/noeul/src/detect.rs)):
the topmost offered window containing the pointer. The list matches the frozen picture (the live
desktop may change behind the overlay), costs no cross-process call on pointer moves, and is
dropped with the overlay. With `[capture] detection = "off"` nothing is listed until `E` turns
detection on; then the list is taken at that moment, skipping the overlay window itself.

### 2. Elements: UI Automation bounding rectangles, off the UI thread, only after `E`

`detect.cycle` (`E`) cycles window → element → off. In element mode the model issues an
[`ElementRequest`](../../crates/noeul/src/detect.rs) (the window under the pointer and the point)
when the pointer moves; the runtime hands it to an
[`ElementWorker`](../../crates/noeul/src/win/detect.rs): one thread per capture, started on the
first request, in a COM multithreaded apartment with one `CUIAutomation` object, ended when the
overlay closes (never joined: a lookup stuck in a hung program is abandoned). Only the newest
request is answered. The worker:

- skips windows of processes with higher rights — the target's token is read
  (`PROCESS_QUERY_LIMITED_INFORMATION`, `TokenElevation`); a token Noeul may not open or an
  elevated one while Noeul is not means `denied` at once (UIPI, *documented*), instead of waiting
  for UI Automation to refuse ([`uia_permission`](../../crates/noeul/src/detect.rs));
- sets `IUIAutomation2`'s connection and transaction timeouts to the 300 ms budget;
- walks the control view from `ElementFromHandle` down: per level one
  `FindAllBuildCache(TreeScope_Children)` with **only `UIA_BoundingRectanglePropertyId`** cached,
  taking the smallest child that contains the point (32 levels, 512 children per level, 300 ms);
- answers `Found(rect)`, `WindowOnly` or `Failed(denied | timeout | unavailable)`.

The full-screen overlay hides every window from `ElementFromPoint`/`WindowFromPoint`, which is why
the walk starts from the window found in §1. The model checks every answer
([`check_element`](../../crates/noeul/src/detect.rs)): it must contain the point and lie inside its
window (one pixel of tolerance); otherwise it is tried once as logical pixels of the window's
monitor (a provider that reports unscaled coordinates under fractional scaling,
[`dpi::rect_from_logical`](../../crates/noeul/src/dpi.rs)), and failing that the window rectangle
is used (`invalid_rect`). A window that was `denied`, timed out or is `unavailable` is not asked
again during that capture. Until an answer arrives, the window rectangle is shown; an older answer
is used only while its element still holds the pointer in the same window.

COM is called through raw vtables (slot numbers from UIAutomationClient.h, checked against the
`windows` 0.62 bindings): no new crate.

### 3. Privacy by construction

The detection types have no field for text. The window list never reads a title, class name,
process name or content; the worker never asks for a name, value, help text, automation id or
pattern — a test inspects `win/detect.rs` for those calls. Status and IPC report the target's kind
and rectangle, whether the pointer is over a protected window, the fallback reason and a count;
the magnifier's colour is drawn, never reported (a colour is a pixel, NOEUL-SEC-008). Under the
debug-build test hook the window list holds only windows of the processes named in
`NOEUL_DEV_DETECT_PIDS` — checked from the owner's process id **before** anything else of a window
is read — so automated runs never look at the user's windows (ADR 0026 §8).

### 4. Click, `Space` and the keyboard pointer

- With nothing selected and no button held, the detected target is drawn as a selection would be
  (undimmed, the accent border, its size in the chip) without handles; a click without a drag, or
  `selection.mark` (`Space`), selects it (J2). That click is not the first press of a
  double-click.
- `pointer.move-*` are **physical** keys (`code:KeyW` …): every key message carries its logical
  chord and its physical chord from the scan code, the logical one tried first at each precedence
  level (so `E` stays `detect.cycle`), and the physical one gives `W A S D` their position on every
  layout. They move the overlay's pointer one device pixel — also during a drag, a move or a resize
  — and the real cursor follows with `SetCursorPos` only while the overlay is the foreground window
  (NOEUL-SEC-012); the test placement never moves it.
- While the keyboard moved the pointer last, **detection and snapping pause** (a corner lands
  exactly where the keys put it); the next real mouse movement resumes them. The warped cursor's own
  `WM_MOUSEMOVE` reports the same position and changes nothing. So `Space` over a highlighted
  window selects it (J2), and `Space` after `W A S D` marks a corner (J1, FLOW-18): first corner,
  then the opposite one, as a rubber band in the new `marking` phase; a click sets the second
  corner, a right-click undoes the mark, `Esc` cancels.

### 5. Acceleration and snapping

A held key's auto-repeats move 1 device pixel for the press and nine repeats, then 2, 5 and 10
([`accel`](../../crates/noeul/src/overlay.rs)), for the pointer keys, the arrows (move) and
`Ctrl`/`Shift`+arrows (grow and shrink an edge); another key starts over at 1. While dragging,
marking or resizing with the mouse, a moving edge within 6 logical pixels of a monitor edge or of an
offered window's edge (whose extent overlaps it) snaps to it; `Alt` held turns snapping off for
that movement, detection off removes the window edges, and keyboard pointing never snaps.

### 6. The magnifier

Shown while selecting (hovering, dragging, resizing, marking) and next to a confirmed selection
where a press would start or resize something; hidden while moving it, over its inside, the
toolbar or the size chip; `magnifier.toggle` (`M`) and `[capture] magnifier` turn it off. It sits
20 logical pixels right of and below the pointer on the pointer's monitor (left or above at its
edges): 15 × 15 pixels of the frozen picture at 8 logical pixels each (12 at 150 %), pixels outside
the picture dark, a one-pixel accent crosshair through the centre row and column and an outlined
centre cell, then the device-pixel position, on a scaled monitor the logical position on that
monitor (`200, 300 at 150 %`), and the pixel's colour as HEX and RGB — the captured sRGB values
([`overlay::color_lines`](../../crates/noeul/src/overlay.rs)). It is composed into the back buffer
with the dirty-rectangle scheme of ADR 0026; its text is GDI's, like the chip's.

### 7. Mixed DPI

All geometry stays in device pixels. [`dpi`](../../crates/noeul/src/dpi.rs) is the one place that
converts: lengths with integer rounding half away from zero (logical → device → logical is the
identity at every Windows scale; repeated conversions never drift), points relative to their
monitor's top-left corner, and logical rectangles of one monitor. Noeul's own drawing uses the DPI
of the monitor it is on (handles, toolbar, magnifier, snapping reach).

### 8. History of rectangles, in memory

The capture sessions keep the rectangles of the last 20 successful captures (pinned, copied or
saved; a repeat of the newest is not added; cancelled ones never), in memory only
([`Captures::history`](../../crates/noeul/src/selection.rs)). Each overlay gets them clipped to its
picture; `history.previous` (`Comma`) steps back from the newest, `history.next` (`Period`)
forward, stopping at both ends; a new drag, click or mark leaves the history. Images in the history
(pinning an earlier capture again from the tray and the CLI) and disk history remain N4
(NOEUL-FR-050, NOEUL-FR-051).

### 9. Settings and status

`config.toml` gains `[capture] detection` (`window`, default, `element`, `off`) and
`[capture] magnifier` (`on`, default, `off`), set with `noeul config set` like the others.
`noeul status` reports `captures.history` and, for the open overlay, `pointer`, `detection`
(`mode`, `windows`, `target`, `protected`, `fallback`, `suspended`), `magnifier` (`enabled`,
`shown`), `history` (`count`, `position`) and the `marking` phase; the IPC schema and its examples
change in the same commit, with invalid examples for an element name and a colour in `status`.

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, one monitor at 100 %, release build,
2026-11-04, other workers building on the same machine.
`cargo run --release -p noeul --example detect_measure -- --runs 50` measures on this process's own
off-screen windows only:

| What | Setting | p50 | p95 |
| --- | --- | --- | --- |
| Window list (EnumWindows + facts of the offered windows) | 100 own windows | 1.83 ms | 2.22 ms |
| Window list | 300 own windows | 4.70 ms | 5.23 ms |
| One element lookup, request to answer (off the UI thread) | 8 child buttons | 7.36 ms | 10.59 ms |
| One element lookup | 40 child buttons | 23.71 ms | 30.07 ms |

The list is taken on the first-frame path; at about 15 µs per offered window it adds a few
milliseconds to NOEUL-NFR-003's budget for an ordinary desktop. Element lookups never block the
overlay: the window rectangle shows until the answer. Idle cost is unchanged: no thread, list or
COM object exists while no overlay is open.

## Consequences

- Hovering is free of cross-process calls in window mode; element mode costs one worker thread per
  capture, and UI Automation clients make some programs (Chromium) turn their accessibility tree
  on while the capture lasts (*documented* behaviour of those programs).
- The window list can be stale for a program that changes after `F1`; it always matches the frozen
  picture, which is what gets pinned.
- `E`, `M`, `Space`, `W A S D`, `Comma` and `Period` are now bound in the overlay; they shadow
  nothing outside it. The final manifest's IDs and defaults are unchanged.
- `noeul.exe` imports `dwmapi.dll` (mapped, not private memory); `UIAutomationCore.dll` is loaded
  only after `E`.

## Alternatives considered

- **`WindowFromPoint` / `ElementFromPoint` on every move:** both see the full-screen overlay first,
  query the live desktop instead of the frozen picture, and make every pointer move a
  cross-process call; rejected.
- **Hiding or making the overlay transparent to hit-testing for each query:** flicker and races
  with the user's pointer; rejected.
- **UI Automation on the UI thread with a timeout:** a hung program would still freeze the overlay
  for the timeout on every move; rejected.
- **Reading class names to exclude desktop or shell windows:** text, however technical; the
  geometry rules suffice (a desktop window is simply the bottom-most rectangle).
- **`Space` always marking a corner** (accept detection only by click): no keyboard path for J2;
  **`Space` always accepting detection**: J1 would need `E E` first. The keyboard-pointer pause
  gives both without a mode switch.
- **Snapping on by default with `Ctrl` to disable:** `Ctrl` already selects key bindings in the
  overlay; `Alt` is the Windows convention for overriding snap-to-grid.
- **History on disk by default:** against NOEUL-SEC-005; it stays opt-in N4.
- **The `windows` crate for COM:** a large new dependency for six interface calls; rejected for
  raw vtables documented slot by slot.

## Not tested here (and why)

- **Real hovered programs on the visible desktop** (browsers, Office, UWP, Explorer), the visible
  highlight and magnifier, the real cursor following the pointer keys, and snapping by hand:
  automated runs must not show windows on the maintainer's desktop, synthesize input or move the
  cursor. Detection and UI Automation are verified against Noeul's tests' own off-screen windows
  (a plain window with buttons, layered, click-through, capture-excluded, `WDA_MONITOR`, hidden and
  DPI-unaware ones), in-process and across processes through the real binary; the manual script is
  in [NOEUL_UX_SPEC §4.8](../NOEUL_UX_SPEC.md#48-step-52-manual-check-of-smart-selection-and-precision).
- **Elevated windows (UIPI):** no elevated process can be started here without UAC; the decision
  is unit-tested with injected token states and stays a manual check.
- **Mixed-DPI and fractional-scaling hardware:** this machine has one 100 % monitor; conversions
  and the magnifier at 150 % are tested with synthetic layouts, a DPI-unaware window is read in
  device pixels only at 100 %.
- **DRM-protected video:** Windows reports no affinity for it; it is captured as delivered
  (black) and detected as an ordinary window — documented, not testable here.

## Requirements

`NOEUL-FR-040`…`NOEUL-FR-043`, `NOEUL-FR-050`, `NOEUL-FR-090`, `NOEUL-FR-091`, `NOEUL-FR-101`,
`NOEUL-FR-102`, `NOEUL-NFR-001`, `NOEUL-NFR-003`, `NOEUL-NFR-012`, `NOEUL-SEC-003`,
`NOEUL-SEC-004`, `NOEUL-SEC-005`, `NOEUL-SEC-007`, `NOEUL-SEC-008`, `NOEUL-SEC-012`.
