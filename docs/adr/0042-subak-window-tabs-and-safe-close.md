# ADR 0042 — Subak window tabs, the tab keys and safe close

Status: **Accepted** · Date: 2026-11-04 · Step 17 · Extends [ADR 0039](0039-subak-window-plain-win32-client.md)
(one Terminal per window) and implements the GUI rows of [ADR 0009](0009-gui-clients-and-keyboard-dispatch.md)
and [ADR 0020](0020-terminal-session-lifecycle.md) §7 · Uses the hotkey contract
([ADR 0017](0017-universal-customizable-hotkeys.md), [HOTKEY_CONTRACT §13.1](../HOTKEY_CONTRACT.md)) ·
Details, user guide and manual checks: [SUBAK_GUI.md](../SUBAK_GUI.md) §3

## Context

Step 16 built `subak-gui` as a window of exactly one Terminal that never creates or ends one,
with a chooser when several run and fixed shortcuts. Step 17 asks for a top tab bar of the
user's Terminals (state, name, active tab, close and rename, optional ordering), the exact keys
`Ctrl+Tab`, `Ctrl+Shift+Tab`, `Ctrl+T`, `Ctrl+W`, a robust distinction between closing a tab,
ending a live Terminal and removing a saved one, tabs that follow other clients, keyboard-only
use without shortcuts leaking into programs, and customizable keys. The window must stay a pure
client with no idle cost (`SUBAK-NFR-002`) and must never end a Terminal the user did not mean
(`SUBAK-FR-014`, `SUBAK-SEC-001` in spirit).

## Decision

1. **Tabs are the Host's Terminals, minus the ones hidden in this window.** Every Terminal the
   Host lists (running, starting, stopping, stopped, failed) is a tab, in the Host's order
   (creation order, SESSION_MODEL §1) unless the user moved a tab in this window. A Terminal
   another client creates appears as a new tab **without** becoming the shown one (keys never
   go to a Terminal the user did not choose); only `Ctrl+T`/`+` in this window shows the new
   one. The chooser of Step 16 is gone: without a name or a remembered Terminal, the first
   running tab opens. Hiding is per window and in memory (it ends with the window; Step 18's
   session manager owns showing and hiding across runs).
2. **One attached pane at a time.** Only the shown tab's pane is attached (interactively, at the
   window's grid). Switching sends `pane.detach {stream_id}` for the old pane, then attaches the
   new one, whose screen and history come back from the Host's snapshot (Step 13). Background
   tabs cost no traffic, no view and no memory; a program that asked for focus reports learns
   that it lost the focus before its pane is detached.
3. **Three distinct ends, each a different request** (SESSION_MODEL §4):
   - **Hide tab** — the tab's `×`, a middle click, "Hide tab" in the tab menu, `tab.hide`
     (no default key): `pane.detach` of this window only; nothing else is sent. The window's
     close button stays a detach of everything (ADR 0009, ADR 0039).
   - **Close Terminal** — `Ctrl+W` (`terminal.close`, destructive), "Close Terminal" in the tab
     menu: `terminal.kill` of the shown Terminal **by id**, first **without** `force`. An idle
     shell ends at once (ADR 0009); for a busy Terminal the Host answers `busy` (ADR 0020 §3)
     and the window asks in an in-window confirmation that names the program and the number of
     processes, with **Cancel** as the default (DESIGN_SYSTEM §3.8). Only an explicit "End
     Terminal" sends `terminal.kill {target: <id>, force: true}`; a rename or another Terminal
     meanwhile cannot redirect it, and the question disappears if the Terminal ends or another
     tab is shown. The confirmation is drawn by the window itself (no modal dialog box, which
     would also appear on the desktop in the off-screen test mode).
   - **Remove a stored Terminal** — `Ctrl+W` on a `stopped` or `failed` tab, "Remove Terminal"
     in its menu: `terminal.remove` without `force` (the Host refuses anything running). It ends
     no process, so it does not ask. On a tab whose Terminal is already gone (its last screen
     shown) `Ctrl+W` or Enter closes the tab.
4. **The keys are Subak's hotkey-contract actions.** The shipped manifest
   (`crates/subak/src/hotkeys.manifest.json`) is the contract's final Subak manifest filtered
   to what this build implements, without the `prefix` leader and its `@prefix` bindings until
   Step 23: `terminal.new` `Ctrl+T`, `terminal.close` `Ctrl+W`, `tab.next` `Ctrl+Tab`,
   `tab.previous` `Ctrl+Shift+Tab`, `terminal.rename` `Ctrl+Shift+R` (new default, beside
   `@prefix char:$`), `tab.hide`/`tab.move-left`/`tab.move-right` (no default), and the Step 16
   copy, paste, zoom and scroll keys as actions of the `terminal` context. The window layers
   `%APPDATA%\mouv\subak\hotkeys.json` over it (safe mode for a broken or newer file,
   `--no-hotkeys`, `KIM_NO_HOTKEYS`) and matches like Kim Manager: logical, `code:` and `char:`
   keys, sequences, the `press`/`release`/`hold` triggers, context before `app`. It re-reads the
   file when it gets the focus (an event, no watcher thread), so `subak hotkeys …` (same Step)
   applies without a restart. `Ctrl+Shift+R` was chosen because a terminal program keeps
   `Ctrl+R` (the shifted chord sends the same control character) and `F2` belongs to programs
   in the pane.
