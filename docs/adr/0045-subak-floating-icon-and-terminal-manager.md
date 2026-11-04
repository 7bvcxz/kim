# ADR 0045 — Subak's floating icon, tray icon and Terminal manager: a tiny process of the window's binary

Status: **Accepted** · Date: 2026-11-04 · Step 18 · Builds on [ADR 0039](0039-subak-window-plain-win32-client.md)
(the plain Win32 client) and [ADR 0042](0042-subak-window-tabs-and-safe-close.md) (tabs, safe
close) · Uses [ADR 0009](0009-gui-clients-and-keyboard-dispatch.md) (GUI clients),
[ADR 0020](0020-terminal-session-lifecycle.md) (the busy guard), the hotkey contract
([ADR 0017](0017-universal-customizable-hotkeys.md)) · Details, user guide and manual checks:
[SUBAK_GUI.md §11](../SUBAK_GUI.md)

## Context

Step 18 asks for an optional, tiny, always-on-top, draggable floating icon and a separate tray
option — both off unless the user enables them, neither duplicating an expensive UI runtime — and
a compact Terminal manager opened from the icon: create, list, open, rename and delete (delete
guarded as in Step 10), status and quick search, opening the chosen Terminal in an existing
window or a new `subak-gui`. It must handle multiple monitors, DPI, a saved position (also when
a monitor disappears), accessibility (keyboard, UI Automation names, touch and click),
pin/hide, and the Windows foreground rules (no focus stealing; `AllowSetForegroundWindow` when
opening a window). Closing the icon or the manager must never end a Host session; a headless
system has neither, and the Host and CLI never need them (`KIM-FR-012`, `SUBAK-FR-003`,
`SUBAK-FR-030`, `SUBAK-FR-032`). The Step 44 addendum asks for a state-driven icon interface
for the later watermelon art and Claude animations, with no animation loop when hidden.

The Subak window (Steps 16–17) is one top-level window per process with its state in a
thread-local; it exists only while a Terminal window is open. The icon must be there when no
window is.

## Decision

1. **A tiny process of the same binary: `subak-gui --float`.** It runs the icon, the tray icon
   and the manager on one window thread, with the window's stack — plain Win32 and GDI, no GUI
   framework, no terminal renderer, no GPU — and the window's network threads (`gui::net`) for
   one IPC connection of kind `gui`, named `subak-float`. It never attaches a pane: it lists the
   Terminals (`terminal.list` full), subscribes to `terminals` and `host`, and sends only the
   manager's `terminal.create`, `terminal.rename`, `terminal.kill` and `terminal.remove`. One
   floating UI per endpoint and session (a named mutex); a second `subak-gui --float` or
   `--manager` asks the running one (over `WM_COPYDATA`) to show itself or its manager and exits.
   *Rejected:* the icon inside every window process (it would exist only while a window does,
   and several windows would show several icons); a multi-window refactor of the window process
   (larger change, no benefit for an idle icon); a separate package (ADR 0039's reasons).
2. **Off unless the user starts it.** Nothing starts it implicitly: not the Host, not the CLI,
   not opening a window. The user runs `subak-gui --float` (or `--manager`), or chooses
   "Floating icon and Terminal manager" in a window's tab menu. `--icon`/`--tray` choose the
   presences (saved); the icon's menu turns each on or off, and turning off the last one quits.
   Starting it at sign-in is not part of this Step (no Run or Startup entry is written).
3. **Its own settings file**, `%APPDATA%\mouv\subak\float.json` (schema `subak-float-v1`):
   `icon`, `tray`, `on_top` (pinned above other windows), `manager_pinned` (the manager stays
   open when it loses the focus) and the icon's `position` with its monitor's DPI. A file of its
   own because the window rewrites `gui.json` from its in-memory copy; two processes sharing one
   file would lose each other's changes. Same rules as `gui.json` (atomic, only on a change,
   unknown keys kept, invalid values fall back alone, broken or newer files read-only).
