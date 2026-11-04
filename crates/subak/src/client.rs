//! Clients of the Host — the client SDK (ADR 0003, ADR 0005, ADR 0019) and the
//! `host start | status | health | stop | attach` commands. The `terminal` commands build on it
//! (`terminal.rs`).
//!
//! Connecting is: open the pipe, **verify who serves it** before sending a single byte (the
//! server process must run as this user and be a `subak.exe`), then `hello` → `welcome` with
//! version negotiation. Starting on demand takes a per-endpoint start mutex, so several clients
//! starting at once give exactly one Host. Nothing here ever kills or replaces a Host: a Host
//! that holds the endpoint but does not answer is reported (`host_unresponsive`), never ended,
//! because it may own live sessions.
//!
//! Each command is an independent short-lived process. Closing, killing or crashing any of them
//! only drops its pipe connection; the Host and its panes are unaffected (`SUBAK-FR-003`).

use crate::cli::Args;
use crate::host::{self, default_shell, profile_dir};
use crate::output::{CliError, Code};
use crate::proto::{self, ClientKind, DetachKeys, FLAG_INPUT, Frame};
use crate::win::pipe::{self, Pipe};
use crate::win::{self, EnvBlock, NamedLock, console, spawn};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

/// The executable a genuine Host runs as (ADR 0005).
pub const HOST_IMAGE: &str = "subak.exe";
/// How long a starter waits for a new Host to serve its pipe.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a starter waits for another starter holding the start lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a client waits for a stopping Host to release the endpoint.
const STOPPING_TIMEOUT: Duration = Duration::from_secs(15);
/// `host.stop` and `terminal.stop|kill|remove` answer once the panes have ended (grace period
/// plus forced end).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// The endpoint from `--endpoint`, or the per-user default.
pub fn endpoint(a: &Args) -> Result<String, CliError> {
    match &a.endpoint {
        Some(e) => Ok(e.clone()),
        None => pipe::default_endpoint().map_err(|e| {
            CliError::new(Code::Io, format!("cannot determine the current user: {e}"))
        }),
    }
}

/// The per-endpoint start lock (`Local\` = this Windows session).
pub fn start_lock_name(endpoint: &str) -> String {
    format!(r"Local\mouv-subak-start-{endpoint}")
}

fn host_not_running(endpoint: &str) -> CliError {
    CliError::new(
        Code::HostNotRunning,
        format!("no Subak Host is serving `{}`", pipe::path(endpoint)),
    )
    .hint("start one with `subak host start`")
}

fn disconnected() -> CliError {
    CliError::new(Code::HostDisconnected, "the Host closed the connection")
        .hint("check `subak host status`; a stopped or crashed Host ends its panes")
}

/// Maps a Host-side IPC error to a CLI error (IPC_PROTOCOL §6); its `details` become detail
/// lines (`key: value`).
fn host_error(code: &str, message: &str, details: &Value) -> CliError {
    let c = match code {
        "protocol_error" | "protocol_unsupported" | "unknown_request" => Code::ProtocolError,
        "host_stopping" => Code::HostDisconnected,
        "invalid_params" => Code::Usage,
        "timeout" => Code::Timeout,
        "unauthorized" => Code::PermissionDenied,
        "target_not_found" => Code::TargetNotFound,
        "busy" => Code::ConfirmationRequired,
        "name_conflict" => Code::NameConflict,
        "invalid_name" => Code::InvalidName,
        "ambiguous_target" => Code::AmbiguousTarget,
        "invalid_state" => Code::InvalidState,
        "spawn_failed" => Code::SpawnFailed,
        _ => Code::Internal,
    };
    let mut e = CliError::new(c, format!("the Host refused the request: {message}"))
        .detail(format!("ipc error code: {code}"));
    if let Some(map) = details.as_object() {
        for (k, v) in map {
            let text = v.as_str().map_or_else(|| v.to_string(), String::from);
            e = e.detail(format!("{k}: {text}"));
        }
    }
    // `spawn_failed` carries what to do next (e.g. how to run a batch script explicitly).
    if let Some(h) = details["hint"].as_str() {
        e = e.hint(h);
    }
    e
}

