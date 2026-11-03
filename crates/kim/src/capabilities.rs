//! Capability documents and loose, orchestrator-free discovery (docs/CLI_CONTRACT.md §9,
//! ADR 0015, KIM-FR-020).
//!
//! Every Kim tool answers `<tool> capabilities --json` with a small, static document that lists
//! its commands and what each one may do (read, write, install, destructive; network; dry-run;
//! confirmation; window). A package may also ship the same document as `capabilities.json` at
//! its root, so `kim tools list|describe` can show capabilities by reading a file — without
//! starting the tool. Starting the tool to ask it (`--probe`) is opt-in, bounded by a timeout,
//! headless (no window, no stdin) and refused for anything but the `capabilities` command.
//!
//! Nothing here runs in the background or keeps a registry: every answer is computed from files
//! and short-lived processes when someone asks.

use crate::error::{ErrorCode, KimError, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `schema` value of a capabilities document.
pub const SCHEMA: &str = "kim.capabilities";
/// Version of the capabilities document format this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
/// Major version of docs/CLI_CONTRACT.md that this build implements.
pub const CLI_CONTRACT: u32 = 1;
/// `schema_version` of the `--json` envelope this build emits.
pub const JSON_SCHEMA_VERSION: u32 = 1;
/// File name of the static capabilities manifest at the root of an installed version.
pub const MANIFEST_FILE: &str = "capabilities.json";
/// Upper bound for a capabilities document or a probe's stdout.
pub const MAX_DOCUMENT_BYTES: u64 = 1024 * 1024;
/// Default time limit for `capabilities --json` of another tool.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A tool's self-description (`kim.capabilities` v1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub schema: String,
    pub schema_version: u32,
    pub tool: String,
    pub version: String,
    /// Major version of the CLI contract the tool implements.
    pub cli_contract: u32,
    /// `schema_version` of the tool's `--json` envelope.
    pub json_schema_version: u32,
    /// `implemented`, or `placeholder` for a tool that is not functional yet.
    pub implementation: Implementation,
    pub summary: String,
    pub commands: Vec<CommandInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Implementation {
    Implemented,
    Placeholder,
}

/// One command of a tool, with the consent-relevant facts a caller needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// Dotted command path, the same string as the `command` field of its `--json` output.
    pub name: String,
    pub usage: String,
    pub summary: String,
    /// The strongest effect the command can have with any of its flags.
    pub effect: Effect,
    /// May use the network (only ever when this command is invoked explicitly).
    pub network: bool,
    /// Supports `--dry-run` (required for every effect other than `read`).
    pub dry_run: bool,
    /// Needs `--yes` when it cannot ask (non-interactive, `--json`, `--no-input`).
    pub requires_yes: bool,
    /// Opens a window. Headless callers never invoke such a command.
    pub opens_window: bool,
}

/// What a command may change. Callers treat an unknown value like `destructive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    /// Changes nothing.
    Read,
    /// Changes only the tool's own settings or caches (reversible).
    Write,
    /// Adds, replaces or activates installed software.
    Install,
    /// Removes software or user data, or ends running work.
    Destructive,
    /// A value from a newer contract; treated as the strongest effect.
    #[serde(other)]
    Unknown,
}

impl Capabilities {
    pub fn command(&self, name: &str) -> Option<&CommandInfo> {
        self.commands.iter().find(|c| c.name == name)
    }
}

