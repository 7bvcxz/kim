//! `subak host serve` — the Host supervisor (ADR 0003, ADR 0005, ADR 0016, ADR 0019).
//!
//! The Host serves IPC v1 ([`crate::proto`]) on the per-user pipe. Every connection must pass,
//! in this order: the pipe's owner-only DACL (kernel), a `hello` within 5 s, the user SID of the
//! connection's own token equal to the Host's, a valid `hello`, a common protocol major. After
//! `welcome` it may send requests; what it may call depends on its client kind
//! ([`proto::authorize`]). Nothing in the protocol runs a command: the only way to reach a
//! process is typing into a pane, which needs an interactive attach by a `cli` or `gui` client.
//!
//! Lifetime (ADR 0003 §4): the Host runs while it owns a live pane or has a client. When both
//! are gone it waits the idle grace period (`--idle-exit`, default 10 s) and exits. It never
//! polls: idle, every thread is blocked in the kernel, and the only timed wait is that grace.
//!
//! Until the session model (Step 10) the Host owns at most one pane, the Step 08 prototype pane
//! (`--shell`, or none with `--no-shell`). When its shell exits the pane ends and the Host
//! idles out. Replay is raw recent output (Step 13); a lagging client is disconnected.
//!
//! Threads: an accept loop, a handler and a writer per connection, the pane's output reader and
//! exit waiter, and the main thread that supervises lifetime and teardown. A panic while
//! handling one request fails that request with `internal_error`; the Host continues.

use crate::output::{CliError, Code};
use crate::proto::{
    self, ClientKind, ErrorCode, FLAG_INPUT, FLAG_SNAPSHOT, Frame, IpcError, Refusal, Request,
    Ring, STREAM,
};
use crate::win::conpty::{self, Pty};
use crate::win::pipe::{self, Listener, Pipe};
use crate::win::{self, EnvBlock, Handle, JobLimits};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Initial pseudo console size until a client attaches with its own.
pub const DEFAULT_SIZE: (u16, u16) = (120, 30);
/// Idle grace period of ADR 0003 §4.
pub const DEFAULT_IDLE_EXIT_S: u64 = 10;
/// Most connections (in handshake or established) served at once; more get `busy`.
pub const MAX_CONNECTIONS: usize = 64;
/// Frames and bytes queued per connection before a slow client is dropped.
const QUEUE_FRAMES: usize = 4096;
const QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// How long a graceful stop waits for the shell after `ClosePseudoConsole`.
const GRACE: Duration = Duration::from_secs(5);

/// What `host serve` needs.
#[derive(Debug)]
pub struct ServeOpts {
    pub endpoint: String,
    /// The prototype pane's command line; `None` starts a Host that owns no pane.
    pub shell: Option<String>,
    pub job_breakaway: Option<bool>,
    pub idle_exit: Duration,
}

/// The shell started when `--shell` is not given.
pub fn default_shell() -> String {
    std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
}

