# Subak scrollback and replay

Status: **current** (implemented in Step 13; Windows only) · Decision:
[ADR 0030](adr/0030-host-terminal-model-scrollback-and-replay.md), refining
[ADR 0008](adr/0008-attach-replay-and-backpressure.md) · Requirements: `SUBAK-FR-001`,
`SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-015`, `SUBAK-FR-042`, `SUBAK-SEC-010`, `SUBAK-SEC-011`,
`KIM-SEC-013`, `KIM-NFR-001`

This document says what the Host keeps of every pane's output, how a client that attaches later
gets a coherent screen, how slow clients are handled, what the CLI can read and clear, and the
privacy rules for terminal output. The code is `crates/subak-term/src/screen.rs` (the terminal
model, portable; moved out of `subak` in Step 15 so client views run the same model —
[TERMINAL_EMULATOR](TERMINAL_EMULATOR.md), ADR 0036), `crates/subak/src/redact.rs` (capture
redaction) and `crates/subak/src/host.rs`
(locking, snapshots, resync, the IPC methods); the wire format is in
[IPC_PROTOCOL §1 and §4](contracts/IPC_PROTOCOL.md); the commands are in
[SUBAK_CLI §3](SUBAK_CLI.md).

## 1. What the Host keeps per pane

Every byte a pane writes goes, on the pane's reader thread, into that pane's **terminal model**:
a VT parser and screen emulator that keeps what a terminal attached to the pane would show.

| Part | What it holds | Bound |
| --- | --- | --- |
| Primary screen | `cols × rows` cells: character, colours (default, 256-colour index, truecolour), attributes (bold, dim, italic, underline, double underline, blink, inverse, hidden, strike, overline), wide-character halves, combining marks, OSC 8 hyperlinks (since Step 15), soft wraps | 16 bytes per cell (120×30: ~58 KB), plus rare per-column marks and links |
| Alternate screen | the same, while a full-screen program uses it (47/1047/1049) | freed when the program leaves it |
| History (scrollback) | rows that scrolled off the top of the primary screen while the scroll region was the whole screen, as compact text with the SGR (and, since Step 15, OSC 8) sequences they need | `history_limit` rows (default **2 000**, 0–100 000) **and 4 MiB** per pane, each row counting 32 bytes more |
| State | cursor (and a pending wrap), scroll region, origin mode, autowrap, insert and newline modes, cursor keys and keypad modes, reverse video, cursor visibility and shape, the mouse tracking and encoding modes (one of each at a time, since Step 15), every other DEC private mode as last set (bracketed paste, focus events…), the active hyperlink, DEC line drawing and the G0/G1 shift, tab stops, the cursor saved by DECSC/1049, the window title, and the bytes of an escape sequence or UTF-8 character still incomplete | small |

**Eviction is deterministic:** when a new history row would exceed either limit, the oldest rows
are dropped first, one by one, until both limits hold. Nothing else is ever dropped, and the
screens are never evicted. A limit of 0 keeps no history (rows are counted as evicted).

