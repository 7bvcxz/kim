# Release, install and update policy

Status: **current** · Step 06 · Decisions: [ADR 0013](adr/0013-kim-manager-architecture.md),
[CATALOG_FORMAT](CATALOG_FORMAT.md) · Requirements: `KIM-SEC-001…005`, `KIM-FR-005…009`,
`KIM-FR-034`

## 1. Consent rules (non-negotiable)

1. **Nothing installs, updates or uninstalls by itself.** Each operation starts from one explicit
   user action naming the tool: a CLI command (`kim install subak`, `kim update subak`,
   `kim uninstall subak`) or a button in Kim Manager. There is no background service, scheduled
   task, auto-updater, or "install on launch" (`KIM-SEC-001`).
2. **Update checks only show information.** `kim update check` and Kim Manager list newer
   versions; they never download a package. Kim Manager refreshes the catalog at start-up only
   when the user enabled *Check for updates when Kim starts* (off by default); `offline_mode`
   blocks every network operation (`KIM-SEC-002`). A settings file that exists but cannot be
   read or parsed **fails closed**: Kim treats it as offline (error `offline_mode`, explaining
   the unreadable file) until the file is fixed or removed, because it may contain the user's
   offline choice.
3. **Dependencies are never installed implicitly.** A release that needs another tool fails with
   `dependency_missing` and names what to install first.
4. **Kim never stops a running program.** Uninstall is refused (`tool_running`, exit 7) while any
   program from the tool's directory runs; an update installs side-by-side, leaves the running
   version untouched and reports that the new version is used on next start (`KIM-FR-034`).
5. **User data survives uninstall** unless the user separately confirms deleting it
   (`--purge-data` plus an interactive `delete` confirmation or `--yes`; in Kim Manager a
   checkbox plus an "I understand" acknowledgement) (`KIM-SEC-005`).
6. **No elevation.** Installs are per user under `%LOCALAPPDATA%\Programs\mouv`; Kim never
   requests administrator rights (`KIM-SEC-004`).

## 2. Install transaction

| Phase | What happens | On failure / cancel |
| --- | --- | --- |
| Plan | Resolve tool, release (newest compatible on the channel, or `--version`), package for this OS/arch/build, dependencies, running programs. `--dry-run` stops here. | Error, nothing created |
| Lock | Per-tool lock `<tool>\.kim.lock` (PID); a lock of a dead process is taken over | `busy` (exit 7) |
| Download | Into `<programs>\.staging\<tool>-<pid>-<ts>\package`; progress events; refuses more bytes than the signed catalog promises; Ctrl+C / Cancel aborts | Staging removed |
| Verify | Size and SHA-256 must match the **signed** catalog (authenticity comes from the catalog signature) | `integrity_failed` (exit 4) |
| Unpack | Zip entries confined to the staging dir (zip-slip refused), ≤ 10 000 entries, ≤ 8 GiB | `package_invalid` (exit 4) |
| Self-check | `<entrypoint> --version` must exit 0 within 15 s (no window) | `self_check_failed` (exit 4) |
| Activate | Move to `versions\<version>`; atomically replace `current.json` (**the only commit point**) | Move undone; previous version exactly as before |
| Clean up | Remove staging; keep the previous version for rollback; remove older versions not in use | In-use versions kept, removed by a later operation |

Leftover staging from a crash is removed by the next operation on that tool. An interrupted
install therefore never leaves a half-activated tool.

## 3. Versioning and channels

- Each tool uses SemVer independently; release tags are `<tool>-v<version>` (CONTRIBUTING).
- Channels: `stable` (default) and `preview`; `--channel preview` considers both. A downgrade
  happens only with an explicit `--version`.
- The previous version stays on disk after an update. A dedicated `kim rollback` command is
  planned with the release work (Step 40); until then reinstall the previous version explicitly.

## 4. Signing keys

| Key | Purpose | Custody |
| --- | --- | --- |
| `4466fe26eca99520` (ed25519) | **Development** key for preview catalogs (Step 05) | Secret seed kept offline by the maintainer, outside the repository, user-only file ACL |
| release key | Stable catalogs and release manifests | To be created for the first real release (Step 40): generated offline, stored encrypted, with a documented backup; never placed in CI |

Rotation: add the new public key to `TRUSTED_KEYS` in a Kim release, publish catalogs signed by
the new key, remove the old key one release later. A compromised key is removed immediately and
the catalog version is bumped so caches move off the old catalog.

Package integrity is anchored in the signed catalog (SHA-256 per asset). Authenticode signing of
the binaries themselves is additional and is used only when a legitimate certificate exists
(Step 40); Kim never fakes or claims a signature it does not have.

## 5. Publishing a release (maintainer checklist)

1. Build the tool with the MSVC target in CI; produce `<tool>-<version>-windows-x86_64.zip`.
2. Compute size and SHA-256; upload to the GitHub release `<tool>-v<version>`.
3. Add the release to `catalog/catalog.json`, bump `catalog_version`, keep `expires_at` ≤ 6
   months ahead.
4. `cargo xtask catalog-sign --key <secret key file>`; copy the catalog to
   `docs/contracts/examples/kim-catalog-v1/bundled.json`; `cargo xtask ci`.
5. Merge to `main`; users see the release after `kim catalog refresh` (or a new Kim build).

No release of Subak exists yet; the bundled catalog lists it with `releases: []`.
