# ADR 0049 — The Ddalgak rule editor: its own on-demand process, egui + glow, store writes and a confirmed reload

Status: **Accepted** (Step 74; visible-window usability pending the maintainer) · Date: 2026-11-04 ·
Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md) §3 (process model) · Applies
[ADR 0014](0014-gui-technology.md) (GUI technology) · Builds on
[ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md) (pipe, single instance) and
[ADR 0028](0028-ddalgak-rule-store-validation-and-reload.md) (rule store, live reload)

## Context

Step 74 draws the visual rule editor of [DDALGAK_GUI_JOURNEYS](../DDALGAK_GUI_JOURNEYS.md) on the
data models Steps 68–73 shipped. Three things were open:

1. **Toolkit.** ADR 0014 chose egui + glow for Kim Manager and Subak's chrome; Subak's window then
   went plain Win32 (ADR 0039) for its terminal grid, and Noeul is plain Win32 (ADR 0023). Nothing
   bound the Ddalgak editor. It needs text fields with IME, multi-line Hangul, combo boxes, drag
   and drop, a screen map, accessibility (UI Automation) and headless flow tests
   (`egui_kittest`, which GUI_JOURNEYS §6 names).
2. **Process.** ADR 0022 §3 says `ddalgak-gui.exe` "owns the engine, the editor and the
   playground, opening windows only on demand". But the tray app's main thread sleeps in its own
   `GetMessageW` loop (hidden window, `RegisterHotKey`, tray icon, session notifications), an
   egui/winit window wants to own its thread's event loop, and `DDALGAK-NFR-003` budgets the tray
   app at < 25 MB with no window open. `cargo xtask deps-check` keeps the whole `ddalgak` package
   (both of its binaries) free of GUI crates, and `ddalgak.exe` (the headless CLI) must never link
   one.
3. **Truth.** `DDALGAK-FR-052`: never show a rule as saved or active before it was committed and
   loaded, while the CLI, another editor or a text editor may change `rules.json` at any time.

## Decision

1. **A third binary in its own package.** The editor is `ddalgak-editor.exe`, package
   `crates/ddalgak-editor` (library + binary), depending on the `ddalgak` library and on
   `eframe` 0.37 with glow and AccessKit, the exact pin Kim Manager uses (no new crate in the
   lock file). The `ddalgak` package stays on the headless list of `deps-check`; a test there now
   also asserts that `ddalgak-editor` does use egui and `ddalgak` does not.
2. **Its own process, on demand.** The tray app starts it from its menu (*Open rule editor*) and
   from Ddalgak's `editor.open` shortcut, which `ddalgak-gui` now registers with `RegisterHotKey`
   like `engine.toggle` (`status` reports it `registered` or `taken`; `unavailable` remains only
   in the schema for older apps). The editor is looked up beside `ddalgak-gui.exe`
   (`ddalgak::editor_path`) and started with null standard handles; the debug test host never
   starts it. The user can also start it directly; it works without the tray app (it then says
   "Ddalgak is not running"). One editor per user session: a named mutex marks the owner, a named
   event asks it to bring its window forward; the second process allows that
   (`AllowSetForegroundWindow`) and exits. Closing the window ends the process and frees its
   memory. The editor never starts the tray app, never turns the engine on by itself (its
   *Turn on* button sends `engine.start` exactly like the CLI, on the user's click), never installs
   a hook, injects input, opens a program or a website, or reads other windows.
3. **egui + glow** (ADR 0014 decision 1) for the window, with the Kim tokens (near-black
   `bg.base`, lilac accent and focus ring, 12/13 px text, 28 px controls) and the Malgun Gothic and
   Segoe UI Symbol fallback from the system fonts. Every control has an accessible name (the flow
   tests fail on an unnamed one); visible labels name their fields with `labelled_by`.
4. **Writes are store transactions**, the same as `ddalgak rule …`: lock, re-read, apply one
   operation to the fresh document, validate before and after and refuse an introduced error
   (`edit::review`, `mode_edit::review` for mode changes), save atomically with backup and a
   history snapshot of origin `gui`. List operations (enable, disable, delete, duplicate, move,
   import) apply to the current file, so a CLI change between two clicks is never lost; a form save
   refuses with `rule_conflict` when the rule itself changed since the form opened. Consent from
   the editor's dialog is recorded `via: "gui"` (launches with the fingerprint of the exact
   targets, which the dialog names). **Undo** restores the document as it was before one of the
   editor's own changes, as a new revision, and only while the file is still at the revision that
   change saved.
