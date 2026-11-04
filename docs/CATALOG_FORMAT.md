# Kim catalog format and read-only catalog CLI

Status: **current** · Step 05 · tool metadata, availability and Noeul/Ddalgak entries: Step 47 ·
Decision: [ADR 0013](adr/0013-kim-manager-architecture.md) ·
Schema: [contracts/schemas/kim-catalog-v1.schema.json](contracts/schemas/kim-catalog-v1.schema.json) ·
Requirements: `KIM-FR-003`, `KIM-FR-004`, `KIM-FR-050…052`, `KIM-SEC-006`

The catalog tells Kim which tools exist, which releases and packages they have, and how to
verify them. It is **data, not code**: reading it never installs, updates or starts anything
(`KIM-FR-002`, `KIM-SEC-001`).

## 1. Files

| File | Where | Purpose |
| --- | --- | --- |
| `catalog.json` | repository [`catalog/`](../catalog/catalog.json); bundled into `kim.exe` at build time; published at `https://raw.githubusercontent.com/hellomouv/kim/main/catalog/catalog.json` | The catalog document |
| `catalog.json.sig` | next to it | Detached ed25519 signature (§4) |
| cache | `%LOCALAPPDATA%\mouv\kim\catalog\catalog.json(.sig)` | Last catalog fetched by an explicit `kim catalog refresh` |

Signed files are marked `-text` in `.gitattributes`: the signature covers exact bytes.

## 2. Document (schema `kim.catalog`, version 1)

```json
{
  "schema": "kim.catalog",
  "schema_version": 1,
  "catalog_version": 1,
  "generated_at": "2026-11-04T00:00:00Z",
  "expires_at": "2027-05-04T00:00:00Z",
  "publisher": "mouv",
  "tools": [
    {
      "id": "subak",
      "name": "Subak",
      "summary": "Persistent Windows-first terminal …",
      "description": "…",
      "homepage": "https://github.com/hellomouv/kim",
      "license": "MIT OR Apache-2.0",
      "support": "in-development",
      "releases": [
        {
          "version": "0.1.0",
          "channel": "preview",
          "published_at": "2026-12-01T00:00:00Z",
          "release_notes": "…",
          "min_kim_version": "0.1.0",
          "dependencies": [{ "tool": "other-tool", "version_req": ">=1.2" }],
          "assets": [
            {
              "os": "windows", "arch": "x86_64", "min_os_build": 17763,
              "url": "https://github.com/hellomouv/kim/releases/download/subak-v0.1.0/subak-0.1.0-windows-x86_64.zip",
              "size": 4194304,
              "sha256": "64 lowercase hex characters",
              "kind": "zip",
              "entrypoint": "subak.exe"
            }
          ]
        }
      ]
    }
  ]
}
```

| Field | Rule |
| --- | --- |
| `schema`, `schema_version` | Must be `kim.catalog` / `1`. A higher version is refused with a hint to update Kim. |
| `catalog_version` | Integer ≥ 1, **strictly increased for every published change** (rollback protection). |
| `generated_at`, `expires_at` | RFC 3339 UTC (`…Z`); `expires_at` > `generated_at`. After `expires_at` the catalog is *stale* (§5). |
| `tools[].id` | `^[a-z][a-z0-9-]{1,31}$`, unique. |
| `tools[].support` | `stable`, `preview`, `in-development`, `deprecated`. |
| `releases[].version` | SemVer, unique per tool. `channel`: `stable` or `preview`. |
| `min_kim_version` | SemVer; releases needing a newer Kim are *incompatible*. |
| `dependencies[]` | Must name another tool in the catalog; `version_req` is a SemVer requirement. Never auto-installed (Step 06 asks). |
| `assets[]` | ≥ 1 per release; unique `os`/`arch` per release. `os` ∈ `windows`, `linux`; `arch` ∈ `x86_64`, `aarch64`; optional `min_os_build`. |
| `assets[].url` | `https://` only (`http://` allowed only for loopback mirrors). |
| `assets[].size`, `sha256` | 1 byte – 4 GiB; 64 lowercase hex. Verified before anything is staged (Step 06). |
| `assets[].kind`, `entrypoint` | `zip` or `exe`; relative path without `..`, drive letters or leading separators. |

Unknown fields are **ignored** (forward compatibility for additive changes); anything that
changes meaning requires `schema_version` 2. Kim reports *every* violated rule at once
(`kim catalog verify`).

