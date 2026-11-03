//! `subak host serve` — the Step 08 **prototype** Host (ADR 0003, ADR 0004, ADR 0016).
//!
//! One Host owns exactly one pane: a ConPTY running one shell inside a kill-on-close job. Clients
//! connect over the prototype pipe ([`crate::proto`]), attach, send input, resize, detach, ask for
//! status, or stop the Host. Threads: an accept loop, one handler + one writer per connection, a
//! ConPTY output reader that never blocks on clients, and an exit waiter. Idle, every thread is
//! blocked in the kernel; nothing polls (`KIM-NFR-001`).
//!
//! Prototype limits (Step 09+): the Host lives exactly as long as its shell (no idle grace, no
//! sessions model); replay is raw recent output rather than a VT snapshot (Step 13); a lagging
//! client is disconnected instead of resynchronized (ADR 0008); no peer identity checks.

use crate::output::{CliError, Code};
use crate::proto::{self, FLAG_INPUT, FLAG_SNAPSHOT, Frame, PROTOCOL, Ring, STREAM};
use crate::win::conpty::{self, Pty};
use crate::win::pipe::{self, Deadline, Listener, Pipe};
use crate::win::{self, Handle};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Initial pseudo console size until a client attaches with its own.
pub const DEFAULT_SIZE: (u16, u16) = (120, 30);
/// Frames queued per connection before a slow client is dropped.
const QUEUE: usize = 4096;
/// How long a graceful stop waits for the shell after `ClosePseudoConsole`.
const GRACE: Duration = Duration::from_secs(5);
const HANDSHAKE: Duration = Duration::from_secs(5);

/// What `host serve` needs.
#[derive(Debug)]
pub struct ServeOpts {
    pub endpoint: String,
    pub shell: String,
    pub job_breakaway: Option<bool>,
}

/// The shell started when `--shell` is not given.
pub fn default_shell() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
}

/// The user profile, the Host's working directory (ADR 0003 §2).
pub fn profile_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

struct Out {
    bytes: Vec<u8>,
    ack: Option<mpsc::Sender<()>>,
}

struct Conn {
    tx: mpsc::SyncSender<Out>,
    pipe: Arc<Pipe>,
    attached: bool,
    pid: Option<u32>,
}

struct State {
    ring: Ring,
    seq: u64,
    conns: HashMap<u64, Conn>,
    next_conn: u64,
    size: (u16, u16),
    stopping: bool,
    event_seq: u64,
}

enum Shutdown {
    Stop { conn: u64, id: Value },
    ShellExited,
}

struct Host {
    pty: Arc<Pty>,
    state: Mutex<State>,
    shutdown: mpsc::Sender<Shutdown>,
    pid: u32,
    started_at: String,
    endpoint: String,
    shell: String,
    job_breakaway: Option<bool>,
    in_job: bool,
    session: u32,
}

