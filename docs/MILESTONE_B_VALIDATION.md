# Milestone B validation: the Subak window, its settings and the user journey

Status: **current** (Step 20, issue #20) · Validates [ADR 0039](adr/0039-subak-window-plain-win32-client.md),
[ADR 0042](adr/0042-subak-window-tabs-and-safe-close.md),
[ADR 0045](adr/0045-subak-floating-icon-and-terminal-manager.md),
[ADR 0048](adr/0048-subak-search-clipboard-and-links.md) and
[ADR 0051](adr/0051-subak-settings-profiles-and-appearance.md) · Requirements: `SUBAK-FR-002`,
`SUBAK-FR-003`, `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-031`,
`SUBAK-FR-037`, `SUBAK-FR-038`, `SUBAK-FR-046`, `SUBAK-FR-048`, `SUBAK-FR-049`, `SUBAK-NFR-001`,
`SUBAK-NFR-002`, `KIM-NFR-012`

> **Milestone B is not complete.** The window (Steps 16–20) is validated on real Windows with
> real Hosts, real ConPTY panes and the window's own off-screen test mode, but its **visible**
> parts — the window on the desktop, the real keyboard and IME, the real clipboard, native menus,
> Windows' own High Contrast, light-mode and animation settings, Narrator, a real pointer — need a
> visible desktop. Agents may not show windows on the maintainer's machine, so those checks are
> **pending — maintainer** (§6, with the results table §6.3). Visual judgement (coherent,
> polished) is the maintainer's. Everything else here was measured on this machine, as stated row
> by row; "simulated" means the window received the Windows value through its test command.

## 1. Result

| Acceptance criterion (issue #20) | Status | Evidence |
| --- | --- | --- |
| The native Windows GUI is coherent, responsive, polished and keyboard-accessible without obscuring the work area | **partly verified**: responsiveness, keyboard access, contrast tokens and the frames are automated off-screen; the visible look and feel is **pending — maintainer** | Frames checked pixel by pixel for `kim-dark`, `kim-light` and High Contrast (`gui_settings.rs`); every action is a shortcut or in a native menu reachable with the Menu key; 150 declared WCAG pairs pass `design-check`; journey latencies §5; the window is a normal, resizable, non-topmost window (only the optional floating icon is always on top, ADR 0045) |
| Preferences survive app restart and do not affect running Host sessions unexpectedly | **verified** (automated, real Host) | `gui_settings.rs::preferences_survive_restart_migrate_from_version_1_and_never_touch_running_terminals`, `config_cli.rs::a_profile_launches_…_settings_never_touch_running_terminals`, `config_boundary.rs` (the Host never reads `config.toml`); the one expected effect — a new text size or line height re-lays out the shown pane's grid, as a zoom does — keeps the process |
| The Milestone B journey is tested on real Windows; remaining nonessential UX deviations are tracked | journey **verified headlessly** (off-screen windows, real Hosts, debug and release builds; §3, §5); the visible run **pending — maintainer** (§6); deviations **tracked** (§7) | `milestone_b.rs`, `e2e/tests/step20_subak_polish.rs`, §7 |

## 2. Scope and plan

Milestone B covers what a person does with the Subak window: install and open it, create, read,
rename and end Terminals (session CRUD), work in several tabs, close the window and reopen it,
and change how it looks and behaves. Not in scope: restore after reboot (Steps 29–31), splits
(Step 22), the prefix table (Step 23), the Claude indicator (Steps 26–28), Linux (M4).

| Journey | How it is checked here | Visible part |
| --- | --- | --- |
| Install | The two programs copied into a fresh folder and run from there (`milestone_b.rs`); the real per-user install by `kim install subak` needs a signed release and is **not exercised** | — |
| Open | `subak-gui` with no Host: it starts one without a shell and shows the empty state; with a Terminal it attaches | the window appearing |
| Session CRUD | `Ctrl+T` (default shell or default profile), `subak new` / `rename` / `ls` / `kill`, `Ctrl+W` on an idle shell, the busy question (Step 17 tests) | the tab bar and the question |
| Multi-tab | three tabs, `Ctrl+Tab` cycles, the screen comes back on each attach | the fade of the marker |
| Close and reopen | `WM_CLOSE` (the close button) is a detach; a new window reopens the last Terminal with the same processes and the earlier output | the window closing |
| Settings | CLI and menu changes, restart, a broken file, the `gui.json` v1 migration | the look of each theme |

## 3. Automated evidence

### 3.1 Commands

```text
cargo test -p subak --test milestone_b -- --nocapture          # the journey, debug build
cargo build --release -p subak --features test-hooks             # off-screen flag compiled in
set SUBAK_MILESTONE_B_BIN=<target>\release                       # (copy the two .exe elsewhere first)
cargo test -p subak --test milestone_b -- --nocapture            # the same journey, release build
cargo test -p subak --test gui_settings --test config_cli --test config_boundary
cargo test -p subak --test gui_window --test gui_float          # Steps 16–19, unchanged
```

### 3.2 The suite

| Test | Journey and checks |
| --- | --- |
| `crates/subak/tests/milestone_b.rs` | install (copy), open with no Host (Host started, no shell), `Ctrl+T` → attached shell, `subak new` ×2, rename to Korean, typing, three `Ctrl+Tab` switches with the screen back, `ls`, idle samples of the window and the Host, close (a detach: the same pids), reopen (three tabs, the earlier output), `subak kill` and `Ctrl+W` |
| `crates/subak/tests/gui_settings.rs` | themes and High Contrast in frames, font, line height, padding, opacity, motion and reduced motion, profiles, the settings menu, mouse and keyboard choices, hover, migration, restart persistence, a broken settings file, running Terminals untouched |
| `crates/subak/tests/config_cli.rs` | `subak config` and `subak profile` with golden files; a profile's exact argv, folder and environment |
| `crates/subak/tests/gui_window.rs`, `gui_float.rs` | Steps 16–19: attach, close/crash/detach/reconnect, tabs, the busy question, rename, shortcuts, search, clipboard, links, export; the floating icon and manager |
| `e2e/tests/step20_subak_polish.rs` (daily e2e only) | the same journey and the settings across a restart with the debug binaries |

### 3.3 Not exercised

- `kim install subak` (no signed Subak release in the catalog yet); the copy stands in for it.
- The visible window and every input that needs a real desktop (§6).

## 4. Failure handling checked on the way

| Situation | Result |
| --- | --- |
| A broken `config.toml` while the window runs | the defaults apply, a banner names the line, the zoom keys and the menu do not rewrite the file (`settings_not_saved`), the pane keeps its process |
| A newer `config.toml` | read-only, `config_from_newer_version` for every edit, never replaced |
| `gui.json` version 1 from Steps 16–19 | migrated once into `config.toml` where it was silent, the old file backed up, unknown keys kept |
| A profile with a secret-looking value | used (the user's file), warned about by name, shown as `***`; `import` refuses it |
| A font family that is not installed | the next one of the chain draws; the window says which |
| The Host stopped while a window runs, a crashed window, `subak detach` | unchanged since Steps 16–17 (`gui_window.rs`) |

## 5. Measurements (the Alpha baseline)

### 5.1 Environment

Windows 10 Pro 22H2 (19045.6466), Intel Core i5-13600KF, 2026-11-04; Rust 1.99.1 (GNU
toolchain). Off-screen test windows (`--test-offscreen`, the release build with
`--features test-hooks`); a private endpoint and `APPDATA`; the programs copied into a fresh
folder for each run (so each start is a first start of that copy, scanned by Defender).

### 5.2 Numbers

Release build, three runs of `milestone_b.rs` (the median, then the range):

| What | Result |
| --- | --- |
| Start to the window's `ready` (window created, laid out) | **101 ms** (100–102) |
| Start to the empty state with no Host running (the window starts the Host, which starts no shell) | **156 ms** (156–158) |
| `Ctrl+T` to an attached `cmd.exe` | **17 ms** (17–22) |
| `Ctrl+Tab` to the next tab attached (snapshot replayed) | **5 ms** (5–6 each) |
| Close (`WM_CLOSE`) to the process gone | **20 ms** |
| Reopen to the last Terminal attached | **33 ms** (33–35) |
| Idle window, attached, three tabs, 30 s | **0.0 ms CPU**, **4.94 MiB private** (4.91–4.97), 15.6 MiB working set, 5–6 threads, 196 handles |
| Idle Host with three `cmd.exe` Terminals, 30 s | **0.0 ms CPU**, **1.85 MiB private** (1.82–1.92), 6.1–7.6 MiB working set, 12 threads, 109 handles |
| Binaries (shipped features) | `subak.exe` 2 506 240 bytes, `subak-gui.exe` 3 370 496 bytes |

Debug build (the same test, for comparison): ready 66 ms, empty state 123 ms, `Ctrl+T` 17 ms,
switches 4–7 ms, close 20 ms, reopen 42 ms; idle window 0.0 ms CPU, 4.92 MiB private; idle
Host 0.0 ms CPU, 2.46 MiB private.

"0.0 ms CPU" means below the counter's resolution over the interval (the process accumulated no
CPU time tick); `SUBAK-NFR-002` asks for 0 CPU while idle (no timer) and a few MiB private. The
window's earlier baselines (SUBAK_GUI §7: Step 17, 4.34 MiB private, 11.5 MiB working set, 150
handles, three tabs with one attached but never switched) are not the same journey; this run
switched tabs, typed and opened menus before it idled.

## 6. Manual acceptance procedure (maintainer)

### 6.1 Before you start

Build `cargo build --release -p subak`. Use a private endpoint: `set E=--endpoint
subak-manual-b`. Keep a copy of `%APPDATA%\mouv\subak\config.toml` if you have one. Nothing
here touches your default Host.

### 6.2 Steps

1. Copy `target\release\subak.exe` and `subak-gui.exe` into a new folder, e.g.
   `%LOCALAPPDATA%\Programs\subak-test\`, and run everything from there.
2. `subak-gui.exe %E%`: the window opens with "No Terminal is open in this window" within a
   moment; `subak.exe %E% status` shows a Host with no Terminal.
3. `Ctrl+T`: a `cmd.exe` tab, the prompt at once. Type `dir`, `echo 한글`.
4. From a console: `subak.exe %E% new build`, `subak.exe %E% rename build 빌드`: the tab appears
   and is renamed. `Ctrl+Shift+R` in the window renames the shown tab.
5. `Ctrl+Tab` / `Ctrl+Shift+Tab` through the tabs: each shows its screen; the marker fades in.
6. Close the window with the X button: `subak.exe %E% ls` lists every Terminal running with the
   same pids. Reopen `subak-gui.exe %E%`: the last tab, its output, all tabs, the same size and
   place.
7. `Ctrl+W` on an idle tab ends it at once; on `subak.exe %E% new busy -- ping -n 1000
   127.0.0.1` it asks first (Cancel is the default).
8. The settings checks of [SUBAK_GUI §13.9](SUBAK_GUI.md) steps 1–11 (themes, fonts, opacity,
   motion, hover, profiles, mouse choices, a broken file, High Contrast, Narrator).
9. Use the window for real work for a while: typing feels immediate, nothing flickers, nothing
   covers the taskbar or other windows (only the optional floating icon is always on top).
10. Clean up: `subak.exe %E% host stop --kill-terminals --yes`; restore your settings file.

### 6.3 Results (pending — to be filled in by the maintainer)

| Manual check | Result |
| --- | --- |
| Open, empty state, Host started without a shell (steps 1–2) | **pending — maintainer** |
| CRUD from the window and the CLI, Korean (steps 3–4, 7) | **pending — maintainer** |
| Tabs and the marker fade (step 5) | **pending — maintainer** |
| Close and reopen with the same processes (step 6) | **pending — maintainer** |
| Settings (step 8, SUBAK_GUI §13.9) | **pending — maintainer** |
| Responsive, coherent, polished, not obscuring the work area (step 9) | **pending — maintainer** (visual judgement) |

Record the Windows build, display scale(s), keyboard layout and pass/fail per row in issue #20.

## 7. Deviations and open issues (tracked)

### 7.1 Found and fixed in this Step

- Settings lived in `gui.json` next to the placement, unversioned against `config.toml`'s plan:
  moved, with a one-time migration and a backup (§4).
- A Step 19 limitation, "no copy on select, click choices or word separators", is closed (§13.4
  of SUBAK_GUI).
- The window had the generic application icon: it now shows Subak's watermelon.

### 7.2 Nonessential UX deviations (tracked, not blocking)

| Deviation | Where it is tracked |
| --- | --- |
| No ligatures (the GDI renderer draws cell by cell) | ADR 0051 §7; SUBAK_GUI §13.10; Tabby `CORE-43` |
| No settings page; profiles edited in the file | SUBAK_GUI §13.10; Tabby `CFG-01` |
| Tab bar in the client area, no tooltips, no `F6` region cycle, search field in the banner row | SUBAK_GUI §8, §12.11, §13.10; DESIGN_SYSTEM §3.5, §4.1 |
| The floating icon and manager stay `kim-dark` | SUBAK_GUI §13.10 |
| Whole-window opacity (no Mica/acrylic) | SUBAK_GUI §13.10; Tabby `CORE-46` |
| No colour schemes, per-profile looks, cursor/blink setting, portable mode, language, welcome tab, sign-in start | SUBAK_GUI §13.10; Tabby matrix rows |
| Colour emoji are monochrome; no UI Automation tree for the terminal text | SUBAK_GUI §8 |

### 7.3 Open issues (not defects of this Step)

- Milestone B's visible acceptance (§6.3).
- Milestone A's live Windows Terminal + Claude Code run is still pending
  ([MILESTONE_A_VALIDATION](MILESTONE_A_VALIDATION.md)).

## 8. Reproducing

Everything in §3 and §5 is reproducible with the commands of §3.1 on Windows 10 1809 or later;
numbers depend on the machine. The off-screen windows never appear on the desktop and never
take the focus; each test uses its own endpoint and `APPDATA`.
