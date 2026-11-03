# Kim CLI contract

Status: **current** · CLI contract **v1** · Step 07 · Decision: [ADR 0015](adr/0015-cli-contract-and-tool-discovery.md)
· Requirements: `KIM-FR-010`, `KIM-FR-011`, `KIM-FR-012`, `KIM-FR-020`, `KIM-NFR-014`,
`KIM-SEC-013`

Every Kim tool's command line follows this contract: `kim` today, `subak` from Step 12, and
every later tool. It is what agents and scripts may rely on. The `kim` command reference is
[KIM_CLI.md](KIM_CLI.md); machine-readable schemas are in
[contracts/schemas/](contracts/schemas/cli-envelope-v1.schema.json).

Words: **MUST**/**MUST NOT** are requirements; **SHOULD** is a strong default that needs a
written reason to break.

## 1. Scope and conformance

- A tool **conforms** to contract v1 when it follows §2–§10. It announces this as
  `"cli_contract": 1` in its capabilities document (§9).
- The rules are enforced by tests: `kim` runs every command against the JSON schemas
  (`crates/kim/tests/cli_contract.rs`), checks streams, exit codes, prompts, permissions,
  partial downloads, offline mode and redaction (`cli_functional.rs`), and proves that headless
  commands start no process (`cli_headless.rs`). Subak's placeholder is validated against the
  same schemas (`crates/subak/tests/cli.rs`).

## 2. Names and verbs

- The binary is the tool id (`kim`, `subak`). Commands are lowercase words, nouns before verbs
  when grouped: `kim catalog list`, `kim tools describe <tool>`, `kim settings set <k> <v>`.
- Standard verbs: `list` (many), `show`/`describe` (one), `status` (summary), `check` (read-only
  comparison), `install`, `update`, `uninstall`, `set`, `refresh`, `verify`, `version`,
  `capabilities`. A new verb needs a reason that none of these fits.
- Long options are `--kebab-case`; a short alias exists only for very common flags (`-q`, `-v`,
  `-y`). Positional arguments are identifiers (a tool id, a target), never free-form switches.
- Words that are commands are reserved and never valid ids (`check`, `all`, `self`, `list` in
  the catalog).
- The **command name** of an invocation is the dotted path of its words: `catalog.list`,
  `tools.describe`, `settings.set`, `update.check`. It appears in the JSON envelope (§7) and in
  the capabilities document (§9).

## 3. Help, version and discoverability

- `--help` (and `-h`) works on every level, prints to stdout, exits 0, and lists every command
  and flag with a one-line description. Help always says what a command will not do when that
  matters (no background work, no network, nothing installed).
- `--version` (`-V`) prints `<tool> <semver>` and exits 0.
- `--help` and `--version` are human text even when `--json` is also given. The machine-readable
  equivalents are `<tool> version --json` and `<tool> capabilities --json`.
- Running the tool with no command prints help and exits 0; with `--json` it is a `usage` error
  document (exit 2), because a JSON caller always expects JSON.

## 4. Interaction: state changes, `--dry-run`, `--yes`, prompts

Every command has an **effect** (§9): `read`, `write` (the tool's own settings or caches),
`install` (adds, replaces or activates software) or `destructive` (removes software or user
data, ends running work).

1. **Nothing implicit.** A command changes only what its arguments name. Nothing installs,
   updates or starts by itself, nothing runs in the background, and the network is used only by
   commands documented to use it (`KIM-SEC-001`, `KIM-SEC-002`, `KIM-NFR-004`).
2. **`--dry-run`** MUST exist on every command whose effect is not `read`. A dry run performs
   the same validation and planning, prints what would happen (exit 0, or the error the real
   run would hit), downloads no packages and writes nothing. The one documented read it may add
   is fetching the small signed catalog (`catalog refresh --dry-run`,
   `update check --refresh --dry-run`); it is still refused in offline mode.
3. **Confirmation.** `destructive` commands confirm before acting. When the tool cannot ask
   they fail with `confirmation_required` (exit 7) unless `--yes`/`-y` is given. `install`
   commands do not prompt: naming the tool on the command line is the explicit consent.
4. **When a tool may ask.** Only when stdin **and** stderr are terminals, **and** neither
   `--json` nor `--no-input` is given, **and** the environment variable `KIM_NO_INPUT` is not set
   (any value except empty, `0` or `false` counts as set). Otherwise it never prompts and never
   waits for input. Prompts go to stderr.
5. **Headless means headless** (`KIM-FR-012`, NON-NEGOTIABLE). A CLI command never opens a
   window, tray icon, browser or console. Only a command whose capability says
   `"opens_window": true` (e.g. a future `subak gui`) may start a GUI, and only because the user
   ran exactly that command. Child processes are started without a console window.
6. **Cancellation.** Ctrl+C during a long operation cancels it with `cancelled` (exit 8) and
   leaves the previous state intact.

## 5. Exit codes

Exit codes are coarse classes for shell scripts; the error code (§6) is the precise reason.

| Exit | Class | Typical action for a caller |
| --- | --- | --- |
| 0 | success | — |
| 1 | failure: internal error, I/O, permission denied | report; for `permission_denied`, fix folder permissions (Kim never elevates) |
| 2 | usage: invalid arguments or a refused source URL | fix the command line |
| 3 | not found: unknown tool, tool not installed | install explicitly or correct the id |
| 4 | integrity / validation: malformed or untrusted data, failed self-check | do not trust the input; report |
| 5 | incompatible: no fitting release, tool too old, capability missing | update explicitly, or pick another version |
| 6 | unavailable: network down, offline mode, timeout | retry later or with a longer timeout; respect offline mode |
| 7 | conflict / needs confirmation: running tool, busy, dependency missing, stale catalog, already installed | resolve the conflict, or re-run with `--yes` / `--allow-stale` as the user decides |
| 8 | cancelled by the user | nothing changed |

Codes 9–63 are reserved for future classes and are not used. A Rust panic exits 101: it is
always a bug (report it).

## 6. Error-code registry

Error codes are stable `snake_case` strings. They are never renamed or reused; new codes are
additive (§8). Each tool documents its codes here; a tool MUST NOT give a registered code a
different meaning or exit code. `crates/kim/tests/cli_contract.rs` checks that this table, the
`kim-cli-v1` schema and `ErrorCode` in `crates/kim/src/error.rs` agree.

| Code | Exit | Meaning |
| --- | --- | --- |
| `io` | 1 | A local file-system operation failed. |
| `internal` | 1 | A bug, or the bundled catalog of this build failed verification. |
| `permission_denied` | 1 | The OS refused access (read-only or protected location). Kim never elevates to work around it. |
| `usage` | 2 | Invalid command-line usage: unknown command or flag, missing or bad argument value. |
| `unsupported_source` | 2 | A source URL uses a refused scheme or host (plain `http://` to a non-loopback host, `ftp://`, …). |
| `tool_not_found` | 3 | The named tool is not in the catalog. |
| `not_installed` | 3 | The tool is not installed, or its installed program is missing. |
| `catalog_invalid` | 4 | The catalog document is malformed or violates the format rules. |
| `signature_invalid` | 4 | The catalog signature does not verify, or the signature file is malformed. |
| `untrusted_key` | 4 | The signature was made by a key this build does not trust. |
| `catalog_rollback` | 4 | A fetched catalog is older than, or conflicts with, the trusted one. |
| `integrity_failed` | 4 | A downloaded package does not match the size or SHA-256 in the signed catalog. |
| `package_invalid` | 4 | The package is malformed: bad archive, unsafe paths, missing entrypoint. |
| `self_check_failed` | 4 | The staged program failed its `--version` self-check. |
| `capabilities_invalid` | 4 | A tool's capabilities document is malformed or describes another tool or version. |
| `no_release` | 5 | The tool has no release on the selected channel. |
| `incompatible` | 5 | No release or package fits this machine or this Kim version. |
| `capabilities_unavailable` | 5 | The tool publishes no capabilities (no manifest, or it predates `capabilities --json`). |
| `capability_missing` | 5 | The tool does not offer a command the caller requires. |
| `tool_incompatible` | 5 | The tool's version, CLI contract or capabilities schema does not satisfy the caller. |
| `network_unavailable` | 6 | A source could not be reached or a transfer broke off (offline, DNS, TLS, HTTP error, partial download). |
| `offline_mode` | 6 | Offline mode is on; the command needs the network. |
| `timeout` | 6 | An operation, such as asking another tool, did not finish within its time limit. |
| `tool_running` | 7 | The tool is running; Kim never stops it by itself. |
| `busy` | 7 | Another Kim operation on this tool is in progress. |
| `dependency_missing` | 7 | A dependency must be installed first; Kim never installs it implicitly. |
| `stale_catalog` | 7 | The catalog expired; refresh it or pass `--allow-stale`. |
| `confirmation_required` | 7 | A destructive command needs `--yes` because the tool cannot ask. |
| `already_installed` | 7 | The requested version is already installed; nothing to do (use `--reinstall` to repair). |
| `cancelled` | 8 | The user cancelled; nothing was changed. |

**Subak codes** (Step 08 `host` prototype). `subak` also uses `io`, `internal`,
`permission_denied`, `usage`, `timeout`, `confirmation_required` and `cancelled` with the meaning
and exit code above. `crates/subak/src/output.rs` tests that this table matches its code.

| Code | Exit | Meaning |
| --- | --- | --- |
| `spawn_failed` | 1 | The Host or its shell could not be started (missing program, bad command line). |
| `host_not_running` | 3 | No Subak Host serves the endpoint. |
| `protocol_error` | 4 | The Host or client sent a malformed or unexpected IPC message, or speaks another protocol version. |
| `unsupported_platform` | 5 | The command needs an OS feature this platform does not have yet (the Host needs Windows ConPTY until M4). |
| `host_disconnected` | 6 | The Host closed the connection unexpectedly (crashed, killed, or shutting down). |
| `host_already_running` | 7 | A Host already serves the endpoint; attach to it or stop it explicitly. |

Subak's IPC error codes ([IPC_PROTOCOL](contracts/IPC_PROTOCOL.md)) are a separate namespace;
the CLI maps them onto the codes above (for example IPC `invalid_params` → `usage`). Step 12
extends this table for Subak's real CLI. `host attach` is the one command whose stdout is not a
document: it streams the terminal, so `--json` is accepted only with `--dry-run`. The `data` of
the prototype's `host` commands is described in [CONPTY_PROOF.md](CONPTY_PROOF.md) and is not yet a
published schema (prototype; Step 12 publishes Subak's CLI schema); its envelopes are validated
against `cli-envelope-v1`.

## 7. Output streams and the `--json` envelope

**Streams.** Results go to **stdout**. Warnings, progress, prompts, diagnostics and human error
messages go to **stderr**. A failing command writes nothing to stdout in human mode. Output is
UTF-8 (`KIM-NFR-013`). Kim prints no color; a tool that adds color MUST honor `NO_COLOR` and
never color non-terminal output.

**Human errors** look like this, on stderr:

```text
kim: error: no tool `nope` in the catalog
  - <one line per detail, if any>
hint: known tools: kim, subak
```

**`--quiet` / `-q`** suppresses warnings and progress on stderr; results and errors still
appear. **`--verbose` / `-v`** adds diagnostic lines (`<tool>: …`) on stderr: which catalog
was used, what is fetched, timing, the error code and exit code. Neither flag ever changes
stdout. They conflict with each other (`usage`).

**`--json`** prints exactly **one** JSON document on stdout — also for failures, including
argument errors — and nothing else on stdout. Human progress is suppressed; stderr stays empty
unless `--verbose` is given. The document is
[cli-envelope-v1](contracts/schemas/cli-envelope-v1.schema.json):

```json
{ "ok": true, "command": "catalog.list", "schema_version": 1, "data": { }, "warnings": [] }
```

```json
{ "ok": false, "command": "tools.describe", "schema_version": 1,
  "error": { "code": "not_installed", "exit_code": 3, "message": "subak is not installed",
             "hint": "install it explicitly with `kim install subak`", "details": [] } }
```

- `command` is the command name (§2); when the arguments cannot be parsed it is the longest
  recognized command path, or the tool name alone (`"kim"`).
- `data` is always an object, so it can grow. `warnings` are human sentences; `details` lists
  individual findings (e.g. every validation problem).
- `error.exit_code` always equals the process exit code.
- Formatting (indentation, key order) is not part of the contract.
- Each tool publishes the `data` of its commands as a schema: `kim` in
  [kim-cli-v1](contracts/schemas/kim-cli-v1.schema.json), with examples generated from real
  runs in `contracts/examples/kim-cli-v1/`.

## 8. Versioning and compatibility

Three version numbers are independent (`KIM-NFR-014`):

| Version | Where | Changes when |
| --- | --- | --- |
| Tool version (SemVer) | `--version`, `capabilities.version` | every release of the tool |
| CLI contract major | `capabilities.cli_contract` | this document changes incompatibly |
| JSON schema version | envelope `schema_version`, `capabilities.json_schema_version` | a tool's `--json` output changes incompatibly |

**Additive changes keep the version:** a new command, a new optional flag, a new field in
`data`, a new error code, a new enum value (e.g. a new tool status), a new warning. Consumers
MUST ignore unknown fields and MUST handle unknown enum values and error codes by their exit
class (§5) — an unknown capability `effect` is treated as `destructive`.

**Breaking changes bump `schema_version`** (or the contract major): removing or renaming a
field, changing a type or meaning, changing an exit code, removing a command. A breaking change
ships with migration notes, and the previous major stays selectable for at least one minor
release (the selection mechanism is specified together with the first v2; none exists yet).

The published schemas are deliberately **closed** (no unknown fields) so regression tests
notice every change; updating a schema for an additive change is part of that change.

## 9. Capabilities, discovery and tool-to-tool invocation

Tools find and use each other without a resident orchestrator (`KIM-FR-020`): there is no
registry service and no daemon. Discovery reads files; invocation starts the other tool as a
short-lived process with a time limit. MCP may be added per tool later as an optional,
additional interface (`KIM-FR-021`).

### 9.1 `<tool> capabilities --json`

Every tool MUST implement `capabilities`. Its `data` is a
[tool-capabilities-v1](contracts/schemas/tool-capabilities-v1.schema.json) document:

```json
{ "schema": "kim.capabilities", "schema_version": 1, "tool": "subak", "version": "0.0.1",
  "cli_contract": 1, "json_schema_version": 1, "implementation": "placeholder",
  "summary": "…",
  "commands": [ { "name": "capabilities", "usage": "subak capabilities [--json]",
                  "summary": "…", "effect": "read", "network": false, "dry_run": false,
                  "requires_yes": false, "opens_window": false } ] }
```

- It lists every command the tool really has, nothing planned. A placeholder says
  `"implementation": "placeholder"` (Subak today).
- `effect` is the strongest effect the command can have with any flag; `network` says it may use
  the network; `dry_run` and `requires_yes` describe §4; `opens_window` is true only for GUI
  launchers.
- The command MUST be cheap and side-effect free: no network, no writes, no Host or GUI start,
  no prompt, answer within one second.

### 9.2 Static manifest

A package SHOULD ship the same document as `capabilities.json` at its root. Kim's installer
keeps it at `%LOCALAPPDATA%\Programs\mouv\<tool>\versions\<version>\capabilities.json`, so
`kim tools list|describe` show capabilities **without starting the tool**. A manifest must name
the installed tool and version exactly; otherwise it is `capabilities_invalid` and never
trusted. Release packaging (Step 40) generates it from `<tool> capabilities --json`.

### 9.3 Discovery with Kim

- `kim tools list --json`: every installed tool (and Kim itself) with version, absolute
  executable path, status and a capabilities summary. Reads install records and manifests only.
- `kim tools describe <tool> --json`: one tool with its full capabilities document (manifest,
  or Kim's built-in one). `--probe` asks the tool itself instead (§9.5).
- Callers that already know a tool's path may run `<path> capabilities --json` directly.

### 9.4 Version negotiation

A caller states what it needs; the answer is success or one structured error:

| Situation | Error | Exit |
| --- | --- | --- |
| The tool is not installed (or its program is missing) | `not_installed` | 3 |
| Installed version does not satisfy a SemVer requirement | `tool_incompatible` | 5 |
| Different `cli_contract` major, or a capabilities `schema_version` the caller cannot read | `tool_incompatible` | 5 |
| No capabilities available (no manifest, or an old tool without `capabilities`) | `capabilities_unavailable` | 5 |
| A required command is not listed | `capability_missing` | 5 |
| The document is malformed or names another tool/version | `capabilities_invalid` | 4 |
| The tool did not answer in time | `timeout` | 6 |

Kim implements this as
`kim tools describe <tool> [--require-version <req>] [--require-command <name>]… [--probe]`,
so a script or agent can check requirements in one call. The envelope's `schema_version` is
checked the same way: a caller that knows v1 refuses a v2 document with `tool_incompatible`.

### 9.5 Invoking another tool

A tool (or agent) that calls another Kim tool MUST:

1. Resolve the executable from `kim tools list --json` (or an explicit path); never through a
   shell.
2. Pass `--json` and `--no-input`, close stdin, and start the process without a console window
   (`CREATE_NO_WINDOW` on Windows). Never pass `--yes` on the user's behalf (§9.6).
3. Bound the call with a timeout (default 5 s for `capabilities`, 30 s for other read commands;
   long operations report progress instead of blocking), kill the child on expiry and report
   `timeout`.
4. Parse the envelope, check `schema_version`, and map failures to the codes of §9.4; a
   non-JSON answer is `capabilities_unavailable` for `capabilities`, otherwise the callee's own
   error code is passed through in `details`.
5. Cap captured output (Kim: 1 MiB) and redact it (§10) before showing or logging it.

`kim tools describe --probe` is the reference implementation (`kim::capabilities::probe`).

### 9.6 Consent and permissions per effect

| Effect of the called command | A tool may invoke it… |
| --- | --- |
| `read` | at any time, e.g. to show status; still bounded by §9.5 |
| `write` | only as the direct result of an explicit user action in the calling tool that names the change |
| `install` | only when the user explicitly asked for that install or update; suggest `--dry-run` first |
| `destructive` | as `install`, and only after the user confirmed in the caller's own UI; only then may the caller pass `--yes` |
| `opens_window: true` | never from a headless path |

Calls are local and run as the same user. No tool accepts remote invocation unless the user
explicitly enabled it (`SUBAK-SEC-012`).

### 9.7 No orchestrator

There is no Kim daemon, registry service or background discovery. Each call is a short-lived
process, idle cost is zero (`KIM-NFR-001`), and a missing or broken tool affects only the calls
that need it. Step 38 may extend interoperability (e.g. richer per-command schemas or MCP), but
within these rules.

## 10. Secrets and redaction

Secrets never appear in JSON output, human output, diagnostics or stored state (`KIM-SEC-013`).

- Every string a tool prints passes through redaction at the output boundary, and every URL a
  tool stores in its own state is redacted first (Kim: the `source_url` of install records and
  refresh results).
- **User-info** in a URL (`https://user:password@host/…`, `https://token@host/…`) becomes
  `https://***@host/…`.
- **Secret-looking parameters** in a query or fragment (names containing `token`, `secret`,
  `pass`, `pwd`, `key`, `sig`, `auth`, `credential`, `session`, `code`, `cookie`) keep their name
  and lose their value: `?token=***&v=2`.
- Redaction is deliberately over-eager; hiding a harmless value is acceptable. The real value is
  still used internally (e.g. to download), it is only never shown or stored.
- Tools MUST NOT print environment variable values, request headers or file contents that may
  hold secrets; proxy URLs from `HTTPS_PROXY`/`HTTP_PROXY` are redacted like any other URL.
- Kim's implementation is `kim::redact`; `cli_functional.rs` proves that credentials in a source
  URL, a proxy URL and a signed download URL never reach stdout, stderr (also with `--verbose`)
  or `current.json`.

## 11. What this contract does not cover

- Subak's IPC protocol and state files ([IPC_PROTOCOL](contracts/IPC_PROTOCOL.md),
  [STATE_SCHEMA](contracts/STATE_SCHEMA.md)).
- GUI behavior ([DESIGN_SYSTEM](DESIGN_SYSTEM.md)); GUIs reach the same operations through the
  library or the CLI (`KIM-FR-013`).
- Shell completion scripts and localized messages (not provided yet).
