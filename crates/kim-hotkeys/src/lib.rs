//! Shared hotkey library for Kim tools (docs/HOTKEY_CONTRACT.md v1, ADR 0017).
//!
//! A library compiled into each tool, never a process: no daemon, no shared hook, no GUI
//! dependency (ADR 0002, `KIM-NFR-020`). It provides the canonical key syntax, the tool's hotkey
//! manifest, the sparse user layer `hotkeys.json` with safe mode and migrations, layering,
//! validation against the reserved keys, conflict detection with deterministic reports and
//! non-destructive remap offers, the `set`/`disable`/`enable`/`reset` workflow, and a narrow
//! Windows registration backend for global chords that never steals a key (`KIM-SEC-020`).

pub mod keys;
