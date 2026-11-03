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

## Architecture and compatibility

| Document | Status | Purpose |
| --- | --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | current | Components, Host internals, object model, lifecycles, close/kill semantics, restart behavior, telemetry |
| [adr/README.md](adr/README.md) | current | Architecture decision records (ADR 0001–0014) |
| [CATALOG_FORMAT.md](CATALOG_FORMAT.md) | current | Kim catalog format, signatures and trust, sources/offline/freshness, tool status, read-only catalog CLI and exit codes |
| [RELEASE_POLICY.md](RELEASE_POLICY.md) | current | Consent rules, install transaction, channels, signing keys, publishing checklist |
| [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md) | current | Kim visual language: tokens, components, keyboard, high-DPI, contrast, Korean/Unicode, reduced motion |
| [RISK_REGISTER.md](RISK_REGISTER.md) | current | ConPTY, job objects, IPC, GUI, SSH, tmux, recovery and Claude risks with owner steps |
| [contracts/IPC_PROTOCOL.md](contracts/IPC_PROTOCOL.md) | draft contract | Subak IPC v1: framing, handshake, methods, events, error codes |
| [contracts/STATE_SCHEMA.md](contracts/STATE_SCHEMA.md) | draft contract | `state.json` v1 and `config.toml` v1 |
| [TMUX_COMPATIBILITY.md](TMUX_COMPATIBILITY.md) | current (inventory) | tmux command/key/option parity matrix with status and evidence |
| [TABBY_FEATURE_MATRIX.md](TABBY_FEATURE_MATRIX.md) | current (inventory) | Tabby feature parity matrix with status and evidence |

Design tokens and mock-ups live in [`design/`](../design/README.md) (`cargo xtask design-check`
enforces declared contrast pairs). GUI toolkit evidence: [`research/gui-eval/`](../research/gui-eval/README.md).

Contract schemas live in `contracts/schemas/`, with valid and invalid examples in
`contracts/examples/`; `cargo xtask schema-check` (also run by `cargo test`) validates them.

## Planned

| Document | Status | Planned by | Purpose |
| --- | --- | --- | --- |
| `CLI_CONTRACT.md` | planned | Step 07 | Shared CLI conventions, exit codes, JSON schemas |
| `CONPTY_PROOF.md` | planned | Step 08 | ConPTY + detached Host proof on real Windows |
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
