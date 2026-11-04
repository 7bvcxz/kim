# ADR 0046 — Ddalgak enable, disable and recovery: a remembered turn-off, scoped `enabled`, a lost hook turns the engine off, stale keys released on demand

Status: **Accepted** (validated in Step 73 by automated tests; the physical emergency chord, a
real lock, sign-out, reboot and stuck key are manual checks for the maintainer) · Date:
2026-11-04 · Step 73 · Refines [ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md)
§4 (start policy, held keys after a kill) and §8 (hook health) and
[ADR 0043](0043-ddalgak-modes-layers-and-runtime-mode-state.md) (modes) · Changes
[DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §2, §7.3, §8, §13 (additive `enabled` of profiles
and modes, `settings.floating_indicator`, `profile_disabled`, `mode_disabled`, the `enable`,
`disable` and `release` commands) and [DDALGAK_SAFETY](../DDALGAK_SAFETY.md) §4, §7, §10

## Context

Step 73 (issue #78) asks for explicit enable/disable and a temporary pause, bound to a
configurable shortcut, the tray, the GUI and the CLI, optionally per mode, profile and rule; an
emergency gesture that works whatever the rules say; disabling that releases everything
deterministically; a status that is obvious without overlays and without polling; and recovery
from a crash, a lock or sign-out, stuck modifiers and a taken shortcut — and never a silent
re-enable of anything the user turned off.

Much existed: the engine states, the pause, the emergency chord checked first on physical
keys (Step 66), the release ledger (Step 68), rule `enabled` (Step 67), modes (Step 72), the
`engine.toggle`/`engine.pause` shortcuts with `taken` reporting. What was missing were decisions
about persistence (what survives a restart), scope (what "disable a mode or profile" means),
the silent hook removal of Windows, and the keys a killed process leaves pressed.

## Decision

1. **Global enable/disable is the engine on/off**, `ddalgak enable|disable` aliases of `engine
   start|stop`; pause/resume stays the temporary switch (hook kept, engine-control rules live,
   default mode restored). **A turn-off by the user is remembered**: the tray, the CLI, a
   shortcut or a rule writes `engine.disabled` in the local data folder; the next start stays
   off (`off_reason: disabled`), even with `engine_on_start: on`, until the user turns the
   engine on (which removes it). A turn-off by the app itself (quit, the session ending, a lost
   hook) is not remembered: `engine_on_start` keeps its meaning for the normal sign-in.
2. **Scoped enable/disable lives in `rules.json`**, through the rule store (lock, revision,
   validation, backup, history, reload): rules (existing `enabled`), modes and profiles
   (additive, optional `enabled`, absent = enabled). A disabled profile runs none of its rules
   (`profile_disabled`) while every rule keeps its own `enabled`. A disabled mode keeps its rules
   limited to it alone from running (`mode_disabled`; a rule also limited to an enabled mode
   keeps working there), refuses every switch to it (rules targeting it are inactive, a `cycle`
   skips it, `mode set` is `mode_disabled`, exit 7), is not offered in the tray menu, and a
   reload that disables the active mode returns to the default mode. The default mode is never
   disabled (`mode_invalid`, ignored). Nothing but the user changes these fields.
3. **Disabling releases deterministically.** The engine off, a pause, the emergency chord and a
   lock release everything at once (the worker's ledger, the core's remaps and motions). A
   scoped disable is a reload: new presses no longer match, and a key held at that moment
   *finishes* with its trigger's up (its output goes up with it), so an application never sees
   half a key press and the user's held key is not cut off under their finger.
4. **The emergency chord stays outside the rule table.** It is decided first in the callback on
   physical key state, with an empty table (an invalid file never loads), conflicting remaps of
   its own keys, a pause, a password field or the secure-desktop flag; tested in the core with
   every key of the chord remapped or blocked, in four orders. It is not re-assigned: Step 73
   keeps `Ctrl+Alt+Shift+Escape` provisional until the maintainer's physical check (the
   documented interaction with Task Manager's `Ctrl+Shift+Escape` is in DDALGAK_ENGINE §16).
5. **A lost hook turns the engine off.** Windows passes an event on when a low-level hook does
   not answer within `LowLevelHooksTimeout` and, from Windows 7 on, removes the hook silently
   with no way for the program to know (*documented*). The callback measures itself and the
   queueing delay of *physical* events (an injected event's timestamp is its sender's); one that
   reaches the threshold — the user's `LowLevelHooksTimeout` capped at 1000 ms (*documented*
   cap), or 300 ms when the value is absent (the hook documentation does not state a default) —
   posts one message to the app, which turns the engine off (`off_reason: hook_lost`), releases
   everything, strikes the tray icon through and says why. It never reinstalls a hook by itself;
   turning the engine on installs a fresh one. A false alarm costs one turn-off with a notice,
   which is the safe direction. The threshold is read once per hook; a debug-only variable lets
   the test host lower it so the whole path is tested with one ordinary callback.
6. **Stale keys are released when it is safe to know.** After an unclean exit (the run marker
   was left) the next start reads `GetAsyncKeyState` once for the keys Ddalgak could have
   pressed (the eight modifiers and every remap or held-button output of the active rules) and
   sends an up, marked as its own, for each one Windows reports down; Alt and Win get a masking
   key first. `ddalgak release` and the tray's *Release stuck keys* do the same on request,
   leaving Ddalgak's own ledger to the worker, skipping modifiers the hook saw physically
   pressed, and making the hook forget its key state. A key the user really holds while the
   engine is off may get an up; its auto-repeat presses it again. The test host may release only
   `F13`–`F24`. Polling `GetAsyncKeyState` stays rejected (ADR 0024 §1): these are single reads
   at moments the user or a crash defines.
7. **The status is a model, drawn by the tray now and the editor later.** `indicator::build`
   turns the engine state, the off reason, the mode indicator, the profile's `enabled`, the
   session lock, a kept-previous reload and a taken shortcut into a shape (ring, struck ring,
   disc, notched disc, bars), a tooltip in words and an attention code; `status.indicator`
   returns it. The floating badge is opt-in (`settings.floating_indicator`: `off` default,
   `paused`, `always`) and drawn by Step 74. Everything is rebuilt on events; nothing polls.
8. **Session events never change the engine state.** A lock or disconnect releases held keys
   and makes rules wait (`skipped_secure_desktop`); the unlock makes the hook forget its key
   state; the engine stays running or paused, so an unlock never resumes what the user paused.
   `WM_ENDSESSION` stops the engine cleanly (no run marker, no `engine.disabled`).
9. **A taken shortcut is said once and never taken.** The app shows one notice at start and
   `taken` in `status`, `health` and the indicator; it never retries a registration.

## Consequences

- "I turned it off" survives a reboot; "it was on when I signed out" still starts on with
  `engine_on_start: on`. A user who wants a shortcut-off to last only for the session uses pause.
- Rules documents gain two optional fields and a setting (additive; v1 kept). An older Ddalgak
  would ignore `enabled` on profiles and modes; no released Ddalgak reads v1 files today.
- The hook-loss check adds one atomic compare per callback; the idle cost is unchanged (tested:
  no hook or worker wake-up over 2 s off, running, paused and locked).
- Automated tests prove the logic, the real hook path with injected obscure keys, a killed test
  host and synthetic session messages; the physical chord, a real lock, sign-out, reboot and a
  real stuck modifier remain manual (DDALGAK_ENGINE §16).

## Alternatives considered

- **A persisted global "disabled" flag in `rules.json`**: mixes a runtime choice into the shared
  document (history, revisions, conflicts with the editor). Rejected for a local marker beside
  the run and emergency markers.
- **Not remembering the turn-off** (`engine_on_start: on` always wins): a user who turned
  Ddalgak off after it misbehaved would find it on after a reboot. Rejected (DDALGAK_SAFETY §4).
- **Releasing held outputs at once on a scoped disable**: cuts a key off under the user's
  finger and sends an up the application did not expect. Rejected; the finish-on-up path of the
  Step 67 reload is deterministic.
- **An active hook probe** (inject a marked key and check the hook saw it): injects input on
  the user's desktop and was already rejected by ADR 0024 §8.
- **Reinstalling a lost hook automatically**: hides a real problem and could loop. Rejected.
- **Changing the engine state to paused on lock**: an unlock would then have to resume by
  itself, a silent re-enable. Rejected.
- **Recording held keys on disk for crash recovery**: a disk write per remap press and a record
  of key use. Rejected for one read of the key state at the next start.

## Requirements

`DDALGAK-FR-002`, `DDALGAK-FR-003`, `DDALGAK-FR-004`, `DDALGAK-FR-016`, `DDALGAK-FR-017`,
`DDALGAK-NFR-001`, `DDALGAK-NFR-002`, `DDALGAK-SEC-003`, `DDALGAK-SEC-005`, `DDALGAK-SEC-010`,
`KIM-SEC-020`, `KIM-SEC-023`.
