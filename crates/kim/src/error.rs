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
    /// | 6 | network unavailable |
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Internal | Self::Io => 1,
            Self::Usage | Self::UnsupportedSource => 2,
            Self::ToolNotFound => 3,
            Self::CatalogInvalid
            | Self::SignatureInvalid
            | Self::UntrustedKey
            | Self::CatalogRollback => 4,
            Self::NetworkUnavailable => 6,
        }
    }
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
    }

    #[test]
    fn serialized_codes_match_as_str() {
        for code in [
            ErrorCode::Usage,
            ErrorCode::ToolNotFound,
            ErrorCode::CatalogInvalid,
            ErrorCode::SignatureInvalid,
            ErrorCode::UntrustedKey,
            ErrorCode::CatalogRollback,
            ErrorCode::NetworkUnavailable,
            ErrorCode::UnsupportedSource,
            ErrorCode::Io,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}