/// The CLI error for a refused handshake. A protocol range mismatch says which side is older,
/// and the client never restarts the Host to fix it (ADR 0005).
fn handshake_refused(err: &Value) -> CliError {
    let code = err["code"].as_str().unwrap_or("?");
    let message = err["message"].as_str().unwrap_or("handshake refused");
    if code == "protocol_unsupported" {
        let host_max = err["details"]["supported"]["max"].as_u64().unwrap_or(0);
        return if host_max < proto::PROTOCOL_MIN {
            CliError::new(
                Code::HostTooOld,
                format!("the running Host is older than this client: {message}"),
            )
            .hint("it keeps running; when its sessions can end, stop it with `subak host stop` and start the new version")
        } else {
            CliError::new(
                Code::HostTooNew,
                format!("the running Host is newer than this client: {message}"),
            )
            .hint("use the subak.exe that matches the running Host, or update this one")
        };
    }
    host_error(code, message, &err["details"])
}

/// Why a connection attempt failed, for start-on-demand decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    NotRunning,
    Stopping,
    Other,
}

/// A failed connection attempt.
#[derive(Debug)]
pub struct Failure {
    pub why: Why,
    pub err: CliError,
}

fn other(err: CliError) -> Failure {
    Failure {
        why: Why::Other,
        err,
    }
}

fn mismatch(endpoint: &str, pid: Option<u32>, reason: &str) -> CliError {
    let mut e = CliError::new(
        Code::HostIdentityMismatch,
        format!(
            "`{}` is not served by a Subak Host of this user: {reason}",
            pipe::path(endpoint)
        ),
    )
    .hint("nothing was sent to it; another program holds this pipe name. Use another --endpoint, or end that program yourself");
    if let Some(pid) = pid {
        e = e.detail(format!("server process id: {pid}"));
    }
    e
}

