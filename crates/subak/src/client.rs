//! Prototype clients of the Step 08 Host: `host start | status | stop | attach`.
//!
//! Each command is an independent short-lived process. Closing, killing or crashing any of them
//! — including the console window an attach runs in — only drops its pipe connection; the Host,
//! its pseudo console and its shell are unaffected (`SUBAK-FR-003`, `SUBAK-FR-015`).

use crate::cli::Args;
use crate::host::{default_shell, profile_dir};
use crate::output::{CliError, Code};
use crate::proto::{self, DetachKeys, FLAG_INPUT, Frame, PROTOCOL, STREAM};
use crate::win::pipe::{self, Deadline, Pipe};
use crate::win::{self, console, spawn};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The endpoint from `--endpoint`, or the per-user default.
pub fn endpoint(a: &Args) -> Result<String, CliError> {
    match &a.endpoint {
        Some(e) => Ok(e.clone()),
        None => pipe::default_endpoint().map_err(|e| {
            CliError::new(Code::Io, format!("cannot determine the current user: {e}"))
        }),
    }
}

fn host_not_running(endpoint: &str) -> CliError {
    CliError::new(
        Code::HostNotRunning,
        format!("no Subak Host is serving `{}`", pipe::path(endpoint)),
    )
    .hint("start one with `subak host start` (Step 08 prototype)")
}

fn disconnected() -> CliError {
    CliError::new(Code::HostDisconnected, "the Host closed the connection")
        .hint("check `subak host status`; a stopped or crashed Host ends its shell")
}

/// Maps a Host-side IPC error code to a CLI error.
fn host_error(code: &str, message: &str) -> CliError {
    let c = match code {
        "protocol_error" | "protocol_unsupported" | "unknown_request" => Code::ProtocolError,
        "host_stopping" => Code::HostDisconnected,
        "invalid_params" => Code::Usage,
        "timeout" => Code::Timeout,
        _ => Code::Internal,
    };
    CliError::new(c, format!("the Host refused the request: {message}"))
        .detail(format!("ipc error code: {code}"))
}

/// One connection to a Host, after the handshake.
#[derive(Debug)]
pub struct Session {
    pipe: Arc<Pipe>,
    next_id: Arc<AtomicU64>,
    pub welcome: Value,
}

fn send(pipe: &Pipe, msg: &Value) -> Result<(), CliError> {
    pipe.write_all_bytes(&proto::encode_control(msg))
        .map_err(|_| disconnected())
}

impl Session {
    pub fn open(endpoint: &str, timeout: Duration) -> Result<Self, CliError> {
        let pipe = pipe::connect(endpoint, timeout).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => host_not_running(endpoint),
            io::ErrorKind::PermissionDenied => CliError::new(
                Code::PermissionDenied,
                format!("access to `{}` was denied", pipe::path(endpoint)),
            )
            .hint("the endpoint belongs to another user or program; use your own endpoint"),
            _ => CliError::new(Code::Io, format!("cannot connect to the Host: {e}")),
        })?;
        send(
            &pipe,
            &json!({"type": "hello", "protocol": {"min": PROTOCOL, "max": PROTOCOL},
                    "client": {"name": "subak-cli", "version": env!("CARGO_PKG_VERSION"), "kind": "cli"}}),
        )?;
        let mut dl = Deadline {
            pipe: &pipe,
            until: Instant::now() + Duration::from_secs(5),
        };
        let welcome = match proto::read_frame(&mut dl) {
            Ok(Some(Frame::Control(m))) if m["type"] == "welcome" => m,
            Ok(Some(Frame::Control(m))) if m["type"] == "error" => {
                return Err(host_error(
                    m["error"]["code"].as_str().unwrap_or("?"),
                    m["error"]["message"]
                        .as_str()
                        .unwrap_or("handshake refused"),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return Err(CliError::new(
                    Code::Timeout,
                    "the Host did not answer the handshake within 5 s",
                ));
            }
            Ok(None) | Err(_) => return Err(disconnected()),
            Ok(Some(_)) => {
                return Err(CliError::new(
                    Code::ProtocolError,
                    "unexpected handshake reply",
                ));
            }
        };
        Ok(Self {
            pipe: Arc::new(pipe),
            next_id: Arc::new(AtomicU64::new(1)),
            welcome,
        })
    }

    pub fn request(&mut self, method: &str, params: &Value) -> Result<Value, CliError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        send(
            &self.pipe,
            &json!({"type": "request", "id": id, "method": method, "params": params}),
        )?;
        loop {
            match proto::read_frame(&mut &*self.pipe) {
                Ok(Some(Frame::Control(m))) if m["type"] == "response" && m["id"] == id => {
                    return if m["ok"] == true {
                        Ok(m["result"].clone())
                    } else {
                        Err(host_error(
                            m["error"]["code"].as_str().unwrap_or("?"),
                            m["error"]["message"].as_str().unwrap_or("request failed"),
                        ))
                    };
                }
                Ok(Some(_)) => {} // events and data are not for a plain request
                Ok(None) => return Err(disconnected()),
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    return Err(CliError::new(
                        Code::ProtocolError,
                        format!("malformed Host message: {e}"),
                    ));
                }
                Err(_) => return Err(disconnected()),
            }
        }
    }
}