/// The user profile, the Host's working directory (ADR 0003 §2): from the logon environment
/// when given, else from this process's environment.
pub fn profile_dir(env: Option<&EnvBlock>) -> Option<PathBuf> {
    env.and_then(|e| e.get("USERPROFILE"))
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

struct Out {
    bytes: Vec<u8>,
    ack: Option<mpsc::Sender<()>>,
}

/// The sending side of one connection: a bounded queue (frames and bytes) to its writer thread.
/// A client that falls that far behind is disconnected; it never blocks the Host.
#[derive(Clone)]
struct Sink {
    tx: mpsc::SyncSender<Out>,
    queued: Arc<AtomicUsize>,
    pipe: Arc<Pipe>,
}

impl Sink {
    fn push(&self, bytes: Vec<u8>, ack: Option<mpsc::Sender<()>>) -> bool {
        let n = bytes.len();
        if self.queued.fetch_add(n, Ordering::SeqCst) + n > QUEUE_BYTES
            || self.tx.try_send(Out { bytes, ack }).is_err()
        {
            self.queued.fetch_sub(n, Ordering::SeqCst);
            self.pipe.cancel(); // its handler sees the error and removes the connection
            return false;
        }
        true
    }

    fn control(&self, msg: &Value) -> bool {
        self.push(proto::encode_control(msg), None)
    }

    /// Queues a last message and waits (bounded) until it was written.
    fn last(&self, msg: &Value) {
        let (atx, arx) = mpsc::channel();
        if self.push(proto::encode_control(msg), Some(atx)) {
            let _ = arx.recv_timeout(Duration::from_secs(1));
        }
    }
}

struct Conn {
    sink: Sink,
    pid: Option<u32>,
    kind: ClientKind,
    name: String,
    version: String,
    /// `Some(interactive)` while attached to the pane.
    attach: Option<bool>,
    topics: Vec<String>,
    event_seq: u64,
}

impl Conn {
    fn event(&mut self, name: &str, data: &Value, ack: Option<mpsc::Sender<()>>) -> bool {
        self.event_seq += 1;
        self.sink.push(
            proto::encode_control(&proto::event(self.event_seq, name, data)),
            ack,
        )
    }
}

struct PaneState {
    ring: Ring,
    seq: u64,
    size: (u16, u16),
    input_bytes: u64,
    /// `Some` once the pane ended: the exit code (if known) and the teardown report.
    ended: Option<(Option<u32>, Value)>,
}

#[derive(Default)]
struct Stats {
    connections_total: u64,
    rejected_identity: u64,
    rejected_limit: u64,
    rejected_handshake: u64,
    rejected_stopping: u64,
    requests_total: u64,
    errors: BTreeMap<&'static str, u64>,
    protocol_errors: u64,
    panics: u64,
    input_frames_dropped: u64,
    lagging_clients_dropped: u64,
}

#[derive(Clone, Copy)]
enum Reject {
    Identity,
    Limit,
    Handshake,
    Stopping,
}

struct State {
    pane: Option<PaneState>,
    conns: HashMap<u64, Conn>,
    /// Accepted connections that have not finished their handshake.
    pending: usize,
    next_conn: u64,
    stopping: bool,
    stats: Stats,
}

impl State {
    fn pane_live(&self) -> bool {
        self.pane.as_ref().is_some_and(|p| p.ended.is_none())
    }

    fn idle(&self) -> bool {
        !self.pane_live() && self.conns.is_empty() && self.pending == 0
    }
}

struct PaneRt {
    id: String,
    pty: Arc<Pty>,
    command: String,
}

enum Ev {
    Stop { conn: u64, id: u64 },
    PaneExited,
    Changed,
}

enum Exit {
    Stop { conn: u64, id: u64 },
    Idle,
}

struct Host {
    pane: Option<PaneRt>,
    state: Mutex<State>,
    events: mpsc::Sender<Ev>,
    pid: u32,
    sid: String,
    started_at: String,
    started: Instant,
    endpoint: String,
    job_breakaway: Option<bool>,
    in_job: bool,
    job: Option<JobLimits>,
    session: u32,
    idle_exit: Duration,
    environment: &'static str,
}

/// Runs the Host until it is stopped or idles out. Returns the final report.
pub fn serve(opts: &ServeOpts) -> Result<Value, CliError> {
    let sid = win::user_sid()
        .map_err(|e| CliError::new(Code::Io, format!("cannot determine the current user: {e}")))?;
    let mut listener = match Listener::bind(&opts.endpoint) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(crate::client::occupied(&opts.endpoint));
        }
        Err(e) => {
            return Err(CliError::new(
                Code::Io,
                format!("cannot create the Host pipe: {e}"),
            ));
        }
    };
    // However the Host was started, panes must receive Ctrl+C normally.
    win::enable_ctrl_c_for_children();
    // Panes get the user's logon environment, never the launcher's (ADR 0003 §6).
    let env = EnvBlock::for_current_user().ok();
    let cwd = profile_dir(env.as_ref());
    let pane = match &opts.shell {
        None => None,
        Some(shell) => {
            let (cols, rows) = DEFAULT_SIZE;
            let pty = Pty::spawn(shell, cwd.as_deref(), cols, rows, env.as_ref()).map_err(|e| {
                CliError::new(Code::SpawnFailed, format!("cannot start `{shell}`: {e}"))
                    .hint("check the --shell command line")
            })?;
            Some(PaneRt {
                id: proto::new_object_id('p'),
                pty: Arc::new(pty),
                command: shell.clone(),
            })
        }
    };
    let (ev_tx, ev_rx) = mpsc::channel();
    let host = Arc::new(Host {
        state: Mutex::new(State {
            pane: pane.as_ref().map(|_| PaneState {
                ring: Ring::new(proto::HISTORY_BYTES),
                seq: 0,
                size: DEFAULT_SIZE,
                input_bytes: 0,
                ended: None,
            }),
            conns: HashMap::new(),
            pending: 0,
            next_conn: 1,
            stopping: false,
            stats: Stats::default(),
        }),
        pane,
        events: ev_tx.clone(),
        pid: win::current_pid(),
        sid,
        started_at: proto::rfc3339(SystemTime::now()),
        started: Instant::now(),
        endpoint: pipe::path(&opts.endpoint),
        job_breakaway: opts.job_breakaway,
        in_job: win::current_process_in_job(),
        job: win::current_job_limits(),
        session: win::session_id(),
        idle_exit: opts.idle_exit,
        environment: if env.is_some() { "user" } else { "inherited" },
    });

    let mut reader = host.pane.as_ref().map(|p| {
        let output = p
            .pty
            .take_output()
            .expect("a fresh pseudo console has its output end");
        let host = Arc::clone(&host);
        thread::spawn(move || host.read_output(&output))
    });
    if let Some(p) = &host.pane {
        let pty = Arc::clone(&p.pty);
        thread::spawn(move || {
            pty.wait_exit(None);
            let _ = ev_tx.send(Ev::PaneExited);
        });
    }
    let stopper = listener.stopper();
    let acceptor = thread::spawn({
        let host = Arc::clone(&host);
        move || {
            let mut backoff = Duration::from_millis(50);
            loop {
                match listener.accept() {
                    Ok(Some(p)) => {
                        backoff = Duration::from_millis(50);
                        host.lock().pending += 1;
                        let host = Arc::clone(&host);
                        thread::spawn(move || host.serve_conn(Arc::new(p)));
                    }
                    Ok(None) => return,
                    // Out of resources for a new instance: retry with a bounded backoff.
                    Err(_) => {
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(1));
                    }
                }
            }
        }
    });

    let exit = host.supervise(&ev_rx, &mut reader);
    let (requester, reason) = match exit {
        Exit::Stop { conn, id } => (Some((conn, id)), "stop_requested"),
        Exit::Idle => (None, "idle"),
    };
    let mut acks = host.notify("host", "host.stopping", &json!({ "reason": reason }), true);
    let was_live = host.lock().pane_live();
    let shell = host.end_pane(&mut reader);
    if was_live {
        acks.extend(host.pane_exited_events(true));
    }
    if let Some((conn, id)) = requester {
        let (atx, arx) = mpsc::channel();
        let st = host.lock();
        if let Some(c) = st.conns.get(&conn) {
            let result = json!({"stopping": true, "shell": shell});
            if c.sink.push(
                proto::encode_control(&proto::response(id, Ok(result))),
                Some(atx),
            ) {
                acks.push(arx);
            }
        }
    }
    let until = Instant::now() + Duration::from_secs(2);
    for a in acks {
        let _ = a.recv_timeout(until.saturating_duration_since(Instant::now()));
    }
    stopper.set();
    join_bounded(acceptor, Duration::from_secs(2));
    Ok(json!({
        "reason": reason,
        "host": { "pid": host.pid },
        "shell": shell,
    }))
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

