//! Ddalgak's visual rule editor (Step 74; docs/DDALGAK_GUI_JOURNEYS.md, ADR 0049).
//!
//! `ddalgak-editor.exe` is a separate, on-demand process: the tray app (`ddalgak-gui.exe`)
//! starts it from its menu or the `editor.open` shortcut, and the user can start it directly.
//! It never runs the engine, installs a hook, injects input, opens a program or a website: it
//! edits `rules.json` through the rule store exactly like the CLI ([`repo`]) and asks the
//! running app, over the same identity-checked pipe as the CLI, to load what it saved
//! ([`sync`]). Closing the window ends the process and frees its memory; the engine's idle cost
//! does not change (ADR 0049).
//!
//! - [`form`]: one rule as a sentence, the eight effect tiles and their forms on the engine's
//!   own data models, the simulation for **Test**;
//! - [`listing`]: rows, badges, search, filters and grouping;
//! - [`capture`]: key capture; [`shortcuts`]: the rule list's shortcuts from `hotkeys.json`;
//! - [`transfer`]: export and import hooks (imports arrive turned off, without consent);
//! - [`app`]: the window; [`theme`]: the Kim design tokens; [`i18n`]: Korean and English.

pub mod app;
pub mod capture;
pub mod form;
pub mod i18n;
pub mod listing;
pub mod repo;
pub mod shortcuts;
pub mod sync;
pub mod theme;
pub mod transfer;

pub use app::{EditorApp, Options};

/// The editor's file name; the tray app starts it from its own folder.
pub const EDITOR_EXE: &str = "ddalgak-editor.exe";
