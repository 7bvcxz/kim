# ADR 0030 — The Host's terminal model, scrollback and replay

Status: **Accepted** · Date: 2026-11-04 · Step 13 · Implements and refines
[ADR 0008](0008-attach-replay-and-backpressure.md); refines [ADR 0025](0025-pane-programs-environment-and-backpressure.md) §6
· Details, limits and evidence: [SCROLLBACK.md](../SCROLLBACK.md)

## Context

Until Step 12 the Host kept the last 64 KiB of each pane's raw output and replayed it on attach.
Steps 09 and 10 recorded what that implies: a client attaching after a full-screen program
started, or after the ring wrapped, saw garbage or a partial screen, and a reattached client got
output it had already shown again. A client that fell 16 MiB behind was disconnected. Step 13
must make reattach coherent for shell output, scrolling and full-screen programs, hand over to
live output without loss, duplication or reordering, bound memory, and give the CLI controls to
inspect, capture and clear history, with a privacy policy for what terminals print.

Measured on Windows 10 Pro 19045 before deciding (a probe pane at 80×24):

- ConPTY is itself a terminal emulator: it renders conhost's buffer as VT, using absolute cursor
  positions, `ESC[K`, `ESC[nX`, `ESC[nC`, SGR (16/256/truecolour), `ESC]0;title BEL` (cmd sets the
  title to the running command) and `\r\n`. Text that scrolls at the bottom arrives as a line feed
  on the last row; a `cls`, a resize or a program switching screens arrives as a full repaint from
  `ESC[H`. Soft-wrapped lines arrive with explicit `\r\n` (the newer ConPTY of Windows Server
  2025, build 26100, seen on CI, sends real soft wraps).
- A program's `ESC[?1049h` is **not** passed through by the inbox ConPTY; conhost switches its own
  buffer and repaints. `ESC[?2004h` (bracketed paste) is passed through.
- Output arrives in chunks of ~83 bytes at ~0.9 MiB/s (ADR 0025).

## Decision

1. **A parsed terminal model per pane, in-house** (`crates/subak/src/screen.rs`, portable, no
   dependency): a VT parser (the Paul Williams state machine, UTF-8, C0, ESC, CSI with private
   markers and sub-parameters, OSC, and DCS/SOS/PM/APC consumed) and an xterm-subset screen:
   cursor and addressing, erase/insert/delete, scroll regions and origin mode, `SU`/`SD`/index/
   reverse index, SGR with 16/256/truecolour and ten attributes, the alternate screen
   (47/1047/1048/1049), DEC private modes (interpreted ones, plus every other mode remembered and
   replayed as set), keypad mode, DEC line drawing, tab stops, saved cursor, title, cursor shape,
   East Asian wide characters and combining marks. Device reports are ignored: the attached
   terminal answers them, never the Host.
   - *Why a model and not a raw stream with a checkpoint:* a raw stream cannot reproduce a
     full-screen program once its start is gone, and a checkpoint is itself a screen state.
   - *Why in-house:* `alacritty_terminal` pulls an event loop, polling and signal handling into a
     headless Host; `vt100` stores scrollback as cell grids (~40 bytes per cell) and cannot
     restore scroll regions, saved cursor or partial sequences in its formatted output. ConPTY's
     output is a small, regular subset, and Linux (M4) needs the general case only for the
     sequences programs send, which the model already parses. The model is ~2 100 lines plus
     tests, with no `unsafe` and no allocation per byte in the common path.
   - *Cost (measured):* feeding typical coloured output runs at 68 MiB/s in a release build
     (11 MiB/s in a debug build), against ConPTY's 0.9 MiB/s; a 120×30 pane with 2 000 history
     lines holds ~245 KB.
2. **Scrollback = rows leaving the top of the primary screen** while the scroll region is the
   whole screen (xterm and tmux semantics), stored compactly as their text plus the SGR sequences
   they need. Bounded per pane by **lines** (`history_limit`, default 2 000, 0–100 000, tmux
   `history-limit`) **and bytes** (4 MiB, each line counting 32 more); the oldest lines are
   evicted first, so eviction is deterministic. The alternate screen has no history. A
   program's `ESC[3J` clears it, as in xterm.
