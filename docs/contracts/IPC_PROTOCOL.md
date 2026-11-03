# Subak IPC protocol v1

Status: **implemented v1** (Step 09, frozen: changes are additive or need major 2) · Designed in
Step 03 · Decisions: [ADR 0005](../adr/0005-local-ipc.md), [ADR 0019](../adr/0019-secure-host-supervisor-and-ipc-v1.md)

Machine-readable schema: [schemas/ipc-v1.schema.json](schemas/ipc-v1.schema.json). Valid and
invalid example messages live in [examples/ipc-v1/](examples/ipc-v1/hello.json) and are checked by
`cargo xtask schema-check`, which is part of `cargo test`. Beyond the examples,
`crates/subak/src/proto.rs` validates every message this build constructs against the schema, and
`crates/subak/tests/host_ipc.rs` validates every control message a real Host sends.

The Step 08 prototype ([CONPTY_PROOF](../CONPTY_PROOF.md), [ADR 0016](../adr/0016-conpty-host-validation.md))
spoke protocol major **0** with this framing. A v1 Host refuses it with `protocol_unsupported`.

## 1. Transport and framing

Per-user named pipe `\\.\pipe\mouv-subak-<user SID>-s<session id>`. It is local only
(`PIPE_REJECT_REMOTE_CLIENTS`), its first instance is created with
`FILE_FLAG_FIRST_PIPE_INSTANCE`, and it carries an owner-only DACL: allow the user, deny
`NETWORK`. All I/O is overlapped on both ends (ADR 0016 §7). Each frame:

```
u32 LE length | u8 kind | payload (length - 1 bytes)
```

| kind | name | payload | limit |
| --- | --- | --- | --- |
| `0x01` | control | UTF-8 JSON object (one message) | 256 KiB |
| `0x02` | data | `u32 LE stream_id` · `u64 LE seq` · `u8 flags` · bytes | 1 MiB of bytes |

Data `flags`: bit 0 = snapshot (Host → client: the bytes reproduce recent output at `seq`),
bit 1 = input (client → Host). Other bits must be zero. A client may send only data frames whose
flags are exactly `input`.

**Limits and timeouts** (enforced by the Host):

| What | Limit | On violation |
| --- | --- | --- |
| Frame length | checked from the 5-byte header, before anything is allocated | `protocol_error`, close |
| `hello` | the whole frame within 5 s of connecting | `timeout` (nothing arrived) or `protocol_error` (incomplete), close |
| Any later frame | complete within 10 s of its first byte | `protocol_error`, close |
| Idle connection | no limit; heartbeats are the client's choice (`host.health`) | — |
| Connections | 64 at once (in handshake or established) | handshake error `busy`, close |
| Send queue per connection | 4096 frames or 16 MiB | the lagging client is disconnected; the pane never waits |

A violation closes **that connection only**. The Host first sends a connection-level error
`{"type":"error","error":{…}}` when it can. It never panics on input; a panic in a request
handler becomes `internal_error` and the Host continues (ADR 0019 §9).

## 2. Handshake and identity

1. **The client verifies the server before sending anything.** It takes the server pid from
   `GetNamedPipeServerProcessId` and checks that this process runs as the same user (token user
   SID) and that its image file name is `subak.exe`. Otherwise it refuses: CLI error
   `host_identity_mismatch` (exit 4), and nothing was sent.
2. Client → `{"type":"hello","protocol":{"min":1,"max":1},"client":{"name":"subak-cli","version":"0.0.1","kind":"cli"}}`.
   `client.name` and `client.version` are 1–64 printable characters. `client.kind` is one of
   `cli`, `gui`, `hook`, `mcp`.
3. **The Host verifies the client** from the token of the connection itself (identification-level
   impersonation, ADR 0019 §2). If its user SID differs from the Host's, the Host answers
   `unauthorized` and closes.
