//! Subak IPC v1 (docs/contracts/IPC_PROTOCOL.md, ADR 0005, ADR 0019): framing, message
//! construction and validation, version negotiation and the per-kind authorization table.
//!
//! Portable: no OS calls. The Windows named-pipe transport is `win::pipe`; the Host and the
//! client SDK use only what is defined here to speak the protocol.
//!
//! Frame: `u32 LE length | u8 kind | payload`, `length` covering kind + payload. Control payload:
//! one UTF-8 JSON object. Data payload: `u32 LE stream | u64 LE seq | u8 flags | bytes`; `seq` is
//! the byte offset of the first byte in the pane's output stream.

use serde_json::{Map, Value, json};
use std::io;
use std::time::{Duration, Instant};

/// Protocol majors this build speaks. The Step 08 prototype was major 0; it and v1 reject each
/// other cleanly during the handshake (`protocol_unsupported`).
pub const PROTOCOL_MIN: u64 = 1;
pub const PROTOCOL_MAX: u64 = 1;
pub const KIND_CONTROL: u8 = 0x01;
pub const KIND_DATA: u8 = 0x02;
/// Largest control payload (one JSON object).
pub const MAX_CONTROL: usize = 256 * 1024;
/// Largest data payload after its 13-byte header.
pub const MAX_DATA: usize = 1024 * 1024;
const DATA_HEADER: usize = 13;
/// Data flag: the bytes replay recent output (prototype: raw history, not a VT snapshot; Step 13).
pub const FLAG_SNAPSHOT: u8 = 0x01;
/// Data flag: client → Host input.
pub const FLAG_INPUT: u8 = 0x02;
/// Recent output kept for replay on attach.
pub const HISTORY_BYTES: usize = 64 * 1024;
/// A connection must complete `hello` → `welcome` within this time.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Once the first byte of a frame arrived, the rest must follow within this time.
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// Default time a client waits for a response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Event topics a client can subscribe to.
pub const TOPICS: [&str; 4] = ["terminals", "panes", "claude", "host"];
/// Additive capabilities announced in `welcome.features`.
pub const FEATURES: [&str; 8] = [
    "host.health",
    "host.stats",
    "events.subscribe",
    "identity.verified",
    "terminal.lifecycle",
    // Step 11: `terminal.create` `env`, panes report `executable` and `env` names.
    "terminal.environment",
    // Step 12: `terminal.create` `command_line` (verbatim), panes report it redacted.
    "terminal.command_line",
    // Step 12: `client.detach`, the `pane.detached` event, client ids in `host.status`.
    "client.detach",
];

/// One decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Control(Value),
    Data {
        stream: u32,
        seq: u64,
        flags: u8,
        bytes: Vec<u8>,
    },
}

fn header(kind: u8, payload_len: usize) -> Vec<u8> {
    let len = u32::try_from(payload_len + 1).expect("frame limits keep lengths far below 4 GiB");
    let mut out = Vec::with_capacity(5 + payload_len);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(kind);
    out
}

pub fn encode_control(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    let mut out = header(KIND_CONTROL, body.len());
    out.extend_from_slice(body.as_bytes());
    out
}