/// Parses and checks a capabilities document claimed to describe `tool` (and, when given,
/// exactly `version`).
pub fn parse(bytes: &[u8], tool: &str, version: Option<&str>) -> Result<Capabilities> {
    let invalid = |m: String| {
        KimError::new(ErrorCode::CapabilitiesInvalid, m)
            .with_hint("the tool's capabilities document is malformed; reinstall or report it")
    };
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return Err(invalid(format!(
            "{tool}'s capabilities document exceeds {MAX_DOCUMENT_BYTES} bytes"
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| invalid(format!("{tool}'s capabilities document is not JSON: {e}")))?;
    from_value(&value, tool, version)
}

fn from_value(
    value: &serde_json::Value,
    tool: &str,
    version: Option<&str>,
) -> Result<Capabilities> {
    let invalid = |m: String| {
        KimError::new(ErrorCode::CapabilitiesInvalid, m)
            .with_hint("the tool's capabilities document is malformed; reinstall or report it")
    };
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCHEMA) {
        return Err(invalid(format!(
            "{tool} answered something that is not a `{SCHEMA}` document"
        )));
    }
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if schema_version != Some(u64::from(SCHEMA_VERSION)) {
        return Err(KimError::new(
            ErrorCode::ToolIncompatible,
            format!(
                "{tool} uses capabilities schema version {}; this Kim reads version {SCHEMA_VERSION}",
                schema_version.map_or_else(|| "?".to_string(), |v| v.to_string())
            ),
        )
        .with_hint("update Kim (or install a matching version of the tool)"));
    }
    let doc: Capabilities = serde_json::from_value(value.clone())
        .map_err(|e| invalid(format!("{tool}'s capabilities document: {e}")))?;
    if doc.tool != tool {
        return Err(invalid(format!(
            "the capabilities document of `{tool}` describes `{}`",
            doc.tool
        )));
    }
    if let Some(v) = version
        && doc.version != v
    {
        return Err(invalid(format!(
            "the capabilities document of {tool} {v} describes version {}",
            doc.version
        )));
    }
    Ok(doc)
}

/// Reads the static manifest shipped with an installed version, if there is one.
pub fn read_manifest(
    version_dir: &Path,
    tool: &str,
    version: &str,
) -> Option<Result<Capabilities>> {
    let path = version_dir.join(MANIFEST_FILE);
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(KimError::io(&path, &e))),
        Ok(bytes) => Some(parse(&bytes, tool, Some(version))),
    }
}

/// Asks a tool directly: runs `<entrypoint> capabilities --json` headless (no window, stdin
/// closed, `KIM_NO_INPUT=1`) and bounded by `timeout`, then validates the answer.
pub fn probe(
    entrypoint: &Path,
    tool: &str,
    version: Option<&str>,
    timeout: Duration,
) -> Result<Capabilities> {
    let mut cmd = Command::new(entrypoint);
    cmd.args(["capabilities", "--json"])
        .env("KIM_NO_INPUT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = run_bounded(cmd, timeout, MAX_DOCUMENT_BYTES).map_err(|e| match e.code {
        ErrorCode::Io if !entrypoint.exists() => KimError::new(
            ErrorCode::NotInstalled,
            format!(
                "{tool}'s program {} is missing; the install looks damaged",
                entrypoint.display()
            ),
        )
        .with_hint(format!(
            "repair it explicitly with `kim install {tool} --reinstall`"
        )),
        _ => e,
    })?;
    let unavailable = || {
        KimError::new(
            ErrorCode::CapabilitiesUnavailable,
            format!(
                "{tool} did not answer `capabilities --json` (exit code {})",
                out.exit_code
                    .map_or_else(|| "none".to_string(), |c| c.to_string())
            ),
        )
        .with_hint("this version of the tool predates the Kim CLI contract; update it explicitly")
    };
    let Ok(envelope) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Err(unavailable());
    };
    if envelope.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let code = envelope
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        return Err(if code == "usage" {
            unavailable()
        } else {
            KimError::new(
                ErrorCode::CapabilitiesInvalid,
                format!("{tool} failed to describe itself: error `{code}`"),
            )
        });
    }
    if envelope.get("command").and_then(serde_json::Value::as_str) != Some("capabilities") {
        return Err(KimError::new(
            ErrorCode::CapabilitiesInvalid,
            format!("{tool} answered `capabilities --json` with something else"),
        ));
    }
    let data = envelope.get("data").cloned().unwrap_or_default();
    from_value(&data, tool, version)
}

/// Output of a bounded child process.
#[derive(Debug, Clone)]
pub struct BoundedOutput {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
}