5. **No leakage, in the ADR 0009 order.** IME composition → the window's modal parts (the busy
   confirmation, the rename field) → the shortcuts → the program. A key that is a shortcut is
   handled before `TranslateMessage`, so it never becomes a character for the program, and its
   auto-repeat is swallowed unless the binding has `repeat: true`. Every other key reaches the
   program. `Ctrl+T` and `Ctrl+W` are shadowed program keys (the contract's
   `shadows_terminal_input`): until Step 23's `Ctrl+B` pass-through, a user gives them back to
   programs by disabling or rebinding the actions.
6. **Rename in place.** `Ctrl+Shift+R`, a double click or the menu turns the shown tab's name
   into an edit field that captures the keyboard (characters, the IME's committed text,
   Backspace/Delete/arrows/Home/End); Enter sends `terminal.rename` by id, Esc cancels. A name
   the Host refuses (`name_conflict`, `invalid_name`) keeps the field open with the reason.
7. **The tab bar is drawn in the client area** by the window's own renderer at the UI size
   (12 px, 32 px high, DESIGN_SYSTEM §3.5 colours, shapes for every state, the active tab bold
   with a lilac marker and joined to the pane, reverse video in High Contrast), with a native
   popup menu per tab (opened outside the window state's borrow; never shown by the off-screen
   test window, which reports the items instead) and the context-menu key for keyboard access.
   The tab bar integrated into the title bar (custom non-client drawing) stays Step 20 work.

## Evidence (this machine: Windows 10 Pro 22H2 19045)

- `crates/subak/tests/gui_window.rs` (debug window off-screen, real Hosts on private endpoints,
  driven by its own messages; a stand-in program plays the busy Claude session):
  `tabs_follow_the_headless_cli_and_ctrl_tab_switches_without_ending_anything`,
  `ctrl_t_creates_and_ctrl_w_closes_asking_first_when_busy_never_hitting_another_terminal`,
  `the_window_x_and_a_tab_x_only_detach_and_hidden_tabs_come_back`,
  `rename_in_place_and_keys_for_the_program_reach_it_once_a_shortcut_is_off`,
  `shortcuts_off_safe_mode_and_focus_transitions`, plus the 16 Step 16 tests updated for tabs.
- Unit tests of the model (no, one and many Terminals, cycling, create, busy kill by id,
  hiding, stored removal, other clients' changes), the key matcher, the tab bar layout and the
  confirmation; golden frames of the tab bar at 100 % and 200 %.
- Idle, debug build, off-screen, attached, 10 s: 0.0 ms CPU, 4.38 MiB private, 6 threads
  (Step 16: 4.06 MiB) — the tab bar adds one small renderer and no thread or timer.

## Consequences

- The window now creates and ends Terminals, but only on `Ctrl+T`/`Ctrl+W`/the menu, and ends a
  busy one only after an explicit answer; opening, switching, hiding and closing the window end
  nothing. ADR 0039 decision 3's "never creates, stops, kills or removes" applies to opening.
- Background tabs show their screen only once shown again (from the snapshot); a tab's
  activity or Claude state is not tracked yet (Steps 26–28).
- Until Step 23, `Ctrl+B` still goes to the program and there is no pass-through for the
  shadowed `Ctrl+T`/`Ctrl+W`.

## Not tested (and why)

- A visible window on the maintainer's desktop, real keyboard input (auto-repeat, real
  `Ctrl+Tab` through the keyboard driver, the context-menu key), the native popup menu, the
  Korean IME inside the rename field, High Contrast tabs: agents never show windows or
  synthesize input on this machine. The manual script is [SUBAK_GUI §9](../SUBAK_GUI.md).

## Alternatives considered

- **Tabs only for Terminals opened in this window** (others in a list): rejected for
  `SUBAK-FR-031` ("one tab per Terminal") and because other clients' Terminals would be
  invisible until Step 18.
- **Keeping every tab attached** (instant switching): rejected for the idle cost and the
  traffic of panes nobody looks at; the snapshot makes switching correct.
- **Asking before every `Ctrl+W`**: rejected by ADR 0009 (an idle shell closes at once); the
  Host's `busy` answer decides, so a client that checked too early cannot end busy work.
- **A Windows message box for the confirmation**: rejected; modal, unstyled, and it would appear
  on the real desktop from the off-screen test window.
- **`F2` for rename**: rejected while the pane has the keyboard (programs use `F2`).

## Requirements

`SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-033`,
`SUBAK-FR-034`, `SUBAK-FR-038`, `SUBAK-NFR-002`, `KIM-NFR-012`.
