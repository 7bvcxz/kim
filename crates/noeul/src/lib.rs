//! Noeul (노을): a small native screenshot and floating-pin tool for Windows (pre-alpha,
//! Step 49 foundation). See docs/NOEUL_PHILOSOPHY.md, docs/NOEUL_REQUIREMENTS.md and
//! docs/adr/0023-noeul-app-foundation.md.
//!
//! Modules, from the bottom up:
//!
//! | Module | What |
//! | --- | --- |
//! | [`frame`] | Owned capture-image buffers and their accounting |
//! | [`capture`] | The OS capture boundary: desktop state, monitors, `CaptureBackend` |
//! | [`selection`] | Capture sessions: one at a time, the frozen picture and its selection |
//! | [`pins`] | Pins: stable IDs, close/reopen/destroy lifecycle, the window trait |
//! | [`annotation`] | **Placeholder**: annotation is Step 56 |
//! | [`settings`], [`paths`] | `config.toml` and the stable per-user folders |
//! | [`hotkeys`] | The shipped manifest subset, global registration, pin keys |
//! | [`ipc`] | Noeul IPC v1: framing and messages between the CLI and the instance |
//! | [`app`] | The instance core that answers IPC, hotkeys, tray and pin events |
//! | [`tray`] | The optional notification-area icon's menu model |
//! | `win` | Win32: GDI capture, pin windows, tray icon, clipboard, named pipe, runtime |

pub mod annotation;
pub mod app;
pub mod capture;
pub mod dib;
pub mod error;
pub mod frame;
pub mod hotkeys;
pub mod ipc;
pub mod paths;
pub mod pins;
pub mod selection;
pub mod settings;
pub mod tray;
pub mod util;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod win;

/// The package version (`noeul --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The executable name the CLI expects to find serving the pipe (identity check).
pub const IMAGE: &str = "noeul.exe";
