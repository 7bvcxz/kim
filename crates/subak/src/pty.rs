//! The platform-neutral pseudo terminal interface of the Host (docs/PTY_PROCESSES.md,
//! ADR 0004, ADR 0011, ADR 0025).
//!
//! This is everything the Host needs from a platform to run one pane: start a program in a new
//! pseudo terminal with its whole process tree contained, read its output, write its input,
//! resize it, end it gracefully or at once, and learn its exit status. Windows implements it with
//! ConPTY and a kill-on-close job ([`crate::win::conpty`], [`crate::win::launch`]). Other
//! platforms get the stub below, which answers `unsupported_platform`, until the Linux Host
//! (M4, `SUBAK-FR-070`) implements the same trait with a pty and a process group or cgroup.
//!
//! Portable: no OS calls here, only types, the validation of environment overrides, and the
//! dispatch to the platform implementation.

use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Variables the Host sets in every pane itself (ADR 0010 §1); an override may not touch them.
pub const RESERVED_VARS: [&str; 2] = ["SUBAK_PANE_ID", "SUBAK_TERMINAL_ID"];
/// Most environment overrides one `terminal.create` may carry.
pub const MAX_ENV_OVERRIDES: usize = 256;
/// Longest variable name (UTF-16 units).
pub const MAX_ENV_NAME: usize = 255;
/// Longest variable value (UTF-16 units; the Windows limit).
pub const MAX_ENV_VALUE: usize = 32_767;

/// Environment overrides of a new pane, in the order given: `Some(value)` sets a variable,
/// `None` removes it from the pane's environment. Names compare case-insensitively (Windows).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvOverrides {
    pub vars: Vec<(String, Option<String>)>,
}

/// What a pane shows about its overrides: names only, never values (CLI_CONTRACT §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvNames {
    pub set: Vec<String>,
    pub unset: Vec<String>,
}

impl EnvOverrides {
    pub fn names(&self) -> EnvNames {
        let mut n = EnvNames::default();
        for (k, v) in &self.vars {
            if v.is_some() {
                n.set.push(k.clone());
            } else {
                n.unset.push(k.clone());
            }
        }
        n
    }

    /// Parses the `env` parameter of `terminal.create`: `null` or an object of names to a string
    /// (set) or `null` (unset). Every message names the variable but never echoes a value.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let map = match v {
            Value::Null => return Ok(Self::default()),
            Value::Object(m) => m,
            _ => {
                return Err(
                    "env must be an object of variable names to a string (set) or null (unset)"
                        .into(),
                );
            }
        };
        if map.len() > MAX_ENV_OVERRIDES {
            return Err(format!(
                "env has {} variables; at most {MAX_ENV_OVERRIDES} are allowed",
                map.len()
            ));
        }
        let mut out = Self::default();
        for (name, value) in map {
            check_env_name(name)?;
            let shown = shown_name(name);
            if out.vars.iter().any(|(k, _)| same_var(k, name)) {
                return Err(format!(
                    "env names `{shown}` twice (names ignore case on Windows)"
                ));
            }
            let value = match value {
                Value::Null => None,
                Value::String(s) => {
                    if s.contains('\0') {
                        return Err(format!("the value of `{shown}` contains a NUL character"));
                    }
                    if s.encode_utf16().count() > MAX_ENV_VALUE {
                        return Err(format!(
                            "the value of `{shown}` is longer than {MAX_ENV_VALUE} characters"
                        ));
                    }
                    Some(s.clone())
                }
                _ => {
                    return Err(format!(
                        "the value of `{shown}` must be a string (set) or null (unset)"
                    ));
                }
            };
            out.vars.push((name.clone(), value));
        }
        Ok(out)
    }
}

/// A name as it may appear in a message: at most 64 characters.
fn shown_name(name: &str) -> String {
    name.chars().take(64).collect()
}

