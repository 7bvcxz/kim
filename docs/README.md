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

## Planned

| Document | Status | Planned by | Purpose |
| --- | --- | --- | --- |
| `ARCHITECTURE.md` | planned | Step 03 | Components, process/PTY ownership, IPC, state store, portable interfaces, diagrams |
| `adr/` | planned | Step 03 | Architecture decision records |
| `TMUX_COMPATIBILITY.md` | planned | Step 03 | tmux command/key parity matrix with evidence |
| `TABBY_FEATURE_MATRIX.md` | planned | Step 03 | Tabby feature parity matrix with evidence |
| `RECOVERY.md` | planned | Step 03 / M2 | Session state, restore and Claude resume procedures |
| `DESIGN_SYSTEM.md` | planned | Step 04 | Visual language, tokens, components, accessibility |
| `CATALOG_FORMAT.md` | planned | Step 05 | Tool catalog manifest format |
| `RELEASE_POLICY.md` | planned | Step 06 | Versioning, channels, signing, explicit-consent update policy |
| `CLI_CONTRACT.md` | planned | Step 07 | Shared CLI conventions, exit codes, JSON schemas |
| `MCP.md` | planned | M3 | MCP server guidance for Kim tools |

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
