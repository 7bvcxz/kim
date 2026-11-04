# Documentation index

Status legend: **current** = written and authoritative · **planned** = not written yet; the
listed step will create it. A planned document does not exist and must not be cited as if it
did.

## Product

| Document | Status | Purpose |
| --- | --- | --- |
| [KIM_PHILOSOPHY.md](KIM_PHILOSOPHY.md) | current | Why Kim exists and the principles every tool follows |
| [SUBAK_PHILOSOPHY.md](SUBAK_PHILOSOPHY.md) | current | Subak's Host/Client model, object model, crucial distinctions, Claude indicator |
| [PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md) | current | Numbered, testable requirements, milestones, platforms, privacy/safety policies |
| [NOEUL_PHILOSOPHY.md](NOEUL_PHILOSOPHY.md) | current (Step 48; app foundation implemented in Step 49) | Noeul, the screenshot and floating-pin tool: the double-click-to-pin deviation from Snipaste, pins, frugality, privacy, keyboard, CLI, license and IP boundary |
| [NOEUL_REQUIREMENTS.md](NOEUL_REQUIREMENTS.md) | current (Step 48; Step 49 registered its first error codes; Step 50 recorded the first overlay measurements; Step 53 the pin measurements; Step 51 the config error codes and the double-click pin latency; Step 52 the detection cost; Step 56 annotation; Step 57 the privacy tools; Step 54 the pin transforms, click-through, thumbnails, groups and the hidden-pin cost) | `NOEUL-*` requirements, milestones N1–N7 (Steps 49–64), platform permissions, performance budgets, deferrals, planned error codes |
| [NOEUL_SNIPASTE_MATRIX.md](NOEUL_SNIPASTE_MATRIX.md) | current (inventory, Snipaste 2.11.3; 81 rows `partial` since Step 54) | Snipaste feature parity matrix with status, requirement, milestone and verification per row |
| [NOEUL_UX_SPEC.md](NOEUL_UX_SPEC.md) | current (specification; the `F1` overlay implemented in Step 50, manual check in §4.5; the pin engine in Step 53, manual check in §4.6; the double-click-to-pin flow, toolbar, copy and save in Step 51, manual check in §4.7; detection, the magnifier, the precision keys and the rectangle history in Step 52, manual check in §4.8; annotation in Step 56, manual check in §4.9; the privacy tools in Step 57, manual check in §4.10; the pin transforms, click-through, thumbnails and groups in Step 54, manual check in §4.11) | Noeul journeys and click paths, wireframes, the exact double-click-to-pin rules and their test cases, default shortcuts, session limits, privacy |
| [DDALGAK_PHILOSOPHY.md](DDALGAK_PHILOSOPHY.md) | current (Step 65; engine core implemented in Step 66) | Ddalgak, the automation engine: one action, useful automation; novice-friendly GUI and headless CLI; safe by construction; honest about AutoHotkey |
| [DDALGAK_REQUIREMENTS.md](DDALGAK_REQUIREMENTS.md) | current (Step 65; D2 rows implemented in Steps 66–67; remap, block and mouse-button triggers in Step 68; send keys, type text and hotstrings in Step 69; program and website launches in Step 70; mouse moves, wheel and clicks in Step 71) | `DDALGAK-*` requirements and milestones D1–D7 (Steps 65–83) |
| [DDALGAK_RULE_MODEL.md](DDALGAK_RULE_MODEL.md) | current (rules document v1; store, validation and rule CLI implemented in Step 67; engine-control, remap and block rules run since Step 68; `send_keys`, `type_text` and hotstring triggers since Step 69; `open_program` (with `env` overrides) and `open_url` since Step 70; `mouse_move` (with grid units and stepped holds), `mouse_scroll` and `mouse_click` since Step 71) | Triggers, the seven core action types with inputs/outputs/errors/safety, modes, conditions, profiles, consent, precedence, validation, playground, outcomes and activity, versioning, CLI preview |
| [DDALGAK_GUI_JOURNEYS.md](DDALGAK_GUI_JOURNEYS.md) | current (design; the remap and block form fields are machine-checked since Step 68, the text, shortcut and hotstring forms and the text editor's data model since Step 69, the program picker and website field since Step 70, the Mouse form, its screen map and the playground's simulated pointer since Step 71) | End-to-end GUI journeys and wireframes: create, capture, effects, playground, profiles, modes, rollback, search/edit/delete/duplicate/export, emergency |
| [DDALGAK_SAFETY.md](DDALGAK_SAFETY.md) | current (Steps 66, 68, 69, 70 and 71 ran the engine, remap, typing, launch and mouse checks they own, §11) | Risks and scope: reserved shortcuts, elevated windows, engine lifecycle, layouts and IME, anti-cheat, hook security and emergency off, password fields and privacy, Linux, recursion and stuck keys, verification plan |
| [DDALGAK_ENGINE.md](DDALGAK_ENGINE.md) | current (implemented, Steps 66–71; Windows) | The engine as built: event model, decision pipeline, threads and lifecycle, own shortcuts, `ddalgak` CLI, measurements (idle CPU and wake-ups, decision cost), remaps and the on-demand mouse hook (§11), typing runs, hotstrings and paste (§12), program and website launches (§13), mouse moves, wheel, clicks and holds across monitors and DPI (§14), real-Windows verification status and manual checks |
| [DDALGAK_AHK_MATRIX.md](DDALGAK_AHK_MATRIX.md) | current (inventory, matrix v2, AutoHotkey v2.0.31; remap rows implemented in Step 68, hotstring and sending rows in Step 69, `Run` rows in Step 70, `MouseMove`/`Click`/wheel rows in Step 71) | AutoHotkey v2 comparison with sources: supported, partial, planned, not intended |

