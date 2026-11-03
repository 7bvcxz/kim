# `kim` command reference and agent workflows

Status: **current** · `kim` 0.0.1 · CLI contract v1 · Step 07 · Rules:
[CLI_CONTRACT](CLI_CONTRACT.md) · Catalog: [CATALOG_FORMAT](CATALOG_FORMAT.md) · Consent:
[RELEASE_POLICY](RELEASE_POLICY.md)

`kim` is headless: it never opens a window, never runs in the background, and never installs,
updates or removes anything you did not name (`KIM-FR-012`, `KIM-SEC-001`). Everything Kim
Manager can do is available here (`KIM-FR-013`). The same list in machine-readable form is
`kim capabilities --json`.

## 1. Global options

| Option | Meaning |
| --- | --- |
| `--json` | Exactly one JSON document on stdout (also for errors); see [CLI_CONTRACT §7](CLI_CONTRACT.md#7-output-streams-and-the---json-envelope) |
| `-q`, `--quiet` | No warnings or progress on stderr |
| `-v`, `--verbose` | Extra diagnostics on stderr (catalog used, fetches, timing, error code) |
| `--no-input` | Never ask, even on a terminal; same as `KIM_NO_INPUT=1` |
| `-h`, `--help` / `-V`, `--version` | Help / version text (always human text) |

## 2. Commands

| Command | Effect | Network | `--dry-run` | Needs `--yes` | What it does |
| --- | --- | --- | --- | --- | --- |
| `kim status` | read | no | — | — | Kim version and build, catalog source and freshness, settings, installed tools, updates known from the local catalog, problems (damaged installs, leftovers) |
| `kim version` | read | no | — | — | Kim version, build, CLI contract and schema versions, whether the local catalog lists a newer Kim (Kim never updates itself) |
| `kim capabilities` | read | no | — | — | Kim's own capabilities document |
| `kim tools list` | read | no | — | — | Installed tools: version, executable, status, capabilities summary |
| `kim tools describe <tool>` | read | no | — | — | One tool with its full capabilities; `--require-version <req>`, `--require-command <name>` (repeatable), `--probe [--timeout <s>]` |
| `kim catalog list` | read | no | — | — | Catalog tools and their status here; `--channel stable\|preview`, `--installed` |
| `kim catalog show <tool>` | read | no | — | — | Description, releases, the package for this machine, compatibility |
| `kim catalog status` | read | no | — | — | Catalog in use, signer, freshness, cache path, trusted keys |
| `kim catalog verify <file>` | read | no | — | — | Verify any catalog file and its signature (`--sig <file>`) |
| `kim catalog refresh` | write | **yes** | yes | — | Fetch, verify and cache the catalog (`--source <url>`); installs nothing |
| `kim update check` | write¹ | yes¹ | yes | — | List available updates; never downloads a package. ¹Only with `--refresh [--source <url>]` |
| `kim install <tool>` | install | **yes** | yes | — | Download, verify, self-check, activate; `--version`, `--channel`, `--reinstall`, `--allow-stale` |
| `kim update <tool>` | install | **yes** | yes | — | Install the newest compatible release of an installed tool; the old version keeps running and is kept for rollback |
| `kim uninstall <tool>` | destructive | no | yes | **yes** (when Kim cannot ask) | Remove program files; user data is kept unless `--purge-data` |
| `kim settings show` | read | no | — | — | Settings and their file |
| `kim settings set <key> <true\|false>` | write | no | yes | — | `offline_mode` (refuse all network use) or `check_updates_on_launch` (Kim Manager only; off by default) |

The `--json` `data` of every command is specified in
[kim-cli-v1](contracts/schemas/kim-cli-v1.schema.json); real examples are in
`contracts/examples/kim-cli-v1/` (e.g. `status.json`, `install.json`, `tools-describe.json`).

Exit codes and error codes: [CLI_CONTRACT §5–§6](CLI_CONTRACT.md#5-exit-codes).

## 3. Files and environment

| Item | Location (Windows) |
| --- | --- |
| Installed tools | `%LOCALAPPDATA%\Programs\mouv\<tool>\` (`current.json`, `versions\<version>\`, optional `capabilities.json` per version) |
| Catalog cache | `%LOCALAPPDATA%\mouv\kim\catalog\` (written only by an explicit refresh) |
| Settings | `%APPDATA%\mouv\kim\settings.json` |
| Tool user data (kept on uninstall) | `%APPDATA%\mouv\<tool>\`, `%LOCALAPPDATA%\mouv\<tool>\` |

| Variable | Effect |
| --- | --- |
| `KIM_NO_INPUT` | Any value except empty, `0` or `false`: never prompt (like `--no-input`) |
| `LOCALAPPDATA`, `APPDATA` | Where Kim reads and writes (tests and sandboxes redirect them) |
| `KIM_DEV_TRUSTED_KEY` | **Debug builds only** (compiled out of release builds): trust one extra development catalog key, for tests |

## 4. Sample agent workflows

Agents should always add `--json --no-input`, branch on the exit code first and the error code
second, and never add `--yes` unless the user agreed to that exact removal. Each workflow below
is exercised by automated tests (named in brackets) and by the e2e scenario
`e2e/tests/step07_cli_contract.rs`.

### 4.1 Health check and inventory

```sh
kim status --json --no-input        # one call: version, catalog freshness, offline mode, tools, updates
kim tools list --json --no-input    # executables and capabilities of installed tools
```

Read `data.problems` (damaged installs, stale catalog, leftovers) and `data.settings.offline_mode`
before planning network work. Neither command writes or fetches anything.
[`cli_contract.rs`, `cli_functional.rs::missing_and_damaged_tools_are_reported_not_guessed`]

### 4.2 Check for updates, then update with consent

```sh
kim update check --refresh --json --no-input   # exit 6 offline_mode/network_unavailable: report, stop
kim update subak --dry-run --json --no-input   # show data.plan to the user
kim update subak --json --no-input             # only after the user said yes
```

`update check` never downloads a package. The update leaves a running Subak untouched and
reports it in `data.result.running`. [`cli_contract.rs`, `cli_install.rs`]

### 4.3 Install a tool only if it is missing (idempotent)

```sh
kim tools describe subak --json --no-input     # exit 0: installed → done; exit 3 not_installed → continue
kim install subak --dry-run --json --no-input  # exit 5 no_release/incompatible: tell the user why
kim install subak --json --no-input            # exit 7 already_installed is also "done"
```

[`cli_contract.rs`, `cli_functional.rs::capability_negotiation_reports_absent_old_and_incompatible_tools`]

### 4.4 Make sure a tool can do what you need, then call it

```sh
kim tools describe subak --require-version ">=0.3" --require-command terminal.list --json --no-input
# exit 0 → data.tool.entrypoint is the program to run:
"<entrypoint>" terminal list --json --no-input   # (Subak's CLI arrives in Step 12)
```

Exit 5 with `tool_incompatible`, `capability_missing` or `capabilities_unavailable` means: do not
call it; tell the user an explicit `kim update subak` may help. Add `--probe` to ask the tool
itself when it ships no manifest (bounded by `--timeout`, no window). Invocation rules:
[CLI_CONTRACT §9.5](CLI_CONTRACT.md#95-invoking-another-tool).
[`cli_functional.rs::capability_negotiation_reports_absent_old_and_incompatible_tools`]

### 4.5 Remove a tool with consent

```sh
kim uninstall subak --dry-run --json --no-input   # show data.plan: what is removed, which data is kept
kim uninstall subak --yes --json --no-input       # only after the user confirmed
```

Without `--yes` the answer is `confirmation_required` (exit 7) and nothing changes; with a running
tool it is `tool_running` (exit 7) — Kim never stops programs. User data is kept unless the user
also asked for `--purge-data`. [`cli_functional.rs::non_interactive_runs_never_prompt_or_hang`]

### 4.6 Respect an offline machine

```sh
kim settings set offline_mode true --dry-run --json --no-input   # preview
kim settings set offline_mode true --json --no-input
```

All reads and dry runs keep working; every network command fails with `offline_mode` (exit 6)
without contacting anything.
[`cli_functional.rs::offline_mode_refuses_every_network_command_and_allows_reads_and_dry_runs`]

### 4.7 Error handling template

| Exit | Agent action |
| --- | --- |
| 0 | use `data` |
| 2 | fix the command (a bug in the agent) |
| 3 | offer an explicit install, or correct the id |
| 4 | stop; the input is untrusted or broken |
| 5 | explain the incompatibility; suggest an explicit update |
| 6 | retry later, or respect offline mode |
| 7 | ask the user (confirmation, running tool, stale catalog, missing dependency) |
| 1, 8 | report `error.message` and `error.hint` |