4. **The icon is a layered tool popup that never activates** (`WS_EX_LAYERED | WS_EX_TOOLWINDOW
   | WS_EX_NOACTIVATE`, topmost while pinned): 40 × 40 logical pixels at its monitor's DPI,
   drawn once per state change into a premultiplied bitmap for `UpdateLayeredWindow`. A click
   never takes the focus from the program being typed into; a drag (beyond the system's drag
   threshold) moves it with mouse capture, snaps 8 logical pixels to the work area's edges and
   keeps it fully inside; the place is saved when the drag ends. A saved place on no current
   monitor — or a display change that leaves the icon off every monitor — brings it onto the
   nearest monitor; the saved place itself is kept for when that monitor returns.
5. **The picture is a pure function of a small state** (`gui::float::art::IconState`: Host
   reachable or not, Terminals, running, failed, the aggregate Claude indicator by the
   `SUBAK-FR-051` priority). Step 44 replaces the procedural glyph (a charcoal-rind, red-flesh
   watermelon slice, provisional) and adds motion behind the same interface; the badge says the
   state in a shape, never in colour alone, and the words are the icon's accessible name and the
   tray tip. Nothing animates; no timer runs while connected.
6. **The tray behind a trait** (`Tray`): `Shell_NotifyIconW` with `NOTIFYICON_VERSION_4`
   (click and keyboard selection open the manager; the context menu is the icon's menu;
   re-added on `TaskbarCreated`), and a recording fake for every automated run, so tests never
   put an icon into the user's notification area.
7. **The manager is made of the system's own controls**: an edit field for the search (named by
   its "Search" label), an owner-drawn list box whose item strings are the rows' accessible
   names ("build, running · cmd.exe · shown in 1 window"), owner-drawn buttons whose window texts
   are their names. Keyboard focus, the IME (Korean search and rename), touch through mouse
   promotion and the accessibility tree come from Windows; we draw the design tokens. It is
   created when it opens and destroyed when it closes, so a closed manager costs nothing. Its
   keys are hotkey-contract actions of a new context `manager` (`manager.open` Enter,
   `manager.new` Ctrl+N, `manager.rename` F2, `manager.delete` Delete, `manager.search` Ctrl+F
   and `/`, `manager.pin` Ctrl+P, `manager.close` Esc), rebindable with `subak hotkeys`; the
   search field gets its editing keys first, Tab and the arrows navigate, a question owns every
   key while open.
8. **Delete always asks, by id, Cancel first** (DESIGN_SYSTEM §3.6 "Delete (danger,
   confirm)"): the question names what ends ("End `x`? It runs PING.EXE (2 processes); they
   will be ended." / "Its shell (cmd.exe) will be closed." / "Remove `x`? Nothing runs in it").
   Consent sends `terminal.kill` by id with `force` only when the question named it busy;
   without `force` the Host's guard still refuses one that turned busy (`busy`), and the
   manager asks again with the Host's details. A stopped or failed Terminal is removed with
   `terminal.remove` (no `force`). This is stricter than the window's `Ctrl+W` (which closes an
   idle shell at once, ADR 0042): a list row is easier to hit by mistake than the visible tab.
9. **New Terminal is the only path that starts a Host** (on demand, as `subak new` does, without
   a shell of its own); then the new Terminal is created with the Host's defaults and opened.
   Without a Host the manager says so and the icon retries the connection with the window's
   backoff (0.5 s … 10 s), and at once when the user hovers or clicks it.
