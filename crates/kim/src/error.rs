//! Structured errors with stable codes and exit codes (see docs/CATALOG_FORMAT.md §7; the
//! cross-tool contract is finalized in Step 07, docs/CLI_CONTRACT.md).

use serde::Serialize;
use std::fmt;

/// Stable, machine-readable error codes. Never rename a variant's serialized form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Invalid command-line usage.
    Usage,
    /// The named tool is not in the catalog.
    ToolNotFound,
    /// The catalog document is malformed or violates the format rules.
    CatalogInvalid,
    /// The signature does not verify, or the signature file is malformed.
    SignatureInvalid,
    /// The signature was made by a key that this Kim build does not trust.
    UntrustedKey,
    /// A fetched catalog is older than (or conflicts with) the one already trusted.
    CatalogRollback,
    /// The catalog source could not be reached (offline, DNS, TLS, HTTP error).
    NetworkUnavailable,
    /// The catalog source URL uses a scheme or host Kim refuses to use.
    UnsupportedSource,
    /// Local file-system error.
    Io,
    /// A bug.
    Internal,
    /// The tool has no release on the selected channel.
    NoRelease,
    /// No release/package fits this machine or Kim version.
    Incompatible,
    /// The tool is not installed.
    NotInstalled,
    /// A dependency must be installed first (Kim never installs it implicitly).
    DependencyMissing,
    /// The tool is running; Kim never stops it by itself.
    ToolRunning,
    /// Another Kim operation on this tool is in progress.
    Busy,
    /// A downloaded package does not match the size/hash in the signed catalog.
    IntegrityFailed,
    /// The package is malformed (bad archive, missing entrypoint, unsafe paths).
    PackageInvalid,
    /// The installed program failed its `--version` self-check.
    SelfCheckFailed,
    /// The user cancelled the operation; nothing was changed.
    Cancelled,
    /// The catalog is stale and the operation needs a fresh one (or explicit `--allow-stale`).
    StaleCatalog,
    /// Offline mode is on; network operations are disabled.
    OfflineMode,
    /// A destructive operation needs explicit confirmation (`--yes` in non-interactive use).
    ConfirmationRequired,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::ToolNotFound => "tool_not_found",
            Self::CatalogInvalid => "catalog_invalid",
            Self::SignatureInvalid => "signature_invalid",
            Self::UntrustedKey => "untrusted_key",
            Self::CatalogRollback => "catalog_rollback",
            Self::NetworkUnavailable => "network_unavailable",
            Self::UnsupportedSource => "unsupported_source",
            Self::Io => "io",
            Self::Internal => "internal",
            Self::NoRelease => "no_release",
            Self::Incompatible => "incompatible",
            Self::NotInstalled => "not_installed",
            Self::DependencyMissing => "dependency_missing",
            Self::ToolRunning => "tool_running",
            Self::Busy => "busy",
            Self::IntegrityFailed => "integrity_failed",
            Self::PackageInvalid => "package_invalid",
            Self::SelfCheckFailed => "self_check_failed",
            Self::Cancelled => "cancelled",
            Self::StaleCatalog => "stale_catalog",
            Self::OfflineMode => "offline_mode",
            Self::ConfirmationRequired => "confirmation_required",
        }
    }

    /// Process exit code for this error.
    ///
    /// | code | meaning |
    /// | --- | --- |
    /// | 1 | internal or I/O failure |
    /// | 2 | usage error (bad arguments, refused source URL) |
    /// | 3 | not found |
    /// | 4 | integrity / trust failure (malformed, bad signature, untrusted key, rollback) |
    /// | 5 | no compatible release |
    /// | 6 | network unavailable / offline mode |
    /// | 7 | conflict: running tool, busy, missing dependency, stale catalog, needs confirmation |
    /// | 8 | cancelled by the user |
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Internal | Self::Io => 1,
            Self::Usage | Self::UnsupportedSource => 2,
            Self::ToolNotFound | Self::NotInstalled => 3,
            Self::CatalogInvalid
            | Self::SignatureInvalid
            | Self::UntrustedKey
            | Self::CatalogRollback
            | Self::IntegrityFailed
            | Self::PackageInvalid
            | Self::SelfCheckFailed => 4,
            Self::NoRelease | Self::Incompatible => 5,
            Self::NetworkUnavailable | Self::OfflineMode => 6,
            Self::ToolRunning
            | Self::Busy
            | Self::DependencyMissing
            | Self::StaleCatalog
            | Self::ConfirmationRequired => 7,
            Self::Cancelled => 8,
        }
    }

    /// Every code, for registries and tests.
    pub const ALL: &'static [ErrorCode] = &[
        Self::Usage,
        Self::ToolNotFound,
        Self::CatalogInvalid,
        Self::SignatureInvalid,
        Self::UntrustedKey,
        Self::CatalogRollback,
        Self::NetworkUnavailable,
        Self::UnsupportedSource,
        Self::Io,
        Self::Internal,
        Self::NoRelease,
        Self::Incompatible,
        Self::NotInstalled,
        Self::DependencyMissing,
        Self::ToolRunning,
        Self::Busy,
        Self::IntegrityFailed,
        Self::PackageInvalid,
        Self::SelfCheckFailed,
        Self::Cancelled,
        Self::StaleCatalog,
        Self::OfflineMode,
        Self::ConfirmationRequired,
    ];
}

/// An error with a stable code, a human message and an optional hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KimError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
    /// Individual findings (e.g. every validation problem), for `--json` and verbose output.
    pub details: Vec<String>,
}

impl KimError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

impl fmt::Display for KimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for KimError {}

pub type Result<T> = std::result::Result<T, KimError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ErrorCode::Usage.exit_code(), 2);
        assert_eq!(ErrorCode::ToolNotFound.exit_code(), 3);
        assert_eq!(ErrorCode::SignatureInvalid.exit_code(), 4);
        assert_eq!(ErrorCode::CatalogRollback.exit_code(), 4);
        assert_eq!(ErrorCode::NetworkUnavailable.exit_code(), 6);
        assert_eq!(ErrorCode::Io.exit_code(), 1);
        assert_eq!(ErrorCode::Incompatible.exit_code(), 5);
        assert_eq!(ErrorCode::ToolRunning.exit_code(), 7);
        assert_eq!(ErrorCode::Cancelled.exit_code(), 8);
    }

    #[test]
    fn registry_has_unique_codes() {
        let mut names: Vec<_> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn serialized_codes_match_as_str() {
        for &code in ErrorCode::ALL {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
