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

## E2E runs

| Date | After Step | Command | Result |
| --- | --- | --- | --- |
| — | — | — | not run yet (next run due after the 15th processed Step) |
