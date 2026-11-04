# ADR 0051 — Subak settings, shell profiles and appearance: config.toml edited in place, profiles resolved by clients, a light theme from the tokens, calm motion with no idle cost

Status: **Accepted** · Date: 2026-11-04 · Step 20 · Implements the `config.toml` part of
[ADR 0006](0006-state-and-configuration-storage.md) · Builds on
[ADR 0036](0036-terminal-emulation-and-rendering-path.md) (the CPU renderer),
[ADR 0039](0039-subak-window-plain-win32-client.md), [ADR 0042](0042-subak-window-tabs-and-safe-close.md)
and [ADR 0048](0048-subak-search-clipboard-and-links.md) (the window and its parts) · Details,
user guide and manual checks: [SUBAK_GUI.md §13](../SUBAK_GUI.md),
[SUBAK_CLI.md §3.13](../SUBAK_CLI.md), [MILESTONE_B_VALIDATION.md](../MILESTONE_B_VALIDATION.md)

## Context

Step 20 asks for restrained visual polish (tab and menu states, subtle motion that honours
reduced motion, consistent spacing, icons, notifications, focus and errors), user-selectable
themes, terminal fonts (ligatures where feasible), size and line height, window opacity,
configurable shortcuts, High Contrast and reduced motion; basic local shell profiles (program,
arguments, folder, environment) without Step 33's advanced profiles and without secrets in
profile files; settings persisted with a version, migrations, backup/export/import safeguards and
validation, invalid settings handled gracefully and never overwritten without a backup; and
settings that never affect running Host sessions unexpectedly. Step 19 moved copy on select,
smart `Ctrl+C`, the click actions and word separators here.

What existed: the window kept its text size and theme in `gui.json` v1 next to its placement
(Step 16); ADR 0006 planned a human-edited, versioned `config.toml` with format-preserving edits
and precise errors (`SUBAK-FR-046`); Noeul ships such a `config.toml` (Step 51: the `toml` parser,
line edits checked by parsing again); the Host's `terminal.create` already takes an explicit
`command`, `cwd` and `env` and refuses IPC v1's `profile` parameter until Step 33; the design
tokens had one theme, `kim-dark`; the window drew no motion at all and had no timer while idle.

## Decision

1. **One settings file, `%APPDATA%\mouv\subak\config.toml`** (`config_version = 1`), as
   ADR 0006 and STATE_SCHEMA §2 planned, with `[appearance]` (theme, font family, text size, line
   height, padding, opacity, motion, bold-as-bright), `[terminal]` (default profile, copy on
   select, smart `Ctrl+C`, right/middle click, word separators) and `[[profiles]]`. It is parsed
   with the `toml` crate Noeul already uses (same version and features; no new crate in the
   lock). **Edits are line edits** (the key's line replaced, a new key put into its section, a
   missing section appended) and are checked by parsing the result again: it must differ in
   exactly the edited keys, or nothing is written — so comments, unknown keys and sections survive
   every edit, which a TOML writer would not guarantee (format-preserving `toml_edit` would add
   `toml_writer` to the lock). Writes are atomic. Key bindings stay in `hotkeys.json`
   (ADR 0017) and the window's placement and remembered tabs in `gui.json`.
