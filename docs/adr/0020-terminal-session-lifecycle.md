# ADR 0020 — Terminal session lifecycle and the meaning of every end

Status: **Accepted** · Date: 2026-11-04 · Step 10 · Implements [ADR 0007](0007-object-identity-and-naming.md)
for Terminals, refines [ADR 0003](0003-subak-host-process-model.md) §7,
[ADR 0009](0009-gui-clients-and-keyboard-dispatch.md) and [ARCHITECTURE §4.2, §5](../ARCHITECTURE.md)

## Context

Until Step 09 a Host owned at most one prototype pane. Step 10 gives it a session model: any number
of named Terminals, each with a Window and a Pane, created, queried, renamed, stopped and removed
by several clients at once. The earlier documents agreed that detaching never ends processes, but
they left gaps and two different meanings of "force" (consent to end busy work, and skipping the
grace period), named a CLI (`subak stop|kill|rm`) before the CLI contract's noun-verb rule existed,
and did not say what a GUI tab's close button does. Running work must never disappear through an
ambiguous action (`SUBAK-SEC-001` in spirit, `SUBAK-FR-014`, `SUBAK-FR-015`).

## Decision

1. **One state machine** (docs/SESSION_MODEL.md §2): `starting → running → stopping →
   stopped | removed`, and `starting → failed | removed`. It is one table in code
   (`session::next`); every change happens under the Host's lock together with its event, so
   concurrent clients never see a state the table does not allow. ARCHITECTURE's `restorable` and
   `unknown` stay reserved for Steps 29–30.
2. **Distinct, fixed meanings; no `close` verb.** *Detach* (and any client disconnect or crash)
   touches nothing. *Stop* ends the processes gracefully (`ClosePseudoConsole`, up to 5 s), then
   terminates what is left in the job, and keeps the Terminal as `stopped`. *Kill* terminates the
   job at once and removes the Terminal (tmux `kill-session`). *Remove* (`rm`) deletes a Terminal
   that runs nothing and refuses a running one (`invalid_state`) unless `force` is given, which
   makes it a kill. A program that exits by itself removes its Terminal unless it was created with
   `remain_on_exit` (then `stopped`).
3. **`force` means consent to end busy work, nothing else.** The Host refuses to stop, kill or
   force-remove a running Terminal that is *busy* (its root is not a known shell, or its job holds
   more than one process) unless the request carries `force`; it answers `busy` with the
   processes. An idle shell may be ended without it, which keeps ADR 0009's "`Ctrl+W` confirms
   only when busy" correct even if a GUI checks too early. Whether the grace period is skipped is
   decided by the verb (stop: graceful; kill: immediate), not by a flag.
4. **The CLI always asks** for `terminal stop`, `kill` and `rm` (or needs `--yes` when it cannot
   ask), also for an idle shell, because headless callers must never end work by accident
   (CLI_CONTRACT §4). Only after consent does it send `force`, and it then targets the Terminal by
   its id, never by a name that could meanwhile belong to another Terminal.
5. **`host stop` with live Terminals needs `--kill-terminals`** (ADR 0003 §7, now implemented):
   without it the CLI fails with `terminals_running` (exit 7) and lists them; with it, it asks or
   needs `--yes`, and sends `kill_terminals`. `--yes` alone no longer stops a Host that owns live
   Terminals.
6. **CLI naming.** The commands are `subak terminal new|list|info|rename|attach|stop|kill|rm`,
   nouns before verbs (CLI_CONTRACT §2). `new`, `info` and `rm` are not standard verbs; they are
   the tmux words users expect and map one to one onto `terminal.create`, `terminal.info` and
   `terminal.remove`. Step 12 may add short top-level forms (`subak new`, `subak ls`) as aliases of
   these commands, with the same semantics.
