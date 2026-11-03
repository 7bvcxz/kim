# `kim` command reference and agent workflows

Status: **current** · `kim` 0.0.1 · CLI contract v1 · Steps 07, 46, 47 · Rules:
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
| `kim catalog list` | read | no | — | — | Catalog tools, their release stage (`availability`: planned, preview, available) and their status here; `--channel stable\|preview`, `--installed` |
| `kim catalog show <tool>` | read | no | — | — | Description, releases, the package for this machine, compatibility, and the tool's features, how it runs, entry points, links, shortcuts, permissions, install locations and manual-only actions ([CATALOG_FORMAT §2.1](CATALOG_FORMAT.md)) |
| `kim catalog status` | read | no | — | — | Catalog in use, signer, freshness, cache path, trusted keys |
| `kim catalog verify <file>` | read | no | — | — | Verify any catalog file and its signature (`--sig <file>`) |
| `kim catalog refresh` | write | **yes** | yes | — | Fetch, verify and cache the catalog (`--source <url>`); installs nothing |
| `kim update check` | write¹ | yes¹ | yes | — | List available updates; never downloads a package. ¹Only with `--refresh [--source <url>]` |
| `kim install <tool>` | install | **yes** | yes | — | Download, verify, self-check, activate; `--version`, `--channel`, `--reinstall`, `--allow-stale` |
| `kim update <tool>` | install | **yes** | yes | — | Install the newest compatible release of an installed tool; the old version keeps running and is kept for rollback |
| `kim uninstall <tool>` | destructive | no | yes | **yes** (when Kim cannot ask) | Remove program files; user data is kept unless `--purge-data` |
| `kim settings show` | read | no | — | — | Settings and their file |
| `kim settings set <key> <true\|false>` | write | no | yes | — | `offline_mode` (refuse all network use) or `check_updates_on_launch` (Kim Manager only; off by default) |
| `kim hotkeys list` | read | no | — | — | Kim Manager's keyboard shortcuts: every action with keys, scope, source and state, plus the fixed navigation keys (`--scope`, `--profile`); §5 |
| `kim hotkeys show <action>` | read | no | — | — | One action and every conflict that involves it, including other installed tools |
| `kim hotkeys set <action> <keys>…` | write | no | yes | — | Replace an action's bindings (`--scope`, `--trigger`, `--hold-ms`, `--repeat`, `--profile`, `--replace`, `--allow-conflict`); refused with `hotkey_conflict` instead of breaking another binding |
| `kim hotkeys disable <action>` / `--all` / `--scope <s>` | write | no | yes | — | Turn one action, every shortcut, or one scope off |
| `kim hotkeys enable --all` / `--scope <s>` | write | no | yes | — | Turn them back on (per action: `reset`) |
| `kim hotkeys reset <action>` / `--all` | destructive¹ | no | yes | `--all` (when Kim cannot ask) | Restore defaults; `--all` keeps `hotkeys.json.bak`. ¹Only `--all` |
| `kim hotkeys check [--all-tools]` | read | no | — | — | Conflict report; `--all-tools` adds every installed Kim tool, from its files only; `--min-severity` |
| `kim hotkeys export` / `import <file>` | read / write | no | — / yes | — | Print the user layer / validate a document and replace the user layer (keeps a backup) |

The `--json` `data` of every command is specified in
[kim-cli-v1](contracts/schemas/kim-cli-v1.schema.json); real examples are in
`contracts/examples/kim-cli-v1/` (e.g. `status.json`, `install.json`, `tools-describe.json`).
The `hotkeys` commands follow the shared [hotkeys-cli-v1](contracts/schemas/hotkeys-cli-v1.schema.json)
(examples in `contracts/examples/hotkeys-cli-v1/`).