pub fn encode_data(stream: u32, seq: u64, flags: u8, bytes: &[u8]) -> Vec<u8> {
    let mut out = header(KIND_DATA, DATA_HEADER + bytes.len());
    out.extend_from_slice(&stream.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(flags);
    out.extend_from_slice(bytes);
    out
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// A byte source whose reads can be bounded by a deadline (the pipe, or a slice in tests).
pub trait TimedRead {
    /// Reads at least one byte, `Ok(0)` at end of stream, `TimedOut` once `until` has passed.
    fn read_until(&mut self, buf: &mut [u8], until: Option<Instant>) -> io::Result<usize>;
}

impl TimedRead for &[u8] {
    fn read_until(&mut self, buf: &mut [u8], _until: Option<Instant>) -> io::Result<usize> {
        io::Read::read(self, buf)
    }
}

fn fill(r: &mut impl TimedRead, buf: &mut [u8], until: Instant) -> io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        match r.read_until(&mut buf[got..], Some(until)) {
            Ok(0) => return Err(invalid("truncated frame")),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return Err(invalid(format!(
                    "incomplete frame: {got} of {} bytes arrived in time",
                    buf.len()
                )));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Reads one frame.
///
/// - `start_by`: the first byte must arrive by then (`None` waits as long as it takes); missing
///   it is `TimedOut`.
/// - `finish`: given the instant the first byte arrived, the deadline for the rest of the frame.
///
/// `Ok(None)` is a clean end of stream at a frame boundary. Every protocol violation (limits,
/// unknown kind or flags, bad JSON, truncation, a frame that never completes) is `InvalidData`,
/// and the length is checked before anything is allocated.
pub fn read_frame(
    r: &mut impl TimedRead,
    start_by: Option<Instant>,
    finish: impl FnOnce(Instant) -> Instant,
) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    if r.read_until(&mut head[..1], start_by)? == 0 {
        return Ok(None);
    }
    let until = finish(Instant::now());
    fill(r, &mut head[1..], until)?;
    let len = u32::from_le_bytes(head[..4].try_into().expect("4 bytes")) as usize;
    if len == 0 {
        return Err(invalid("zero-length frame"));
    }
    let kind = head[4];
    let limit = match kind {
        KIND_CONTROL => MAX_CONTROL,
        KIND_DATA => MAX_DATA + DATA_HEADER,
        other => return Err(invalid(format!("unknown frame kind 0x{other:02x}"))),
    };
    let payload = len - 1;
    if payload > limit {
        return Err(invalid(format!(
            "frame of {payload} bytes exceeds the limit of {limit} bytes"
        )));
    }
    let mut body = vec![0u8; payload];
    fill(r, &mut body, until)?;
    match kind {
        KIND_CONTROL => {
            let v: Value =
                serde_json::from_slice(&body).map_err(|_| invalid("control frame is not JSON"))?;
            if !v.is_object() {
                return Err(invalid("control frame is not a JSON object"));
            }
            Ok(Some(Frame::Control(v)))
        }
        _ => {
            if payload < DATA_HEADER {
                return Err(invalid("data frame header truncated"));
            }
            let stream = u32::from_le_bytes(body[0..4].try_into().expect("4 bytes"));
            let seq = u64::from_le_bytes(body[4..12].try_into().expect("8 bytes"));
            let flags = body[12];
            if flags & !(FLAG_SNAPSHOT | FLAG_INPUT) != 0 {
                return Err(invalid("unknown data flags"));
            }
            body.drain(..DATA_HEADER);
            Ok(Some(Frame::Data {
                stream,
                seq,
                flags,
                bytes: body,
            }))
        }
    }
}

/// Stable IPC error codes (a closed set in v1; docs/contracts/IPC_PROTOCOL.md §6). The Host does
/// not produce `state_from_newer_version` (Step 29) or `unsupported_platform` (M4) yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub enum ErrorCode {
    ProtocolError,
    ProtocolUnsupported,
    UnknownRequest,
    InvalidParams,
    Unauthorized,
    TargetNotFound,
    AmbiguousTarget,
    InvalidName,
    NameConflict,
    Busy,
    SpawnFailed,
    InvalidState,
    StateFromNewerVersion,
    UnsupportedPlatform,
    HostStopping,
    Timeout,
    InternalError,
}

impl ErrorCode {
    #[cfg(test)]
    pub const ALL: [Self; 17] = [
        Self::ProtocolError,
        Self::ProtocolUnsupported,
        Self::UnknownRequest,
        Self::InvalidParams,
        Self::Unauthorized,
        Self::TargetNotFound,
        Self::AmbiguousTarget,
        Self::InvalidName,
        Self::NameConflict,
        Self::Busy,
        Self::SpawnFailed,
        Self::InvalidState,
        Self::StateFromNewerVersion,
        Self::UnsupportedPlatform,
        Self::HostStopping,
        Self::Timeout,
        Self::InternalError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolError => "protocol_error",
            Self::ProtocolUnsupported => "protocol_unsupported",
            Self::UnknownRequest => "unknown_request",
            Self::InvalidParams => "invalid_params",
            Self::Unauthorized => "unauthorized",
            Self::TargetNotFound => "target_not_found",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::InvalidName => "invalid_name",
            Self::NameConflict => "name_conflict",
            Self::Busy => "busy",
            Self::SpawnFailed => "spawn_failed",
            Self::InvalidState => "invalid_state",
            Self::StateFromNewerVersion => "state_from_newer_version",
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::HostStopping => "host_stopping",
            Self::Timeout => "timeout",
            Self::InternalError => "internal_error",
        }
    }
}

/// An IPC error: stable `code`, English `message`, structured `details` (always an object).
#[derive(Debug, Clone, PartialEq)]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
}

impl IpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: json!({}),
        }
    }

    #[must_use]
    pub fn details(mut self, details: Value) -> Self {
        if details.is_object() {
            self.details = details;
        }
        self
    }

    /// `invalid_params` naming the offending field (`details.field`).
    pub fn param(field: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message).details(json!({ "field": field }))
    }

    fn to_json(&self) -> Value {
        json!({"code": self.code.as_str(), "message": self.message, "details": self.details})
    }
}

/// What a client is; it decides which methods it may call (ADR 0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Cli,
    Gui,
    Hook,
    Mcp,
}

impl ClientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Gui => "gui",
            Self::Hook => "hook",
            Self::Mcp => "mcp",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "cli" => Self::Cli,
            "gui" => Self::Gui,
            "hook" => Self::Hook,
            "mcp" => Self::Mcp,
            _ => return None,
        })
    }
}

/// `hello` (client → Host, first message).
pub fn hello(kind: ClientKind, name: &str) -> Value {
    json!({
        "type": "hello",
        "protocol": {"min": PROTOCOL_MIN, "max": PROTOCOL_MAX},
        "client": {"name": name, "version": env!("CARGO_PKG_VERSION"), "kind": kind.as_str()},
    })
}

/// `welcome` (Host → client, ends a successful handshake).
pub fn welcome(protocol: u64, pid: u32, started_at: &str) -> Value {
    json!({
        "type": "welcome",
        "protocol": protocol,
        "host": {"version": env!("CARGO_PKG_VERSION"), "pid": pid, "started_at": started_at},
        "features": FEATURES,
    })
}

