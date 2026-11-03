# Architecture decision records

Each ADR records one decision: context, the decision, consequences, rejected alternatives and
the requirements it serves. ADRs are immutable once **Accepted**; a later ADR may supersede one
(and the old one is marked *Superseded by NNNN*). Proposed ADRs may change freely.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-independent-tool-binaries.md) | Independent tool binaries and workspace boundaries | Accepted |
| [0003](0003-subak-host-process-model.md) | Subak Host process model | Accepted |
| [0004](0004-pty-and-process-ownership.md) | PTY and process ownership on Windows | Accepted (validated in Step 08) |
| [0005](0005-local-ipc.md) | Local IPC transport, security and versioning | Accepted (implemented in Step 09) |
| [0006](0006-state-and-configuration-storage.md) | State and configuration storage | Accepted |
| [0007](0007-object-identity-and-naming.md) | Object identity, naming and targets | Accepted |
| [0008](0008-attach-replay-and-backpressure.md) | Attach, replay and backpressure | Accepted (refined in Step 13) |
| [0009](0009-gui-clients-and-keyboard-dispatch.md) | GUI clients and keyboard dispatch | Accepted (toolkit chosen in Step 04) |
| [0010](0010-claude-code-integration.md) | Claude Code status integration | Accepted (refined in Step 27) |
| [0011](0011-portability-boundary.md) | Portability boundary for Linux | Accepted |
| [0012](0012-local-metrics-no-telemetry.md) | Local performance metrics, no telemetry | Accepted |
| [0013](0013-kim-manager-architecture.md) | Kim manager: catalog and explicit installer | Accepted (refined in Steps 05–06) |

"Accepted (validated in Step N)" means the decision is made, but the named Step must confirm it
on real Windows; if validation fails, that Step writes a superseding ADR.