/// One character uppercased by the simple (one-to-one) mapping, as Windows compares names.
fn upcase(c: char) -> char {
    let mut u = c.to_uppercase();
    match (u.next(), u.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// Whether two variable names are the same variable (Windows: case-insensitive, character by
/// character).
pub fn same_var(a: &str, b: &str) -> bool {
    a.chars().map(upcase).eq(b.chars().map(upcase))
}

/// The key variable names sort by (Windows sorts environment blocks case-insensitively).
pub fn var_key(name: &str) -> String {
    name.chars().map(upcase).collect()
}

/// A variable name an override may use: 1–255 characters, no `=`, no control characters, not
/// reserved by the Host.
pub fn check_env_name(name: &str) -> Result<(), String> {
    let shown = shown_name(name);
    if name.is_empty() {
        return Err("an env variable name is empty".into());
    }
    if name.encode_utf16().count() > MAX_ENV_NAME {
        return Err(format!(
            "the env variable name `{shown}…` is longer than {MAX_ENV_NAME} characters"
        ));
    }
    if name.contains('=') {
        return Err(format!(
            "the env variable name `{shown}` contains `=`; pass NAME and value separately"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("an env variable name contains a control character".into());
    }
    if RESERVED_VARS.iter().any(|r| same_var(r, name)) {
        return Err(format!(
            "`{shown}` is set by the Host for every pane (ADR 0010) and cannot be overridden"
        ));
    }
    Ok(())
}

/// What to start in a new pseudo terminal.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    /// The program as the user named it: a path (relative paths are relative to `cwd`), or a
    /// bare name looked up on the `PATH` of the pane's environment.
    pub program: &'a str,
    pub args: &'a [String],
    /// Windows: a command line used verbatim instead of quoting `program` and `args` (the
    /// `--shell` line of `host serve`); its first word must be `program`.
    pub raw_line: Option<&'a str>,
    /// Absolute.
    pub cwd: &'a Path,
    pub env: &'a EnvOverrides,
    /// Variables the Host adds (`SUBAK_PANE_ID`, `SUBAK_TERMINAL_ID`); they win over everything.
    pub host_vars: &'a [(&'a str, &'a str)],
    pub size: (u16, u16),
}

/// Why a start failed: the `reason` of IPC `spawn_failed` (IPC_PROTOCOL §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No such program (or not on the pane's `PATH`).
    NotFound,
    /// The OS refused to run it (permissions, policy, elevation required).
    AccessDenied,
    /// The working directory does not exist, is not a directory, or cannot be entered.
    BadCwd,
    /// It exists but is not a program Windows can start directly (a batch or PowerShell script,
    /// a document, a corrupt or foreign executable).
    NotExecutable,
    Other,
    /// This platform has no pseudo terminal implementation yet (IPC `unsupported_platform`).
    UnsupportedPlatform,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::AccessDenied => "access_denied",
            Self::BadCwd => "bad_cwd",
            Self::NotExecutable => "not_executable",
            Self::Other => "other",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

/// A failed start, with a message that names the program but never an environment value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnError {
    pub reason: Reason,
    pub message: String,
    pub hint: Option<String>,
    /// The OS error code, when there was one.
    pub os_error: Option<i32>,
}

impl SpawnError {
    pub fn new(reason: Reason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
            hint: None,
            os_error: None,
        }
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn os(mut self, code: Option<i32>) -> Self {
        self.os_error = code;
        self
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn unsupported() -> Self {
        Self::new(
            Reason::UnsupportedPlatform,
            "pseudo terminals are implemented only on Windows (ConPTY) until the Linux Host (M4)",
        )
    }
}

/// One running pane: a pseudo terminal with one root process and its contained process tree.
pub trait Pty: Send + Sync + std::fmt::Debug {
    /// The root process.
    fn pid(&self) -> u32;
    /// The output stream, once (for the reader thread); `Ok(0)` from `read` at its end.
    fn take_output(&self) -> Option<Box<dyn io::Read + Send>>;
    /// Writes input bytes (UTF-8 and VT sequences; `0x03` is Ctrl+C). Blocks while the pseudo
    /// terminal does not accept more, which is the backpressure of the writing client.
    fn write_input(&self, bytes: &[u8]) -> io::Result<()>;
    fn resize(&self, cols: u16, rows: u16) -> io::Result<()>;
    /// Closes the pseudo terminal once (Windows: its processes get `CTRL_CLOSE_EVENT`) and the
    /// input. May block until the terminal host has exited, so the output must still be read.
    fn close(&self);
    /// Waits for the root process; `Some(exit status)` once it exited.
    fn wait_exit(&self, timeout: Option<Duration>) -> Option<u32>;
    /// Whether the root process still runs.
    fn running(&self) -> bool {
        self.wait_exit(Some(Duration::ZERO)).is_none()
    }
    /// Processes currently in the pane's process tree container (Windows: its job).
    fn processes(&self) -> u32;
    /// Ends every process in the container at once with `code`.
    fn kill_tree(&self, code: u32);
}

/// A started pane.
#[derive(Debug)]
pub struct Started {
    pub pty: Arc<dyn Pty>,
    /// The program file that runs (the resolved path).
    pub executable: PathBuf,
}

/// Checks a start without starting anything (dry runs): the working directory, the program
/// (resolved on the pane's `PATH`) and the command line. Returns the executable.
pub fn plan(spec: &Spec) -> Result<PathBuf, SpawnError> {
    #[cfg(windows)]
    return crate::win::launch::prepare(spec).map(|p| p.app);
    #[cfg(not(windows))]
    {
        let _ = spec;
        Err(SpawnError::unsupported())
    }
}

/// Starts `spec` in a new pseudo terminal.
pub fn spawn(spec: &Spec) -> Result<Started, SpawnError> {
    #[cfg(windows)]
    return crate::win::launch::spawn(spec);
    #[cfg(not(windows))]
    {
        let _ = spec;
        Err(SpawnError::unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn env_overrides_parse_set_and_unset_in_order() {
        let e = EnvOverrides::from_json(&json!({"B_VAR": "2", "A_VAR": null, "Grüße": "안녕"}))
            .unwrap();
        // serde_json keeps object keys sorted; the order is that of the parsed object.
        assert_eq!(e.vars.len(), 3);
        let n = e.names();
        assert_eq!(n.unset, vec!["A_VAR".to_string()]);
        assert!(n.set.contains(&"Grüße".to_string()) && n.set.contains(&"B_VAR".to_string()));
        assert!(
            EnvOverrides::from_json(&Value::Null)
                .unwrap()
                .vars
                .is_empty()
        );
        assert!(EnvOverrides::from_json(&json!({})).unwrap().vars.is_empty());
    }

    #[test]
    fn env_overrides_are_validated_without_echoing_values() {
        let secret = "s3cr3t-value-never-shown";
        for (bad, why) in [
            (json!("FOO=1"), "an object"),
            (json!(["FOO"]), "an object"),
            (json!({"": "x"}), "empty"),
            (json!({"A=B": "x"}), "contains `=`"),
            (json!({"TAB\tNAME": "x"}), "control"),
            (json!({"X": 1}), "must be a string"),
            (json!({"X": true}), "must be a string"),
            (json!({"TOKEN": format!("{secret}\0")}), "NUL"),
            (
                json!({"TOKEN": "x".repeat(MAX_ENV_VALUE + 1)}),
                "longer than",
            ),
            (json!({"subak_pane_id": "p-1"}), "set by the Host"),
            (json!({"SUBAK_TERMINAL_ID": null}), "set by the Host"),
            (json!({"N".repeat(MAX_ENV_NAME + 1): "x"}), "longer than"),
        ] {
            let e = EnvOverrides::from_json(&bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
            assert!(!e.contains(secret), "a value leaked into `{e}`");
        }
        let many: serde_json::Map<String, Value> = (0..=MAX_ENV_OVERRIDES)
            .map(|i| (format!("V{i}"), json!("x")))
            .collect();
        assert!(
            EnvOverrides::from_json(&Value::Object(many))
                .unwrap_err()
                .contains("at most")
        );
        // Names ignore case: two spellings of one variable are refused.
        let e = EnvOverrides::from_json(&json!({"Path": "a", "PATH": "b"})).unwrap_err();
        assert!(e.contains("twice"), "{e}");
        assert!(EnvOverrides::from_json(&json!({"MAX": "x".repeat(MAX_ENV_VALUE)})).is_ok());
    }

    #[test]
    fn variable_names_compare_like_windows() {
        assert!(same_var("Path", "PATH"));
        assert!(same_var("ÄBC", "äbc"));
        // One-to-one mapping only: `ß` has no single uppercase character, so it stays itself.
        assert!(!same_var("grüße", "GRÜSSE"));
        assert!(!same_var("PATH", "PATHEXT"));
        assert_eq!(var_key("Path"), "PATH");
    }

    #[test]
    fn reasons_have_stable_names() {
        let all = [
            Reason::NotFound,
            Reason::AccessDenied,
            Reason::BadCwd,
            Reason::NotExecutable,
            Reason::Other,
            Reason::UnsupportedPlatform,
        ];
        let names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            [
                "not_found",
                "access_denied",
                "bad_cwd",
                "not_executable",
                "other",
                "unsupported_platform"
            ]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_stub_answers_unsupported_platform() {
        let env = EnvOverrides::default();
        let spec = Spec {
            program: "sh",
            args: &[],
            raw_line: None,
            cwd: Path::new("/"),
            env: &env,
            host_vars: &[],
            size: (80, 24),
        };
        assert_eq!(
            spawn(&spec).unwrap_err().reason,
            Reason::UnsupportedPlatform
        );
        assert_eq!(plan(&spec).unwrap_err().reason, Reason::UnsupportedPlatform);
    }
}
