# Subak IPC protocol v1 (draft)

Status: **draft contract** · Step 03 · Implemented and frozen in Step 09 · Decision:
[ADR 0005](../adr/0005-local-ipc.md)

Machine-readable schema: [schemas/ipc-v1.schema.json](schemas/ipc-v1.schema.json); valid and
invalid example messages live in [examples/ipc-v1/](examples/ipc-v1/hello.json) and are checked
by `cargo xtask schema-check` (part of `cargo test`).

The Step 08 prototype (`subak host …`, [CONPTY_PROOF](../CONPTY_PROOF.md),
[ADR 0016](../adr/0016-conpty-host-validation.md)) already uses the §1 framing with protocol
major **0** and a subset of methods (`host.status`, `host.stop`, `pane.attach`, `pane.resize`,
`pane.detach`). It is not v1 and has no identity checks. Measured in that prototype: pipe handles
must be overlapped on both ends.

## 1. Transport and framing

Per-user named pipe `\\.\pipe\mouv-subak-<SID>-s<session id>` (ADR 0005). Each frame:

```
u32 LE length | u8 kind | payload (length - 1 bytes)
```

| kind | name | payload | limit |
| --- | --- | --- | --- |
| `0x01` | control | UTF-8 JSON object (one message) | 256 KiB |
| `0x02` | data | `u32 LE stream_id` · `u64 LE seq` · `u8 flags` · bytes | 1 MiB |

Data `flags`: bit 0 = snapshot (bytes reproduce the screen at `seq`), bit 1 = input
(client → Host). Other bits must be zero. Violations → `protocol_error` and disconnect.

## 2. Handshake

1. Client → `{"type":"hello","protocol":{"min":1,"max":1},"client":{"name":"subak-cli","version":"0.1.0","kind":"cli"}}`
2. Host → `{"type":"welcome","protocol":1,"host":{"version":"0.1.0","pid":1234,"started_at":"…"},"features":[…]}`
   or `{"type":"error","error":{"code":"protocol_unsupported","message":"…","details":{"supported":{"min":1,"max":1}}}}` and close.

`client.kind` ∈ `cli`, `gui`, `hook`, `mcp`. No other message is accepted before `welcome`;
the Host closes connections that do not complete the handshake within 5 s.

## 3. Requests, responses, events

```
request   {"type":"request","id":<u64>,"method":"<name>","params":{…}}
response  {"type":"response","id":<u64>,"ok":true,"result":{…}}
          {"type":"response","id":<u64>,"ok":false,"error":{"code":"…","message":"…","details":{…}}}
event     {"type":"event","seq":<u64>,"event":"<name>","data":{…}}
```

- `id` is chosen by the client, unique per connection; responses arrive in request order.
- `message` is human-readable English and may change; **`code` is the stable contract**.
- Events are delivered only after `events.subscribe` and carry a per-connection `seq`.

## 4. Methods (v1)

