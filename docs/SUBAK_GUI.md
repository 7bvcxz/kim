# The Subak window (`subak-gui`)

Status: **current** (implemented, Steps 16–18; the visible-window checks of §9 and the
visible-icon and real-tray checks of §11.9 are pending the maintainer) · Decisions:
[ADR 0039](adr/0039-subak-window-plain-win32-client.md),
[ADR 0042](adr/0042-subak-window-tabs-and-safe-close.md) (tabs),
[ADR 0045](adr/0045-subak-floating-icon-and-terminal-manager.md) (floating icon and Terminal
manager) · Builds on
[TERMINAL_EMULATOR](TERMINAL_EMULATOR.md) (Step 15), [IPC_PROTOCOL](contracts/IPC_PROTOCOL.md),
[SESSION_MODEL](SESSION_MODEL.md), [HOTKEY_CONTRACT](HOTKEY_CONTRACT.md),
[DESIGN_SYSTEM](DESIGN_SYSTEM.md) · Requirements: `SUBAK-FR-003`, `SUBAK-FR-011`,
`SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-033`, `SUBAK-FR-034`,
`SUBAK-FR-037`, `SUBAK-FR-038`, `SUBAK-FR-046`, `SUBAK-NFR-001`, `SUBAK-NFR-002`; Step 18:
`SUBAK-FR-030`, `SUBAK-FR-032`, `SUBAK-FR-039`, `KIM-FR-012`

`subak-gui.exe` is a small native window that shows the Host's **Terminals as tabs** and the
shown tab's pane live. It is a client of the per-user Host, like `subak attach`: the Terminals
and their programs live in the Host, so closing the window, hiding a tab — or the window
crashing — never ends them. The Host and the `subak` CLI work without it. The optional floating
icon, tray icon and Terminal manager (`subak-gui --float`, Step 18) are §11; search and clipboard
UX (Step 19) and visual polish and settings (Step 20) come next.

## 1. Starting it

```text
subak-gui [<terminal>] [--endpoint <name>] [--no-hotkeys]
subak-gui --float [--icon] [--tray] [--manager] [--endpoint <name>] [--no-hotkeys]   (§11)
subak-gui --manager [--endpoint <name>]                                               (§11)
subak-gui --help | --version
```

