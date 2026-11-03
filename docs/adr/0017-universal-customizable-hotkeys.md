# ADR 0017 — Universal customizable hotkeys without a shared hook or daemon

Status: **Accepted** · Date: 2026-11-04 · Step 45 · Implemented from Step 46 (library and Kim
Manager), Steps 17 and 23 (Subak), 61 (Noeul), 66 and 73 (Ddalgak)

## Context

Kim is growing from one GUI (Kim Manager) to several: Subak's terminal GUI, Noeul (screenshot
and pin) and Ddalgak (automation). Users must be able to see, change, disable and reset every
tool's shortcuts, also headlessly, without one tool stealing another's keys or the OS's
(`KIM-FR-040…046`, `KIM-SEC-020…023`). Three constraints shape the design:

- Kim has **no always-on processes** (KIM_PHILOSOPHY §3, `KIM-NFR-001`). A global shortcut
  needs a running process, and a system-wide keyboard hook sees every keystroke.
- Subak's keyboard dispatch is already decided ([ADR 0009](0009-gui-clients-and-keyboard-dispatch.md)):
  IME guard, `Ctrl+B` prefix, app shortcuts, PTY, with pass-through for shadowed keys.
- Windows' `RegisterHotKey` is exclusive (first registrant wins), cannot tell left from right
  modifiers, has no key-up, and reserves `Win` chords and `F12`. Only a low-level hook can do
  more, and a hook can also read and swallow everything.

## Decision

1. **One contract, implemented per tool.** [HOTKEY_CONTRACT.md](../HOTKEY_CONTRACT.md) v1
   defines action IDs, scopes, the key syntax, storage, the `hotkeys` CLI, conflict and
   precedence rules, and safety rules for every Kim tool with a GUI. CLI-only operations need no
   hotkeys. Each tool implements the contract itself. A shared crate for parsing, layering and
   conflict checks is extracted in Step 46, when two tools use it (ADR 0002). It is a library,
   never a process.
2. **No shared hook, hotkey service or daemon.** Each tool declares the strongest input backend
   it uses: `focus` (its own windows), `register` (`RegisterHotKey`, global single chords) or
   `hook` (low-level hook). Only Ddalgak, whose purpose is user-defined automation, may use
   `hook`, only while the user has it enabled, and visibly. Global shortcuts work only while
   their tool runs; shortcuts never start a tool. Bindings that exceed a tool's backend are
   rejected instead of adding a hook.
