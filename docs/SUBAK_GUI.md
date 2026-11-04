# The Subak window (`subak-gui`)

Status: **current** (implemented, Step 16; the visible-window checks of §9 are pending the
maintainer) · Decision: [ADR 0039](adr/0039-subak-window-plain-win32-client.md) · Builds on
[TERMINAL_EMULATOR](TERMINAL_EMULATOR.md) (Step 15), [IPC_PROTOCOL](contracts/IPC_PROTOCOL.md),
[SESSION_MODEL](SESSION_MODEL.md), [DESIGN_SYSTEM](DESIGN_SYSTEM.md) · Requirements:
`SUBAK-FR-003`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-037`, `SUBAK-FR-046`,
`SUBAK-NFR-001`, `SUBAK-NFR-002`

`subak-gui.exe` is a small native window that shows **one Subak Terminal** live. It is a client
of the per-user Host, like `subak attach`: the Terminal and its programs live in the Host, so
closing the window — or the window crashing — never ends them. The Host and the `subak` CLI work
without it. Tabs (Step 17), the floating icon and session manager (Step 18), search and clipboard
UX (Step 19) and visual polish and settings (Step 20) come next.

## 1. Starting it

```text
subak-gui [<terminal>] [--endpoint <name>]
subak-gui --help | --version
```

| Situation | What the window does |
| --- | --- |
| `<terminal>` given (a name, case ignored, or a `t-` id) | Opens it. If it does not exist or does not run, says so and lists the running ones. |
| No `<terminal>`, the Terminal shown last still runs | Reopens it (remembered by id, then by name, in `gui.json`). |
| Exactly one Terminal runs | Opens it. |
| Several run | Lists them: `↑`/`↓` (or a click) selects, `Enter` (or a double click) opens. |
| None runs | "No Terminal is running in the Host" — the next Terminal any client creates (`subak new`) opens by itself. |
| No Host runs | Starts one, as the CLI does (same start lock), **without a shell**: opening the window never starts a shell of its own. |
| The endpoint is held by something that is not a Subak Host of this user, or no Host can start | An error screen with the stable code (`host_identity_mismatch`, `spawn_failed`, …), the message and `Enter` to retry. Nothing was sent to the impostor. |

The window never creates, stops, kills or removes a Terminal in Step 16. To create one, use
`subak new`; `Ctrl+T` and `Ctrl+W` arrive with tabs in Step 17.

`subak-gui.exe` must sit next to `subak.exe` (it starts that program on demand). `--endpoint`
selects a private Host endpoint, as `subak --endpoint` does.

## 2. Closing, crashing, reconnecting

| Event | Effect |
| --- | --- |
| Close button, Alt+F4, the window menu's Close | A **detach**: the Terminal keeps running (`subak ls`). |
| The window's process ends (crash, Task Manager) | The Host drops the connection; nothing else changes. |
| Another client detaches the window (`subak detach <terminal>`) | The last screen stays, with a banner "Detached by cli client (pid …); `x` keeps running. Enter: reattach". |
| The program exits, or the Terminal is stopped or removed | The last screen stays, with a banner saying what happened and the exit code; `Enter` lists the running Terminals. |
| The Host goes away (stopped, crashed) | The last screen stays, with a banner "Disconnected: …. Reconnecting to `x` (attempt n)…". The window retries after 0.5, 1, 2, 4, 8 s, then every 10 s, and **never starts a Host while reconnecting**. When a Host serves the endpoint again it reattaches the same Terminal by id if it runs, else lists what runs and says the old one is gone. `Enter` retries at once. |

A Host stops its Terminals' programs when it ends (ADR 0003), so a reconnect after a Host crash
finds new Terminals at most; restoring them is Steps 29–31.

## 3. Keyboard, IME and mouse

Keys go to the program, in the order of ADR 0009:

1. **IME guard.** While an input method composes (Korean, Japanese, Chinese), keys belong to it.
   The composition is drawn inline at the cursor (underlined, on `bg.overlay`), the candidate
   list opens below the cursor, and only committed text is sent.
2. *(The `Ctrl+B` prefix table is Step 23. Until then `Ctrl+B` goes to the program.)*
3. **The window's shortcuts** (Windows Terminal's defaults; fixed until Step 17 makes them
   rebindable with `subak hotkeys`):

   | Action id | Keys | What |
   | --- | --- | --- |
   | `clipboard.copy` | `Ctrl+Shift+C`, `Ctrl+Insert` | Copy the selection (plain text, CR LF line ends) |
   | `clipboard.paste` | `Ctrl+Shift+V`, `Shift+Insert` | Paste: line breaks become Enter, control characters are removed, bracketed when the program asked (`SUBAK-SEC-013`) |
   | `view.zoom-in` / `view.zoom-out` / `view.zoom-reset` | `Ctrl+Plus` / `Ctrl+Minus` / `Ctrl+0` (and the numpad `+`, `-`, `0`) | Text size 8–36 px (default 13), saved |
   | `scroll.page-up` / `scroll.page-down` | `Ctrl+Shift+PageUp` / `PageDown` | Scroll the history a page |
   | `scroll.line-up` / `scroll.line-down` | `Ctrl+Shift+Up` / `Down` | Scroll a line |
   | `scroll.top` / `scroll.bottom` | `Ctrl+Shift+Home` / `End` | Oldest line / live screen |

4. **Everything else** goes to the program in the xterm encoding (TERMINAL_EMULATOR §4.1):
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
always selects, as in xterm. Over the list of Terminals a click selects and a double click opens.

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
  screens use the system's window, text and highlight colours and no lilac; the chooser marks the
  selection in reverse video. It follows changes while the window is open.
- **States are never blank and never colour-only** (DESIGN_SYSTEM §3.8): connecting ("Connecting
  to the Subak Host…" and the endpoint), opening, empty, the chooser (the selected row has a `›`
  and a lilac marker), the error screen (code, message, hint, "Press Enter to try again"), and
  the banners of §2 (a one-line strip in the status colour at 12 % over `bg.raised`, a 3 px marker,
  words that say what happened and what `Enter` does).

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
| Model | `crates/subak/src/gui/model.rs` | The Host conversation as a state machine without I/O: which Terminal to attach, phases, snapshots and resyncs, the pane's end, reconnects with backoff |
| Screens | `gui/screens.rs` | VT text of the state screens and banners in the design tokens, and their High Contrast form |
| Layout | `gui/layout.rs` | DPI scale, grid ↔ client area, saved placements on monitors |
| Keys | `gui/keys.rs` | The shortcut table (hotkey-contract syntax, via `kim-hotkeys`), key and character mapping |
| Preferences | `gui/prefs.rs` | `gui.json` |
| Composition | `gui/compose.rs` | The client area from its layers (also what test shots and goldens use) |
| Network | `gui/net.rs` | A reader thread (connect or start, identity checks, frames in) and a writer thread (frames out) |
| Window | `gui/window.rs` | Win32: paint, keyboard, IME, mouse, clipboard, DPI, High Contrast, test hooks |
| Binary | `crates/subak/src/bin/subak-gui.rs` | GUI subsystem (no console window) |

Data path: the reader queues frames and posts one message per batch; the window feeds them to the
pane's `TermView` (the same model as the Host), renders only changed rows, and invalidates only
those. A snapshot at attach (or after a resync) starts a fresh view at the snapshot's size; once
complete, the view follows the window's grid. The interactive view answers the program's device
reports. Synchronized output (mode 2026) holds the frame until the program ends its update, at
most 150 ms.

Safety: the window is an IPC client of kind `gui` with the CLI's identity checks (same user,
`subak.exe`, before anything is sent); it never kills, never starts a shell, never touches another
user's Host, never opens links (OSC 8 hit-testing only; opening is Step 19), never honours OSC 52,
and never writes the system clipboard except for an explicit copy.

## 7. Measurements

This machine: Windows 10 Pro 22H2 19045, Intel Core i5-13600KF, 2026-11-04.

| What | Result |
| --- | --- |
| Idle, release build (`--features test-hooks` for the off-screen flag), attached to a live cmd.exe Terminal, 30 s, no output (`step16_idle.ps1`, the Step 15 sampler's quantities) | **0.0 ms CPU, 3.95 MiB private, 11.3 MiB working set, 5 threads, 154 handles** |
| Idle, debug build, same, 10 s (`tests/gui_window.rs::idle_window_costs_no_cpu_and_little_memory`) | 0.0 ms CPU, 4.06 MiB private, 12.6 MiB working set, 6 threads, 153 handles |
| Step 15 references (ADR 0036) | renderer + GDI window, no IPC: 3.40 MiB private, 1 thread; egui + glow: 107.0 MiB private, 12 threads |
| Binary sizes (release, shipped features) | `subak-gui.exe` 917 KB; `subak.exe` 1.57 MB |
| `subak.exe` carries no window or renderer | its import table names none of `CreateDIBSection`, `GetFontData`, `SetDIBitsToDevice`, `CreateWindowExW`, `ImmGetContext`, `gdi32` (checked with `grep -c -a` on the release binary); `subak-gui.exe` names all of them |

The threads beyond Step 15's one: the reader and writer (blocked in the kernel when idle), and
the system's input-method threads.

## 8. Known limits (Step 16)

- One Terminal per window; no tabs, `Ctrl+T`/`Ctrl+W`, session manager or floating icon
  (Steps 17–18).
- Shortcuts are fixed; `hotkeys.json` and `subak hotkeys` for Subak are Step 17. No `Ctrl+B`
  prefix (Step 23).
- No multi-line paste warning, search, link opening, copy-as-HTML (Step 19).
- win32-input-mode is not granted: Ctrl+Break and key releases do not reach programs.
- UI text uses the terminal font; the integrated tab bar, icons, settings UI and themes are
  Steps 17 and 20. The taskbar shows the generic application icon.
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

| Manual check | Result |
| --- | --- |
| Visible window, rendering, Korean (steps 1–2) | **pending — maintainer** |
| Korean IME composition and Hanja list (step 3) | **pending — maintainer** |
| Keyboard and focus (steps 4, 6) | **pending — maintainer** |
| Mouse and clipboard (step 5) | **pending — maintainer** |
| Maximize/minimize and DPI across monitors (steps 7–8) | **pending — maintainer** |
| High Contrast (step 9) | **pending — maintainer** |
| Close, crash, detach, reconnect (steps 10–12) | automated off-screen (§10); visible run **pending — maintainer** |

## 10. Tests

| Test | What it checks |
| --- | --- |
| `crates/subak/tests/gui_window.rs` (Windows; the debug build's off-screen window driven by its own messages) | attach to a CLI-created Terminal: same pane pid, no second cmd.exe, one pane, a `gui` client, typed and Korean input reaching the shell (window and `subak capture`); close (WM_CLOSE) and crash (TerminateProcess) leave the Host and the pane with the same pids, a new window reopens the remembered Terminal with its screen; `subak detach` → banner → Enter reattaches; a stopped Host → disconnected banner, no Host started while retrying, a returning Host reconnected; empty, chooser and error (a pipe squatter) screens; paste, copy, wheel, cursor keys, IME composition and commit, focus; synthetic DPI 192/96, resize, minimize, zoom, `gui.json` persistence and its error rules; frames of the window's own content (`SUBAK_GUI_SHOTS=<dir>` keeps them as BMP); idle CPU and memory |
| `crates/subak/src/gui/*.rs` unit tests | the model (attach decisions, snapshots, resyncs, ends, backoff), screens and banners (words, actions, High Contrast, tokens = `design/tokens.json`), layout with synthetic DPI 96–288 and monitor layouts, shortcut parsing and reserved keys, character mapping, preferences (sandbox config dir, unknown keys, invalid and newer files, the schema), golden frames |
| `crates/subak/tests/golden/gui_*.txt` | golden frames (built-in rasterizer) of the connecting, empty, chooser and error screens at 100 % and 200 %, the banner over a pane, the inline IME composition; `SUBAK_BLESS=1` refreshes them |
| `e2e/tests/step16_subak_window.rs` (ignored; `cargo xtask e2e` only) | the release window against a release Host: attach without a new shell, input, close and crash keep the pane, reconnect |
