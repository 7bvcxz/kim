# Product Requirements

Status: **current** · Owner: mouv · Derived from [KIM_PHILOSOPHY](KIM_PHILOSOPHY.md) and
[SUBAK_PHILOSOPHY](SUBAK_PHILOSOPHY.md)

## 0. How to read this document

- **IDs** are unique and permanent: `KIM-*` for the Kim manager and all tools, `SUBAK-*` for
  Subak. `FR` = functional, `NFR` = non-functional, `SEC` = security/privacy/safety. IDs are
  never renumbered or reused; a dropped requirement is marked *withdrawn*, not deleted.
- **Priority:** `P0` must ship in its milestone (milestone blocker) · `P1` should ship in its
  milestone · `P2` long-term aspiration, tracked but not promised.
- **Milestone:** `M1` Kim Foundation · `M2` Subak MVP · `M3` Parity expansion · `M4` Linux
  (see §1).
- **Verify:** `T` automated test · `M` manual check on real Windows · `I` inspection/review ·
  `D` documentation exists and is accurate.
- Requirements marked **NON-NEGOTIABLE** may not be relaxed by any ADR, setting default,
  feature flag or test shortcut.

## 1. Milestones

| Milestone | Goal | Done when |
| --- | --- | --- |
| **M1 – Kim Foundation** | Docs and engineering standards; architecture and compatibility specs; design system; Kim tool catalog; manual installer + Kim Manager GUI; headless Kim CLI | All `P0` `KIM-*` requirements with milestone M1 pass their verification |
| **M2 – Subak MVP** | A daily-usable persistent terminal on Windows: Host/Client, ConPTY sessions, headless CLI, tabbed GUI with floating icon, restore after reboot, Claude indicator | All `P0` `SUBAK-*` M2 requirements pass, including manual Windows verification |
| **M3 – Parity expansion** | Breadth: tmux command/key/format compatibility, Tabby-class features (profiles, SSH, file transfer), MCP servers | Tracked per item in the tmux and Tabby matrices |
| **M4 – Linux** | Linux builds of Kim and Subak with tested runtime support | Linux runtime tests pass on real Linux |

The immediate MVP is **M1 + M2**. tmux and Tabby *parity* is long-term (`P2`, M3); the M2 MVP
includes only the explicitly listed basic subset (`SUBAK-FR-033`, `SUBAK-FR-035`).

## 2. Target platforms

| Platform | Status |
| --- | --- |
| Windows 11, x86_64 | **Primary target** (M1, M2) |
| Windows 10 version 1809 or later, x86_64 (ConPTY minimum) | **Primary target** (M1, M2) |
| Windows on ARM64 | Planned, not tested, not claimed |
| Linux x86_64 (glibc) | Planned for M4. Before M4: portable *compile* checks only, no runtime claim |
| macOS | Out of scope (no plan) |

## 3. Kim requirements

### 3.1 Functional

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| KIM-FR-001 | Each Kim tool is a separately versioned, separately installable native binary; no tool requires another to be installed unless it declares that dependency explicitly. | P0 | M1 | T, I |
| KIM-FR-002 | Installing Kim installs, starts and schedules **no** other tool. | P0 | M1 | T, M |
| KIM-FR-003 | Kim shows a catalog of tools with id, name, description, version, platform/arch, license and support status, readable offline from a trusted local cache or bundled catalog. | P0 | M1 | T |
| KIM-FR-004 | The catalog states per tool whether it is *installed*, *available*, *update available* or *incompatible* with this machine. | P0 | M1 | T |
| KIM-FR-005 | On explicit request, Kim installs a tool: download → verify → stage → atomic install, with accurate progress, cancellation and retry. | P0 | M1 | T, M |
| KIM-FR-006 | On explicit request, Kim uninstalls a tool (data preservation per `KIM-SEC-005`). | P0 | M1 | T, M |
| KIM-FR-007 | On explicit request, Kim updates a tool; an interrupted or failed update leaves the previous working version installed and usable. | P0 | M1 | T |
| KIM-FR-008 | Kim can check for updates and display remote version and release notes; checking never downloads or installs binaries. | P0 | M1 | T |
| KIM-FR-009 | A small native Kim Manager GUI shows available vs installed tools, details, versions, progress, completion and errors, with explicit Install / Uninstall / Update buttons. | P1 | M1 | M |
| KIM-FR-010 | Every Kim capability is available from the CLI, with human-readable output and `--json` machine-readable output. | P0 | M1 | T |
| KIM-FR-011 | The CLI follows one contract across tools: discoverable `--help`, data on stdout and diagnostics on stderr, documented exit codes, stable structured error codes, `--quiet`/`--verbose`, `--dry-run` for state-changing commands, and no interactive prompts when non-interactive. | P0 | M1 | T, D |
| KIM-FR-012 | **NON-NEGOTIABLE.** Headless/CLI/agent paths never open a GUI window, tray icon, browser or interactive prompt. | P0 | M1 | T, I |
| KIM-FR-013 | Every state-changing GUI operation has an equivalent CLI command, and vice versa. | P1 | M1 | I |
| KIM-FR-020 | Installed tools are discoverable with their capabilities, and tools can invoke one another through documented CLI/JSON contracts — without a permanently running central orchestrator. | P1 | M1 | T, D |
| KIM-FR-021 | A tool may additionally expose a local, opt-in MCP server; MCP is never the only interface. | P2 | M3 | T, D |
| KIM-FR-034 | Updating or uninstalling a tool that is running (e.g. a Subak Host with live Terminals) never terminates it silently: Kim reports it and the user chooses to defer or to stop it. | P0 | M1 | T |

