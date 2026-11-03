//! Kim manager library: tool catalog model and validation, signature verification, catalog
//! sources (bundled, cache, explicit refresh), tool status evaluation, platform detection and
//! per-user paths.
//!
//! This crate never installs, updates or starts anything by itself. Network access happens only
//! inside [`catalog::store::refresh`], which the CLI calls only on an explicit
//! `kim catalog refresh`. It has no GUI dependencies (ADR 0002).

pub mod catalog;
pub mod error;
pub mod fetch;
pub mod paths;
pub mod platform;
pub mod time;

/// Version of this Kim build.
pub const KIM_VERSION: &str = env!("CARGO_PKG_VERSION");
