//! The client side of the CLI ↔ app protocol (ADR 0024 §5), shared by `ddalgak` (the CLI) and
//! `ddalgak-editor` (Step 74, ADR 0049): connect, verify **before sending anything** that the
//! pipe is served by `ddalgak-gui.exe` of the same user, send one request line, read one response
//! line with a timeout. Neither client ever starts the app (`DDALGAK-FR-060`).

use super::pipe;
use crate::ipc::{self, Method};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Who answered, as verified before sending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub pid: u32,
    pub image: PathBuf,
    pub latency_ms: u128,
}

/// Why a call failed: a CLI error code (`engine_not_running`, `app_unresponsive`,
/// `app_identity_mismatch`, `protocol_error` or the app's own code) and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

impl CallError {
    fn new(code: &str, message: impl Into<String>) -> CallError {
        CallError {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// One request to the running app, after verifying who serves the pipe.
pub fn call(endpoint: &str, method: &Method, dry_run: bool) -> Result<(Value, Peer), CallError> {
    let started = Instant::now();
    let conn = pipe::connect(endpoint, Duration::from_secs(2)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CallError::new("engine_not_running", "Ddalgak is not running")
        } else {
            CallError::new(
                "app_unresponsive",
                format!("cannot connect to the app: {e}"),
            )
        }
    })?;
    let pid = conn.server_pid().ok_or_else(|| {
        CallError::new(
            "app_identity_mismatch",
            "the endpoint's server cannot be identified",
        )
    })?;
    let image = super::process_image(pid).map_err(|e| {
        CallError::new(
            "app_identity_mismatch",
            format!("cannot verify the process serving the endpoint (pid {pid}): {e}"),
        )
    })?;
    let name = image
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let same_user = super::process_user_sid(pid).ok() == super::user_sid().ok();
    if !name.eq_ignore_ascii_case(crate::GUI_EXE) || !same_user {
        return Err(CallError::new(
            "app_identity_mismatch",
            format!(
                "the endpoint is served by `{name}` (pid {pid}){}, not by ddalgak-gui.exe of this user; nothing was sent",
                if same_user { "" } else { " of another user" }
            ),
        ));
    }
    conn.write_all_bytes(ipc::request(method, dry_run).as_bytes())
        .map_err(|e| CallError::new("app_unresponsive", format!("cannot send: {e}")))?;
    let deadline = Instant::now() + Duration::from_millis(ipc::TIMEOUT_MS + 500);
    let line = pipe::read_line(&conn, ipc::MAX_RESPONSE, deadline).map_err(|e| {
        CallError::new(
            "app_unresponsive",
            format!("the app did not answer ({e}); it is neither stopped nor restarted"),
        )
    })?;
    let latency_ms = started.elapsed().as_millis();
    let result =
        ipc::parse_response(&line).map_err(|(code, message)| CallError { code, message })?;
    Ok((
        result,
        Peer {
            pid,
            image,
            latency_ms,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_app_is_engine_not_running_and_nothing_is_started() {
        let endpoint = format!("ddalgak-client-unit-{}", std::process::id());
        let e = call(&endpoint, &Method::Status, true).unwrap_err();
        assert_eq!(e.code, "engine_not_running");
    }
}
