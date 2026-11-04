# ADR 0043 — Ddalgak modes: layers over a parent, runtime mode state in the hook, never restored

Status: **Accepted** · Date: 2026-11-04 · Step 72 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
(modes as part of the rule model) · Changes [DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §5.10
(keys held at a switch are released, not kept), §7.1 (precedence along a layer's chain), §7.3,
§8 (`mode_cycle`, `mode_invalid`), §13 (the `mode` commands) · Builds on
[ADR 0031](0031-ddalgak-remap-execution-and-mouse-hook.md) (the core's remap slots and the
worker's ledger) and [ADR 0040](0040-ddalgak-mouse-actions.md) (held buttons and motions)

## Context

Step 72 (issue #77) makes user-defined modes work: create, rename, copy and delete them; switch,
toggle, cycle, hold and return by key; precedence when the same key means different things in
different modes; dependency checks; safe transitions; an indicator; and a decision about whether
the active mode survives a restart. Step 65 fixed the `mode` step (§5.10: `set`, `toggle`, `cycle`,
`hold`, `previous`, `default`), the `modes` field of rules and the `mode` object (`id`, `title`,
`color`), but not how layers relate, where the active mode lives at run time, what happens to keys
held when the mode changes, or what a restart does.

## Decision

1. **Two policies, one static field.** A mode without `parent` is **exclusive**: in it only its own
   rules and the rules without `modes` apply. A mode with `parent` (additive in v1) is a **layer**:
   a key without a rule of the layer itself falls back to the rules of its parent, the parent's
   parent and so on (its *chain*), then to the rules without `modes`, then to the application.
   The chain is static, so precedence is decided when the table is compiled: one precedence list
   per mode. **Exactly one mode is active**, as Step 65 said; the chain only says whose rules it
   borrows.
2. **Precedence** (§7.1 step 6.1, refined): in the active mode, rules of that mode, then of its
   parent, …, then rules without `modes`; within each, higher `priority`, then file order (Step
   77's conditions slot in after the chain distance). The nearest layer wins; nothing is
   ambiguous, and `trigger_shadowed`/`trigger_overridden` are computed per mode along the chains.
3. **Dependency checks.** A missing parent is `unknown_mode`; a parent chain that comes back to a
   mode is `mode_cycle` (both in the reference checks of `xtask/src/ddalgak.rs` and its port);
   the default mode with a parent, `one_shot` or `timeout_ms` is `mode_invalid`. The engine never
   gets broader: a broken parent is dropped (the mode becomes exclusive), and the default mode's
   exit fields are ignored. Mode commands refuse any change that introduces one of them
   (`mode_invalid`, exit 2), and `mode delete` refuses the default mode, a parent of a layer and
   a mode a rule switches to (`mode_in_use`, exit 7).
4. **The mode state lives in the hook thread's core**, next to the remap slots: a latched mode, the
   previous latched mode, up to four temporary layers held by their trigger keys, a pending
   one-shot exit and a timed mode's idle time. Mode rules fired on press, release, repeat or as a
   sequence change it **inside the hook callback**, allocation-free, so the very next key is
   decided in the new mode. Mode rules on an `on: hold` trigger (timed by the worker), the CLI
   (`mode set|toggle|reset`) and the tray reach the same state machine through a posted request
   the hook thread answers; the host keeps only a mirror for the tray. Rejected: the active mode
   as an atomic shared with the host (two writers, a race between a key and a CLI request); the
   worker deciding switches (a key typed right after the switch key would still see the old mode).
5. **Temporary layers.** `hold` pushes a layer for as long as its key is down; layers stack (a
   second held key on top) and each ends with its own key-up, in any order. A latched change
   (`set`, `toggle`, `cycle`, `previous`, `default`) while a layer is held ends every layer, and
   the layer's key-up then changes nothing: no child mode outlives the switch that replaced it
   (the "unexpected child mode" cleanup). A key reset (another desktop, unlock, an elevated window
   taking focus), a session lock and a pause end every layer too, because their key-ups may never
   reach the hook. A fifth held layer key passes unchanged.
6. **Enter and exit behaviour.** Entering or leaving a mode never injects anything and needs no
   consent. Two optional exits (additive): `one_shot` (after a switch to the mode, the up of the
   next key that is not a modifier returns to the mode before — a sticky layer for people who
   cannot hold a key) and `timeout_ms` (1–3600 s without a key press returns to the default mode;
   never while a layer is held). A timed exit always goes to the default mode, so two timed modes
   can never bounce between each other. The hook thread's one-shot timer exists only while a
   timed mode or an undecided dual role waits (idle cost unchanged).
7. **A switch releases everything the old mode held** (`DDALGAK-SEC-005`): when the active mode
   changes, the core drops its remap slots, held buttons, motions, pending taps, holds,
   sequences and the hotstring buffer, and the worker releases its whole ledger, then presses
   again the user's still-held trigger modifiers a remap had lifted. A key held across the switch
   keeps its up consumed and does nothing more. This **changes** Step 65's sentence in §5.10 ("keys
   held when the mode changes stay with the rule that pressed them"): with layers, a remap of the
   old mode staying held in the new one is exactly the stuck key the requirement forbids. Changes
   that keep the active mode (setting the mode that is already active) release nothing.
8. **Persistence and restart.** Mode definitions are stored in `rules.json` with every other rule
   field (revision, lock, backup, history). **The active mode is never stored and never
   restored.** Every engine start, every pause (the safe reset), a profile switch and the
   emergency chord begin in the default mode; a reload keeps the latched mode by ID when it still
   exists (otherwise the default mode) and ends held layers. Reasons: a non-default mode changes
   what typing keys do; after a restart, a crash, an emergency off or a reboot the indicator may
   be off and the user may not remember being in it, so the keyboard would behave unexpectedly
   with no visible cue; the default mode is the one state the user can always predict. `ddalgak
   mode reset` and the tray's "Back to <default>" are the safe reset at any time; the emergency
   chord turns the engine off and the next start is in the default mode. Rejected: an opt-in
   "restore last mode" (Step 65 left it open) — it would need the runtime state written to disk on
   every key-driven switch, and its benefit (saving one key press after a restart) does not
   outweigh a keyboard that surprises the user.
9. **The indicator is off where it is not needed.** `settings.mode_indicator` (additive): `auto`
   (default) shows the mode in the tray only for a profile with more than one mode; `tray` always;
   `off` never. When visible, the tooltip names the mode ("Ddalgak: engine on · mode N
   Navigation"), a running engine outside its default mode has its own icon shape (a disc with a
   notch), and the tray menu has a `Mode` submenu and "Back to <default>". The title is always
   shown with the icon and the color. `status` and `mode status` always show the full state. The
   compact floating indicator window and the editor's header are drawn by Steps 73 and 74 from the
   same data model (`modes::Indicator`, `modes::menu`, `mode_edit::ModeDraft`); no indicator
   window exists in this Step and none appears in tests.
10. **Not in this Step.** A bare modifier as a `hold` layer key (bare modifiers stay remaps or
    `on: release`), a dual-role layer key (tap for a key, hold for a layer), and swallowing every
    unbound typing key in a mode (Step 65 §7.3 left it to Step 72: rejected for now — a keyboard
    that types nothing until the user finds the way out is worse than writing `block` rules for
    the keys a mode should silence). Mode steps together with other steps wait for ordered runs
    (Step 78).

## Consequences

- The rules schema gains `mode.icon`, `mode.parent`, `mode.one_shot`, `mode.timeout_ms` and
  `settings.mode_indicator`, all additive: an older Ddalgak ignores them (it runs no mode steps).
- The CLI gains `mode list|show|add|edit|rename|copy|delete|set|toggle|reset|status` and the codes
  `mode_invalid` (2) and `mode_in_use` (7); `status` gains `profile.mode_state`.
- Precedence per mode costs one list per mode in the compiled table (at most 32 × 1000 indexes);
  a decision scans only the active mode's list.
- Automated tests cover the state machine, precedence, the core's releases, the worker, the CLI and
  the real hook with injected obscure keys; real typing in real applications, the real tray icon
  and its menu are manual checks (DDALGAK_ENGINE §15).