### 3.2 Non-functional

Budgets are initial targets; Step 03 defines how each is measured, and a budget may only be
changed by an ADR with measurements.

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| KIM-NFR-001 | **Idle CPU.** Kim CLI commands exit when done (no residency). A long-running Kim process with no activity averages < 0.1 % of one core over 60 s and does not poll faster than needed. | P0 | M1 | T, M |
| KIM-NFR-002 | **Memory.** Kim CLI peak working set < 30 MB for catalog operations; Kim Manager GUI idle < 80 MB. | P1 | M1 | M |
| KIM-NFR-003 | **Disk.** Kim CLI binary ≤ 15 MB; no writes while idle; logs capped (default 10 MB per tool). | P1 | M1 | M |
| KIM-NFR-004 | **Network.** Zero network connections unless the user invoked an operation that needs the network or enabled update checks. | P0 | M1 | T, I |
| KIM-NFR-005 | **Latency.** `kim catalog list` from cache completes in < 200 ms on a typical machine. | P1 | M1 | M |
| KIM-NFR-010 | Runs on the primary Windows targets in §2. | P0 | M1 | M |
| KIM-NFR-011 | OS-specific code sits behind narrow portable interfaces; Linux runtime support is not claimed before M4. | P0 | M1 | I |
| KIM-NFR-012 | **Accessibility.** Every GUI function is keyboard-operable with visible focus; text contrast ≥ 4.5:1 (WCAG AA); honors system reduced-motion and per-monitor DPI scaling. | P1 | M1 | M |
| KIM-NFR-013 | **Unicode.** CLI I/O is UTF-8; GUIs render Korean/CJK and fall back to system fonts for missing glyphs. | P0 | M1 | T, M |
| KIM-NFR-014 | **Contract stability.** CLI JSON output, IPC messages and stored state carry schema versions; breaking changes require a new major schema version plus migration/compatibility notes. | P0 | M1 | T, D |

### 3.3 Security, privacy and safety

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| KIM-SEC-001 | **NON-NEGOTIABLE.** Kim never installs, updates or removes any tool without an explicit user action naming that operation (GUI button press or CLI command). No background, scheduled, on-launch or "silent" install/update exists. | P0 | M1 | T, I |
| KIM-SEC-002 | **NON-NEGOTIABLE.** Update checks are optional and display-only, honor the offline/privacy setting, and are **off by default** at launch; `kim update check` remains available on demand. | P0 | M1 | T |
| KIM-SEC-003 | Every downloaded artifact is verified (SHA-256 and signature against a trusted key) before staging; any failure aborts with no change to installed state. | P0 | M1 | T |
| KIM-SEC-004 | Per-user installs by default; never elevate silently; elevation only on explicit request with an explanation. | P0 | M1 | T, I |
| KIM-SEC-005 | Uninstall preserves user data and configuration unless the user separately and explicitly confirms deleting it. | P0 | M1 | T |
| KIM-SEC-010 | No account, sign-in or cloud service is required for any functionality. | P0 | M1 | I |
| KIM-SEC-011 | No ads and no tracking. | P0 | M1 | I |
| KIM-SEC-012 | Telemetry is off by default. Diagnostics stay local, never contain terminal content, command lines, file paths, environment values or user names, and leave the machine only through an explicit user export. | P0 | M1 | I, T |
| KIM-SEC-013 | Secrets (tokens, passwords, keys, values marked secret) never appear in logs, JSON output, crash reports or plaintext state. | P0 | M1 | T, I |
| KIM-SEC-014 | A documented process exists for privately reporting security issues. | P0 | M1 | D |