fn flag(params: &Value, name: &str) -> Result<bool, IpcError> {
    match &params[name] {
        Value::Null => Ok(false),
        Value::Bool(b) => Ok(*b),
        _ => Err(IpcError::param(
            name,
            format!("{name} must be true or false"),
        )),
    }
}

fn size_param(v: &Value, field: &str) -> Result<Option<(u16, u16)>, IpcError> {
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
        _ => Err(IpcError::param(
            field,
            "size needs cols and rows between 2 and 1000",
        )),
    }
}

enum Reply {
    Now(Result<Value, IpcError>),
    Queued,
}

impl Host {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lifetime of ADR 0003 §4. Returns why the Host stops; `stopping` is set by then.
    fn supervise(&self, rx: &mpsc::Receiver<Ev>, reader: &mut Option<JoinHandle<()>>) -> Exit {
        loop {
            let idle = self.lock().idle();
            let ev = if idle {
                match rx.recv_timeout(self.idle_exit) {
                    Ok(ev) => ev,
                    Err(_) => {
                        let mut st = self.lock();
                        if st.idle() && !st.stopping {
                            st.stopping = true;
                            return Exit::Idle;
                        }
                        continue;
                    }
                }
            } else {
                match rx.recv() {
                    Ok(ev) => ev,
                    // Unreachable: the Host itself holds a sender.
                    Err(_) => return Exit::Idle,
                }
            };
            match ev {
                Ev::Changed => {}
                Ev::PaneExited => {
                    if self.lock().pane_live() {
                        self.end_pane(reader);
                        self.pane_exited_events(false);
                    }
                }
                Ev::Stop { conn, id } => return Exit::Stop { conn, id },
            }
        }
    }

