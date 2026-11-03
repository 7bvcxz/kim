//! Shared hotkey library for Kim tools (docs/HOTKEY_CONTRACT.md v1, ADR 0017).
//!
//! A library compiled into each tool, never a process: no daemon, no shared hook, no GUI
//! dependency (ADR 0002, `KIM-NFR-020`). It provides the canonical key syntax ([`keys`]), the
//! tool's hotkey manifest ([`manifest`]), the sparse user layer `hotkeys.json` ([`layer`]) with
//! safe mode, atomic writes, backups and migrations ([`store`]), layering, validation against the
//! reserved keys ([`reserved`]), conflict detection with deterministic reports and
//! non-destructive remap offers ([`check`]), the `set`/`disable`/`enable`/`reset` workflow
//! ([`edit`]), the `hotkeys list|show` views ([`view`]), and a narrow Windows registration
//! backend for global chords that never steals a key ([`register`], `KIM-SEC-020`).

pub mod check;
pub mod edit;
pub mod keys;
pub mod layer;
pub mod manifest;
pub mod register;
pub mod reserved;
pub mod scope;
pub mod store;
pub mod view;

pub use check::{Analysis, Code, Conflict, Severity, ToolState, analyze};
pub use keys::{Chord, Sequence, Step};
pub use layer::UserLayer;
pub use manifest::Manifest;
pub use scope::Scope;

/// `KIM_NO_HOTKEYS` (any value except empty, `0` or `false`) starts a GUI with every shortcut
/// off, without touching the file (§8.3 4, `KIM-SEC-023`).
pub fn no_hotkeys_env() -> bool {
    std::env::var_os("KIM_NO_HOTKEYS").is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
}
