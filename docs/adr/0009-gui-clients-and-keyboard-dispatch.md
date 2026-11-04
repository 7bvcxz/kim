# ADR 0009 — GUI clients and keyboard dispatch

Status: **Accepted** · Date: 2026-11-03 · Step 03 · GUI toolkit chosen in Step 04; implemented in Steps 16, 17 and 23

## Context

The Subak GUI shows Terminals as tabs and must never own sessions (`SUBAK-FR-003`). Its
shortcuts (`Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, `Ctrl+Shift+Tab`) and the `Ctrl+B` prefix compete
with keys that programs in the terminal need (`SUBAK-FR-033`, `SUBAK-FR-034`).

## Decision

### Process structure

- `subak-gui.exe` is a pure client: one process hosts the tabbed window(s), the floating icon
  and the session manager. It connects to the Host over IPC ([ADR 0005](0005-local-ipc.md))
  and renders pane streams.
- Closing a window or quitting the GUI is a **detach**. A crash is also a detach. The GUI
  never kills anything except through an explicit kill request.
- The floating icon is part of `subak-gui.exe`. It only runs when the user enabled it, so
  there is no second UI runtime (Step 18).
- The toolkit is decided by the GUI technology ADR in Step 04. It must support per-monitor DPI,
  IME (Korean), accessibility (UI Automation), and headless rendering for automated UI tests.

### Close and kill semantics in the GUI

| Action | Effect |
| --- | --- |
| Window close button, `Alt+F4`, quit, `Ctrl+B d` | Detach this client. All Terminals keep running. |
| `Ctrl+W` / tab context menu "Close Terminal" | **Kill** the Terminal ([SUBAK_PHILOSOPHY §6.1](../SUBAK_PHILOSOPHY.md)). Asks for confirmation if any pane runs more than an idle shell; the confirmation names what will be killed. A setting can make `Ctrl+W` "hide tab (keep running)" instead. |
| Tab context menu "Hide tab" | Removes the tab from this window only. The Terminal keeps running and stays listed in the session manager. |
| Session manager "Delete" | Kill + remove the saved Terminal (with confirmation). |
| `Ctrl+B x` / `Ctrl+B &` | Kill the pane or window, with tmux-style confirmation. |

"Idle shell" means that the pane's foreground process is the shell it started with and it has
no child processes. Detection is on the Host side through the pane's job object.

### Keyboard dispatch (in order)

1. **IME composition guard.** While an IME composition is active (Korean, Japanese, Chinese),
   keys go to the IME. No shortcut fires.
2. **Prefix state.** If the previous key was the prefix (`Ctrl+B`), the next key is looked up
   in the prefix table (Step 23).
   - Unbound keys are swallowed and a short hint is shown, as tmux does.
   - `Ctrl+B Ctrl+B` sends a literal `Ctrl+B`.
   - **`Ctrl+B Ctrl+W`, `Ctrl+B Ctrl+T` and `Ctrl+B Ctrl+Tab` send the shadowed key literally
     to the program.** This is the pass-through for keys that the GUI otherwise reserves.
3. **Application shortcuts** (`Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, `Ctrl+Shift+Tab`, user-defined).
   - Every one can be rebound or disabled.
   - "Terminal-first mode" disables all application shortcuts except the prefix.
4. **Everything else** is encoded for the PTY: the xterm key encoding, plus win32-input-mode
   when the program requested it.

Prefix timeout and repeat behavior follow tmux defaults (`repeat-time` 500 ms for resize keys).
Bindings are configurable per profile.

## Consequences

- Users of readline, Vim and Emacs keep their keys through the pass-through, or by rebinding.
- Because `Ctrl+W` = kill follows the issue's explicit request, the confirmation step is
  mandatory whenever the Terminal is busy.

## Alternatives considered

- **`Ctrl+Shift+T` / `Ctrl+Shift+W`** (the Windows Terminal convention): avoids the conflicts,
  but contradicts the requested defaults. It is offered as a one-click preset.
- **Making `Ctrl+W` only hide the tab:** safer, but surprising to users of every other tabbed
  terminal, and hidden Terminals would pile up. It is available as a setting.

## Requirements

`SUBAK-FR-003`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-033`,
`SUBAK-FR-034`, `KIM-NFR-012`.

## Notes

- 2026-11-04 (Step 04): the toolkit decision is [ADR 0014](0014-gui-technology.md) — egui +
  glow for chrome, a dedicated cell-grid renderer for terminal content.
- 2026-11-04 (Step 45): Subak's bindings follow the cross-tool hotkey contract
  ([ADR 0017](0017-universal-customizable-hotkeys.md),
  [HOTKEY_CONTRACT §13.1](../HOTKEY_CONTRACT.md)). The dispatch order above is unchanged. The
  prefix becomes the leader `prefix`, the pass-through rule is derived from the current `app`
  bindings, "terminal-first mode" is a preset of disabled overrides, per-profile bindings are
  `profiles` overlays, and bindings live in `hotkeys.json` instead of the `[keys]` table of
  `config.toml`.
- 2026-11-04 (Step 10): [ADR 0020](0020-terminal-session-lifecycle.md) §7 fixes what the table
  above left open: a tab's × button is "Hide tab" (a detach of that tab; the Terminal keeps
  running and stays in the session manager). `Ctrl+W` / "Close Terminal" stays a kill. The Host
  enforces "confirms when busy": `terminal.kill` of a busy Terminal is refused (`busy`) unless the
  GUI sends `force`, which it may only after the user confirmed.
- 2026-11-04 (Step 16): `subak-gui.exe` exists as a pure client of one Terminal
  ([ADR 0039](0039-subak-window-plain-win32-client.md)). Close, Alt+F4 and a crash are detaches
  (tested with pids). Its dispatch implements stages 1, 3 and 4 before `TranslateMessage`; the
  prefix (stage 2) and the tab shortcuts arrive in Steps 17 and 23.