/// Runs the Host until its shell exits or a client stops it. Returns the final report.
pub fn serve(opts: &ServeOpts) -> Result<Value, CliError> {
    let mut listener = Listener::bind(&opts.endpoint).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            CliError::new(
                Code::HostAlreadyRunning,
                format!("a Host already serves endpoint `{}`", opts.endpoint),
            )
            .hint("attach to it with `subak host attach`, or stop it with `subak host stop`")
        } else {
            CliError::new(Code::Io, format!("cannot create the Host pipe: {e}"))
        }
    })?;
    let (cols, rows) = DEFAULT_SIZE;
    let pty = Pty::spawn(&opts.shell, profile_dir().as_deref(), cols, rows).map_err(|e| {
        CliError::new(
            Code::SpawnFailed,
            format!("cannot start `{}`: {e}", opts.shell),
        )
        .hint("check the --shell command line")
    })?;
    let pty = Arc::new(pty);
    let output = pty
        .take_output()
        .expect("a fresh pseudo console has its output end");
    let (sd_tx, sd_rx) = mpsc::channel();
    let host = Arc::new(Host {
        pty: Arc::clone(&pty),
        state: Mutex::new(State {
            ring: Ring::new(proto::HISTORY_BYTES),
            seq: 0,
            conns: HashMap::new(),
            next_conn: 1,
            size: DEFAULT_SIZE,
            stopping: false,
            event_seq: 0,
        }),
        shutdown: sd_tx,
        pid: win::current_pid(),
        started_at: proto::rfc3339(SystemTime::now()),
        endpoint: pipe::path(&opts.endpoint),
        shell: opts.shell.clone(),
        job_breakaway: opts.job_breakaway,
        in_job: win::current_process_in_job(),
        session: win::session_id(),
    });

    let reader = thread::spawn({
        let host = Arc::clone(&host);
        move || host.read_output(&output)
    });
    thread::spawn({
        let host = Arc::clone(&host);
        move || {
            host.pty.wait_exit(None);
            let _ = host.shutdown.send(Shutdown::ShellExited);
        }
    });
    let stopper = listener.stopper();
    let acceptor = thread::spawn({
        let host = Arc::clone(&host);
        move || {
            while let Ok(Some(p)) = listener.accept() {
                let host = Arc::clone(&host);
                thread::spawn(move || host.serve_conn(&Arc::new(p)));
            }
        }
    });

    let why = sd_rx.recv().unwrap_or(Shutdown::ShellExited);
    host.lock().stopping = true;
    let processes = pty.job_processes();
    let (requester, reason) = match why {
        Shutdown::Stop { conn, id } => (Some((conn, id)), "stop_requested"),
        Shutdown::ShellExited => (None, "shell_exited"),
    };
    // Graceful: ClosePseudoConsole (CTRL_CLOSE_EVENT to the shell) on a helper thread, because on
    // Windows 10 it blocks until conhost exits; the reader keeps draining meanwhile.
    let closer = thread::spawn({
        let pty = Arc::clone(&pty);
        move || pty.close_console()
    });
    let mut exit = pty.wait_exit(Some(GRACE));
    let forced = exit.is_none() || pty.job_processes() > 0;
    if forced {
        pty.terminate_job(1);
        exit = pty.wait_exit(Some(GRACE));
    }
    join_bounded(closer, GRACE);
    join_bounded(reader, GRACE); // all pane output reaches clients before the final events
    let report = json!({
        "reason": reason,
        "host": { "pid": host.pid },
        "shell": {
            "pid": pty.pid(),
            "exit_code": exit,
            "processes_in_job": processes,
            "forced": forced,
            "job_processes_left": pty.job_processes(),
        },
    });
    host.finish(requester.as_ref(), &report, reason);
    stopper.set();
    join_bounded(acceptor, Duration::from_secs(2));
    Ok(report)
}

/// Joins a thread, but gives up after `limit` (process exit reclaims anything left).
fn join_bounded(t: JoinHandle<()>, limit: Duration) {
    let until = Instant::now() + limit;
    while !t.is_finished() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    if t.is_finished() {
        let _ = t.join();
    }
}

fn response(id: &Value, result: Result<Value, (&str, String)>) -> Vec<u8> {
    proto::encode_control(&match result {
        Ok(r) => json!({"type": "response", "id": id, "ok": true, "result": r}),
        Err((code, message)) => json!({
            "type": "response", "id": id, "ok": false,
            "error": {"code": code, "message": message, "details": {}},
        }),
    })
}

fn size_param(v: &Value) -> Result<Option<(u16, u16)>, (&'static str, String)> {
    if v.is_null() {
        return Ok(None);
    }
    let dim = |k: &str| {
        v[k].as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| (2..=1000).contains(n))
    };
    match (dim("cols"), dim("rows")) {
        (Some(c), Some(r)) => Ok(Some((c, r))),
        _ => Err((
            "invalid_params",
            "size needs cols and rows between 2 and 1000".into(),
        )),
    }
}

