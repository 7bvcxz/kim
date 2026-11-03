# ADR 0013 — Kim manager: catalog and explicit installer

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Refined in Steps 05, 06 and 07

## Context

Kim lists tools and installs, updates and uninstalls them only on explicit request
(`KIM-SEC-001`, `KIM-SEC-002`). Artifacts must be verified (`KIM-SEC-003`), installs are per
user without elevation (`KIM-SEC-004`), and a failed update must leave the old version usable
(`KIM-FR-007`).

## Decision

1. **Catalog**
   - A signed JSON document lists tools and releases. Format: Step 05, `docs/CATALOG_FORMAT.md`.
   - A copy is **bundled** in `kim.exe`, and the last fetched catalog is cached in
     `%LOCALAPPDATA%\mouv\kim\catalog\`.
   - Fetching happens only on `kim catalog refresh` or when the user enabled launch-time checks.
   - A catalog is accepted only if its signature verifies against a public key compiled into
     Kim, and if it is not older than the cached one (rollback protection).
2. **Install layout (per user, no elevation)**:
   - `%LOCALAPPDATA%\Programs\mouv\<tool>\versions\<version>\…`: one directory per installed
     version;
   - `%LOCALAPPDATA%\Programs\mouv\<tool>\current.json`: a pointer to the active version,
     replaced atomically;
   - `%LOCALAPPDATA%\Programs\mouv\bin\<tool>.exe`: a small launcher, or a hard link refreshed
     on activation. Adding this directory to the user `PATH` is **asked**, never assumed.
3. **Transactional install and update**:
   - download to `staging\<id>.partial`, which is resumable and can be cancelled;
   - verify the size, SHA-256 and the signature of the release manifest;
   - unpack into `versions\<version>.staging`, then rename to `versions\<version>`;
   - run the tool's own `--version` self-check;
   - atomically switch `current.json`.

   Any failure before the switch leaves the previous version active, and the staging is
   removed. The previous version is kept for rollback until the user removes it.
4. **Running tools.** Before switching or uninstalling, Kim asks the tool whether it is running.
   For Subak, it asks whether a Host with live Terminals exists, via `subak host status --json`.
   If one does, Kim reports it and offers *defer* or *stop (with a restore snapshot)*. Kim never
   kills a tool silently (`KIM-FR-034`).
5. **Uninstall** removes program files only. User data (`%APPDATA%\mouv\<tool>`,
   `%LOCALAPPDATA%\mouv\<tool>`) is removed only with a separate, explicit confirmation
   (`KIM-SEC-005`).
6. **No background component**: no service, no scheduled task, no updater process. `kim.exe`
   runs, does what was asked, and exits (`KIM-NFR-001`).

## Consequences

- An interrupted install or update never corrupts the active version.
- Signing-key management and release publishing: Step 06 (`RELEASE_POLICY.md`) and Step 40.

## Alternatives considered

- **MSI/MSIX installers:** heavier, and system-wide by default, with elevation prompts. A Kim
  install could still be offered as MSIX later for users who want it.
- **winget as the installer:** it is an external dependency, and update behavior is not under
  Kim's consent rules. A winget manifest may be published as an *additional* channel (Step 40).

## Requirements

`KIM-FR-002…008`, `KIM-FR-034`, `KIM-SEC-001…005`, `KIM-NFR-001`, `KIM-NFR-004`.

## Notes

- 2026-11-04 (Step 05): catalog format, signature scheme (ed25519, domain-separated, key ids),
  source selection, staleness and rollback rules are specified in
  [CATALOG_FORMAT.md](../CATALOG_FORMAT.md). The first trusted key is a development key for
  preview catalogs; release-grade key custody follows in Step 06/40.