4. Version negotiation: the Host picks the highest protocol major in both ranges.
5. Host → `{"type":"welcome","protocol":1,"host":{"version":"0.0.1","pid":1234,"started_at":"…"},"features":[…]}`.
   The client checks that `host.pid` is the pid it verified.

   Otherwise the Host sends `{"type":"error","error":{"code":…,"message":…,"details":{…}}}` and
   closes:

   | Code | When |
   | --- | --- |
   | `protocol_unsupported` | no common major; `details.supported` = `{min, max}` of the Host |
   | `protocol_error` | not a valid `hello` (wrong type, missing or malformed fields, unknown kind, a data frame) |
   | `timeout` | no frame within 5 s |
   | `unauthorized` | the connection's user is not the Host's user |
   | `busy` | 64 connections are already open |
   | `host_stopping` | the Host is shutting down |

`features` of the Step 09 Host: `host.health`, `host.stats`, `events.subscribe`,
`identity.verified`, `prototype.single_pane`. Clients must ignore features they do not know.

## 3. Requests, responses, events

```
request   {"type":"request","id":<u64>,"method":"<name>","params":{…}}
response  {"type":"response","id":<u64>,"ok":true,"result":{…}}
          {"type":"response","id":<u64>,"ok":false,"error":{"code":"…","message":"…","details":{…}}}
event     {"type":"event","seq":<u64>,"event":"<name>","data":{…}}
```

- `id` is chosen by the client, unique per connection. Responses arrive in request order;
  `host.stop` and `pane.attach` answer once their work is queued or done.
- `message` is human-readable English and may change; **`code` is the stable contract**.
- Events are delivered only after `events.subscribe`, with a per-connection `seq`.
- After `welcome`, a control message that is not a request, or whose `id` is not a non-negative
  integer, is a `protocol_error` and closes the connection. A malformed `method` gets
  `unknown_request`, and `params` that are not an object get `invalid_params`
  (`details.field = "params"`); the connection stays open.
- Default client timeout: 10 s per request (30 s for `host.stop`).

## 4. Methods (v1)

Authorization is checked against this whole table, including methods of later Steps, before
anything else. A method a kind may not call answers `unauthorized`. A permitted method that this
Host does not implement yet answers `unknown_request`, as does any name not in the table.

| Method | Params | Result | Kinds | Step |
| --- | --- | --- | --- | --- |
| `host.health` | — | `{healthy, pid, version, protocol, uptime_s, stopping, terminals, clients}` | all | **09** |
| `host.status` | — | §4.1 | all | **09** |
| `host.stats` | — | metrics per [ADR 0012](../adr/0012-local-metrics-no-telemetry.md), §4.2 | cli, gui, mcp | **09** |
| `host.stop` | `{kill_terminals, dry_run}` | `{dry_run: true, would_kill: [pane ids], would_stop: status}` or `{stopping: true, shell}` (after the panes ended) | cli, gui | **09** |
| `events.subscribe` | `{topics: [...]}` from `terminals`, `panes`, `claude`, `host` (replaces earlier topics) | `{topics}` | cli, gui, mcp | **09** |
| `terminal.list` | `{detail: "summary" \| "full"}` | `{terminals: [Terminal]}` | cli, gui, mcp | 10 |
| `terminal.info` | `{target}` | `{terminal: Terminal}` | cli, gui, mcp | 10 |
| `terminal.create` | `{name?, profile?, command?, cwd?, env?, size?}` | `{terminal_id, window_id, pane_id, name}` | cli, gui, mcp* | 10 |
| `terminal.rename` | `{target, name}` | `{terminal_id, name}` | cli, gui, mcp* | 10 |
| `terminal.stop` | `{target, force, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.kill` | `{target, force, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.remove` | `{target, dry_run}` | `{affected}` | cli, gui, mcp* | 10 |
| `terminal.restore` | `{target, dry_run, claude_resume}` | `{plan}` or `{restored}` | cli, gui, mcp* | 30 |
| `window.create` / `.list` / `.rename` / `.kill` / `.select` | target-based | objects | cli, gui, mcp* | 21 |
| `pane.split` / `.list` / `.kill` / `.select` / `.swap` / `.zoom` | target-based | objects | cli, gui, mcp* | 21–22 |
| `pane.attach` | `{target?, interactive?, size?}` | `{stream_id, pane_id, seq, interactive, pane: {id, pid, size}, replay_bytes}`, then a snapshot data frame and live output | cli, gui, mcp* | **09** (prototype pane), 12–13 |
| `pane.detach` | `{stream_id?}` | `{}` | cli, gui, mcp | **09**, 12 |
| `pane.resize` | `{cols, rows}` | `{cols, rows}` | cli, gui | **09**, 12 |
| `pane.send_keys` | `{target, keys: [string], literal}` | `{}` | cli, gui, mcp* | 12, 24 |
| `pane.capture` | `{target, start?, end?, format: "text" \| "vt"}` | `{text}` | cli, gui, mcp* | 13 |
| `claude.report` | `{pane_id, session_id, event, at, reason?}` | `{}` | **hook only** | 27 |
| `claude.status` | `{target?}` | `{panes: [{pane_id, state, since, session_id?}]}` | cli, gui, mcp | 28 |