### 2.1 Tool metadata (optional, Step 47)

A tool may describe itself beyond its releases. Every field below is **optional and additive**:
a catalog without them means exactly what it meant before, Kim versions before Step 47 ignore
them, and the signature covers them like everything else.

```json
{
  "id": "noeul", "…": "…", "releases": [],
  "features": ["F1 starts a region capture; a double-click inside the selection pins it to the desktop"],
  "runs": "tray",
  "entry_points": [
    { "kind": "gui", "command": "noeul", "summary": "Starts Noeul in the notification area", "status": "planned" },
    { "kind": "cli", "command": "noeul status --json", "summary": "Reports whether Noeul runs", "status": "planned" }
  ],
  "links": [ { "kind": "docs", "title": "Noeul philosophy (Step 48)", "url": "https://github.com/hellomouv/kim/blob/main/docs/NOEUL_PHILOSOPHY.md" } ],
  "shortcuts": {
    "manifest": "hotkeys.manifest.json", "backend": "register",
    "actions": [ { "action": "capture.region", "title": "Capture a region", "scope": "global", "keys": ["F1"] } ]
  },
  "permissions": [
    { "id": "screen-capture", "detail": "Reads the screen only when you start a capture" },
    { "id": "autostart", "detail": "Starts when you sign in only if you turn that on", "opt_in": true }
  ]
}
```

| Field | Rule |
| --- | --- |
| `features[]` | ≤ 16 lines, each 1–120 characters: what the tool does, one feature per line. |
| `runs` | How the program runs. Known values: `on-demand` (exits when its command or window is done), `window` (runs while its window is open), `tray` (keeps running in the notification area until the user quits it), `host` (keeps a background host while it owns sessions). Pattern `^[a-z][a-z0-9-]{1,31}$`; Kim shows a value it does not know as it is. Kim uses it for the hint when it refuses to uninstall a running tool. |
| `entry_points[]` | ≤ 32. `kind`: `gui`, `cli` or `mcp` (a `cli` entry point is headless by the [CLI contract](CLI_CONTRACT.md): it never opens a window). `command` and `summary`: 1–200 characters. `status`: `available` or `planned`. **A tool without releases may only declare `planned` entry points**: nothing can run what nobody can download. |
| `links[]` | ≤ 16. `kind`: `docs`, `mcp`, `shortcuts`, `source`, …; `title` 1–80 characters; `url` `https://` only. Kim Manager shows them as text and starts no browser. |
| `shortcuts` | A summary of the tool's hotkey manifest ([HOTKEY_CONTRACT §3.4](HOTKEY_CONTRACT.md)). `manifest` is the manifest's file name in the package (`hotkeys.manifest.json`); `backend` is `focus`, `register` or `hook` (§4.2); `actions[]` (≤ 64) lists the `global` and `app` actions with their default `keys` in canonical key syntax (§5, checked by Kim; an empty list for an unbound action), `scope` `global`, `app` or `context:<id>`, and `bindable: false` for keys the contract fixes. For the bundled catalog a test keeps the summary equal to the contract's manifest examples. |
| `permissions[]` | ≤ 32, unique `id`s: what the tool does that matters for security. Known ids: `global-hotkeys` (registers system-wide shortcuts), `keyboard-hook` (installs a low-level keyboard hook), `mouse-hook` (installs a low-level mouse hook; Step 68), `screen-capture`, `clipboard`, `input-injection` (injects keyboard and mouse input), `launch-programs`, `window-control`, `autostart`, `network`. `detail` (1–200 characters) says when and how; `opt_in: true` marks a capability the tool uses only after the user turned it on. Unknown ids are allowed and shown with their `detail`. |

What a catalog **cannot** declare, because Kim decides it (and shows it next to the metadata):

- **Install locations.** Program files go to `%LOCALAPPDATA%\Programs\mouv\<tool>`; user data is
  `%APPDATA%\mouv\<tool>` and `%LOCALAPPDATA%\mouv\<tool>`. Both are fixed by the tool id, per
  user, never elevated (`KIM-SEC-004`). A catalog field that chose a path would let a signed but
  mistaken catalog write anywhere.
- **Update policy.** Installing, updating and uninstalling are manual only (`kim install`,
  `kim update`, `kim uninstall <tool>` or the matching Kim Manager button); `kim update check`
  only shows updates (`KIM-SEC-001`, `KIM-SEC-002`). No catalog value can turn on an automatic
  update.