Exit codes and error codes: [CLI_CONTRACT §5–§6](CLI_CONTRACT.md#5-exit-codes).

## 3. Files and environment

| Item | Location (Windows) |
| --- | --- |
| Installed tools | `%LOCALAPPDATA%\Programs\mouv\<tool>\` (`current.json`, `versions\<version>\`, optional `capabilities.json` per version) |
| Catalog cache | `%LOCALAPPDATA%\mouv\kim\catalog\` (written only by an explicit refresh) |
| Settings | `%APPDATA%\mouv\kim\settings.json` |
| Keyboard shortcuts (only your changes) | `%APPDATA%\mouv\kim\hotkeys.json`, backup `hotkeys.json.bak` (§5) |
| Other tools' shortcuts (read by `hotkeys check --all-tools`) | `versions\<version>\hotkeys.manifest.json` of each install and `%APPDATA%\mouv\<tool>\hotkeys.json` |
| Tool user data (kept on uninstall) | `%APPDATA%\mouv\<tool>\`, `%LOCALAPPDATA%\mouv\<tool>\` |

| Variable | Effect |
| --- | --- |
| `KIM_NO_INPUT` | Any value except empty, `0` or `false`: never prompt (like `--no-input`) |
| `KIM_NO_HOTKEYS` | Any value except empty, `0` or `false`: Kim Manager starts with every shortcut off (like `kim-manager --no-hotkeys`); the file is not touched |
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

### 4.6 Tell the user what a tool may do before installing it

```sh
kim catalog show noeul --json --no-input         # data.tool.availability: planned → nothing to install yet
kim install noeul --dry-run --json --no-input    # data.plan.permissions, data.plan.dependencies
```

Show `data.permissions` (or `data.plan.permissions`) to the user before asking for consent,
including which ones are `opt_in`. `availability: planned` means no release exists: `kim install`
fails with `no_release` (exit 5); do not retry. For an installed tool, `data.shortcuts` comes
from its installed hotkey manifest and the user's `hotkeys.json` (`source: "installed"`).
[`catalog_tools.rs`]

### 4.7 Respect an offline machine

```sh
kim settings set offline_mode true --dry-run --json --no-input   # preview
kim settings set offline_mode true --json --no-input
```

All reads and dry runs keep working; every network command fails with `offline_mode` (exit 6)
without contacting anything.
[`cli_functional.rs::offline_mode_refuses_every_network_command_and_allows_reads_and_dry_runs`]

### 4.8 Error handling template

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

## 5. Keyboard shortcuts

Kim Manager's shortcuts follow the cross-tool [hotkey contract](HOTKEY_CONTRACT.md); the same
`hotkeys` commands exist in every Kim tool with a GUI. The `kim hotkeys` commands only read and
write files: they never open a window, never register a hotkey, never install a keyboard hook
and never start another tool (`KIM-FR-045`).

| Action | Default | What it does in Kim Manager |
| --- | --- | --- |
| `catalog.refresh` | `F5` | Same as **Refresh catalog** |
| `help.show` | `F1` | Opens the shortcut list (every action, its keys and state) |
| `tool.install` | none | Same as **Install** for the selected tool |
| `tool.update` | none | Same as **Update** for the selected tool |
| `tool.uninstall` | none | Opens the same confirmation as **Uninstall…**; a shortcut never skips it |

`Tab`, `Shift+Tab`, `F6`, `Enter`, `Space`, `Escape`, `Shift+F10` and `Alt+F4` are fixed
navigation keys (listed, never rebindable). Kim Manager reads keys only from its own window
(`focus` backend): nothing is registered system-wide and nothing runs when the window is closed.

**Change, disable and restore a shortcut** (no reinstall, no restart: an open Kim Manager picks
the change up when its window gets focus again):

```text
kim hotkeys list                          # what is bound, and where
kim hotkeys set catalog.refresh Ctrl+R    # aliases work: ctrl+r, C-r
kim hotkeys set tool.install "Ctrl+K I"   # a sequence is one quoted argument
kim hotkeys disable help.show             # this action has no key now
kim hotkeys reset catalog.refresh         # back to F5
kim hotkeys disable --all                 # every shortcut off (enable --all turns them on)
kim hotkeys reset --all --yes             # discard every change; hotkeys.json.bak is kept
```

Keys are written as in the contract (§5): modifiers in the order `Ctrl`, `Alt`, `Shift`,
`Win`, then a key name such as `R`, `F5`, `PageDown`, `Plus`, `code:KeyZ` (a physical key) or,
after a first step, `char:?` (a character). Each `<keys>` argument is one binding.

**Conflicts are refused, never resolved silently.** A key the OS or Kim reserves fails with
`hotkey_reserved` (exit 2), keys that do not parse with `hotkey_invalid` (2), an unknown action
with `action_not_found` (3), and a key another action of Kim Manager already uses with
`hotkey_conflict` (7); nothing is written in any of these cases. `--replace` takes the key away
from the other action and lists both changes. Warnings (for example `global_shadows_app`: a key
another installed tool uses system-wide) are shown but do not block.

**Across tools.** `kim hotkeys check --all-tools` reads every installed tool's
`hotkeys.manifest.json` and `hotkeys.json` and reports conflicts between them (for example two
tools that both want `F1` globally) with the exact command to fix each one in the tool that owns
it (`noeul hotkeys set capture.region Shift+F1`). Kim never edits another tool's file. A
successful check exits 0 even when it finds conflicts; read `data.summary`.

**If `hotkeys.json` is broken** (bad JSON, unknown key, a reserved chord), Kim Manager uses its
defaults and `kim hotkeys list` says why (`config.state: "invalid"`). Changes are refused with
`hotkeys_invalid` (4) until you fix the file or run `kim hotkeys reset --all --yes`, which keeps
the broken file as `hotkeys.json.bak`. A file written by a newer Kim is read-only
(`hotkeys_from_newer_version`, 5). `kim-manager --no-hotkeys` or `KIM_NO_HOTKEYS=1` starts Kim
Manager with every shortcut off.

**Not yet:** an in-window binding editor with keyboard capture (contract §11.2). Until then the
shortcut list (F1) shows the keys and the CLI changes them.
