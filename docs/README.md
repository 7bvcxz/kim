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
| [NOEUL_REQUIREMENTS.md](NOEUL_REQUIREMENTS.md) | current (Step 48; Step 49 registered its first error codes) | `NOEUL-*` requirements, milestones N1–N7 (Steps 49–64), platform permissions, performance budgets, deferrals, planned error codes |
| [NOEUL_SNIPASTE_MATRIX.md](NOEUL_SNIPASTE_MATRIX.md) | current (inventory, Snipaste 2.11.3; 18 rows `partial` since Step 49) | Snipaste feature parity matrix with status, requirement, milestone and verification per row |
| [NOEUL_UX_SPEC.md](NOEUL_UX_SPEC.md) | current (specification) | Noeul journeys and click paths, wireframes, the exact double-click-to-pin rules and their test cases, default shortcuts, session limits, privacy |

## Architecture and compatibility

| Document | Status | Purpose |
| --- | --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | current | Components, Host internals, object model, lifecycles, close/kill semantics, restart behavior, telemetry |
| [adr/README.md](adr/README.md) | current | Architecture decision records (ADR 0001–0021) |
| [CONPTY_PROOF.md](CONPTY_PROOF.md) | current (Step 08 prototype) | Windows ConPTY + detached Host proof: what "detached" means, evidence with process ids, OS limitations and power states, manual procedures (Windows Terminal, sleep, logoff pending) |
| [CLI_CONTRACT.md](CLI_CONTRACT.md) | current (contract v1) | Cross-tool CLI contract: names, help, streams, `--json` envelope, exit codes, error-code registry, dry-run/confirmation, versioning, capabilities and tool-to-tool invocation, secret redaction |
| [KIM_CLI.md](KIM_CLI.md) | current | `kim` command reference, files and environment, sample agent workflows |
| [HOTKEY_CONTRACT.md](HOTKEY_CONTRACT.md) | current (contract v1; implemented by `kim-hotkeys` and `kim`, Step 46) | Customizable shortcuts for every Kim GUI tool: action IDs, scopes, key syntax, layouts and IME, reserved keys, `hotkeys.json`, conflicts and precedence, `hotkeys` CLI, accessibility and safety, Subak/Kim Manager/Noeul/Ddalgak mapping |
| [CATALOG_FORMAT.md](CATALOG_FORMAT.md) | current (Step 47: tool metadata; Noeul and Ddalgak listed as planned) | Kim catalog format, optional tool metadata (features, entry points, links, shortcut summaries, permissions), signatures and trust, sources/offline/freshness, availability and tool status, read-only catalog CLI and exit codes |
| [RELEASE_POLICY.md](RELEASE_POLICY.md) | current | Consent rules (incl. disclosure of permissions and dependencies, running background utilities), install transaction, channels, signing keys, publishing checklist |
| [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md) | current | Kim visual language: tokens, components, keyboard, high-DPI, contrast, Korean/Unicode, reduced motion |
| [RISK_REGISTER.md](RISK_REGISTER.md) | current | ConPTY, job objects, IPC, GUI, SSH, tmux, recovery and Claude risks with owner steps |
| [contracts/IPC_PROTOCOL.md](contracts/IPC_PROTOCOL.md) | current (implemented v1, Step 09) | Subak IPC v1: framing and limits, handshake with mutual identity checks, version negotiation, methods and authorization by client kind, events, error codes, Host lifetime and start on demand, manual cross-user checks |
| [contracts/STATE_SCHEMA.md](contracts/STATE_SCHEMA.md) | draft contract | `state.json` v1 and `config.toml` v1 |
| [TMUX_COMPATIBILITY.md](TMUX_COMPATIBILITY.md) | current (inventory) | tmux command/key/option parity matrix with status and evidence |
| [TABBY_FEATURE_MATRIX.md](TABBY_FEATURE_MATRIX.md) | current (inventory) | Tabby feature parity matrix with status and evidence |

Design tokens and mock-ups live in [`design/`](../design/README.md) (`cargo xtask design-check`
enforces declared contrast pairs). GUI toolkit evidence: [`research/gui-eval/`](../research/gui-eval/README.md).

Contract schemas live in `contracts/schemas/`, with valid and invalid examples in
`contracts/examples/`; `cargo xtask schema-check` (also run by `cargo test`) validates them.
The CLI schemas (`cli-envelope-v1`, `kim-cli-v1`, `tool-capabilities-v1`) are additionally
checked against the real `kim` and `subak` binaries by their integration tests; the
`kim-cli-v1` examples are golden files generated from real runs. `noeul-ipc-v1` (Noeul's own
IPC between its CLI and the running instance, [ADR 0023](adr/0023-noeul-app-foundation.md)) is
checked against every message `crates/noeul` builds, and its CLI output against `cli-envelope-v1`,
`tool-capabilities-v1` and `hotkeys-cli-v1`, by the `noeul` tests. The hotkey schemas
(`kim-hotkeys-v1`, `kim-hotkey-manifest-v1`, `hotkeys-cli-v1`, `hotkey-scenarios-v1`) are
implemented by the shared `kim-hotkeys` crate and `kim hotkeys` (Step 46), whose tests validate
real output against them; for the scenarios, `schema-check` also recomputes every expected
conflict report with a reference checker, and the library recomputes them again
([HOTKEY_CONTRACT §15](HOTKEY_CONTRACT.md)).

## Planned

| Document | Status | Planned by | Purpose |
| --- | --- | --- | --- |
| `SESSION_MODEL.md` | planned | Step 10 | Session state machine and naming rules as implemented |
| `SUBAK_CLI.md` | planned | Step 12 | Subak CLI reference |
| `MILESTONE_A_VALIDATION.md` | planned | Step 14 | Persistent sessions validation evidence |
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
