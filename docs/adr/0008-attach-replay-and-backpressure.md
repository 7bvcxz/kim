# ADR 0008 — Attach, replay and backpressure

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Refined and implemented in Step 13

## Context

A client that attaches to a running Pane must see a coherent screen, even for full-screen TUIs
such as editors and Claude Code. Several clients may watch the same Pane. A slow client must
never stall the program running in the Pane, or other clients.

## Decision

1. **The Host keeps terminal state per Pane.** The pane reader thread feeds every output byte
   into:
   - a headless **VT emulator** (screen grid, cursor, modes, alternate screen, title). The
     crate is chosen in Step 13/15, with `alacritty_terminal` and `vt100` as candidates.
   - a bounded **history ring** of raw output with monotonically increasing sequence numbers
     (default 4 MiB per pane, configurable).
2. **Attach = snapshot + stream.** On attach, the Host serializes the emulator state as a VT
   byte sequence that reproduces the current screen (and scrollback up to a limit), tagged with
   the sequence number it reflects. Then it streams data frames after that sequence. Nothing is
   lost, duplicated or reordered, because snapshot and stream are taken under the same pane
   lock.
3. **Backpressure.** Each client subscription has a bounded queue (default 1 MiB).
   - The pane reader never blocks on a client.
   - When a client's queue overflows, its pending frames are dropped and it receives a
     `pane.resync` event followed by a fresh snapshot.
   - Clients do not affect each other.
   - The PTY itself is read as fast as it produces, so a program never blocks because of a
     slow viewer.
4. **Size arbitration.** A pane has one size, as in a real terminal.
   - Default policy: the **most recently active** interactive client (the one that last sent
     input or a resize) sets the size. This is tmux's `window-size latest`.
   - Read-only viewers never resize.
   - The CLI `attach` is interactive. Headless capture (`subak capture`) is not.
5. **Input.** Any interactive attached client may send input. Input is serialized in arrival
   order. There are no locks between clients in v1; the tmux `-r` read-only attach mode is
   supported.
6. **Idle cost.** With no output and no clients, nothing runs: threads are blocked on I/O, and
   there are no timers.

## Consequences

- Reattach shows the same screen a continuously attached viewer would see, within the limits
  of the emulator's fidelity, which Step 15 measures with conformance tests.
- Memory per pane is bounded: emulator grid + ring + scrollback limit.
- Scrollback persisted to disk is a separate, opt-in feature ([ADR 0006](0006-state-and-configuration-storage.md)).

## Alternatives considered

- **Replaying raw history only:** wrong results for full-screen TUIs once the ring has wrapped.
- **Blocking the PTY when clients lag:** would freeze the user's programs because of a viewer.
- **Per-client independent sizes:** impossible with one PTY per pane.

## Requirements

`SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-015`, `SUBAK-FR-042`, `KIM-NFR-001`.

## Notes

- 2026-11-04 (Step 11): the per-connection send queue is bounded by bytes only (16 MiB,
  each frame counting its size plus 64), no longer also by 4096 frames: ConPTY delivers
  ~83-byte chunks on Windows 10, so a frame count dropped a client after ~0.35 s of lag.
  Output frames are shared by every client of a pane. A lagging client is still
  disconnected until this ADR's resync arrives in Step 13
  ([ADR 0025](0025-pane-programs-environment-and-backpressure.md) §6).
- 2026-11-04 (Step 13): implemented by [ADR 0030](0030-host-terminal-model-scrollback-and-replay.md).
  §1: the emulator is an in-house model (`crates/subak/src/screen.rs`), not `alacritty_terminal`
  or `vt100`; the raw history ring is gone, and the scrollback is the model's history (default
  2 000 rows and 4 MiB per pane). §2: as decided, with the snapshot spanning several data frames
  at one `seq`. §3: the per-connection queue stays 16 MiB (ADR 0025); a lagging stream's queued
  output is dropped and it is resynced once its queue drained to 4 MiB. `subak capture` is
  implemented (§4 names it). Evidence: [SCROLLBACK.md](../SCROLLBACK.md) §8.
