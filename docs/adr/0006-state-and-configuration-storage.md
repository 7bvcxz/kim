# ADR 0006 — State and configuration storage

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Implemented in Steps 10, 20, 29

## Context

Subak must persist structure for restore (`SUBAK-FR-040`), keep configuration durable and
forgiving (`SUBAK-FR-046`), and never lose a good state during a migration or restore
(`SUBAK-SEC-003`). Data must stay local and per user, and secrets must not be persisted
(`SUBAK-SEC-011`, `KIM-SEC-013`).

## Decision

### Locations (Windows)

| Data | Path | Format |
| --- | --- | --- |
| User configuration | `%APPDATA%\mouv\subak\config.toml` | TOML, hand-editable |
| Session state | `%LOCALAPPDATA%\mouv\subak\state\state.json` | JSON, versioned schema |
| State backups | `%LOCALAPPDATA%\mouv\subak\state\backups\state-<timestamp>.json` (keep last 5) | JSON |
| Optional scrollback (off by default) | `%LOCALAPPDATA%\mouv\subak\history\<pane id>.log` | bounded, append-only chunks |
| Logs | `%LOCALAPPDATA%\mouv\subak\logs\` (rotating, 10 MB cap) | text, no secrets/content |
| Kim manager | `%LOCALAPPDATA%\mouv\kim\` (catalog cache, install records) | JSON |

Directories are created with an ACL that grants only the user (and SYSTEM/Administrators, which
inherit by Windows default and cannot be meaningfully excluded). The Linux mapping (M4) is
`$XDG_CONFIG_HOME`, `$XDG_STATE_HOME`, mode 0700.

### Writing rules

1. **Atomic replace**: write `state.json.tmp` → `FlushFileBuffers` → `MoveFileExW(...,
   MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`. A crash leaves either the old file or
   the new one, never a torn one.
2. **Coalesced writes**: at most one write per 500 ms after a change, plus a write on graceful
   stop. No writes while idle.
3. **Integrity**: the document carries `schema`, `schema_version`, `written_at`, and a CRC-32
   of the canonical body. If the CRC or parse fails, Subak loads the newest valid backup, keeps
   the corrupt file as `state.corrupt-<ts>.json`, and reports it.
4. **Migrations**: a chain `vN → vN+1` of pure functions. The current file is backed up before
   any migration. A file from a *newer* schema is never rewritten. Subak opens it read-only
   and reports `state_from_newer_version`.
5. **Configuration**: unknown keys are preserved on rewrite (format-preserving TOML edits). An
   invalid value falls back to its default, with a precise error message naming key and line.
   Subak never crashes on configuration.
6. **Data minimization**: state stores names, IDs, topology, layouts, cwd, profile reference,
   the explicitly configured launch command, non-secret environment overrides, restore policy
   and Claude session references. It never stores PIDs as truth (only as volatile hints),
   terminal content (unless scrollback persistence is enabled), or secret values. Variables
   marked secret are stored as references to Windows Credential Manager entries (Step 34).

## Consequences

- State is inspectable and repairable by humans.
- JSON is fine for the expected sizes (hundreds of panes). If profiling shows otherwise, an ADR
  may move to a binary or embedded store.
- Schemas: [contracts/STATE_SCHEMA.md](../contracts/STATE_SCHEMA.md).

## Alternatives considered

- **SQLite:** robust, but a C dependency, opaque to users, and overkill for one small document.
- **Registry:** not portable and not human-friendly.
- **One file per terminal:** more files to keep consistent atomically, and cross-terminal
  invariants (unique names) get harder.

## Requirements

`SUBAK-FR-040`, `SUBAK-FR-046`, `SUBAK-SEC-003`, `SUBAK-SEC-011`, `KIM-SEC-013`,
`KIM-NFR-003`.

## Notes

- 2026-11-04 (Step 20): `config.toml` is implemented for the window's settings and the basic
  shell profiles ([ADR 0051](0051-subak-settings-profiles-and-appearance.md),
  [STATE_SCHEMA §2](../contracts/STATE_SCHEMA.md)). Rule 5's "format-preserving TOML edits" are
  line edits checked by parsing the result again (only the edited keys may differ, else nothing
  is written), as Noeul does; an invalid file is never rewritten by an edit, and only `reset
  --all` or `import` replace it after a backup in `backups\` (the newest 5). The Host does not
  read the file; the `[host]`, `[restore]`, `[scrollback]` and `[claude]` sections of the draft
  remain planned for their Steps and are kept as unknown keys until then.
