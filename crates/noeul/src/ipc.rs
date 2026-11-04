//! Noeul IPC v1: how the CLI controls the running instance (ADR 0023 §4, NOEUL-FR-003,
//! NOEUL-SEC-011). Schema: `docs/contracts/schemas/noeul-ipc-v1.schema.json`.
//!
//! **Transport** (Windows, `win::pipe`): the per-user, per-session named pipe
//! `\\.\pipe\mouv-noeul-<user SID>-s<session>`, local only, owner-only DACL, first instance
//! exclusive (the pipe itself is the single-instance guard). The client checks that the server
//! process runs as the same user *and* is a `noeul.exe` before sending anything; the server
//! checks the user SID of each connection's own token. No network listener exists.
//!
//! **Framing:** a 4-byte little-endian length, then that many bytes of one UTF-8 JSON object.
//! At most [`MAX_FRAME`] bytes per frame (requests and answers are small: pictures never cross
//! the pipe, the instance captures them itself). A malformed frame ends the connection after a
//! connection-level `error` message; the instance keeps running.
//!
//! **Messages:** `hello` (client, within [`HANDSHAKE_TIMEOUT`]) → `welcome` (server, with the
//! negotiated protocol major), then any number of `request` → `response` pairs in order. Errors
//! carry a registered CLI error code (docs/CLI_CONTRACT.md §6), so the CLI passes them through.

use crate::error::{Code, NoeulError};
use serde_json::{Map, Value, json};
use std::io::{self, Read};
use std::time::Duration;

pub const PROTOCOL_MIN: u64 = 1;
pub const PROTOCOL_MAX: u64 = 1;
/// Largest frame body in bytes.
pub const MAX_FRAME: usize = 64 * 1024;
/// How long the server waits for `hello`, and the client for `welcome`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a client waits for one answer (a capture of a large region takes well under this).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Methods of v1 and whether they change state.
pub const METHODS: &[(&str, bool)] = &[
    ("status", false),
    ("activate", false),
    ("capture", true),
    ("capture.region", true),
    ("capture.cancel", true),
    ("pin.list", false),
    ("pin.close", true),
    ("pin.reopen", true),
    ("hotkeys.reload", true),
    ("quit", true),
];

/// One frame: length prefix and body.
pub fn encode(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).expect("a JSON value serializes");
    let len = u32::try_from(body.len()).expect("frames are small");
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Why a frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The stream ended inside a frame.
    Truncated,
    /// The announced length exceeds [`MAX_FRAME`] (or is zero).
    BadLength(u32),
    /// The body is not one JSON object.
    NotJson(String),
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Truncated => f.write_str("the connection ended inside a frame"),
            FrameError::BadLength(n) => {
                write!(f, "frame length {n} is not between 1 and {MAX_FRAME} bytes")
            }
            FrameError::NotJson(e) => write!(f, "the frame is not a JSON object: {e}"),
            FrameError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<bool, FrameError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => {
                return if got == 0 {
                    Ok(false)
                } else {
                    Err(FrameError::Truncated)
                };
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    Ok(true)
}

/// Reads one frame. `Ok(None)` when the peer closed the connection between frames.
pub fn read_frame(r: &mut impl Read) -> Result<Option<Value>, FrameError> {
    let mut len = [0u8; 4];
    if !read_full(r, &mut len)? {
        return Ok(None);
    }
    let n = u32::from_le_bytes(len);
    if n == 0 || n as usize > MAX_FRAME {
        return Err(FrameError::BadLength(n));
    }
    let mut body = vec![0u8; n as usize];
    if !read_full(r, &mut body)? {
        return Err(FrameError::Truncated);
    }
    let v: Value = serde_json::from_slice(&body).map_err(|e| FrameError::NotJson(e.to_string()))?;
    if !v.is_object() {
        return Err(FrameError::NotJson("not an object".into()));
    }
    Ok(Some(v))
}

pub fn hello(client: &str) -> Value {
    json!({
        "type": "hello",
        "protocol": {"min": PROTOCOL_MIN, "max": PROTOCOL_MAX},
        "client": {"name": client, "version": crate::VERSION, "pid": std::process::id()},
    })
}

