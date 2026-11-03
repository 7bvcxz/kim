# Subak state and configuration schemas v1 (draft)

Status: **draft contract** · Step 03 · Implemented in Steps 10/29 (state) and 20 (config) ·
Decision: [ADR 0006](../adr/0006-state-and-configuration-storage.md)

## 1. `state.json` (schema `subak.state`, version 1)

Machine-readable: [schemas/subak-state-v1.schema.json](schemas/subak-state-v1.schema.json);
examples in [examples/subak-state-v1/](examples/subak-state-v1/two-terminals.json), validated
by `cargo xtask schema-check`.

```json
{
  "schema": "subak.state",
  "schema_version": 1,
  "written_at": "2026-11-03T12:00:00Z",
  "writer": { "version": "0.1.0" },
  "crc32": "1c291ca3",
  "terminals": [ Terminal ]
}
```

| Object | Fields |
| --- | --- |
| Terminal | `id` (`t-` + 12 hex), `name`, `created_at`, `profile` (string or null), `restore` (`ask` \| `auto` \| `never`), `remain_on_exit` (bool), `active_window` (window id), `windows` (≥ 1) |
| Window | `id`, `index` (≥ 0), `name`, `automatic_name` (bool), `active_pane`, `layout` (Layout), `panes` (≥ 1) |
| Layout | `{ "type": "pane", "pane": "p-…" }` or `{ "type": "split", "direction": "horizontal" \| "vertical", "children": [{ "size": 0–1, "node": Layout }] (≥ 2) }` |
| Pane | `id`, `index`, `cwd`, `command` (`{program, args}` or null = profile default), `restart_command` (same shape or null; **opt-in**, never derived), `env` (non-secret overrides), `env_secret_refs` (name → credential reference), `title` (or null), `claude` (`{session_id, resume}` or null), `scrollback` (`{persisted, file?}`) |

**Never stored:** PIDs (except as volatile hints outside this file), terminal content (unless
scrollback persistence is enabled, and then in separate files), secret values, client
identities.

### Integrity and migration

- `crc32` is the CRC-32 (IEEE) of the document serialized canonically **without** the `crc32`
  field (sorted keys, no insignificant whitespace), as 8 lowercase hex digits.
- Load: parse → check `schema` → check `crc32` → migrate `schema_version` step by step (backup
  first) → validate. A newer `schema_version` than the reader knows → read-only,
  `state_from_newer_version`.

## 2. `config.toml` (version 1)

Human-edited; unknown keys preserved; invalid values fall back to defaults with an error that
names the key and line (`SUBAK-FR-046`). Defaults shown:

```toml
config_version = 1

[host]
start_at_login = false        # opt-in (SUBAK-FR-002)
idle_exit_seconds = 10

[restore]
mode = "ask"                  # ask | auto | never — auto applies only to Terminals with restore = "auto"
claude_resume = false         # opt-in (SUBAK-FR-045)

[scrollback]
lines = 10000
history_bytes_per_pane = 4194304
persist = false               # opt-in (SUBAK-FR-042)

[gui]
floating_icon = false
tray_icon = true
close_terminal_shortcut = "kill"   # kill | hide (ADR 0009)

[keys]
prefix = "Ctrl+B"
new_terminal = "Ctrl+T"
close_terminal = "Ctrl+W"
next_terminal = "Ctrl+Tab"
previous_terminal = "Ctrl+Shift+Tab"
terminal_first_mode = false

[claude]
indicator = true
```

Profiles (`[[profiles]]`) arrive in Step 20/33 and are specified there.
