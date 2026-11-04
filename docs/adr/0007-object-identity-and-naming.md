# ADR 0007 — Object identity, naming and targets

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Implemented in Steps 10 and 21, with tmux target syntax in Step 24

## Context

Terminals, Windows and Panes need identities that are stable across clients, Host restarts and
restores, are never reused (`SUBAK-FR-010`), and are distinct from human names. Scripts and
agents need unambiguous targets. tmux users expect `session:window.pane`.

## Decision

### IDs

| Object | Format | Example |
| --- | --- | --- |
| Terminal | `t-` + 12 lowercase hex chars | `t-3fa9c0d21b7e` |
| Window | `w-` + 12 hex | `w-08c1d2e3f4a5` |
| Pane | `p-` + 12 hex | `p-9b0e7d6c5a41` |

- IDs are random 48-bit values from the OS CSPRNG. Before use they are checked against every
  live and persisted ID. They are never reassigned, even after deletion.
- IDs survive Host restarts and restores: a restored Terminal keeps its ID and is marked
  `restored`.
- PIDs are volatile hints and never identities.

### Names

- **Terminal names** are unique per Host. Comparison is case-insensitive (the Windows
  convention) and Unicode-NFC-normalized; the original case is kept for display.
  - Allowed: 1–64 characters after trimming, no control characters, no `:` or `.` (target
    delimiters), must not start with `-`, and must not look like an ID (`t-`/`w-`/`p-` + 12 hex).
  - Default: the lowest free positive integer (`1`, `2`, …), like tmux sessions.
  - Creating or renaming to an existing name fails with `name_conflict`. Nothing is silently
    suffixed, because scripts must see the conflict.
- **Window names** need not be unique (tmux semantics). By default a window is named after the
  pane's foreground program (automatic rename, Step 21) until the user renames it.
- **Window and pane indexes**: windows are numbered from 0 within a Terminal and panes from 0
  within a Window. Indexes may change; IDs never do.

### Targets

`<terminal>[:<window>[.<pane>]]`:

- `<terminal>` is a Terminal name or ID.
- `<window>` is an index, a name, or an ID.
- `<pane>` is an index or an ID.
- An ID alone (`p-9b0e…`) is a complete target.
- The headless CLI accepts **exact** names and IDs only. Ambiguity returns `ambiguous_target`
  with the candidates, and an unknown target returns `target_not_found`.
- The tmux-compatible command prompt (Step 24) additionally accepts tmux forms (`$n`, `@n`,
  `%n`, `{last}`, prefix matches), mapped onto the same objects.

## Consequences

- Agents should use IDs; humans can use names.
- Case-insensitive uniqueness prevents `Work` and `work` from being two Terminals, which avoids
  confusion on Windows.

## Alternatives considered

- **tmux-style small integers as IDs** (`$1`, `@2`, `%3`): these are reused across server
  restarts, which breaks durable references and restore.
- **UUIDs:** too long for humans typing CLI targets. 48 bits of randomness is enough for
  per-user object counts, and collisions are checked anyway.

## Requirements

`SUBAK-FR-010`, `SUBAK-FR-011`, `SUBAK-FR-012`, `SUBAK-FR-013`, `SUBAK-FR-060`.

## Notes

- 2026-11-04 (Step 10): implemented for Terminals ([SESSION_MODEL §3](../SESSION_MODEL.md),
  [ADR 0020](0020-terminal-session-lifecycle.md)). Precisions: the comparison key is NFC, then the
  locale-independent Unicode lowercase mapping, then NFC again; invisible formatting characters
  (bidirectional overrides, zero-width characters, BOM, line and paragraph separators, soft
  hyphen) are refused like control characters; the ID check rejects ID-shaped names in any case.
  A Host never reissues an id it issued; across Host restarts uniqueness rests on the 48 random
  bits until Step 29 persists the issued ids. Ids come from `std`'s OS-randomly keyed SipHash, not
  from a direct CSPRNG call. "Exact" target names compare by that key (case and composition
  ignored), never by prefix. `terminal.*` operations take only Terminal targets.
