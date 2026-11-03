# ADR 0005 — Local IPC transport, security and versioning

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Implemented in Step 09 (prototype in Step 08)

## Context

Clients (CLI, GUI, Claude hook reporter, later MCP) talk to the Host. The channel must be local
only, restricted to the same user, resistant to endpoint squatting and impersonation, versioned,
and cheap while idle (`SUBAK-SEC-010`, `SUBAK-SEC-012`, `KIM-NFR-014`).

## Decision

### Transport (Windows)

- **Named pipe** `\\.\pipe\mouv-subak-<user SID>-s<logon session id>`, in byte mode with
  overlapped I/O, `PIPE_REJECT_REMOTE_CLIENTS`. The first instance is created with
  `FILE_FLAG_FIRST_PIPE_INSTANCE`.
- **DACL**: a single ACE that grants the Host user's SID full access, plus an explicit deny for
  `NETWORK`. Everyone else, including other users and anonymous callers, cannot open the pipe.
- **Mutual identity checks**
  - The *server* gets the client PID (`GetNamedPipeClientProcessId`) and checks that the
    client's token user SID equals its own. On mismatch it drops the connection.
  - The *client* gets the server PID (`GetNamedPipeServerProcessId`) and checks that the
    server's token user SID equals its own and that the image path is a `subak.exe`. If either
    check fails it refuses the connection and reports `host_identity_mismatch` (exit code
    documented in Step 12). This defeats a squatter owned by another user that creates the pipe
    name first.

### Framing

`[u32 little-endian length][u8 kind][payload]`, where `length` covers `kind` + `payload`.

| kind | payload | max size |
| --- | --- | --- |
| `0x01` control | UTF-8 JSON object | 256 KiB |
| `0x02` data | `u32 stream id` · `u64 sequence` · `u8 flags` (snapshot / input) · raw bytes | 1 MiB |

Larger, truncated or malformed frames produce a `protocol_error` and disconnect that client
only. The Host never panics on input.

### Protocol

- **Handshake first**, within 5 s, or the Host disconnects. The client sends
  `hello {protocol: {min, max}, client: {name, version, kind}}`. The Host answers
  `welcome {protocol, host: {version, pid, started_at}, features: [...]}` or
  `error protocol_unsupported {supported: {min, max}}`.
- **Requests** `{id, type, params}` → **responses** `{id, ok: true, result}` or
  `{id, ok: false, error: {code, message, details}}`. Requests on one connection are answered in
  order. Default client timeout is 10 s. Long operations return immediately and report through
  events.
- **Events** `{event, seq, data}` go only to clients that subscribed (`events.subscribe`).
- **Compatibility**
  - The major protocol version changes only for breaking changes.
  - Minor, additive changes are advertised in `features`.
  - Unknown JSON fields are ignored.
  - An unknown `type` returns `unknown_request`.
  - A newer client talking to an older Host degrades, or reports `host_too_old` with an
    explicit hint. It never restarts the Host by itself.
- **Request authorization by client kind**:
  - `cli` and `gui` get full access for the same user;
  - `hook` may only send `claude.report`;
  - `mcp` gets the capability set configured by the user (Step 37).

### Portability

The transport is a narrow trait (`connect`, `accept`, `read_frame`, `write_frame`,
`peer_identity`). The planned Linux implementation is a Unix domain socket in
`$XDG_RUNTIME_DIR/mouv-subak/` (directory mode 0700, `SO_PEERCRED` checks), in M4
([ADR 0011](0011-portability-boundary.md)).

## Consequences

- No TCP listener ever exists by default.
- Data frames avoid base64 overhead for terminal output.
- The detailed message catalog lives in
  [contracts/IPC_PROTOCOL.md](../contracts/IPC_PROTOCOL.md) and is versioned with JSON schemas.

## Alternatives considered

- **TCP on localhost:** reachable by other users, and needs token auth. Rejected.
- **Unix sockets on Windows (`AF_UNIX`):** file-permission semantics are less mature on
  Windows, and named pipes have first-class ACLs. Rejected for Windows.
- **gRPC/protobuf:** heavy dependencies and codegen for a small protocol. JSON control plus
  binary data frames is enough and easy to debug.

## Requirements

`SUBAK-SEC-010`, `SUBAK-SEC-012`, `SUBAK-FR-004`, `KIM-NFR-014`, `KIM-NFR-004`.