/// `host start`: spawn a detached Host and wait until it serves the endpoint.
pub fn start(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let exe = std::env::current_exe()
        .map_err(|e| CliError::new(Code::Io, format!("cannot locate subak.exe: {e}")))?;
    let shell = a.shell.clone().unwrap_or_else(default_shell);
    let cwd = profile_dir()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| CliError::new(Code::Io, "no working directory for the Host"))?;
    let args_for = |breakaway: bool| -> Vec<String> {
        [
            "host",
            "serve",
            "--endpoint",
            endpoint,
            "--shell",
            &shell,
            "--job-breakaway",
            if breakaway { "true" } else { "false" },
        ]
        .map(String::from)
        .to_vec()
    };
    if a.dry_run {
        return Ok(json!({
            "dry_run": true,
            "would_start": {
                "program": exe.display().to_string(),
                "arguments": args_for(true),
                "cwd": cwd.display().to_string(),
                "creation_flags": spawn::FLAG_NAMES,
                "stdio": "NUL (no other inherited handles)",
                "endpoint": pipe::path(endpoint),
                "shell": shell,
            },
        }));
    }
    if let Ok(mut s) = Session::open(endpoint, Duration::from_millis(500)) {
        let pid = s
            .request("host.status", &json!({}))
            .ok()
            .map(|st| st["host"]["pid"].clone());
        return Err(CliError::new(
            Code::HostAlreadyRunning,
            format!(
                "a Host already serves `{}` (pid {})",
                pipe::path(endpoint),
                pid.unwrap_or(Value::Null)
            ),
        )
        .hint("attach with `subak host attach`, or stop it with `subak host stop`"));
    }
    let spawned = spawn::spawn_detached(&exe, args_for, &cwd)
        .map_err(|e| CliError::new(Code::SpawnFailed, format!("cannot start the Host: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Session::open(endpoint, Duration::from_millis(200)) {
            Ok(mut s) => {
                let st = s.request("host.status", &json!({}))?;
                if st["host"]["pid"] != spawned.pid {
                    return Err(CliError::new(
                        Code::HostAlreadyRunning,
                        "another Host started on this endpoint at the same time",
                    ));
                }
                return Ok(json!({
                    "started": true,
                    "endpoint": st["endpoint"],
                    "host": st["host"],
                    "pane": st["pane"],
                }));
            }
            Err(e) if e.code == Code::HostNotRunning => {}
            Err(e) => return Err(e),
        }
        if let Some(code) = win::exit_code(spawned.process.raw()) {
            return Err(if code == u32::from(Code::HostAlreadyRunning.exit_code()) {
                CliError::new(
                    Code::HostAlreadyRunning,
                    "another Host already serves this endpoint",
                )
            } else {
                CliError::new(
                    Code::SpawnFailed,
                    format!("the Host exited during startup (exit code {code})"),
                )
                .hint("check the --shell command line; run `subak host serve` in a console to see the error")
            });
        }
        if Instant::now() > deadline {
            return Err(
                CliError::new(Code::Timeout, "the Host did not open its pipe within 10 s")
                    .detail(format!("host pid {}", spawned.pid)),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn status(endpoint: &str) -> Result<Value, CliError> {
    Session::open(endpoint, Duration::from_secs(2))?.request("host.status", &json!({}))
}

fn can_prompt(a: &Args) -> bool {
    let env_no_input = std::env::var("KIM_NO_INPUT")
        .is_ok_and(|v| !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false")));
    !a.json
        && !a.no_input
        && !env_no_input
        && io::stdin().is_terminal()
        && io::stderr().is_terminal()
}

/// `host stop`: destructive — ends the shell and every process in its job, then the Host.
pub fn stop(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = Session::open(endpoint, Duration::from_secs(2))?;
    let st = s.request("host.status", &json!({}))?;
    if a.dry_run {
        return Ok(
            json!({"dry_run": true, "would_stop": {"host": st["host"], "pane": st["pane"], "clients": st["clients"]}}),
        );
    }
    let host_pid = st["host"]["pid"]
        .as_u64()
        .and_then(|p| u32::try_from(p).ok());
    if !a.yes {
        let question = format!(
            "Stop the Subak Host (pid {}) and end its shell (pid {}, {} process(es))? [y/N] ",
            st["host"]["pid"], st["pane"]["pid"], st["pane"]["job_processes"]
        );
        if !can_prompt(a) {
            return Err(CliError::new(
                Code::ConfirmationRequired,
                "stopping the Host ends its shell and every process started in it",
            )
            .hint("re-run with --yes, or with --dry-run to see what would end"));
        }
        eprint!("{question}");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            return Err(CliError::new(
                Code::Cancelled,
                "not stopped; nothing changed",
            ));
        }
    }
    let host_proc = host_pid.and_then(win::open_process);
    let report = s.request("host.stop", &json!({"dry_run": false}))?;
    let exited = host_proc
        .as_ref()
        .is_none_or(|h| win::wait(h.raw(), Some(Duration::from_secs(10))));
    Ok(json!({
        "stopped": true,
        "host": {"pid": st["host"]["pid"], "exited": exited},
        "shell": report["shell"],
    }))
}

/// How an attach ended.
enum End {
    Detached,
    Stopped { exit_code: Value, reason: Value },
}

/// `host attach`: stream the pane until detach (`Ctrl+B d`, or end of piped input), the shell's
/// exit or an explicit stop.
pub fn attach(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = Session::open(endpoint, Duration::from_secs(2))?;
    let console_in = console::stdin_is_console();
    let size = a.size.or_else(|| {
        if console_in {
            console::window_size()
        } else {
            None
        }
    });
    if a.dry_run {
        let st = s.request("host.status", &json!({}))?;
        return Ok(json!({"dry_run": true, "would_attach": {
            "host": st["host"], "pane": st["pane"],
            "size": size.map(|(c, r)| json!({"cols": c, "rows": r})),
        }}));
    }
    let res = s.request(
        "pane.attach",
        &json!({"size": size.map(|(c, r)| json!({"cols": c, "rows": r}))}),
    )?;
    let host_pid = s.welcome["host"]["pid"].clone();
    if !a.quiet {
        eprintln!(
            "[subak: attached to Host pid {host_pid}, shell pid {}; Ctrl+B d detaches]",
            res["pane"]["pid"]
        );
    }
    let detach_id = Arc::new(AtomicU64::new(0));
    let raw = (console_in || console::stdout_is_console()).then(console::RawMode::enable);
    std::thread::spawn({
        let pipe = Arc::clone(&s.pipe);
        let ids = Arc::clone(&s.next_id);
        let detach_id = Arc::clone(&detach_id);
        move || input(&pipe, console_in, size, &ids, &detach_id)
    });
    let mut stdout = io::stdout().lock();
    let mut exit_code = Value::Null;
    let mut stopping = Value::Null;
    let end = loop {
        match proto::read_frame(&mut &*s.pipe) {
            Ok(Some(Frame::Data { bytes, .. })) => {
                if stdout
                    .write_all(&bytes)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    // Our own output is gone (e.g. the reader of a pipe went away): detach.
                    break End::Detached;
                }
            }
            Ok(Some(Frame::Control(m))) => match m["type"].as_str() {
                Some("event") if m["event"] == "pane.exited" => {
                    exit_code = m["data"]["exit_code"].clone();
                }
                Some("event") if m["event"] == "host.stopping" => {
                    stopping = m["data"]["reason"].clone();
                }
                Some("response") if m["id"] == detach_id.load(Ordering::SeqCst) => {
                    break End::Detached;
                }
                _ => {}
            },
            Ok(None) | Err(_) if !stopping.is_null() => {
                break End::Stopped {
                    exit_code,
                    reason: stopping,
                };
            }
            Ok(None) | Err(_) => {
                drop(raw);
                return Err(disconnected());
            }
        }
    };
    drop(raw);
    Ok(match end {
        End::Detached => json!({"detached": true, "host": {"pid": host_pid}, "pane": res["pane"]}),
        End::Stopped { exit_code, reason } => json!({
            "detached": false, "host": {"pid": host_pid, "stopped": reason},
            "pane": res["pane"], "shell_exit_code": exit_code,
        }),
    })
}

/// The input side of an attach (its own thread): keys or piped bytes → data frames.
fn input(
    pipe: &Pipe,
    console_in: bool,
    size: Option<(u16, u16)>,
    ids: &AtomicU64,
    detach_id: &AtomicU64,
) {
    let mut keys = DetachKeys::default();
    let request = |method: &str, params: Value| -> bool {
        let id = ids.fetch_add(1, Ordering::SeqCst);
        if method == "pane.detach" {
            detach_id.store(id, Ordering::SeqCst);
        }
        send(
            pipe,
            &json!({"type": "request", "id": id, "method": method, "params": params}),
        )
        .is_ok()
    };
    let mut forward = |bytes: &[u8]| -> Option<bool> {
        let (fwd, detach) = keys.feed(bytes);
        if !fwd.is_empty() {
            for chunk in fwd.chunks(32 * 1024) {
                pipe.write_all_bytes(&proto::encode_data(STREAM, 0, FLAG_INPUT, chunk))
                    .ok()?;
            }
        }
        Some(detach)
    };
    if console_in {
        let mut size = size.unwrap_or((0, 0));
        let mut high = None;
        loop {
            let Ok(events) = console::read_input(&mut size, &mut high, Duration::from_millis(250))
            else {
                request("pane.detach", json!({}));
                return;
            };
            for ev in events {
                match ev {
                    console::Input::Bytes(b) => match forward(&b) {
                        Some(false) => {}
                        Some(true) => {
                            request("pane.detach", json!({}));
                            return;
                        }
                        None => return,
                    },
                    console::Input::Resize(c, r) => {
                        if !request("pane.resize", json!({"cols": c, "rows": r})) {
                            return;
                        }
                    }
                }
            }
        }
    }
    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 8192];
    let mut first = true;
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => 0,
            Ok(n) => n,
        };
        if n == 0 {
            request("pane.detach", json!({})); // end of piped input = detach
            return;
        }
        let bytes = proto::strip_leading_bom(first, &buf[..n]);
        first = false;
        match forward(bytes) {
            Some(false) => {}
            Some(true) => {
                request("pane.detach", json!({}));
                return;
            }
            None => return,
        }
    }
}
