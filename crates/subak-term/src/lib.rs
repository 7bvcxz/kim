//! `subak-term` — Subak's terminal emulation core (ADR 0030, ADR 0036,
//! docs/TERMINAL_EMULATOR.md).
//!
//! One parser and one screen model serve both sides of Subak, so a pane looks the same wherever
//! it is shown:
//!
//! - [`screen`] — the VT parser and xterm-subset screen with its bounded scrollback, snapshot and
//!   capture. The Host feeds every byte a pane writes into one of these; a client view feeds the
//!   Host's snapshot and live stream into its own copy.
//!
//! Portable and headless: no OS calls and no allocation per byte in the common path.

pub mod screen;
