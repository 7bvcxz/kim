# ADR 0054 — Subak internal windows and panes: Host-owned topology and selection, two-level end claims, no window or pane command that ends a Terminal

Status: **Accepted** · Date: 2026-11-04 · Step 21 · Implements the Window and Pane levels of
[ADR 0007](0007-object-identity-and-naming.md) and the IPC v1 methods reserved for Step 21 in
[IPC_PROTOCOL §4](../contracts/IPC_PROTOCOL.md) · Builds on
[ADR 0004](0004-pty-and-process-ownership.md) (a pseudo console and a kill-on-close job per pane),
[ADR 0020](0020-terminal-session-lifecycle.md) (the Terminal state machine, the busy guard, one
teardown), [ADR 0027](0027-subak-cli-structure-and-headless-control.md) (the CLI) and
[ADR 0042](0042-subak-window-tabs-and-safe-close.md) (a tab is a Terminal) · Details:
[SESSION_MODEL.md](../SESSION_MODEL.md), [SUBAK_CLI.md §3.14](../SUBAK_CLI.md),
[TMUX_COMPATIBILITY.md](../TMUX_COMPATIBILITY.md)

## Context

Until Step 21 every Terminal had exactly one internal window with one pane, although the model,
the ids (`w-`, `p-`) and the targets (`<terminal>[:<window>[.<pane>]]`) already had the three
levels. Step 21 asks for real windows and panes under each Terminal, each pane with its own
process through the Host, durable ids and lifecycle events; tmux-like naming, numbering and
targets kept apart from Subak's Terminal tabs; create/list/select/rename/close for windows and
create/list/focus/close for panes over versioned IPC and the CLI; consistent selection for several
clients; topology kept without surprise process termination; concurrency and cleanup safeguards;
and a minimal, honest GUI. Splits and layouts are Step 22, the prefix keys Step 23, persistence to
disk Step 29.

## Decision

1. **The Host owns the topology and the selection.** A Terminal holds windows; a window holds
   panes; each pane keeps its own pseudo console, kill-on-close job, terminal model, bounded
   history and data stream (nothing is shared between panes). The active window of a Terminal and
   the active pane of each window are Host state, the same for every client (tmux: per session).
   A Terminal-level or window-level target means the active pane, so existing clients keep
   working unchanged.
2. **Numbering as tmux does it.** A new window takes the lowest free index from 0 and keeps it;
   windows are listed by index. Pane indexes are contiguous within their window and renumbered
   when a pane goes. Ids never change and are never reused. Window names need not be unique; they
   follow the Terminal name rules plus "not only digits" (a target reads digits as an index); an
   ambiguous name is `ambiguous_target`. A window is named after its first program until renamed.
3. **Methods and CLI.** `window.create`, `window.list`, `window.select`, `window.rename`,
   `window.kill`, `pane.create`, `pane.list`, `pane.select`, `pane.kill` (IPC v1, additive, feature
   `terminal.windows`); `subak window new|ls|select|rename|kill` and `subak pane new|ls|select|kill`
   with tmux's long names as aliases. Each takes exactly one level: a window command never takes a
   Terminal or pane, a pane command never a Terminal or window (`invalid_params`, CLI `usage`), and
   the Terminal commands still take only Terminals. A window or pane id under another Terminal is
   `target_not_found`. `mcp` clients may list (like `terminal.list`), nothing else until Step 37.
   Every state change has `dry_run`; kills ask (or need `--yes`) and act on the confirmed id.
4. **`pane.create`, not `pane.split`.** Without Step 22's layout a new pane has no geometry, so
   it is created by its own method; `pane.split` stays reserved for Step 22, which adds the split
   geometry (and may create through `pane.create`). Until then a window's `layout` is one pane, its
   active one: a window shows one pane at a time, as a zoomed tmux window does.
5. **Two-level end claims, and no window or pane command ends a Terminal.** A Terminal's end (stop,
   kill, Host stop, or the exit of its last running pane) is claimed by setting its `ending`; the
   claimer claims every pane, tears them down in parallel and applies the Terminal state machine
   once. A pane-level end (`pane.kill`, `window.kill`, or a pane whose program exits while others
   run) claims only its panes, under the same lock that checked that another unclaimed pane still
   runs, and takes them out of the model (a window goes with its last pane; the selection moves to
   a neighbour). Hence `window.kill`/`pane.kill` of the last running window or pane is
   `invalid_state` (`details.reason` `last_window`/`last_pane`): ending it would end the Terminal,
   which only `terminal.kill` does; of two clients killing a Terminal's last two panes at once,
   exactly one succeeds. The busy guard applies per pane (`busy` with the plan unless `force`).
6. **Creation is invisible until it runs.** Ids are reserved under the lock, the process starts
   outside it, and the window or pane enters the model (with its events) only once it runs and if
   its Terminal still runs and nothing ends it; otherwise the new process is ended at once
   (`invalid_state` or `host_stopping`). A failed start leaves no window, pane or pseudo console.
   Limits: 64 panes per Terminal, 512 running panes per Host (`busy`).
7. **Events** (topic `terminals`): `window.created`, `window.renamed`, `window.removed`,
   `window.selected`, `pane.created`, `pane.removed`, `pane.selected`, each in the critical
   section of its change; `pane.exited` (topic `panes`) as before. When a whole Terminal is
   removed, `terminal.removed` alone reports it.
8. **The GUI stays a Terminal-per-tab client.** It shows the active window's active pane, names it
   in the title when there are several, and follows `window.selected`/`pane.selected` (and a pane
   exit that moved the selection) by detaching the old pane and attaching the new one. It has no
   keys to switch windows or panes yet (Step 23's prefix table) and draws no split (Step 22).
9. **Topology is kept by the Host, not on disk yet.** Clients that detach, disconnect or crash
   change nothing; a reconnecting client sees the same windows, panes and selection. Writing it to
   `state.json` is Step 29; until then it lives as long as the Host, like the Terminals.

## Alternatives considered

- **Per-client selection** (each client its own active window): simpler for independent viewers,
  but tmux users expect one current window per session, and the Host's `pane.attach` already
  resolves a Terminal target to "the" active pane. Rejected; a client can always attach to a pane
  by id.
- **Let `window.kill` of the last window end the Terminal** (tmux does). Rejected: Subak's rule
  "no command ends a whole Terminal through a pane reference" (ADR 0007, SESSION_MODEL §3) and the
  GUI's guarded `Ctrl+W` stay the only ways to end a Terminal.
- **Implement `pane.split` now without geometry.** Rejected as dishonest: a "split" that is not
  shown split would be a placeholder.
- **Remove an exited pane only when the user says so** (`remain-on-exit` per pane). Deferred:
  tmux removes it by default; the per-pane option and dead panes are planned with `respawn-pane`.

## Consequences

- Clients that only know Terminals keep working; new events and methods are additive (IPC
  compatibility rules 1–4). The schema types every new event and result.
- A Terminal can now hold up to 64 processes trees; `stop`, `kill`, `rm --force` and `host stop`
  end every pane, and their `affected` lists one report per pane.
- Step 22 adds the layout tree and `pane.split` on top of this model; Step 23 binds the prefix keys
  to these methods; Step 29 persists the topology.
