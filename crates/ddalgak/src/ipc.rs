//! The CLI ↔ tray-app protocol (ADR 0024 §5), following the pattern of ADR 0005/0019.
//!
//! - **Transport:** a named pipe `\\.\pipe\mouv-ddalgak-<user SID>-s<session>`, local only,
//!   owner-only DACL, first instance created by the app (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
//! - **Identity:** the client checks, before sending anything, that the server process is
//!   `ddalgak-gui.exe` of the same user; the app checks the client's user SID from the
//!   connection's token. A mismatch ends the connection without an answer.
//! - **Framing:** one request and one response per connection, each one JSON document on one
//!   line (UTF-8, `\n`), at most [`MAX_REQUEST`] / [`MAX_RESPONSE`] bytes, with a read timeout.
//! - **Content:** method names, flags, states, rule IDs and counters. Never keys or rule content.
//!
//! The CLI never starts the app (`DDALGAK-FR-060`): no app, no pipe, `engine_not_running`.

use serde_json::{Value, json};

/// Protocol version; a different major is refused with `protocol_error`.
pub const PROTOCOL: u64 = 1;
pub const MAX_REQUEST: usize = 4096;
pub const MAX_RESPONSE: usize = 256 * 1024;
/// How long either side waits for the other's line.
pub const TIMEOUT_MS: u64 = 3000;

/// A mode change the CLI asks for (Step 72). Mode IDs only; the app resolves them in its active
/// profile.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ModeCmd {
    Set(String),
    Toggle(String),
    /// Back to the default mode (the safe reset).
    Reset,
}

/// What a client may ask.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Method {
    Status,
    Health,
    Engine(crate::lifecycle::Request),
    /// End the tray app (engine off first, held keys released).
    Quit,
    /// Read `rules.json` again now and apply it if it is valid (Step 67; the app also reloads on
    /// file-change notifications).
    RulesReload,
    /// Change the active mode of the running engine (Step 72).
    Mode(ModeCmd),
    /// Step 73: release every key and button Ddalgak holds, forget the hook's key state and
    /// release keys Windows still considers pressed that nobody holds (`ddalgak release`).
    Release,
}

impl Method {
    pub fn name(&self) -> String {
        match self {
            Method::Status => "status".into(),
            Method::Health => "health".into(),
            Method::Engine(r) => format!("engine.{}", r.as_str()),
            Method::Quit => "app.quit".into(),
            Method::RulesReload => "rules.reload".into(),
            Method::Mode(ModeCmd::Set(_)) => "mode.set".into(),
            Method::Mode(ModeCmd::Toggle(_)) => "mode.toggle".into(),
            Method::Mode(ModeCmd::Reset) => "mode.reset".into(),
            Method::Release => "keys.release".into(),
        }
    }

    pub fn parse(name: &str) -> Option<Method> {
        Some(match name {
            "status" => Method::Status,
            "health" => Method::Health,
            "app.quit" => Method::Quit,
            "rules.reload" => Method::RulesReload,
            "keys.release" => Method::Release,
            other => {
                let r = crate::lifecycle::Request::parse(other.strip_prefix("engine.")?)?;
                if r == crate::lifecycle::Request::Emergency {
                    return None; // only the physical chord trips the emergency off
                }
                Method::Engine(r)
            }
        })
    }
}

/// One request line.
pub fn request(method: &Method, dry_run: bool) -> String {
    let mut v = json!({
        "ddalgak_ipc": PROTOCOL,
        "method": method.name(),
        "dry_run": dry_run,
    });
    if let Method::Mode(ModeCmd::Set(m) | ModeCmd::Toggle(m)) = method {
        v["mode"] = json!(m);
    }
    let mut line = v.to_string();
    line.push('\n');
    line
}