enum Reply {
    Now(Result<Value, (&'static str, String)>),
    Queued,
}

impl Host {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read_output(&self, out: &Handle) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match conpty::read_output(out, &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut st = self.lock();
            st.ring.push(&buf[..n]);
            let seq = st.seq;
            st.seq += n as u64;
            let frame = proto::encode_data(STREAM, seq, 0, &buf[..n]);
            let lagging: Vec<u64> = st
                .conns
                .iter()
                .filter(|(_, c)| {
                    c.attached
                        && c.tx
                            .try_send(Out {
                                bytes: frame.clone(),
                                ack: None,
                            })
                            .is_err()
                })
                .map(|(id, _)| *id)
                .collect();
            for id in lagging {
                // Never block the pane on a client: drop it (resync is Step 13, ADR 0008).
                if let Some(c) = st.conns.remove(&id) {
                    c.pipe.cancel();
                }
            }
        }
    }

    fn status(&self) -> Value {
        let st = self.lock();
        let attached = st.conns.values().filter(|c| c.attached).count();
        let clients: Vec<Value> = st
            .conns
            .values()
            .map(|c| json!({"pid": c.pid, "attached": c.attached}))
            .collect();
        json!({
            "prototype": "step-08",
            "protocol": PROTOCOL,
            "endpoint": self.endpoint,
            "host": {
                "pid": self.pid,
                "version": env!("CARGO_PKG_VERSION"),
                "started_at": self.started_at,
                "session_id": self.session,
                "job_breakaway": self.job_breakaway,
                "in_job": self.in_job,
                "stopping": st.stopping,
            },
            "pane": {
                "pid": self.pty.pid(),
                "command": self.shell,
                "running": win::exit_code(self.pty.process_handle()).is_none(),
                "size": {"cols": st.size.0, "rows": st.size.1},
                "job_processes": self.pty.job_processes(),
                "output_bytes": st.seq,
            },
            "clients": {"connected": st.conns.len(), "attached": attached, "list": clients},
        })
    }

    fn event(st: &mut State, name: &str, data: &Value) -> Vec<u8> {
        st.event_seq += 1;
        proto::encode_control(
            &json!({"type": "event", "seq": st.event_seq, "event": name, "data": data}),
        )
    }

    /// Sends the final messages and waits (bounded) until they are written to every pipe.
    fn finish(&self, requester: Option<&(u64, Value)>, report: &Value, reason: &str) {
        let mut acks = Vec::new();
        let mut st = self.lock();
        let ids: Vec<u64> = st.conns.keys().copied().collect();
        for id in ids {
            let mut frames = Vec::new();
            if let Some((rid, req)) = requester
                && *rid == id
            {
                frames.push(response(req, Ok(report.clone())));
            }
            frames.push(Self::event(
                &mut st,
                "pane.exited",
                &json!({"pane": 1, "pid": self.pty.pid(), "exit_code": report["shell"]["exit_code"]}),
            ));
            frames.push(Self::event(
                &mut st,
                "host.stopping",
                &json!({"reason": reason}),
            ));
            let conn = &st.conns[&id];
            let last = frames.len() - 1;
            for (i, bytes) in frames.into_iter().enumerate() {
                let (atx, arx) = mpsc::channel();
                let ack = (i == last).then_some(atx);
                if conn.tx.try_send(Out { bytes, ack }).is_ok() && i == last {
                    acks.push(arx);
                }
            }
        }
        drop(st);
        let until = Instant::now() + Duration::from_secs(2);
        for a in acks {
            let _ = a.recv_timeout(until.saturating_duration_since(Instant::now()));
        }
    }

    fn serve_conn(self: Arc<Self>, pipe: &Arc<Pipe>) {
        let (tx, rx) = mpsc::sync_channel::<Out>(QUEUE);
        let writer = thread::spawn({
            let pipe = Arc::clone(pipe);
            move || {
                for out in rx {
                    if pipe.write_all_bytes(&out.bytes).is_err() {
                        pipe.cancel();
                        return;
                    }
                    if let Some(a) = out.ack {
                        let _ = a.send(());
                    }
                }
            }
        });
        if let Some(id) = self.handshake(pipe, &tx) {
            self.requests(id, pipe, &tx);
            self.lock().conns.remove(&id);
        }
        drop(tx);
        // Unblocks a writer stuck on a client that stopped reading.
        let until = Instant::now() + Duration::from_secs(2);
        while !writer.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        pipe.cancel();
        let _ = writer.join();
    }

    fn send_final(tx: &mpsc::SyncSender<Out>, bytes: Vec<u8>) {
        let (atx, arx) = mpsc::channel();
        if tx
            .try_send(Out {
                bytes,
                ack: Some(atx),
            })
            .is_ok()
        {
            let _ = arx.recv_timeout(Duration::from_secs(1));
        }
    }

    fn handshake(&self, pipe: &Arc<Pipe>, tx: &mpsc::SyncSender<Out>) -> Option<u64> {
        let mut dl = Deadline {
            pipe,
            until: Instant::now() + HANDSHAKE,
        };
        let hello = match proto::read_frame(&mut dl) {
            Ok(Some(Frame::Control(m))) if m["type"] == "hello" => m,
            _ => {
                Self::send_final(
                    tx,
                    proto::encode_control(&json!({"type": "error", "error": {
                        "code": "protocol_error",
                        "message": "expected hello within 5 s", "details": {}}})),
                );
                return None;
            }
        };
        let min = hello["protocol"]["min"].as_u64().unwrap_or(u64::MAX);
        let max = hello["protocol"]["max"].as_u64().unwrap_or(0);
        if !(min..=max).contains(&PROTOCOL) {
            Self::send_final(
                tx,
                proto::encode_control(&json!({"type": "error", "error": {
                    "code": "protocol_unsupported",
                    "message": "this is the Step 08 prototype Host (protocol 0)",
                    "details": {"supported": {"min": PROTOCOL, "max": PROTOCOL}}}})),
            );
            return None;
        }
        let mut st = self.lock();
        if st.stopping {
            return None;
        }
        let id = st.next_conn;
        st.next_conn += 1;
        st.conns.insert(
            id,
            Conn {
                tx: tx.clone(),
                pipe: Arc::clone(pipe),
                attached: false,
                pid: pipe.client_pid(),
            },
        );
        let welcome = json!({
            "type": "welcome",
            "protocol": PROTOCOL,
            "host": {"version": env!("CARGO_PKG_VERSION"), "pid": self.pid, "started_at": self.started_at},
            "features": ["step08-prototype", "single-pane"],
        });
        let _ = tx.try_send(Out {
            bytes: proto::encode_control(&welcome),
            ack: None,
        });
        Some(id)
    }

    fn requests(&self, id: u64, pipe: &Arc<Pipe>, tx: &mpsc::SyncSender<Out>) {
        loop {
            let frame = match proto::read_frame(&mut &**pipe) {
                Ok(Some(f)) => f,
                Ok(None) => return,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::InvalidData {
                        Self::send_final(
                            tx,
                            proto::encode_control(&json!({"type": "error", "error": {
                                "code": "protocol_error", "message": e.to_string(), "details": {}}})),
                        );
                    }
                    return;
                }
            };
            match frame {
                Frame::Data { flags, bytes, .. } if flags & FLAG_INPUT != 0 => {
                    let attached = {
                        let st = self.lock();
                        !st.stopping && st.conns.get(&id).is_some_and(|c| c.attached)
                    };
                    if attached {
                        let _ = self.pty.write_input(&bytes);
                    }
                }
                Frame::Data { .. } => return,
                Frame::Control(m) => {
                    if m["type"] != "request" {
                        return;
                    }
                    let rid = m["id"].clone();
                    let reply =
                        self.handle(id, &rid, m["method"].as_str().unwrap_or(""), &m["params"]);
                    if let Reply::Now(r) = reply {
                        let _ = tx.try_send(Out {
                            bytes: response(&rid, r),
                            ack: None,
                        });
                    }
                }
            }
        }
    }