/// A connection-level error (`{type: "error"}`): a refused handshake, or the reason the Host
/// closes a connection after a protocol violation. The Host closes the connection after it.
pub fn connection_error(err: &IpcError) -> Value {
    json!({"type": "error", "error": err.to_json()})
}

pub fn request(id: u64, method: &str, params: &Value) -> Value {
    json!({"type": "request", "id": id, "method": method, "params": params})
}

pub fn response(id: u64, result: Result<Value, IpcError>) -> Value {
    match result {
        Ok(r) => json!({"type": "response", "id": id, "ok": true, "result": r}),
        Err(e) => json!({"type": "response", "id": id, "ok": false, "error": e.to_json()}),
    }
}

pub fn event(seq: u64, name: &str, data: &Value) -> Value {
    json!({"type": "event", "seq": seq, "event": name, "data": data})
}

/// A validated `hello`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub min: u64,
    pub max: u64,
    pub name: String,
    pub version: String,
    pub kind: ClientKind,
}

fn short_string(v: &Value, what: &str) -> Result<String, IpcError> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.chars().count() <= 64 && !s.chars().any(char::is_control) => {
            Ok(s.to_string())
        }
        _ => Err(IpcError::new(
            ErrorCode::ProtocolError,
            format!("hello: {what} must be a string of 1-64 printable characters"),
        )),
    }
}

/// Validates the first message of a connection. Any problem is `protocol_error`; a protocol
/// range the Host does not speak is decided separately by [`negotiate`].
pub fn parse_hello(m: &Value) -> Result<Hello, IpcError> {
    let bad = |msg: &str| IpcError::new(ErrorCode::ProtocolError, msg.to_string());
    if m["type"] != "hello" {
        return Err(bad("the first message must be hello"));
    }
    let (Some(min), Some(max)) = (m["protocol"]["min"].as_u64(), m["protocol"]["max"].as_u64())
    else {
        return Err(bad("hello: protocol.min and protocol.max must be integers"));
    };
    let client = &m["client"];
    if !client.is_object() {
        return Err(bad("hello: client is missing"));
    }
    let kind_text = client["kind"].as_str().unwrap_or("");
    let Some(kind) = ClientKind::parse(kind_text) else {
        return Err(bad("hello: client.kind must be cli, gui, hook or mcp"));
    };
    Ok(Hello {
        min,
        max,
        name: short_string(&client["name"], "client.name")?,
        version: short_string(&client["version"], "client.version")?,
        kind,
    })
}

/// The protocol major both sides speak: the highest in the overlap, or `None`.
pub fn negotiate(min: u64, max: u64) -> Option<u64> {
    let hi = max.min(PROTOCOL_MAX);
    (min <= max && hi >= min.max(PROTOCOL_MIN)).then_some(hi)
}

/// The `protocol_unsupported` refusal, with the range this Host speaks.
pub fn unsupported(min: u64, max: u64) -> IpcError {
    IpcError::new(
        ErrorCode::ProtocolUnsupported,
        format!(
            "client speaks protocol {min}..{max}, this Host speaks {PROTOCOL_MIN}..{PROTOCOL_MAX}"
        ),
    )
    .details(json!({"supported": {"min": PROTOCOL_MIN, "max": PROTOCOL_MAX}}))
}

/// A validated request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// Why a control message after the handshake was refused.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// Not a request at all, or no usable id: `protocol_error`, close the connection.
    Fatal(String),
    /// A request that gets an error response; the connection stays open.
    Reply(u64, IpcError),
}

fn valid_method_name(m: &str) -> bool {
    // ^[a-z]+(\.[a-z_]+)+$, without a regex dependency.
    let mut parts = m.split('.');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    !first.is_empty()
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && !rest.is_empty()
        && rest
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
}

/// Validates a control message received after `welcome`.
pub fn parse_request(m: &Value) -> Result<Request, Refusal> {
    match m["type"].as_str() {
        Some("request") => {}
        Some(other) => {
            return Err(Refusal::Fatal(format!(
                "unexpected message type `{}` from a client",
                other.chars().take(32).collect::<String>()
            )));
        }
        None => return Err(Refusal::Fatal("message without a type".into())),
    }
    let Some(id) = m["id"].as_u64() else {
        return Err(Refusal::Fatal(
            "request id must be a non-negative integer".into(),
        ));
    };
    let method = match m["method"].as_str() {
        Some(s) if valid_method_name(s) => s.to_string(),
        _ => {
            return Err(Refusal::Reply(
                id,
                IpcError::new(ErrorCode::UnknownRequest, "method is missing or malformed"),
            ));
        }
    };
    let params = match &m["params"] {
        Value::Null => Value::Object(Map::new()),
        p @ Value::Object(_) => p.clone(),
        _ => {
            return Err(Refusal::Reply(
                id,
                IpcError::param("params", "params must be an object"),
            ));
        }
    };
    Ok(Request { id, method, params })
}

