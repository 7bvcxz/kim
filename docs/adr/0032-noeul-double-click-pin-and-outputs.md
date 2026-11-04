# ADR 0032 — Noeul double-click to pin: press counting, the toolbar, separate outputs and gesture settings

Status: **Accepted** (Step 51) · Date: 2026-11-04 · Builds on
[ADR 0021](0021-noeul-product-definition.md) (§1–§3, §5), [ADR 0026](0026-noeul-capture-overlay.md)
(§4, §5, §8) and [ADR 0029](0029-noeul-pin-engine.md) (§1, §3)

## Context

Step 51 (issue #56) implements Noeul's signature flow exactly as the maintainer asked: `F1`, drag
a rectangle, **double-click inside it**, and that rectangle floats at once as an always-on-top
picture — no dialog, no extra key, no copy. ADR 0021 fixed the product rules and
[NOEUL_UX_SPEC §4](../NOEUL_UX_SPEC.md#4-the-signature-flow-precise-rules) the 13 rules and 38
test cases; ADR 0026 left the gesture, the toolbar, the selection's menu, copy and save to this
Step, and ADR 0029 provided the entry point (`App::pin_selection` with a guard time). What
remained open were implementation decisions: how presses are counted, what exactly the toolbar
does with the keyboard, how the outputs fail, where the gesture setting and the opt-in
"Snipaste-style gestures" preset live and how they are changed, and how automated runs stay off
the maintainer's desktop, clipboard and Pictures folder.

## Decision

### 1. Noeul counts the presses itself, with the user's values read before every press

The overlay model ([`overlay::Overlay`](../../crates/noeul/src/overlay.rs)) remembers the first
press P1 only when it is a plain left press (no `Ctrl`, `Shift`, `Alt` or `Win`) in the
**interior** of a confirmed selection and is released before the pointer left the drag rectangle.
A second plain left press P2 in the interior completes the double-click when
`P2.time − P1.time ≤ double-click time` (message times, wrapping) and P2 lies within half the
double-click rectangle of P1 on each axis. The output fires on P2's **press**. Every other press
— on a handle, the border band, the toolbar or the size chip where it covers the selection,
outside, with a modifier, a right or middle press, a key between the clicks — ends the wait, and
a press that became a drag, move or resize never becomes P1 (UX spec §4.2 rules 1–5). The window
class has no `CS_DBLCLKS`; the owner refreshes the double-click time and rectangle and the drag
threshold from Windows before every press, so a change in the Windows settings applies from the
next press (rule 13) without listening for `WM_SETTINGCHANGE`.

A single click inside a selection has no action in this build, so a click never waits to be told
apart from a double-click: there is no timer and no delayed action.

### 2. The output, its guard and its failure

The model reports `Output(Pin | Copy | Save | QuickSave, rect, via)`; `via` is the key, the
double-click, the middle-click, the toolbar or the menu.

- **Pin** goes through `App::pin_selection` (no clipboard, no file). A pin made by a mouse gesture
  (double-click, middle-click, toolbar) is created with `guard` = the double-click time, so the
  third and fourth press of a quadruple click are swallowed by the new pin (ADR 0029 §1).
- **Copy** encodes the selection in memory as PNG (8-bit RGB when opaque, RGBA otherwise) and as a
  packed `CF_DIBV5` (bottom-up, `BI_BITFIELDS`, sRGB) and hands both to the shell's clipboard
  writer, which opens the clipboard only for the handover (owner: the control window; retried for
  500 ms) and adds `CanUploadToCloudClipboard` = 0 — NOEUL-FR-034's privacy default, whose setting
  is still N2.
- **Save** asks the runtime for the system save dialog (`GetSaveFileNameW`, owned by the overlay,
  starting in the quick-save folder with `noeul-YYYYMMDD-HHMMSS.png`, asking before replacing); the
  dialog is modal, so it is shown with the core released, like the pin menu (ADR 0029 §3). Only
  the chosen file is written; cancelling leaves the overlay as it was.
- **Quick save** writes a new `noeul-YYYYMMDD-HHMMSS.png` (then `-2`, `-3`, …) in
  `[capture] quick_save_folder`, default the user's Pictures folder + `Noeul`; it claims the name
  atomically and never overwrites (NOEUL-FR-032).

A focused pin offers the same outputs for its own picture at 100 %, whatever its zoom —
`pin.copy` (`Ctrl+C`), `pin.save` (`Ctrl+S`, the dialog owned by the pin) and `pin.quick-save`
(`Ctrl+Shift+S`) — and its menu lists Copy, Save… and Quick save (UX spec §3.3); a failure there
is a notice and never closes the pin.

Saved PNGs hold `IHDR`, `IDAT` and `IEND` only: no text, time, program or window metadata
(NOEUL-FR-031, NOEUL-SEC-006). Every failure (a pin window that cannot be created, a busy
clipboard, an unwritable folder, no quick-save folder) keeps the overlay and its selection with a
one-line banner; a successful output ends the session and frees the frozen picture. Pinning never
calls the clipboard, so a disabled or held clipboard cannot affect it (NOEUL-FR-025).

### 3. The toolbar and the keyboard focus rule

The toolbar holds this build's outputs only: **Pin** (first, accent-filled, primary), Copy, Save
and Cancel (UX spec §3.1, §5); the annotation tools, undo and redo join it with Step 56. It sits
right-aligned 6 logical pixels below the selection, above it when the monitor has no room below
(above the size chip if they would meet), and inside the selection's bottom edge when there is
room neither below nor above (a selection filling the monitor). Button widths are fixed logical
sizes, so hit-testing needs no text measurement; the platform draws and centers the labels. It is
hidden while a drag, move or resize is in progress.

- A button runs on **release** over itself; `Shift`+Save quick-saves; a press on the bar is never
  a click of the double-click gesture and never starts a selection.
- `Tab` and `Shift+Tab` move the keyboard focus from the canvas through the buttons and back; the
  arrows move between buttons. While a button has focus the navigation keys go to it **first**:
  `Enter` or `Space` runs it (so `Enter` on Copy copies, ADR 0021 §5), and `Escape` cancels the
  capture, as it does everywhere in the overlay — a button has nothing of its own to cancel, and
  `Escape` must always leave (NOEUL-SEC-013). Any other key keeps its overlay binding. A press in
  the canvas gives the focus back to the canvas. `noeul status` reports the focus.

The selection's context menu (right-click inside, `Shift+F10`, the menu key; UX spec §3.2) lists
Pin, Copy, Save…, Quick save, Select monitor, Last rectangle and Cancel with their effective keys,
and is shown with the core released like the pin menu.

### 4. The gesture settings and the preset

`config.toml` gains `[gestures] double_click` (`pin`, default, `copy`, `save`, `none`),
`[gestures] middle_click` (`pin`, default, `copy`, `none`), `[gestures] preset` (`noeul`, default,
`snipaste`) and `[capture] quick_save_folder` (an absolute folder). They are read at start, by the
settings-folder watcher that already reloads `hotkeys.json`, and on IPC `settings.reload`, and
apply from the next capture. `capture.pin` keeps its own keys whatever the gesture does.

`noeul config set <key> <value>` and `noeul config reset [general|capture|gestures] --yes` (both
with `--dry-run`) are the settings' first writers. They edit only the lines of the changed keys
in place; the edited text is parsed again and must differ from the old one in exactly those keys,
otherwise nothing is written (`config_invalid`); an invalid or newer file is never rewritten
(`config_invalid`, `config_from_newer_version`). This is the "edit keys in place" promise of the
settings module, without a TOML writer dependency.

The **Snipaste-style gestures** preset follows the HOTKEY_CONTRACT precedent of a preset that
writes overrides ("terminal-first mode", §13.1): `config set gestures.preset snipaste` writes
`preset = "snipaste"`, `double_click = "copy"`, `middle_click = "pin"` and, through the same
editing path as `noeul hotkeys set` (refusing a conflict), the overlay bindings `capture.pin` →
`Ctrl+T` and `capture.copy` → `Ctrl+C`, `Enter` into `hotkeys.json`. It is shown by name in
`noeul config show` and `noeul hotkeys list`; `config set gestures.preset noeul` or
`config reset gestures` undo both, resetting a preset binding only while it is still exactly the
preset's. Because the preset's keys are ordinary user overrides, every tool's conflict check
(`kim hotkeys check --all-tools`) sees them.

### 5. Automated runs stay off the user's things

Under the debug-build test hook (ADR 0026 §8) Copy writes to an in-memory stand-in (never the
clipboard; `NOEUL_DEV_CLIPBOARD=busy` makes it fail like a held clipboard), the selection's menu
is recorded instead of shown, the save dialog is never shown — the answer is a new file in an
explicitly configured quick-save folder, or "cancelled" — and quick save never falls back to the
user's Pictures folder. Release builds contain none of this. Tests drive the real binary's
off-screen overlay with messages posted to its own window; posted key messages cannot carry
modifiers, so those tests bind extra plain-letter keys in their sandbox's `hotkeys.json`.

### 6. PNG through the `png` crate

PNG is encoded with `png` 0.18.1 (MIT/Apache-2.0), already in the workspace lock with all its
dependencies (`fdeflate`, `flate2`, `miniz_oxide`, `crc32fast`), with the fast compression level.
A hand-written stored-deflate encoder would have avoided the dependency but made every saved and
copied picture as large as its raw pixels.

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, release build, 2026-11-04.
`cargo run --release -p noeul --example overlay_measure -- --double-click --runs 50` runs the
instance core's real path on a synthetic off-screen monitor of the selection's size: overlay
window, a drag over the monitor, a first click, then the time from handing the **second press**
to the core until it returns — the selection cut from the frozen picture, the pin window
created, rendered and shown (off-screen, never activated, not topmost), then the overlay window
destroyed and the frozen picture freed (so the numbers include the teardown after the pin is
already visible).

| Selection | Run 1 p50 / p95 | Run 2 p50 / p95 | Budget (NOEUL-NFR-004) |
| --- | --- | --- | --- |
| 1920×1080 | 10.6 / 14.5 ms | 10.7 / 11.8 ms | p95 ≤ 50 ms |
| 3840×2160 | 46.5 / 99.2 ms | 39.2 / 67.5 ms | p95 ≤ 100 ms |

Run 1 ran while other workers built Rust code on the same machine. DWM shows the pin at its next
composition, at most one refresh later (*documented*). The idle cost is unchanged: Step 51 adds
no timer, thread or hook; the overlay still exists only while a capture is open.

## Consequences

- The signature flow works on real input exactly as the spec defines it; its visible behaviour on
  the maintainer's desktop still needs the manual script of
  [NOEUL_UX_SPEC §4.7](../NOEUL_UX_SPEC.md#47-step-51-manual-check-of-the-signature-flow).
- `noeul.exe` imports `comdlg32.dll` for the save dialog (mapped, not private memory; no change to
  the idle cost) and grows by the PNG encoder.
- `Ctrl+C`, `Ctrl+S` and `Ctrl+Shift+S` are now bound in the overlay; they shadow nothing outside it.
- A user who rebinds `capture.copy` after turning the preset on keeps that binding when the preset
  is turned off.

## Alternatives considered

- **Windows' own double-click messages (`CS_DBLCLKS`, `WM_LBUTTONDBLCLK`)**: they cannot express
  the interior-only rule, cannot exclude a press that started a move, and the first press of a
  pair would already have started a move; rejected, as the spec says.
- **Firing the pin on P2's release**: slower and lets the release drag the new pin; rejected (ADR
  0021).
- **`Escape` on a focused toolbar button returning the focus to the canvas**: two presses to
  leave, against "`Escape` always leaves"; rejected.
- **The preset as a hidden layer between the manifest defaults and the user layer**: invisible to
  `kim hotkeys check --all-tools`, which reads only the manifest and `hotkeys.json`; rejected for
  ordinary overrides.
- **Re-reading `config.toml` at every `F1`**: a file read on the latency path of every capture;
  rejected for the watcher plus `settings.reload`.
- **A debug-only IPC method accepting synthetic pointer events** (UX spec §4.4's original e2e
  plan): posting messages to Noeul's own window drives the same window procedure with no new
  surface in the protocol; rejected.
- **Showing the save dialog under the test hook**: it would appear on the maintainer's screen;
  rejected.

## Not tested here (and why)

- The real `F1`, the visible overlay and toolbar, a physical double-click, the pin staying above
  other programs after the overlay closed, focus, the save dialog, and pasting the copied picture
  into other programs: automated runs must not show windows on the maintainer's desktop, register
  `F1`, synthesize input or touch the clipboard. The real clipboard handover
  (`win::clipboard::write_image`) and `GetSaveFileNameW` therefore run only in the manual script;
  their inputs (the PNG and `CF_DIBV5` bytes, the file names, the save request) are tested.
- Real mixed-DPI and multi-monitor hardware: tested with synthetic layouts (100 % and 150 %, a
  selection across the seam) in the model, the instance core and the real off-screen window.

## Requirements

`NOEUL-FR-020`…`NOEUL-FR-026`, `NOEUL-FR-030`…`NOEUL-FR-032`, `NOEUL-FR-034`, `NOEUL-FR-090`,
`NOEUL-FR-100`, `NOEUL-FR-102`, `NOEUL-FR-114`, `NOEUL-NFR-012`, `NOEUL-SEC-005`,
`NOEUL-SEC-006`, `NOEUL-SEC-012`, `NOEUL-SEC-013`.