    fn handle(&self, conn: u64, rid: &Value, method: &str, params: &Value) -> Reply {
        if self.lock().stopping && method != "host.status" {
            return Reply::Now(Err(("host_stopping", "the Host is shutting down".into())));
        }
        match method {
            "host.status" => Reply::Now(Ok(self.status())),
            "host.stop" if params["dry_run"] == true => {
                Reply::Now(Ok(json!({"dry_run": true, "would_stop": self.status()})))
            }
            "host.stop" => {
                let _ = self.shutdown.send(Shutdown::Stop {
                    conn,
                    id: rid.clone(),
                });
                Reply::Queued
            }
            "pane.attach" => self.attach(conn, rid, params),
            "pane.resize" => Reply::Now(size_param(params).and_then(|s| {
                let (c, r) =
                    s.ok_or(("invalid_params", "cols and rows are required".to_string()))?;
                self.pty
                    .resize(c, r)
                    .map_err(|e| ("internal_error", format!("resize failed: {e}")))?;
                self.lock().size = (c, r);
                Ok(json!({"cols": c, "rows": r}))
            })),
            "pane.detach" => {
                if let Some(c) = self.lock().conns.get_mut(&conn) {
                    c.attached = false;
                }
                Reply::Now(Ok(json!({})))
            }
            other => Reply::Now(Err((
                "unknown_request",
                format!("unknown method `{other}` (Step 08 prototype)"),
            ))),
        }
    }

    fn attach(&self, conn: u64, rid: &Value, params: &Value) -> Reply {
        let size = match size_param(&params["size"]) {
            Ok(s) => s,
            Err(e) => return Reply::Now(Err(e)),
        };
        if let Some((c, r)) = size
            && let Err(e) = self.pty.resize(c, r)
        {
            return Reply::Now(Err(("internal_error", format!("resize failed: {e}"))));
        }
        // Response, replay and the start of live output are queued under one lock, so the
        // reader cannot interleave: no gap, no duplicate.
        let mut st = self.lock();
        if let Some(s) = size {
            st.size = s;
        }
        let seq = st.seq;
        let replay = st.ring.replay();
        let cur = st.size;
        let Some(c) = st.conns.get_mut(&conn) else {
            return Reply::Queued;
        };
        c.attached = true;
        let result = json!({
            "stream_id": STREAM,
            "seq": seq,
            "pane": {"pid": self.pty.pid(), "size": {"cols": cur.0, "rows": cur.1}},
            "replay_bytes": replay.len(),
        });
        let _ = c.tx.try_send(Out {
            bytes: response(rid, Ok(result)),
            ack: None,
        });
        if !replay.is_empty() {
            let _ = c.tx.try_send(Out {
                bytes: proto::encode_data(
                    STREAM,
                    seq - replay.len() as u64,
                    FLAG_SNAPSHOT,
                    &replay,
                ),
                ack: None,
            });
        }
        Reply::Queued
    }
}