- **Availability** (§6): derived from `releases`, never declared.
- **The shortcuts of an installed tool.** Once a tool is installed, Kim shows the shortcuts of its
  *installed* `hotkeys.manifest.json` (which must name exactly this tool and version) together
  with the user's `hotkeys.json`, by the same rules as `kim hotkeys check --all-tools`
  ([HOTKEY_CONTRACT §9.2](HOTKEY_CONTRACT.md)). The catalog's summary is used only before an
  install, or with a note saying why the installed manifest could not be used (`KIM-FR-052`).

Before every install or update the plan discloses the declared `permissions` and the release's
`dependencies` (`kim install <tool> --dry-run`, and Kim Manager's details pane before the button
is pressed; `KIM-SEC-006`). Disclosure is not consent: the install still starts only from the
user's explicit action, and a missing dependency is never installed for the user.

## 3. Bundled catalog policy

The bundled catalog lists only **real** Kim tools and ships **no placeholder releases**: until a
tool has a real, signed release it is listed with `releases: []` (availability *planned*). This is
enforced by a test (`the_real_bundled_catalog_is_signed_by_a_trusted_key_and_installs_nothing`).

Since `catalog_version` 3 (Step 47) it lists four tools, each separately installable and none
depending on another: `kim` and `subak` (unchanged since Step 05), `noeul` (a Snipaste-style
screenshot and pin utility) and `ddalgak` (an AutoHotkey-style automation engine). Noeul and
Ddalgak are `support: in-development` with `releases: []` and the metadata of §2.1: features,
`runs: tray`, planned GUI, CLI and MCP entry points, links to their design issues and to the
hotkey contract, their shortcut summaries (Noeul: `F1` and `F3` global with the `register`
backend; Ddalgak: `hook` backend, only the fixed emergency chord bound) and their permissions
(Ddalgak's keyboard hook, input injection and program launching are `opt_in`). Tests keep this
honest (`the_bundled_catalog_lists_noeul_and_ddalgak_as_planned_tools_with_metadata`,
`bundled_shortcut_summaries_match_the_contract_hotkey_manifests`). Each tool's release Step
(63 for Noeul, 82 for Ddalgak) adds real releases; until then `kim install noeul` fails with
`no_release`.
Hypothetical tools exist only in test fixtures (`crates/kim/tests/fixtures/`,
`docs/contracts/examples/kim-catalog-v1/`) and use the reserved `.invalid` domain.

## 4. Signatures and trust

### 4.1 Format

`catalog.json.sig`:

```json
{ "algorithm": "ed25519", "key_id": "4466fe26eca99520", "signature": "<128 hex chars>" }
```

The signed message is `"mouv.kim.catalog.v1\n" || <exact bytes of catalog.json>`. The domain
prefix prevents a signature made for another document type from verifying as a catalog. `key_id`
is the first 8 bytes of SHA-256 of the 32-byte public key, in hex.

### 4.2 Verification order

1. Size limits (catalog ≤ 4 MiB, signature ≤ 16 KiB).
2. Signature file well-formed, algorithm `ed25519`, `key_id` in this build's trusted set
   (otherwise `untrusted_key`).
3. Strict ed25519 verification of the exact bytes (otherwise `signature_invalid`).
4. Schema and semantic validation (otherwise `catalog_invalid`).

### 4.3 Keys and rotation

Trusted public keys are compiled into Kim (`crates/kim/src/catalog/signature.rs`). The current
key `4466fe26eca99520` is a **development key for preview catalogs**; its secret seed is kept
offline by the maintainer (never in the repository). Rotation: add the new public key → publish
catalogs signed with it → remove the old key in a later Kim release. A release-grade key and its
custody are defined in Step 06 (`RELEASE_POLICY.md`) and Step 40.

Publisher commands (developer tooling):

```sh
cargo xtask catalog-keygen --out <secret-key-file>   # once; keep the file secret
cargo xtask catalog-sign --key <secret-key-file>     # signs catalog/catalog.json
```

## 5. Sources, offline behavior and freshness

| Situation | Behavior |
| --- | --- |
| No network, never refreshed | Bundled catalog is used. Everything read-only works. |
| Cache present and valid | The higher `catalog_version` of bundled vs. cache is used. |
| Cache tampered, truncated or half-written | Ignored with a warning; bundled (or a valid older cache) is used. The file is left in place for inspection. |
| Catalog past `expires_at` | Still used for listing, flagged **stale** (`stale: true`, warning). Step 06 decides whether installing from a stale catalog needs confirmation. |
| `kim catalog refresh` | The **only** network access. Fetches `<source>` and `<source>.sig`, verifies (§4.2), refuses a lower `catalog_version` or the same version with different content (`catalog_rollback`), then replaces the cache atomically (temp files → rename). On any failure the existing cache is untouched. |

Allowed refresh sources: `https://…`, `http://` to `127.0.0.1`/`localhost`/`[::1]`, and
`file://` (offline mirrors). Transport is defense in depth; trust comes from the signature. HTTPS
uses the operating system's TLS stack and certificate store (SChannel on Windows).

No command fetches automatically: not at start-up, not on a timer, not when a catalog is stale
(`KIM-SEC-002`, `KIM-NFR-004`).

## 6. Availability and tool status

**Availability** is the release stage of a tool: the same on every machine, and derived from the
signed `releases` alone (`KIM-FR-051`). A tool is shown as downloadable only because a signed,
hashed release exists, never because a catalog says so.

| Availability | Meaning |
| --- | --- |
| `planned` | No release on any channel. Listed so users know it exists; nothing can be installed (`kim install` fails with `no_release`: "… is planned"). |
| `preview` | Only preview-channel releases exist (`--channel preview`). |
| `available` | At least one stable release exists. Whether one fits *this* machine is the status below. |

`kim catalog list` and Kim Manager show a tool that is not installed as `available`,
`available (preview)`, `incompatible`, `planned` or `preview only`.

| Status | Meaning |
| --- | --- |
| `installed` | Installed, current on the selected channel. (`kim` itself is the running build.) |
| `update-available` | Installed; a newer compatible release exists. **Shown only**, never applied automatically. |
| `available` | Not installed; a compatible release exists. Install only on explicit request (Step 06). |
| `incompatible` | Releases exist but none fits this machine: OS, architecture, Windows build or required Kim version. The reason is reported. |
| `unreleased` | No release on the selected channel (a note says when only preview releases exist). |
| `damaged` | An install record exists but cannot be read (repair arrives with the installer, Step 06). |

Channel selection: `--channel stable` (default) considers stable releases; `--channel preview`
considers stable and preview. Install records are `%LOCALAPPDATA%\Programs\mouv\<tool>\current.json`
(written by the installer, Step 06).

## 7. CLI (read-only catalog commands)

| Command | What it does | Network | Writes |
| --- | --- | --- | --- |
| `kim catalog list [--channel stable\|preview]` | Tools and their status on this machine | no | nothing |
| `kim catalog show <tool> [--channel …]` | One tool: description, releases, package for this machine, compatibility, availability, the metadata of §2.1, Kim's install locations and manual-only actions, and for an installed tool its installed shortcuts | no | nothing |
| `kim catalog status` | Active source (bundled/cache), signer, freshness, cache path, trusted keys | no | nothing |
| `kim catalog verify <file> [--sig <file>]` | Full verification of any catalog file (publishers, agents) | no | nothing |
| `kim catalog refresh [--source <url>]` | Explicit fetch + verify + atomic cache update | **yes** | the cache only |

Global options: `--json` (one JSON document on stdout), `-q/--quiet` (no warnings on stderr).

JSON success envelope:

```json
{ "ok": true, "command": "catalog.list", "schema_version": 1, "data": { … }, "warnings": [] }
```

JSON error envelope (also on stdout when `--json` is given):

```json
{ "ok": false, "command": "catalog.show", "schema_version": 1,
  "error": { "code": "tool_not_found", "exit_code": 3, "message": "…", "hint": "…", "details": [] } }
```

| Exit code | Error codes |
| --- | --- |
| 0 | — |
| 1 | `io`, `internal` |
| 2 | `usage`, `unsupported_source` |
| 3 | `tool_not_found` |
| 4 | `catalog_invalid`, `signature_invalid`, `untrusted_key`, `catalog_rollback` |
| 6 | `network_unavailable` |

Human output goes to stdout, diagnostics and warnings to stderr. The shared, cross-tool CLI
contract — `--dry-run` (also `catalog refresh --dry-run`: fetch and verify, write nothing),
`--verbose`, `--no-input`, exit-code classes and the full error-code registry — is
[CLI_CONTRACT.md](CLI_CONTRACT.md) (Step 07); these codes are its first entries. Every command
of `kim` is listed in [KIM_CLI.md](KIM_CLI.md).
