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
    InvalidName,
    AmbiguousTarget,
    HostNotRunning,
    TargetNotFound,
    ProtocolError,
    HostIdentityMismatch,
    /// Only constructed on non-Windows targets (ADR 0011 stub).
    #[cfg_attr(windows, allow(dead_code))]
    UnsupportedPlatform,
    HostTooOld,
    HostTooNew,
    HostDisconnected,
    HostUnresponsive,
    Timeout,
    HostAlreadyRunning,
    NameConflict,
    InvalidState,
    TerminalsRunning,
    ConfirmationRequired,
    Cancelled,
    // The `hotkeys` command group (docs/HOTKEY_CONTRACT.md §10.4, Step 17).
    HotkeyInvalid,
    HotkeyReserved,
    ActionNotFound,
    HotkeysInvalid,
    HotkeyUnsupported,
    HotkeysFromNewerVersion,
    HotkeyConflict,
    // The `config` and `profile` commands and `new --profile` (Step 20, ADR 0051).
    ConfigInvalid,
    ConfigFromNewerVersion,
    ProfileNotFound,
    SecretInConfig,
}

impl Code {
    #[cfg(test)]
    pub const ALL: [Self; 34] = [
        Self::Io,
        Self::Internal,
        Self::PermissionDenied,
        Self::SpawnFailed,
        Self::Usage,
        Self::InvalidName,
        Self::AmbiguousTarget,
        Self::HostNotRunning,
        Self::TargetNotFound,
        Self::ProtocolError,
        Self::HostIdentityMismatch,
        Self::UnsupportedPlatform,
        Self::HostTooOld,
        Self::HostTooNew,
        Self::HostDisconnected,
        Self::HostUnresponsive,
        Self::Timeout,
        Self::HostAlreadyRunning,
        Self::NameConflict,
        Self::InvalidState,
        Self::TerminalsRunning,
        Self::ConfirmationRequired,
        Self::Cancelled,
        Self::HotkeyInvalid,
        Self::HotkeyReserved,
        Self::ActionNotFound,
        Self::HotkeysInvalid,
        Self::HotkeyUnsupported,
        Self::HotkeysFromNewerVersion,
        Self::HotkeyConflict,
        Self::ConfigInvalid,
        Self::ConfigFromNewerVersion,
        Self::ProfileNotFound,
        Self::SecretInConfig,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Internal => "internal",
            Self::PermissionDenied => "permission_denied",
            Self::SpawnFailed => "spawn_failed",
            Self::Usage => "usage",
            Self::InvalidName => "invalid_name",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::HostNotRunning => "host_not_running",
            Self::TargetNotFound => "target_not_found",
            Self::ProtocolError => "protocol_error",
            Self::HostIdentityMismatch => "host_identity_mismatch",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::HostTooOld => "host_too_old",
            Self::HostTooNew => "host_too_new",
            Self::HostDisconnected => "host_disconnected",
            Self::HostUnresponsive => "host_unresponsive",
            Self::Timeout => "timeout",
            Self::HostAlreadyRunning => "host_already_running",
            Self::NameConflict => "name_conflict",
            Self::InvalidState => "invalid_state",
            Self::TerminalsRunning => "terminals_running",
            Self::ConfirmationRequired => "confirmation_required",
            Self::Cancelled => "cancelled",
            Self::HotkeyInvalid => "hotkey_invalid",
            Self::HotkeyReserved => "hotkey_reserved",
            Self::ActionNotFound => "action_not_found",
            Self::HotkeysInvalid => "hotkeys_invalid",
            Self::HotkeyUnsupported => "hotkey_unsupported",
            Self::HotkeysFromNewerVersion => "hotkeys_from_newer_version",
            Self::HotkeyConflict => "hotkey_conflict",
            Self::ConfigInvalid => "config_invalid",
            Self::ConfigFromNewerVersion => "config_from_newer_version",
            Self::ProfileNotFound => "profile_not_found",
            Self::SecretInConfig => "secret_in_config",
        }
    }

    /// The exit class (docs/CLI_CONTRACT.md §5).
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Io | Self::Internal | Self::PermissionDenied | Self::SpawnFailed => 1,
            Self::Usage
            | Self::InvalidName
            | Self::AmbiguousTarget
            | Self::HotkeyInvalid
            | Self::HotkeyReserved => 2,
            Self::HostNotRunning
            | Self::TargetNotFound
            | Self::ActionNotFound
            | Self::ProfileNotFound => 3,
            Self::ProtocolError
            | Self::HostIdentityMismatch
            | Self::HotkeysInvalid
            | Self::ConfigInvalid
            | Self::SecretInConfig => 4,
            Self::UnsupportedPlatform
            | Self::HostTooOld
            | Self::HostTooNew
            | Self::HotkeyUnsupported
            | Self::HotkeysFromNewerVersion
            | Self::ConfigFromNewerVersion => 5,
            Self::HostDisconnected | Self::HostUnresponsive | Self::Timeout => 6,
            Self::HostAlreadyRunning
            | Self::NameConflict
            | Self::InvalidState
            | Self::TerminalsRunning
            | Self::ConfirmationRequired
            | Self::HotkeyConflict => 7,
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

    /// docs/contracts/schemas/subak-cli-v1.schema.json lists exactly these codes, each in the
    /// exit class of `Code::exit_code` (Step 12).
    #[test]
    fn the_cli_schema_registers_every_code_with_its_exit_class() {
        let schema: Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../docs/contracts/schemas/subak-cli-v1.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let listed: Vec<&str> = schema["$defs"]["error_code"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        let mut ours: Vec<&str> = Code::ALL.iter().map(|c| c.as_str()).collect();
        let mut sorted = listed.clone();
        sorted.sort_unstable();
        ours.sort_unstable();
        assert_eq!(sorted, ours, "subak-cli-v1 error_code enum");
        for class in schema["$defs"]["error"]["oneOf"].as_array().unwrap() {
            let exit = class["properties"]["exit_code"]["const"].as_u64().unwrap();
            for c in class["properties"]["code"]["enum"].as_array().unwrap() {
                let code = Code::ALL
                    .iter()
                    .find(|k| k.as_str() == c.as_str().unwrap())
                    .unwrap();
                assert_eq!(u64::from(code.exit_code()), exit, "{c}");
            }
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
