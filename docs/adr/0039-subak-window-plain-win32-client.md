# ADR 0039 — The Subak window: a plain Win32 IPC client of one Terminal

Status: **Accepted** · Date: 2026-11-04 · Step 16 · Follows [ADR 0036](0036-terminal-emulation-and-rendering-path.md)
decision 4, refines [ADR 0014](0014-gui-technology.md) (chrome toolkit) for the Subak window,
implements the process structure of [ADR 0009](0009-gui-clients-and-keyboard-dispatch.md) for one
window, uses [ADR 0005](0005-local-ipc.md)/[ADR 0019](0019-secure-host-supervisor-and-ipc-v1.md)
as a client of kind `gui` · Details, user guide and manual checks: [SUBAK_GUI.md](../SUBAK_GUI.md)

## Context

Step 16 builds the first Subak window: a native Windows app that connects to the per-user Host,
shows one existing Terminal's pane live, and is never needed by the Host or the CLI. It must be
light when idle (`KIM-NFR-001`, `KIM-NFR-002`, `SUBAK-NFR-001`), handle keyboard, IME (Korean),
focus, per-monitor DPI, minimize and maximize, show clear loading/empty/error/disconnected
states, keep display preferences, and survive its own close or crash without touching the Host
(`SUBAK-FR-003`, `SUBAK-FR-015`). Tabs (Step 17), the floating icon and manager (Step 18),
search and clipboard UX (Step 19) and visual polish and settings (Step 20) build on it.

ADR 0036 measured idle cost off-screen: the `subak-term` CPU renderer with a plain GDI window at
3.40 MiB private and 1 thread, egui + glow at 107.0 MiB private and 12 threads, and told this Step
to build on plain Win32 or justify the GPU stack.

## Decision

1. **Plain Win32, no GUI framework, no GPU.** One top-level window on one thread; the pane is
   drawn by the `subak-term` renderer into a CPU frame and presented with `SetDIBitsToDevice`;
   only damaged rows are invalidated. State screens, the banner and the inline IME composition
   are drawn by the same renderer (the terminal font chain, so Korean names look like the pane).
   The window's own chrome is the native caption (dark mode through DWM; caption/text/border
   colours on Windows 11). *Measured* (§Evidence): no reason to accept the GPU stack's cost.
2. **Binary `subak-gui.exe`, built by the `subak` package.** The package becomes a library with
   two thin binaries (`subak`, `subak-gui`), so the window shares the client SDK — pipe, identity
   checks before sending anything, IPC v1, start on demand — instead of a second implementation,
   and `cargo build` puts `subak-gui.exe` next to `subak.exe`, which it starts on demand.
   `subak-gui` is a GUI-subsystem program (no console window); `--help` attaches to the parent
   console. The Host never calls the GUI code or the renderer; the package now builds
   `subak-term` with `render` (ADR 0036 said the Host builds without it), and the linker leaves
   the unused renderer out of `subak.exe` (§Evidence). No GUI crate enters the package
   (`cargo xtask deps-check`).
3. **A pure client.** It says `{"kind": "gui", "name": "subak-gui"}`, verifies the Host like the
   CLI, and only lists, subscribes, attaches interactively at its grid size, resizes and
   detaches. It never creates, stops, kills or removes a Terminal in Step 16 (`Ctrl+T`/`Ctrl+W`
   are Step 17). A Host it starts on demand runs **no shell** (`--no-shell`), so opening the
   window never starts a duplicate shell. Which Terminal: the one named on the command line, else
   the one shown last if it still runs (`gui.json`, by id, then name), else the only running
   one, else the user chooses from a list; with none, the empty screen opens the next Terminal
   created by any client.
4. **The conversation is a tested state machine** (`gui::model`): phases `connecting`, `empty`,
   `choosing`, `attaching`, `attached`, `ended` (exited, detached by another client, stopped,
   removed — the last screen stays with a banner and Enter acts), `disconnected` (the last screen
   stays with a banner; retries after 0.5, 1, 2, 4, 8 s, then every 10 s, **never starting a
   Host**; reattaches the same Terminal by id when a Host serves it again, or says it is gone),
   and `failed` (the first connection: the stable error code, the message, Enter retries).
5. **Network on two blocking threads.** A reader thread connects (identity checks, handshake)
   and reads frames; a writer thread writes frames from a channel. They wake the window with one
   posted message per batch. Idle, both wait in the kernel; the only timer the window ever arms is
   the reconnect delay while disconnected, a 150 ms one-shot while a program holds synchronized
   output (mode 2026), and the test hook's lifetime limit.
6. **Keyboard dispatch (ADR 0009 order).** In the message loop, before `TranslateMessage`: the IME
   guard (composition or `VK_PROCESSKEY` → the IME), then the window's shortcuts, then cursor,
   editing and function keys for the program in the xterm encoding of `subak-term`; characters
   come from `WM_CHAR`/`WM_SYSCHAR` (Alt → ESC prefix, AltGr text kept). Handled keys never become
   characters. Alt+F4 and Alt+Space keep their Windows meaning; a lone Alt or F10 does not open
   the (absent) menu bar. The `Ctrl+B` prefix is Step 23's: Step 16 sends `Ctrl+B` to the
   program. win32-input-mode is **not** granted (as the CLI): keys xterm cannot express, such as
   Ctrl+Break, are not sent yet.
7. **Shortcuts in the hotkey contract's syntax** (context `terminal`): Windows Terminal's
   defaults for copy (`Ctrl+Shift+C`, `Ctrl+Insert`), paste (`Ctrl+Shift+V`, `Shift+Insert`),
   text size (`Ctrl+Plus`/`Ctrl+Minus`/`Ctrl+0` and the numpad keys) and scrolling
   (`Ctrl+Shift+PageUp/PageDown/Up/Down/Home/End`), parsed and mapped by `kim-hotkeys` and checked
   against its reserved keys. They are fixed until Step 17 adds Subak's manifest and
   `subak hotkeys`.