| Method | Params | Result | Kinds | Step |
| --- | --- | --- | --- | --- |
| `host.status` | — | `{version, pid, started_at, protocol, terminals, clients, job_breakaway, idle_exit_s}` | all | 09 |
| `host.stats` | — | metrics per [ADR 0012](../adr/0012-local-metrics-no-telemetry.md) | cli, gui, mcp | 09 |
| `host.stop` | `{kill_terminals, dry_run}` | `{would_kill: [ids]}` or `{stopping: true}` | cli, gui | 09 |
| `events.subscribe` | `{topics: ["terminals","panes","claude","host"]}` | `{}` | cli, gui, mcp | 09 |
| `terminal.list` | `{detail: "summary" \| "full"}` | `{terminals: [Terminal]}` | cli, gui, mcp | 10 |
| `terminal.info` | `{target}` | `{terminal: Terminal}` | cli, gui, mcp | 10 |
| `terminal.create` | `{name?, profile?, command?, cwd?, env?, size?}` | `{terminal_id, window_id, pane_id, name}` | cli, gui, mcp* | 10 |
| `terminal.rename` | `{target, name}` | `{terminal_id, name}` | cli, gui, mcp* | 10 |
| `terminal.stop` | `{target, force, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.kill` | `{target, force, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.remove` | `{target, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.restore` | `{target, dry_run, claude_resume}` | `{plan}` or `{restored}` | cli, gui, mcp* | 30 |
| `window.create` / `.list` / `.rename` / `.kill` / `.select` | target-based | objects | cli, gui, mcp* | 21 |
| `pane.split` / `.list` / `.kill` / `.select` / `.resize` / `.swap` / `.zoom` | target-based | objects | cli, gui, mcp* | 21–22 |
| `pane.attach` | `{target, interactive, size?}` | `{stream_id, pane_id, seq}` then snapshot + output data frames | cli, gui, mcp* | 12–13 |
| `pane.detach` | `{stream_id}` | `{}` | cli, gui, mcp | 12 |
| `pane.resize` | `{stream_id, cols, rows}` | `{}` | cli, gui | 12 |
| `pane.send_keys` | `{target, keys: [string], literal}` | `{}` | cli, gui, mcp* | 12, 24 |
| `pane.capture` | `{target, start?, end?, format: "text" \| "vt"}` | `{text}` | cli, gui, mcp* | 13 |
| `claude.report` | `{pane_id, session_id, event, at, reason?}` | `{}` | **hook only** | 27 |
| `claude.status` | `{target?}` | `{panes: [{pane_id, state, since, session_id?}]}` | cli, gui, mcp | 28 |

`mcp*` = only when the user granted that capability to MCP clients (Step 37).

**Object shapes** (`Terminal`, `Window`, `Pane`) follow the state schema
([STATE_SCHEMA.md](STATE_SCHEMA.md)) plus volatile fields (`state`, `pid` hint, `exit_code`,
`clients`, `claude_state`). `target` follows [ADR 0007](../adr/0007-object-identity-and-naming.md).

## 5. Events (v1)

`terminal.created`, `terminal.renamed`, `terminal.state_changed`, `terminal.removed`,
`window.created`, `window.renamed`, `window.removed`, `pane.created`, `pane.exited`
(`{pane_id, exit_code}`), `pane.removed`, `pane.resync` (`{stream_id}` — client must discard
buffered output; a snapshot follows), `claude.state_changed`, `host.stopping`.

## 6. Error codes (stable)

| Code | Meaning |
| --- | --- |
| `protocol_error` | Malformed frame/JSON, limit exceeded; connection closed |
| `protocol_unsupported` | No common protocol major version |
| `unknown_request` | Method not known to this Host |
| `invalid_params` | Params missing/invalid; `details.field` names the field |
| `unauthorized` | Method not allowed for this client kind/capability |
| `target_not_found` | No object matches the target |
| `ambiguous_target` | Several objects match; `details.candidates` lists IDs |
| `invalid_name` | Name violates ADR 0007 rules |
| `name_conflict` | Name already used (case-insensitive) |
| `busy` | Destructive operation on a busy object without `force` |
| `spawn_failed` | Process could not start; `details.reason` (`not_found`, `access_denied`, `bad_cwd`, `other`) |
| `invalid_state` | Operation not allowed in the object's state (e.g. restore a `running` Terminal) |
| `state_from_newer_version` | Stored state was written by a newer Subak; read-only |
| `unsupported_platform` | Feature not available on this OS |
| `host_stopping` | Host is shutting down |
| `timeout` | Operation exceeded its deadline |
| `internal_error` | Bug; request failed, Host continues |

CLI exit codes map from these codes (Step 07 `CLI_CONTRACT.md`, Step 12).

## 7. Compatibility rules

1. Unknown JSON fields are ignored by both sides.
2. New methods, events, fields and error codes are **additive** (minor; advertised via
   `features`).
3. Removing/renaming anything or changing semantics requires protocol major `2`; a Host may
   serve several majors.
4. Clients must handle unknown error codes as generic failures and unknown events by ignoring
   them.
