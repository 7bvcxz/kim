# Kim catalog format and read-only catalog CLI

Status: **current** · Step 05 · Decision: [ADR 0013](adr/0013-kim-manager-architecture.md) ·
Schema: [contracts/schemas/kim-catalog-v1.schema.json](contracts/schemas/kim-catalog-v1.schema.json)

The catalog tells Kim which tools exist, which releases and packages they have, and how to
verify them. It is **data, not code**: reading it never installs, updates or starts anything
(`KIM-FR-002`, `KIM-SEC-001`).

## 1. Files

| File | Where | Purpose |
| --- | --- | --- |
| `catalog.json` | repository [`catalog/`](../catalog/catalog.json); bundled into `kim.exe` at build time; published at `https://raw.githubusercontent.com/7bvcxz/kim/main/catalog/catalog.json` | The catalog document |
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
      "homepage": "https://github.com/7bvcxz/kim",
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
              "url": "https://github.com/7bvcxz/kim/releases/download/subak-v0.1.0/subak-0.1.0-windows-x86_64.zip",
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

## 3. Bundled catalog policy

The bundled catalog lists only **real** Kim tools and ships **no placeholder releases**: until a
tool has a real, signed release it is listed with `releases: []` (status *not released*). This is
enforced by a test (`the_real_bundled_catalog_is_signed_by_a_trusted_key_and_installs_nothing`).
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

## 6. Tool status

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
| `kim catalog show <tool> [--channel …]` | One tool: description, releases, package for this machine, compatibility | no | nothing |
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
contract (including `--dry-run`, `--verbose` and the full error-code registry) is finalized in
Step 07 (`CLI_CONTRACT.md`); these codes are its first entries.
