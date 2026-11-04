//! CLI contract v1 output for `ddalgak` (docs/CLI_CONTRACT.md §5–§7): error codes and their exit
//! classes, the `--json` envelope and human error text. Shared codes keep the meaning and exit
//! code registered in CLI_CONTRACT.md §6; Ddalgak's own codes are registered there too.

use serde_json::{Value, json};

/// Stable error codes `ddalgak` and `ddalgak-gui` can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Io,
    Internal,
    PermissionDenied,
    HookFailed,
    Usage,
    NotInstalled,
    EngineNotRunning,
    ProtocolError,
    AppIdentityMismatch,
    RulesInvalid,
    UnsupportedPlatform,
    RulesFromNewerVersion,
    EngineElevated,
    AppUnresponsive,
    EngineOff,
    AppAlreadyRunning,
}

impl Code {
    pub const ALL: [Self; 16] = [
        Self::Io,
        Self::Internal,
        Self::PermissionDenied,
        Self::HookFailed,
        Self::Usage,
        Self::NotInstalled,
        Self::EngineNotRunning,
        Self::ProtocolError,
        Self::AppIdentityMismatch,
        Self::RulesInvalid,
        Self::UnsupportedPlatform,
        Self::RulesFromNewerVersion,
        Self::EngineElevated,
        Self::AppUnresponsive,
        Self::EngineOff,
        Self::AppAlreadyRunning,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Internal => "internal",
            Self::PermissionDenied => "permission_denied",
            Self::HookFailed => "hook_failed",
            Self::Usage => "usage",
            Self::NotInstalled => "not_installed",
            Self::EngineNotRunning => "engine_not_running",
            Self::ProtocolError => "protocol_error",
            Self::AppIdentityMismatch => "app_identity_mismatch",
            Self::RulesInvalid => "rules_invalid",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::RulesFromNewerVersion => "rules_from_newer_version",
            Self::EngineElevated => "engine_elevated",
            Self::AppUnresponsive => "app_unresponsive",
            Self::EngineOff => "engine_off",
            Self::AppAlreadyRunning => "app_already_running",
        }
    }

    pub fn parse(s: &str) -> Option<Code> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// The exit class (docs/CLI_CONTRACT.md §5).
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Io | Self::Internal | Self::PermissionDenied | Self::HookFailed => 1,
            Self::Usage => 2,
            Self::NotInstalled | Self::EngineNotRunning => 3,
            Self::ProtocolError | Self::AppIdentityMismatch | Self::RulesInvalid => 4,
            Self::UnsupportedPlatform | Self::RulesFromNewerVersion | Self::EngineElevated => 5,
            Self::AppUnresponsive => 6,
            Self::EngineOff | Self::AppAlreadyRunning => 7,
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

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(Code::Usage, message).hint("see `ddalgak --help`")
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

/// The human error text for stderr.
pub fn human_error(err: &CliError) -> String {
    let mut text = format!("ddalgak: error: {}\n", err.message);
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
    fn codes_are_unique_snake_case_and_in_documented_classes() {
        let mut seen = std::collections::BTreeSet::new();
        for c in Code::ALL {
            assert!(seen.insert(c.as_str()));
            assert!(
                c.as_str()
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_')
            );
            assert_eq!(Code::parse(c.as_str()), Some(c));
        }
        // Codes shared with other tools keep their registered exit class.
        assert_eq!(Code::Usage.exit_code(), 2);
        assert_eq!(Code::NotInstalled.exit_code(), 3);
        assert_eq!(Code::ProtocolError.exit_code(), 4);
        assert_eq!(Code::UnsupportedPlatform.exit_code(), 5);
        // DDALGAK_RULE_MODEL §13 fixed these.
        assert_eq!(Code::EngineNotRunning.exit_code(), 3);
        assert_eq!(Code::RulesInvalid.exit_code(), 4);
        assert_eq!(Code::RulesFromNewerVersion.exit_code(), 5);
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
                "`{}` must be in CLI_CONTRACT.md §6 with exit {}",
                c.as_str(),
                c.exit_code()
            );
        }
    }

    #[test]
    fn envelopes_have_the_contract_shape() {
        let ok: Value = serde_json::from_str(&success("status", &json!({"a": 1}), &[])).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["schema_version"], 1);
        let err = CliError::new(Code::EngineNotRunning, "not running").hint("start it");
        let bad: Value = serde_json::from_str(&failure("engine.start", &err)).unwrap();
        assert_eq!(bad["error"]["code"], "engine_not_running");
        assert_eq!(bad["error"]["exit_code"], 3);
        assert!(human_error(&err).starts_with("ddalgak: error: not running\nhint: start it"));
    }
}