## Architecture and compatibility

| Document | Status | Purpose |
| --- | --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | current | Components, Host internals, object model, lifecycles, close/kill semantics, restart behavior, telemetry |
| [adr/README.md](adr/README.md) | current | Architecture decision records (ADR 0001–0017, 0019–0032, 0034–0041, 0044; numbers 0018 and 0033 were reserved and not used) |
| [SESSION_MODEL.md](SESSION_MODEL.md) | current (implemented, Step 10; CLI verbs and `detach` from Step 12; pane screens and history from Step 13) | Terminal → Window → Pane model, the Terminal state machine, naming and target rules, what detach/stop/kill/rm/host stop do to processes, consent and the busy rule, concurrency guarantees, evidence |
| [SUBAK_CLI.md](SUBAK_CLI.md) | current (implemented, Step 12; `capture`, `history`, `clear-history` from Step 13) | Subak CLI reference: `new`, `ls`, `status`, `info`, `attach`, `detach`, `rename`, `stop`, `kill`, `rm`, `capture`, `history`, `clear-history`, `host …`; output, exit codes and consent; attach keys, Ctrl+C/Ctrl+Break and console restore with measurements; verbatim command lines for cmd.exe; agent and headless recipes; tmux mapping |
| [TERMINAL_EMULATOR.md](TERMINAL_EMULATOR.md) | current (implemented, Step 15; used by the Subak window since Step 16; visible-window check pending the maintainer) | Subak's terminal emulator: the model shared by the Host and client views (`subak-term`), what is emulated, the client view (viewport, selection and copy, hyperlinks, device reports), input encoding (keys, paste, mouse, focus), the CPU cell-grid renderer (themes, glyph fallback, DPI, damage), conformance and real-ConPTY tests, the benchmark harness and its results, known limits, the manual check |
| [SUBAK_GUI.md](SUBAK_GUI.md) | current (implemented, Step 16; visible-window checks pending the maintainer) | The Subak window `subak-gui`: starting it and which Terminal opens, close/crash/detach/reconnect, keyboard dispatch, shortcuts, IME, mouse, DPI, states and banners, High Contrast, `gui.json` preferences, how it works, idle measurements, known limits, the maintainer's manual script, tests |
| [SCROLLBACK.md](SCROLLBACK.md) | current (implemented, Step 13; model shared with client views since Step 15) | Scrollback and replay: what the Host keeps per pane (terminal model, bounded history, deterministic eviction), attach as snapshot + live stream without loss or duplication, resize and readers of different sizes, `capture`/`history`/`clear-history`, slow-client resync and idle cost, the privacy and security policy for terminal output (redaction, retention, persistence deferred), known limits, evidence |
| [PTY_PROCESSES.md](PTY_PROCESSES.md) | current (implemented, Step 11; lagging clients resynced since Step 13) | What runs in a pane: program resolution and quoting, PowerShell/cmd/pwsh, working directory, the three environment layers and secrets, size/Unicode/Ctrl+C, exit status, process-tree containment, backpressure and memory, the platform-neutral PTY interface, errors, evidence |
| [CONPTY_PROOF.md](CONPTY_PROOF.md) | current (Step 08 prototype) | Windows ConPTY + detached Host proof: what "detached" means, evidence with process ids, OS limitations and power states, manual procedures (Windows Terminal, sleep, logoff pending) |
| [MILESTONE_A_VALIDATION.md](MILESTONE_A_VALIDATION.md) | current (Step 14; **Milestone A not complete**: the live Windows Terminal + Claude Code acceptance run is pending) | Persistent sessions and reconnection validated: the plan, the automated integration suite and its evidence, the failure and concurrency matrix, baseline measurements with the machine and environment, the maintainer's manual procedure (`scripts/milestone-a-smoke.ps1`) with its pending results table, defects and open risks |
| [CLI_CONTRACT.md](CLI_CONTRACT.md) | current (contract v1) | Cross-tool CLI contract: names, help, streams, `--json` envelope, exit codes, error-code registry, dry-run/confirmation, versioning, capabilities and tool-to-tool invocation, secret redaction |
| [KIM_CLI.md](KIM_CLI.md) | current | `kim` command reference, files and environment, sample agent workflows |
| [HOTKEY_CONTRACT.md](HOTKEY_CONTRACT.md) | current (contract v1; implemented by `kim-hotkeys` and `kim`, Step 46) | Customizable shortcuts for every Kim GUI tool: action IDs, scopes, key syntax, layouts and IME, reserved keys, `hotkeys.json`, conflicts and precedence, `hotkeys` CLI, accessibility and safety, Subak/Kim Manager/Noeul/Ddalgak mapping |
| [CATALOG_FORMAT.md](CATALOG_FORMAT.md) | current (Step 47: tool metadata; Noeul and Ddalgak listed as planned) | Kim catalog format, optional tool metadata (features, entry points, links, shortcut summaries, permissions), signatures and trust, sources/offline/freshness, availability and tool status, read-only catalog CLI and exit codes |
| [RELEASE_POLICY.md](RELEASE_POLICY.md) | current | Consent rules (incl. disclosure of permissions and dependencies, running background utilities), install transaction, channels, signing keys, publishing checklist |
| [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md) | current | Kim visual language: tokens, components, keyboard, high-DPI, contrast, Korean/Unicode, reduced motion |
| [RISK_REGISTER.md](RISK_REGISTER.md) | current | ConPTY, job objects, IPC, GUI, SSH, tmux, recovery and Claude risks with owner steps |
| [contracts/IPC_PROTOCOL.md](contracts/IPC_PROTOCOL.md) | current (implemented v1, Steps 09–13) | Subak IPC v1: framing and limits, handshake with mutual identity checks, version negotiation, methods and authorization by client kind, Terminal methods and events, error codes, Host lifetime and start on demand, manual cross-user checks |
| [contracts/STATE_SCHEMA.md](contracts/STATE_SCHEMA.md) | draft contract | `state.json` v1 and `config.toml` v1 |
| [TMUX_COMPATIBILITY.md](TMUX_COMPATIBILITY.md) | current (inventory) | tmux command/key/option parity matrix with status and evidence |
| [TABBY_FEATURE_MATRIX.md](TABBY_FEATURE_MATRIX.md) | current (inventory) | Tabby feature parity matrix with status and evidence |

