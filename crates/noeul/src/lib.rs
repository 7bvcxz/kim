//! Noeul (노을): a small native screenshot and floating-pin tool for Windows (pre-alpha,
//! Step 49 foundation). See docs/NOEUL_PHILOSOPHY.md and docs/adr/0023-noeul-app-foundation.md.

pub mod error;

/// The package version (`noeul --version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