8. **IME composition is drawn inline** (DESIGN_SYSTEM §4.4): the window handles
   `WM_IME_COMPOSITION` itself (the IME's own composition window is turned off), draws the
   composition underlined on `bg.overlay` at the cursor, places the candidate list below the
   cursor, and sends only committed text.
9. **Preferences** in `%APPDATA%\mouv\subak\gui.json` (schema `subak-gui-v1`): text size, theme
   choice (`auto` follows Windows High Contrast), the restored window rectangle with its DPI and
   the maximized flag, and the last Terminal (id and name, a hint). Independent of the Host's
   state; no process ids, content or secrets. Written atomically and only on a change (move or
   resize ended, maximize, zoom, another Terminal, close); unknown keys kept; an invalid value
   falls back alone; a file that is not JSON or from a newer version is used as defaults and never
   rewritten, with a one-time banner. A saved rectangle is used only if a title bar's worth of it
   lies on a current monitor, rescaled from its DPI to that monitor's.
10. **Per-monitor DPI v2.** `WM_DPICHANGED` re-rasterizes glyphs at the new scale (never bitmap
    scaling) and takes Windows' suggested rectangle; the grid follows the client area and the pane
    follows the grid. A minimized window (a 0 × 0 client area) never resizes the pane.
11. **Test hooks only in debug builds** (or with the `test-hooks` feature, used only to measure a
    release build): `--test-offscreen` places the window at (-20000, -20000) as a tool window
    (no taskbar button, not in Alt+Tab) with `WS_EX_NOACTIVATE`, shown with `SW_SHOWNOACTIVATE`;
    it reports its state as JSON lines, takes `WM_COPYDATA` test commands, uses a private
    clipboard, writes its own composed frames to BMP on request, and closes itself after
    `--test-max-seconds`. Shipped builds behave normally.

## Evidence (this machine: Windows 10 Pro 22H2 19045, i5-13600KF)

- Idle, off-screen, attached to a live cmd.exe Terminal with no output: **release build, 30 s:
  0.0 ms CPU, 3.95 MiB private, 11.3 MiB working set, 5 threads, 154 handles**; debug build,
  10 s: 0.0 ms CPU, 4.06 MiB private, 6 threads
  (`tests/gui_window.rs::idle_window_costs_no_cpu_and_little_memory`). Step 15's probe (renderer
  + GDI window, no IPC): 3.40 MiB, 1 thread; egui + glow: 107.0 MiB, 12 threads. The extra
  threads are the reader, the writer and the system's input-method threads.
- Binaries (release): `subak-gui.exe` 917 KB; `subak.exe` 1.57 MB, whose import table names no
  window, GDI or IME function (`CreateWindowExW`, `CreateDIBSection`, `GetFontData`,
  `SetDIBitsToDevice`, `ImmGetContext`, `gdi32`), while `subak-gui.exe` names them all.
- 8 integration tests drive real Hosts on private endpoints with the off-screen window; the
  library's unit tests cover the model, screens, layout, keys, preferences and golden frames.

## Consequences

- Subak's GUI Steps keep plain Win32: tabs (Step 17) are drawn by the window itself, as the
  banner is now; the integrated tab bar of DESIGN_SYSTEM §3.5 needs custom non-client drawing
  then. ADR 0014's egui chrome is not used for Subak (Kim Manager keeps it).
- UI text is drawn in the terminal font (mono). A proportional UI font for chrome (Segoe UI
  Variable) needs a second rasterizer path (Step 20).
- The window has no accessibility tree yet (UI Automation for a custom-drawn terminal is
  Step 20/39 work; Windows Terminal implements `ITextProvider`). Named in SUBAK_GUI §8.
- Colour emoji stay monochrome (ADR 0036); cursor blinking is not animated (no idle timer;
  reduced motion is honoured trivially).

## Not tested (and why)

- A **visible** window on the maintainer's desktop, real keyboard and IME input, real
  monitor-to-monitor moves, maximize/minimize through the caption: agents never show windows or
  synthesize input on this machine. The manual script is [SUBAK_GUI §9](../SUBAK_GUI.md).
- Windows 11 caption colours (this machine is Windows 10).

## Alternatives considered

- **egui + glow for the window** (ADR 0014): ~100 MiB private idle for chrome alone, 12 threads,
  an OpenGL driver; the terminal grid would still be our renderer. Rejected (ADR 0036 decision 4).
- **`subak gui` as a subcommand of `subak.exe`**: one binary, but a console-subsystem program
  flashes a console window when started from Explorer or a shortcut, and the Host's image would
  carry the window. Rejected for a separate GUI-subsystem binary in the same package.
- **A separate `subak-gui` package depending on `subak`**: its tests could not start `subak.exe`
  (Cargo builds another package's binaries only as artifact dependencies, unstable), and the
  sibling lookup for start on demand would not hold in `target/`. Rejected.
- **Driving `subak attach` as a child process** (the Step 15 harness): an extra process per
  window, the CLI's detach keys intercepting `Ctrl+B d`, no Terminal list or events. Rejected.

## Requirements

`SUBAK-FR-003`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-FR-037`, `SUBAK-FR-046`,
`SUBAK-NFR-001`, `SUBAK-NFR-002`, `SUBAK-SEC-010`, `KIM-NFR-001`, `KIM-NFR-002`,
`KIM-NFR-011`, `KIM-NFR-012`, `KIM-NFR-013`.
