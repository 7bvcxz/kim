# ADR 0015 — CLI contract v1 and orchestrator-free tool discovery

Status: **Accepted** · Date: 2026-11-04 · Step 07

## Context

Every Kim tool must be usable by humans and agents through one predictable command-line
contract (`KIM-FR-010`, `KIM-FR-011`), headless paths must never open a GUI (`KIM-FR-012`), and
tools must discover and call each other without a resident orchestrator (`KIM-FR-020`). Steps 05
and 06 established a JSON envelope, stable error codes and exit codes for `kim`; Step 07 turns
them into a cross-tool contract before Subak's CLI (Step 12) is written against it.

## Decision

1. **One contract document, enforced by tests.** [CLI_CONTRACT.md](../CLI_CONTRACT.md) (v1)
   defines names, help, streams, `--quiet`/`--verbose`/`--no-input`, `--dry-run` for every
   effect other than `read`, `--yes` for destructive commands, exit-code classes, the error
   registry, the `--json` envelope, versioning and redaction. JSON Schemas
   (`cli-envelope-v1`, `kim-cli-v1`, `tool-capabilities-v1`) make it machine-checkable, and
   the `kim` tests validate every real `--json` output against them.
2. **Exit codes stay coarse classes (1–8).** New conditions get new error codes inside an
   existing class instead of new exit codes: `permission_denied` (1), `already_installed` (7),
   `capabilities_unavailable`, `capability_missing`, `tool_incompatible` (5),
   `capabilities_invalid` (4), `timeout` (6, "unavailable" now covers timeouts as well as the
   network). `already_installed` replaces the `usage` code Step 06 used for "nothing to do",
   because it is a state conflict, not a malformed command; the exit code changes from 2 to 7.
   Settings commands now report `settings.show` / `settings.set` instead of `settings`.
3. **Capabilities instead of an orchestrator.** Each tool implements a cheap, side-effect-free
   `<tool> capabilities --json` that lists its real commands with their effect, network use,
   dry-run and confirmation needs, and whether they open a window. Packages ship the same
   document as `capabilities.json`; `kim tools list|describe` read it from the install without
   starting anything. `--probe` asks the tool itself, headless and time-limited. Version
   negotiation is a set of structured errors (`not_installed`, `tool_incompatible`,
   `capability_missing`, `capabilities_unavailable`, `capabilities_invalid`, `timeout`).
   Consent rules are tied to the declared effect: only `read` commands may be called freely.
4. **Redaction at the output boundary.** Every string `kim` prints (JSON, human output,
   `--verbose` diagnostics) and every URL it stores is passed through `kim::redact`, which
   masks URL user-info and secret-looking query/fragment parameters. Doing it once at the
   boundary covers messages produced by dependencies (e.g. HTTP errors that echo URLs).
5. **No shared `kim-cli` crate yet.** ADR 0002 creates shared crates on real reuse. Today only
   `kim` implements the contract in Rust; Subak's placeholder emits two static documents. The
   shared surface is the contract and the schemas, not code. When Step 12 writes Subak's real
   CLI, the envelope/error/redaction helpers move into `crates/kim-cli` (no GUI dependencies),
   with this ADR as the justification.
6. **One schema validator, compiled twice.** The subset JSON Schema validator moves from
   `xtask/src/schema.rs` to the self-contained `xtask/src/jsonschema.rs`. `cargo xtask
   schema-check` uses it for committed examples; the `kim` and `subak` integration tests include
   the same file as a `#[path]` module to validate real binary output. Rejected: a dev-dependency
   on xtask (xtask already depends on `kim`, so it would be a cycle and compile `kim` twice);
   a separate crate (a crate for ~300 lines used only by tests, against ADR 0002's "real reuse"
   rule); the `jsonschema` crate (large dependency tree, and a different keyword subset than the
   one the repository's schemas are restricted to); a copied validator (drift).
7. **Headless proof by measurement.** Besides `deps-check` (no GUI crates in headless packages)
   and a source inspection test (only two documented process spawns in `kim`), a Windows test
   runs every read-only command, dry run and failure inside a job object created before `kim`
   executes, and asserts the job never contained another process.

## Consequences

- Agents get one way to call any Kim tool; scripts can branch on exit classes.
- Every additive output change requires updating a schema and the golden examples
  (`KIM_BLESS=1 cargo test -p kim --test cli_contract`), which makes changes visible in review.
- `already_installed` (exit 7) and `settings.show`/`settings.set` are behavior changes for
  callers of Step 06's CLI; no released Kim exists yet, so no migration is needed.
- Discovery shows capabilities only for tools that ship a manifest; older tools need `--probe`
  (an explicit, bounded process start) or report `capabilities_unavailable`.

## Alternatives considered

- **A Kim daemon/registry that tools register with:** contradicts "no always-on daemons" and
  adds a failure point; rejected.
- **Discovery by always probing installed binaries:** starts processes on every listing and
  could start a GUI for a foreign or old program that ignores `capabilities`; kept as opt-in
  `--probe` only.
- **Fine-grained exit codes per error:** collide with shell conventions and are hard to keep
  stable; error codes carry the precision instead.

## Requirements

`KIM-FR-010`, `KIM-FR-011`, `KIM-FR-012`, `KIM-FR-013`, `KIM-FR-020`, `KIM-FR-021`,
`KIM-NFR-001`, `KIM-NFR-014`, `KIM-SEC-001`, `KIM-SEC-013`.
