# E2E ledger

Rules: [AGENTS.md §4](../AGENTS.md#4-development-cadence-tests-and-e2e-maintainer-rules).
The e2e suite runs **once per 15 processed Step issues** and never otherwise. Append one row per
processed Step issue; when *Since last e2e run* reaches 15, run the suite once and log it below.

Harness: crate `kim-e2e` (`e2e/`), one test file per Step (`e2e/tests/stepNN_<topic>.rs`).
Run with `cargo xtask e2e`, which refuses unless the counter below has reached 15.

## Processed Step issues

| # | Step issue | Features added to e2e | Since last e2e run |
| --- | --- | --- | --- |
| 1 | #1 Step 01 – Product philosophy (docs only) | none (no executable feature) | 1 |
| 2 | #2 Step 02 – Rust workspace and engineering standards | `step02_placeholder_binaries.rs`: `kim`/`subak` version, help, usage errors, no writes to user data dirs | 2 |
| 3 | #3 Step 03 – Architecture and compatibility specifications (design) | none (no product feature; contract fixtures are checked by `cargo test` and will be reused by Step 09 e2e) | 3 |
| 4 | #4 Step 04 – Kim native design system (design) | none (no product feature; token contrast is checked by `cargo test` via `design-check`) | 4 |
| 5 | #5 Step 05 – Kim tool catalog | `step05_catalog.rs`: offline signed listing writes nothing, explicit refresh from a local mirror, tampered catalog exit 4, unknown tool exit 3; `step02` help check updated for the real `kim` CLI | 5 |
| 6 | #6 Step 06 – Kim Manager and manual installer | `step06_installer.rs`: debug `kim` explicit install → update check → tampered update refused → update → uninstall needs `--yes`, keeps data; release `kim` ignores the development key | 6 |
| 7 | #7 Step 07 – Kim CLI contract and headless foundation | `step07_cli_contract.rs` (4 tests): JSON envelope/schemas on real output, capabilities and `tools list/describe`, headless (no GUI/child processes), redaction | 7 |
| 8 | #50 Step 45 – Universal customizable hotkey contract (specification) | none (no executable feature; 17 conflict scenarios are recomputed by `cargo xtask schema-check`, and Step 46 adds the e2e scenario for `kim hotkeys`) | 8 |
| 9 | #8 Step 08 – Subak ConPTY Host proof (merged; issue open for manual Windows Terminal close, sleep and logoff checks) | `step08_conpty_host.rs`: real `subak host start/status/attach/stop` on a private endpoint, Host survives client kill, reattach sees earlier shell state, explicit stop needs `--yes` and leaves nothing behind | 9 |
| 10 | #51 Step 46 – Shared hotkey configuration and conflict detection | `step46_hotkeys.rs` (3 tests): `kim hotkeys` list/set/disable/enable/reset cycle in an isolated profile, cross-tool `check --all-tools` conflicts read from files only, a broken `hotkeys.json` means safe mode and explicit recovery | 10 |
| 11 | #9 Step 09 – Secure Host supervisor and IPC v1 | `step09_host_ipc.rs` (3 tests): an agent starts one healthy Host on demand that exits when idle, two terminals share one pane and a killed one changes nothing, a program holding the pipe name is refused and sent nothing | 11 |
| 12 | #52 Step 47 – Noeul and Ddalgak in the Kim tool catalog | `step47_catalog_tools.rs`: (2 tests) a fresh machine lists Noeul and Ddalgak as planned and installs nothing; a test-signed Noeul release is disclosed, installed alone and never stopped while it runs | 12 |
| 13 | #53 Step 48 – Noeul philosophy, requirements and Snipaste parity matrix (specification) | none (no executable feature; the F1 → double-click-to-pin e2e scenario is planned for Step 51 in `docs/NOEUL_UX_SPEC.md`) | 13 |
| 14 | #70 Step 65 – Ddalgak philosophy, AHK v2 matrix, rule model and safety (specification) | none (no executable feature; `ddalgak-rules-v1` and `ddalgak-simulation-v1` examples are checked by `cargo xtask schema-check`; the engine e2e scenario comes with Step 66) | 14 |
| 15 | #10 Step 10 – Terminal session lifecycle | `step10_session_lifecycle.rs`: (2 tests) an agent manages named Terminals with the real `subak terminal` commands without losing running work (detach changes nothing, destructive actions need consent, `host stop` needs `--kill-terminals`); six agents creating one name at once get exactly one Terminal. Step 08/09 scenarios updated to the new `host stop` rule | 15 |
| 16 | #54 Step 49 – Noeul native app and capture service foundation | `step49_noeul_foundation.rs` (3 tests, real `noeul` with `--no-tray`, private endpoint, obscure chords only): headless discovery starts and writes nothing; start, hand-over to the running instance, rebind while running and quit; honest errors without an instance | 1 |
| 17 | #11 Step 11 – PTY and shell process management | `step11_pty_shells.rs` (2 tests): an agent runs PowerShell and CMD with environment overrides and no leaked secrets; explicit programs report exit codes and failures are precise (missing program, script refused with a hint, bad working directory) | 2 |
| 18 | #71 Step 66 – Ddalgak Windows native hotkey event engine (merged; issue open for the maintainer's manual shortcut-press, emergency-chord and tray checks) | `step66_ddalgak_engine.rs` (3 tests): a headless agent never starts the app; an agent drives the engine of the running app (obscure chords, private Run key); after a crash or an emergency the engine starts off | 3 |
| 19 | #55 Step 50 – Noeul F1 region capture overlay (merged; issue open for the maintainer's real-F1 check, `NOEUL_UX_SPEC.md` §4.5) | `step50_noeul_overlay.rs` (3 tests): the overlay is discoverable headlessly; the capture chord (obscure) registers, a dry run captures nothing and a conflict is reported; rapid triggers open one off-screen overlay (debug synthetic desktop) and cancel leaves nothing | 4 |

## E2E runs

| Date | After Step | Command | Result |
| --- | --- | --- | --- |
| 2026-11-04 09:53 | 15 (#10 Step 10) | `cargo xtask e2e` at e65c2a7 | **FAIL** — `step07_cli_contract::agent_discovers_negotiates_and_probes_an_installed_subak`: the scenario still assumed Subak lacked `terminal.list`, which Step 10 added (stale test, not a product bug). `cargo test` then stopped, so the Step 08/09/10/46/47 files did not run. |
| 2026-11-04 09:55 | 15 (same run, verification) | `cargo xtask e2e` with the Step 07 fix and `--no-fail-fast` (aa4bad2) | **FAIL** — 22 of 23 passed; `step09_host_ipc::a_program_holding_the_pipe_name_is_refused_and_sent_nothing` failed about 1 in 3: the PowerShell squatter started listening only after printing READY, so .NET threw when Subak connected and left first, and the squatter exited. Subak sent 0 bytes every time (security property held). |
| 2026-11-04 10:02 | 15 (same run, verification) | `cargo xtask e2e` with both fixes (ab1d6c7, merged as the next main commit) | **PASS** — 23 of 23 scenarios across 9 files; no `subak.exe`/`kim.exe` left running. Fixes: Step 07 uses a never-declared command for `capability_missing`; the Step 09 squatter listens before READY (15/15 focused runs); `cargo xtask e2e` passes `--no-fail-fast`. |