7. **GUI tabs.** Closing a tab with its × button or "Hide tab" is a *detach* of that tab: the
   Terminal keeps running and stays listed in the session manager. Ending a Terminal from the GUI is
   only the named action "Close Terminal" (`Ctrl+W`, `SUBAK-FR-033`), which is a kill and confirms
   when the Terminal is busy. ADR 0009 named `Ctrl+W` and "Hide tab" but not the × button; this
   closes that gap on the safe side.
8. **Names** follow ADR 0007 with two precisions: comparison keys are NFC, then the
   locale-independent Unicode lowercase mapping, then NFC again; and invisible formatting
   characters (bidirectional overrides, zero-width characters, BOM, line and paragraph separators,
   soft hyphen) are refused next to control characters, so two names cannot look identical.
   Normalization uses the `unicode-normalization` crate (MIT/Apache-2.0), the first new runtime
   dependency of `subak` since `serde_json`.
9. **Ids are never reissued by a Host**: it keeps every id it issued. Across Host restarts this
   rests on 48 random bits until Step 29 persists the issued ids. The ids come from the
   OS-randomly keyed SipHash of `std` (as in Step 09), not from a direct CSPRNG call.
10. **No persistence yet.** Until Step 29, Terminals end with their Host and stored (`stopped`,
    `failed`) records are forgotten when it exits; a Host with only stored Terminals idles out,
    because only live Terminals keep it (ADR 0003 §4). This is documented, not hidden.
11. **IPC v1 grows additively** (IPC_PROTOCOL §7): `terminal.list|info|create|rename|stop|kill|remove`
    are implemented as listed, with additive parameters (`remain_on_exit`, `keep_on_failure`,
    `dry_run`, `force` on `terminal.remove`, `stream_id` on `pane.resize`) and result fields;
    `pane.attach` accepts any target; new events are typed in the schema. The feature
    `prototype.single_pane` is replaced by `terminal.lifecycle`, because the Host no longer owns a
    single pane. `profile` and `env` are refused with `invalid_params` until Steps 33 and 11.

## Consequences

- Scripts and agents can rely on one meaning per verb and on errors instead of surprises:
  `name_conflict`, `invalid_name`, `invalid_state`, `ambiguous_target` and `terminals_running` are
  new CLI codes (CLI_CONTRACT §6).
- Existing scripts that ran `subak host stop --yes` against a Host with a live shell now get
  `terminals_running` and must add `--kill-terminals`. This is intended: the old form ended work
  that a later step's user might not expect it to end.
- The busy rule is a heuristic over the job (known shell + one process). It errs towards asking.
- Measured evidence and the test list are in docs/SESSION_MODEL.md §7.

## Alternatives considered

- **A single `close` command** that detaches or kills depending on state: rejected; it is exactly
  the ambiguity this step removes.
- **`force` as "skip the grace period"**: rejected; consent and speed are separate questions, and
  the verb already says which teardown is meant.
- **Silently suffixing a colliding name** (`work (2)`): rejected by ADR 0007, because scripts must
  see the conflict.
- **Requiring consent from the Host for every end, idle or not**: rejected for the GUI's `Ctrl+W`
  on an idle shell (`SUBAK-FR-033`); the CLI asks anyway.
- **Keeping the Host alive for stored Terminals**: rejected until they are persisted; a resident
  Host that owns no process contradicts ADR 0003 §4.

## Requirements

`SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-010`, `SUBAK-FR-011`, `SUBAK-FR-012`, `SUBAK-FR-013`,
`SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-016`, `SUBAK-FR-033`, `SUBAK-SEC-001`, `KIM-NFR-014`.

## Notes

- 2026-11-04 (Step 12): the CLI verbs of this decision are top level since
  [ADR 0027](0027-subak-cli-structure-and-headless-control.md): `subak new|ls|info|rename|attach|stop|kill|rm`;
  `subak terminal <verb>` remains as a deprecated alias with the same meaning. `subak detach`
  (IPC `client.detach`) adds detaching other clients; like every detach it ends nothing.
