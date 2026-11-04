# ADR 0022 — Ddalgak: declarative rules, a tray-app engine and safety by construction

Status: **Accepted** · Date: 2026-11-04 · Step 65 (specification) · Implemented from Steps 66–83 ·
Builds on [ADR 0002](0002-independent-tool-binaries.md) and [ADR 0017](0017-universal-customizable-hotkeys.md)

## Context

Ddalgak is the Kim tool for user-defined automation: one key press remaps a key, types text,
opens a program or website, moves the mouse, switches a mode or pauses the engine
([DDALGAK_PHILOSOPHY](../DDALGAK_PHILOSOPHY.md)). Users know this space from AutoHotkey, whose v2
is a general-purpose scripting language with an interpreter. Ddalgak must instead be a small
native Rust tool with a novice-friendly visual editor and a headless CLI, without an AutoHotkey
runtime, Electron, a resident Kim daemon, an account or a cloud.

ADR 0017 already decided that only Ddalgak may use a low-level keyboard hook, only while the
user has the engine on and visibly, that it keeps a non-remappable emergency chord, and that its
rule triggers take part in cross-tool conflict checks as `rule.<id>`. It left the rule model,
the process model and the detailed safety rules to Step 65. The Kim catalog (Step 47) already
discloses Ddalgak's permissions (`keyboard-hook`, `input-injection`, `launch-programs`,
`window-control`, `clipboard`) and lists it as planned.

## Decision

1. **Rules are declarative data, not a language.** A rule is *trigger → ordered steps*, with
   modes, conditions, priority, pass-through, an enabled flag and consent, stored as a versioned
   JSON document `ddalgak.rules` v1 ([DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md),
   [schema](../contracts/schemas/ddalgak-rules-v1.schema.json)). There is no interpreter, no
   expressions and no user code. AutoHotkey is a behavioral reference only; a versioned matrix
   states what is covered ([DDALGAK_AHK_MATRIX](../DDALGAK_AHK_MATRIX.md)), and a later importer
   converts only a declarative subset into disabled rules without executing anything.
2. **Seven core action categories with precise contracts**: `remap` (plus `block`), `send_keys`,
   `type_text`, `open_program`, `open_url`, `mouse_move`/`mouse_scroll`/`mouse_click`, `mode`,
   `engine`, and `delay` for ordered steps. Future types (`window`, `clipboard`, `run_command`,
   `notify`, macro control) are reserved names, added additively. A rule containing a step type
   or condition a version cannot evaluate is kept but inactive, never broadened.
3. **Process model: two binaries, one resident app.** Per ADR 0002, `ddalgak.exe` is the
   headless CLI (no GUI toolkit; never opens a window or tray icon, never installs a hook, never
   starts the app) and `ddalgak-gui.exe` is the tray app that owns the engine (hook thread,
   worker and injector threads), the editor and the playground, opening windows only on demand.
   The app runs only when the user starts it or opted into start at sign-in; it is not a service
   and no other Kim tool depends on it. The CLI edits files directly and talks to a running app
   through a per-user, identity-checked local IPC (the pattern of ADR 0005/0019; Step 66 fixes the
   details). The engine starts `off`; it is never turned on by a rule, an import, a crash
   recovery or an update. Rules can pause, resume or turn it off. Ddalgak refuses to turn the
   engine on when it runs elevated.
4. **Separate files.** Rules live in `%APPDATA%\mouv\ddalgak\rules.json`; Ddalgak's own GUI
   shortcuts stay in `hotkeys.json` under the hotkey contract. The final hotkey manifest keeps
   the four global actions the catalog summarizes and adds the editor's page contexts `rules` and
   `playground`; `rule.` is reserved for rule triggers.
5. **Trigger syntax** is the hotkey contract's v1 syntax plus lock keys and bare side-specific
   modifiers, without `char:` keys (hook-side character translation disturbs dead keys) or
   leaders. Triggers never use the scope-independent reserved chords, `F12` or the emergency
   chord; widened remaps never capture a reserved chord. **`Win` chords are not triggers in v1**
   (the contract reserves them); revisiting that needs a contract revision and an ADR. Outputs
   may use `Win` chords and IME keys but never the Secure Attention Sequence or the emergency
   chord.
6. **Recursion is impossible by construction.** Ddalgak tags its own injected events and never
   matches them, so mutual remaps are swaps. A rule that accepts other programs' injected input
   may not inject; a rule whose output is its own trigger is refused; a per-rule rate limit stops
   loops through other tools.
7. **Consent per permission per rule.** `input-injection`, `launch-programs` and `clipboard` need
   explicit consent recorded on the rule (GUI dialog or `--allow`). A launch consent is bound to
   an RFC 8785 SHA-256 fingerprint of the exact targets, so a changed path, argument or URL needs
   new consent. Consent is never imported, never granted by an agent, never copied to changed
   targets. Launches use `CreateProcess` with argument lists; shells and script hosts, non-`http(s)`
   URLs and credentials in URLs are refused; nothing elevates.
