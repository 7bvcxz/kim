# ADR 0040 — Ddalgak mouse actions: physical desktop pixels, normalized absolute moves, holds on the remap ledger

Status: **Accepted** · Date: 2026-11-04 · Step 71 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
(mouse steps as declarative actions) · Changes [DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §5.7
(coordinates, clamping, new optional `unit`, `grid` and stepped `while_held`), §5.8, §5.9, §11, §12 ·
Builds on [ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md),
[ADR 0031](0031-ddalgak-remap-execution-and-mouse-hook.md) (the core's reference-counted remap
slots and the worker's ledger), [ADR 0034](0034-ddalgak-typing-runs-hotstrings-and-clipboard.md)
(worker runs and their checks)

## Context

Step 71 (issue #76) makes `mouse_move`, `mouse_scroll` and `mouse_click` run: moves in four
directions in pixels or user units, relative and absolute positioning, vertical and horizontal
wheel, click, double click and hold of five buttons, key-to-pointer navigation with speed,
acceleration and a repeat rate, a virtual grid, multiple monitors with negative coordinates and
per-monitor DPI, and no stuck synthetic button on any way out. The rule model already fixed the
step shapes (§5.7–§5.9) but not the coordinate system across monitors of different scales, how a
"relative" move avoids Windows' pointer acceleration, what "clamped to the visible desktop" means
between monitors, how a move or a button lasts exactly as long as its trigger, and how all of this
can be tested on the maintainer's machine without ever moving the real pointer.

## Decision

1. **The engine works in physical virtual-desktop pixels.** Every position Ddalgak reads (the
   monitors and their effective DPI, the pointer, the active window's content area, the window at
   a point) is read with the calling thread in per-monitor DPI awareness v2
   (`SetThreadDpiAwarenessContext`, restored afterwards), and a call that moves the pointer is
   sent in the same mode. The app's own DPI mode is unchanged: only these calls switch.
2. **Rule coordinates are logical pixels of the monitor they refer to.** A relative `dx`/`dy` is
   scaled by the DPI of the monitor the pointer is on; a `primary_monitor` point by the primary
   monitor's; an `active_window` point by the window's DPI, from the top-left corner of its
   content (client) area. A `virtual_screen` point is a **physical desktop pixel**: a point that
   may lie on any of several monitors with different scales has no single logical size, so the
   wording "logical pixels scaled by the target monitor's DPI" of §5.7 no longer applies to it.
   Rounding is half away from zero.
3. **Every move is an absolute move to a checked pixel.** Ddalgak sends `MOUSEEVENTF_MOVE |
   MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` with coordinates normalized over the virtual
   desktop (0–65535), choosing the middle of the pixel's interval so either rounding Windows may
   apply lands on that pixel (unit-tested for every pixel of several layouts against the mapping
   `left + n × width / 65536`). A relative move is computed from the pointer position read just
   before. Windows' relative mouse input (`MOUSEEVENTF_MOVE` alone) is not used: it passes
   through the mouse speed and "enhance pointer precision" settings, so the distance would not be
   the rule's, and it cannot be kept on the desktop. **Known limit**, documented and not
   worked around: programs that read raw relative mouse input (many games with a captured
   pointer) may ignore absolute moves; Ddalgak makes no game or anti-cheat promise.
4. **Never outside the desktop.** The visible desktop is the union of the monitor rectangles, not
   their bounding box. An absolute target outside every monitor lands on the nearest pixel of a
   monitor (`clamped: true`). A relative move — a nudge, a step, a tick of a continuous motion —
   travels from the pointer towards its target and stops at the last pixel before its path
   leaves the desktop, as a physical mouse stops at an edge with no monitor behind it; it crosses
   from one monitor onto an adjacent one. A grid step stays on the grid of the pointer's monitor.
   Normalized coordinates are clamped to 0–65535 in any case.
5. **A held button is a remap on the existing ledger.** `mouse_click` with `op: hold` compiles to
   a remap slot whose target is the button (`Output::button`), so it shares the core's reference
   counts with remaps that hold the same button, releases the trigger's modifiers while held and
   restores them, never auto-repeats, keeps the button through a reload until its trigger goes
   up, and is released by the worker on pause, stop, session lock, a desktop switch, an elevated
   window taking focus, the emergency chord, the worker's end and the panic hook — the paths of
   `DDALGAK-SEC-005` that ADR 0031 already tests. When the rule names `at`, the pointer moves
   there in the same `SendInput` call as the button's down. There is still no "button down"
   without a held trigger: a click-lock toggle is not offered (Windows' own ClickLock setting is
   the accessible alternative), because a button that stays down after its key is up is the
   stuck-button risk this design exists to prevent.
6. **A move while held is a motion the worker ticks.** The core fires the rule on the trigger's
   down and keeps a small record of the trigger key (at most eight at once; Up and Right together
   move diagonally); the trigger's up sends `MotionEnd`. Every path that releases remaps also
   sends a release-all that stops every motion, a reload stops them (the core drops its records),
   and a motion held longer than 60 s ends with `timeout`, the safety net for a trigger-up the
   hook never saw. The worker moves the pointer every 10 ms (continuous) or at the step's times
   (stepped) **only while a motion exists**, so an idle engine still has no timer
   (`DDALGAK-NFR-001`). Continuous speed is `speed × (1 + acceleration × min(t, 1 s) / 1 s)`,
   capped at 10 000 logical px/s, and a late tick moves at most 50 ms of distance, so a stalled
   worker never makes the pointer jump; fractions of a pixel are carried over.
7. **Stepped holds and a virtual grid are additive fields.** `while_held` may instead carry
   `repeat_ms` (20–2000) and `delay_ms` (0–2000, default 300): the step at once, after the delay
   again, then at that pace — a repeat rate under the user's control and independent of the
   keyboard's repeat settings (useful with Filter Keys or a slow repeat). `unit: grid` with
   `grid: { columns, rows }` (2–64) counts `dx`/`dy` in cells of the pointer's monitor and lands on
   cell centres; grid moves are steps, never continuous. An older Ddalgak keeps a rule with these
   fields inactive (`rule_unsupported`/`step_invalid`), never broader.
8. **Who receives the input is checked at the point.** Before every mouse step, every held
   button's down and every motion tick, the worker asks who would receive pointer input at the
   target: the foreground thread's capture window during a drag, otherwise the window at the
   point. A higher integrity level (UIPI drops such input silently) gives
   `skipped_elevated_target`; the secure desktop or a locked session gives
   `skipped_secure_desktop`; nothing is sent. The core's foreground check for injecting rules
   stays as the first filter. `relative_to: active_window` without a foreground window gives
   `skipped_disabled_field`. A refused button down still releases the trigger's modifiers, so its
   release restores them and sends no unmatched up.
9. **Clicks and the wheel.** A click is a down and an up of the logical button in one call
   (left and right follow "swap buttons"); a double click is two clicks in one call, so they
   arrive within the double-click time at the same point without Ddalgak waiting. The wheel sends
   one event per notch, ±120, `MOUSEEVENTF_WHEEL` (positive away from the user) or
   `MOUSEEVENTF_HWHEEL` (positive to the right), so applications that handle one notch per
   message see each.
10. **Mouse steps run together, in order; mixing waits for Step 78.** A rule made only of
    non-held mouse steps runs them at once on the worker (one call per step; `on_error: stop`
    ends at the first failure). Mouse steps with other step types, and a hotstring with a mouse
    step, stay `step_unsupported` until ordered multi-step runs (Step 78).
11. **No pointer tracking and no mouse hook.** Mouse actions read positions only when a rule runs
    and never install the low-level mouse hook, which still exists only for mouse-button triggers
    (ADR 0031); `status` keeps reporting `engine.hook.mouse_installed: false` for mouse-action
    rules.
12. **Safe by construction in tests.** A `SimPointer` is both a screen and an injection backend
    over a pretend cursor: the debug test host, the playground preview (`mouse::Playground`, the
    "safe cursor-position preview" of GUI_JOURNEYS J5) and every test use it. Automated tests
    build the real `INPUT` structures and compare them field by field but never send a mouse
    event; the only real input in the real-hook smoke test is the obscure keys F13–F15.
13. **Wheel triggers are not part of Step 71.** Wheel notches as triggers (AutoHotkey's
    `WheelUp::`, matrix row 1.16) would make the mouse hook consume wheel events for matching
    chords — a separate risk on a shared machine and not in issue #76. They stay planned without
    an owner Step; the matrix says so.

## Consequences

- Rules move the pointer to exactly the pixel they describe on every monitor arrangement and
  scale, cannot send it off the desktop, and never leave a synthetic button down on any path the
  engine knows of. A killed process still cannot release anything (Step 73).
- One additive change to `ddalgak-rules-v1` (`unit`, `grid`, `while_held.repeat_ms`,
  `while_held.delay_ms`) and to `ddalgak-simulation-v1` (`unit`, `repeat_ms` in the move
  preview); new example `14-mouse-navigation.json`. No new outcome or error code.
- The real behavior on the maintainer's monitors, scales, games and administrator windows is a
  manual check (DDALGAK_ENGINE §14): automated tests may not move the pointer of this machine.

## Alternatives considered

- **Relative `SendInput` moves** (`MOUSEEVENTF_MOVE` without `ABSOLUTE`): simplest and what some
  games expect, but subject to pointer acceleration and unbounded; rejected as the default. A
  per-rule raw mode could be added later behind its own field if users need it for games.
- **Logical virtual-screen coordinates** (scale the whole point by "the" DPI): ambiguous with
  mixed scales; rejected for physical desktop pixels.
- **Clamping to the bounding box of all monitors**: lets the pointer target dead zones between
  monitors of different sizes; rejected for the union of monitors.
- **A separate hold mechanism for buttons**: would duplicate the reference counts and release
  paths that remaps already prove; rejected.
- **A process-wide per-monitor DPI manifest for `ddalgak-gui`**: would change every coordinate the
  tray and future editor see; the per-call thread scope is narrower.