**Not kept:** the raw byte stream (since Step 13 there is no raw replay ring), what scrolls off a
partial scroll region or the alternate screen (as in xterm and tmux), clipboard requests
(OSC 52), device reports and queries (the attached terminal answers them, never the Host; a
client view answers them only when it owns the pane's size, [TERMINAL_EMULATOR §3.4](TERMINAL_EMULATOR.md)).
Hyperlinks (OSC 8) are kept since Step 15; `capture --vt` includes them, plain `capture` does not.

## 2. Attach: snapshot, then live output

`pane.attach` (and so `subak attach`) works like this, all under the lock the pane's reader
takes for every chunk it feeds the model:

1. the model is resized if an interactive client asked for another size (§3);
2. the response names `seq`, the pane's output position, and `snapshot: {bytes, frames,
   history_lines, alternate, size}`;
3. one or more data frames flagged *snapshot*, each carrying that `seq`, reproduce the terminal
   on a client terminal of `size` that is in its default state;
4. live output follows, its first frame at exactly `seq`.

Because the snapshot and the start of live output are taken under the same lock as the model
update, **nothing is lost, duplicated or reordered** between them (measured in §8 with clients
attaching while thousands of lines were printed).

The snapshot is plain VT, in this order: a reset of rendition, charsets, margins and modes; the
history (`history_lines` rows, default all that are kept), printed from the top and then
scrolled up into the client terminal's **own scrollback**, once; the primary screen at absolute
positions; when a full-screen program is active, the cursor it saved, `ESC[?1049h` and the
alternate screen; tab stops when they differ from the default; the saved cursor; the scroll
region and origin mode; charsets; modes; the cursor shape; the title; the current rendition;
the cursor position (a pending wrap at the last column is reproduced by writing that cell
again); cursor visibility; and finally the raw bytes of an incomplete sequence, so the live bytes
that follow complete it exactly as they would have.

A reattached CLI client therefore shows the screen as it was, with the earlier lines in its
scrollback, and **no line twice** (Step 09–12's raw replay showed recent output again).

On Windows 10, ConPTY does not pass a program's `ESC[?1049h` through: conhost switches its own
buffer and repaints. The model then sees the program's screen as a repaint of the primary screen,
which the snapshot reproduces exactly; when the program exits, ConPTY repaints the shell's
screen. Programs that do reach the terminal with 1049 (Linux later, newer ConPTY) are replayed
with their alternate screen.

## 3. Resize and readers of different sizes

- **One size per pane** (ADR 0008 §4): the most recent interactive attach with a size, or
  `pane.resize`, sets it. Read-only viewers never resize; their `size` is ignored.
- The Host resizes the **model first**, then the pseudo console, as a terminal does. Rows above
  the cursor that no longer fit move into the history; the screen is cut or padded without reflow;
  the scroll region becomes the whole screen. ConPTY then repaints the screen at the new size, and
  every attached client receives that repaint as live output.
- **History rows re-wrap** at the width of the client they are replayed to: a 100-column row
  from before a resize to 80 columns wraps into two rows in a new 80-column client and joins back
  with `capture --join` (measured, §8). On Windows 10, ConPTY ends long lines with explicit line
  breaks, so a soft wrap is not preserved across ConPTY's own repaint; the newer ConPTY of
  Windows Server 2025 (build 26100, the CI runner) sends soft wraps, which the model keeps as
  wrapped rows that join back.
- A snapshot is made for a client terminal of the pane's size. A read-only viewer whose window is
  smaller sees its own terminal wrap or cut the lines; a larger one sees them at the pane's size.
  The rows a snapshot's history occupies are computed for the pane's width, so a viewer of
  another width may lose or repeat a few history rows in its own scrollback (the screen itself is
  always painted fully).

## 4. Capture, history and clear-history

| CLI | IPC | tmux | What |
| --- | --- | --- | --- |
| `subak capture [<target>]` | `pane.capture` | `capture-pane -p` | the screen rows as text |
| `--start <n\|->`, `--end <n\|->`, `--history` | `start`, `end` | `-S`, `-E`, `-S -` | a range: screen rows from 0, history lines from -1 (the newest) down; `-` is the oldest history line (start) or the last screen row (end); `--history` = `--start -` |
| `--vt` | `format: "vt"` | `-e` | keep colours and attributes as SGR sequences |
| `--join` | `join: true` | `-J` | join wrapped rows into one line |
| `--no-redact` | `redact: false` | — | show secret-looking values (§6) |
| `subak history [<target>]` | `pane.capture` with an empty range | — | counters only: lines, bytes, limits, evicted, cleared, total; the screen size, alternate screen and cursor |
| `subak clear-history [<target>]` | `pane.clear_history` | `clear-history` | delete the kept history; the screen stays; destructive (asks, or `--yes`; `--dry-run` shows the plan) |
| `subak new --history-limit <n>` | `terminal.create` `history_limit` | `history-limit` | rows the pane keeps (0–100 000, default 2 000) |

- Without a target, these commands use the pane they run in (`SUBAK_PANE_ID`), else the Host's
  only Terminal (`ambiguous_target` with the candidates when there are several).
- A **stopped** Terminal (`--remain-on-exit`, `subak stop`) keeps its screen and history, so the
  output of a finished build can still be captured; a removed one has none (`target_not_found`).
- One `pane.capture` answer carries at most 192 KiB of lines (a control message is limited to
  256 KiB); older lines are left out and `truncated` is true. `subak capture` asks again for the
  older part until it has the whole range (`requests` says how many). Output arriving between
  those requests shifts history line numbers; capture a quiet pane, or page with `--end`.
- Panes report their `history` counters in `terminal.info` (`subak info`); `host.status`
  (`subak host status`) reports `scrollback: {panes, lines, bytes, memory_bytes,
  default_limit_lines, limit_bytes_per_pane, persisted: false}`; `host.stats` reports the same
  totals plus `evicted_lines_total`, `cleared_lines_total`, `snapshots_total`,
  `snapshot_bytes_total` and `captures_total`, and `ipc.lagging_resyncs` and
  `ipc.output_frames_skipped` (§5). All are numbers; none carries content (ADR 0012).
- **Access control** (IPC_PROTOCOL §4): only `cli` and `gui` clients may attach, capture or clear
  history. MCP clients get it only after the user grants it (Step 37); hook clients never.
  Measured: `hook` and `mcp` connections are refused all three with `unauthorized` (§8). Every
  connection is the Host's own user (the pipe's DACL and the token check, ADR 0019).

## 5. Load: slow clients, high volume, disconnect, idle

- **The pane never waits.** The reader drains ConPTY continuously and only queues output.
- **Slow consumers are resynced, not disconnected** (since Step 13). A connection's send queue is
  bounded by 16 MiB (ADR 0025). Output that does not fit makes that attachment *lag*: its queued
  output frames are dropped at once (control messages stay), no more output is queued for it,
  and its memory is freed. When the client has read enough that its queue is down to 4 MiB, the
  Host sends `pane.resync {pane_id, terminal_id, stream_id, seq}` (to a connection subscribed to
  `panes`) and a fresh snapshot (it starts with CAN, ending any sequence the client was in, and
  carries the history rows the dropped output added), then live output from `seq`. A control
  message that does not fit still disconnects the client (`ipc.lagging_clients_dropped`).
- **High volume.** Measured with 20 MiB typed through ConPTY (0.85 MiB/s, conhost is the limit):
  the fast client received everything; two clients that never read stayed connected, were skipped
  (~300 000 frames) and resynced; the Host's peak private memory grew by 18–26 MiB; `host.health`
  answered within 2 ms. The history stayed at its limit (2 000 rows, ~200 KB).
- **Disconnect and reconnect.** A client that disconnects or crashes owns nothing; the model keeps
  running. A reconnecting client gets a snapshot like any other (Step 10–12 evidence still holds).
- **Idle.** No timer or thread was added: the model runs on the reader thread when output
  arrives, a snapshot when someone attaches, a resync when a writer drains. Measured: a Host with
  two panes, two attached clients and their history used 0 ms of CPU over 10 s and kept its
  thread count; 10 attach and capture cycles left no thread behind (§8).

## 6. Privacy and security policy for terminal output

Terminal output is user data and may contain secrets (tokens echoed by tools, passwords typed
where echo was not disabled, keys printed by mistake). The rules:

1. **What is kept:** the screen(s) and at most `history_limit` rows / 4 MiB of history per pane
   (§1), and nothing else of the output.
2. **Where:** only in the memory of the user's own Host process. Nothing of it is written to
   disk, to a log, to the event stream or to any JSON document except an explicit capture. (Like
   any process memory it can reach the Windows page file or a crash dump the user configured;
   the Host does not lock its pages.)
3. **For how long:** until the row is evicted by newer output, the history is cleared
   (`subak clear-history`, or a program's `ESC[3J`), the Terminal is removed (`subak kill`,
   `subak rm`, its program exiting without `--remain-on-exit`), or the Host exits. A stopped
   Terminal keeps it until it is removed.
4. **Who can read it:** a client of the same user, of kind `cli` or `gui` (§4). Never another
   user, never a hook client, MCP clients only with the Step 37 grant.
5. **Captures redact by default.** `pane.capture` replaces secret-looking values with `***`:
   values of secret-looking `name=value` / `name: value` pairs (names containing token, secret,
   pass, pwd, key, auth, credential or cookie; after `:` only values of 4+ characters or quoted
   ones), the credential after `Bearer`/`Basic`/`Token`, well-known token shapes (GitHub, GitLab,
   Slack, `sk-…`, npm, AWS access key ids, Google API keys, JSON Web Tokens; their prefix stays),
   URL passwords, and the lines of a `-----BEGIN … PRIVATE KEY-----` block. `redacted` counts the
   hidden values, and the CLI says so on stderr. The rules are heuristic: a secret in a form they
   do not know, or one split over two rows, stays visible. `--no-redact` / `redact: false` shows
   the exact text and is the caller's explicit choice. The same detector redacts every command
   a client is shown (CLI_CONTRACT §10; since Step 16, after e2e run 2 found a `ghp_…` token
   shown verbatim in a pane's `command_line`).
6. **Not redacted:** what an attached client sees (attach is the terminal itself, as before
   Step 13), including the title the program set. Pane titles are therefore not reported in any
   JSON document (cmd.exe puts whole command lines into its title).
7. **Clearing** removes the Host's copy only. Clients that already showed the output keep it in
   their own scrollback; clear those yourself (e.g. `cls` and the terminal's own clear).
8. **Persistence** of history is **not implemented**. It is deferred to the persistence Steps
   (29+), where it will be opt-in and off by default (`SUBAK-FR-042`), stored with user-only
   permissions (`SUBAK-SEC-011`), crash-safe, bounded on disk, and shown after a restore as
   read-only history separated from new output. Until then, history is gone when the Host exits.

## 7. Known limits

- **Character widths** come from the model's own table (Hangul, CJK, fullwidth forms, emoji,
  combining marks). A character conhost measures differently (some emoji, ambiguous-width
  symbols) shifts the rest of its row in the model until ConPTY next positions the cursor.
- **ConPTY on Windows 10** passes neither the alternate screen switch, mouse modes nor OSC 8
  hyperlinks through (measured in Step 15); what it repaints is what is replayed. Soft wraps
  arrive as hard line breaks there (build 26100 sends them as soft wraps, and since Step 15 the
  snapshot reproduces them on the client).
- **History rows** are rows, not logical lines, so a long line counts as several rows against
  `history_limit` (as in tmux).
- **Viewers of another size** see the pane at the pane's size (§3); there is no per-client size.
- **Origin mode with a pending wrap** at the moment of a snapshot is reproduced without the
  pending wrap (the next character lands one column early once).
- **Large captures** are paged (192 KiB of lines per answer); concurrent output can shift lines
  between pages (§4).
- **Totals are per pane:** 256 Terminals × 4 MiB of history is the worst case (~1 GiB); there is
  no Host-wide cap (ADR 0030).
- **No search** in history and no GUI scrollbar yet (Steps 16–25); `capture` piped to `findstr`
  works today.

## 8. Evidence

Automated, on real Windows 10 Pro 19045 with real `subak.exe` Hosts on private endpoints
(`crates/subak/tests/scrollback_replay.rs`; the model's unit tests in `crates/subak-term/src/screen.rs`
and, since Step 15, its conformance fixtures in `crates/subak-term/tests/conformance.rs`
and the redaction rules in `crates/subak/src/redact.rs`; the e2e scenario
`e2e/tests/step13_scrollback_replay.rs`). Every viewer is a raw IPC client feeding a client-side
copy of the model, checking each frame's `seq`.

| Test | What it shows |
| --- | --- |
| `reattach_after_shell_output_and_scrolling_reproduces_history_and_screen` | 190 lines at 80×24: a late client's history, cells, attributes and cursor equal those of a client attached from the start and the Host's capture; again after detaching, more output, and reattaching |
| `reattach_during_and_after_a_full_screen_program` | an alternate-screen test program (colours, a scroll region, absolute moves, hidden cursor, bracketed paste): a late client sees it exactly; after it exits the shell screen returns on all clients |
| `reattach_while_more_pages_a_file` | `more` paging a file: late clients see the same page and prompt, before and after paging |
| `concurrent_output_during_attach_is_never_lost_duplicated_or_reordered` | 8 clients attached at different `seq` within 0.8 s while 10 000 lines were printed: each reconstructed `n-1`…`n-10000` exactly once and in order and equals the Host's capture; live frames continued each snapshot's `seq` |
| `resize_and_readers_with_different_sizes_follow_the_pane_size` | a viewer asking 60×20 got the pane's 100×30; a resize to 80×24 reached cmd (`mode con`), all clients matched the Host after ConPTY's repaint; 100-column history rows re-wrapped at 80 columns and joined back |
| `history_is_bounded_evicts_the_oldest_lines_and_clears_on_request` | a 50-row limit kept exactly the newest 50 rows (consecutive, ending right above the screen), `evicted = total − 50`; `clear-history` needed `--yes`, removed 50 rows, kept the screen; a 0 limit kept none; 100 001 is a usage error |
| `a_stalled_client_during_sustained_output_is_resynced_to_the_same_screen` | 20 MiB through ConPTY: a client that stopped reading stayed connected (~178 000 frames skipped), got `pane.resync` and a snapshot, and then matched the Host's screen; Host private bytes peak +18 MiB; history held at 2 000 rows / 204 000 bytes |
| `an_idle_host_with_screens_and_clients_does_no_work` | 2 panes, 2 attached clients: Host CPU 0 ms over 10 s, threads 13 → 13; 10 attach and capture cycles left 13 threads |
| `history_access_is_restricted_and_secrets_stay_out_of_json` | a GitHub token and a password printed in a pane: `capture` (JSON and human) showed `***` (4 values hidden), `--no-redact` showed them; `history`, `info`, `ls`, `status`, `host status` and `host.stats` carried no content and no title; `hook` and `mcp` clients were refused `pane.capture`, `pane.clear_history` and `pane.attach` |
| `pty_shells.rs::large_output_streams_to_fast_clients_while_stalled_ones_are_resynced` | (Step 11's test, updated) two stalled clients are kept and resynced instead of dropped; live output continues the resync snapshot's `seq` without a gap |
| `subak_cli.rs::every_command_answers_one_schema_valid_document_with_stable_exit_codes` | golden `capture`, `history` and `clear-history --dry-run` documents; a capture of a stopped Terminal with a redacted token |
| `screen.rs` unit tests | for each feature in §1: a snapshot fed into a fresh model reproduces both screens, cursor, modes, region, title, tabs, charsets and history; snapshots cut in the middle of a sequence or UTF-8 character continue exactly; 200 000 fuzzed bytes with resizes never panic and still replay; feeding typical coloured output runs at 68 MiB/s in a release build (11 MiB/s in a debug build; ConPTY delivers 0.9 MiB/s), 2 000 rows of history at 120×30 hold ~245 KB |

**Not tested** (needs a desktop session, so not headless): Windows Terminal or a conhost window
as the attaching terminal, checked by eye. Steps: start `subak new work`, run
`for /L %i in (1,1,200) do @echo line-%i` in `subak attach work`, press `Ctrl+B d`, run
`subak attach work` again in another window. **Expected:** the same screen, the earlier lines in
that window's scrollback once, live output continuing; then run a full-screen program (for
example `edit` from PowerShell 7 if installed, or `more` on a long file), detach and reattach:
the program's screen reappears.
