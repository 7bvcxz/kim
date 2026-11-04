//! Stable error codes and exit classes (docs/CLI_CONTRACT.md §5–§6).
//!
//! Codes shared with `kim` and `subak` keep their registered meaning and exit code; Noeul's own
//! codes are registered in the Noeul table of CLI_CONTRACT.md §6 (a test reads that document).

use std::fmt;

/// Every error code `noeul` can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    Io,
    Internal,
    PermissionDenied,
    Usage,
    HotkeyInvalid,
    HotkeyReserved,
    ActionNotFound,
    NoeulNotRunning,
    PinNotFound,
    NothingToPin,
    ProtocolError,
    NoeulIdentityMismatch,
    HotkeysInvalid,
    ImageInvalid,
    UnsupportedPlatform,
    NoeulIncompatible,
    HotkeyUnsupported,
    HotkeysFromNewerVersion,
    DesktopUnavailable,
    ClipboardUnavailable,
    NoeulUnresponsive,
    Timeout,
    Busy,
    ConfirmationRequired,
    HotkeyConflict,
    Cancelled,
    NoeulStartFailed,
}

impl Code {
    pub const ALL: &'static [Code] = &[
        Code::Io,
        Code::Internal,
        Code::PermissionDenied,
        Code::Usage,
        Code::HotkeyInvalid,
        Code::HotkeyReserved,
        Code::ActionNotFound,
        Code::NoeulNotRunning,
        Code::PinNotFound,
        Code::NothingToPin,
        Code::ProtocolError,
        Code::NoeulIdentityMismatch,
        Code::HotkeysInvalid,
        Code::ImageInvalid,
        Code::UnsupportedPlatform,
        Code::NoeulIncompatible,
        Code::HotkeyUnsupported,
        Code::HotkeysFromNewerVersion,
        Code::DesktopUnavailable,
        Code::ClipboardUnavailable,
        Code::NoeulUnresponsive,
        Code::Timeout,
        Code::Busy,
        Code::ConfirmationRequired,
        Code::HotkeyConflict,
        Code::Cancelled,
        Code::NoeulStartFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::Io => "io",
            Code::Internal => "internal",
            Code::PermissionDenied => "permission_denied",
            Code::Usage => "usage",
            Code::HotkeyInvalid => "hotkey_invalid",
            Code::HotkeyReserved => "hotkey_reserved",
            Code::ActionNotFound => "action_not_found",
            Code::NoeulNotRunning => "noeul_not_running",
            Code::PinNotFound => "pin_not_found",
            Code::NothingToPin => "nothing_to_pin",
            Code::ProtocolError => "protocol_error",
            Code::NoeulIdentityMismatch => "noeul_identity_mismatch",
            Code::HotkeysInvalid => "hotkeys_invalid",
            Code::ImageInvalid => "image_invalid",
            Code::UnsupportedPlatform => "unsupported_platform",
            Code::NoeulIncompatible => "noeul_incompatible",
            Code::HotkeyUnsupported => "hotkey_unsupported",
            Code::HotkeysFromNewerVersion => "hotkeys_from_newer_version",
            Code::DesktopUnavailable => "desktop_unavailable",
            Code::ClipboardUnavailable => "clipboard_unavailable",
            Code::NoeulUnresponsive => "noeul_unresponsive",
            Code::Timeout => "timeout",
            Code::Busy => "busy",
            Code::ConfirmationRequired => "confirmation_required",
            Code::HotkeyConflict => "hotkey_conflict",
            Code::Cancelled => "cancelled",
            Code::NoeulStartFailed => "noeul_start_failed",
        }
    }

    pub fn parse(s: &str) -> Option<Code> {
        Code::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The exit class (docs/CLI_CONTRACT.md §5).
    pub fn exit_code(self) -> u8 {
        match self {
            Code::Io | Code::Internal | Code::PermissionDenied | Code::NoeulStartFailed => 1,
            Code::Usage | Code::HotkeyInvalid | Code::HotkeyReserved => 2,
            Code::ActionNotFound
            | Code::NoeulNotRunning
            | Code::PinNotFound
            | Code::NothingToPin => 3,
            Code::ProtocolError
            | Code::NoeulIdentityMismatch
            | Code::HotkeysInvalid
            | Code::ImageInvalid => 4,
            Code::UnsupportedPlatform
            | Code::NoeulIncompatible
            | Code::HotkeyUnsupported
            | Code::HotkeysFromNewerVersion => 5,
            Code::DesktopUnavailable
            | Code::ClipboardUnavailable
            | Code::NoeulUnresponsive
            | Code::Timeout => 6,
            Code::Busy | Code::ConfirmationRequired | Code::HotkeyConflict => 7,
            Code::Cancelled => 8,
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failure with everything the envelope and the human error text need. Messages never carry
/// pixels, clipboard text, window titles or image paths (NOEUL-SEC-008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoeulError {
    pub code: Code,
    pub message: String,
    pub hint: Option<String>,
    pub details: Vec<String>,
}

impl NoeulError {
    pub fn new(code: Code, message: impl Into<String>) -> NoeulError {
        NoeulError {
            code,
            message: message.into(),
            hint: None,
            details: Vec::new(),
        }
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> NoeulError {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> NoeulError {
        self.details.push(detail.into());
        self
    }

    #[must_use]
    pub fn details(mut self, details: Vec<String>) -> NoeulError {
        self.details.extend(details);
        self
    }

    pub fn io(what: impl fmt::Display, e: &std::io::Error) -> NoeulError {
        let code = if e.kind() == std::io::ErrorKind::PermissionDenied {
            Code::PermissionDenied
        } else {
            Code::Io
        };
        NoeulError::new(code, format!("{what}: {e}"))
    }

    pub fn unsupported_platform(what: &str) -> NoeulError {
        NoeulError::new(
            Code::UnsupportedPlatform,
            format!("{what} is not supported on this platform yet"),
        )
        .hint("Noeul's desktop features are implemented for Windows only; Linux follows in milestone N7 (ADR 0011)")
    }

    pub fn not_running() -> NoeulError {
        NoeulError::new(Code::NoeulNotRunning, "Noeul is not running")
            .hint("start it with `noeul start`, or pass --start")
    }
}

impl fmt::Display for NoeulError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for NoeulError {}

pub type Result<T> = std::result::Result<T, NoeulError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case_and_in_documented_classes() {
        let mut seen = std::collections::BTreeSet::new();
        for c in Code::ALL {
            assert!(seen.insert(c.as_str()), "duplicate {c}");
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            );
            assert!((1..=8).contains(&c.exit_code()));
            assert_eq!(Code::parse(c.as_str()), Some(*c));
        }
        // Shared codes keep the exit class of the registry (docs/CLI_CONTRACT.md §6).
        assert_eq!(Code::Usage.exit_code(), 2);
        assert_eq!(Code::ProtocolError.exit_code(), 4);
        assert_eq!(Code::UnsupportedPlatform.exit_code(), 5);
        assert_eq!(Code::Timeout.exit_code(), 6);
        assert_eq!(Code::ConfirmationRequired.exit_code(), 7);
        assert_eq!(Code::Cancelled.exit_code(), 8);
        assert_eq!(Code::HotkeyConflict.exit_code(), 7);
    }

    #[test]
    fn every_code_is_registered_in_the_cli_contract_with_its_exit_code() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/CLI_CONTRACT.md"),
        )
        .unwrap();
        let mut rows = std::collections::BTreeMap::new();
        for line in doc.lines() {
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            if cells.len() >= 3
                && let Some(code) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`'))
                && let Ok(exit) = cells[1].parse::<u8>()
            {
                assert!(
                    rows.insert(code.to_string(), exit).is_none(),
                    "`{code}` documented twice"
                );
            }
        }
        for c in Code::ALL {
            assert_eq!(
                rows.get(c.as_str()),
                Some(&c.exit_code()),
                "`{c}` must be in CLI_CONTRACT.md §6 with exit {}",
                c.exit_code()
            );
        }
    }
}