Design tokens and mock-ups live in [`design/`](../design/README.md) (`cargo xtask design-check`
enforces declared contrast pairs). GUI toolkit evidence: [`research/gui-eval/`](../research/gui-eval/README.md);
terminal renderer evidence: [`research/terminal-eval/`](../research/terminal-eval/README.md).

Contract schemas live in `contracts/schemas/`, with valid and invalid examples in
`contracts/examples/`; `cargo xtask schema-check` (also run by `cargo test`) validates them.
The CLI schemas (`cli-envelope-v1`, `kim-cli-v1`, `subak-cli-v1`, `tool-capabilities-v1`) are
additionally checked against the real `kim` and `subak` binaries by their integration tests; the
`kim-cli-v1` and `subak-cli-v1` examples are golden files generated from real runs. `noeul-ipc-v1` (Noeul's own
IPC between its CLI and the running instance, [ADR 0023](adr/0023-noeul-app-foundation.md)) is
checked against every message `crates/noeul` builds, and its CLI output against `cli-envelope-v1`,
`tool-capabilities-v1` and `hotkeys-cli-v1`, by the `noeul` tests. The hotkey schemas
(`kim-hotkeys-v1`, `kim-hotkey-manifest-v1`, `hotkeys-cli-v1`, `hotkey-scenarios-v1`) are
implemented by the shared `kim-hotkeys` crate and `kim hotkeys` (Step 46), whose tests validate
real output against them; for the scenarios, `schema-check` also recomputes every expected
conflict report with a reference checker, and the library recomputes them again
([HOTKEY_CONTRACT §15](HOTKEY_CONTRACT.md)). The `ipc-v1` schema also types the Terminal,
Window and Pane objects, the `terminal.*` events and the Step 10 method results; Subak's tests
validate every control message and result a real Host sends against it.

