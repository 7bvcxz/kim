//! CLI contract v1 output (docs/CLI_CONTRACT.md §5–§7): error codes and their exit classes, the
//! `--json` envelope and human error text. Codes shared with `kim` keep kim's meaning and exit
//! code; Subak's own codes are registered in CLI_CONTRACT.md §6.

use serde_json::{Value, json};

/// Stable error codes `subak` can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Io,
    Internal,
    PermissionDenied,
    SpawnFailed,
    Usage,
    HostNotRunning,
    ProtocolError,
    /// Only constructed on non-Windows targets (ADR 0011 stub).
    #[cfg_attr(windows, allow(dead_code))]
    UnsupportedPlatform,
    HostDisconnected,
    Timeout,
    HostAlreadyRunning,
    ConfirmationRequired,
    Cancelled,
}

impl Code {
    #[cfg(test)]
    pub const ALL: [Self; 13] = [
        Self::Io,
        Self::Internal,
        Self::PermissionDenied,
        Self::SpawnFailed,
        Self::Usage,
        Self::HostNotRunning,
        Self::ProtocolError,
        Self::UnsupportedPlatform,
        Self::HostDisconnected,
        Self::Timeout,
        Self::HostAlreadyRunning,
        Self::ConfirmationRequired,
        Self::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Internal => "internal",
            Self::PermissionDenied => "permission_denied",
            Self::SpawnFailed => "spawn_failed",
            Self::Usage => "usage",
            Self::HostNotRunning => "host_not_running",
            Self::ProtocolError => "protocol_error",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::HostDisconnected => "host_disconnected",
            Self::Timeout => "timeout",
            Self::HostAlreadyRunning => "host_already_running",
            Self::ConfirmationRequired => "confirmation_required",
            Self::Cancelled => "cancelled",
        }
    }

    /// The exit class (docs/CLI_CONTRACT.md §5).
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Io | Self::Internal | Self::PermissionDenied | Self::SpawnFailed => 1,
            Self::Usage => 2,
            Self::HostNotRunning => 3,
            Self::ProtocolError => 4,
            Self::UnsupportedPlatform => 5,
            Self::HostDisconnected | Self::Timeout => 6,
            Self::HostAlreadyRunning | Self::ConfirmationRequired => 7,
            Self::Cancelled => 8,
        }
    }
}

/// A failure with everything the envelope and the human error text need.
#[derive(Debug)]
pub struct CliError {
    pub code: Code,
    pub message: String,
    pub hint: Option<String>,
    pub details: Vec<String>,
}

impl CliError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            details: Vec::new(),
        }
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.details.push(detail.into());
        self
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Code::Usage, message).hint("see `subak --help`")
    }
}

/// The success document of `--json`.
pub fn success(command: &str, data: &Value, warnings: &[String]) -> String {
    json!({
        "ok": true,
        "command": command,
        "schema_version": 1,
        "data": data,
        "warnings": warnings,
    })
    .to_string()
}

/// The failure document of `--json`; `error.exit_code` equals the process exit code.
pub fn failure(command: &str, err: &CliError) -> String {
    json!({
        "ok": false,
        "command": command,
        "schema_version": 1,
        "error": {
            "code": err.code.as_str(),
            "exit_code": err.code.exit_code(),
            "message": err.message,
            "hint": err.hint,
            "details": err.details,
        },
    })
    .to_string()
}

/// The human error text for stderr (docs/CLI_CONTRACT.md §7).
pub fn human_error(err: &CliError) -> String {
    let mut text = format!("subak: error: {}\n", err.message);
    for d in &err.details {
        text.push_str(&format!("  - {d}\n"));
    }
    if let Some(h) = &err.hint {
        text.push_str(&format!("hint: {h}\n"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_snake_case_unique_and_in_documented_classes() {
        let mut seen = std::collections::BTreeSet::new();
        for c in Code::ALL {
            assert!(seen.insert(c.as_str()), "duplicate {}", c.as_str());
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            );
            assert!((1..=8).contains(&c.exit_code()));
        }
        // Codes shared with kim keep kim's exit class (docs/CLI_CONTRACT.md §6).
        assert_eq!(Code::Usage.exit_code(), 2);
        assert_eq!(Code::Timeout.exit_code(), 6);
        assert_eq!(Code::ConfirmationRequired.exit_code(), 7);
        assert_eq!(Code::Cancelled.exit_code(), 8);
        assert_eq!(Code::PermissionDenied.exit_code(), 1);
    }

    #[test]
    fn every_code_is_registered_in_the_cli_contract_with_its_exit_code() {
        // docs/CLI_CONTRACT.md §6: one row per code, `| `code` | exit | meaning |`, in either
        // the shared table or the Subak table.
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
                "`{}` must be in CLI_CONTRACT.md §6 with exit {}",
                c.as_str(),
                c.exit_code()
            );
        }
    }

    #[test]
    fn envelopes_have_the_contract_shape() {
        let ok: Value =
            serde_json::from_str(&success("host.status", &json!({"a": 1}), &[])).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["schema_version"], 1);
        assert_eq!(ok["data"]["a"], 1);
        let err = CliError::new(Code::HostNotRunning, "no Host").hint("start one");
        let bad: Value = serde_json::from_str(&failure("host.stop", &err)).unwrap();
        assert_eq!(bad["error"]["code"], "host_not_running");
        assert_eq!(bad["error"]["exit_code"], 3);
        assert_eq!(bad["error"]["hint"], "start one");
        assert!(human_error(&err).starts_with("subak: error: no Host\nhint: start one"));
    }
}
