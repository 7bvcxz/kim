# E2E ledger

Rules: [AGENTS.md §4](../AGENTS.md#4-development-cadence-tests-and-e2e-maintainer-rules).
The e2e suite runs **once per 15 processed Step issues** and never otherwise. Append one row per
processed Step issue; when *Since last e2e run* reaches 15, run the suite once and log it below.

The e2e harness itself is introduced by Step 02 (Rust workspace); until then Steps add no e2e
scenarios because they ship no executable features.

## Processed Step issues

| # | Step issue | Features added to e2e | Since last e2e run |
| --- | --- | --- | --- |
| 1 | #1 Step 01 – Product philosophy (docs only) | none (no executable feature) | 1 |

## E2E runs

| Date | After Step | Command | Result |
| --- | --- | --- | --- |
| — | — | — | not run yet (next run due after the 15th processed Step) |