/// One v1 method: who may call it and whether this Host implements it yet.
#[derive(Debug, Clone, Copy)]
pub struct Method {
    pub name: &'static str,
    pub kinds: &'static [ClientKind],
    pub implemented: bool,
}

const ALL: &[ClientKind] = &[
    ClientKind::Cli,
    ClientKind::Gui,
    ClientKind::Hook,
    ClientKind::Mcp,
];
const CGM: &[ClientKind] = &[ClientKind::Cli, ClientKind::Gui, ClientKind::Mcp];
/// `cli`, `gui`, and `mcp*` methods: MCP clients get these only after the user grants them
/// (Step 37), so until then they are cli/gui only.
const CG: &[ClientKind] = &[ClientKind::Cli, ClientKind::Gui];
const HOOK: &[ClientKind] = &[ClientKind::Hook];

const fn m(name: &'static str, kinds: &'static [ClientKind], implemented: bool) -> Method {
    Method {
        name,
        kinds,
        implemented,
    }
}

/// The v1 method table (docs/contracts/IPC_PROTOCOL.md §4). Methods of later Steps are listed so
/// that authorization never depends on what happens to be implemented.
pub const METHODS: &[Method] = &[
    m("host.health", ALL, true),
    m("host.status", ALL, true),
    m("host.stats", CGM, true),
    m("host.stop", CG, true),
    m("events.subscribe", CGM, true),
    m("pane.attach", CG, true),
    m("pane.detach", CGM, true),
    m("pane.resize", CG, true),
    // Step 12: detaching other clients changes what they see, so MCP needs the grant (Step 37).
    m("client.detach", CG, true),
    m("terminal.list", CGM, true),
    m("terminal.info", CGM, true),
    m("terminal.create", CG, true),
    m("terminal.rename", CG, true),
    m("terminal.stop", CG, true),
    m("terminal.kill", CG, true),
    m("terminal.remove", CG, true),
    m("terminal.restore", CG, false),
    m("window.create", CG, false),
    m("window.list", CG, false),
    m("window.rename", CG, false),
    m("window.kill", CG, false),
    m("window.select", CG, false),
    m("pane.split", CG, false),
    m("pane.list", CG, false),
    m("pane.kill", CG, false),
    m("pane.select", CG, false),
    m("pane.swap", CG, false),
    m("pane.zoom", CG, false),
    m("pane.send_keys", CG, false),
    m("pane.capture", CG, false),
    m("claude.report", HOOK, false),
    m("claude.status", CGM, false),
];

/// Whether `kind` may call `method`, and whether this Host can serve it.
pub fn authorize(kind: ClientKind, method: &str) -> Result<&'static Method, IpcError> {
    let Some(m) = METHODS.iter().find(|m| m.name == method) else {
        return Err(IpcError::new(
            ErrorCode::UnknownRequest,
            format!(
                "unknown method `{}`",
                method.chars().take(64).collect::<String>()
            ),
        ));
    };
    if !m.kinds.contains(&kind) {
        return Err(IpcError::new(
            ErrorCode::Unauthorized,
            format!("a `{}` client may not call `{method}`", kind.as_str()),
        ));
    }
    if !m.implemented {
        return Err(IpcError::new(
            ErrorCode::UnknownRequest,
            format!("`{method}` is part of IPC v1 but not implemented by this Host yet"),
        ));
    }
    Ok(m)
}

/// A new object id per ADR 0007: `<prefix>-` and 12 lowercase hex digits from the OS-seeded
/// hasher keys, so ids are not predictable across processes.
pub fn new_object_id(prefix: char) -> String {
    use std::hash::BuildHasher;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let h = std::collections::hash_map::RandomState::new().hash_one((nanos, std::process::id()));
    format!("{prefix}-{:012x}", h & 0xFFFF_FFFF_FFFF)
}

/// Bounded history of recent pane output for replay on attach.
#[derive(Debug)]
pub struct Ring {
    buf: std::collections::VecDeque<u8>,
    cap: usize,
    wrapped: bool,
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: std::collections::VecDeque::with_capacity(cap),
            cap,
            wrapped: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        let bytes = if bytes.len() > self.cap {
            self.wrapped = true;
            &bytes[bytes.len() - self.cap..]
        } else {
            bytes
        };
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.wrapped = true;
            self.buf.drain(..overflow);
        }
        self.buf.extend(bytes);
    }

    /// The bytes to replay. Once old output was dropped, replay starts after the first line
    /// break so it does not begin inside an escape sequence or a UTF-8 character.
    pub fn replay(&self) -> Vec<u8> {
        let all: Vec<u8> = self.buf.iter().copied().collect();
        if !self.wrapped {
            return all;
        }
        match all.iter().position(|&b| b == b'\n') {
            Some(i) => all[i + 1..].to_vec(),
            None => Vec::new(),
        }
    }
}

/// tmux-style detach key: `Ctrl+B d`. `Ctrl+B Ctrl+B` sends one literal `Ctrl+B`; `Ctrl+B`
/// followed by anything else sends both bytes unchanged.
#[derive(Debug, Default)]
pub struct DetachKeys {
    prefix: bool,
}

pub const PREFIX: u8 = 0x02;