/// Parses a request line: the method and `dry_run`, or an error code and message.
pub fn parse_request(line: &[u8]) -> Result<(Method, bool), (&'static str, String)> {
    let v: Value = serde_json::from_slice(line)
        .map_err(|e| ("protocol_error", format!("not a JSON request: {e}")))?;
    if v["ddalgak_ipc"].as_u64() != Some(PROTOCOL) {
        return Err(("protocol_error", "unsupported protocol version".into()));
    }
    let name = v["method"].as_str().unwrap_or_default();
    let mode = || {
        v["mode"]
            .as_str()
            .filter(|m| crate::model::is_short_id(m))
            .map(str::to_string)
            .ok_or(("usage", format!("`{name}` needs a mode ID")))
    };
    let method = match name {
        "mode.set" => Method::Mode(ModeCmd::Set(mode()?)),
        "mode.toggle" => Method::Mode(ModeCmd::Toggle(mode()?)),
        "mode.reset" => Method::Mode(ModeCmd::Reset),
        _ => Method::parse(name).ok_or(("usage", format!("unknown method `{name}`")))?,
    };
    Ok((method, v["dry_run"].as_bool().unwrap_or(false)))
}

/// A success line.
pub fn ok(result: &Value) -> String {
    let mut line = json!({ "ddalgak_ipc": PROTOCOL, "ok": true, "result": result }).to_string();
    line.push('\n');
    line
}

/// A failure line.
pub fn err(code: &str, message: &str) -> String {
    let mut line = json!({
        "ddalgak_ipc": PROTOCOL,
        "ok": false,
        "error": { "code": code, "message": message },
    })
    .to_string();
    line.push('\n');
    line
}

/// Parses a response line into the result or `(code, message)`.
pub fn parse_response(line: &[u8]) -> Result<Value, (String, String)> {
    let v: Value = serde_json::from_slice(line).map_err(|e| {
        (
            "protocol_error".to_string(),
            format!("not a JSON response: {e}"),
        )
    })?;
    if v["ddalgak_ipc"].as_u64() != Some(PROTOCOL) {
        return Err((
            "protocol_error".into(),
            "the app speaks another protocol version".into(),
        ));
    }
    if v["ok"] == true {
        Ok(v["result"].clone())
    } else {
        Err((
            v["error"]["code"]
                .as_str()
                .unwrap_or("internal")
                .to_string(),
            v["error"]["message"].as_str().unwrap_or("").to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::Request;

    #[test]
    fn requests_round_trip_and_the_emergency_is_not_a_method() {
        for m in [
            Method::Status,
            Method::Health,
            Method::Quit,
            Method::RulesReload,
            Method::Engine(Request::Start),
            Method::Engine(Request::Pause),
            Method::Engine(Request::Resume),
            Method::Engine(Request::Stop),
            Method::Mode(ModeCmd::Set("nav".into())),
            Method::Mode(ModeCmd::Toggle("sym-2".into())),
            Method::Mode(ModeCmd::Reset),
            Method::Release,
        ] {
            let line = request(&m, true);
            assert!(line.ends_with('\n') && line.len() < MAX_REQUEST);
            assert_eq!(parse_request(line.trim_end().as_bytes()), Ok((m, true)));
        }
        assert_eq!(Method::parse("engine.emergency"), None);
        // A mode request names one mode ID, nothing else.
        for bad in [
            br#"{"ddalgak_ipc":1,"method":"mode.set"}"#.as_slice(),
            br#"{"ddalgak_ipc":1,"method":"mode.set","mode":"Not An ID"}"#.as_slice(),
            br#"{"ddalgak_ipc":1,"method":"mode.toggle","mode":7}"#.as_slice(),
        ] {
            assert_eq!(parse_request(bad).unwrap_err().0, "usage");
        }
        assert_eq!(
            parse_request(br#"{"ddalgak_ipc":1,"method":"keylog"}"#)
                .unwrap_err()
                .0,
            "usage"
        );
        assert_eq!(
            parse_request(br#"{"ddalgak_ipc":2,"method":"status"}"#)
                .unwrap_err()
                .0,
            "protocol_error"
        );
        assert_eq!(parse_request(b"\xff").unwrap_err().0, "protocol_error");
    }

    #[test]
    fn responses_round_trip() {
        assert_eq!(
            parse_response(ok(&json!({"a": 1})).as_bytes()).unwrap()["a"],
            1
        );
        assert_eq!(
            parse_response(err("engine_elevated", "no").as_bytes()).unwrap_err(),
            ("engine_elevated".to_string(), "no".to_string())
        );
    }
}