## 4. Subak requirements

### 4.1 Host, Client and object model

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-001 | A per-user Host process owns all Terminals, pseudo-consoles, process trees, scrollback and session state. Only the Host starts, signals or kills terminal processes. | P0 | M2 | T, I |
| SUBAK-FR-002 | The Host starts on demand and runs only while it owns ≥ 1 live Terminal or a client is attached; starting at login happens only if the user enabled it. | P0 | M2 | T |
| SUBAK-FR-003 | A client closing, detaching, crashing or being killed never affects processes in the Host. | P0 | M2 | T, M |
| SUBAK-FR-004 | Multiple clients (GUI, CLI, agents) can be attached at the same time and observe consistent state. | P1 | M2 | T |
| SUBAK-FR-010 | Hierarchy Terminal → Window → Pane → process; every object has a stable, never-reused ID and an editable name; CLI and GUI use the same IDs. | P0 | M2 | T |
| SUBAK-FR-016 | Pane processes run under Windows ConPTY; each pane's process tree is contained (e.g. Job Object) so ending a pane ends all its descendants. | P0 | M2 | T, M |

### 4.2 Terminal CRUD, attach and detach

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-011 | **Create** a Terminal from a profile (shell, cwd, env, title) via GUI `Ctrl+T` and CLI. | P0 | M2 | T, M |
| SUBAK-FR-012 | **Read**: list Terminals/Windows/Panes with state and Claude status (human and JSON); capture pane text. | P0 | M2 | T |
| SUBAK-FR-013 | **Update**: rename; send input/keys; create/close Windows and Panes; resize. | P0 | M2 | T |
| SUBAK-FR-014 | **Delete**: kill a Terminal and its process trees, only on explicit request; the GUI confirms when any pane runs more than an idle shell; the CLI supports `--dry-run` and refuses busy Terminals without `--force` when non-interactive. | P0 | M2 | T, M |
| SUBAK-FR-015 | Attach/detach from CLI and GUI without affecting processes; closing the GUI window is a detach. | P0 | M2 | T, M |

### 4.3 GUI

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-030 | Optional tiny floating icon (or tray icon) showing aggregate status and opening the GUI; can be disabled. | P1 | M2 | M |
| SUBAK-FR-031 | Native tabbed GUI (one tab per Terminal) using the Kim design system, with correct Korean/CJK rendering. No Electron or embedded browser. | P0 | M2 | M, I |
| SUBAK-FR-032 | Session manager listing every Terminal in the Host, including ones not open as tabs. | P1 | M2 | M |
| SUBAK-FR-033 | Default keys: `Ctrl+T` new, `Ctrl+W` close (= delete, confirmed when busy), `Ctrl+Tab` / `Ctrl+Shift+Tab` next/previous tab, `Ctrl+B` prefix with the basic table `c n p 0-9 % " arrows x & z d , $` and `Ctrl+B Ctrl+B` for a literal `Ctrl+B`. | P0 | M2 | T, M |
| SUBAK-FR-034 | Every GUI shortcut is rebindable, and a shadowed key (e.g. `Ctrl+W`) can be passed through to the program. | P1 | M2 | T, M |
| SUBAK-FR-035 | Split panes and switch windows in the GUI. | P1 | M2 | M |

### 4.4 Persistence, restore and Claude resume

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-040 | Persist structure, names, layout, cwd, profile and non-secret environment on change, atomically. | P0 | M2 | T |
| SUBAK-FR-041 | After reboot, logoff or Host crash, offer to restore the structure with new processes; restored objects are marked `restored`. | P0 | M2 | T, M |
| SUBAK-FR-042 | Optional (off by default) scrollback persistence, shown after restore as read-only history separated from new output. | P1 | M2 | T, M |
| SUBAK-FR-043 | A restored pane is never presented as the original process. | P0 | M2 | I, M |
| SUBAK-FR-044 | Sleep, hibernate, lock and Remote Desktop disconnect never trigger restore or create duplicate Terminals. | P0 | M2 | T, M |
| SUBAK-FR-045 | Opt-in Claude resume: on restore, a pane that ran Claude Code with a known session ID runs `claude --resume <id>`. | P1 | M2 | T, M |
| SUBAK-FR-046 | Configuration is human-readable and versioned; unknown keys are preserved; an invalid config falls back to defaults with a clear error, never a crash. | P0 | M2 | T |