impl DetachKeys {
    /// Filters `input`; returns the bytes to forward and whether the user asked to detach.
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if self.prefix {
                self.prefix = false;
                match b {
                    b'd' => return (out, true),
                    PREFIX => out.push(PREFIX),
                    other => out.extend_from_slice(&[PREFIX, other]),
                }
            } else if b == PREFIX {
                self.prefix = true;
            } else {
                out.push(b);
            }
        }
        (out, false)
    }
}

/// Removes win32-input-mode requests (`ESC[?9001h` / `ESC[?9001l`) from pane output on its way to
/// a client's terminal.
///
/// Newer inbox ConPTY (seen on Windows Server 2025, build 26100) asks its terminal for
/// win32-input-mode. Passed through, the request switches the *client's* console into that mode:
/// every key then arrives as an escape sequence, so `Ctrl+B d` is never seen as `0x02 d`. The
/// client does not implement win32-input-mode, so it does not grant it; the pane's ConPTY accepts
/// plain VT input instead. Sequences split across frames are handled.
#[derive(Debug, Default)]
pub struct OutputFilter {
    pending: Vec<u8>,
}

const REFUSED: [&[u8]; 2] = [b"\x1b[?9001h", b"\x1b[?9001l"];

impl OutputFilter {
    pub fn feed(&mut self, input: &[u8]) -> Vec<u8> {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(input);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b {
                let rest = &data[i..];
                if let Some(seq) = REFUSED.iter().find(|s| rest.starts_with(s)) {
                    i += seq.len();
                    continue;
                }
                if REFUSED.iter().any(|s| s.starts_with(rest)) {
                    self.pending = rest.to_vec(); // may complete in the next frame
                    break;
                }
            }
            out.push(data[i]);
            i += 1;
        }
        out
    }
}

/// Drops a UTF-8 byte order mark at the very start of redirected input: it is never a keystroke,
/// and .NET Framework (Windows PowerShell 5.1) writes one to a child's stdin before any data.
pub fn strip_leading_bom(first_chunk: bool, bytes: &[u8]) -> &[u8] {
    if first_chunk {
        bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
    } else {
        bytes
    }
}

