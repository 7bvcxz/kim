# ADR 0021 — Noeul product definition: double-click pins, privacy defaults and a clean-room boundary

Status: **Accepted** · Date: 2026-11-04 · Step 48 · Implemented from Step 49 (Noeul Steps 49–64) ·
Builds on [ADR 0002](0002-independent-tool-binaries.md), [ADR 0011](0011-portability-boundary.md)
and [ADR 0017](0017-universal-customizable-hotkeys.md)

## Context

Noeul is a new Kim tool: a small native screenshot, annotation and floating-pin utility for
Windows, inspired by Snipaste. Step 47 listed it in the Kim catalog as planned, with the
permissions it will ask for (global hotkeys, screen capture, clipboard, opt-in autostart) and a
summary of its shortcuts. Before any code exists, several product decisions have to be fixed
because later Steps (49–64) build on them:

- The maintainer asked for one flow above all: **`F1`, drag, double-click inside the selection,
  and the selection is pinned at once.** Snipaste, the reference product, copies to the
  clipboard on that double-click and pins with `Ctrl+T` or a middle-click; a double-click on a
  pinned image closes it.
- Kim has no always-on processes and no shared hook (`KIM-NFR-020`, ADR 0017). Noeul may only use
  `RegisterHotKey` (HOTKEY_CONTRACT §13.3), and headless paths must never open UI (`KIM-FR-012`).
- A screenshot tool sees everything on screen, so its defaults decide how private it is.
- Noeul must reproduce Snipaste's useful ideas without copying Snipaste.

## Decision

