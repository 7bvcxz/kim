# Contributing to Kim

Thank you for helping. Read [AGENTS.md](AGENTS.md) first: its rules apply to every human and
AI contributor. Security problems go through [SECURITY.md](SECURITY.md), never public issues.

## Developer setup (Windows first)

1. **Install Rust with rustup** — <https://rustup.rs>. The exact toolchain (and `rustfmt`,
   `clippy`) is pinned in `rust-toolchain.toml`; rustup installs it automatically on first use
   in this repository (or run `rustup toolchain install`).
2. **Choose a Windows C toolchain** — either works for development:
   - **MSVC (recommended, used by CI and releases):** Visual Studio Build Tools with the
     *Desktop development with C++* workload. Requires administrator rights to install.
   - **GNU (no administrator rights):** `rustup-init.exe --default-host x86_64-pc-windows-gnu`.
     rustup ships the needed MinGW linker. Note that release builds are produced with MSVC.
     Crates using `windows-sys` (raw-dylib) need an **x86_64** GNU `dlltool` *and* `as` on
     `PATH`; the rustup bundle ships `dlltool` without `as`. Unzip the `as.exe`/`dlltool.exe`
     (plus their DLLs) from a checksum-verified [WinLibs](https://winlibs.com) x86_64 UCRT
     release into a user directory and put it first on `PATH`. Symptoms otherwise:
     `dlltool … Invalid bfd target` (an old 32-bit MinGW on `PATH`) or `dlltool … CreateProcess`
     (no `as`).
3. **Clone and verify:**

   ```sh
   git clone https://github.com/hellomouv/kim
   cd kim
   cargo xtask ci
   ```

Linux and macOS can compile the workspace, but **Linux runtime support is not claimed** until
milestone M4 (`KIM-NFR-011`); macOS is out of scope.

## Everyday commands

| Purpose | Command |
| --- | --- |
| Everything CI checks (fmt, clippy, tests, docs) | `cargo xtask ci` |
| Format | `cargo fmt --all` |
| Lint (warnings are errors) | `cargo clippy --workspace --all-targets -- -D warnings` |
| Tests (unit + integration; never e2e) | `cargo test` |
| Documentation consistency | `cargo xtask docs-check` |
| Contract schemas vs examples | `cargo xtask schema-check [--verbose]` |
| Refresh `kim --json` golden examples after an intended output change (review the diff) | `KIM_BLESS=1 cargo test -p kim --test cli_contract` |
| Design token contrast | `cargo xtask design-check [--verbose]` |
| Dependency policy | `cargo deny check` (see SECURITY.md) |
| E2E status | `cargo xtask e2e-status` |
| E2E suite (only when due) | `cargo xtask e2e` |

## Workspace layout

| Path | Package | What it is |
| --- | --- | --- |
| `crates/kim` | `kim` | Kim manager library + CLI (binary `kim`): catalog, signatures, installer, status, tool discovery ([CLI contract](docs/CLI_CONTRACT.md)) |
| `catalog/` | — | The signed catalog bundled into `kim` and published for `kim catalog refresh` ([format](docs/CATALOG_FORMAT.md)) |
| `crates/kim-manager` | `kim-manager` | Kim Manager GUI (egui + glow, ADR 0014); the only package allowed to depend on GUI crates so far |
| `crates/subak` | `subak` | Subak CLI (binary `subak`): `capabilities` and the Step 08 ConPTY Host prototype (`subak host …`, [CONPTY_PROOF](docs/CONPTY_PROOF.md)); sessions and GUI arrive in later steps |
| `xtask` | `xtask` | Repository automation (docs/schema checks, CI gate, gated e2e runner); never shipped |
| `e2e` | `kim-e2e` | End-to-end suite; excluded from `default-members` and CI |

Every tool is its own package and binary with its own version — no tool depends on another
tool's crate (`KIM-FR-001`). Shared library crates are created only when real reuse appears,
and must not drag GUI runtimes into headless binaries. Nothing in the workspace installs a
background service.

## Tests and the e2e cadence

- Every feature ships with tests in the same change, and those tests run in `cargo test`.
- Every feature also gets an e2e scenario in `e2e/tests/` (one file per Step,
  `stepNN_<topic>.rs`). E2E tests are `#[ignore]`d **and** refuse to run unless started by
  `cargo xtask e2e`.
- The e2e suite runs **once per 15 processed Step issues**. `cargo xtask e2e` checks
  [e2e/LEDGER.md](e2e/LEDGER.md) and refuses otherwise. Record every processed Step and every
  e2e run in the ledger.

## Versioning conventions

- Each tool follows [Semantic Versioning](https://semver.org) **independently** (the `version`
  in its own `Cargo.toml`). Pre-1.0 (`0.y.z`) minor bumps may break compatibility; this is
  called out in release notes.
- Release tags are per tool: `kim-v0.1.0`, `subak-v0.1.0`.
- Schemas that cross process or version boundaries (CLI `--json` output, IPC, stored state,
  catalog manifests) carry their own schema version, independent of the tool version
  (`KIM-NFR-014`).
- Details of channels, signing and update consent: `docs/RELEASE_POLICY.md` (planned, Step 06).

## Minimum supported Rust version (MSRV)

Kim ships binaries, not libraries, so the MSRV **equals the pinned toolchain** in
`rust-toolchain.toml` (mirrored as `rust-version` in the workspace `Cargo.toml`). Bump both
together in a dedicated change, at most once per Rust release cycle, after `cargo xtask ci` passes.

## Build profiles

- `dev`: line-table debug info for faster builds.
- `release`: thin LTO, one codegen unit, stripped symbols, panics unwind (so the Subak Host can
  isolate a failing request without losing every session).

## Branches and merging

This repository does not use pull requests (see [AGENTS.md](AGENTS.md) §6).

- Branch per issue; reference the issue (`Refs #N`) and requirement IDs in commits.
- Push the branch: CI runs on every branch push. It must pass: Windows fmt/clippy/tests/docs,
  the Linux compile check, and the dependency policy.
- Merge into `main` locally with `git merge --no-ff` after the branch's CI is green and
  `cargo xtask ci` passes on the merged result, then push `main`.
- Keep changes focused; update the docs your change affects.

## License

Kim is dual-licensed under **MIT OR Apache-2.0** ([LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE)), the Rust ecosystem convention. Unless you state otherwise,
any contribution you submit is licensed the same way, without additional terms.