/// RFC 3339 UTC timestamp with second precision, without a date crate.
pub fn rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Test support: checks `v` against one definition (`#/$defs/<def>`) of the IPC v1 schema, such
/// as a method's result, which a whole response cannot be checked against (it does not name its
/// method).
#[cfg(test)]
pub fn assert_ipc_def(def: &str, v: &Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/contracts/schemas/ipc-v1.schema.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(schema["$defs"][def].is_object(), "no definition `{def}`");
    let wrapper = json!({"$defs": schema["$defs"], "$ref": format!("#/$defs/{def}")});
    let errors = crate::jsonschema::Validator::new(&wrapper)
        .validate(v)
        .unwrap();
    assert!(errors.is_empty(), "{def}: {errors:?}\n{v:#}");
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::jsonschema;

    /// A data stream id, as the Host gives its first pane.
    const STREAM: u32 = 1;

    #[test]
    fn result_examples_match_their_method_definitions() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/ipc-v1");
        for (file, def) in [
            ("response-terminal-create.json", "result_terminal_create"),
            (
                "response-terminal-create-dry-run.json",
                "result_terminal_create",
            ),
            ("response-terminal-list.json", "result_terminal_list"),
            ("response-terminal-info.json", "result_terminal_info"),
            ("response-terminal-stop.json", "result_terminal_end"),
            ("response-terminal-kill.json", "result_terminal_end"),
        ] {
            let doc: Value =
                serde_json::from_slice(&std::fs::read(dir.join(file)).unwrap()).unwrap();
            assert_ipc_def(def, &doc["result"]);
        }
        // The busy refusal carries the same plan a dry run shows.
        let busy: Value = serde_json::from_slice(
            &std::fs::read(dir.join("response-terminal-busy.json")).unwrap(),
        )
        .unwrap();
        assert_ipc_def("end_plan", &busy["error"]["details"]);
    }

    fn read(mut r: &[u8]) -> io::Result<Option<Frame>> {
        read_frame(&mut r, None, |t| t + FRAME_TIMEOUT)
    }

    /// Every message this build constructs must be valid IPC v1 (docs/contracts/schemas).
    fn assert_valid(msg: &Value) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/schemas/ipc-v1.schema.json");
        let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let errors = jsonschema::Validator::new(&schema).validate(msg).unwrap();
        assert!(errors.is_empty(), "{errors:?}\n{msg:#}");
    }

    #[test]
    fn frames_round_trip() {
        let mut wire = encode_control(&json!({"type": "hello", "protocol": {"min": 1, "max": 1}}));
        wire.extend(encode_data(STREAM, 42, FLAG_INPUT, b"echo hi\r"));
        wire.extend(encode_data(STREAM, 0, 0, b""));
        let mut r = wire.as_slice();
        let next = |r: &mut &[u8]| read_frame(r, None, |t| t + FRAME_TIMEOUT).unwrap();
        assert_eq!(
            next(&mut r),
            Some(Frame::Control(
                json!({"type": "hello", "protocol": {"min": 1, "max": 1}})
            ))
        );
        assert_eq!(
            next(&mut r),
            Some(Frame::Data {
                stream: STREAM,
                seq: 42,
                flags: FLAG_INPUT,
                bytes: b"echo hi\r".to_vec()
            })
        );
        assert!(matches!(next(&mut r), Some(Frame::Data { .. })));
        assert_eq!(next(&mut r), None, "clean EOF");
    }

    #[test]
    fn malformed_frames_are_invalid_data_never_panics() {
        let cases: Vec<Vec<u8>> = vec![
            vec![1, 0],                      // truncated header
            vec![0, 0, 0, 0, 1],             // zero length
            vec![255, 255, 255, 255, 1],     // too long (rejected before allocating)
            vec![3, 0, 0, 0, 1, b'{'],       // truncated body
            vec![2, 0, 0, 0, 1, b'1'],       // JSON but not an object
            vec![2, 0, 0, 0, 9, b'x'],       // unknown kind
            vec![5, 0, 0, 0, 2, 1, 0, 0, 0], // data header truncated
            encode_data(1, 0, 0x80, b"x"),   // unknown flag
            {
                let mut v = header(KIND_CONTROL, MAX_CONTROL + 1);
                v.resize(5 + MAX_CONTROL + 1, b' ');
                v
            },
            header(KIND_DATA, MAX_DATA + DATA_HEADER + 1), // oversized data, header only
        ];
        for (i, c) in cases.iter().enumerate() {
            let err = read(c).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "case {i}: {err}");
        }
        // The largest allowed frames are accepted.
        let mut big = header(KIND_CONTROL, MAX_CONTROL);
        big.extend_from_slice(b"{\"a\":\"");
        big.resize(5 + MAX_CONTROL - 2, b'x');
        big.extend_from_slice(b"\"}");
        assert!(matches!(read(&big), Ok(Some(Frame::Control(_)))));
        let data = encode_data(1, 0, FLAG_INPUT, &vec![0u8; MAX_DATA]);
        assert!(matches!(read(&data), Ok(Some(Frame::Data { .. }))));
    }

    /// A reader that delivers some bytes and then stalls until its deadline.
    struct Stall(Vec<u8>);

    impl TimedRead for Stall {
        fn read_until(&mut self, buf: &mut [u8], until: Option<Instant>) -> io::Result<usize> {
            if self.0.is_empty() {
                if let Some(u) = until {
                    std::thread::sleep(u.saturating_duration_since(Instant::now()));
                    return Err(io::ErrorKind::TimedOut.into());
                }
                return Ok(0);
            }
            let n = buf.len().min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn a_frame_must_start_and_complete_in_time() {
        let soon = || Instant::now() + Duration::from_millis(50);
        // Nothing arrives: TimedOut (the Host answers `timeout` during the handshake).
        let e = read_frame(&mut Stall(vec![]), Some(soon()), |t| t).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        // A frame that starts but never completes is a protocol violation, not a timeout.
        let mut half = encode_control(&json!({"type": "hello"}));
        half.truncate(9);
        let e = read_frame(&mut Stall(half), None, |t| t + Duration::from_millis(50)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("incomplete frame"), "{e}");
    }

    /// xorshift64*: deterministic pseudo-random bytes for the fuzz tests (no extra crate).
    pub struct Rng(pub u64);

    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        pub fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    #[test]
    fn fuzzed_frames_and_messages_never_panic() {
        let mut rng = Rng(0x5eed_0009);
        let seeds: Vec<Vec<u8>> = vec![
            encode_control(&hello(ClientKind::Cli, "subak-cli")),
            encode_control(&request(1, "host.status", &json!({}))),
            encode_data(STREAM, 7, FLAG_INPUT, b"dir\r"),
        ];
        for i in 0..20_000 {
            let mut wire = if i % 3 == 0 {
                (0..rng.below(64)).map(|_| rng.next() as u8).collect()
            } else {
                let mut w = seeds[rng.below(3) as usize].clone();
                for _ in 0..=rng.below(4) {
                    let at = rng.below(w.len() as u64) as usize;
                    w[at] ^= 1 << rng.below(8);
                }
                if rng.below(4) == 0 {
                    w.truncate(rng.below(w.len() as u64) as usize);
                }
                w
            };
            wire.truncate(4096);
            let mut r = wire.as_slice();
            while let Ok(Some(f)) = read_frame(&mut r, None, |t| t + FRAME_TIMEOUT) {
                if let Frame::Control(m) = f {
                    let _ = parse_hello(&m);
                    if let Ok(req) = parse_request(&m) {
                        let _ = authorize(ClientKind::Hook, &req.method);
                    }
                }
            }
        }
    }

    #[test]
    fn hello_validation_and_version_negotiation() {
        let h = parse_hello(&hello(ClientKind::Gui, "subak-gui")).unwrap();
        assert_eq!((h.min, h.max, h.kind), (1, 1, ClientKind::Gui));
        assert_eq!(negotiate(h.min, h.max), Some(1));
        // The Step 08 prototype (0..0) and a future client (2..3) share no major.
        assert_eq!(negotiate(0, 0), None);
        assert_eq!(negotiate(2, 3), None);
        assert_eq!(negotiate(0, 5), Some(1), "highest common major");
        assert_eq!(negotiate(1, 0), None, "empty range");
        for (bad, why) in [
            (json!({"type": "request"}), "not hello"),
            (
                json!({"type": "hello", "protocol": {"min": "1", "max": 1}, "client": {}}),
                "types",
            ),
            (
                json!({"type": "hello", "protocol": {"min": 1, "max": 1}}),
                "no client",
            ),
            (
                json!({"type": "hello", "protocol": {"min": 1, "max": 1},
                       "client": {"name": "x", "version": "0.1.0", "kind": "root"}}),
                "unknown kind",
            ),
            (
                json!({"type": "hello", "protocol": {"min": 1, "max": 1},
                       "client": {"name": "x".repeat(65), "version": "0.1.0", "kind": "cli"}}),
                "long name",
            ),
            (
                json!({"type": "hello", "protocol": {"min": -1, "max": 1},
                       "client": {"name": "x", "version": "0.1.0", "kind": "cli"}}),
                "negative",
            ),
        ] {
            let e = parse_hello(&bad).unwrap_err();
            assert_eq!(e.code, ErrorCode::ProtocolError, "{why}");
        }
        let u = unsupported(2, 2);
        assert_eq!(u.details["supported"], json!({"min": 1, "max": 1}));
        assert_valid(&connection_error(&u));
    }

    #[test]
    fn requests_are_validated_before_dispatch() {
        let ok = parse_request(&request(7, "host.status", &json!({}))).unwrap();
        assert_eq!((ok.id, ok.method.as_str()), (7, "host.status"));
        let no_params =
            parse_request(&json!({"type": "request", "id": 8, "method": "host.health"}));
        assert_eq!(no_params.unwrap().params, json!({}));
        let fatal = |m: Value| matches!(parse_request(&m), Err(Refusal::Fatal(_)));
        assert!(fatal(json!({"type": "shell", "command": "del /s /q C:\\"})));
        assert!(fatal(json!({"type": "hello"})), "a second hello");
        assert!(fatal(
            json!({"type": "response", "id": 1, "ok": true, "result": {}})
        ));
        assert!(fatal(json!({"id": 1, "method": "host.status"})));
        assert!(fatal(
            json!({"type": "request", "id": -1, "method": "host.status"})
        ));
        assert!(fatal(
            json!({"type": "request", "id": "1", "method": "host.status"})
        ));
        let reply = |m: Value| match parse_request(&m) {
            Err(Refusal::Reply(_, e)) => e.code,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            reply(json!({"type": "request", "id": 1, "method": "Host.Status"})),
            ErrorCode::UnknownRequest
        );
        assert_eq!(
            reply(json!({"type": "request", "id": 1, "method": 5})),
            ErrorCode::UnknownRequest
        );
        assert_eq!(
            reply(json!({"type": "request", "id": 1, "method": "host.status", "params": [1]})),
            ErrorCode::InvalidParams
        );
    }

    #[test]
    fn methods_are_authorized_by_client_kind() {
        use ClientKind::{Cli, Gui, Hook, Mcp};
        let code = |k, m| authorize(k, m).err().map(|e| e.code);
        for k in [Cli, Gui, Hook, Mcp] {
            assert_eq!(code(k, "host.health"), None);
            assert_eq!(code(k, "host.status"), None);
            assert_eq!(code(k, "shell.exec"), Some(ErrorCode::UnknownRequest));
        }
        for m in [
            "pane.attach",
            "pane.resize",
            "host.stop",
            "host.stats",
            "pane.detach",
        ] {
            assert_eq!(code(Hook, m), Some(ErrorCode::Unauthorized), "hook {m}");
            assert_eq!(code(Cli, m), None, "cli {m}");
            assert_eq!(code(Gui, m), None, "gui {m}");
        }
        // MCP gets no capability that runs or types anything until the user grants it (Step 37).
        for m in ["pane.attach", "pane.resize", "host.stop", "terminal.create"] {
            assert_eq!(code(Mcp, m), Some(ErrorCode::Unauthorized), "mcp {m}");
        }
        assert_eq!(code(Mcp, "host.stats"), None);
        // Step 10: the Terminal lifecycle is served to cli/gui; discovery also to mcp.
        for m in [
            "terminal.create",
            "terminal.rename",
            "terminal.stop",
            "terminal.kill",
            "terminal.remove",
        ] {
            assert_eq!(code(Cli, m), None, "cli {m}");
            assert_eq!(code(Gui, m), None, "gui {m}");
            assert_eq!(code(Mcp, m), Some(ErrorCode::Unauthorized), "mcp {m}");
            assert_eq!(code(Hook, m), Some(ErrorCode::Unauthorized), "hook {m}");
        }
        for m in ["terminal.list", "terminal.info"] {
            assert_eq!(code(Mcp, m), None, "mcp {m}");
            assert_eq!(code(Hook, m), Some(ErrorCode::Unauthorized), "hook {m}");
        }
        // Step 12: detaching other clients is for cli/gui only.
        assert_eq!(code(Cli, "client.detach"), None);
        assert_eq!(code(Gui, "client.detach"), None);
        assert_eq!(code(Mcp, "client.detach"), Some(ErrorCode::Unauthorized));
        assert_eq!(code(Hook, "client.detach"), Some(ErrorCode::Unauthorized));
        // Later Steps' methods: refused by kind first, then reported as not implemented.
        assert_eq!(
            code(Cli, "terminal.restore"),
            Some(ErrorCode::UnknownRequest)
        );
        assert_eq!(code(Hook, "claude.report"), Some(ErrorCode::UnknownRequest));
        assert_eq!(code(Cli, "claude.report"), Some(ErrorCode::Unauthorized));
        // Every method name fits the schema's pattern.
        for m in METHODS {
            assert!(valid_method_name(m.name), "{}", m.name);
            assert_valid(&request(1, m.name, &json!({})));
        }
    }

    #[test]
    fn every_constructed_message_is_valid_ipc_v1() {
        assert_valid(&hello(ClientKind::Cli, "subak-cli"));
        assert_valid(&welcome(1, 4242, "2026-11-04T01:02:03Z"));
        assert_valid(&response(1, Ok(json!({"healthy": true}))));
        for code in ErrorCode::ALL {
            assert_valid(&response(2, Err(IpcError::new(code, "x"))));
            assert_valid(&connection_error(&IpcError::new(code, "x")));
        }
        assert_valid(&response(3, Err(IpcError::param("cols", "bad"))));
        assert_valid(&event(
            1,
            "pane.exited",
            &json!({"pane_id": new_object_id('p'), "exit_code": 0}),
        ));
        assert_valid(&event(
            2,
            "host.stopping",
            &json!({"reason": "stop_requested"}),
        ));
        // And the contract's closed error set is exactly ErrorCode::ALL.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/schemas/ipc-v1.schema.json");
        let schema: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let listed: Vec<&str> = schema["$defs"]["error_code"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let ours: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(listed, ours);
    }

    #[test]
    fn object_ids_follow_adr_0007() {
        let a = new_object_id('p');
        let b = new_object_id('p');
        assert_ne!(a, b);
        assert!(a.len() == 14 && a.starts_with("p-"), "{a}");
        assert!(
            a[2..]
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn ring_keeps_recent_output_and_replays_from_a_line_start() {
        let mut r = Ring::new(16);
        r.push(b"abc\n");
        assert_eq!(r.replay(), b"abc\n");
        r.push(b"0123456789\nxyz");
        // 18 bytes pushed, cap 16: "c\n0123456789\nxyz" kept; replay starts after the first \n.
        assert_eq!(r.replay(), b"0123456789\nxyz");
        r.push(&[b'q'; 40]);
        assert_eq!(
            r.replay(),
            b"",
            "no line start left: replay nothing rather than garbage"
        );
    }

    #[test]
    fn detach_keys() {
        let mut k = DetachKeys::default();
        assert_eq!(k.feed(b"ls\r"), (b"ls\r".to_vec(), false));
        assert_eq!(k.feed(&[PREFIX, PREFIX, b'x']), (vec![PREFIX, b'x'], false));
        assert_eq!(k.feed(&[PREFIX, b'c']), (vec![PREFIX, b'c'], false));
        assert_eq!(k.feed(&[b'a', PREFIX]), (b"a".to_vec(), false));
        assert_eq!(
            k.feed(b"d"),
            (Vec::new(), true),
            "prefix carried across reads"
        );
        assert_eq!(k.feed(&[b'z', PREFIX, b'd', b'q']), (b"z".to_vec(), true));
    }

    #[test]
    fn win32_input_mode_requests_are_removed_even_when_split() {
        let stream: &[u8] = b"\x1b[?25l\x1b[?9001h\x1b[?1004hC:\\>dir\r\n\x1b[?9001lend\x1b[?90";
        let expected: &[u8] = b"\x1b[?25l\x1b[?1004hC:\\>dir\r\nend";
        for split in 0..=stream.len() {
            let mut f = OutputFilter::default();
            let mut out = f.feed(&stream[..split]);
            out.extend(f.feed(&stream[split..]));
            assert_eq!(out, expected, "split at {split}");
        }
        // Other sequences, and a lone ESC that turns out not to be a request, pass unchanged.
        let mut f = OutputFilter::default();
        assert_eq!(f.feed(b"a\x1b[?900"), b"a");
        assert_eq!(f.feed(b"2hb"), b"\x1b[?9002hb");
        assert_eq!(f.feed(b"\x1b"), b"");
        assert_eq!(f.feed(b"[1mX"), b"\x1b[1mX");
    }

    #[test]
    fn only_a_leading_bom_is_dropped() {
        assert_eq!(strip_leading_bom(true, b"\xEF\xBB\xBFdir\r"), b"dir\r");
        assert_eq!(strip_leading_bom(true, b"dir\r"), b"dir\r");
        assert_eq!(strip_leading_bom(false, b"\xEF\xBB\xBFx"), b"\xEF\xBB\xBFx");
    }

    #[test]
    fn timestamps() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(1_793_750_400 + 3723)),
            "2026-11-04T01:02:03Z"
        );
        assert_eq!(
            rfc3339(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }
}
