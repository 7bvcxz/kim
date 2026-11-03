//! Kim manager library: tool catalog model and validation, signature verification, catalog
//! sources (bundled, cache, explicit refresh), tool status evaluation, the explicit installer,
//! installed-tool inventory, capability documents, Kim Manager's hotkey manifest and read-only
//! cross-tool hotkey discovery, secret redaction, platform detection and per-user paths.
//!
//! This crate never installs, updates or starts anything by itself: [`install`] runs only the
//! plan a user explicitly requested, and [`capabilities::probe`] starts a tool only when a
//! caller explicitly asks it to describe itself. Network access happens only in an explicit
//! catalog refresh or package download. It has no GUI dependencies (ADR 0002), so headless
//! binaries stay small.

pub mod capabilities;
pub mod catalog;
pub mod error;
pub mod fetch;
pub mod hotkeys;
pub mod install;
pub mod inventory;
pub mod paths;
pub mod platform;
pub mod redact;
pub mod running;
pub mod settings;
pub mod time;

/// Version of this Kim build.
pub const KIM_VERSION: &str = env!("CARGO_PKG_VERSION");