    /// Ends the pane once (graceful, then forced) and returns its report; later calls return the
    /// same report. `ClosePseudoConsole` runs on a helper thread because on Windows 10 it blocks
    /// until conhost exits, while the reader keeps draining.
    fn end_pane(&self, reader: &mut Option<JoinHandle<()>>) -> Value {
        let Some(p) = &self.pane else {
            return Value::Null;
        };
        if let Some((_, report)) = self.lock().pane.as_ref().and_then(|s| s.ended.clone()) {
            return report;
        }
        let processes = p.pty.job_processes();
        let closer = thread::spawn({
            let pty = Arc::clone(&p.pty);
            move || pty.close_console()
        });
        let mut exit = p.pty.wait_exit(Some(GRACE));
        let forced = exit.is_none() || p.pty.job_processes() > 0;
        if forced {
            p.pty.terminate_job(1);
            exit = p.pty.wait_exit(Some(GRACE));
        }
        join_bounded(closer, GRACE);
        if let Some(r) = reader.take() {
            join_bounded(r, GRACE); // all pane output is queued before the exit event
        }
        let report = json!({
            "pane_id": p.id,
            "pid": p.pty.pid(),
            "exit_code": exit,
            "processes_in_job": processes,
            "forced": forced,
            "job_processes_left": p.pty.job_processes(),
        });
        let mut st = self.lock();
        if let Some(s) = st.pane.as_mut() {
            s.ended = Some((exit, report.clone()));
        }
        for c in st.conns.values_mut() {
            c.attach = None;
        }
        report
    }

    /// `pane.exited` to the subscribers of `panes`, once the pane ended.
    fn pane_exited_events(&self, with_ack: bool) -> Vec<mpsc::Receiver<()>> {
        let Some(p) = &self.pane else {
            return Vec::new();
        };
        let code = self
            .lock()
            .pane
            .as_ref()
            .and_then(|s| s.ended.as_ref().map(|e| e.0));
        let Some(code) = code else {
            return Vec::new();
        };
        self.notify(
            "panes",
            "pane.exited",
            &json!({"pane_id": p.id, "pid": p.pty.pid(), "exit_code": code}),
            with_ack,
        )
    }

    /// Sends an event to every client subscribed to `topic`.
    fn notify(
        &self,
        topic: &str,
        name: &str,
        data: &Value,
        with_ack: bool,
    ) -> Vec<mpsc::Receiver<()>> {
        let mut acks = Vec::new();
        let mut st = self.lock();
        for c in st.conns.values_mut() {
            if !c.topics.iter().any(|t| t == topic) {
                continue;
            }
            let (atx, arx) = mpsc::channel();
            if c.event(name, data, with_ack.then_some(atx)) && with_ack {
                acks.push(arx);
            }
        }
        acks
    }