`mcp*` = only once the user granted that capability to MCP clients (Step 37). Until then MCP
clients may call only the unstarred methods. Bold Step = implemented by the Step 09 Host.

**Step 09 pane semantics.** Until the session model (Step 10) a Host owns at most one pane, the
Step 08 prototype pane. `pane.attach` with `target` absent or equal to its `p-…` id attaches to
it (`target_not_found` otherwise, or when the Host was started with `--no-shell`; `invalid_state`
once its process exited). `interactive: false` makes a read-only viewer: it receives output,
but its input frames are dropped and it may not resize (`invalid_state`). `pane.resize` needs an
interactive attach. Input frames reach the pane only from an interactively attached connection;
all others are dropped and counted (`host.stats` `ipc.input_frames_dropped`).

**Object shapes** (`Terminal`, `Window`, `Pane`) follow the state schema
([STATE_SCHEMA.md](STATE_SCHEMA.md)) plus volatile fields (`state`, `pid` hint, `exit_code`,
`clients`, `claude_state`). `target` follows [ADR 0007](../adr/0007-object-identity-and-naming.md).

### 4.1 `host.status` result

```json
{ "protocol": 1, "endpoint": "\\\\.\\pipe\\mouv-subak-S-1-5-21-…-s1",
  "host": { "pid": 1234, "version": "0.0.1", "started_at": "2026-11-04T01:02:03Z", "uptime_s": 42,
            "session_id": 1, "job_breakaway": true, "in_job": false,
            "job": null, "stopping": false, "idle_exit_s": 10, "environment": "user" },
  "terminals": 1,
  "pane": { "id": "p-9b0e7d6c5a41", "pid": 5678, "command": "C:\\Windows\\system32\\cmd.exe",
            "running": true, "exit_code": null, "size": {"cols": 120, "rows": 30},
            "job_processes": 1, "output_bytes": 2048 },
  "clients": { "connected": 2, "attached": 1,
               "list": [ {"pid": 4321, "kind": "cli", "name": "subak-cli", "version": "0.0.1",
                          "attached": true, "interactive": true, "topics": ["panes", "host"]} ] } }
```

`host.job` is `{kill_on_close, breakaway_ok, silent_breakaway_ok}` of the Host's immediate job,
or `null` outside any job (risk R-03). `host.environment` is `user` (built with
`CreateEnvironmentBlock`) or `inherited` (fallback). `pane` is `null` for a Host without a pane.

### 4.2 `host.stats` result

Numbers only (ADR 0012): `host.{uptime_s, handle_count}`, `terminals`, `panes`, `clients`,
`pane.{output_bytes_total, input_bytes_total}`, and `ipc.{connections_total,
rejected.{identity, limit, handshake, stopping}, requests_total, errors_total.{<code>: n},
protocol_errors, panics, input_frames_dropped, lagging_clients_dropped}`.

## 5. Events (v1)

Implemented in Step 09: `pane.exited` (`{pane_id, pid, exit_code}`, topic `panes`, sent after the
pane's last output) and `host.stopping` (`{reason: "stop_requested" | "idle"}`, topic `host`, sent
before the Host ends its panes).

Later Steps: `terminal.created`, `terminal.renamed`, `terminal.state_changed`,
`terminal.removed`, `window.created`, `window.renamed`, `window.removed`, `pane.created`,
`pane.removed`, `pane.resync` (`{stream_id}`: the client must discard buffered output; a snapshot
follows), `claude.state_changed`.

## 6. Error codes (stable)