pub fn welcome(protocol: u64, pid: u32, started_at: &str) -> Value {
    json!({
        "type": "welcome",
        "protocol": protocol,
        "instance": {"pid": pid, "version": crate::VERSION, "started_at": started_at},
    })
}

pub fn request(id: u64, method: &str, params: Value) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), json!("request"));
    m.insert("id".into(), json!(id));
    m.insert("method".into(), json!(method));
    m.insert("params".into(), params);
    Value::Object(m)
}

pub fn ok(id: u64, result: Value) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), json!("response"));
    m.insert("id".into(), json!(id));
    m.insert("result".into(), result);
    Value::Object(m)
}

fn error_object(e: &NoeulError) -> Value {
    let mut m = Map::new();
    m.insert("code".into(), json!(e.code.as_str()));
    m.insert("message".into(), json!(e.message));
    if let Some(h) = &e.hint {
        m.insert("hint".into(), json!(h));
    }
    if !e.details.is_empty() {
        m.insert("details".into(), json!(e.details));
    }
    Value::Object(m)
}

pub fn err(id: u64, e: &NoeulError) -> Value {
    json!({"type": "response", "id": id, "error": error_object(e)})
}

/// A connection-level error; the sender closes the connection after it.
pub fn connection_error(e: &NoeulError) -> Value {
    json!({"type": "error", "error": error_object(e)})
}

/// Reads the `error` object of a response or connection error back into a [`NoeulError`].
/// Unknown codes become `internal` (CLI_CONTRACT §8: unknown codes are handled by class).
pub fn parse_error(v: &Value) -> NoeulError {
    let code = v["code"]
        .as_str()
        .and_then(Code::parse)
        .unwrap_or(Code::Internal);
    let mut e = NoeulError::new(
        code,
        v["message"]
            .as_str()
            .unwrap_or("the instance reported an error"),
    );
    if let Some(h) = v["hint"].as_str() {
        e = e.hint(h);
    }
    if let Some(d) = v["details"].as_array() {
        e = e.details(
            d.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        );
    }
    e
}

/// A validated `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub min: u64,
    pub max: u64,
    pub name: String,
}

fn protocol_error(msg: impl Into<String>) -> NoeulError {
    NoeulError::new(Code::ProtocolError, msg)
}

pub fn parse_hello(v: &Value) -> Result<Hello, NoeulError> {
    if v["type"] != "hello" {
        return Err(protocol_error("expected hello"));
    }
    let min = v["protocol"]["min"].as_u64();
    let max = v["protocol"]["max"].as_u64();
    let name = v["client"]["name"].as_str();
    match (min, max, name) {
        (Some(min), Some(max), Some(name)) if min >= 1 && min <= max && !name.is_empty() => {
            Ok(Hello {
                min,
                max,
                name: name.chars().take(64).collect(),
            })
        }
        _ => Err(protocol_error("malformed hello")),
    }
}

/// The highest common protocol major.
pub fn negotiate(min: u64, max: u64) -> Option<u64> {
    let hi = max.min(PROTOCOL_MAX);
    (hi >= min.max(PROTOCOL_MIN)).then_some(hi)
}

/// A validated request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// Validates a request frame. `Err((id, error))`: answer with that error (the id is 0 when it
/// could not be read, which the schema reserves for that case).
pub fn parse_request(v: &Value) -> Result<Request, (u64, NoeulError)> {
    if v["type"] != "request" {
        return Err((0, protocol_error("expected a request")));
    }
    let Some(id) = v["id"].as_u64().filter(|i| *i >= 1) else {
        return Err((0, protocol_error("a request needs an id of at least 1")));
    };
    let Some(method) = v["method"].as_str() else {
        return Err((id, protocol_error("a request needs a method")));
    };
    if !METHODS.iter().any(|(m, _)| *m == method) {
        return Err((
            id,
            NoeulError::new(Code::Usage, format!("unknown method `{method}`"))
                .hint("this instance speaks Noeul IPC v1"),
        ));
    }
    let params = match v.get("params") {
        None => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => return Err((id, NoeulError::new(Code::Usage, "params must be an object"))),
    };
    Ok(Request {
        id,
        method: method.to_string(),
        params,
    })
}

