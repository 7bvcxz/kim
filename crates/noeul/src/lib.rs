//! Noeul (노을): a small native screenshot and floating-pin tool for Windows (pre-alpha:
//! Step 49 foundation, Step 50 region selection overlay, Step 51 double-click to pin, Step 52
//! smart selection and pixel precision, Step 53 floating pin engine, Step 56 annotation, Step 55
//! clipboard paste and rich sources). See
//! docs/NOEUL_PHILOSOPHY.md, docs/NOEUL_REQUIREMENTS.md, docs/adr/0023-noeul-app-foundation.md,
//! docs/adr/0026-noeul-capture-overlay.md, docs/adr/0029-noeul-pin-engine.md and
//! docs/adr/0038-noeul-annotation.md.
//!
//! Modules, from the bottom up:
//!
//! | Module | What |
//! | --- | --- |
//! | [`frame`] | Owned capture-image buffers and their accounting |
//! | [`capture`] | The OS capture boundary: desktop state, monitors, `CaptureBackend` |
//! | [`selection`] | Capture sessions: one at a time, the frozen picture, [`selection::CaptureSelection`] |
//! | [`overlay`] | The region-selection overlay: state machine, double-click gesture, toolbar, detection highlight, magnifier, precision keys, snapping, history, geometry, pixels |
//! | [`detect`] | What hover detection offers: window facts and their classification, element requests and answers (rectangles only) |
//! | [`dpi`] | Logical and device pixels on mixed-DPI layouts |
//! | [`displays`] | Display changes (debounced), monitor and window capture targets, logical mapping, gaps, the synthetic pointer (Step 58) |
//! | [`output`] | Copy and save of a selection: PNG and `CF_DIBV5` in memory, files only where the user asked |
//! | [`pins`] | The pin engine's model: stable IDs, placement, zoom, hide/show, close/reopen/destroy, the window trait |
//! | [`zoom`] | Pin zoom levels, sizes, fitting and the premultiplying scaler |
//! | [`transform`] | Pin transforms (Step 54): orientation, opacity, thumbnails, border resizing, and the window's outline, shadow and click-through badge |
//! | [`gif`] | Recognising an animated GIF on the clipboard, so a pin never flattens one silently |
//! | [`paste`] | What `F3` takes from the clipboard (Step 55): formats, priority, colours, copied paths, private content, the stand-in |
//! | [`html`] | Noeul's own sanitizer for clipboard HTML (Step 55): text and basic formatting only, no script, no remote resource |
//! | [`card`] | Text, HTML, colour and file-name cards (Step 55): document, layout, legible colours, pixels |
//! | [`decode`] | Local image decoding (Step 55): PNG, JPEG, GIF, BMP, ICO, TIFF, TGA with limits, alpha, ICC to sRGB, multi-image counts |
//! | [`annotation`] | Annotation (Step 56): editable vector objects, undo/redo, the editor (tools, text, IME), the rasterizer and the export |
//! | [`settings`], [`paths`] | `config.toml` and the stable per-user folders |
//! | [`hotkeys`] | The shipped manifest subset, global registration, pin keys |
//! | [`ipc`] | Noeul IPC v1: framing and messages between the CLI and the instance |
//! | [`app`] | The instance core that answers IPC, hotkeys, tray and pin events |
//! | [`tray`] | The optional notification-area icon's menu model |
//! | `win` | Win32: GDI capture, pin windows, tray icon, clipboard, named pipe, runtime |

pub mod annotation;
pub mod app;
pub mod capture;
pub mod card;
pub mod decode;
pub mod detect;
pub mod dib;
pub mod displays;
pub mod dpi;
pub mod error;
pub mod frame;
pub mod gif;
pub mod hotkeys;
pub mod html;
pub mod ipc;
pub mod output;
pub mod overlay;
pub mod paste;
pub mod paths;
pub mod pins;
pub mod selection;
pub mod settings;
pub mod transform;
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