/// Runs `cmd` with a time limit and a cap on captured stdout. The child gets no console window
/// on Windows. On timeout the child is killed and `timeout` is returned.
pub fn run_bounded(mut cmd: Command, timeout: Duration, max_stdout: u64) -> Result<BoundedOutput> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .spawn()
        .map_err(|e| KimError::new(ErrorCode::Io, format!("cannot start {program}: {e}")))?;
    let stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout {
            let _ = out.take(max_stdout + 1).read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(KimError::new(
                    ErrorCode::Timeout,
                    format!("{program} did not finish within {timeout:?} and was stopped"),
                )
                .with_hint("the tool may be hung or too old; retry with a longer --timeout"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(KimError::new(ErrorCode::Io, format!("{program}: {e}"))),
        }
    };
    // A grandchild could hold the pipe open: never wait past the deadline for the reader.
    let remaining = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(100));
    let stdout = rx.recv_timeout(remaining).unwrap_or_default();
    if stdout.len() as u64 > max_stdout {
        return Err(KimError::new(
            ErrorCode::CapabilitiesInvalid,
            format!("{program} printed more than {max_stdout} bytes"),
        ));
    }
    Ok(BoundedOutput {
        exit_code: status.code(),
        stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(tool: &str, version: &str) -> serde_json::Value {
        serde_json::json!({
            "schema": SCHEMA, "schema_version": 1, "tool": tool, "version": version,
            "cli_contract": 1, "json_schema_version": 1, "implementation": "implemented",
            "summary": "s",
            "commands": [{"name": "capabilities", "usage": "t capabilities", "summary": "s",
                          "effect": "read", "network": false, "dry_run": false,
                          "requires_yes": false, "opens_window": false}]
        })
    }

    #[test]
    fn valid_documents_parse_and_identity_is_checked() {
        let bytes = serde_json::to_vec(&doc("t", "1.0.0")).unwrap();
        let c = parse(&bytes, "t", Some("1.0.0")).unwrap();
        assert_eq!(c.command("capabilities").unwrap().effect, Effect::Read);
        assert_eq!(
            parse(&bytes, "other", None).unwrap_err().code,
            ErrorCode::CapabilitiesInvalid
        );
        assert_eq!(
            parse(&bytes, "t", Some("2.0.0")).unwrap_err().code,
            ErrorCode::CapabilitiesInvalid
        );
        assert_eq!(
            parse(b"{nope", "t", None).unwrap_err().code,
            ErrorCode::CapabilitiesInvalid
        );
    }

    #[test]
    fn a_newer_schema_is_incompatible_and_unknown_effects_are_tolerated() {
        let mut v = doc("t", "1.0.0");
        v["schema_version"] = 2.into();
        let e = parse(&serde_json::to_vec(&v).unwrap(), "t", None).unwrap_err();
        assert_eq!(e.code, ErrorCode::ToolIncompatible);
        let mut v = doc("t", "1.0.0");
        v["commands"][0]["effect"] = "teleport".into();
        let c = parse(&serde_json::to_vec(&v).unwrap(), "t", None).unwrap();
        assert_eq!(c.commands[0].effect, Effect::Unknown);
        assert!(
            Effect::Unknown > Effect::Destructive,
            "unknown ranks strongest"
        );
    }

    #[test]
    fn missing_manifest_is_none() {
        let dir = std::env::temp_dir().join(format!("kim-caps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_manifest(&dir, "t", "1.0.0").is_none());
        std::fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_vec(&doc("t", "1.0.0")).unwrap(),
        )
        .unwrap();
        assert!(read_manifest(&dir, "t", "1.0.0").unwrap().is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    #[cfg(windows)]
    fn bounded_runs_kill_a_hung_child_on_timeout() {
        // PING with 30 echo requests takes ~29 s; the limit is 300 ms.
        let mut cmd = Command::new(r"C:\Windows\System32\PING.EXE");
        cmd.args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let started = Instant::now();
        let e = run_bounded(cmd, Duration::from_millis(300), 1024).unwrap_err();
        assert_eq!(e.code, ErrorCode::Timeout);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "child was killed"
        );
    }

    #[test]
    fn a_missing_program_is_reported() {
        let e = probe(
            Path::new("definitely/not/here/tool.exe"),
            "t",
            None,
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotInstalled);
    }
}