    fn read_output(&self, out: &Handle) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match conpty::read_output(out, &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut guard = self.lock();
            let st = &mut *guard;
            let Some(pane) = st.pane.as_mut() else {
                return;
            };
            pane.ring.push(&buf[..n]);
            let seq = pane.seq;
            pane.seq += n as u64;
            let frame = proto::encode_data(STREAM, seq, 0, &buf[..n]);
            for c in st.conns.values_mut() {
                // Never block the pane on a client: one that lags is disconnected by its sink.
                if c.attach.is_some() && !c.sink.push(frame.clone(), None) {
                    c.attach = None;
                    st.stats.lagging_clients_dropped += 1;
                }
            }
        }
    }

    fn reject(&self, sink: &Sink, why: Reject, err: &IpcError) -> Option<u64> {
        {
            let mut st = self.lock();
            let s = &mut st.stats;
            match why {
                Reject::Identity => s.rejected_identity += 1,
                Reject::Limit => s.rejected_limit += 1,
                Reject::Handshake => s.rejected_handshake += 1,
                Reject::Stopping => s.rejected_stopping += 1,
            }
        }
        sink.last(&proto::connection_error(err));
        None
    }

    fn serve_conn(self: Arc<Self>, pipe: Arc<Pipe>) {
        let (tx, rx) = mpsc::sync_channel::<Out>(QUEUE_FRAMES);
        let queued = Arc::new(AtomicUsize::new(0));
        let sink = Sink {
            tx,
            queued: Arc::clone(&queued),
            pipe: Arc::clone(&pipe),
        };
        let writer = thread::spawn({
            let pipe = Arc::clone(&pipe);
            move || {
                for out in rx {
                    let ok = pipe.write_all_bytes(&out.bytes).is_ok();
                    queued.fetch_sub(out.bytes.len(), Ordering::SeqCst);
                    if !ok {
                        pipe.cancel();
                        return;
                    }
                    if let Some(a) = out.ack {
                        let _ = a.send(());
                    }
                }
            }
        });
        let hs = catch_unwind(AssertUnwindSafe(|| self.handshake(&pipe, &sink)));
        {
            let mut st = self.lock();
            st.pending = st.pending.saturating_sub(1);
            if hs.is_err() {
                st.stats.panics += 1;
            }
        }
        if let Ok(Some(id)) = hs {
            let r = catch_unwind(AssertUnwindSafe(|| self.requests(id, &pipe, &sink)));
            let mut st = self.lock();
            if r.is_err() {
                st.stats.panics += 1;
            }
            st.conns.remove(&id);
        }
        let _ = self.events.send(Ev::Changed);
        drop(sink);
        // Unblocks a writer stuck on a client that stopped reading.
        let until = Instant::now() + Duration::from_secs(2);
        while !writer.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        pipe.cancel();
        let _ = writer.join();
    }

    fn handshake(&self, pipe: &Arc<Pipe>, sink: &Sink) -> Option<u64> {
        let over_limit = {
            let mut st = self.lock();
            st.stats.connections_total += 1;
            st.conns.len() + st.pending > MAX_CONNECTIONS
        };
        if over_limit {
            return self.reject(
                sink,
                Reject::Limit,
                &IpcError::new(
                    ErrorCode::Busy,
                    format!("the Host serves at most {MAX_CONNECTIONS} connections"),
                ),
            );
        }
        let deadline = Instant::now() + proto::HANDSHAKE_TIMEOUT;
        let msg = match proto::read_frame(&mut &**pipe, Some(deadline), |_| deadline) {
            Ok(Some(Frame::Control(m))) => m,
            Ok(Some(Frame::Data { .. })) => {
                let e = IpcError::new(ErrorCode::ProtocolError, "expected hello, got a data frame");
                return self.reject(sink, Reject::Handshake, &e);
            }
            Ok(None) => return None,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                let e = IpcError::new(
                    ErrorCode::Timeout,
                    format!("no hello within {} s", proto::HANDSHAKE_TIMEOUT.as_secs()),
                );
                return self.reject(sink, Reject::Handshake, &e);
            }
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let e = IpcError::new(ErrorCode::ProtocolError, e.to_string());
                return self.reject(sink, Reject::Handshake, &e);
            }
            Err(_) => return None,
        };
        // Who is on the other end: the token of this very connection (ADR 0005, ADR 0019).
        if !pipe.client_user_sid().is_ok_and(|sid| sid == self.sid) {
            let e = IpcError::new(
                ErrorCode::Unauthorized,
                "the client does not run as the Host's user",
            );
            return self.reject(sink, Reject::Identity, &e);
        }
        let hello = match proto::parse_hello(&msg) {
            Ok(h) => h,
            Err(e) => return self.reject(sink, Reject::Handshake, &e),
        };
        let Some(protocol) = proto::negotiate(hello.min, hello.max) else {
            let e = proto::unsupported(hello.min, hello.max);
            return self.reject(sink, Reject::Handshake, &e);
        };
        let mut st = self.lock();
        if st.stopping {
            drop(st);
            let e = IpcError::new(ErrorCode::HostStopping, "the Host is shutting down");
            return self.reject(sink, Reject::Stopping, &e);
        }
        let id = st.next_conn;
        st.next_conn += 1;
        // Queued under the lock, so no event or output can precede the welcome.
        if !sink.control(&proto::welcome(protocol, self.pid, &self.started_at)) {
            return None;
        }
        st.conns.insert(
            id,
            Conn {
                sink: sink.clone(),
                pid: pipe.client_pid(),
                kind: hello.kind,
                name: hello.name,
                version: hello.version,
                attach: None,
                topics: Vec::new(),
                event_seq: 0,
            },
        );
        Some(id)
    }

    fn violation(&self, sink: &Sink, message: &str) {
        self.lock().stats.protocol_errors += 1;
        sink.last(&proto::connection_error(&IpcError::new(
            ErrorCode::ProtocolError,
            message,
        )));
    }

    fn answer(&self, sink: &Sink, id: u64, result: Result<Value, IpcError>) {
        if let Err(e) = &result {
            *self.lock().stats.errors.entry(e.code.as_str()).or_default() += 1;
        }
        sink.control(&proto::response(id, result));
    }

    fn requests(&self, id: u64, pipe: &Arc<Pipe>, sink: &Sink) {
        loop {
            let frame = match proto::read_frame(&mut &**pipe, None, |t| t + proto::FRAME_TIMEOUT) {
                Ok(Some(f)) => f,
                Ok(None) => return,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                    return self.violation(sink, &e.to_string());
                }
                Err(_) => return,
            };
            match frame {
                Frame::Data {
                    stream,
                    flags,
                    bytes,
                    ..
                } => {
                    if flags != FLAG_INPUT {
                        return self.violation(sink, "clients send only input data frames");
                    }
                    self.input(id, stream, &bytes);
                }
                Frame::Control(m) => {
                    let req = match proto::parse_request(&m) {
                        Ok(r) => r,
                        Err(Refusal::Fatal(msg)) => return self.violation(sink, &msg),
                        Err(Refusal::Reply(rid, e)) => {
                            self.answer(sink, rid, Err(e));
                            continue;
                        }
                    };
                    let kind = {
                        let mut st = self.lock();
                        st.stats.requests_total += 1;
                        st.conns.get(&id).map(|c| c.kind)
                    };
                    let Some(kind) = kind else { return };
                    let reply = match proto::authorize(kind, &req.method) {
                        Err(e) => Reply::Now(Err(e)),
                        Ok(_) => catch_unwind(AssertUnwindSafe(|| self.handle(id, &req)))
                            .unwrap_or_else(|_| {
                                self.lock().stats.panics += 1;
                                Reply::Now(Err(IpcError::new(
                                    ErrorCode::InternalError,
                                    "the request failed inside the Host; the Host continues",
                                )))
                            }),
                    };
                    if let Reply::Now(r) = reply {
                        self.answer(sink, req.id, r);
                    }
                }
            }
        }
    }

    fn input(&self, conn: u64, stream: u32, bytes: &[u8]) {
        let allowed = {
            let mut st = self.lock();
            let ok = !st.stopping
                && st.pane_live()
                && stream == STREAM
                && st.conns.get(&conn).and_then(|c| c.attach) == Some(true);
            if ok {
                if let Some(p) = st.pane.as_mut() {
                    p.input_bytes += bytes.len() as u64;
                }
            } else {
                st.stats.input_frames_dropped += 1;
            }
            ok
        };
        // Only an interactively attached cli/gui connection reaches the pane; nothing else does.
        if allowed && let Some(p) = &self.pane {
            let _ = p.pty.write_input(bytes);
        }
    }

    fn handle(&self, conn: u64, req: &Request) -> Reply {
        let method = req.method.as_str();
        if self.lock().stopping && !matches!(method, "host.status" | "host.health") {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::HostStopping,
                "the Host is shutting down",
            )));
        }
        let p = &req.params;
        match method {
            "host.health" => Reply::Now(Ok(self.health())),
            "host.status" => Reply::Now(Ok(self.status())),
            "host.stats" => Reply::Now(Ok(self.stats())),
            "host.stop" => self.stop(conn, req),
            "events.subscribe" => Reply::Now(self.subscribe(conn, p)),
            "pane.attach" => self.attach(conn, req),
            "pane.detach" => Reply::Now(self.detach(conn, p)),
            "pane.resize" => Reply::Now(self.resize(conn, p)),
            other => Reply::Now(Err(IpcError::new(
                ErrorCode::UnknownRequest,
                format!("unknown method `{other}`"),
            ))),
        }
    }

    fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    fn health(&self) -> Value {
        let st = self.lock();
        json!({
            "healthy": !st.stopping,
            "pid": self.pid,
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": proto::PROTOCOL_MAX,
            "uptime_s": self.uptime_s(),
            "stopping": st.stopping,
            "terminals": u8::from(st.pane_live()),
            "clients": st.conns.len(),
        })
    }

    fn status(&self) -> Value {
        let st = self.lock();
        let attached = st.conns.values().filter(|c| c.attach.is_some()).count();
        let clients: Vec<Value> = st
            .conns
            .values()
            .map(|c| {
                json!({
                    "pid": c.pid, "kind": c.kind.as_str(), "name": c.name, "version": c.version,
                    "attached": c.attach.is_some(), "interactive": c.attach == Some(true),
                    "topics": c.topics,
                })
            })
            .collect();
        let pane = match (&self.pane, &st.pane) {
            (Some(p), Some(s)) => json!({
                "id": p.id,
                "pid": p.pty.pid(),
                "command": p.command,
                "running": s.ended.is_none() && win::exit_code(p.pty.process_handle()).is_none(),
                "exit_code": s.ended.as_ref().and_then(|e| e.0),
                "size": {"cols": s.size.0, "rows": s.size.1},
                "job_processes": p.pty.job_processes(),
                "output_bytes": s.seq,
            }),
            _ => Value::Null,
        };
        json!({
            "protocol": proto::PROTOCOL_MAX,
            "endpoint": self.endpoint,
            "host": {
                "pid": self.pid,
                "version": env!("CARGO_PKG_VERSION"),
                "started_at": self.started_at,
                "uptime_s": self.uptime_s(),
                "session_id": self.session,
                "job_breakaway": self.job_breakaway,
                "in_job": self.in_job,
                "job": self.job.map(|j| json!({
                    "kill_on_close": j.kill_on_close,
                    "breakaway_ok": j.breakaway_ok,
                    "silent_breakaway_ok": j.silent_breakaway_ok,
                })),
                "stopping": st.stopping,
                "idle_exit_s": self.idle_exit.as_secs(),
                "environment": self.environment,
            },
            "terminals": u8::from(st.pane_live()),
            "pane": pane,
            "clients": {"connected": st.conns.len(), "attached": attached, "list": clients},
        })
    }

    /// Local metrics (ADR 0012): numbers and enumerations only.
    fn stats(&self) -> Value {
        let st = self.lock();
        let s = &st.stats;
        let (out, input) = st.pane.as_ref().map_or((0, 0), |p| (p.seq, p.input_bytes));
        json!({
            "host": {"uptime_s": self.uptime_s(), "handle_count": win::handle_count()},
            "terminals": u8::from(st.pane_live()),
            "panes": u8::from(st.pane_live()),
            "clients": st.conns.len(),
            "pane": {"output_bytes_total": out, "input_bytes_total": input},
            "ipc": {
                "connections_total": s.connections_total,
                "rejected": {
                    "identity": s.rejected_identity,
                    "limit": s.rejected_limit,
                    "handshake": s.rejected_handshake,
                    "stopping": s.rejected_stopping,
                },
                "requests_total": s.requests_total,
                "errors_total": s.errors,
                "protocol_errors": s.protocol_errors,
                "panics": s.panics,
                "input_frames_dropped": s.input_frames_dropped,
                "lagging_clients_dropped": s.lagging_clients_dropped,
            },
        })
    }

    fn live_panes(&self) -> Vec<String> {
        match &self.pane {
            Some(p) if self.lock().pane_live() => vec![p.id.clone()],
            _ => Vec::new(),
        }
    }

    fn stop(&self, conn: u64, req: &Request) -> Reply {
        let (dry_run, kill) = match (
            flag(&req.params, "dry_run"),
            flag(&req.params, "kill_terminals"),
        ) {
            (Ok(d), Ok(k)) => (d, k),
            (Err(e), _) | (_, Err(e)) => return Reply::Now(Err(e)),
        };
        let live = self.live_panes();
        if dry_run {
            return Reply::Now(Ok(json!({
                "dry_run": true, "would_kill": live, "would_stop": self.status(),
            })));
        }
        if !live.is_empty() && !kill {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::Busy,
                "the Host owns live Terminals; set kill_terminals to end them",
            )
            .details(json!({ "live": live }))));
        }
        {
            let mut st = self.lock();
            if st.stopping {
                return Reply::Now(Err(IpcError::new(
                    ErrorCode::HostStopping,
                    "the Host is already stopping",
                )));
            }
            st.stopping = true;
        }
        let _ = self.events.send(Ev::Stop { conn, id: req.id });
        Reply::Queued // answered when the panes have ended
    }

    fn subscribe(&self, conn: u64, params: &Value) -> Result<Value, IpcError> {
        let bad = || {
            IpcError::param(
                "topics",
                "topics must be a list of: terminals, panes, claude, host",
            )
        };
        let topics: Vec<String> = params["topics"]
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|t| {
                t.as_str()
                    .filter(|t| proto::TOPICS.contains(t))
                    .map(String::from)
            })
            .collect::<Option<_>>()
            .ok_or_else(bad)?;
        if let Some(c) = self.lock().conns.get_mut(&conn) {
            c.topics.clone_from(&topics);
        }
        Ok(json!({ "topics": topics }))
    }

    fn attach(&self, conn: u64, req: &Request) -> Reply {
        let p = &req.params;
        let Some(pane) = &self.pane else {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::TargetNotFound,
                "this Host owns no pane (it was started with --no-shell)",
            )));
        };
        if !(p["target"].is_null() || p["target"] == pane.id.as_str()) {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::TargetNotFound,
                "no pane matches the target",
            )
            .details(json!({"candidates": [pane.id]}))));
        }
        let interactive = match &p["interactive"] {
            Value::Null => true,
            Value::Bool(b) => *b,
            _ => {
                return Reply::Now(Err(IpcError::param(
                    "interactive",
                    "interactive must be true or false",
                )));
            }
        };
        let size = match size_param(&p["size"], "size") {
            Ok(s) if interactive => s,
            Ok(_) => None, // a read-only viewer never resizes the pane
            Err(e) => return Reply::Now(Err(e)),
        };
        if !self.lock().pane_live() {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::InvalidState,
                "the pane's process has exited",
            )));
        }
        if let Some((c, r)) = size
            && let Err(e) = pane.pty.resize(c, r)
        {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::InternalError,
                format!("resize failed: {e}"),
            )));
        }
        // Response, replay and the start of live output are queued under one lock, so the
        // reader cannot interleave: no gap, no duplicate.
        let mut guard = self.lock();
        let st = &mut *guard;
        let Some(ps) = st.pane.as_mut() else {
            return Reply::Queued;
        };
        if let Some(s) = size {
            ps.size = s;
        }
        let (seq, replay, cur) = (ps.seq, ps.ring.replay(), ps.size);
        let Some(c) = st.conns.get_mut(&conn) else {
            return Reply::Queued;
        };
        c.attach = Some(interactive);
        let result = json!({
            "stream_id": STREAM,
            "pane_id": pane.id,
            "seq": seq,
            "interactive": interactive,
            "pane": {"id": pane.id, "pid": pane.pty.pid(), "size": {"cols": cur.0, "rows": cur.1}},
            "replay_bytes": replay.len(),
        });
        c.sink.control(&proto::response(req.id, Ok(result)));
        if !replay.is_empty() {
            c.sink.push(
                proto::encode_data(STREAM, seq - replay.len() as u64, FLAG_SNAPSHOT, &replay),
                None,
            );
        }
        Reply::Queued
    }

    fn detach(&self, conn: u64, params: &Value) -> Result<Value, IpcError> {
        match &params["stream_id"] {
            Value::Null => {}
            v if v.as_u64() == Some(u64::from(STREAM)) => {}
            _ => return Err(IpcError::param("stream_id", "no such stream")),
        }
        if let Some(c) = self.lock().conns.get_mut(&conn) {
            c.attach = None;
        }
        Ok(json!({}))
    }

    fn resize(&self, conn: u64, params: &Value) -> Result<Value, IpcError> {
        let (c, r) = size_param(params, "cols")?
            .ok_or_else(|| IpcError::param("cols", "cols and rows are required"))?;
        let Some(pane) = &self.pane else {
            return Err(IpcError::new(
                ErrorCode::TargetNotFound,
                "this Host owns no pane",
            ));
        };
        if self.lock().conns.get(&conn).and_then(|c| c.attach) != Some(true) {
            return Err(IpcError::new(
                ErrorCode::InvalidState,
                "attach interactively before resizing the pane",
            ));
        }
        pane.pty
            .resize(c, r)
            .map_err(|e| IpcError::new(ErrorCode::InternalError, format!("resize failed: {e}")))?;
        if let Some(ps) = self.lock().pane.as_mut() {
            ps.size = (c, r);
        }
        Ok(json!({"cols": c, "rows": r}))
    }
}