/// Checks who serves `pipe` — before anything is sent to it (ADR 0005): the server process must
/// run as this user and be a `subak.exe`. Returns its process id.
pub fn verify_host(pipe: &Pipe, endpoint: &str) -> Result<u32, CliError> {
    let Some(pid) = pipe.server_pid() else {
        return Err(mismatch(endpoint, None, "the server process is unknown"));
    };
    let me = win::user_sid()
        .map_err(|e| CliError::new(Code::Io, format!("cannot determine the current user: {e}")))?;
    match win::process_user_sid(pid) {
        Ok(sid) if sid == me => {}
        Ok(_) => return Err(mismatch(endpoint, Some(pid), "it runs as another user")),
        Err(e) => {
            return Err(mismatch(
                endpoint,
                Some(pid),
                &format!("its owner cannot be verified ({e})"),
            )
            .detail("an elevated (administrator) Host cannot be verified by a normal client"));
        }
    }
    let image = win::process_image(pid).map_err(|e| {
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
    if !name.eq_ignore_ascii_case(HOST_IMAGE) {
        return Err(mismatch(
            endpoint,
            Some(pid),
            &format!("its program is `{name}`, not {HOST_IMAGE}"),
        )
        .detail(format!("server program: {}", image.display())));
    }
    Ok(pid)
}

/// `host serve` found its endpoint taken: by a genuine Host, or by something else.
pub fn occupied(endpoint: &str) -> CliError {
    let running = |pid: Option<u32>| {
        let mut e = CliError::new(
            Code::HostAlreadyRunning,
            format!("a Host already serves `{}`", pipe::path(endpoint)),
        )
        .hint("attach with `subak attach`, or stop it with `subak host stop`");
        if let Some(pid) = pid {
            e = e.detail(format!("host pid: {pid}"));
        }
        e
    };
    match pipe::connect(endpoint, Duration::from_secs(1)) {
        Ok(p) => match verify_host(&p, endpoint) {
            Ok(pid) => running(Some(pid)),
            Err(e) => e,
        },
        Err(_) => running(None),
    }
}

/// One connection to a Host, after the identity checks and the handshake.
#[derive(Debug)]
pub struct Session {
    pipe: Arc<Pipe>,
    next_id: Arc<AtomicU64>,
    pub welcome: Value,
    pub host_pid: u32,
}

fn send(pipe: &Pipe, msg: &Value) -> Result<(), CliError> {
    pipe.write_all_bytes(&proto::encode_control(msg))
        .map_err(|_| disconnected())
}

impl Session {
    pub fn open(endpoint: &str, wait: Duration) -> Result<Self, CliError> {
        Self::try_open(endpoint, wait).map_err(|f| f.err)
    }

    /// Connects; `wait` bounds the wait while every pipe instance is busy.
    pub fn try_open(endpoint: &str, wait: Duration) -> Result<Self, Failure> {
        let pipe = pipe::connect(endpoint, wait).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Failure {
                why: Why::NotRunning,
                err: host_not_running(endpoint),
            },
            io::ErrorKind::PermissionDenied => other(
                CliError::new(
                    Code::PermissionDenied,
                    format!("access to `{}` was denied", pipe::path(endpoint)),
                )
                .hint("the endpoint belongs to another user, or this process runs sandboxed (low integrity)"),
            ),
            // The Host always keeps one instance listening; one that stays busy is not served.
            _ if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => other(
                CliError::new(
                    Code::HostUnresponsive,
                    format!(
                        "`{}` exists, but no pipe instance became free within {} ms",
                        pipe::path(endpoint),
                        wait.as_millis()
                    ),
                )
                .hint("the Host may be busy or hung; it was neither stopped nor replaced"),
            ),
            _ => other(CliError::new(
                Code::Io,
                format!("cannot connect to the Host: {e}"),
            )),
        })?;
        let pid = verify_host(&pipe, endpoint).map_err(other)?;
        send(&pipe, &proto::hello(ClientKind::Cli, "subak-cli")).map_err(other)?;
        let deadline = Instant::now() + proto::HANDSHAKE_TIMEOUT;
        let welcome = match proto::read_frame(&mut &pipe, Some(deadline), |_| deadline) {
            Ok(Some(Frame::Control(m))) if m["type"] == "welcome" => m,
            Ok(Some(Frame::Control(m))) if m["type"] == "error" => {
                let why = if m["error"]["code"] == "host_stopping" {
                    Why::Stopping
                } else {
                    Why::Other
                };
                return Err(Failure {
                    why,
                    err: handshake_refused(&m["error"]),
                });
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return Err(other(
                    CliError::new(
                        Code::HostUnresponsive,
                        format!(
                            "Host pid {pid} accepted the connection but did not answer within {} s",
                            proto::HANDSHAKE_TIMEOUT.as_secs()
                        ),
                    )
                    .hint("it may be busy or hung; it was neither stopped nor replaced, because it may own live sessions. Try again, or end that process yourself"),
                ));
            }
            Ok(None) | Err(_) => return Err(other(disconnected())),
            Ok(Some(_)) => {
                return Err(other(CliError::new(
                    Code::ProtocolError,
                    "unexpected handshake reply",
                )));
            }
        };
        let negotiated = welcome["protocol"].as_u64().unwrap_or(0);
        if !(proto::PROTOCOL_MIN..=proto::PROTOCOL_MAX).contains(&negotiated)
            || welcome["host"]["pid"].as_u64() != Some(u64::from(pid))
        {
            return Err(other(CliError::new(
                Code::ProtocolError,
                "the Host's welcome does not match the verified server process or protocol",
            )));
        }
        Ok(Self {
            pipe: Arc::new(pipe),
            next_id: Arc::new(AtomicU64::new(1)),
            welcome,
            host_pid: pid,
        })
    }

    pub fn request(&mut self, method: &str, params: &Value) -> Result<Value, CliError> {
        self.request_within(method, params, proto::REQUEST_TIMEOUT)
    }

    pub fn request_within(
        &mut self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        send(&self.pipe, &proto::request(id, method, params))?;
        let deadline = Instant::now() + timeout;
        loop {
            match proto::read_frame(&mut &*self.pipe, Some(deadline), |t| {
                t + proto::FRAME_TIMEOUT
            }) {
                Ok(Some(Frame::Control(m))) if m["type"] == "response" && m["id"] == id => {
                    return if m["ok"] == true {
                        Ok(m["result"].clone())
                    } else {
                        Err(host_error(
                            m["error"]["code"].as_str().unwrap_or("?"),
                            m["error"]["message"].as_str().unwrap_or("request failed"),
                            &m["error"]["details"],
                        ))
                    };
                }
                Ok(Some(Frame::Control(m))) if m["type"] == "error" => {
                    return Err(handshake_refused(&m["error"]));
                }
                Ok(Some(_)) => {} // events and data are not for a plain request
                Ok(None) => return Err(disconnected()),
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(CliError::new(
                        Code::Timeout,
                        format!(
                            "the Host did not answer `{method}` within {} s",
                            timeout.as_secs()
                        ),
                    ));
                }
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

/// How a Host started on demand is configured.
#[derive(Debug, Clone)]
pub struct Plan {
    pub shell: Option<String>,
    pub idle_exit_s: u64,
}

impl Plan {
    pub fn from_args(a: &Args) -> Self {
        Self {
            shell: if a.no_shell {
                None
            } else {
                Some(a.shell.clone().unwrap_or_else(default_shell))
            },
            idle_exit_s: a.idle_exit.unwrap_or(host::DEFAULT_IDLE_EXIT_S),
        }
    }

    fn args(&self, endpoint: &str, breakaway: bool) -> Vec<String> {
        let mut v: Vec<String> = ["host", "serve", "--endpoint", endpoint]
            .map(String::from)
            .to_vec();
        match &self.shell {
            Some(s) => v.extend(["--shell".into(), s.clone()]),
            None => v.push("--no-shell".into()),
        }
        v.extend([
            "--idle-exit".into(),
            self.idle_exit_s.to_string(),
            "--job-breakaway".into(),
            breakaway.to_string(),
        ]);
        v
    }
}

/// Waits until a stopping Host has released the endpoint.
fn wait_gone(endpoint: &str) -> Result<(), CliError> {
    let until = Instant::now() + STOPPING_TIMEOUT;
    loop {
        match Session::try_open(endpoint, Duration::from_millis(100)) {
            Err(Failure {
                why: Why::NotRunning,
                ..
            }) => return Ok(()),
            Err(Failure {
                why: Why::Stopping, ..
            }) => {}
            Ok(_) => return Ok(()), // a new Host already took over; the caller reconnects
            Err(f) => return Err(f.err),
        }
        if Instant::now() > until {
            return Err(CliError::new(
                Code::HostUnresponsive,
                "the Host has been stopping for 15 s and still holds the endpoint",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The result of [`connect_or_start`].
#[derive(Debug)]
pub struct Connected {
    pub session: Session,
    /// The process id of the Host this call started, `None` when one already ran.
    pub started: Option<u32>,
    /// The start lock was abandoned by a starter that crashed mid-start.
    pub lock_abandoned: bool,
}

/// Start on demand (ADR 0003 §3): connect to the Host, or start one. Several callers at once
/// get one Host: the start lock serializes starters, and whoever holds it re-checks the endpoint
/// before spawning. A stopping Host is waited for; any other refusal (identity mismatch, an
/// unresponsive or incompatible Host) is returned as is: nothing is killed or replaced.
pub fn connect_or_start(endpoint: &str, plan: &Plan) -> Result<Connected, CliError> {
    let attempt = |wait| match Session::try_open(endpoint, wait) {
        Ok(s) => Ok(Some(s)),
        Err(Failure {
            why: Why::NotRunning,
            ..
        }) => Ok(None),
        Err(Failure {
            why: Why::Stopping, ..
        }) => wait_gone(endpoint).map(|()| None),
        Err(f) => Err(f.err),
    };
    if let Some(session) = attempt(Duration::from_millis(500))? {
        return Ok(Connected {
            session,
            started: None,
            lock_abandoned: false,
        });
    }
    let lock = NamedLock::acquire(&start_lock_name(endpoint), LOCK_TIMEOUT).map_err(|e| {
        if e.kind() == io::ErrorKind::TimedOut {
            CliError::new(
                Code::Timeout,
                "another client has been starting the Host for 20 s",
            )
        } else {
            CliError::new(Code::Io, format!("cannot take the Host start lock: {e}"))
        }
    })?;
    // Another starter may have finished while this one waited for the lock.
    if let Some(session) = attempt(Duration::from_millis(500))? {
        return Ok(Connected {
            session,
            started: None,
            lock_abandoned: lock.abandoned,
        });
    }
    let spawned = spawn_host(endpoint, plan)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match Session::try_open(endpoint, Duration::from_millis(200)) {
            Ok(session) => {
                let started = (session.host_pid == spawned.pid).then_some(spawned.pid);
                return Ok(Connected {
                    session,
                    started,
                    lock_abandoned: lock.abandoned,
                });
            }
            Err(Failure {
                why: Why::NotRunning,
                ..
            }) => {}
            Err(f) => return Err(f.err),
        }
        if let Some(code) = win::exit_code(spawned.process.raw()) {
            return Err(if code == u32::from(Code::HostAlreadyRunning.exit_code()) {
                CliError::new(
                    Code::HostAlreadyRunning,
                    "another Host already serves this endpoint",
                )
            } else if code == u32::from(Code::HostIdentityMismatch.exit_code()) {
                mismatch(endpoint, None, "another program took the pipe name")
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

fn host_exe() -> Result<PathBuf, CliError> {
    std::env::current_exe()
        .map_err(|e| CliError::new(Code::Io, format!("cannot locate subak.exe: {e}")))
}

fn spawn_host(endpoint: &str, plan: &Plan) -> Result<spawn::Spawned, CliError> {
    let exe = host_exe()?;
    // ADR 0003 §6: the Host gets the user's logon environment, not this terminal's.
    let env = EnvBlock::for_current_user().ok();
    let cwd = host_cwd(env.as_ref())?;
    spawn::spawn_detached(&exe, |b| plan.args(endpoint, b), &cwd, env.as_ref())
        .map_err(|e| CliError::new(Code::SpawnFailed, format!("cannot start the Host: {e}")))
}

fn host_cwd(env: Option<&EnvBlock>) -> Result<PathBuf, CliError> {
    profile_dir(env)
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| CliError::new(Code::Io, "no working directory for the Host"))
}

fn job_json(j: Option<win::JobLimits>) -> Value {
    j.map_or(Value::Null, |j| {
        json!({
            "kill_on_close": j.kill_on_close,
            "breakaway_ok": j.breakaway_ok,
            "silent_breakaway_ok": j.silent_breakaway_ok,
        })
    })
}

/// `host start`: start a detached Host and wait until it serves the endpoint.
pub fn start(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let plan = Plan::from_args(a);
    // The job this command runs in decides whether the Host can leave it (risk R-03).
    let launcher_job = job_json(win::current_job_limits());
    if a.dry_run {
        let env = EnvBlock::for_current_user().ok();
        return Ok(json!({
            "dry_run": true,
            "would_start": {
                "program": host_exe()?.display().to_string(),
                "arguments": plan.args(endpoint, true),
                "cwd": host_cwd(env.as_ref())?.display().to_string(),
                "creation_flags": spawn::FLAG_NAMES,
                "stdio": "NUL (no other inherited handles)",
                "environment": if env.is_some() { "user (CreateEnvironmentBlock)" } else { "inherited" },
                "endpoint": pipe::path(endpoint),
                "start_lock": start_lock_name(endpoint),
                "shell": plan.shell,
                "idle_exit_s": plan.idle_exit_s,
                "launcher_job": launcher_job,
            },
        }));
    }
    let Connected {
        mut session,
        started,
        lock_abandoned,
    } = connect_or_start(endpoint, &plan)?;
    let st = session.request("host.status", &json!({}))?;
    if started.is_none() {
        return Err(CliError::new(
            Code::HostAlreadyRunning,
            format!("a Host already serves `{}`", pipe::path(endpoint)),
        )
        .detail(format!("host pid: {}", session.host_pid))
        .hint("attach with `subak attach`, or stop it with `subak host stop`"));
    }
    Ok(json!({
        "started": true,
        "endpoint": st["endpoint"],
        "host": st["host"],
        "pane": st["pane"],
        "launcher_job": launcher_job,
        "start_lock_abandoned": lock_abandoned,
    }))
}

pub fn status(endpoint: &str) -> Result<Value, CliError> {
    Session::open(endpoint, Duration::from_secs(2))?.request("host.status", &json!({}))
}

/// `host health`: the heartbeat. Connects (identity checks, handshake) and asks `host.health`.
pub fn health(endpoint: &str) -> Result<Value, CliError> {
    let t = Instant::now();
    let mut s = Session::open(endpoint, Duration::from_secs(2))?;
    let h = s.request("host.health", &json!({}))?;
    let ms = u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
    if h["healthy"] != true {
        return Err(CliError::new(
            Code::HostDisconnected,
            format!("Host pid {} is shutting down", s.host_pid),
        ));
    }
    Ok(json!({
        "healthy": true,
        "endpoint": pipe::path(endpoint),
        "host": {
            "pid": h["pid"], "version": h["version"], "protocol": h["protocol"],
            "uptime_s": h["uptime_s"], "terminals": h["terminals"], "clients": h["clients"],
        },
        "identity": {"same_user": true, "image": HOST_IMAGE},
        "features": s.welcome["features"],
        "latency_ms": ms,
    }))
}

/// Whether this command may ask the user (CLI_CONTRACT §4.4).
pub fn can_prompt(a: &Args) -> bool {
    let env_no_input = std::env::var("KIM_NO_INPUT")
        .is_ok_and(|v| !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false")));
    !a.json
        && !a.no_input
        && !env_no_input
        && io::stdin().is_terminal()
        && io::stderr().is_terminal()
}

/// Asks a yes/no question on stderr when allowed (CLI_CONTRACT §4), or fails with
/// `confirmation_required` naming what `--yes` would confirm. `--yes` skips the question.
pub fn confirm(a: &Args, question: &str, what: &str, hint: &str) -> Result<(), CliError> {
    if a.yes {
        return Ok(());
    }
    if !can_prompt(a) {
        return Err(
            CliError::new(Code::ConfirmationRequired, what.to_string()).hint(hint.to_string())
        );
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    if matches!(line.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err(CliError::new(Code::Cancelled, "nothing changed"))
    }
}

/// `host stop`: ends the live Terminals (only with `--kill-terminals`, destructive, needs
/// consent; ADR 0003 §7) and then the Host.
pub fn stop(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = Session::open(endpoint, Duration::from_secs(2))?;
    let st = s.request("host.status", &json!({}))?;
    if a.dry_run {
        let plan = s.request("host.stop", &json!({"dry_run": true}))?;
        return Ok(json!({"dry_run": true, "would_stop": {
            "host": st["host"], "pane": st["pane"], "clients": st["clients"],
            "would_kill": plan["would_kill"], "would_end": plan["would_end"],
            "needs_kill_terminals": !plan["would_end"].as_array().is_none_or(Vec::is_empty),
        }}));
    }
    let plan = s.request("host.stop", &json!({"dry_run": true}))?;
    let ending = plan["would_end"].as_array().cloned().unwrap_or_default();
    if !ending.is_empty() {
        let names: Vec<String> = ending
            .iter()
            .map(|t| {
                format!(
                    "`{}` ({})",
                    t["name"].as_str().unwrap_or("?"),
                    t["terminal_id"].as_str().unwrap_or("?")
                )
            })
            .collect();
        if !a.kill_terminals {
            let mut e = CliError::new(
                Code::TerminalsRunning,
                format!(
                    "the Host owns {} live Terminal(s); stopping it would end them",
                    ending.len()
                ),
            )
            .hint("stop or kill them first (`subak stop|kill`), or pass --kill-terminals (with --yes when it cannot ask)");
            for n in &names {
                e = e.detail(format!("live Terminal: {n}"));
            }
            return Err(e);
        }
        let processes: u64 = ending.iter().filter_map(|t| t["processes"].as_u64()).sum();
        confirm(
            a,
            &format!(
                "Stop the Subak Host (pid {}) and end {} Terminal(s) ({}) with {processes} process(es)?",
                st["host"]["pid"],
                ending.len(),
                names.join(", ")
            ),
            "stopping the Host ends every live Terminal and every process started in it",
            "re-run with --kill-terminals --yes, or with --dry-run to see what would end",
        )?;
    }
    let live = !ending.is_empty();
    let host_proc = win::open_process(s.host_pid);
    let report = s.request_within(
        "host.stop",
        &json!({"dry_run": false, "kill_terminals": live}),
        STOP_TIMEOUT,
    )?;
    let exited = host_proc
        .as_ref()
        .is_none_or(|h| win::wait(h.raw(), Some(Duration::from_secs(10))));
    Ok(json!({
        "stopped": true,
        "host": {"pid": s.host_pid, "exited": exited},
        "shell": report["shell"],
        "terminals": report["terminals"],
    }))
}

/// How an attach ended.
enum End {
    /// By this client: `Ctrl+B d`, end of piped input, Ctrl+Break, or its output went away.
    Detached,
    /// By another client's `client.detach` (`subak detach`); `by` names it.
    DetachedBy(Value),
    Ended {
        exit_code: Value,
        stopped: Value,
    },
}

/// Sends `pane.detach` for an attach and remembers its id, so the stream loop recognizes the
/// answer. Shared by the input thread and the console control handler.
#[derive(Clone)]
struct Detacher {
    pipe: Arc<Pipe>,
    ids: Arc<AtomicU64>,
    detach_id: Arc<AtomicU64>,
    /// Serializes whole frames: the input thread and the console control handler both write.
    writing: Arc<std::sync::Mutex<()>>,
}

impl Detacher {
    /// Writes one whole frame.
    fn write(&self, frame: &[u8]) -> io::Result<()> {
        let _guard = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pipe.write_all_bytes(frame)
    }

    fn request(&self, method: &str, params: &Value) -> bool {
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        if method == "pane.detach" {
            self.detach_id.store(id, Ordering::SeqCst);
        }
        self.write(&proto::encode_control(&proto::request(id, method, params)))
            .is_ok()
    }

    fn detach(&self) -> bool {
        self.request("pane.detach", &json!({}))
    }
}

/// `attach [<target>]` and `host attach` (`target` = `None`: the Host's only running pane):
/// stream the pane until detach (`Ctrl+B d`, the end of piped input, Ctrl+Break, or another
/// client's `subak detach`), the pane's end or the Host's stop. Detaching never stops anything,
/// and every exit path the process survives puts its console back ([`console::restore`]).
pub fn attach(a: &Args, endpoint: &str, target: Option<&str>) -> Result<Value, CliError> {
    let mut s = Session::open(endpoint, Duration::from_secs(2))?;
    let console_in = console::stdin_is_console();
    let console_out = console::stdout_is_console();
    let interactive = !a.read_only;
    let size = a.size.filter(|_| interactive).or_else(|| {
        if console_in && interactive {
            console::window_size()
        } else {
            None
        }
    });
    let size_json = size.map(|(c, r)| json!({"cols": c, "rows": r}));
    if a.dry_run {
        let st = s.request("host.status", &json!({}))?;
        let terminal = match target {
            Some(t) => s.request("terminal.info", &json!({"target": t}))?["terminal"].clone(),
            None => Value::Null,
        };
        return Ok(json!({"dry_run": true, "would_attach": {
            "host": st["host"], "pane": st["pane"], "terminal": terminal,
            "size": size_json, "interactive": interactive,
            "input": if a.read_only { "none" } else if console_in { "console" } else { "redirected" },
        }}));
    }
    // Names for the notices; listed before attaching, because output follows the attach at once.
    let names = s.request("terminal.list", &json!({"detail": "summary"}))?;
    s.request("events.subscribe", &json!({"topics": ["panes", "host"]}))?;
    let res = s.request(
        "pane.attach",
        &json!({"target": target, "interactive": interactive, "size": size_json}),
    )?;
    let host_pid = s.host_pid;
    let pane_id = res["pane_id"].clone();
    let name = names["terminals"]
        .as_array()
        .and_then(|l| l.iter().find(|t| t["id"] == res["terminal_id"]))
        .map_or(Value::Null, |t| t["name"].clone());
    let stream = res["stream_id"]
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| CliError::new(Code::ProtocolError, "pane.attach returned no stream id"))?;
    if !a.quiet {
        let shown = name.as_str().unwrap_or("?");
        let input = if a.read_only {
            "; read-only: keys are not sent"
        } else if console_in {
            ""
        } else {
            "; input is not a console: its bytes are sent as they are, and its end detaches"
        };
        eprintln!(
            "[subak: attached to `{shown}` ({}) in Host pid {host_pid}, pid {}; Ctrl+B d detaches{input}]",
            text(&res["terminal_id"]),
            res["pane"]["pid"]
        );
    }
    let detacher = Detacher {
        pipe: Arc::clone(&s.pipe),
        ids: Arc::clone(&s.next_id),
        detach_id: Arc::new(AtomicU64::new(0)),
        writing: Arc::new(std::sync::Mutex::new(())),
    };
    let raw = (console_in || console_out).then(console::RawMode::enable);
    // A panic anywhere still puts the console back; Ctrl+Break detaches cleanly.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        console::restore();
        previous_hook(info);
    }));
    console::handle_signals(Box::new({
        let d = detacher.clone();
        move || {
            d.detach();
        }
    }));
    // A read-only attach with redirected input reads nothing: it runs until it is detached
    // (`subak detach`, Ctrl+Break), the pane ends or the Host stops.
    if console_in || interactive {
        std::thread::spawn({
            let d = detacher.clone();
            let verbose = a.verbose;
            move || input(&d, stream, console_in, interactive, size, verbose)
        });
    }
    let end = stream_output(&s, &pane_id, &detacher, a.verbose);
    console::release_signals();
    drop(raw);
    let _ = std::panic::take_hook(); // ours; the default hook is back
    let end = end?;
    Ok(match end {
        End::Detached => json!({
            "detached": true, "detached_by": null, "host": {"pid": host_pid}, "pane": res["pane"],
            "terminal_id": res["terminal_id"], "name": name,
        }),
        End::DetachedBy(by) => json!({
            "detached": true, "detached_by": by, "host": {"pid": host_pid}, "pane": res["pane"],
            "terminal_id": res["terminal_id"], "name": name,
        }),
        End::Ended { exit_code, stopped } => json!({
            "detached": false, "host": {"pid": host_pid, "stopped": stopped},
            "pane": res["pane"], "terminal_id": res["terminal_id"], "name": name,
            "shell_exit_code": exit_code,
        }),
    })
}

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), String::from)
}

/// The output side of an attach: pane data to stdout until the attach ends.
fn stream_output(
    s: &Session,
    pane_id: &Value,
    detacher: &Detacher,
    verbose: bool,
) -> Result<End, CliError> {
    let mut stdout = io::stdout().lock();
    let mut stopped = Value::Null;
    // This client is the pane's terminal and does not grant win32-input-mode (see OutputFilter).
    let mut filter = proto::OutputFilter::default();
    // What the pane switches on in this terminal, so a detach can switch it off again.
    let mut modes = console::ModeTracker::default();
    loop {
        match proto::read_frame(&mut &*s.pipe, None, |t| t + proto::FRAME_TIMEOUT) {
            Ok(Some(Frame::Data { bytes, .. })) => {
                let shown = filter.feed(&bytes);
                modes.feed(&shown);
                if stdout
                    .write_all(&shown)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    // Our own output is gone (e.g. the reader of a pipe went away): detach.
                    return Ok(End::Detached);
                }
            }
            Ok(Some(Frame::Control(m))) => match m["type"].as_str() {
                Some("event")
                    if m["event"] == "pane.exited" && m["data"]["pane_id"] == *pane_id =>
                {
                    return Ok(End::Ended {
                        exit_code: m["data"]["exit_code"].clone(),
                        stopped,
                    });
                }
                Some("event")
                    if m["event"] == "pane.detached" && m["data"]["pane_id"] == *pane_id =>
                {
                    return Ok(End::DetachedBy(m["data"]["by"].clone()));
                }
                // Step 13: this client fell behind; the Host skipped output and a snapshot that
                // repaints the screen follows (nothing to discard: frames are written at once).
                Some("event") if m["event"] == "pane.resync" && verbose => {
                    eprint!("subak: fell behind the pane's output; repainting\r\n");
                }
                Some("event") if m["event"] == "host.stopping" => {
                    stopped = m["data"]["reason"].clone();
                }
                Some("response") if m["id"] == detacher.detach_id.load(Ordering::SeqCst) => {
                    if verbose {
                        eprint!("subak: detach acknowledged by the Host\r\n");
                    }
                    return Ok(End::Detached);
                }
                _ => {}
            },
            Ok(None) | Err(_) if !stopped.is_null() => {
                return Ok(End::Ended {
                    exit_code: Value::Null,
                    stopped,
                });
            }
            Ok(None) | Err(_) => return Err(disconnected()),
        }
    }
}

/// The input side of an attach (its own thread): keys or piped bytes → data frames. A read-only
/// attach reads console keys only to see the detach keys, and sends nothing else.
fn input(
    d: &Detacher,
    stream: u32,
    console_in: bool,
    interactive: bool,
    size: Option<(u16, u16)>,
    verbose: bool,
) {
    let mut keys = DetachKeys::default();
    let mut forward = |bytes: &[u8]| -> Option<bool> {
        let (fwd, detach) = keys.feed(bytes);
        if !fwd.is_empty() && interactive {
            for chunk in fwd.chunks(32 * 1024) {
                d.write(&proto::encode_data(stream, 0, FLAG_INPUT, chunk))
                    .ok()?;
            }
        }
        Some(detach)
    };
    if console_in {
        let mut size = size.unwrap_or((0, 0));
        let mut high = None;
        loop {
            let Ok(events) =
                console::read_input(&mut size, &mut high, Duration::from_millis(250), verbose)
            else {
                d.detach();
                return;
            };
            for ev in events {
                match ev {
                    console::Input::Bytes(b) => match forward(&b) {
                        Some(false) => {}
                        Some(true) => {
                            if verbose {
                                eprint!("subak: Ctrl+B d: detaching\r\n");
                            }
                            d.detach();
                            return;
                        }
                        None => return,
                    },
                    // A viewer never resizes the pane (the Host refuses it).
                    console::Input::Resize(..) if !interactive => {}
                    console::Input::Resize(c, r) => {
                        if !d.request(
                            "pane.resize",
                            &json!({"cols": c, "rows": r, "stream_id": stream}),
                        ) {
                            return;
                        }
                    }
                }
            }
        }
    }
    let request = |method: &str, params: Value| d.request(method, &params);
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

/// The serve options for `host serve` from the command line.
pub fn serve_opts(a: &Args, endpoint: String) -> host::ServeOpts {
    let plan = Plan::from_args(a);
    host::ServeOpts {
        endpoint,
        shell: plan.shell,
        job_breakaway: a.job_breakaway,
        idle_exit: Duration::from_secs(plan.idle_exit_s),
    }
}

/// `host serve --dry-run`.
pub fn serve_plan(opts: &host::ServeOpts) -> Value {
    let env = EnvBlock::for_current_user().ok();
    json!({"dry_run": true, "would_serve": {
        "endpoint": pipe::path(&opts.endpoint),
        "shell": opts.shell,
        "idle_exit_s": opts.idle_exit.as_secs(),
        "cwd": profile_dir(env.as_ref()).map(|p| p.display().to_string()),
        "environment": if env.is_some() { "user (CreateEnvironmentBlock)" } else { "inherited" },
    }})
}