The Ddalgak rule schemas (`ddalgak-rules-v1`, `ddalgak-simulation-v1`) are specifications (Step 65;
the engine reads `ddalgak-rules-v1` documents for its engine-control rules since Step 66, its remap and block rules since Step 68, its typing rules and hotstrings since Step 69, its launch rules since Step 70 and its mouse rules since Step 71); `schema-check` validates their examples and runs the
cross-reference checks of [DDALGAK_RULE_MODEL §8](DDALGAK_RULE_MODEL.md) on them.

## Planned

| Document | Status | Planned by | Purpose |
| --- | --- | --- | --- |
| `MILESTONE_B_VALIDATION.md` | planned | Step 20 | GUI journey validation evidence |
| `RECOVERY.md` | planned | Steps 29–31 | Persistence, cold recovery and Claude resume procedures |
| `MCP_GUIDE.md` | planned | Step 37 | Subak MCP server guide for agents |

## Repository and engineering

| Document | Status | Purpose |
| --- | --- | --- |
| [AGENTS.md](../AGENTS.md) | current | Working rules for contributors and coding agents |
| [CONTRIBUTING.md](../CONTRIBUTING.md) | current | Developer setup, commands, workspace layout, versioning, MSRV |
| [SECURITY.md](../SECURITY.md) | current | Vulnerability reporting, dependency and supply-chain policy |
| [e2e/LEDGER.md](../e2e/LEDGER.md) | current | Processed Step issues and e2e run log |

`cargo xtask docs-check` (run by CI) fails when a document in `docs/` is missing from this
index, when a planned document already exists, when a link is broken, or when a requirement ID
is undefined.