3. **Three scopes.** `global`, `app` (the tool's windows have focus) and named `context`s.
   Global defaults are allowed only for a tool's primary entry action (Noeul `F1`, `F3`).
4. **Portable, canonical key syntax.** Canonical text such as `Ctrl+Shift+T`, `@prefix char:c`
   or `Ctrl+code:KeyZ`, with three key identities: logical (virtual key, follows the layout's
   letters), physical (`code:`, scan-code position) and character (`char:`, the produced
   character, only in contexts and after a first step). Generic and side-specific modifiers,
   `AltGr`, up to four-step sequences, named leaders (Subak's prefix becomes leader `prefix`),
   and press, release and hold triggers. No Windows virtual-key numbers are stored.
5. **Reserved keys are never bound.** OS chords (Secure Attention Sequence, every `Win` chord,
   Task Manager, task switching, accessibility toggles, global `F12`), Kim's navigation and
   typing keys in `app`/`global` scope, IME mode keys, and one emergency chord. The schemas
   reject the scope-independent ones.
6. **Sparse user layer in `hotkeys.json`.** Per tool, `%APPDATA%\mouv\<tool>\hotkeys.json`
   (`kim.hotkeys` v1, JSON) stores only the user's overrides, so updates can improve untouched
   defaults and never overwrite user choices. Layering: default → user → profile. Unknown
   fields and actions are preserved. A broken or newer file puts the tool in safe mode, with no
   global hotkeys and the file left untouched. Migrations are pure `vN → vN+1` steps with a
   backup. Each tool also ships a hotkey manifest, so Kim can show and check shortcuts without
   starting the tool. Subak's draft `[keys]` table in `config.toml` is superseded before it
   shipped.
7. **Detect and report; never steal.** A check combines the tool's own layers, other installed
   Kim tools (files only, through `kim hotkeys check --all-tools`, no daemon), the reserved
   table and, at registration, `ERROR_HOTKEY_ALREADY_REGISTERED`. Conflicts have stable codes,
   severities and one deterministic subject. They come with non-destructive offers (rebind
   suggestions, disable, keep, reset) that are never applied automatically. Precedence: OS,
   then the first global registrant, then the focused tool (IME guard, pending sequence,
   innermost context, app). A user binding beats a default; nothing beats a leader. A change
   that causes an error conflict is refused unless the user passes `--replace` (same tool) or
   `--allow-conflict` (cross-tool). A tool never edits another tool's configuration.
8. **Headless `hotkeys` command group** under CLI contract v1: `list`, `show`, `set`, `disable`,
   `enable`, `reset`, `check`, `export`, `import`, with seven new error codes registered in
   CLI_CONTRACT §6. No `hotkeys` command opens a window, registers a hotkey or installs a hook.
9. **Safety.** The secure desktop, UAC and sign-in are never reached or worked around (no
   service, SYSTEM component or `UIAccess`). Password fields are not intercepted. Nothing is
   keylogged. Every tool has a full disable switch (GUI, CLI, `--no-hotkeys`/`KIM_NO_HOTKEYS`,
   file), and a hook tool has a non-remappable emergency chord (provisionally
   `Ctrl+Alt+Shift+Escape`, verified in Step 73).
10. **Machine-checked specification.** JSON schemas (`kim-hotkeys-v1`, `kim-hotkey-manifest-v1`,
    `hotkeys-cli-v1`, `hotkey-scenarios-v1`) with valid and invalid examples, plus conflict and
    override scenarios. A small reference checker in `xtask` recomputes the scenarios in
    `cargo xtask schema-check`. The subset validator gains the `not` keyword, which reserved
    chords need. The checker is not a library. Step 46's implementation must pass the same
    scenarios and may then replace it.

## Consequences

- Users get one model and one command group for every tool, and agents can inspect and change
  bindings headlessly.
- A tool that is not running has no global shortcuts. This is the price of no daemon; Noeul and
  Ddalgak offer opt-in start at sign-in like any other explicit user setting.
- Global chords are first come, first served at the OS level. Kim makes the race visible
  (`cross_tool_global`, `taken`) instead of hiding it.
- Without a hook, Noeul and Subak cannot offer left/right-specific or release/hold global
  shortcuts. Those stay with Ddalgak.
- The schemas reject reserved chords, so a user layer that only becomes invalid when a later
  contract revision reserves more chords puts the tool in safe mode. Contract revisions that add
  reserved chords must call this out.
- Subak's `config.toml` loses its `[keys]` table (never shipped); STATE_SCHEMA is updated.

## Alternatives considered

- **A Kim hotkey daemon that owns all global shortcuts and dispatches to tools:** one place for
  conflicts, but an always-on process, a single point of failure and a privileged keystroke
  router. Rejected (KIM_PHILOSOPHY §3, `KIM-NFR-020`).
- **A low-level hook in every tool:** left/right modifiers and release triggers everywhere, but
  every small tool would see every keystroke. Disproportionate and keylogger-shaped. Rejected;
  only Ddalgak, opt-in.
- **Stealing keys on conflict** (unregistering, or hooking over another program's hotkey):
  violates user expectations and `KIM-SEC-020`. Rejected.
- **Storing virtual-key codes or full binding tables:** not portable to Linux, and full tables
  freeze old defaults into user files. Rejected for portable names and a sparse layer.
- **TOML for the user layer** (like Subak's `config.toml`): friendlier for hand edits, but the
  hotkey document is mostly written by the GUI and CLI, crosses tools during checks, and is
  validated by the same JSON Schema tooling as every other contract. Kim's own settings are
  already JSON.
- **Electron-style accelerator strings** (`CommandOrControl+X`): no physical, character or
  sequence keys, and no leader concept for Subak's prefix table.

## Requirements

`KIM-FR-040…046`, `KIM-NFR-020…022`, `KIM-SEC-020…023`, `KIM-FR-012`, `KIM-FR-013`,
`KIM-NFR-001`, `KIM-NFR-012`, `KIM-SEC-001`, `KIM-SEC-004`, `KIM-SEC-013`, `SUBAK-FR-033`,
`SUBAK-FR-034`.