10. **Opening hands the foreground on explicitly.** The floating UI asks the windows of its
    endpoint, front to back (`WM_COPYDATA`, `COPYDATA_OPEN`: `probe`, then `open` with the
    Terminal's id), after `AllowSetForegroundWindow(<that window's process>)`; the window shows
    the tab (a hidden one first comes back) and calls `SetForegroundWindow` for itself. With no
    window it starts `subak-gui <id>` and allows that process too. These requests can only show
    or select — never create, end or change a Terminal — are answered only for the receiver's
    own endpoint, and are capped at 4 KiB. The manager takes the focus only when the user opened
    it (the process received that input), and closes when it loses the focus unless pinned.
11. **The window keeps hidden tabs across runs** (`gui.json` `hidden_terminals`, ids): Step 17
    left tab hiding per run; the manager lists every Terminal anyway and opening a hidden one
    shows its tab again.
12. **Test hooks** (debug builds or `test-hooks`): every floating window on a synthetic monitor
    layout far off every real monitor (`--test-monitors`; a layout on a real monitor is
    refused), never activated, never given the focus or the mouse capture; a real
    `WM_DPICHANGED` is ignored; the fake tray; menus reported, not shown; JSON-line events and
    `WM_COPYDATA` test commands; pictures of its own content only (`PrintWindow` into memory);
    windows it starts are test windows.

## Evidence (this machine: Windows 10 Pro 22H2 19045)

- `crates/subak/tests/gui_float.rs`: 8 tests against real Hosts on private endpoints, driven by
  the floating UI's own window messages (no synthesized input, nothing on the desktop) — the
  no-Host and empty states, following CLI changes, search, open in a new and in an existing
  window, rename, the guarded delete, New Terminal starting a Host, drag and snap with a saved
  place, synthetic monitor layouts with `WM_DISPLAYCHANGE` recovery and DPI changes, 30 quick
  open/close cycles, idle cost, the fake tray and menus, forwarding of a second start, hidden
  tabs across window runs.
- Idle, debug build, off-screen, connected to a Host with three Terminals, 10 s: **0.0 ms CPU,
  1.97 MiB private, 11.0 MiB working set, 6 threads, 136 handles** with the manager closed, and
  0.0 ms CPU, 1.96 MiB private with it open. 30 open/close cycles: GDI objects 4 → 4, USER
  objects 5 → 5. (Release figures: [SUBAK_GUI §11.8](../SUBAK_GUI.md).)
- The accessibility tree (MSAA, which the system's UI Automation proxies of these controls
  expose), read from the off-screen manager: editable text "Search", list "Terminals" with one
  item per row named as above, push buttons "New Terminal", "Open", "Rename", "Delete…", "Pin
  (keep open: off)", "Close manager", and during a question the text "End `busy`? It runs
  ping.exe; they will be ended." with "Cancel" and "End Terminal".

## Consequences

- One more process while the user wants the icon (a few MiB, no CPU idle); none otherwise.
- The manager uses the system's UI font (Segoe UI) through GDI, unlike the window's terminal
  font; DESIGN_SYSTEM's UI font is Segoe UI Variable, which the window adopts in Step 20.
- `gui.show` (a global hotkey, no default) is still not shipped; keyboard access to the floating
  UI is the tray (Win+B), `subak-gui --manager` (for example as a shortcut key of a Start-menu
  link) and the manager's own keys.

## Not tested (and why)

- The **visible** icon and manager on the maintainer's desktop, a real tray icon, real mouse
  drags across real monitors with different DPI, touch, the real foreground hand-off and the
  native menus, a screen reader (Narrator) on the manager, High Contrast: agents never show
  windows, put icons in the notification area or synthesize input on this machine. The manual
  script is [SUBAK_GUI §11.9](../SUBAK_GUI.md).
- Windows 11.

## Alternatives considered

- **egui or another toolkit for the manager**: ~100 MiB idle (ADR 0036); rejected.
- **A custom-drawn manager with its own UI Automation provider**: more code (COM providers for
  a list, an edit field, buttons) than the system's controls, which already have keyboard, IME
  and accessibility; rejected for this Step.
- **Asking only when the Host says busy** (as `Ctrl+W`): rejected for a list (see 8).
- **The icon starting a Host to show the list**: rejected; an idle icon must not create Host
  processes (`SUBAK-FR-002` spirit, ADR 0003).

## Requirements

`SUBAK-FR-030`, `SUBAK-FR-032`, `SUBAK-FR-039`, `SUBAK-FR-003`, `SUBAK-FR-014`,
`SUBAK-FR-034`, `SUBAK-FR-046`, `SUBAK-FR-051`, `SUBAK-NFR-002`, `KIM-FR-012`, `KIM-NFR-012`.