5. **Truthful sync.** After every save, and whenever the folder watcher (the engine's
   `FindFirstChangeNotificationW` watcher; no polling) sees `rules.json` change, the editor asks the
   running app `rules.reload` over the identity-checked pipe (the client moved into
   `ddalgak::win::client`, shared with the CLI). The status line says *saved and active (revision
   N)* only when the reply's active revision is N, the saved and active rules are in sync and the
   engine runs; otherwise *saved; waiting for Ddalgak to load it*, *saved and loaded; the engine is
   off/paused*, *saved; Ddalgak is not running*, *saved; Ddalgak kept its previous rules* (with
   the reason) or *saved; Ddalgak did not confirm the reload*. A row says *active* only in the
   first case. IPC runs on one worker thread; a reply wakes the window.
6. **Validation off the UI thread's lifetime.** The schema validator keeps its compiled patterns
   and their search caches per thread; the editor reads and commits on short-lived scoped threads,
   so that memory is freed when the work ends (measured: 152 → 96 MiB working set with 100 rules).
7. **Test is the simulation.** The editor's *Test* (and `F5`) describes what the rule would do with
   the engine's own previews (remap, text, launch, mouse on a simulated pointer, mode); it never
   types, opens, or moves anything. The full playground with live tests is Step 76.

## Alternatives

- **The editor as a window of `ddalgak-gui.exe`** (ADR 0022 §3 literally). Rejected: the tray
  app would link egui/winit/glow (the `ddalgak` package leaves the headless list, and
  `ddalgak.exe` shares that package), two event loops would compete for the main thread or the
  window would live on a second UI thread, and the GL driver's memory (~55 MiB, ADR 0014) would
  stay in the engine process after the window closes, against the < 25 MB tray budget.
- **`ddalgak-gui.exe --editor`** (one binary, two processes, as Subak's floating icon in ADR
  0045). Rejected for the same package reason: the GUI crates would be linked into the tray app's
  binary even when unused.
- **Plain Win32 like Subak's window and Noeul.** Lowest memory, but forms, IME text fields, combo
  boxes, drag and drop and UI Automation would all be built by hand, and there is no headless test
  harness; kept as ADR 0014's fallback B if the memory budget below cannot be met.
- **wgpu instead of glow.** ADR 0014 measured 4× the memory and a 1.6 s start. Rejected.
- **Showing "saved" when the file is written.** Rejected by `DDALGAK-FR-052`; the reload answer is
  the acknowledgement.

## Consequences

- The engine's idle cost and memory do not change: the tray app has no GUI code, only a menu item
  and one more `RegisterHotKey`. The editor costs nothing while closed.
- Measured on the reference machine (Windows 10 19045, release build, glow, window off-screen and
  not activated, `crates/ddalgak-editor/examples/measure.rs`): first frame **157–288 ms**; CPU while
  idle **0 ms** and **0 repainted frames** over 30 s; working set **76 MiB** with no rules,
  **96 MiB** with 100 rules, **99 MiB** with 1000 rules (private 107–128 MiB); Kim Manager's preview
  on the same machine **74 MiB**. `DDALGAK-NFR-003` ("editor open: < 80 MB") is therefore **met
  only for an empty editor**; with rules the window is ~20 MiB over. The budget belongs to
  milestone D6 (Step 82), which either trims it (font sizes and atlas, row text caching) or takes
  ADR 0014's fallback B.
- One more binary to ship: the release packaging and the Kim catalog entry (Step 82) must include
  `ddalgak-editor.exe` beside `ddalgak-gui.exe`; without it *Open rule editor* says it is not
  installed.
- The CLI's IPC client moved into the library unchanged in behavior (same codes and messages).
- Visible-window behavior (focus handover to an already open editor, the real tray menu, IME in
  the real window, screen readers) is verified by the maintainer's manual script
  (GUI_JOURNEYS §8), not by the automated tests.

## Requirements

`DDALGAK-FR-050`, `DDALGAK-FR-051`, `DDALGAK-FR-052`, `DDALGAK-FR-053`, `DDALGAK-FR-032`,
`DDALGAK-FR-060`, `DDALGAK-FR-061`, `DDALGAK-NFR-001`, `DDALGAK-NFR-003`, `DDALGAK-NFR-007`,
`DDALGAK-SEC-013`, `KIM-NFR-012`.
