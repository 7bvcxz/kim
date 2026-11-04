# ADR 0028 — Ddalgak rule store: run-time schema per rule, locked atomic writes, drafts and live reload

Status: **Accepted** · Date: 2026-11-04 · Step 67 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
§10 (who checks rules) and [DDALGAK_RULE_MODEL §2](../DDALGAK_RULE_MODEL.md) (what "fails the
schema" means for loading) · Builds on [ADR 0006](0006-state-and-configuration-storage.md),
[ADR 0015](0015-cli-contract-and-tool-discovery.md), [ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md)

## Context

Step 67 turns `rules.json` from a file the engine only reads into a store that the CLI (and later
the editor) changes. Several decisions were open or in tension:

- DDALGAK_RULE_MODEL §2 says a file that fails the schema is never loaded, while
  `DDALGAK-FR-006` also says a rule with an error is inactive and the others keep working.
  Because the schema checks single rules (reserved triggers, missing consent, broken steps), the
  literal reading turns one bad hand edit — or a trigger kind a newer Ddalgak added — into "no
  rules at all", and Step 66 had already shipped per-rule inactivity for trigger problems.
- ADR 0022 §10 made `xtask/src/ddalgak.rs` a reference that Step 67 must agree with, without
  saying how agreement is kept.
- The CLI must work without the app, two CLIs may run at once, a crash may hit any write, and the
  running app must pick up changes "within one file-change notification" (`DDALGAK-FR-009`)
  without a stuck hook or dropped rules.
- Saved, active and unsaved rules must be distinguishable (`DDALGAK-FR-052`), also headlessly.

## Decision

1. **The published schema runs inside Ddalgak.** The crate compiles in
   `docs/contracts/schemas/ddalgak-rules-v1.schema.json` and the validator of
   `cargo xtask schema-check` (`xtask/src/jsonschema.rs`, included as a module, as the CLI tests
   already did). This makes `regex` a run-time dependency of `ddalgak` (features limited to what
   the schema's patterns use). The validator now matches with its cached compiled patterns instead
   of clones, which made validation about four times faster; 1000 rules load, validate and compile
   in 33–34 ms in a release build (`DDALGAK-NFR-004`: < 100 ms with the hook).
2. **The schema is applied in two parts.** The document *outside its rules* (structure,
   profiles, modes, settings, at most 1000 rules per profile) decides whether the file loads: a
   violation there, invalid JSON, a missing active profile or default mode, or a rule that cannot
   be named (no valid `id`, no boolean `enabled`) means `rules_invalid` — not loaded, never
   rewritten automatically, a running engine keeps its previous rules. **Each rule is checked on
   its own**: a violation makes only that rule inactive, with the §8 code of the part that is
   wrong (`trigger_reserved`, `trigger_invalid`, `output_reserved`, `step_invalid`,
   `recursion_risk`, `consent_missing`, `rule_invalid`). The split gives the same verdict as the
   whole rule schema (a test compares both on every fixture and on mutations). A rule that fails
   the schema is kept exactly as written and claims no trigger, so it can never deactivate a valid
   rule as a duplicate.
3. **Agreement with the reference is tested, not hoped for.** `validate::semantic_errors` is a
   line-by-line port of the xtask checker; Ddalgak's tests compile the xtask file itself and
   compare both on every fixture and on generated mutations. The runtime extends the reference
   with checks only it can make (unknown fields and step types, conditions, Ddalgak's own
   shortcuts, shadowed and overridden triggers, hidden sequences, output cycles, chord warnings,
   missing program files). New codes are additive (§9): `duplicate_binding` (error),
   `trigger_shadowed`, `sequence_prefix`, `target_missing` (warnings), `trigger_overridden`,
   `output_cycle` (infos).
4. **A change that adds an error is refused.** Every CLI change validates the document before
   and after; any error the change introduces refuses it with nothing written (`rule_conflict`
   with offers for triggers, `hotkey_invalid`/`hotkey_reserved`, `rule_invalid`); existing
   problems elsewhere do not block an unrelated change. Offers follow the hotkey contract (§9.5)
   and are never applied by themselves. A new rule that lacks the consent it needs is saved
   disabled (`consent_required`); editing or enabling into missing consent is refused; revoking a
   needed consent disables the rule; copies are created disabled.
5. **Writes: one OS file lock, atomic replace, backup, history.** Writers take an exclusive
   `LockFileEx` lock on `rules.json.lock` (std `File::try_lock`), re-read the file, check the
   revision the caller started from (`--if-revision`; `rule_conflict`), copy the current file to
   `rules.json.bak`, write `rules.json.tmp`, flush it and rename it over `rules.json`, then keep a
   snapshot in `history/` (the last 50). The OS releases the lock of a crashed or killed writer,
   so no stale-lock recovery exists or is needed; a waiting writer gives up after 10 s with `busy`.
   A crash leaves the old or the new file, never a torn one; a leftover `rules.json.tmp` is shown
   as `interrupted_save` and replaced by the next save. A restore over an unusable file keeps that
   file as `rules.corrupt-<time>.json` and the backup untouched.
6. **Drafts are a file.** `--draft` edits `rules.draft.json`, a whole document whose `revision`
   is the saved revision it started from; `rules draft apply` saves it only if the saved rules
   did not change since (`rule_conflict` otherwise). The editor (Step 74) can use the same file to
   survive a crash.
7. **Live reload has two triggers and one rule.** The app watches its configuration folder with
   `FindFirstChangeNotificationW` on a waiting thread (no polling, no wake-ups while nothing
   changes), and the CLI asks over the existing pipe after each saved change (`rules.reload`), so
   it can report whether the active rules now equal the saved ones. Either way the app re-reads
   and compares the bytes: a usable, changed file becomes the active rules at once — with the
   engine on, a new table is swapped into the running hook between two callbacks, its consumed
   key pairs kept, and the worker drops signals of the old table — while an unusable file keeps
   the previous active rules (`kept_previous`, one tray notice per bad version). `status` shows
   the saved file, the active set, the draft and the last reload separately.

## Consequences

- One broken rule never silences the others, and an older Ddalgak keeps a newer trigger kind
  inactive instead of dropping the whole file. The price is that the §2 wording changes: "fails
  the schema" now means "fails it outside the rules" for loading.
- `schema-check` and the runtime cannot drift: same schema file, same validator, a tested port
  of the cross-references. Changing the schema changes the app, which is intended.
- The shipped binaries grow with the `regex` engine, the typed model and the rule commands
  (release builds on Windows, measured in Step 67: `ddalgak.exe` 854 KB → 3.1 MB,
  `ddalgak-gui.exe` 994 KB → 2.5 MB); still small native binaries, well below the platform's
  15 MB CLI budget (`KIM-NFR-003`).
- A rule that fires exactly while a reload swaps the table may be dropped; keys already consumed
  stay paired, so nothing sticks. Held outputs of remaps arrive in Step 68, which must keep them
  with the rule that pressed them across a reload.
- Hand edits are still welcome; they are validated on the next read and never rewritten.

## Alternatives considered

- **Hand-written validation in Rust instead of the schema at run time.** No `regex` in the
  binaries, but a second implementation of ~30 patterns and the step `oneOf`s that could drift
  from the published schema. Rejected; the fixtures would only sample the agreement.
- **Literal "a schema failure anywhere means not loaded".** Simple and loud, but one bad rule (or
  an additive change from a newer version) disables everything, against `DDALGAK-FR-006`'s "the
  others keep working" and against Step 66's shipped behavior. Rejected.
- **A lock file created exclusively (`create_new`) with a PID inside.** Works without OS locks
  but needs stale-lock detection after a crash, which is exactly the case Step 67 must survive.
  Rejected for the OS lock that dies with its process.
- **Optimistic concurrency only (revision check, no lock).** Two simultaneous CLIs would make one
  of them fail although their changes do not conflict. Rejected; the revision check stays for
  callers that read earlier (`--if-revision`, the editor).
- **Reload only on IPC, or only on notifications.** IPC alone misses hand edits; notifications
  alone cannot tell the CLI whether the app applied the change. Both are used.
- **Drafts only in the editor's memory.** Not visible headlessly and lost in a crash. Rejected.

## Requirements

`DDALGAK-FR-005`, `DDALGAK-FR-006`, `DDALGAK-FR-007`, `DDALGAK-FR-009`, `DDALGAK-FR-021`,
`DDALGAK-FR-031`, `DDALGAK-FR-052`, `DDALGAK-FR-060`, `DDALGAK-NFR-004`, `DDALGAK-NFR-008`,
`DDALGAK-SEC-004`, `DDALGAK-SEC-006`, `DDALGAK-SEC-009`.
