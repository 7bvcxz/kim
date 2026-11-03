# AGENTS.md — Repository working rules

These rules apply to every contributor, human or AI (Claude Code and other coding agents), for
the whole `7bvcxz/kim` repository. When a rule here conflicts with a convenience, the rule wins.
When a rule conflicts with an explicit, documented decision record (ADR), follow the ADR and
update this file in the same change.

## 1. Read before you change

1. Read [docs/README.md](docs/README.md) and the documents it marks **current** that touch your
   change: at minimum [KIM_PHILOSOPHY](docs/KIM_PHILOSOPHY.md),
   [SUBAK_PHILOSOPHY](docs/SUBAK_PHILOSOPHY.md) and
   [PRODUCT_REQUIREMENTS](docs/PRODUCT_REQUIREMENTS.md).
2. Inspect the actual repository state. Never assume a planned file, crate or command exists;
   the docs index says what is current and what is only planned.
3. For a GitHub issue, read the issue and the earlier Step issues it depends on.

## 2. Non-negotiable product rules

These restate requirements from [PRODUCT_REQUIREMENTS](docs/PRODUCT_REQUIREMENTS.md); never
weaken them in code, defaults, tests or docs.

- **No automatic installs or updates — ever** (`KIM-SEC-001`, `KIM-SEC-002`). Installing,
  removing or updating any tool requires an explicit user action. "Checking" for updates may
  only display information and must respect offline/privacy settings.
- **Headless means headless** (`KIM-FR-012`). A CLI/headless/agent code path must never open a
  window, tray icon, browser or prompt.
- **Session recovery is non-destructive** (`SUBAK-SEC-001`…`SUBAK-SEC-003`). Recovery never
  kills a live session, never overwrites state without a backup, and never re-runs an arbitrary
  command without explicit opt-in.
- **Local-first and private.** No account requirement, no ads, no telemetry or network calls
  that the user did not opt into. Secrets never appear in logs or JSON output.
- **No Electron, no mandatory always-on daemon, no monolithic runtime.** Each Kim tool is an
  independent native Rust binary. Subak's Host is a per-user process that runs only while it
  owns sessions or the user asked it to run.

## 3. Engineering rules

- **Small, focused, complete changes.** Deliver the smallest change that fully satisfies the
  task, with real behavior and real tests. Preserve existing working behavior.
- **No fake completeness.** Never present a mock, stub, placeholder or TODO as a finished
  feature. Placeholders must say so in code, `--help`, and docs.
- **Stable CLI contract.** Every command supports human output and `--json`; uses documented
  exit codes; writes data to stdout and diagnostics to stderr; and supports `--dry-run` for
  state-changing operations. The binding rules, error-code registry and schemas are in
  [docs/CLI_CONTRACT.md](docs/CLI_CONTRACT.md); a new command or error code updates that
  document, the JSON schemas and the golden examples in the same change.
- **Portable boundaries, Windows first.** Put OS-specific code behind narrow interfaces so Linux
  can be added later; do not implement Linux support prematurely, and never claim it.
- **Verification.** Run formatting, lint, build and tests relevant to the change, plus a manual
  smoke check of the real behavior on Windows when applicable. If something cannot be
  verified (hardware, OS, network), say **not tested** explicitly and give reproducible manual
  steps.
- **Decisions are recorded.** A change that contradicts or extends an earlier decision needs an
  ADR (`docs/adr/`, planned) explaining the tradeoff — never shift scope silently.
- **Docs move with code.** Update the philosophy, requirements, architecture, CLI, security,
  compatibility and recovery docs that your change affects, and the status column in
  [docs/README.md](docs/README.md).
- **Requirement traceability.** Reference requirement IDs (e.g. `SUBAK-FR-010`) in tests, ADRs
  and commit messages when a change implements or affects them.

## 4. Development cadence, tests and e2e (maintainer rules)

These rules come directly from the maintainer and are mandatory.

1. **Commit and push often.** Commit *and push* every time a Step issue is completed **and**
   every time a self-contained development item inside it is completed. Never sit on unpushed
   finished work.
2. **Every feature ships with its tests.** When a feature is developed, write tests for it in
   the same change and run them before committing (unit/integration tests run by the normal
   test command).
3. **Every feature is added to the e2e suite.** Each feature also gets an end-to-end scenario
   in the e2e suite (`e2e/`), exercising the real binaries the way a user or agent would.
4. **The e2e suite never runs routinely.** It must not run as part of the normal test command,
   CI on push/PR, pre-commit hooks, or ad-hoc verification. It runs **exactly once per 15
   processed Step issues** (after the 15th, 30th, 45th … Step issue is processed).
5. **Keep the ledger.** [e2e/LEDGER.md](e2e/LEDGER.md) records every processed Step issue and
   every e2e run. When a Step completes, append it; when the count since the last e2e run
   reaches 15, run the e2e suite once, record the result, and report it in that Step's
   completion comment. Failures found by the e2e run are fixed (or filed as issues) before
   the next Step starts.

## 5. Working on this machine (agents)

- Work headlessly. The maintainer uses the same computer while agents work: do not open GUI
  windows, browsers, or focus-stealing processes; use headless test modes.
- Do not install system-wide software or trigger elevation (UAC) without asking first.

## 6. Git and GitHub workflow

- Branch per issue (`step-NN-short-name` or `issue-N-short-name`); the very first commit of the
  repository is the only exception.
- Commit messages: imperative summary, then a body that references the issue
  (`Refs #N`) and the requirement IDs touched.
- Open a pull request, let CI pass, merge with a merge commit so reported SHAs stay valid.
- After completing an issue, post a **Korean completion report** as a comment on that issue
  with the headings `구현 내용`, `설계 및 주요 결정`, `변경 파일`, `테스트 및 실제 결과`,
  `커밋 및 Push`, `남은 문제와 다음 Step`, giving concrete evidence (commands, outputs, SHAs).
- Close an issue only when its acceptance criteria genuinely pass. If they do not, leave it open
  and say what is missing.
