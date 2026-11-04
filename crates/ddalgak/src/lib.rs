//! Ddalgak: a native, Windows-first automation engine (docs/DDALGAK_PHILOSOPHY.md, ADR 0022,
//! ADR 0024).
//!
//! Step 74 adds the **rule editor** as its own on-demand process (`ddalgak-editor.exe`, package
//! `ddalgak-editor`, ADR 0049): the tray app opens it from its menu and the `editor.open`
//! shortcut ([`win::host`]); the editor and the CLI share the identity-checked pipe client
//! ([`win::client`]).
//!
//! Step 73 adds **enable, disable, pause and recovery**: the user's turn-off survives restarts
//! ([`lifecycle`]), per-profile and per-mode `enabled` (`profile_disabled`, `mode_disabled`),
//! stale-key release after a crash and on request and the hook-timeout check ([`recovery`]), and
//! the status indicator model for the tray and the optional floating badge ([`indicator`];
//! ADR 0046).
//!
//! Step 72 adds **user-defined modes and layers** ([`modes`]: exclusive modes and layers over a
//! parent, set, toggle, cycle, hold, previous and default run inside the hook callback, one-shot
//! and timed exits, per-mode precedence, every held output released on a switch, the indicator
//! and menu data; [`mode_edit`]: mode CRUD on the rules document and the editor's Modes page;
//! ADR 0043).
//!
//! Step 71 adds **mouse actions** ([`mouse`]: moves in logical pixels or grid cells and to points
//! across monitors with per-monitor DPI, normalized absolute `SendInput` moves that never leave
//! the desktop, the wheel, clicks, holds on the remap ledger, motions while a key is held, a
//! simulated pointer for the playground and the tests; ADR 0040).
//!
//! Step 70 adds **program and website launches** ([`launch`]: `CreateProcessW` with a quoted
//! argument list, the shell's `open` for `http`/`https` and packaged apps, never a shell string or
//! elevation; a launch thread per run; the editor's program picker and website field; ADR 0037).
//!
//! Step 68 adds **remaps and blocks** ([`remap`]; the core's remap state machine, the worker's
//! output, the optional low-level mouse hook for mouse-button triggers; ADR 0031).
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
pub mod edit;
pub mod event;
pub mod hotstring;
pub mod indicator;
pub mod inject;
pub mod ipc;
pub mod launch;
pub mod lifecycle;
pub mod mode_edit;
pub mod model;
pub mod modes;
pub mod mouse;
pub mod paths;
pub mod recovery;
pub mod remap;
pub mod rules;
pub mod run;
pub mod schema;
pub mod shortcuts;
pub mod startup;
pub mod stats;
pub mod store;
pub mod table;
pub mod text;
pub mod time;
pub mod trigger;
pub mod validate;
pub mod vk;
pub mod worker;

/// The JSON Schema validator of `cargo xtask schema-check` (ADR 0015), compiled into Ddalgak so
/// rules are checked by the very same code as the contract examples (ADR 0028).
#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code, unused_qualifications, missing_debug_implementations)]
pub(crate) mod jsonschema;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod win;

/// The tool ID (catalog, hotkey contract, CLI contract).
pub const TOOL: &str = "ddalgak";
/// The package version (both binaries).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The tray app's file name; the CLI verifies that the pipe is served by it.
pub const GUI_EXE: &str = "ddalgak-gui.exe";
/// The rule editor's file name (Step 74, ADR 0049): a separate on-demand process beside the tray
/// app, built by the `ddalgak-editor` package so neither binary here links a GUI toolkit.
pub const EDITOR_EXE: &str = "ddalgak-editor.exe";

/// Where the editor is for a tray app at `gui`: the same folder.
pub fn editor_path(gui: &std::path::Path) -> std::path::PathBuf {
    gui.parent().map_or_else(
        || std::path::PathBuf::from(EDITOR_EXE),
        |d| d.join(EDITOR_EXE),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_editor_sits_beside_the_tray_app() {
        let p = super::editor_path(std::path::Path::new(
            r"C:\Programs\mouv\ddalgak\ddalgak-gui.exe",
        ));
        assert_eq!(
            p,
            std::path::Path::new(r"C:\Programs\mouv\ddalgak\ddalgak-editor.exe")
        );
    }
}