### 4.5 Claude status indicator

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-050 | Each pane running Claude Code has one of five states — `idle`, `in-progress`, `need-input`, `error`, `done` (unseen completion; becomes `idle` when viewed) — or `unknown` when evidence is missing or stale; panes without Claude are `none`. | P1 | M2 | T |
| SUBAK-FR-051 | States aggregate to Window, tab, session manager and floating icon with priority `need-input` > `error` > `done` > `in-progress` > `unknown` > `idle` > `none`, each with a text label (not color alone). | P1 | M2 | T |
| SUBAK-FR-052 | State detection is local and never stores or transmits conversation content. | P0 | M2 | I |
| SUBAK-FR-053 | Claude state is queryable through the CLI as JSON. | P1 | M2 | T |

### 4.6 Long-term parity and platforms

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-FR-060 | Aspire to full tmux compatibility (session/window/pane commands, key table, `send-keys`, `capture-pane`, format strings); each item tracked as implemented/planned/unsupported with evidence. | P2 | M3 | D, T |
| SUBAK-FR-061 | Aspire to Tabby functional parity (profiles, SSH, file transfer, themes, search); tracked per feature with evidence. | P2 | M3 | D, T |
| SUBAK-FR-062 | Optional local MCP server for Subak (list, create, send keys, capture, Claude state). | P2 | M3 | T |
| SUBAK-FR-070 | Linux Host and Clients. | P2 | M4 | T, M |

### 4.7 Security and safety

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| SUBAK-SEC-001 | **NON-NEGOTIABLE.** Recovery never kills, replaces or duplicates a live session. If a Host is running, restore is not offered for Terminals it still owns. | P0 | M2 | T |
| SUBAK-SEC-002 | **NON-NEGOTIABLE.** Restore never re-runs arbitrary commands automatically; only per-pane opt-in restart commands and opt-in Claude resume are executed. | P0 | M2 | T, I |
| SUBAK-SEC-003 | **NON-NEGOTIABLE.** State writes are atomic; a backup is taken before any migration or restore; a failed restore leaves the previous state intact. | P0 | M2 | T |
| SUBAK-SEC-010 | Client–Host IPC is local and restricted to the same user; no network listener by default. | P0 | M2 | T, I |
| SUBAK-SEC-011 | Persisted state and scrollback are stored with user-only permissions; variables marked secret are not persisted. | P0 | M2 | T, I |
| SUBAK-SEC-012 | Remote/agent input injection is limited to the same user through the local IPC; any remote control requires explicit enablement. | P0 | M2 | T, I |

## 5. Data privacy and safety policies

1. **Local-first.** All data (catalog cache, configuration, session state, scrollback, logs)
   stays on the user's machine in per-user locations.
2. **Network.** The only network operations are: catalog refresh, update check, and downloads
   for an install/update the user requested — each initiated or enabled by the user.
   (`KIM-NFR-004`, `KIM-SEC-002`)
3. **No collection.** No telemetry, analytics, crash upload, account or advertising ID.
   (`KIM-SEC-010…012`)
4. **Secrets.** Never logged, never in JSON, never persisted in plaintext.
   (`KIM-SEC-013`, `SUBAK-SEC-011`)
5. **Consent for change.** Installing, updating, uninstalling, deleting user data, killing a
   Terminal, and re-running commands on restore each require an explicit user action.
   (`KIM-SEC-001`, `KIM-SEC-005`, `SUBAK-FR-014`, `SUBAK-SEC-002`)
6. **Non-destructive recovery.** Recovery is additive and reversible. (`SUBAK-SEC-001…003`)

## 6. Out of scope for M1 (Kim Foundation)

- Any Subak runtime functionality: Host, ConPTY, terminal GUI, floating icon, restore,
  Claude indicator (all M2).
- tmux/Tabby parity work and MCP servers (M3).
- Linux runtime support (M4); macOS entirely.
- Automatic or background updates of any kind (never in scope).
- Accounts, cloud sync, plugin marketplaces, paid features.
- Authenticode code-signing certificate procurement (artifact signatures for the catalog are in
  scope via `KIM-SEC-003`).
