# ADR 0002 — Independent tool binaries and workspace boundaries

Status: **Accepted** · Date: 2026-11-03 · Step 03

## Context

Kim is a platform of small tools that are installed one by one (`KIM-FR-001`, `KIM-FR-002`).
GUIs are optional, and headless paths must never load or open a GUI (`KIM-FR-012`). Tools must
interoperate without a monolithic runtime (`KIM-FR-020`).

## Decision

1. **One package per shipped tool.** `crates/kim` (Kim manager) and `crates/subak` (Subak) are
   separate packages with independent SemVer versions, release tags and installs. Neither
   depends on the other's crate.
2. **GUI binaries are separate from headless binaries.**
   - Kim: `kim.exe` (CLI, console subsystem) and `kim-manager.exe` (GUI, windows subsystem,
     Step 06).
   - Subak: `subak.exe` (CLI **and** Host, console subsystem) and `subak-gui.exe` (tabs,
     floating icon, session manager; windows subsystem, Steps 16–18).
   - A GUI binary links the UI toolkit; a headless binary never does. The CLI may *launch* a GUI
     only on an explicit GUI command (e.g. `subak gui`), never implicitly.
3. **Shared library crates only on real reuse.** Candidates, created when the second consumer
   appears: `subak-proto` (IPC types shared by `subak` and `subak-gui`), `kim-design` (design
   tokens shared by both GUIs), `kim-cli` (shared CLI contract helpers, Step 07). A shared crate
   must not pull a UI runtime into a headless binary.
4. **Tools talk through public contracts only**: CLI + `--json`, exit codes, documented files,
   and later MCP. No tool loads another tool's code.

## Consequences

- Kim can install Subak, and Subak can run without Kim being installed.
- Headless binaries stay small, with predictable idle cost.
- Some duplication between the two GUIs until `kim-design` exists. This is acceptable.

## Alternatives considered

- A single `kim` binary with all tools as subcommands: violates independent installs and drags
  every tool's dependencies everywhere.
- A plugin host loading tools as DLLs: fragile ABI and a de facto monolithic runtime.

## Requirements

`KIM-FR-001`, `KIM-FR-002`, `KIM-FR-012`, `KIM-FR-020`, `KIM-NFR-003`.
