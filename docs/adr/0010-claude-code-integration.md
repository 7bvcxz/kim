# ADR 0010 — Claude Code status integration

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Refined and implemented in Steps 27, 28 and 30

## Context

Subak shows a per-pane Claude Code state (`SUBAK-FR-050…053`) and can resume Claude
conversations after a reboot (`SUBAK-FR-045`). Parsing terminal text is fragile and invades
privacy. Claude Code offers official **hooks**: user-configured commands that run on events
and receive JSON on stdin with fields such as `session_id`, `hook_event_name` and `cwd`. It
also offers `claude --resume <session-id>`.

## Decision

1. **Pane correlation by environment.** The Host starts every pane with `SUBAK_PANE_ID=p-…` and
   `SUBAK_TERMINAL_ID=t-…` in its environment. A Claude Code process started in that pane
   inherits them, and so do its hook commands.
2. **Opt-in hook installation** (`subak claude hooks install`, Step 27).
   - Subak adds hook entries that run `subak claude report`.
   - It edits the user's Claude settings by **merging**: existing hooks and settings are
     preserved, a backup is written first, and `subak claude hooks uninstall` removes exactly
     what Subak added.
   - Nothing happens without the explicit command.
3. **Reporter.** `subak claude report` reads the hook JSON from stdin. It forwards only
   `{pane_id, session_id, event, timestamp}` and, where needed, a coarse reason (for example
   `permission`), over IPC as a client of kind `hook`. It never forwards prompts, tool inputs
   or outputs, transcript paths or file contents. It exits quickly. If the Host is not
   running, it does nothing and exits 0 so it never disturbs Claude.
4. **State machine** (Step 28) maps events to states:
   - a prompt was submitted → `in-progress`;
   - a permission request or notification is waiting for the user → `need-input`;
   - the turn stopped → `done` (or `idle` once the user has seen it);
   - a failure → `error`;
   - the session ended → `none`.

   When evidence is missing or stale (no event within a staleness window while a `claude`
   process exists in the pane's job), the state is `unknown`. A completed tool call is never
   treated as `done`.
5. **Resume.** The last known `session_id` per pane is persisted, and nothing else. On restore,
   if the user enabled Claude resume, the pane starts `claude --resume <id>` in the saved cwd,
   once (with duplicate protection, Step 30). Tool actions are never auto-approved.
6. **Without hooks**, Subak can only say `unknown` (a `claude` process is present) or `none`.
   It never guesses states from screen text.

## Consequences

- Accurate states depend on the user opting in to hooks; this is honest and explicit.
- If the hook event names or fields change in a Claude Code version, Step 27 records the
  supported versions and degrades to `unknown`.

## Alternatives considered

- **Screen scraping for spinners or prompts:** fragile, privacy-hostile, and wrong with other
  locales or themes.
- **Reading Claude's transcript files:** contains conversation content. Rejected for privacy.

## Requirements

`SUBAK-FR-045`, `SUBAK-FR-050`, `SUBAK-FR-051`, `SUBAK-FR-052`, `SUBAK-FR-053`,
`SUBAK-SEC-002`, `KIM-SEC-013`.
