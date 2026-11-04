# ADR 0031 — Ddalgak remaps: decided in the hook, sent by the worker, a mouse hook only on demand

Status: **Accepted** · Date: 2026-11-04 · Step 68 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
(remap semantics, the `mouse-hook` permission) and [ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md)
§1, §3, §4, §7 (no mouse hook in Step 66, who injects, held-key release) · Changes
[DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §3.3 (pass-through with a remap) and moves two step
types to later Steps · Builds on [ADR 0028](0028-ddalgak-rule-store-validation-and-reload.md)

## Context

Step 68 makes `remap` and `block` rules run. A remap is not a one-shot action: its target must go
down with the trigger, repeat with it and go up with it; the trigger's own modifiers must be
released logically while a chord trigger is held (`Ctrl+Shift+J` → `Down` must not send
`Ctrl+Shift+Down`) and restored afterwards; a dual-role key (`CapsLock` → `LCtrl`, a tap is
`Escape`) is undecided until another key, a time or the release decides it; and nothing may stay
logically pressed on any path (`DDALGAK-SEC-005`). The issue also asks for mouse-button mappings
(which need a low-level mouse hook, a permission the catalog must disclose first, ADR 0022) and
for an explicit pass-through of the original key, which RULE_MODEL §3.3 had forbidden for remaps.

ADR 0024 fixed that the hook callback only decides and pushes a small signal to the worker; it
never runs steps. Several designs fit or bend that rule.

## Decision

1. **The core decides every output event; the worker only sends them.** The allocation-free core
   keeps a fixed table of 16 *held remaps* and decides, from its key state, which trigger
   modifiers to release (exact chord triggers only; never with pass-through), which target
   modifiers and which key to press, and which to restore on release. It hands the worker
   self-contained instructions (`RemapOp::Press | Repeat | Release | Tap`) that carry the output
   keys, not rule indexes; the worker owns the injector and its ledger and carries them out with
   one `SendInput` call each. Consequences: the callback stays within its budget (measured
   ≤ 100 µs p99 with real input), a held output is released correctly even after a reload
   changed the rules (ADR 0028's open point), and the timing logic is unit-tested without
   injecting anything.
2. **Reference counts, and the user's own keys are the user's.** Two remaps holding the same
   output (key or modifier) share it: it goes down once and up when the last one lets go. A target
   the user already holds (a physical `F16`, a physical `LCtrl` under `CapsLock` → `LCtrl`) is
   neither pressed nor released by Ddalgak. A physical modifier released while a remap had
   released it logically has its up consumed (the application already saw it go up) and is not
   restored. These rules keep "consumed down, consumed up" true for outputs as well.
3. **The modifier state rules see is the application's.** A modifier whose down Ddalgak consumed
   (a remapped `RAlt`) is not held for matching; a modifier Ddalgak injected (the `LCtrl` of
   `CapsLock` → `LCtrl`) is, so a `Ctrl+J` rule fires while CapsLock is held, like a real Ctrl.
   Outputs themselves never trigger rules (own input stays invisible, RULE_MODEL §7.4): remaps do
   not chain, swaps cannot loop.
4. **An exact chord beats a widened match.** A plain-key `remap`/`block` trigger still matches with
   modifiers held (§3.2), but a rule whose trigger is exactly the held chord wins over it, whatever
   the order (`J` → `Down` does not take `Ctrl+J` from a `Ctrl+J` rule). Every `Win` chord is
   reserved under widening as well, also for lock keys and mouse buttons.
5. **Dual roles are decided in the hook thread.** The core asks for a one-shot thread timer
   (`SetTimer` without a window on the hook thread, existing only while a dual-role key is
   undecided). Another key going down presses the target first; that key's down — and later its
   repeats and up — are held back and sent on unchanged behind the target (`Signal::Replay`), so
   `CapsLock`+`C` is Ctrl+C even within the tap time. A key with its own rule decides the dual role
   and fires its rule instead. Mouse clicks never decide a dual role (they are not seen without a
   mouse trigger, and replaying a click with its position is not done); the target is pressed when
   the tap time passes, so Ctrl+click works after it. Undecided dual roles are dropped on a reload
   (no output); held ones stay.
6. **Pass-through is allowed for a remap without `tap`.** `trigger.pass_through: true` with a remap
   lets the original key reach the application and holds the target too (AutoHotkey's `~`); the
   trigger's modifiers are then not released. With `tap` it stays forbidden (a dual role chooses
   between two outputs). The schema's `holds_trigger` constraint is relaxed accordingly (additive:
   an older Ddalgak keeps such a rule inactive); the invalid fixture became
   `remap-tap-with-pass-through.json`.
7. **Mouse buttons.** Triggers: `MouseMiddle`, `MouseX1`, `MouseX2`, with modifiers, as a whole
   trigger. `MouseLeft`/`MouseRight` are never triggers, so no rule can take away clicking.
   Outputs (remaps only): all five; left and right are the logical primary and secondary buttons
   (the "swap buttons" setting is applied). Mouse-button events go through the same core as keys
   (as `VK_*BUTTON` codes a keyboard never sends). The low-level mouse hook (`WH_MOUSE_LL`) is
   installed on the hook thread **only while the engine is on and an active rule has a
   mouse-button trigger**, and removed when a reload drops the last one; its callback passes every
   move, wheel and primary/secondary button event on without looking at it. `status` and `health`
   show `engine.hook.mouse_installed`. The permission is `mouse-hook` (catalog follow-up below).
   Wheel triggers are left to Step 71.
8. **Release on every path the engine can see.** Pause, stop, session lock and profile switches:
   the hook thread drops its remaps and tells the worker `ReleaseAll` (ordered after every press it
   decided before) and restores the user's trigger modifiers that are still physically held. A
   desktop switch, an unlock and **an elevated window taking focus** (whose keys never reach the
   hook, so a trigger-up would be lost) forget the key state and release everything. The
   emergency chord, the worker's `Drop` and the panic hook release too. A killed process cannot;
   that path stays with Step 73.
9. **The debug test host never injects.** Its worker gets a backend that sends nothing, so a
   remapped key pressed on the real keyboard while an automated test runs can never type for the
   user. Real-input tests run in-process with obscure keys only.
10. **Scope.** `send_keys` moves to Step 69 (it shares the "release held modifiers, type, restore"
    path with text typing) and `delay`/ordered multi-step runs to Step 78; both stay inactive with
    `step_unsupported` naming that Step.

**Catalog follow-up (master; the catalog is re-signed there):** add the permission
`{ "id": "mouse-hook", "detail": "A low-level mouse hook exists only while the engine is on and one of your rules uses a mouse button as its trigger; it acts on the middle and side buttons only and records nothing", "opt_in": true }`
to the Ddalgak entry of `catalog/catalog.json`, list `mouse-hook` among the known IDs of
[CATALOG_FORMAT §2.1](../CATALOG_FORMAT.md) and in `KNOWN_PERMISSIONS` of
`crates/kim/src/catalog/mod.rs` ("Installs a low-level mouse hook"), then re-sign. Until then the
binary already behaves as disclosed here; Ddalgak is not released before D6.

## Consequences

- Output timing depends on the worker thread waking up: measured 2–3 ms from an injected trigger
  to its target's down on this machine, including the test's 2 ms polling. A burst typed faster
  than that could see a later key reach an application before an earlier remap's output; human
  typing is far slower. The dual-role replay removes the one case where order matters within the
  same press.
- The mouse hook wakes the hook thread for every mouse move while it exists: the cost of a
  mouse-button rule, paid only while one is enabled and the engine is on.
- Behavior with the Korean IME (`HangulMode` as an output), AltGr layouts (`AltGr` is sent as Right
  Alt), elevated windows (the release when one takes focus is sent after the focus moved and may
  be refused by UIPI) and real mouse buttons is **not tested** on this machine; DDALGAK_ENGINE §11
  lists the manual checks.

## Alternatives considered

- **`SendInput` inside the hook callback.** Strict ordering with later keys, but a blocking system
  call in the callback against ADR 0024 §3, and output decisions would still need the worker for
  timers. Rejected.
- **The worker times dual roles.** The worker would press the target on its own clock while the
  core decides interrupts on its own: two owners of one decision, and a race on every interrupt.
  Rejected for the hook-thread timer.
- **Rule indexes in the signals.** Smaller, but a reload in the middle of a hold would release the
  wrong key or none. Rejected.
- **Pressing every target and modifier unconditionally.** Simple, but releasing a key the user
  still holds or another remap holds leaves applications with unmatched ups. Rejected for
  reference counts.
- **Left and right buttons as triggers.** AutoHotkey allows them; a wrong rule could make the mouse
  unusable until the engine is turned off. Not in v1.
- **Keep pass-through forbidden for remaps.** The issue asks for it; with the trigger's modifiers
  left alone and dual roles excluded it pairs correctly. Allowed.

## Requirements

`DDALGAK-FR-008`, `DDALGAK-FR-010`, `DDALGAK-FR-020`, `DDALGAK-FR-060`, `DDALGAK-FR-071`,
`DDALGAK-NFR-001`, `DDALGAK-NFR-002`, `DDALGAK-NFR-005`, `DDALGAK-NFR-008`, `DDALGAK-SEC-003`,
`DDALGAK-SEC-004`, `DDALGAK-SEC-005`, `DDALGAK-SEC-007`, `DDALGAK-SEC-009`, `DDALGAK-SEC-010`.
