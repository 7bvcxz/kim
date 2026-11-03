# ADR 0001 — Record architecture decisions

Status: **Accepted** · Date: 2026-11-03 · Step 03

## Context

Kim and Subak are built step by step by humans and coding agents over many issues. Decisions
made early (process model, IPC, storage) constrain everything after them. AGENTS.md requires
that scope changes and tradeoffs are documented, never silently shifted.

## Decision

- Significant decisions are recorded as numbered Markdown files in `docs/adr/`, indexed in
  [README.md](README.md).
- Template: **Status**, **Context**, **Decision**, **Consequences**, **Alternatives
  considered**, **Requirements** (IDs from [PRODUCT_REQUIREMENTS](../PRODUCT_REQUIREMENTS.md)).
- Statuses: Proposed → Accepted → (Superseded by NNNN | Deprecated). Accepted ADRs are not
  rewritten; corrections of fact may be appended with a dated note.
- A decision that a later Step must validate on real Windows says so in its status.

## Consequences

Future contributors can see why the system looks the way it does and which assumption to
re-check when something fails.

## Alternatives considered

- Decisions only in issue comments: not searchable from the repository, easily lost.
- One large design document: hard to supersede a single decision cleanly.
