# ADR 0052 — Ddalgak profiles: an atomic switch, versions from the history and inert, portable transfer

Status: **Accepted** (Step 75; the visible profile browser pending the maintainer) · Date:
2026-11-04 · Builds on [ADR 0022](0022-ddalgak-rule-model-and-safety.md),
[ADR 0028](0028-ddalgak-rule-store-validation-and-reload.md),
[ADR 0043](0043-ddalgak-modes-layers-and-runtime-mode-state.md),
[ADR 0046](0046-ddalgak-enable-disable-and-recovery.md),
[ADR 0049](0049-ddalgak-editor-process-and-toolkit.md)

## Context

DDALGAK_RULE_MODEL §10 already specified what profiles are (one active, a switch releases
everything, history snapshots, export without consent, import disabled) but not how a switch
reaches a running hook, what a "version of a profile" is when the store snapshots whole
documents, or how an export stays portable and free of secrets. Step 75 also had to keep the
rule store's single write path (lock, re-read, revision, review, atomic save; ADR 0028) and the
editor's confirmed reload (ADR 0049).

## Decision

1. **A switch is a reload with a different `active_profile`.** `profile apply` writes only that
   field through the rule store. The running app notices that the profile changed and, instead of
   the reload's carry-over, swaps the table with `Core::switch_table` between two hook callbacks:
   pending taps, holds, sequences and undecided dual roles are dropped, the worker cancels runs
   and releases every output (`Msg::Cancel`), lifted trigger modifiers still held are restored,
   **keys whose down was consumed keep their up consumed**, and the new profile starts in **its
   default mode** — never a mode carried over by ID, even when both profiles have one. The reload
   answer names the profile (`active_set.profile`), so the CLI reports `switched` and the editor
   shows a pending switch until then. One atomic file write plus one table swap is the whole
   switch: a crash leaves the old or the new profile, never a mixture.
2. **Versions come from the existing history.** A version of a profile is that profile in a
   history snapshot; consecutive identical ones collapse. `profile save` adds **save points**:
   labelled snapshots in `savepoints/`, apart from the automatic history (which never prunes them;
   the last 100 are kept). A version is **working** when the profile had no error in it;
   `profile rollback` restores the newest working version that differs from now. Restore and
   rollback replace one profile as a new change (itself restorable), never switch, and rename a
   rule ID another profile took meanwhile.
3. **Transfer stays `ddalgak.rules` v1.** An export is the document with an `exported` block,
   every rule off, without consent, settings or revision (as §10 said), program paths and
   working folders under a known folder written as tokens (`%LocalAppData%\…`), and the values of
   `open_program` environment overrides **withheld** (`exported.withheld: ["env_values"]`) unless
   `--include-env-values`. An import validates and migrates the file with the store's own
   `interpret` (a newer `schema_version` is refused, unknown fields are kept), restores modes the
   file refers to but lacks (empty, so rules keep their limits), renames clashing profile and rule
   IDs, and previews permissions, exact launch targets (only on request in the CLI), missing
   programs, duplicate triggers, collisions with Ddalgak's own shortcuts and codes that would keep
   a rule inactive. It adds profiles **never active, every rule off, no consent**; problems of the
   imported rules themselves do not refuse it (they stay inactive and are listed), anything else
   it would break does.
4. **One profile command set for agents and people.** `ddalgak profile
   list|show|add|edit|rename|duplicate|delete|apply|save|history|restore|rollback|export|import`
   with `--json`, `--dry-run` (the preview of apply and import) and confirmations for delete,
   restore and rollback; new codes `profile_in_use` (7) and `import_invalid` (4). The editor's
   Profiles page offers the same operations with a side-by-side diff before a switch or restore.
5. **User-controlled defaults** are the active profile, which persists across restarts and is
   changed only by the user (apply, restore never switches, import never switches), and each
   profile's default mode (Step 72).

## Consequences

- Nothing new runs in the hook: a switch costs one table swap like a reload.
- A version list reads up to 150 small files on demand; fine for the editor and the CLI.
- Withholding environment values means an imported program rule may need its values typed
  again; the import preview and the rule's consent dialog make that visible.
- Per-app automatic profile switching ("optional per-app contexts") is **not** built: conditions
  per rule (Step 77) cover it without switching profiles behind the user's back.

## Alternatives considered

- **Per-profile files.** Easier versioning, but two files would have to change atomically for a
  switch and every reader would change. Rejected.
- **Carry the mode over by ID on a switch** (what a reload does). Rejected: two profiles with a
  `nav` mode would leave the user in a layer of the other profile.
- **Export environment values by default.** Rejected: they are often tokens (`DDALGAK-SEC-011`).
- **Refuse an import with any rule problem.** Rejected: one newer step type would block a whole
  profile; the problem rules arrive off and named instead (`DDALGAK-FR-006`).

## Requirements

`DDALGAK-FR-030`, `DDALGAK-FR-031`, `DDALGAK-FR-032`, `DDALGAK-FR-060`, `DDALGAK-SEC-005`,
`DDALGAK-SEC-006`, `DDALGAK-SEC-011`, `DDALGAK-SEC-013`, `DDALGAK-NFR-005`, `DDALGAK-NFR-008`.