8. **Stepping aside instead of working around.** On the secure desktop, in password fields,
   during IME composition and for injecting rules over elevated windows (UIPI) Ddalgak skips the
   rule and lets the key pass unchanged, with a stated reason. No `UIAccess`, service, driver or
   SYSTEM component; no concealment from anti-cheat.
9. **A safe playground with a machine-checked contract.** Simulation is the default and is a
   pure function of rules, context and event; a live test types only into the playground and
   opens a program only after a per-launch confirmation. The result format
   ([ddalgak-simulation-v1](../contracts/schemas/ddalgak-simulation-v1.schema.json)) encodes these
   properties, and summarizes personal content (counts, file names, hosts) instead of echoing it.
10. **Machine-checked fixtures.** `cargo xtask schema-check` validates the rule and playground
    examples (valid and invalid) and runs a small reference checker (`xtask/src/ddalgak.rs`) for
    the cross-reference rules (unique IDs, references, self-triggering outputs, duplicate
    triggers, stale consent fingerprints, run length) and for the link between playground
    examples and the rules they trace. The checker needs SHA-256, so `xtask` depends on `sha2`,
    which `kim` already uses. Like ADR 0017's hotkey checker it is a reference for the fixtures,
    not a library; Step 67 implements the same rules and must agree with the same files.

## Consequences

- Novices get a tool without syntax, and agents get versioned JSON, simulation and explain
  output. Power users lose what only a language gives (arbitrary logic, `DllCall`, script GUIs);
  the matrix says so honestly.
- A rule never does more than it says: an older Ddalgak keeps newer rules inactive, imported
  rules arrive disabled, and changed launch targets need fresh consent.
- Two binaries match Kim and Subak and keep the CLI free of GUI code. The resident part is one
  user-started tray process, so with the app closed there is no hook and no shortcut at all, the
  price of having no daemon (as in ADR 0017).
- Without `Win`-chord triggers and `UIAccess`, some popular AutoHotkey uses (Win+N launchers,
  automating administrator windows) are not available in v1. Both are recorded as deliberate.
- The catalog's GUI entry point command (`ddalgak`) should become `ddalgak-gui` and its docs link
  should point to these documents in the next catalog version; Step 65 does not change the signed
  catalog. Mouse-button triggers (Step 68) need a `mouse-hook` permission disclosed in the catalog
  before they ship.
- Step 66 owns the IPC details, the hook health check and the performance budget; Step 73 the
  real-Windows verification of the emergency chord; Step 82 the release acceptance matrix.

## Alternatives considered

- **Embed a scripting language** (an AutoHotkey-compatible interpreter, Lua, Rhai, JavaScript):
  maximal power, but a large attack surface, a runtime to ship and update, and a model novices
  cannot read or a GUI cannot edit faithfully. Rejected; bounded macro control comes additively.
- **Claim AutoHotkey compatibility** or execute `.ahk` files: dishonest for a declarative model
  and unsafe. Rejected; a previewing importer for a defined subset instead.
- **`RegisterHotKey` only**: no remaps, no left/right modifiers, no release or hold triggers, no
  sequences. Insufficient for Ddalgak's purpose (ADR 0017 already allows the hook).
- **A kernel-level input driver** (filter driver or virtual HID device): works in more games and
  elevated windows, but needs administrator installation, signing and a privileged component, and
  looks like cheating software. Rejected (`KIM-SEC-004`, `DDALGAK-SEC-007`, `DDALGAK-SEC-012`).
- **A separate engine process or Windows service** next to the GUI: lower idle memory than a GUI
  process, but a third executable, a service violates "no mandatory daemon", and a headless engine
  could hold an invisible hook. Rejected for the tray app that keeps windows closed when idle.
- **One binary for CLI and GUI**: simpler packaging, but every CLI call would load GUI code and
  ADR 0002 forbids it. Rejected.
- **Rules inside `hotkeys.json`**: one file, but rules are not GUI actions, have their own
  validation, consent and history, and would bloat the contract's sparse user layer. Rejected, as
  ADR 0017 anticipated.
- **TOML or YAML rules**: friendlier for hand edits, but the document is written mostly by the GUI
  and CLI, crosses into exports and imports, and uses the same JSON Schema tooling as every other
  contract. JSON chosen; a YAML export may be added later as a view.
- **Global consent switches only** (one "allow launching programs" setting): fewer prompts, but an
  imported or edited rule could launch anything once the switch is on. Rejected for per-rule,
  fingerprinted consent.
- **`UIAccess` to reach elevated windows** (as AutoHotkey offers): needs a signed binary in a
  protected folder and raises Ddalgak's reach to every window. Rejected.

## Requirements

`DDALGAK-FR-001…076`, `DDALGAK-NFR-001…010`, `DDALGAK-SEC-001…014`, `KIM-FR-001`, `KIM-FR-012`,
`KIM-FR-013`, `KIM-FR-040…046`, `KIM-NFR-020…022`, `KIM-SEC-001`, `KIM-SEC-004`, `KIM-SEC-006`,
`KIM-SEC-013`, `KIM-SEC-020…023`.
