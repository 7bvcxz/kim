# ADR 0012 — Local performance metrics, no telemetry

Status: **Accepted** · Date: 2026-11-03 · Step 03

## Context

Budgets for idle CPU, memory, latency and throughput (`KIM-NFR-001…005`) need measurement.
Kim promises no telemetry and no collection of sensitive data (`KIM-SEC-012`).

## Decision

1. **Metrics live in memory in the Host and are read locally on demand**, through
   `subak host stats [--json]` (IPC `host.stats`). Nothing is sent anywhere. Nothing is written
   to disk unless the user exports it.
2. **Allowed metric content: numbers and enumerations only.** No terminal content, command
   lines, paths, environment values, names chosen by the user, or Claude data.
3. **Initial metric set** ([ARCHITECTURE §8](../ARCHITECTURE.md) defines units and method):

   | Metric | Kind |
   | --- | --- |
   | `host.uptime_s`, `host.cpu_time_ms`, `host.working_set_bytes`, `host.handle_count`, `host.thread_count` | gauge |
   | `terminals`, `windows`, `panes`, `clients` | gauge |
   | `pane.output_bytes_total`, `pane.input_bytes_total` | counter |
   | `ipc.requests_total{type}`, `ipc.errors_total{code}` | counter |
   | `ipc.request_latency_us{type}` (p50/p95/max over a sliding window) | histogram |
   | `attach.snapshot_bytes`, `attach.latency_us` | histogram |
   | `client.resyncs_total` | counter |
   | `state.writes_total`, `state.write_latency_us` | counter / histogram |

4. **Benchmarks** that need external observation (idle CPU over 60 s, keystroke echo latency)
   run in harnesses (Steps 14, 15, 39) and record the machine, OS build and method next to the
   results. No number is published without that context.

## Consequences

Users and agents can diagnose performance without giving up privacy. Regression thresholds
(Step 39) are based on these measurements.

## Requirements

`KIM-NFR-001`, `KIM-NFR-002`, `KIM-NFR-005`, `KIM-SEC-012`, `KIM-NFR-004`.
