# ADR 0011 — Portability boundary for Linux

Status: **Accepted** · Date: 2026-11-03 · Step 03

## Context

Windows is first (`KIM-NFR-010`); Linux comes in M4 (`SUBAK-FR-070`). Code must be ready for a
Linux port without implementing or claiming Linux prematurely (`KIM-NFR-011`).

## Decision

1. **Five narrow platform interfaces**, each in a `platform` module with a `windows`
   implementation:

   | Interface | Responsibility | Windows | Linux (M4) |
   | --- | --- | --- | --- |
   | `Pty` | spawn a process in a pseudo terminal; read, write and resize; exit status | ConPTY | `openpty` + `fork/exec`, process group |
   | `ProcessTree` | contain and kill a pane's descendants; enumerate the foreground process | Job Object | process group / cgroup (TBD) |
   | `IpcTransport` | per-user local endpoint, peer identity | named pipe + DACL | Unix socket + `SO_PEERCRED` |
   | `Paths` | config, state, logs and runtime directories | Known Folders | XDG |
   | `SecretStore` | store and fetch secrets by reference | Credential Manager / DPAPI | Secret Service / keyring |

2. **Everything else is portable**: session model, protocol, state schema, VT emulation,
   layout, key dispatch logic and CLI.
3. **On non-Windows targets**, the interfaces compile to an implementation that returns a
   structured `unsupported_platform` error. Commands that need them exit with the documented
   error code. CI checks compilation on Linux and runs no runtime tests there (Step 02).
4. **No speculative abstraction.** A trait is introduced only where both an implementation and
   a test exist. Until M4 the "Linux implementation" is just the unsupported stub.

## Consequences

The Linux port (Steps 41–42) replaces stubs instead of untangling Windows calls spread through
the codebase.

## Requirements

`KIM-NFR-010`, `KIM-NFR-011`, `SUBAK-FR-070`.

## Notes

- 2026-11-04 (Step 11): the `Pty` and `ProcessTree` rows of §1 are one trait for a pane,
  `pty::Pty` in `crates/subak/src/pty.rs`, because the pseudo terminal and its container are
  created together; the foreground-process query of `ProcessTree` arrives with Step 21.
  Non-Windows targets get the `unsupported_platform` stub of §3
  ([ADR 0025](0025-pane-programs-environment-and-backpressure.md) §7).