2. **Never lose a good file.** An invalid value falls back to its default alone, reported with
   its key and **line**; an invalid profile is left out with its reason. A file that is not TOML
   or is newer is used as defaults and is **never rewritten by an edit** (`config_invalid`,
   `config_from_newer_version`); only an explicit `reset --all` or `import` replaces an invalid
   file, and only after copying it to `backups\config-invalid-<UTC>.toml`. `reset --all`,
   `import` and the `gui.json` migration back the file up first (`backups\`, the newest 5 per
   kind). A newer file is never replaced.
3. **The Host never reads settings; profiles are resolved by clients.** The window (`Ctrl+T`,
   `+`, "New Terminal: <profile>") and `subak new --profile` turn a profile into the explicit
   `command`, `cwd` and `env` of `terminal.create`; `--cwd`, `--env` and `-- <program>` on the
   command line win over the profile's. So changing settings can never change a running
   Terminal; a broken file can never stop the Host; and a profile is a client-side preset, which
   Step 33 can still turn into a Host-side `profile` reference without breaking IPC v1.
   `tests/config_boundary.rs` keeps Host code from reading the file.
4. **No secrets in profile files.** Subak never writes a secret-looking value: `import` refuses
   (`secret_in_config`) environment values whose name or value the one secret detector
   (`crate::redact`) flags, `--password`-style arguments and `***` from an export. A value a user
   typed in by hand is used (it is their file), reported as a warning naming the variable, never
   its value, and shown as `***` everywhere (CLI human and JSON output, the window, `export`).
5. **`gui.json` version 2** holds only the placement and remembered tabs. A version 1 file's text
   size and theme are moved into `config.toml` once — only where `config.toml` is silent; the
   old file is backed up (`backups\gui-v1-<UTC>.json`), then rewritten as version 2. If
   `config.toml` cannot take them (invalid, newer), version 1 stays as it is and its values apply
   in memory.
6. **A light theme from the tokens, not a second implementation.** `design/tokens.json` gains
   `kim-light` with its own contrast pairs (`design-check` verifies 150 pairs over both themes).
   The window's screens, tab bar, banners, search bar and questions keep writing `kim-dark` token
   colours; `Chrome::retint` maps each to the same role of `kim-light`, as High Contrast already
   maps them to reverse video in the system colours. `theme = "system"` follows Windows' light or
   dark app mode (one registry value, read-only); High Contrast wins for `auto` and `system`;
   High Contrast is always opaque.
7. **Fonts through the existing GDI rasterizer.** `appearance.font_family` goes first in the
   design system's chain, so Korean, emoji and box drawing still fall back per character; a
   family that is not installed is skipped and the window says which one is used. **No
   ligatures**: the renderer draws each cell on its own with glyph indices (ADR 0036); ligatures
   need shaping across cells (DirectWrite or Uniscribe runs), a change to the renderer's core that
   is deferred and stated in the menu and the docs rather than pretended.
8. **Calm motion that costs nothing idle.** The active tab's marker fades in (120 ms) and a banner
   fades in rising 4 px (160 ms), on `easing.standard`. A 16 ms timer runs only while a
   transition runs and is killed after; terminal content, layout and typing never animate.
   Reduced motion — Windows' "Show animations in Windows" off, or `motion = "reduced"` — sets
   every duration to 0: no timer at all.
9. **A native settings menu, the same keys as the CLI** (`KIM-FR-013`). The tab and pane menus
   gain "Settings" (theme, text size, font, line spacing, opacity, motion, copy on select, smart
   `Ctrl+C`, right click, middle click, "Edit settings file…", "Reload settings") and one
   "New Terminal: <profile>" item per profile. Windows' menus are keyboard-operable (the Menu key,
   arrows) and exposed to screen readers. Each choice is applied at once and saved with the same
   line edit as `subak config set`; the window re-reads the file when it gets the focus.
   *Rejected for now:* a full settings page (a second UI toolkit surface, or many owner-drawn
   controls) — the file plus the menu cover every setting; profiles are edited in the file.
10. **Polish within the plain Win32 window**: hover on tabs (`bg.hover`, a brighter `×`), the
    watermelon as the window's icon (the floating icon's art), window opacity with
    `WS_EX_LAYERED`, toasts (non-critical notices that hide after 4 s; errors stay until a key).

## Evidence (this machine: Windows 10 Pro 22H2 19045)

- `crates/subak/src/config.rs` unit tests (parsing, key and line in every problem, fallbacks,
  broken and newer files, line edits keeping comments and unknown keys, refusals, reset,
  backups and pruning, export redaction, import checks, the `gui.json` adoption, the schema);
  `tests/config_cli.rs` (every command against its golden file; a profile launching its exact
  argv, folder and environment through a real Host; a running pane unchanged by every setting).
- `tests/gui_settings.rs`: light, dark and High Contrast frames checked pixel by pixel; font
  family, line height, padding, opacity (204 of 255 at 80 %); transitions of 120/160 ms that end,
  and none under reduced motion; `Ctrl+T` and menu profiles; the menu saving into
  `config.toml`; copy on select, smart `Ctrl+C`, right and middle click, word separators; hover;
  the `gui.json` v1 migration with its backup; restart persistence; a broken file never
  rewritten.
- `tests/milestone_b.rs` and [MILESTONE_B_VALIDATION.md](../MILESTONE_B_VALIDATION.md): the
  journey and the idle baseline (debug and release).

## Consequences

- Settings are hand-editable and diffable; tools and agents use `subak config` / `profile` with
  JSON; the window and the CLI change the same lines.
- The Host stays free of settings; Step 29 (persistence) can store a Terminal's profile name as
  metadata without the Host ever applying it.
- A light theme is a token set plus a mapping; a third theme (or user colour schemes, Tabby
  `CORE-37`/`38`) is a data change to the same tables.
- Line edits cannot rewrite a key written in an unusual TOML form (a dotted key at the top level,
  an inline table): such an edit is refused with `config_invalid` and the file is left alone.

## Not tested (and why)

The visible window (themes, motion, opacity over other windows, the hover, the native menus, the
taskbar icon), Notepad opened by "Edit settings file…", Windows' own High Contrast and light-mode
switches and "Show animations" setting, and Narrator on the menus need a visible desktop and the
maintainer: agents on this machine never show windows. The tests simulate those inputs with the
off-screen window's `system` test command. Manual steps: [SUBAK_GUI.md §13.9](../SUBAK_GUI.md).

## Alternatives considered

- **Settings as JSON** (like `gui.json` and `hotkeys.json`): rejected — ADR 0006 and the draft
  contract chose TOML for hand-editing with comments, and Noeul already ships the same pattern.
- **`toml_edit` for format-preserving writes**: rejected for now — it adds a crate to the lock;
  checked line edits already keep everything else intact and refuse what they cannot do.
- **The Host applying profiles** (IPC `profile`): deferred to Step 33 — it would make the Host read
  user settings, so a settings change could reach running sessions, which this Step forbids.
- **Animations on every interaction** (hover fades, menu fades): rejected — native menus animate
  per Windows' own setting; more timers would add wake-ups for little benefit.

## Requirements

`SUBAK-FR-048`, `SUBAK-FR-049`, `SUBAK-FR-046`, `SUBAK-FR-011`, `SUBAK-FR-031`, `SUBAK-FR-061`,
`SUBAK-NFR-002`, `KIM-NFR-012`, `KIM-FR-013`, `KIM-SEC-013`.