1. **The signature gesture pins.** A double-click in the interior of a confirmed selection
   creates a pin of exactly that rectangle, at the same place and pixel size, and closes the
   overlay; it writes neither the clipboard nor a file. `Enter` (when the selection has focus),
   the toolbar's first and primary **Pin** button, the context menu and a middle-click do the
   same. Copy (`Ctrl+C`), save (`Ctrl+S`) and quick save (`Ctrl+Shift+S`) are separate actions
   that never pin. The exact rules (hit zones, the system double-click time, rectangle and drag
   threshold, presses that never count, annotation mode) are in
   [NOEUL_UX_SPEC §4.2](../NOEUL_UX_SPEC.md#42-rules), with 38 specified test cases.
2. **The gesture is configurable, the default is not negotiable silently.** The double-click
   action is a Noeul setting (`pin`, `copy`, `save`, `none`), because the hotkey contract leaves
   mouse gestures to each tool. An opt-in, named "Snipaste-style gestures" preset restores
   Snipaste's copy-on-double-click and `Ctrl+T` pin; reset undoes it.
3. **A pin survives the gesture that made it.** A double-click on a pin has no default action
   (it never closes the pin), and a new pin ignores presses for one double-click time. Closing is
   `Esc`, `Ctrl+W` or the menu. `F3` always pins the clipboard; reopening a closed pin is its own
   command.
4. **Process model.** Noeul is one per-user process that runs while the user keeps it running,
   with an optional tray icon and an opt-in start at sign-in; no service, helper or scheduled
   task. A second start hands its request to the running instance over a same-user named pipe
   with the identity checks of ADR 0005 and ADR 0019, and a plain second start opens settings.
   A CLI capture to a file or the clipboard runs in the short-lived CLI process and opens no
   window; pin commands need the running instance and never start it implicitly.
5. **Input.** Backend `register` only. The final hotkey manifest
   ([`noeul.json`](../contracts/examples/kim-hotkey-manifest-v1/noeul.json)) keeps `F1` and `F3`
   as the only global defaults and adds three contexts: `capture-overlay`, `annotate` (inner, so
   `Esc` leaves a tool before it cancels) and `pin`. Global actions without defaults (hide all
   pins, make all pins clickable, reopen closed pin, capture the active window) are added in the
   Steps that implement them, together with the catalog's shortcut summary. Two Noeul rules apply
   the contract:
   - **Navigation keys bound in a context yield to a focused control.** `Enter`, `Space`, `Tab`,
     `Esc` and arrows go to a focused toolbar button, menu or text field first, so `Enter` on
     **Copy** copies. This is how Noeul uses HOTKEY_CONTRACT §7.2 ("a context may bind navigation
     keys") without breaking keyboard navigation; a later contract revision may adopt the rule
     for every tool.
   - **The overlay and pins have no IME context** (letter keys work in Hangul mode). The text
     tool enables the IME and suspends the other contexts while a label is edited.
6. **Privacy defaults.** Memory first: disk history, auto-save, pin backup and copy-as-file are
   off until the user turns them on; saved images carry no identifying metadata; the clipboard is
   read only on paste and never watched; window detection reads geometry only; captures requested
   through the CLI or IPC are announced (setting: notify, allow silently, deny), and MCP capture
   is off until enabled; Noeul makes no network connection at all. The secure desktop, the lock
   screen and content excluded by display affinity or DRM are never captured or worked around.
7. **Capture and rendering technology are decided by measurement, later.** Step 49 compares GDI,
   DXGI Desktop Duplication and Windows.Graphics.Capture against NOEUL-NFR-002 and NOEUL-NFR-003
   and the constraints above (no elevation, no consent or border surprises, display affinity
   respected) and records its choice as a note here or in a new ADR. The idle budget (20 MB)
   means the resident process holds no capture buffer or GPU context while idle; whether the
   overlay and pins use egui with glow (ADR 0014) or plain layered windows is Step 49's and
   Step 53's measured choice.
8. **Clean-room boundary.** Noeul reproduces functionality and interaction ideas only. It never
   copies Snipaste's code, binaries, assets, text, translations or configuration formats, and
   uses the name only to refer to Snipaste. Third-party material follows the license rules of
   [NOEUL_PHILOSOPHY §10.1](../NOEUL_PHILOSOPHY.md#101-third-party-licenses).
9. **Roadmap.** Milestones N1–N7 map to Steps 49–64 ([NOEUL_REQUIREMENTS §1](../NOEUL_REQUIREMENTS.md#1-roadmap-and-milestones));
   the first public release is N6 (Step 63). Each build exposes only the actions it implements.

## Consequences

- Snipaste users must learn one different gesture. Tooltips, the first-run hint, the shortcut
  list, the parity matrix and the preset make the difference visible.
- Without a hook or polling, Noeul cannot offer Snipaste's super-snip (`Win`+drag), hot corners,
  or per-program hotkey exceptions; the requirements list them as not intended.
- Pins live in memory by default, so quitting closes them. Quitting with open pins and backup
  off asks once in the GUI and needs `--yes` from `noeul quit` (CLI contract), and says how to
  turn backup on.
- Changing Noeul's global or app actions always changes the Kim catalog's shortcut summary in the
  same change; the catalog test enforces it.
- Noeul's manifest carries 73 actions; the conflict scenarios and the library recompute it with
  no conflict of its own beyond the intended `F1` shadowing of other tools' help (scenario 02).

## Alternatives considered

- **Keep Snipaste's double-click-to-copy:** familiar to Snipaste users, but not the flow Noeul
  exists for, and every capture would overwrite the clipboard. Rejected; available as a preset.
- **Double-click pins and also copies:** a clipboard side effect on every pin, against the memory
  and clipboard rules. Rejected.
- **Pin on release of the second click, or after a short confirmation:** slower, with no benefit.
  Rejected; the pin fires on the second press.
- **Double-click on a pin closes it (Snipaste):** the tail of the creating gesture could close the
  new pin. Rejected.
- **`F3` reopens closed pins before pasting (Snipaste):** two meanings for one key. Rejected.
- **`Tab` cycles detection modes (Snipaste):** breaks keyboard navigation of the toolbar. Rejected
  for `E`.
- **History and pin backup on by default:** convenient, but screenshots of sensitive screens would
  pile up on disk silently. Rejected; opt-in.
- **An embedded browser engine for HTML cards:** heavy and a remote-content risk. Rejected for a
  small sanitizing renderer.
- **Importing Snipaste's settings:** would tie Noeul to a private format. Rejected.

## Requirements

`NOEUL-FR-002`, `NOEUL-FR-003`, `NOEUL-FR-020`…`NOEUL-FR-026`, `NOEUL-FR-065`, `NOEUL-FR-100`,
`NOEUL-FR-114`, `NOEUL-NFR-001`…`NOEUL-NFR-004`, `NOEUL-NFR-008`, `NOEUL-NFR-012`,
`NOEUL-SEC-001`…`NOEUL-SEC-013`, `KIM-FR-012`, `KIM-NFR-020`, `KIM-SEC-020`…`KIM-SEC-022`.

## Notes

- 2026-11-04 (Step 56, [ADR 0038](0038-noeul-annotation.md)): the final manifest gained seven
  `annotate` context actions for keyboard-only object editing (`annotate.delete`,
  `annotate.select-next`, `annotate.select-previous`, `annotate.bring-forward`,
  `annotate.send-backward`, `annotate.bring-to-front`, `annotate.send-to-back`), so it now carries
  80 actions; the conflict scenarios still find no conflict of Noeul's own. No global or app action
  changed, so the Kim catalog's shortcut summary is unchanged.
