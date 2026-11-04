//! The CLI side of Noeul IPC v1 (ADR 0023 §4): connect, check who serves the pipe, hand-shake,
//! send requests. Nothing is sent before the server process is verified as a `noeul.exe` of this
//! user (NOEUL-SEC-011); a squatter or impostor gets nothing and is reported.

use crate::error::{Code, NoeulError, Result};
use crate::ipc;
use crate::win::pipe::{self, Pipe, Until};
use crate::win::{process_image, process_user_sid, user_sid};
use serde_json::Value;
use std::io;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

/// How long `connect` waits while every pipe instance is busy.
pub const CONNECT_WAIT: Duration = Duration::from_secs(2);

fn mismatch(endpoint: &str, pid: Option<u32>, reason: &str) -> NoeulError {
    let mut e = NoeulError::new(
        Code::NoeulIdentityMismatch,
        format!(
            "`{}` is not served by Noeul of this user: {reason}",
            pipe::path(endpoint)
        ),
    )
    .hint("nothing was sent to it; another program holds this pipe name. End that program yourself, or use another --endpoint");
    if let Some(pid) = pid {
        e = e.detail(format!("server process id: {pid}"));
    }
    e
}

/// Checks the server process before anything is sent: same user, image `noeul.exe`.
pub fn verify_server(p: &Pipe, endpoint: &str) -> Result<u32> {
    let Some(pid) = p.server_pid() else {
        return Err(mismatch(endpoint, None, "the server process is unknown"));
    };
    let me = user_sid().map_err(|e| {
        NoeulError::new(Code::Io, format!("cannot determine the current user: {e}"))
    })?;
    match process_user_sid(pid) {
        Ok(sid) if sid == me => {}
        Ok(_) => return Err(mismatch(endpoint, Some(pid), "it runs as another user")),
        Err(e) => {
            return Err(mismatch(
                endpoint,
                Some(pid),
                &format!("its owner cannot be verified ({e})"),
            )
            .detail("an elevated (administrator) process cannot be verified by a normal client"));
        }
    }
    let image = process_image(pid).map_err(|e| {
        mismatch(
            endpoint,
            Some(pid),
            &format!("its program cannot be read ({e})"),
        )
    })?;
    let name = image
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !name.eq_ignore_ascii_case(crate::IMAGE) {
        return Err(mismatch(
            endpoint,
            Some(pid),
            &format!("its program is `{name}`, not {}", crate::IMAGE),
        ));
    }
    Ok(pid)
}

/// One verified, hand-shaken connection to the instance.
#[derive(Debug)]
pub struct Session {
    pipe: Pipe,
    next_id: u64,
    pub welcome: Value,
    pub pid: u32,
    pub endpoint: String,
}

fn send(p: &Pipe, msg: &Value) -> Result<()> {
    p.write_all_bytes(&ipc::encode(msg))
        .map_err(|_| disconnected())
}

fn disconnected() -> NoeulError {
    NoeulError::new(
        Code::NoeulUnresponsive,
        "the Noeul instance closed the connection",
    )
    .hint("it may be shutting down; try again")
}

/// Connects to the instance at `endpoint`. `noeul_not_running` when nobody serves it.
pub fn open(endpoint: &str) -> Result<Session> {
    let p = pipe::connect(endpoint, CONNECT_WAIT).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => NoeulError::not_running(),
        io::ErrorKind::PermissionDenied => NoeulError::new(
            Code::PermissionDenied,
            format!("access to `{}` was denied", pipe::path(endpoint)),
        )
        .hint("the endpoint belongs to another user, or this process runs sandboxed"),
        _ if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => NoeulError::new(
            Code::NoeulUnresponsive,
            format!(
                "`{}` exists, but no pipe instance became free within {} s",
                pipe::path(endpoint),
                CONNECT_WAIT.as_secs()
            ),
        )
        .hint("Noeul may be busy or hung; it was not stopped"),
        _ => NoeulError::new(Code::Io, format!("cannot connect to Noeul: {e}")),
    })?;
    let pid = verify_server(&p, endpoint)?;
    send(&p, &ipc::hello("noeul-cli"))?;
    let deadline = Instant::now() + ipc::HANDSHAKE_TIMEOUT;
    let mut r = Until {
        pipe: &p,
        deadline: Some(deadline),
    };
    let welcome = match ipc::read_frame(&mut r) {
        Ok(Some(m)) if m["type"] == "welcome" => m,
        Ok(Some(m)) if m["type"] == "error" => return Err(ipc::parse_error(&m["error"])),
        Err(ipc::FrameError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
            return Err(NoeulError::new(
                Code::NoeulUnresponsive,
                format!(
                    "Noeul (pid {pid}) accepted the connection but did not answer within {} s",
                    ipc::HANDSHAKE_TIMEOUT.as_secs()
                ),
            )
            .hint("it may be busy or hung; it was not stopped"));
        }
        Ok(None) | Err(ipc::FrameError::Io(_) | ipc::FrameError::Truncated) => {
            return Err(disconnected());
        }
        Ok(Some(_)) | Err(_) => {
            return Err(NoeulError::new(
                Code::ProtocolError,
                "unexpected handshake reply",
            ));
        }
    };
    if welcome["instance"]["pid"].as_u64() != Some(u64::from(pid))
        || welcome["protocol"]
            .as_u64()
            .and_then(|p| ipc::negotiate(p, p))
            .is_none()
    {
        return Err(NoeulError::new(
            Code::ProtocolError,
            "the welcome does not match the verified server process or protocol",
        ));
    }
    Ok(Session {
        pipe: p,
        next_id: 1,
        welcome,
        pid,
        endpoint: endpoint.to_string(),
    })
}

impl Session {
    /// One request and its answer.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        send(&self.pipe, &ipc::request(id, method, params))?;
        let mut r = Until {
            pipe: &self.pipe,
            deadline: Some(Instant::now() + ipc::REQUEST_TIMEOUT),
        };
        match ipc::read_frame(&mut r) {
            Ok(Some(m)) if m["type"] == "response" && m["id"].as_u64() == Some(id) => {
                if m.get("error").is_some() {
                    Err(ipc::parse_error(&m["error"]))
                } else {
                    Ok(m["result"].clone())
                }
            }
            Ok(Some(m)) if m["type"] == "error" => Err(ipc::parse_error(&m["error"])),
            Err(ipc::FrameError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
                Err(NoeulError::new(
                    Code::NoeulUnresponsive,
                    format!(
                        "Noeul did not answer `{method}` within {} s",
                        ipc::REQUEST_TIMEOUT.as_secs()
                    ),
                ))
            }
            Ok(None) | Err(ipc::FrameError::Io(_) | ipc::FrameError::Truncated) => {
                Err(disconnected())
            }
            Ok(Some(_)) | Err(_) => Err(NoeulError::new(
                Code::ProtocolError,
                format!("unexpected answer to `{method}`"),
            )),
        }
    }
}
