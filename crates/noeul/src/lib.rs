//! Noeul (노을): a small native screenshot and floating-pin tool for Windows (pre-alpha:
//! Step 49 foundation, Step 50 region selection overlay, Step 53 floating pin engine). See
//! docs/NOEUL_PHILOSOPHY.md, docs/NOEUL_REQUIREMENTS.md, docs/adr/0023-noeul-app-foundation.md,
//! docs/adr/0026-noeul-capture-overlay.md and docs/adr/0029-noeul-pin-engine.md.
//!
//! Modules, from the bottom up:
//!
//! | Module | What |
//! | --- | --- |
//! | [`frame`] | Owned capture-image buffers and their accounting |
//! | [`capture`] | The OS capture boundary: desktop state, monitors, `CaptureBackend` |
//! | [`selection`] | Capture sessions: one at a time, the frozen picture, [`selection::CaptureSelection`] |
//! | [`overlay`] | The region-selection overlay: state machine, double-click gesture, toolbar, geometry, DPI math, pixels |
//! | [`output`] | Copy and save of a selection: PNG and `CF_DIBV5` in memory, files only where the user asked |
//! | [`pins`] | The pin engine's model: stable IDs, placement, zoom, hide/show, close/reopen/destroy, the window trait |
//! | [`zoom`] | Pin zoom levels, sizes, fitting and the premultiplying scaler |
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
pub mod detect;
pub mod dib;
pub mod dpi;
pub mod error;
pub mod frame;
pub mod hotkeys;
pub mod ipc;
pub mod output;
pub mod overlay;
pub mod paths;
pub mod pins;
pub mod selection;
pub mod settings;
pub mod tray;
pub mod util;
pub mod zoom;

#[cfg(windows)]
#[allow(unsafe_code)]
pub mod win;

/// The validator of `cargo xtask schema-check`, so unit tests check every IPC message this build
/// constructs against docs/contracts/schemas (ADR 0015).
#[cfg(test)]
#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code, unused_qualifications, clippy::all)]
mod jsonschema;

/// Validates `v` against `docs/contracts/schemas/<name>.schema.json`, or against one of its
/// `$defs` with `name#def`.
#[cfg(test)]
pub(crate) fn schema_errors(name: &str, v: &serde_json::Value) -> Vec<String> {
    let (file, def) = name
        .split_once('#')
        .map_or((name, None), |(f, d)| (f, Some(d)));
    let mut schema: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../docs/contracts/schemas/{file}.schema.json")),
        )
        .unwrap(),
    )
    .unwrap();
    if let Some(d) = def {
        let defs = schema["$defs"].clone();
        schema = serde_json::json!({"$ref": format!("#/$defs/{d}"), "$defs": defs});
    }
    jsonschema::Validator::new(&schema).validate(v).unwrap()
}

/// The package version (`noeul --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The executable name the CLI expects to find serving the pipe (identity check).
pub const IMAGE: &str = "noeul.exe";