| Situation | What the window does |
| --- | --- |
| Always | Every Terminal of the Host is a tab (§2), in the Host's order. |
| `<terminal>` given (a name, case ignored, or a `t-` id) | Shows its tab. If it does not exist or does not run, says so in a banner and shows the first running tab. |
| No `<terminal>`, the Terminal shown last still runs | Shows it (remembered by id, then by name, in `gui.json`). |
| Otherwise | Shows the first running tab (Step 16's chooser is replaced by the tabs). |
| None exists | "No Terminal is open in this window": `Ctrl+T` (or `+`) starts one; the next Terminal any client creates (`subak new`) opens by itself. |
| No Host runs | Starts one, as the CLI does (same start lock), **without a shell**: opening the window never starts a shell of its own. |
| The endpoint is held by something that is not a Subak Host of this user, or no Host can start | An error screen with the stable code (`host_identity_mismatch`, `spawn_failed`, …), the message and `Enter` to retry. Nothing was sent to the impostor. |

Opening the window never creates, stops, kills or removes a Terminal. Only the explicit actions
of §2 do: `Ctrl+T` creates one, `Ctrl+W` ends or removes the shown one.

`subak-gui.exe` must sit next to `subak.exe` (it starts that program on demand). `--endpoint`
selects a private Host endpoint, as `subak --endpoint` does. `--no-hotkeys` (or `KIM_NO_HOTKEYS`)
starts the window with every shortcut off (§3).

## 2. Tabs, closing, crashing, reconnecting

**The tab bar** (32 px, DESIGN_SYSTEM §3.5) lists the Host's Terminals — running, starting,
stopping, stopped and failed ones — in the Host's order (creation order), except the ones hidden
in this window. Each tab shows its state as a shape (`●` running, `◐` starting or stopping, `■`
stopped, `▲` failed, `○` gone or unknown while disconnected), its name (cut with `…`) and a `×`.
The shown tab is bold, filled like the pane below it and marked with a lilac line on top (reverse
video in High Contrast). `+` after the last tab creates a Terminal; `‹`/`›` appear when the tabs
do not fit (the shown tab always stays visible). Only the shown tab's pane is attached: switching
detaches the old pane and attaches the new one, whose screen and history come back from the
Host; a program that asked for focus reports is told it lost the focus first.

**Other clients.** A Terminal created elsewhere (`subak new`) appears as a new tab at once but
does not take over the window (keys never move to a Terminal you did not choose). Renames,
stops and kills by any client update the tabs at once; a killed or exited shown Terminal keeps
its last screen with a banner (Enter closes the tab).

| Action | Keys and mouse | What it does to the Terminal |
| --- | --- | --- |
| **Next / previous tab** | `Ctrl+Tab` / `Ctrl+Shift+Tab` (wrapping), a click on a tab | Nothing: a detach of the old pane, an attach of the new one |
| **New Terminal** | `Ctrl+T`, `+`, the tab menu | Creates one with the Host's defaults (your default shell in your profile folder) at the window's size and shows it |
| **Rename** | `Ctrl+Shift+R`, a double click on the tab, the tab menu | The name becomes an edit field (Enter saves by id, Esc cancels; Korean through the IME works); a name in use or invalid is refused by the Host and the field stays with the reason |
| **Hide tab** (view dismissal) | The tab's `×`, a middle click, the tab menu, `tab.hide` (no default key) | **Nothing**: a detach of this window. The Terminal keeps running, `subak ls` lists it, and the tab menu's "Show hidden: …" brings it back. Hiding lasts while this window runs. |
| **Close Terminal** (end a live Terminal) | `Ctrl+W`, the tab menu | `terminal.kill` of the shown Terminal by its id. An idle shell ends at once. When the Host reports it **busy** (it runs more than an idle shell, e.g. a Claude session), the window asks: "End `x`? It runs … (n processes in its job); they will be ended. [ Cancel (Esc) ] End Terminal" — **Cancel is the default**: `Esc`, Enter on Cancel or a click on it ends nothing; `Tab`/arrows move to "End Terminal", Enter or a click ends exactly that Terminal. The question goes away if the Terminal ends or another tab is shown. Keys typed meanwhile reach no program. |
| **Remove a stored Terminal** | `Ctrl+W` on a stopped or failed tab, "Remove Terminal" in the tab menu | `terminal.remove` without force: deletes the Host's record; nothing runs in it, so it does not ask. Restarting a stopped Terminal is Step 30. |
| **Move a tab** | The tab menu, `tab.move-left`/`tab.move-right` (no default keys) | Nothing: the order is this window's |
| **Tab menu** | A right click on a tab or the bar, the context-menu key (`Menu`) | A native menu with the actions above and "Show hidden: …" |

The window's close button stays a detach of everything (below). A setting that makes `Ctrl+W`
only hide the tab (ADR 0009) is `subak hotkeys set tab.hide Ctrl+W --replace`.

| Event | Effect |
| --- | --- |
| Close button, Alt+F4, the window menu's Close | A **detach**: every Terminal keeps running (`subak ls`). |
| The window's process ends (crash, Task Manager) | The Host drops the connection; nothing else changes. |
| Another client detaches the window (`subak detach <terminal>`) | The last screen stays, with a banner "Detached by cli client (pid …); `x` keeps running. Enter: reattach". |
| The program exits, or the Terminal is stopped or removed by another client | The last screen stays, with a banner saying what happened and the exit code; `Enter` closes the tab of a Terminal that is gone (a stopped one stays: `Ctrl+W` removes it). |
| The Host goes away (stopped, crashed) | The last screen stays, with a banner "Disconnected: …. Reconnecting to `x` (attempt n)…"; the tabs show `○`. The window retries after 0.5, 1, 2, 4, 8 s, then every 10 s, and **never starts a Host while reconnecting**. When a Host serves the endpoint again it reattaches the same Terminal by id if it runs, else shows the first running tab and says the old one is gone. `Enter` retries at once. |

A Host stops its Terminals' programs when it ends (ADR 0003), so a reconnect after a Host crash
finds new Terminals at most; restoring them is Steps 29–31.

## 3. Keyboard, IME and mouse

Keys go to the program, in the order of ADR 0009:

1. **IME guard.** While an input method composes (Korean, Japanese, Chinese), keys belong to it.
   The composition is drawn inline at the cursor (underlined, on `bg.overlay`), the candidate
   list opens below the cursor, and only committed text is sent.
2. **The window's own fields.** While the busy confirmation (§2) is open, its keys answer it and
   every other key is swallowed; while a tab is renamed, the field takes the keys. Nothing
   reaches the program and no shortcut fires.
3. *(The `Ctrl+B` prefix table is Step 23. Until then `Ctrl+B` goes to the program.)*
4. **The shortcuts** — Subak's actions of the [hotkey contract](HOTKEY_CONTRACT.md) (§13.1): the
   manifest shipped as `hotkeys.manifest.json`, layered with `%APPDATA%\mouv\subak\hotkeys.json`
   (`subak hotkeys …`, [SUBAK_CLI §3.12](SUBAK_CLI.md)). The window re-reads the file when it gets
   the focus; a broken or newer file means the defaults (safe mode) and a banner; `--no-hotkeys`
   and `KIM_NO_HOTKEYS` turn every shortcut off. Sequences (`Ctrl+K Ctrl+N`), physical `code:`
   and `char:` keys and the `release`/`hold` triggers work as the contract says; a pending
   sequence shows a banner and `Esc`, a click or focus loss cancels it. A shortcut's key never
   reaches the program, not even its auto-repeat (unless the binding has `repeat: true`).

   | Action id | Default keys | Scope | What |
   | --- | --- | --- | --- |
   | `terminal.new` | `Ctrl+T` | app | New Terminal (§2) |
   | `terminal.close` | `Ctrl+W` | app | Close Terminal: ends the shown one, asks when busy; removes a stored one (§2) |
   | `tab.next` / `tab.previous` | `Ctrl+Tab` / `Ctrl+Shift+Tab` | app | Next / previous tab |
   | `terminal.rename` | `Ctrl+Shift+R` (`Ctrl+B $` with Step 23) | app | Rename the shown Terminal |
   | `tab.hide`, `tab.move-left`, `tab.move-right` | none | app | Hide the shown tab; move it |

   `Ctrl+T` and `Ctrl+W` are keys programs use too (readline's transpose and delete-word): the
   contract reports them as `shadows_terminal_input`. Until the `Ctrl+B` pass-through of
   Step 23, a program gets them once you disable or rebind the action
   (`subak hotkeys disable terminal.close`). The Step 16 keys below are actions of the
   `terminal` context and are rebindable the same way:

   | Action id | Keys | What |
   | --- | --- | --- |
   | `clipboard.copy` | `Ctrl+Shift+C`, `Ctrl+Insert` | Copy the selection (plain text, CR LF line ends) |
   | `clipboard.paste` | `Ctrl+Shift+V`, `Shift+Insert` | Paste: line breaks become Enter, control characters are removed, bracketed when the program asked (`SUBAK-SEC-013`) |
   | `view.zoom-in` / `view.zoom-out` / `view.zoom-reset` | `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0` (and the numpad `+`, `-`, `0`) | Text size 8–36 px (default 13), saved |
   | `scroll.page-up` / `scroll.page-down` | `Ctrl+Shift+PageUp` / `PageDown` | Scroll the history a page |
   | `scroll.line-up` / `scroll.line-down` | `Ctrl+Shift+Up` / `Down` | Scroll a line |
   | `scroll.top` / `scroll.bottom` | `Ctrl+Shift+Home` / `End` | Oldest line / live screen |

5. **Everything else** goes to the program in the xterm encoding (TERMINAL_EMULATOR §4.1):
   characters as UTF-8 (Alt adds `ESC`; AltGr characters are text), `Ctrl+letter` as C0 controls,
   `Ctrl+Space` as NUL, `Ctrl+2`…`Ctrl+8` as the VT220 codes, cursor keys (`SS3` in application
   cursor mode), Home/End/Insert/Delete/PageUp/PageDown and F1–F12 with modifiers, `Shift+Tab`
   as `CSI Z`. `Ctrl+C` interrupts the program; it is never "copy". Alt+F4 closes the window and
   Alt+Space opens its window menu, as in every Windows program; a lone Alt or F10 does not open
   a menu bar. Typing returns a scrolled-back view to the live screen.

The **mouse**: drag selects (double click: word, triple click: line, Alt+drag: block); a single
click clears the selection; right click copies a selection, or pastes when there is none; the
wheel scrolls the history 3 lines per notch (or goes to the program when it tracks the mouse, or
sends cursor keys on the alternate screen when the program asked for alternate scroll); Shift
always selects, as in xterm. On the tab bar: a click shows a tab, a double click renames it, a
middle click or its `×` hides it, a right click opens its menu, `+` creates a Terminal.

**Focus**: the cursor is a filled block (or the program's shape) with focus and a hollow box
without it; programs that asked for focus reports (mode 1004) get them. The cursor does not
blink (no idle timer; DESIGN_SYSTEM §4.5).

## 4. Window, DPI and states

- A normal resizable window with minimize and maximize, the native caption in dark mode (on
  Windows 11 also the caption, text and border colours of the tokens), the terminal surface in
  `terminal.background` with 8 px padding, the area around the cell grid in the same colour.
  Title: the program's title and the Terminal name, "`title` — `name` — Subak".
- **Per-monitor DPI v2.** Moving to a monitor with another scale re-rasterizes the glyphs at the
  new size (never a bitmap scale), takes the size Windows suggests, and the pane follows the new
  grid. The smallest window holds 20 × 5 cells.
- **Resizing** sends the new grid to the Host (one size per pane, the latest interactive client's,
  ADR 0008); a resize to the pane's current size changes nothing. **Minimizing** never resizes the
  pane.
- **High Contrast**: with Windows High Contrast on (and `theme: "auto"`), the terminal and the
  screens use the system's window, text and highlight colours and no lilac; the shown tab and the
  focused confirmation button are in reverse video. It follows changes while the window is open.
- **States are never blank and never colour-only** (DESIGN_SYSTEM §3.8): connecting ("Connecting
  to the Subak Host…" and the endpoint), opening, empty, a stored Terminal ("`x` is stopped …
  `Ctrl+W` removes it", naming the keys as bound), the error screen (code, message, hint, "Press
  Enter to try again"), the banners of §2 (a one-line strip in the status colour at 12 % over
  `bg.raised`, a 3 px marker, words that say what happened and what `Enter` does), and the busy
  confirmation (danger colour, the focused button filled and in brackets).

## 5. Preferences: `%APPDATA%\mouv\subak\gui.json`

The window's own file, independent of the Host's state (no process ids, no terminal content, no
secrets). Schema: [subak-gui-v1](contracts/schemas/subak-gui-v1.schema.json), examples in
`contracts/examples/subak-gui-v1/`.

```json
{
  "schema": "subak.gui",
  "schema_version": 1,
  "font_size": 15.0,
  "theme": "auto",
  "window": { "x": 240, "y": 120, "width": 1452, "height": 912, "dpi": 144, "maximized": false },
  "last_terminal": { "id": "t-3f2a9c41d07e", "name": "빌드" }
}
```

| Key | Meaning |
| --- | --- |
| `font_size` | Terminal text size in logical pixels, 8–36 (default 13; changed with the zoom keys) |
| `theme` | `auto` (kim-dark, or the system colours under High Contrast), `kim-dark`, `high-contrast` |
| `window` | The restored (not maximized) outer rectangle in physical pixels at `dpi`, and `maximized`. Used only if a title bar's worth of it lies on a current monitor; its size is rescaled to that monitor's DPI. |
| `last_terminal` | The Terminal shown last; reopened when it still runs |
| `hidden_terminals` | Terminals hidden in the window when it last ran, by id (Step 18). A new window keeps them hidden; they keep running, the tab menu's "Show hidden: …" and the Terminal manager (§11) show them again |

- Written **atomically** (a temporary file, flushed, renamed) and only on a change: a move or
  resize that ended, maximize/restore, a zoom, another Terminal, closing. Never while idle.
- Unknown keys are kept when the file is rewritten (`SUBAK-FR-046`).
- An invalid value falls back to its default alone; the window says so once in a banner.
- A file that is not JSON, not a `subak.gui` document, or from a newer schema version is used as
  defaults and **never rewritten** by this run; the banner says so.
- Step 20 moves display settings into the versioned `config.toml` with migrations; the window
  placement stays here.

## 6. How it works

| Part | Where | Role |
| --- | --- | --- |
| Model | `crates/subak/src/gui/model.rs` | The Host conversation as a state machine without I/O: the tabs and which one to attach, create/kill/remove/rename by id, the busy answer, other clients' changes, phases, snapshots and resyncs, the pane's end, reconnects with backoff |
| Screens | `gui/screens.rs` | VT text of the state screens, banners and the busy confirmation in the design tokens, and their High Contrast form |
| Tabs | `gui/tabs.rs` | The tab bar: tab widths, scrolling, states as shapes, the rename field, clickable spans |
| Layout | `gui/layout.rs` | DPI scale, grid ↔ client area, saved placements on monitors |
| Keys | `gui/keys.rs` | The shortcuts: Subak's manifest and `hotkeys.json` through `kim-hotkeys` (`crate::hotkeys`), logical/physical/character keys, sequences and triggers; key and character mapping |
| Preferences | `gui/prefs.rs` | `gui.json` |
| Composition | `gui/compose.rs` | The client area from its layers (also what test shots and goldens use) |
| Network | `gui/net.rs` | A reader thread (connect or start, identity checks, frames in) and a writer thread (frames out) |
| Window | `gui/window.rs` | Win32: paint (tab bar strip, pane, banner, IME), keyboard dispatch, the confirmation and the rename field, the native tab menu, mouse, clipboard, DPI, High Contrast, test hooks |
| Binary | `crates/subak/src/bin/subak-gui.rs` | GUI subsystem (no console window) |
| Floating UI | `gui/float/*.rs` | `--float` (§11): placement, the icon's picture, the manager's model, `float.json`, the tray trait (portable, unit-tested) and the Win32 icon, tray and manager (`float/win.rs`) |

Data path: the reader queues frames and posts one message per batch; the window feeds them to the
pane's `TermView` (the same model as the Host), renders only changed rows, and invalidates only
those. A snapshot at attach (or after a resync) starts a fresh view at the snapshot's size; once
complete, the view follows the window's grid. The interactive view answers the program's device
reports. Synchronized output (mode 2026) holds the frame until the program ends its update, at
most 150 ms.

Safety: the window is an IPC client of kind `gui` with the CLI's identity checks (same user,
`subak.exe`, before anything is sent); it ends a Terminal only on `Ctrl+W` or the menu, by id and
with `force` only after the user's answer, never starts a shell by opening, never touches another
user's Host, never opens links (OSC 8 hit-testing only; opening is Step 19), never honours OSC 52,
and never writes the system clipboard except for an explicit copy.

## 7. Measurements

This machine: Windows 10 Pro 22H2 19045, Intel Core i5-13600KF, 2026-11-04.

| What | Result |
| --- | --- |
| Idle, release build (`--features test-hooks` for the off-screen flag), attached to a live cmd.exe Terminal, 30 s, no output (`step16_idle.ps1`, the Step 15 sampler's quantities) | Step 16: **0.0 ms CPU, 3.95 MiB private, 11.3 MiB working set, 5 threads, 154 handles**. Step 17, three tabs (one attached), the same sampler (`step17_idle.ps1`, `Process.TotalProcessorTime` over 30 s): **0 ms CPU, 4.34 MiB private, 11.5 MiB working set, 5 threads, 150 handles** |
| Idle, debug build, same, 10 s (`tests/gui_window.rs::idle_window_costs_no_cpu_and_little_memory`) | Step 16: 0.0 ms CPU, 4.06 MiB private, 12.6 MiB working set, 6 threads, 153 handles. **Step 17 (with the tab bar): 0.0 ms CPU, 4.38 MiB private, 13.5 MiB working set, 6 threads, 157 handles** |
| Step 15 references (ADR 0036) | renderer + GDI window, no IPC: 3.40 MiB private, 1 thread; egui + glow: 107.0 MiB private, 12 threads |
| Binary sizes (release, shipped features) | Step 16: `subak-gui.exe` 917 KB; `subak.exe` 1.57 MB. Step 17 (the hotkey contract's checker and editor in both, `subak hotkeys` in `subak.exe`): `subak-gui.exe` 1.31 MB; `subak.exe` 2.10 MB |
| `subak.exe` carries no window or renderer | its import table names none of `CreateDIBSection`, `GetFontData`, `SetDIBitsToDevice`, `CreateWindowExW`, `ImmGetContext`, `gdi32` (checked with `grep -c -a` on the release binary); `subak-gui.exe` names all of them |

The threads beyond Step 15's one: the reader and writer (blocked in the kernel when idle), and
the system's input-method threads.

## 8. Known limits (Steps 16–17)

- One window per process. Hidden tabs are kept across runs (`gui.json`, Step 18; with several
  windows the one closed last decides); the tab order is the window's own and not saved.
- No `Ctrl+B` prefix or pass-through (Step 23): a program gets `Ctrl+T`/`Ctrl+W` only once the
  action is disabled or rebound. `AltGr` and `Win` chords are not matched by the window.
- A background tab is not attached: its activity and Claude state are not shown (Steps 26–28).
- No multi-line paste warning, search, link opening, copy-as-HTML (Step 19).
- win32-input-mode is not granted: Ctrl+Break and key releases do not reach programs.
- UI text uses the terminal font; the tab bar is in the client area (the integrated title-bar tab
  bar of DESIGN_SYSTEM §3.5 needs custom non-client drawing), icons, settings UI and themes are
  Step 20. No hover states or tooltips on tabs yet. The taskbar shows the generic application
  icon.
- No UI Automation tree yet (screen readers do not read the terminal); planned with Step 20/39.
- Colour emoji are monochrome; the cursor does not blink.
- The banner covers the pane's first row while it is shown.

## 9. Manual verification (maintainer)

Agents on this machine never show windows or synthesize input, so these checks are the
maintainer's. Build first: `cargo build --release -p subak` (both binaries land in
`target\release`). Use a private endpoint so nothing touches your own Host:
`set E=--endpoint subak-manual-16`.

1. `target\release\subak.exe %E% new work` — then `target\release\subak-gui.exe %E% work`. A
   window opens with the cmd.exe prompt; the title reads "… — work — Subak"; the caption is dark.
2. Type `dir`, `echo 한글 테스트`, `ver`. Output appears at once; Korean sits in two cells per
   syllable without overlap.
3. Switch the input method to Korean (한/영) and type `echo 한글`: while composing, the syllable is
   drawn underlined at the cursor; nothing reaches cmd.exe until the syllable is committed;
   Hanja conversion (한자 key) opens its list below the cursor. Shortcuts (`Ctrl+Shift+C`) do not
   fire during composition.
4. Keys: `↑` recalls the last command; `Ctrl+C` interrupts `ping -t localhost`; `Esc`, `Tab`
   completion, `F7` (cmd history list), `Ctrl+Plus`/`Ctrl+Minus`/`Ctrl+0` change the text size;
   `Alt+F4` closes (then reopen with step 1's second command).
5. Mouse: drag selects, double click a word, triple click a line; right click copies; right
   click again pastes; `Ctrl+Shift+V` pastes; the wheel scrolls back, typing returns to the live
   screen.
6. Focus: click another window — the cursor becomes a hollow box; click back — filled.
7. Maximize, restore, minimize, restore: the content re-lays out each time; `subak.exe %E% info
   work` shows the pane size following the window (and unchanged while minimized).
8. DPI: move the window to a monitor with another scale (or change the scale in Settings →
   Display → Scale): text re-renders sharp at the new size, without blur or a flash of a wrong
   size.
9. High Contrast: turn on a Contrast theme (Settings → Accessibility → Contrast themes): the window
   switches to the system colours; turn it off: kim-dark returns.
10. Close the window with the X button: `subak.exe %E% ls` still lists `work` as running with the
    same pid. Reopen with `subak-gui.exe %E%` (no name): it reopens `work`, at the same size and
    position, with the earlier output.
11. End `subak-gui.exe` in Task Manager: `subak.exe %E% ls` still shows `work` running.
12. With the window open, run `subak.exe %E% detach work`: a banner says it was detached; `Enter`
    reattaches. Run `subak.exe %E% host stop --kill-terminals --yes`: the banner says
    "Disconnected …"; run `subak.exe %E% new other`: within 10 s the window lists `other`.
13. Clean up: `subak.exe %E% host stop --kill-terminals --yes`.
14. Record the Windows build, scale(s), keyboard layout and pass/fail per step in the Step 16
    issue.

**Tabs (Step 17).** With the same `%E%`:

15. `subak.exe %E% new one`, `subak.exe %E% new two`, then `subak-gui.exe %E%`: two tabs; `one` is
    shown, bold, with a lilac line on top joined to the pane.
16. From a console: `subak.exe %E% new three` — a third tab appears, `one` stays shown;
    `subak.exe %E% rename two 둘` — the tab reads `둘`; `subak.exe %E% kill three --yes` — the tab
    goes.
17. Press `Ctrl+Tab`, `Ctrl+Shift+Tab` (also hold `Ctrl+Tab`: it switches once, nothing reaches
    the shell): the tabs switch; typed text goes to the shown tab only.
18. `Ctrl+T`: a new tab running cmd.exe; `Ctrl+W` on it closes it without a question.
19. `subak.exe %E% new busy -- ping -n 1000 127.0.0.1`, show it, `Ctrl+W`: the question names
    `PING.EXE`; Enter (Cancel is focused) keeps it; `Ctrl+W`, `Tab`, Enter ends it; `one` still
    runs.
20. `Ctrl+Shift+R`, type `작업` with the Korean IME (composition drawn in the tab), Enter:
    `subak.exe %E% ls` shows the new name. Double click a tab: the same field.
21. Click a tab's `×`: the tab goes, `subak.exe %E% ls` still shows it running; right click the bar
    → "Show hidden: …" brings it back. Press the `Menu` key: the shown tab's menu opens under it.
22. `subak.exe %E% hotkeys disable terminal.close`, click into the window: `Ctrl+W` now deletes a
    word in PowerShell (`subak.exe %E% new ps -- powershell`); `subak.exe %E% hotkeys reset
    terminal.close`, click again: `Ctrl+W` closes again.
23. Turn on a Contrast theme: the shown tab is in reverse video, states keep their shapes.
24. Close the window with X: `subak.exe %E% ls` lists every Terminal running. Clean up as in 13.

| Manual check | Result |
| --- | --- |
| Visible window, rendering, Korean (steps 1–2) | **pending — maintainer** |
| Korean IME composition and Hanja list (step 3) | **pending — maintainer** |
| Keyboard and focus (steps 4, 6) | **pending — maintainer** |
| Mouse and clipboard (step 5) | **pending — maintainer** |
| Maximize/minimize and DPI across monitors (steps 7–8) | **pending — maintainer** |
| High Contrast (step 9) | **pending — maintainer** |
| Close, crash, detach, reconnect (steps 10–12) | automated off-screen (§10); visible run **pending — maintainer** |
| Tabs, the tab keys, busy confirmation, rename, hide, other clients (steps 15–21, 24) | automated off-screen with the window's own messages (§10); visible run, real keyboard and the native menu **pending — maintainer** |
| Korean IME in the rename field (step 20), a disabled `Ctrl+W` in a real program (step 22), High Contrast tabs (step 23) | **pending — maintainer** |

## 10. Tests

| Test | What it checks |
| --- | --- |
| `crates/subak/tests/gui_window.rs` (Windows; the debug build's off-screen window driven by its own messages) | attach to a CLI-created Terminal: same pane pid, no second cmd.exe, one pane, a `gui` client, typed and Korean input reaching the shell (window and `subak capture`); close (WM_CLOSE) and crash (TerminateProcess) leave the Host and the pane with the same pids, a new window reopens the remembered Terminal with its screen; `subak detach` → banner → Enter reattaches; a stopped Host → disconnected banner, no Host started while retrying, a returning Host reconnected; empty, two-tab and error (a pipe squatter) screens; paste, copy, wheel, cursor keys, IME composition and commit, focus; synthetic DPI 192/96, resize, minimize, zoom, `gui.json` persistence and its error rules; frames of the window's own content (`SUBAK_GUI_SHOTS=<dir>` keeps them as BMP); idle CPU and memory |
| `crates/subak/tests/gui_window.rs` Step 17 tests (a stand-in program, this test binary run in a pane, plays the busy Claude session; never Claude Code) | `tabs_follow_the_headless_cli_and_ctrl_tab_switches_without_ending_anything` (CLI new/rename/stop/kill follow; `Ctrl+Tab`/`Ctrl+Shift+Tab` move the attachment; input to the shown tab only; `Ctrl+W` on a stored tab removes only the record); `ctrl_t_creates_and_ctrl_w_closes_asking_first_when_busy_never_hitting_another_terminal` (window-size create, idle shells closed at once, repeated create/close, the question for the busy stand-in: Esc, Enter on Cancel and a click on Cancel end nothing, keys typed meanwhile never reach it, consent ends only its id); `the_window_x_and_a_tab_x_only_detach_and_hidden_tabs_come_back` (a click on `×`, a middle click and `WM_CLOSE` are detaches, the Menu key's menu, "Show hidden"); `rename_in_place_and_keys_for_the_program_reach_it_once_a_shortcut_is_off` (Korean rename, `name_conflict` keeps the field, a disabled `Ctrl+T` reaches the program as 0x14 after a focus reload, `Ctrl+Tab` never does); `shortcuts_off_safe_mode_and_focus_transitions` (a broken `hotkeys.json`, a sequence cancelled by focus loss, input following the shown tab, `--no-hotkeys`) |
| `crates/subak/src/gui/*.rs` unit tests | the model (no, one and many Terminals as tabs, cycling, a late attach answer, create, `Ctrl+W` by id with the busy answer and consent, hiding and unhiding, stored removal, other clients' create/rename/stop/kill, attach decisions, snapshots, resyncs, ends, backoff), the key matcher (the requested keys with exact modifiers, Step 16 keys, keys for programs, auto-repeat, rebinding and disabling, sequences, `code:`/`char:` keys, release and hold, side-specific modifiers, safe mode and reload, `--no-hotkeys`), the tab bar (no/one/many tabs, scrolling with the shown tab visible, the rename field), screens, banners and the confirmation (words, actions, the safe default, High Contrast, tokens = `design/tokens.json`), layout with synthetic DPI 96–288 and monitor layouts, character mapping, preferences, golden frames |
| `crates/subak/tests/golden/gui_*.txt` | golden frames (built-in rasterizer) of the connecting, empty, stored-Terminal and error screens at 100 % and 200 %, the tab bar at 100 % and 200 %, the banner over a pane, the inline IME composition; `SUBAK_BLESS=1` refreshes them |
| `e2e/tests/step16_subak_window.rs` (ignored; `cargo xtask e2e` only) | the release window against a release Host: attach without a new shell, input, close and crash keep the pane, reconnect |
| `e2e/tests/step17_subak_tabs.rs` (ignored; `cargo xtask e2e` only) | tabs following CLI new/rename/kill, `Ctrl+Tab` switching with input to the shown tab only; `Ctrl+T`, `Ctrl+W` asking before ending a busy Terminal (Esc keeps it, consent ends only it), the window X only detaching |
| Step 18 | the floating icon and Terminal manager: §11.10 |

## 11. The floating icon, tray icon and Terminal manager (Step 18)

`subak-gui --float` runs a small, optional companion to the windows: a **floating icon** that
stays above other windows, and/or a **tray icon**, and the **Terminal manager** they open. It is
a tiny process of the same binary on the same plain Win32 stack (ADR 0045): no GUI framework, no
terminal renderer, no GPU. It is a client of the Host like a window; it never attaches a pane,
and closing it — or the manager — ends nothing.

### 11.1 Starting and stopping it

```text
subak-gui --float                 the saved choice (first time: the floating icon)
subak-gui --float --tray          only a tray icon (saved)
subak-gui --float --icon --tray   both (saved)
subak-gui --manager               open the Terminal manager (starting the floating UI if needed)
```

- **Off unless you start it.** The Host and the `subak` CLI never start it and never need it;
  opening a window does not start it. Besides the command line, a window's tab menu has
  "Floating icon and Terminal manager". Nothing is added to sign-in (Run or Startup) by this
  Step.
- **One per endpoint** (and Windows session): running `subak-gui --float` or `--manager` again
  asks the running one to show itself (or its manager) and exits. `--endpoint` works as for the
  window.
- **Quit** from the icon's menu ("Quit (Terminals keep running)"), or turn off both the floating
  icon and the tray icon. A quit, a crash or ending the process in Task Manager is a disconnect:
  no Terminal, window or Host is touched.
- **Headless means none of it**: without a desktop session nothing of this runs; the CLI and the
  Host behave exactly as before (`KIM-FR-012`).

### 11.2 The floating icon

| Behaviour | What it does |
| --- | --- |
| Size and look | 40 × 40 logical pixels at its monitor's DPI (60 px at 150 %, 80 px at 200 %), radius 10 on `bg.overlay`, a 1 px `border.subtle` edge, the watermelon glyph (§11.3) and a 10 px state badge |
| Never takes the focus | A tool window (no taskbar button, not in Alt+Tab) that never activates: clicking it does not move the keyboard away from the program you type into |
| Click | Opens the Terminal manager beside it (§11.4); a second click closes it |
| Drag | Moves it; it snaps to a work area's edges within 8 logical pixels and always stays fully inside a work area (never under the taskbar). The place is saved when you let go |
| Right click (press and hold on touch) | Its menu: Terminals…, Open Subak window, New Terminal, Always on top ✓, Show floating icon ✓, Show tray icon ✓, Quit (Terminals keep running) |
| Pin (Always on top) | On by default; unchecked, the icon and the manager are normal windows in the z-order |
| Hide | "Show floating icon" off hides it (the tray icon brings it back); with neither on, the floating UI quits |
| Monitors | Its place is restored on the monitor it was on, at that monitor's DPI. If that monitor is gone — or a display change (resolution, monitor unplugged, taskbar moved) leaves the icon off every work area — it comes back onto the nearest monitor; the saved place is kept for when that monitor returns |
| DPI | Per-monitor DPI v2: moving to a monitor with another scale redraws it at the new size |
| Hover | Without a Host, hovering retries the connection at once (otherwise every 0.5 s … 10 s) |
| Assistive technology | Its window name is its state in words ("Subak — 3 Terminals, 2 running"), the same as the tray tip |

### 11.3 The picture and its state (Step 44 interface)

The icon is a pure function of a small state (`gui::float::art::IconState`): whether a Host is
reachable, how many Terminals it has, how many run or failed, and the aggregate Claude indicator
(priority `need-input` > `error` > `done` > `in-progress` > `unknown` > `idle` > `none`,
`SUBAK-FR-051`; always `none` until the Host reports Claude states, Steps 26–28). It is drawn
once per state change; nothing animates and no timer runs, so a hidden icon costs nothing. The
glyph is Subak's identity drawn procedurally — a watermelon slice with a charcoal rind (with a
light edge so it never reads black-on-black), a pale band and red flesh with dark seeds — and is
**provisional**: Step 44 owns the final art and the per-state motion behind the same interface.
The badge is a shape, never colour alone: a hollow ring (no Host), a ring with a gap
(connecting), a triangle (a Terminal failed), Claude shapes later; nothing when all is well.
Under High Contrast the glyph is an outline in the system's text colour.

### 11.4 The Terminal manager

A compact popup (380 × 440 logical pixels) beside the icon — above it when the icon is in the
lower half of its work area, below otherwise, aligned to the side with room — or near the tray
icon. It lists **every Terminal of the Host**, including ones no window shows (a hidden tab, no
window at all), in the Host's order:

`● build    running · cmd.exe · shown in 1 window` · `● agent    running · busy: claude.exe (3 processes)` · `■ old    stopped`

The state is a shape and a word (`●` running, `◐` starting or stopping, `■` stopped, `▲`
failed). The header says the totals ("3 Terminals · 2 running · 1 stopped"); the status line
shows the keys as bound, or what just happened.

| Action | Keys (context `manager`, rebindable with `subak hotkeys`) | Mouse and touch | What it does |
| --- | --- | --- | --- |
| Search | type in the field; `Ctrl+F` or `/` focuses it; typing in the list goes there too | the field | Filters by name (case and composed/decomposed Hangul folded), program, state or id prefix; `Esc` clears it |
| Move | `Up`/`Down`/`PageUp`/`PageDown` (also from the field), `Home`/`End` in the list | click a row | The selection |
| Open | `Enter` | double click, "Open" | Shows the Terminal: a window of this Host shows its tab (a hidden tab comes back) and comes to the front; with no window, a new `subak-gui <id>` opens. The manager closes unless pinned |
| New Terminal | `Ctrl+N` | "New Terminal" | Creates one with the Host's defaults and opens it. Without a Host, it starts one first — the only thing that starts a Host here |
| Rename | `F2` | "Rename" | The row's name becomes an edit field (the IME works): `Enter` saves by id, `Esc` cancels; a name in use or invalid keeps the field with the reason |
| Delete | `Delete` (in the list) | "Delete…" | **Always asks first**, naming what ends: "End `x`? It runs PING.EXE (2 processes); they will be ended." / "End `x`? Its shell (cmd.exe) will be closed." / "Remove `x`? Nothing runs in it; its record is deleted." **Cancel is focused**: `Esc`, `Enter` on Cancel or a click on it ends nothing; `Tab`/arrows move to "End Terminal" (or "Remove"). Consent sends exactly one request for exactly that Terminal, by id: `terminal.kill` (with `force` only when the question said it is busy; one that turned busy since is refused by the Host and asked again) or `terminal.remove`. While the question is open, other keys reach nothing |
| Pin | `Ctrl+P` | "Pin" | Keeps the manager open when it loses the focus (saved); unpinned it closes like a flyout |
| Close | `Esc` (with an empty search) | `×`, click the icon again | Closes the manager; an open question counts as Cancel. Nothing else changes |
| Row menu | `Shift+F10`, the Menu key | right click | Open, Rename…, Delete…, New Terminal |

`Tab`/`Shift+Tab` move between the search field, the list and the buttons; `Enter`/`Space`
press a focused button. The search field gets its editing keys first (`Delete`, `Backspace`,
arrows, `Home`/`End`, `Ctrl+A/C/V/X/Y/Z`, characters).

**States are never blank** (DESIGN_SYSTEM §3.8): "Connecting to the Subak Host…"; "The Subak Host
is not running (…). New Terminal (Ctrl+N) starts it with a Terminal."; "Cannot reach the Subak
Host: … (code). It is retried in a moment." (an impostor on the endpoint, for example); "No
Terminals yet. New Terminal (Ctrl+N) creates one."; "No Terminal matches “x”."

**Accessibility.** The manager is built from the system's own controls, so keyboard focus, the
IME, touch (mouse promotion) and the accessibility tree come from Windows: an editable text named
"Search", a list named "Terminals" whose items are the rows in words, push buttons named "New
Terminal", "Open", "Rename", "Delete…", "Pin (keep open: off)", "Close manager", and while asking
the question as text with "Cancel" and "End Terminal". Focus is a 2 px `focus.ring`; the search
field has a `border.strong` boundary; High Contrast uses the system colours.

### 11.5 Opening a Terminal, and the focus rules

- The floating icon never takes the focus. The manager takes it only when you opened it (a
  click on the icon or the tray, the tray's keyboard selection, `subak-gui --manager`), which
  Windows allows the process that received that input.
- **Opening hands the foreground on explicitly**: the floating UI asks the windows of its Host,
  front to back, whether they serve it, calls `AllowSetForegroundWindow` for that window's
  process, and asks it to show the Terminal; the window shows the tab and calls
  `SetForegroundWindow` for itself (restoring itself if minimized). With no window, it starts
  `subak-gui <id>` and allows that process. Nothing else steals the focus.
- These window-to-window requests (`WM_COPYDATA`) can only show or select — never create, end or
  change a Terminal; a window answers only for its own endpoint, and requests over 4 KiB are
  ignored. Windows' UIPI keeps lower-integrity processes from sending them.

### 11.6 Settings: `%APPDATA%\mouv\subak\float.json`

Schema: [subak-float-v1](contracts/schemas/subak-float-v1.schema.json), examples in
`contracts/examples/subak-float-v1/`.

```json
{ "schema": "subak.float", "schema_version": 1, "icon": true, "tray": false, "on_top": true,
  "manager_pinned": false, "position": { "x": 2856, "y": 1316, "dpi": 144 } }
```

| Key | Meaning |
| --- | --- |
| `icon`, `tray` | Which presences show (default: the floating icon only) |
| `on_top` | Pinned above other windows (default on) |
| `manager_pinned` | The manager stays open when it loses the focus (default off) |
| `position` | The icon's top-left corner in physical pixels and its monitor's DPI; restored as §11.2 says |

Its own file (the windows rewrite `gui.json` from memory; two processes sharing one file would
lose each other's changes). Same rules as `gui.json` (§5): written atomically and only on a
change, unknown keys kept, an invalid value falls back alone, a broken or newer file is used as
defaults and never rewritten.

### 11.7 How it works

| Part | Where | Role |
| --- | --- | --- |
| Placement | `gui/float/place.rs` | Icon size at a DPI, the default place, drag with snapping, settling on a monitor, recovery from a lost monitor, the manager's rectangle (synthetic DPI and monitor layouts in its tests) |
| Picture | `gui/float/art.rs` | The icon from `IconState` (premultiplied BGRA, 4 × 4 supersampled), the badge, the words, Claude aggregation |
| Manager model | `gui/float/list.rs` | The Host conversation (subscribe, list, follow events, create, rename, kill, remove by id), search, selection by id, the delete question, no-Host and retry |
| Settings | `gui/float/fprefs.rs` | `float.json` |
| Tray | `gui/float/tray.rs` | The `Tray` trait and the recording `FakeTray` |
| Win32 | `gui/float/win.rs` | The layered icon window (`UpdateLayeredWindow`), `Shell_NotifyIconW`, the manager of system controls (owner-drawn list box and buttons, an edit field), keyboard dispatch before `TranslateMessage`, opening Terminals in windows, single instance, test hooks |
| Window side | `gui/window.rs`, `gui/model.rs` | Answers `probe`/`open` requests (shows the tab, comes forward), keeps hidden tabs in `gui.json` |

One connection (`subak-float`, kind `gui`) through the window's network threads; idle, both wait
in the kernel. The only timer is the reconnect delay while no Host is reachable.

### 11.8 Measurements

This machine: Windows 10 Pro 22H2 19045, 2026-11-04.

| What | Result |
| --- | --- |
| Idle, debug build, off-screen, connected to a Host with 3 Terminals, 10 s (`tests/gui_float.rs`) | icon only: **0.0 ms CPU, 1.97 MiB private, 11.0 MiB working set, 6 threads, 136 handles**; icon and manager open: 0.0 ms CPU, 1.96 MiB private, 11.0 MiB working set, 6 threads |
| RELEASE_ROW |
| 30 quick open/close cycles of the manager | GDI objects 4 → 4, USER objects 5 → 5 (nothing leaks) |

### 11.9 Manual verification (maintainer)

Agents on this machine never show windows, put icons into the notification area or synthesize
input, so these checks are the maintainer's. Build: `cargo build --release -p subak`; use a
private endpoint: `set E=--endpoint subak-manual-18`.

1. `target\release\subak-gui.exe %E% --float`: a 40 px watermelon icon appears at the bottom
   right of the primary monitor, above other windows, with a hollow-ring badge; no taskbar
   button; the program you were typing in keeps the focus. `subak.exe %E% ls` says no Host runs.
2. Click the icon: the manager opens above it with "The Subak Host is not running …". Press
   `Ctrl+N`: a Host starts, a Terminal is created and a Subak window opens with it, in the
   foreground; the manager closes.
3. `subak.exe %E% new work`, `subak.exe %E% new 빌드`: the icon's tooltip/name says
   "3 Terminals, 3 running" within a moment (hover the icon).
4. Click the icon, type `빌` (Korean IME): only `빌드` remains; `Esc` clears; `Down`, `Enter`:
   the existing window shows `빌드`'s tab and comes to the front.
5. In the window hide `work` (its `×`), then in the manager choose `work`, `Enter`: the tab is
   back. Close the window, reopen `subak-gui.exe %E%`: hidden tabs stay hidden.
6. In the manager: `F2` on `빌드`, type `작업`, `Enter`: `subak.exe %E% ls` shows it.
7. `subak.exe %E% new busy -- ping -n 1000 127.0.0.1`; in the manager select it, `Delete`: the
   question names `PING.EXE` with Cancel focused; `Enter` keeps it; `Delete`, `Tab`, `Enter`
   ends it and nothing else.
8. Drag the icon near a screen edge: it snaps; release it, quit from its menu, start it again:
   it is where you left it. Drag it to another monitor (with another scale): it is 40 logical px
   there. Unplug (or disable) that monitor: the icon comes back onto the remaining one.
9. Right click → "Always on top" off: other windows can cover it. "Show tray icon" on: a tray
   icon with the same tip; click it: the manager opens near it; `Win+B`, arrows, `Enter` on it:
   the manager opens with the keyboard. "Show floating icon" off: only the tray remains.
10. Narrator (`Ctrl+Win+Enter`) on the manager: it reads "Search, edit", "Terminals, list", each
    row in words, the buttons by name, and the delete question.
11. Turn on a Contrast theme: the icon is an outline, the manager uses system colours.
12. Quit from the menu: every Terminal still runs (`subak.exe %E% ls`). Clean up:
    `subak.exe %E% host stop --kill-terminals --yes`.

| Manual check | Result |
| --- | --- |
| Visible icon, no focus stealing, no-Host state, New Terminal with a Host started on demand (steps 1–3) | **pending — maintainer** |
| Search with the Korean IME, open in an existing window with the foreground handed on, hidden tabs (steps 4–5) | automated off-screen (§11.10; IME not); visible run **pending — maintainer** |
| Rename and the guarded delete (steps 6–7) | automated off-screen; visible run **pending — maintainer** |
| Real drags, snapping, saved place, real monitors and DPI, unplugging (step 8) | automated with window messages on synthetic layouts; real hardware **pending — maintainer** |
| Always on top, the real tray icon and its keyboard selection (step 9) | the tray is a fake in tests; **pending — maintainer** |
| Narrator (step 10) | the accessibility tree read with MSAA from the off-screen manager (ADR 0045 evidence); Narrator **pending — maintainer** |
| High Contrast (step 11) | **pending — maintainer** |

### 11.10 Tests

| Test | What it checks |
| --- | --- |
| `crates/subak/tests/gui_float.rs` (Windows; the off-screen floating UI driven by its own window messages, real Hosts on private endpoints) | `the_icon_starts_only_on_request_shows_no_host_and_follows_the_cli_without_starting_one` (tool window that never activates, no Host started, the no-Host manager, hover retry, CLI changes followed, quit ends nothing); `the_manager_searches_opens_renames_and_deletes_by_id_asking_first` (accessible names, search, typing from the list, open in a new then an existing window with the foreground request, rename in Korean, the guarded delete for busy and idle Terminals, keys during a question reach nothing); `new_terminal_from_the_manager_starts_a_host_on_demand_and_opens_it`; `drag_snaps_and_saves_and_display_and_dpi_changes_keep_the_icon_on_a_monitor` (posted mouse messages, snapping, saved place across runs, a synthetic two-monitor layout, `WM_DISPLAYCHANGE` after a monitor goes, DPI 192, the manager inside the work area, an on-screen layout refused); `repeated_quick_open_close_leaks_nothing_and_the_idle_icon_and_manager_cost_no_cpu`; `the_tray_is_a_fake_in_tests_and_the_menu_switches_pin_and_presence_and_quits_ending_nothing`; `a_second_start_forwards_to_the_running_one_and_neither_cli_nor_host_starts_it`; `hidden_tabs_persist_across_runs_and_the_manager_shows_a_hidden_terminal_in_its_window` |
| `crates/subak/src/gui/float/*.rs` unit tests | placement with synthetic DPI and monitor layouts (default place, snapping, clamping, recovery, the manager's rectangle), the picture (tile, glyph, badges per state, every size, High Contrast), Claude aggregation, the manager model (no Host and retries, New Terminal starting a Host, events from other clients, search with Hangul folding, selection, rename by id, the delete question and its answers), `float.json` and its schema, the fake tray, `--test-monitors` |
| `e2e/tests/step18_floating_manager.rs` (ignored; `cargo xtask e2e` only) | the floating manager following a Host it did not start, search, open, `Ctrl+N`, rename, the guarded delete; drag, saved place, a monitor going away |

### 11.11 Known limits (Step 18)

- The watermelon glyph is provisional (Step 44 owns the art and the Claude motion); there is no
  Claude state yet (Steps 26–28), so the badge shows Host and failure states only.
- No hover tooltip window on the floating icon (its accessible name and the tray tip carry the
  words); no `gui.show` global hotkey (`subak-gui --manager` with a link's shortcut key instead);
  no sign-in start.
- The manager has no "Stop" (a stopped Terminal cannot be restarted before Step 30) and no
  multi-select.
- Opening a Terminal picks the frontmost window of the Host; choosing a particular window is
  Step 21's.
- The manager uses Segoe UI through GDI; the list box's scroll bar is the classic one.
