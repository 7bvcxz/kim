# Architecture decision records

Each ADR records one decision: context, the decision, consequences, rejected alternatives and
the requirements it serves. ADRs are immutable once **Accepted**; a later ADR may supersede one
(and the old one is marked *Superseded by NNNN*). Proposed ADRs may change freely.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-independent-tool-binaries.md) | Independent tool binaries and workspace boundaries | Accepted (note: Step 07) |
| [0003](0003-subak-host-process-model.md) | Subak Host process model | Accepted (validated in Step 08, see 0016; implemented in Step 09, see 0019; §7 in Step 10, see 0020) |
| [0004](0004-pty-and-process-ownership.md) | PTY and process ownership on Windows | Accepted (validated in Step 08, see 0016; refined in Step 11, see 0025) |
| [0005](0005-local-ipc.md) | Local IPC transport, security and versioning | Accepted (prototype in Step 08; implemented in Step 09, see 0019) |
| [0006](0006-state-and-configuration-storage.md) | State and configuration storage | Accepted |
| [0007](0007-object-identity-and-naming.md) | Object identity, naming and targets | Accepted (implemented for Terminals in Step 10, see 0020) |
| [0008](0008-attach-replay-and-backpressure.md) | Attach, replay and backpressure | Accepted (send queue refined in Step 11, see 0025; refined in Step 13) |
| [0009](0009-gui-clients-and-keyboard-dispatch.md) | GUI clients and keyboard dispatch | Accepted (toolkit chosen in Step 04; bindings follow ADR 0017; tab close refined by 0020) |
| [0010](0010-claude-code-integration.md) | Claude Code status integration | Accepted (§1 pane variables implemented in Step 11; refined in Step 27) |
| [0011](0011-portability-boundary.md) | Portability boundary for Linux | Accepted (Pty and ProcessTree form one trait since Step 11, see 0025) |
| [0012](0012-local-metrics-no-telemetry.md) | Local performance metrics, no telemetry | Accepted |
| [0013](0013-kim-manager-architecture.md) | Kim manager: catalog and explicit installer | Accepted (refined in Steps 05–07) |
| [0014](0014-gui-technology.md) | GUI technology: egui + glow for chrome, dedicated terminal renderer | Accepted (re-validated in Steps 15–16) |
| [0015](0015-cli-contract-and-tool-discovery.md) | CLI contract v1 and orchestrator-free tool discovery | Accepted |
| [0016](0016-conpty-host-validation.md) | ConPTY ownership and the detached Host, validated on Windows | Accepted |
| [0017](0017-universal-customizable-hotkeys.md) | Universal customizable hotkeys without a shared hook or daemon | Accepted (implemented from Step 46) |
| [0019](0019-secure-host-supervisor-and-ipc-v1.md) | Secure Host supervisor and IPC v1 | Accepted |
| [0020](0020-terminal-session-lifecycle.md) | Terminal session lifecycle and the meaning of every end | Accepted |
| [0021](0021-noeul-product-definition.md) | Noeul product definition: double-click pins, privacy defaults and a clean-room boundary | Accepted (implemented from Step 49) |
| [0022](0022-ddalgak-rule-model-and-safety.md) | Ddalgak: declarative rules, a tray-app engine and safety by construction | Accepted (implemented from Steps 66–83) |
| [0025](0025-pane-programs-environment-and-backpressure.md) | Pane programs, their environment, and backpressure | Accepted |

"Accepted (validated in Step N)" means the decision is made, but the named Step must confirm it
on real Windows; if validation fails, that Step writes a superseding ADR.
