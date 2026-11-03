//! Kim manager library: tool catalog model and validation, signature verification, catalog
//! sources (bundled, cache, explicit refresh), tool status evaluation, platform detection and
//! per-user paths.
//!
//! This crate never installs, updates or starts anything by itself: [`install`] runs only the
//! plan a user explicitly requested. Network access happens only in an explicit catalog refresh
//! or package download. It has no GUI dependencies (ADR 0002), so headless binaries stay small.

pub mod catalog;
pub mod error;
pub mod fetch;
pub mod install;
pub mod paths;
pub mod platform;
pub mod running;
pub mod settings;
pub mod time;

/// Version of this Kim build.
pub const KIM_VERSION: &str = env!("CARGO_PKG_VERSION");
