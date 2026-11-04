//! Ddalgak: a native, Windows-first automation engine (docs/DDALGAK_PHILOSOPHY.md, ADR 0022,
//! ADR 0024).
//!
//! Step 66 provides the **engine**: the normalized event model ([`event`]), the trigger syntax
//! ([`trigger`]), compiled trigger tables ([`table`]), the allocation-free decision core that runs
//! inside the low-level keyboard hook ([`core`]), the worker thread ([`worker`]), the lifecycle
//! with its crash and emergency markers ([`lifecycle`]), injection with a held-key ledger
//! ([`inject`]), Ddalgak's own global shortcuts ([`shortcuts`]), the read-only runtime view of
//! `rules.json` ([`rules`]; the rule store is Step 67), the CLI ↔ app protocol ([`ipc`]), opt-in
//! start at sign-in ([`startup`]) and, on Windows, the platform layer ([`win`]): the hook thread,
//! `SendInput`, the identity-checked pipe, the single-instance lock, context watchers, the tray
//! icon and the engine host that `ddalgak-gui.exe` runs.
//!
//! Two binaries use it: `ddalgak` (headless CLI; it never installs a hook, opens a window or a
//! tray icon, or starts the app) and `ddalgak-gui` (the tray app that owns the engine).
//! Linux is not supported before milestone D7 (`DDALGAK-NFR-009`); the portable modules compile
//! and are tested everywhere.

pub mod core;
pub mod event;
pub mod inject;
pub mod ipc;
pub mod lifecycle;
pub mod paths;
pub mod rules;
pub mod shortcuts;
pub mod startup;
pub mod stats;
pub mod table;
pub mod time;
pub mod trigger;
pub mod vk;
pub mod worker;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod win;

/// The tool ID (catalog, hotkey contract, CLI contract).
pub const TOOL: &str = "ddalgak";
/// The package version (both binaries).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The tray app's file name; the CLI verifies that the pipe is served by it.
pub const GUI_EXE: &str = "ddalgak-gui.exe";
