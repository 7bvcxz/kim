//! `subak-term` — Subak's terminal emulation core (ADR 0030, ADR 0036,
//! docs/TERMINAL_EMULATOR.md).
//!
//! One parser and one screen model serve both sides of Subak, so a pane looks the same wherever
//! it is shown:
//!
//! - [`screen`] — the VT parser and xterm-subset screen with its bounded scrollback, snapshot and
//!   capture. The Host feeds every byte a pane writes into one of these; a client view feeds the
//!   Host's snapshot and live stream into its own copy.
//! - [`view`] — one client's view of a pane: the model plus the scrollback viewport, selection
//!   and copy, hyperlink hit tests and input, all testable without drawing anything.
//! - [`input`] — keys, pasted text, mouse events and focus changes as the bytes a program
//!   expects, under the modes it set; what a paste would do ([`PasteCheck`]).
//! - [`search`] — bounded literal and regular-expression search of a view's history and screen
//!   (Step 19); [`links`] — web addresses in the text and the validation of every link target.
//! - `render` (feature `render`, on by default; the Host turns it off) — the CPU cell-grid
//!   renderer that draws a view into a pixel frame, with the system's fonts through GDI on
//!   Windows and a deterministic built-in rasterizer for tests.
//!
//! Portable and headless except `render::gdi`: no OS calls and no allocation per byte in the
//! common path.

pub mod input;
pub mod links;
#[cfg(feature = "render")]
pub mod render;
pub mod screen;
pub mod search;
pub mod view;

pub use input::{Key, Mods, MouseAction, MouseButton, MouseEvent, PasteCheck};
pub use screen::{ModeState, Screen};
pub use view::{Motion, MouseOutcome, Point, SelectMode, Span, TermView};