/// A boolean parameter (absent = false).
pub fn flag(params: &Value, name: &str) -> Result<bool, NoeulError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(NoeulError::new(
            Code::Usage,
            format!("parameter `{name}` must be true or false"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bytes: &[u8]) -> Result<Option<Value>, FrameError> {
        read_frame(&mut &bytes[..])
    }

    #[test]
    fn frames_round_trip_and_bad_frames_are_refused() {
        let msg = request(7, "status", json!({}));
        let bytes = encode(&msg);
        assert_eq!(&bytes[..4], &(bytes.len() as u32 - 4).to_le_bytes());
        assert_eq!(read(&bytes).unwrap(), Some(msg.clone()));
        // Two frames back to back.
        let mut two = bytes.clone();
        two.extend(encode(&ok(7, json!({"a": 1}))));
        let mut r = &two[..];
        assert_eq!(read_frame(&mut r).unwrap(), Some(msg));
        assert_eq!(read_frame(&mut r).unwrap().unwrap()["result"]["a"], 1);
        assert!(
            read_frame(&mut r).unwrap().is_none(),
            "clean end between frames"
        );

        assert!(matches!(read(&bytes[..2]), Err(FrameError::Truncated)));
        assert!(matches!(
            read(&bytes[..bytes.len() - 1]),
            Err(FrameError::Truncated)
        ));
        assert!(matches!(
            read(&0u32.to_le_bytes()),
            Err(FrameError::BadLength(0))
        ));
        let big = (MAX_FRAME as u32 + 1).to_le_bytes();
        assert!(matches!(read(&big), Err(FrameError::BadLength(_))));
        let mut junk = 5u32.to_le_bytes().to_vec();
        junk.extend_from_slice(b"nope!");
        assert!(matches!(read(&junk), Err(FrameError::NotJson(_))));
        let mut arr = 2u32.to_le_bytes().to_vec();
        arr.extend_from_slice(b"[]");
        assert!(matches!(read(&arr), Err(FrameError::NotJson(_))));
    }

    #[test]
    fn handshake_and_requests_are_validated() {
        let h = parse_hello(&hello("noeul-cli")).unwrap();
        assert_eq!((h.min, h.max, h.name.as_str()), (1, 1, "noeul-cli"));
        assert_eq!(negotiate(h.min, h.max), Some(1));
        assert_eq!(negotiate(2, 3), None, "a newer-only client is refused");
        assert_eq!(negotiate(1, 9), Some(1));
        for bad in [
            json!({"type": "hello"}),
            json!({"type": "hello", "protocol": {"min": 2, "max": 1}, "client": {"name": "x"}}),
            json!({"type": "hello", "protocol": {"min": 1, "max": 1}, "client": {"name": ""}}),
            json!({"type": "request", "id": 1, "method": "status"}),
        ] {
            assert_eq!(
                parse_hello(&bad).unwrap_err().code,
                Code::ProtocolError,
                "{bad}"
            );
        }

        let r = parse_request(&request(3, "pin.list", json!({}))).unwrap();
        assert_eq!((r.id, r.method.as_str()), (3, "pin.list"));
        let r = parse_request(&json!({"type": "request", "id": 4, "method": "status"})).unwrap();
        assert_eq!(r.params, json!({}), "params default to an empty object");
        assert_eq!(
            parse_request(&json!({"type": "request", "method": "status"}))
                .unwrap_err()
                .0,
            0
        );
        let (id, e) = parse_request(&request(5, "format.disk", json!({}))).unwrap_err();
        assert_eq!((id, e.code), (5, Code::Usage));
        let (_, e) =
            parse_request(&json!({"type": "request", "id": 6, "method": "status", "params": 3}))
                .unwrap_err();
        assert_eq!(e.code, Code::Usage);
        assert!(flag(&json!({"dry_run": true}), "dry_run").unwrap());
        assert!(!flag(&json!({}), "dry_run").unwrap());
        assert!(flag(&json!({"dry_run": "yes"}), "dry_run").is_err());
    }

    #[test]
    fn errors_round_trip_with_registered_codes() {
        let e = NoeulError::new(Code::PinNotFound, "no pin `pin-9`")
            .hint("run `noeul pin list`")
            .detail("open pins: 0");
        let back = parse_error(&err(2, &e)["error"]);
        assert_eq!(back, e);
        let unknown = parse_error(&json!({"code": "from_the_future", "message": "?"}));
        assert_eq!(unknown.code, Code::Internal);
    }
}