3. **Attach = snapshot + stream, under one lock.** The pane's reader feeds every chunk into the
   model, advances the pane's sequence number and queues the chunk for attached clients while
   holding the Host lock; `pane.attach` takes the same lock to serialize the model and register
   the attachment. The snapshot is one or more data frames flagged *snapshot*, all carrying the
   `seq` the response names; live output continues at exactly that `seq`. The snapshot is VT:
   the requested history (printed and scrolled into the client terminal's own scrollback), the
   primary screen at absolute positions, then — if active — the cursor 1049 saved, `?1049h` and
   the alternate screen, then tab stops, the saved cursor, the scroll region, origin mode,
   charsets, modes, cursor shape, title, the current rendition, cursor visibility, the cursor
   (a pending wrap is reproduced by writing the last cell again), and finally the raw bytes of a
   sequence or UTF-8 character that was still incomplete, so the live bytes that follow complete
   it. The result says `snapshot: {bytes, frames, history_lines, alternate, size}`.
4. **Lagging clients are resynced, not dropped** (ADR 0008 §3, deferred by ADR 0025 §6). Output
   that does not fit in a connection's 16 MiB queue makes that *stream* lag: its queued output
   frames are dropped at once (control messages stay), no more output is queued for it, and once
   the writer has drained the queue to 4 MiB the Host sends `pane.resync` (to a connection
   subscribed to `panes`) and a snapshot that starts with CAN (ending a sequence the client may
   be in) and carries the history lines the dropped frames added. A control message that does
   not fit still disconnects. The pane never waits for a client.
5. **Size policy.** A pane has one size (ADR 0008 §4): the latest interactive attach or resize
   sets it; the model is resized first (as a terminal does before it tells its PTY), with rows
   that no longer fit above the cursor moving into the history, no reflow of the screen (ConPTY
   repaints it after the resize), and history rows re-wrapping at the client's width when
   replayed. Read-only viewers never resize; they get snapshots at the pane's size, and every
   attached client receives ConPTY's repaint after a resize.
6. **Retention and privacy.** History lives only in the Host's memory: never on disk, in logs,
   in events or in any JSON document except an explicit capture. It goes when the Terminal is
   removed or the Host exits; a `stopped` Terminal keeps its screen and history for capture.
   Persisting history is **deferred to the persistence Steps (29+)**; when it comes it is opt-in
   (`SUBAK-FR-042`) and user-only (`SUBAK-SEC-011`). Pane titles stay unreported in JSON (cmd puts
   whole command lines into them).
7. **Controls and access.** `pane.capture {target?, start?, end?, format: text|vt, join?,
   redact?}` (tmux `capture-pane`, line numbers as tmux: screen from 0, history from -1 down,
   `"-"` for the edges; at most 192 KiB of lines per answer, `truncated` otherwise) and
   `pane.clear_history {target?, dry_run?}` are `cli`/`gui` methods (MCP after the Step 37
   grant; hooks never). Captures **redact by default** (secret-looking assignments, bearer
   credentials, known token shapes, URL passwords, private-key blocks; `redact: false` shows
   exact text). Panes report numeric `history` counters; `host.status.scrollback` and
   `host.stats.scrollback` report totals. CLI: `subak capture`, `subak history`, `subak
   clear-history` (destructive: asks or needs `--yes`) and `subak new --history-limit`.
8. **Idle.** Nothing new runs when nothing happens: the model works on the reader thread when
   output arrives, a resync is triggered by the writer when it drains, and there is no timer.

## Consequences

- Reattach shows the screen a continuously attached client shows — measured equal (history,
  cells, attributes, cursor) for shell output, scrolling, `more`, and an alternate-screen test
  program, and for eight clients attaching while 3 000 lines were printed (SCROLLBACK §8).
- A reattached client no longer receives output it already had; the CLI attach puts the history
  into the console's scrollback once.
- Memory per pane is bounded: two grids of `cols × rows × 16` bytes and at most 4 MiB of history;
  256 Terminals × 4 MiB is the worst case (~1 GiB), documented rather than capped across panes.
- A stalled client costs at most its queue while it lags and nothing after the drop.
- `pane.attach` results gain `snapshot`; `replay_bytes` now counts the snapshot. Data frames
  flagged snapshot now carry the `seq` of the state they reproduce (Step 12: the offset of their
  first raw byte). Additive for clients that write frames as they come (`features`:
  `pane.snapshot`, `pane.capture`).
- Width tables are the model's own (Hangul, CJK, fullwidth, emoji, combining marks); a character
  conhost measures differently can shift the rest of its row until ConPTY repositions the cursor.

## Alternatives considered

- **Raw history with a periodic checkpoint:** needs the model anyway for the checkpoint, and
  replays output the client already showed.
- **`vt100` / `alacritty_terminal` crates:** see decision 1 (memory per scrollback cell, missing
  state in the formatted output, an event loop in a headless Host).
- **Resizing the pseudo console on attach so ConPTY repaints:** programs would see a resize on
  every attach, it cannot carry history, and it races with live output.
- **Disconnecting lagging clients (Step 11):** a viewer on a slow link would lose its session
  view under load; the resync keeps it connected with bounded memory.
- **Redaction off by default:** captures are often read by scripts and agents; `KIM-SEC-013`
  keeps secrets out of JSON output. Exact text needs an explicit `--no-redact`.
- **A Host-wide history cap:** cross-pane eviction would make one pane's output delete another's
  history; per-pane limits are deterministic.

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-015`, `SUBAK-FR-042`, `SUBAK-SEC-010`,
`SUBAK-SEC-011`, `KIM-SEC-013`, `KIM-NFR-001`, `KIM-NFR-011`, `KIM-NFR-013`.