| Code | Meaning |
| --- | --- |
| `protocol_error` | Malformed frame/JSON, limit exceeded; connection closed |
| `protocol_unsupported` | No common protocol major version |
| `unknown_request` | Method not known to, or not yet implemented by, this Host |
| `invalid_params` | Params missing/invalid; `details.field` names the field |
| `unauthorized` | Method not allowed for this client kind/capability, or a client of another user |
| `target_not_found` | No object matches the target |
| `ambiguous_target` | Several objects match; `details.candidates` lists IDs |
| `invalid_name` | Name violates ADR 0007 rules |
| `name_conflict` | Name already used (case-insensitive) |
| `busy` | Destructive operation on a busy object without `force` (e.g. `host.stop` with live panes and no `kill_terminals`), or the connection limit |
| `spawn_failed` | Process could not start; `details.reason` (`not_found`, `access_denied`, `bad_cwd`, `other`) |
| `invalid_state` | Operation not allowed in the object's state (e.g. restore a `running` Terminal) |
| `state_from_newer_version` | Stored state was written by a newer Subak; read-only |
| `unsupported_platform` | Feature not available on this OS |
| `host_stopping` | Host is shutting down |
| `timeout` | Operation exceeded its deadline (e.g. no `hello` within 5 s) |
| `internal_error` | Bug; request failed, Host continues |

The CLI maps them to its own codes ([CLI_CONTRACT §6](../CLI_CONTRACT.md)): `invalid_params` →
`usage`, `unauthorized` → `permission_denied`, `target_not_found` → `target_not_found`, `busy` →
`confirmation_required`, `host_stopping` → `host_disconnected`, `timeout` → `timeout`,
`protocol_unsupported` → `host_too_old` / `host_too_new`, protocol errors → `protocol_error`, and
anything else → `internal`. Client-side failures have their own codes: `host_identity_mismatch`
(the server is not `subak.exe` of this user) and `host_unresponsive` (a verified Host does not
answer in time, or its pipe stays busy). A client never stops or replaces such a Host.

## 7. Compatibility rules

1. Unknown JSON fields are ignored by both sides.
2. New methods, events, fields and error codes are **additive** (minor; advertised via
   `features`).
3. Removing/renaming anything or changing semantics requires protocol major `2`; a Host may
   serve several majors.
4. Clients must handle unknown error codes as generic failures and unknown events by ignoring
   them.
5. A client never restarts a Host to resolve a version mismatch (`host_too_old`, `host_too_new`).

## 8. Host lifetime, start on demand and manual checks

- **Start on demand** (ADR 0003 §3, ADR 0019 §4): connect; if nobody serves the endpoint, take
  the named mutex `Local\mouv-subak-start-<endpoint>` (wait up to 20 s), connect again, and only
  then spawn `subak host serve` detached (ADR 0016) with the user's logon environment. Then wait
  up to 10 s for the pipe. Several starters at once give one Host: measured with 6 concurrent
  `host start` calls, which produced one Host and five `host_already_running` errors naming its
  pid.
- **Idle exit** (ADR 0003 §4): the Host exits when it has owned no live pane and no connection for
  the idle grace period (`--idle-exit`, default 10 s). Measured: 2.02 s for a 2 s grace.
- **Stale endpoints**: a crashed Host's pipe disappears with it, so the next start succeeds at once.
  A stopping Host is waited for. A hung Host is reported as `host_unresponsive` and left alone. A
  foreign holder is reported as `host_identity_mismatch`.

**Not verified headlessly** (needs a second account or elevation on the maintainer's desktop):

1. *Another Windows user cannot connect.* Create a second local user (or use an existing one).
   As the first user run `subak host start --no-shell --idle-exit 600 --endpoint subak-xuser`.
   As the second user, in `runas /user:<other> cmd`, run
   `subak host status --endpoint subak-xuser --json`. **Expected:** `permission_denied` (exit 1),
   because the DACL refuses the open; the Host's `host.stats` shows no new connection. Then stop
   the Host as the first user.
2. *Elevated Host, normal client.* Start the Host from an administrator terminal, then run
   `subak host health` from a normal one. **Expected:** `host_identity_mismatch` ("its owner
   cannot be verified"), and the Host keeps running. The reverse (normal Host, elevated client)
   is expected to work, because the Host reads the client from its connection token.
