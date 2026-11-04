//! `subak host serve` — the Host supervisor and session manager (ADR 0003, ADR 0005, ADR 0016,
//! ADR 0019, ADR 0020; docs/SESSION_MODEL.md).
//!
//! The Host serves IPC v1 ([`crate::proto`]) on the per-user pipe. Every connection must pass,
//! in this order: the pipe's owner-only DACL (kernel), a `hello` within 5 s, the user SID of the
//! connection's own token equal to the Host's, a valid `hello`, a common protocol major. After
//! `welcome` it may send requests; what it may call depends on its client kind
//! ([`proto::authorize`]). A process is only ever started by `terminal.create` from a `cli` or
//! `gui` client (or `--shell` at Host start), and only typed into by an interactively attached
//! `cli` or `gui` connection.
//!
//! **Session manager.** The Host owns any number of Terminals ([`crate::session`]): each has one
//! internal Window with one Pane, and each Pane its own pseudo console (ConPTY) and its own
//! kill-on-close job, so every Terminal's process tree is isolated. The model and the state
//! machine live in `session.rs`; this file owns the OS side (pseudo consoles, jobs, threads,
//! output rings) and applies every state change under one lock, emitting its event in the same
//! step, so a client that subscribes and then lists never misses or double-counts a change.
//!
//! Ending a pane is claimed once, under the lock: whoever claims it (a stop, a kill, a Host stop,
//! or the pane's own exit waiter when its shell exits by itself) performs the teardown; anyone
//! who arrives later waits for that result, and a kill arriving during a graceful stop escalates
//! it. Client connections own nothing: a client that detaches, disconnects or crashes only loses
//! its subscriptions and attachments.
//!
//! Lifetime (ADR 0003 §4): the Host runs while it owns a live Terminal or has a client. When both
//! are gone it waits the idle grace period (`--idle-exit`, default 10 s) and exits. It never
//! polls: idle, every thread is blocked in the kernel, and the only timed wait is that grace.
//! Until Step 29 persists them, stopped Terminal records live only as long as the Host.
//!
//! Threads: an accept loop, a handler and a writer per connection, an output reader and an exit
//! waiter per pane, and the main thread that supervises lifetime and teardown. A panic while
//! handling one request fails that request with `internal_error`; the Host continues.

use crate::output::{CliError, Code};
use crate::proto::{
    self, ClientKind, ErrorCode, FLAG_INPUT, FLAG_SNAPSHOT, Frame, IpcError, Refusal, Request, Ring,
};
use crate::pty::{self, EnvOverrides, Pty};
use crate::session::{self, Command, EndCause, LastExit, Registry, Spec, Transition};
use crate::win::pipe::{self, Listener, Pipe};
use crate::win::{self, EnvBlock, JobLimits};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Initial pseudo console size until a client attaches with its own.
pub const DEFAULT_SIZE: (u16, u16) = (120, 30);
/// Idle grace period of ADR 0003 §4.
pub const DEFAULT_IDLE_EXIT_S: u64 = 10;
/// Most connections (in handshake or established) served at once; more get `busy`.
pub const MAX_CONNECTIONS: usize = 64;
/// Most Terminals (live and stored) one Host owns; more get `busy`.
pub const MAX_TERMINALS: usize = 256;
/// Bytes queued per connection before a slow client is dropped; each queued frame also counts
/// [`FRAME_COST`], so many tiny frames (ConPTY's output arrives in chunks of ~80 bytes on
/// Windows 10) are bounded by memory, not by a frame count (ADR 0025).
const QUEUE_BYTES: usize = 16 * 1024 * 1024;
const FRAME_COST: usize = 64;
/// Most bytes the writer of one connection takes off its queue for one write.
const WRITE_BATCH: usize = 256 * 1024;
/// How long a graceful stop waits for the processes after `ClosePseudoConsole`.
const GRACE: Duration = Duration::from_secs(5);
/// How long a second party waits for a teardown someone else performs.
const TEARDOWN_WAIT: Duration = Duration::from_secs(30);
/// Longest command line `CreateProcessW` accepts.
const MAX_COMMAND_LINE: usize = win::launch::MAX_COMMAND_LINE;

/// What `host serve` needs.
#[derive(Debug)]
pub struct ServeOpts {
    pub endpoint: String,
    /// The command line of a first Terminal started with the Host; `None` starts a Host that
    /// owns no Terminal (what start on demand uses).
    pub shell: Option<String>,
    pub job_breakaway: Option<bool>,
    pub idle_exit: Duration,
}

/// The shell started when no command is given.
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

/// One queued frame. Output frames are shared by every client attached to the pane (`Arc`), so
/// several lagging clients hold one copy of the same output, not one each.
struct Out {
    bytes: Arc<[u8]>,
    ack: Option<mpsc::Sender<()>>,
}

#[derive(Default)]
struct Pending {
    items: std::collections::VecDeque<Out>,
    /// Bytes plus [`FRAME_COST`] per item.
    cost: usize,
    closed: bool,
}

/// The queue between a connection's senders and its writer thread.
#[derive(Default)]
struct Queue {
    pending: Mutex<Pending>,
    ready: Condvar,
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The writer's side: the next batch (several small frames are written at once), or `None`
    /// once the queue is closed and drained.
    fn take(&self) -> Option<Vec<Out>> {
        let mut q = self.lock();
        while q.items.is_empty() && !q.closed {
            q = self
                .ready
                .wait(q)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let mut batch = Vec::new();
        let mut size = 0;
        while let Some(o) = q.items.front() {
            if !batch.is_empty() && size + o.bytes.len() > WRITE_BATCH {
                break;
            }
            let o = q.items.pop_front().expect("front exists");
            size += o.bytes.len();
            q.cost -= o.bytes.len() + FRAME_COST;
            let ack = o.ack.is_some();
            batch.push(o);
            if ack {
                break; // a barrier is acknowledged as soon as everything before it is written
            }
        }
        (!batch.is_empty()).then_some(batch)
    }

    /// No more frames: the writer drains what is queued and ends.
    fn close(&self) {
        self.lock().closed = true;
        self.ready.notify_all();
    }
}

/// The sending side of one connection: a queue bounded in bytes to its writer thread. A client
/// that falls that far behind is disconnected; it never blocks the Host.
#[derive(Clone)]
struct Sink {
    queue: Arc<Queue>,
    pipe: Arc<Pipe>,
}

impl Sink {
    fn push(&self, bytes: Arc<[u8]>, ack: Option<mpsc::Sender<()>>) -> bool {
        let cost = bytes.len() + FRAME_COST;
        {
            let mut q = self.queue.lock();
            if !q.closed && q.cost + cost <= QUEUE_BYTES {
                q.cost += cost;
                q.items.push_back(Out { bytes, ack });
                drop(q);
                self.queue.ready.notify_one();
                return true;
            }
        }
        self.pipe.cancel(); // its handler sees the error and removes the connection
        false
    }

    fn control(&self, msg: &Value) -> bool {
        self.push(proto::encode_control(msg).into(), None)
    }

    /// Queues a last message and waits (bounded) until it was written.
    fn last(&self, msg: &Value) {
        let (atx, arx) = mpsc::channel();
        if self.push(proto::encode_control(msg).into(), Some(atx)) {
            let _ = arx.recv_timeout(Duration::from_secs(1));
        }
    }

    /// A barrier: signals once everything queued before it was written.
    fn flushed(&self) -> Option<mpsc::Receiver<()>> {
        let (atx, arx) = mpsc::channel();
        self.push(Arc::from(&[][..]), Some(atx)).then_some(arx)
    }
}

struct Conn {
    sink: Sink,
    pid: Option<u32>,
    kind: ClientKind,
    name: String,
    version: String,
    /// Streams this connection is attached to, and whether interactively.
    attached: BTreeMap<u32, bool>,
    topics: Vec<String>,
    event_seq: u64,
}

impl Conn {
    fn event(&mut self, name: &str, data: &Value) -> bool {
        self.event_seq += 1;
        self.sink.push(
            proto::encode_control(&proto::event(self.event_seq, name, data)).into(),
            None,
        )
    }
}

/// Recent output and counters of one running pane.
struct PaneOut {
    ring: Ring,
    seq: u64,
    input_bytes: u64,
}

/// The OS side of one running pane: its pseudo console and job, its reader thread, and the
/// result of its teardown once someone performed it.
struct PaneRt {
    pane_id: String,
    terminal_id: String,
    stream: u32,
    pty: Arc<dyn Pty>,
    reader: Mutex<Option<JoinHandle<()>>>,
    /// The job was terminated (not every process ended by itself).
    forced: AtomicBool,
    done: Mutex<Option<Value>>,
    done_cv: Condvar,
}

impl PaneRt {
    fn terminate(&self) {
        self.forced.store(true, Ordering::SeqCst);
        self.pty.kill_tree(1);
    }

    fn set_done(&self, report: Value) {
        *self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(report);
        self.done_cv.notify_all();
    }

    /// The teardown report of whoever claimed it, waiting (bounded) until it is there.
    fn wait_done(&self) -> Value {
        let guard = self
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, _) = self
            .done_cv
            .wait_timeout_while(guard, TEARDOWN_WAIT, |d| d.is_none())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone().unwrap_or_else(
            || json!({"pane_id": self.pane_id, "terminal_id": self.terminal_id, "pending": true}),
        )
    }
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
    terminals_created: u64,
    spawn_failures: u64,
    terminals_removed: u64,
    /// Output and input bytes of panes that already ended.
    ended_output_bytes: u64,
    ended_input_bytes: u64,
}

#[derive(Clone, Copy)]
enum Reject {
    Identity,
    Limit,
    Handshake,
    Stopping,
}

struct State {
    reg: Registry,
    out: HashMap<String, PaneOut>,
    rt: HashMap<String, Arc<PaneRt>>,
    conns: HashMap<u64, Conn>,
    /// Accepted connections that have not finished their handshake.
    pending: usize,
    next_conn: u64,
    stopping: bool,
    stats: Stats,
    /// The Terminal started with `--shell` (its pane is `host.status.pane`).
    initial: Option<String>,
}

impl State {
    fn live_terminals(&self) -> usize {
        self.reg
            .terminals
            .iter()
            .filter(|t| t.state.is_live())
            .count()
    }

    fn idle(&self) -> bool {
        self.live_terminals() == 0 && self.conns.is_empty() && self.pending == 0
    }

    fn live(&self, p: &session::Pane) -> session::Live {
        let rt = self.rt.get(&p.id);
        session::Live {
            running: rt.is_some_and(|r| r.pty.running()),
            processes: rt.map_or(0, |r| r.pty.processes()),
            clients: self
                .conns
                .values()
                .filter(|c| c.attached.contains_key(&p.stream))
                .count(),
            output_bytes: self.out.get(&p.id).map_or(0, |o| o.seq),
        }
    }

    fn terminal_json(&self, t: &session::Terminal) -> Value {
        session::terminal_json(t, &|p| self.live(p))
    }

    fn summary_json(&self, t: &session::Terminal) -> Value {
        session::summary_json(t, &|p| self.live(p))
    }

    fn rt_of_stream(&self, stream: u32) -> Option<&Arc<PaneRt>> {
        self.rt.values().find(|r| r.stream == stream)
    }

    /// Sends an event to every connection subscribed to `topic`, in the same critical section
    /// as the change it reports.
    fn notify(&mut self, topic: &str, name: &str, data: &Value) {
        for c in self.conns.values_mut() {
            if c.topics.iter().any(|t| t == topic) {
                c.event(name, data);
            }
        }
    }

    fn state_changed(&mut self, terminal_id: &str, previous: &str, cause: Option<EndCause>) {
        let Some(t) = self.reg.get(terminal_id) else {
            return;
        };
        let data = json!({
            "terminal_id": t.id, "name": t.name.display, "state": t.state.as_str(),
            "previous_state": previous, "cause": cause.map(EndCause::as_str),
        });
        self.notify("terminals", "terminal.state_changed", &data);
    }

    fn removed(&mut self, terminal_id: &str, name: &str, reason: &str) {
        self.stats.terminals_removed += 1;
        let data = json!({"terminal_id": terminal_id, "name": name, "reason": reason});
        self.notify("terminals", "terminal.removed", &data);
    }
}

enum Ev {
    Stop { conn: u64, id: u64 },
    Changed,
}

enum Exit {
    Stop { conn: u64, id: u64 },
    Idle,
}

struct Host {
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
    /// Whether the logon environment could be built at start (`user`) or not (`inherited`).
    /// Each new pane builds its own, fresh (`win::launch`, ADR 0025).
    environment: &'static str,
    /// The default working directory of new panes: the user profile.
    home: Option<PathBuf>,
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
    let home = profile_dir(env.as_ref());
    let (ev_tx, ev_rx) = mpsc::channel();
    let host = Arc::new(Host {
        state: Mutex::new(State {
            reg: Registry::default(),
            out: HashMap::new(),
            rt: HashMap::new(),
            conns: HashMap::new(),
            pending: 0,
            next_conn: 1,
            stopping: false,
            stats: Stats::default(),
            initial: None,
        }),
        events: ev_tx,
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
        home,
    });
    if let Some(shell) = &opts.shell {
        let spec = Spec {
            name: None,
            command: session::split_command_line(shell),
            cwd: host.home.as_ref().map(|p| p.display().to_string()),
            env: pty::EnvNames::default(),
            size: DEFAULT_SIZE,
            remain_on_exit: false,
        };
        let launch = Launch {
            raw_line: Some(shell.clone()),
            env: EnvOverrides::default(),
        };
        let created = host.create(spec, &launch, false).map_err(|e| {
            CliError::new(
                Code::SpawnFailed,
                format!("cannot start `{shell}`: {}", e.message),
            )
            .hint("check the --shell command line")
        })?;
        host.lock().initial = Some(created["terminal_id"].as_str().unwrap_or("").to_string());
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
                        thread::spawn(move || host.serve_conn(&Arc::new(p)));
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

    let exit = host.supervise(&ev_rx);
    let (requester, reason) = match exit {
        Exit::Stop { conn, id } => (Some((conn, id)), "stop_requested"),
        Exit::Idle => (None, "idle"),
    };
    host.lock()
        .notify("host", "host.stopping", &json!({ "reason": reason }));
    let initial = host.lock().initial.clone();
    let ended = host.end_all();
    let shell = initial
        .and_then(|tid| {
            ended
                .iter()
                .find(|r| r["terminal_id"] == tid.as_str())
                .cloned()
        })
        .unwrap_or(Value::Null);
    let mut acks = Vec::new();
    {
        let st = host.lock();
        if let Some((conn, id)) = requester
            && let Some(c) = st.conns.get(&conn)
        {
            let result = json!({"stopping": true, "shell": shell, "terminals": ended});
            c.sink.control(&proto::response(id, Ok(result)));
        }
        // Every event and the response reach the clients before the Host exits.
        acks.extend(st.conns.values().filter_map(|c| c.sink.flushed()));
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
        "terminals": ended,
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

/// The required `target` string.
fn target(params: &Value) -> Result<&str, IpcError> {
    params["target"]
        .as_str()
        .ok_or_else(|| IpcError::param("target", "target must be a Terminal name or id"))
}

fn optional_string<'a>(params: &'a Value, name: &str) -> Result<Option<&'a str>, IpcError> {
    match &params[name] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.as_str())),
        _ => Err(IpcError::param(name, format!("{name} must be a string"))),
    }
}

/// How a new pane's program is started, beyond what the model keeps: the verbatim `--shell`
/// line, and the environment overrides with their values (used once, never stored).
#[derive(Debug, Default)]
struct Launch {
    raw_line: Option<String>,
    env: EnvOverrides,
}

/// The `spawn_failed` error of a failed start (IPC_PROTOCOL §6): `details.reason` and, when
/// there is one, `details.hint` and `details.os_error`.
fn spawn_failed(e: &pty::SpawnError, program: &str, keep: bool, tid: Option<&str>) -> IpcError {
    let mut details = json!({
        "reason": e.reason.as_str(),
        "program": program,
        "kept": keep,
        "terminal_id": tid,
    });
    if let Some(h) = &e.hint {
        details["hint"] = json!(h);
    }
    if let Some(c) = e.os_error {
        details["os_error"] = json!(c);
    }
    let code = if e.reason == pty::Reason::UnsupportedPlatform {
        ErrorCode::UnsupportedPlatform
    } else {
        ErrorCode::SpawnFailed
    };
    IpcError::new(code, e.message.clone()).details(details)
}

enum Reply {
    Now(Result<Value, IpcError>),
    Queued,
}

/// Ends a freshly started pane that must not be kept (the Host began stopping meanwhile).
fn discard(pty: &Arc<dyn Pty>, mut output: Box<dyn Read + Send>) {
    pty.kill_tree(1);
    let drain = thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        while matches!(output.read(&mut buf), Ok(n) if n > 0) {}
    });
    let closer = thread::spawn({
        let pty = Arc::clone(pty);
        move || pty.close()
    });
    pty.wait_exit(Some(GRACE));
    join_bounded(closer, GRACE);
    join_bounded(drain, GRACE);
}

impl Host {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Lifetime of ADR 0003 §4. Returns why the Host stops; `stopping` is set by then.
    fn supervise(&self, rx: &mpsc::Receiver<Ev>) -> Exit {
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
                Ev::Stop { conn, id } => return Exit::Stop { conn, id },
            }
        }
    }

    /// Creates a Terminal (`terminal.create`, or the `--shell` Terminal of `host serve`): the
    /// name and ids are reserved under the lock (so concurrent creates with one name give one
    /// Terminal), the pane is started outside it, and a failed start leaves nothing behind
    /// unless `keep` asks for a `failed` record. Returns the `terminal.create` result.
    fn create(
        self: &Arc<Self>,
        spec: Spec,
        launch: &Launch,
        keep: bool,
    ) -> Result<Value, IpcError> {
        let (tid, pane_id, stream, size, cwd, command) = {
            let mut st = self.lock();
            if st.stopping {
                return Err(IpcError::new(
                    ErrorCode::HostStopping,
                    "the Host is shutting down",
                ));
            }
            if st.reg.terminals.len() >= MAX_TERMINALS {
                return Err(IpcError::new(
                    ErrorCode::Busy,
                    format!("this Host already owns {MAX_TERMINALS} Terminals"),
                ));
            }
            let created_at = proto::rfc3339(SystemTime::now());
            let i = st.reg.reserve(spec, created_at, proto::new_object_id)?;
            st.stats.terminals_created += 1;
            let t = &st.reg.terminals[i];
            let p = t.active_pane();
            let out = (
                t.id.clone(),
                p.id.clone(),
                p.stream,
                p.size,
                p.cwd.clone(),
                p.command.clone(),
            );
            let data = json!({"terminal": st.terminal_json(t)});
            st.notify("terminals", "terminal.created", &data);
            out
        };
        let program = command.program.clone();
        let cwd = cwd
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let spawned = pty::spawn(&pty::Spec {
            program: &command.program,
            args: &command.args,
            raw_line: launch.raw_line.as_deref(),
            cwd: &cwd,
            env: &launch.env,
            // ADR 0010 §1: how a program (and Claude Code's hooks) knows which pane it runs in.
            host_vars: &[("SUBAK_PANE_ID", &pane_id), ("SUBAK_TERMINAL_ID", &tid)],
            size,
        });
        let started = match spawned {
            Ok(s) => s,
            Err(e) => {
                let mut st = self.lock();
                st.stats.spawn_failures += 1;
                let name = st
                    .reg
                    .get(&tid)
                    .map(|t| t.name.display.clone())
                    .unwrap_or_default();
                let _ = st.reg.apply(&tid, Transition::SpawnFailed { keep });
                if keep {
                    if let Some(t) = st.reg.get_mut(&tid) {
                        t.failure = Some(session::Failure {
                            reason: e.reason.as_str(),
                            message: e.message.clone(),
                        });
                    }
                    st.state_changed(&tid, "starting", None);
                } else {
                    st.removed(&tid, &name, "spawn_failed");
                }
                return Err(spawn_failed(&e, &program, keep, keep.then_some(&*tid)));
            }
        };
        let pty = started.pty;
        let output = pty
            .take_output()
            .expect("a fresh pseudo console has its output end");
        let rt = Arc::new(PaneRt {
            pane_id: pane_id.clone(),
            terminal_id: tid.clone(),
            stream,
            pty: Arc::clone(&pty),
            reader: Mutex::new(None),
            forced: AtomicBool::new(false),
            done: Mutex::new(None),
            done_cv: Condvar::new(),
        });
        let result = {
            let mut st = self.lock();
            if st.stopping || st.reg.get(&tid).is_none() {
                drop(st);
                discard(&pty, output);
                let mut st = self.lock();
                if let Some(i) = st.reg.index_of(&tid) {
                    let t = st.reg.terminals.remove(i);
                    st.removed(&tid, &t.name.display, "host_stopping");
                }
                return Err(IpcError::new(
                    ErrorCode::HostStopping,
                    "the Host began shutting down while the Terminal started",
                ));
            }
            st.reg.apply(&tid, Transition::Spawned)?;
            if let Some(t) = st.reg.get_mut(&tid) {
                let p = t.active_pane_mut();
                p.pid = Some(pty.pid());
                p.executable = Some(started.executable.display().to_string());
            }
            st.out.insert(
                pane_id.clone(),
                PaneOut {
                    ring: Ring::new(proto::HISTORY_BYTES),
                    seq: 0,
                    input_bytes: 0,
                },
            );
            st.rt.insert(pane_id.clone(), Arc::clone(&rt));
            st.state_changed(&tid, "starting", None);
            let t = st.reg.get(&tid).expect("just registered");
            json!({
                "terminal_id": t.id,
                "window_id": t.windows[0].id,
                "pane_id": pane_id,
                "name": t.name.display,
                "terminal": st.terminal_json(t),
            })
        };
        // The reader is registered before the exit waiter can tear the pane down.
        let reader = thread::spawn({
            let host = Arc::clone(self);
            let rt = Arc::clone(&rt);
            let mut output = output;
            move || host.read_output(&rt, &mut *output)
        });
        *rt.reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reader);
        thread::spawn({
            let host = Arc::clone(self);
            move || {
                rt.pty.wait_exit(None);
                host.end_pane(&rt.pane_id, EndCause::Exited, false);
            }
        });
        let _ = self.events.send(Ev::Changed);
        Ok(result)
    }

    /// Ends one pane once and returns its report. The first caller claims the teardown and
    /// performs it; later callers wait for its result (a later, stronger cause still decides the
    /// Terminal's fate, and `immediate` ends the job at once).
    fn end_pane(&self, pane_id: &str, cause: EndCause, immediate: bool) -> Value {
        let rt = {
            let mut st = self.lock();
            let Some(rt) = st.rt.get(pane_id).cloned() else {
                return Value::Null;
            };
            let Some(t) = st.reg.get_mut(&rt.terminal_id) else {
                return Value::Null;
            };
            if let Some(prev) = t.ending {
                if cause > prev {
                    t.ending = Some(cause);
                }
                drop(st);
                if immediate {
                    rt.terminate();
                }
                return rt.wait_done();
            }
            t.ending = Some(cause);
            let tid = rt.terminal_id.clone();
            if st.reg.apply(&tid, Transition::EndBegins).is_ok() {
                st.state_changed(&tid, "running", Some(cause));
            }
            rt
        };
        let report = self.teardown(&rt, immediate);
        self.finish(&rt, &report);
        rt.set_done(report.clone());
        report
    }

    /// Graceful, then forced (ADR 0004, ARCHITECTURE §5): `ClosePseudoConsole` sends
    /// `CTRL_CLOSE_EVENT`, the processes get the grace period, and whatever is left in the job
    /// is terminated. `immediate` terminates the job first. All output is drained before the
    /// report, so `pane.exited` follows the pane's last output.
    fn teardown(&self, rt: &PaneRt, immediate: bool) -> Value {
        let processes = rt.pty.processes();
        if immediate {
            rt.terminate();
        }
        // On Windows 10 ClosePseudoConsole blocks until conhost exits, while the reader drains.
        let closer = thread::spawn({
            let pty = Arc::clone(&rt.pty);
            move || pty.close()
        });
        let mut exit = rt.pty.wait_exit(Some(GRACE));
        if exit.is_none() || rt.pty.processes() > 0 {
            rt.terminate();
            exit = rt.pty.wait_exit(Some(GRACE));
        }
        join_bounded(closer, GRACE);
        let reader = rt
            .reader
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(r) = reader {
            join_bounded(r, GRACE);
        }
        json!({
            "terminal_id": rt.terminal_id,
            "pane_id": rt.pane_id,
            "pid": rt.pty.pid(),
            "exit_code": exit,
            "processes_in_job": processes,
            "forced": rt.forced.load(Ordering::SeqCst),
            "job_processes_left": rt.pty.processes(),
        })
    }

    /// Applies the end of a pane to the model: detaches its clients, records how it ended, and
    /// moves its Terminal to `stopped` or removes it, with the events, in one critical section.
    fn finish(&self, rt: &PaneRt, report: &Value) {
        {
            let mut st = self.lock();
            if let Some(o) = st.out.remove(&rt.pane_id) {
                st.stats.ended_output_bytes += o.seq;
                st.stats.ended_input_bytes += o.input_bytes;
            }
            st.rt.remove(&rt.pane_id);
            for c in st.conns.values_mut() {
                c.attached.remove(&rt.stream);
            }
            let tid = rt.terminal_id.clone();
            let exit_code = report["exit_code"]
                .as_u64()
                .and_then(|c| u32::try_from(c).ok());
            let Some(t) = st.reg.get_mut(&tid) else {
                return;
            };
            let cause = t.ending.take().unwrap_or(EndCause::Exited);
            t.last_exit = Some(LastExit {
                pid: Some(rt.pty.pid()),
                exit_code,
                forced: report["forced"] == true,
                cause,
            });
            t.active_pane_mut().pid = None;
            let (name, remain) = (t.name.display.clone(), t.remain_on_exit);
            let data = json!({
                "pane_id": rt.pane_id, "terminal_id": tid, "pid": rt.pty.pid(), "exit_code": exit_code,
            });
            st.notify("panes", "pane.exited", &data);
            match st.reg.apply(
                &tid,
                Transition::Ended {
                    cause,
                    remain_on_exit: remain,
                },
            ) {
                Ok(Some(_)) => st.state_changed(&tid, "stopping", Some(cause)),
                Ok(None) => st.removed(&tid, &name, cause.as_str()),
                Err(_) => {}
            }
        }
        let _ = self.events.send(Ev::Changed);
    }

    /// Ends every running pane in parallel (Host stop or idle exit); returns their reports.
    fn end_all(&self) -> Vec<Value> {
        let panes: Vec<String> = self.lock().rt.keys().cloned().collect();
        thread::scope(|s| {
            let handles: Vec<_> = panes
                .iter()
                .map(|p| s.spawn(move || self.end_pane(p, EndCause::HostStop, false)))
                .collect();
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|r| !r.is_null())
                .collect()
        })
    }

    /// The pane's reader thread: drains the pseudo terminal continuously (a full output pipe
    /// would block the pane's programs), keeps the replay ring, and fans each chunk out to the
    /// attached clients without ever waiting for one.
    fn read_output(&self, rt: &PaneRt, out: &mut dyn Read) {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match out.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let mut guard = self.lock();
            let st = &mut *guard;
            let Some(pane) = st.out.get_mut(&rt.pane_id) else {
                return;
            };
            pane.ring.push(&buf[..n]);
            let seq = pane.seq;
            pane.seq += n as u64;
            let frame: Arc<[u8]> = proto::encode_data(rt.stream, seq, 0, &buf[..n]).into();
            for c in st.conns.values_mut() {
                // Never block the pane on a client: one that lags is disconnected by its sink.
                if c.attached.contains_key(&rt.stream) && !c.sink.push(Arc::clone(&frame), None) {
                    c.attached.clear();
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

    fn serve_conn(self: Arc<Self>, pipe: &Arc<Pipe>) {
        let queue = Arc::new(Queue::default());
        let sink = Sink {
            queue: Arc::clone(&queue),
            pipe: Arc::clone(pipe),
        };
        let writer = thread::spawn({
            let pipe = Arc::clone(pipe);
            move || {
                let mut buf = Vec::new();
                while let Some(batch) = queue.take() {
                    // One write for a run of small frames (the client still reads them one
                    // by one); a single frame is written as it is.
                    let ok = if let [one] = batch.as_slice() {
                        pipe.write_all_bytes(&one.bytes).is_ok()
                    } else {
                        buf.clear();
                        for o in &batch {
                            buf.extend_from_slice(&o.bytes);
                        }
                        pipe.write_all_bytes(&buf).is_ok()
                    };
                    if !ok {
                        pipe.cancel();
                        return;
                    }
                    for a in batch.into_iter().filter_map(|o| o.ack) {
                        let _ = a.send(());
                    }
                }
            }
        });
        let hs = catch_unwind(AssertUnwindSafe(|| self.handshake(pipe, &sink)));
        {
            let mut st = self.lock();
            st.pending = st.pending.saturating_sub(1);
            if hs.is_err() {
                st.stats.panics += 1;
            }
        }
        if let Ok(Some(id)) = hs {
            let r = catch_unwind(AssertUnwindSafe(|| self.requests(id, pipe, &sink)));
            let mut st = self.lock();
            if r.is_err() {
                st.stats.panics += 1;
            }
            // A client that disappears owns nothing: only its subscriptions and attachments go.
            st.conns.remove(&id);
        }
        let _ = self.events.send(Ev::Changed);
        // The writer sends what is queued (e.g. a last error message), then ends.
        sink.queue.close();
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
                attached: BTreeMap::new(),
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

    fn requests(self: &Arc<Self>, id: u64, pipe: &Arc<Pipe>, sink: &Sink) {
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
        let rt = {
            let mut st = self.lock();
            let interactive =
                st.conns.get(&conn).and_then(|c| c.attached.get(&stream)) == Some(&true);
            let rt = st.rt_of_stream(stream).cloned().filter(|r| {
                interactive
                    && st
                        .reg
                        .get(&r.terminal_id)
                        .is_some_and(|t| t.state == session::State::Running && t.ending.is_none())
            });
            match (&rt, st.stopping) {
                (Some(r), false) => {
                    if let Some(o) = st.out.get_mut(&r.pane_id) {
                        o.input_bytes += bytes.len() as u64;
                    }
                }
                _ => st.stats.input_frames_dropped += 1,
            }
            rt.filter(|_| !st.stopping)
        };
        // Only an interactively attached cli/gui connection reaches a pane; nothing else does.
        if let Some(r) = rt {
            let _ = r.pty.write_input(bytes);
        }
    }

    fn handle(self: &Arc<Self>, conn: u64, req: &Request) -> Reply {
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
            "terminal.list" => Reply::Now(self.list(p)),
            "terminal.info" => Reply::Now(self.info(p)),
            "terminal.create" => Reply::Now(self.create_request(p)),
            "terminal.rename" => Reply::Now(self.rename(p)),
            "terminal.stop" => Reply::Now(self.end_request(p, Op::Stop)),
            "terminal.kill" => Reply::Now(self.end_request(p, Op::Kill)),
            "terminal.remove" => Reply::Now(self.end_request(p, Op::Remove)),
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
            "terminals": st.live_terminals(),
            "clients": st.conns.len(),
        })
    }

    /// `host.status.pane`: the pane of the Terminal started with `--shell`, while it exists
    /// (the Step 09 shape).
    fn initial_pane(&self, st: &State) -> Value {
        let Some(t) = st.initial.as_deref().and_then(|id| st.reg.get(id)) else {
            return Value::Null;
        };
        let p = t.active_pane();
        let live = st.live(p);
        let running = t.state.is_live() && live.running;
        let pid = p.pid.or_else(|| t.last_exit.as_ref().and_then(|e| e.pid));
        let line = std::iter::once(p.command.program.clone())
            .chain(session::redact_args(&p.command.args))
            .collect::<Vec<_>>()
            .join(" ");
        json!({
            "id": p.id,
            "terminal_id": t.id,
            "pid": pid,
            "command": line,
            "running": running,
            "exit_code": t.last_exit.as_ref().and_then(|e| e.exit_code),
            "size": {"cols": p.size.0, "rows": p.size.1},
            "job_processes": live.processes,
            "output_bytes": live.output_bytes,
        })
    }

    fn status(&self) -> Value {
        let st = self.lock();
        let attached = st.conns.values().filter(|c| !c.attached.is_empty()).count();
        let clients: Vec<Value> = st
            .conns
            .values()
            .map(|c| {
                json!({
                    "pid": c.pid, "kind": c.kind.as_str(), "name": c.name, "version": c.version,
                    "attached": !c.attached.is_empty(),
                    "interactive": c.attached.values().any(|i| *i),
                    "streams": c.attached.keys().collect::<Vec<_>>(),
                    "topics": c.topics,
                })
            })
            .collect();
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
            "terminals": st.live_terminals(),
            "terminals_total": st.reg.terminals.len(),
            "pane": self.initial_pane(&st),
            "clients": {"connected": st.conns.len(), "attached": attached, "list": clients},
        })
    }

    /// Local metrics (ADR 0012): numbers and enumerations only.
    fn stats(&self) -> Value {
        let mem = win::memory();
        let st = self.lock();
        let s = &st.stats;
        let out: u64 = st.out.values().map(|o| o.seq).sum::<u64>() + s.ended_output_bytes;
        let input: u64 = st.out.values().map(|o| o.input_bytes).sum::<u64>() + s.ended_input_bytes;
        json!({
            "host": {
                "uptime_s": self.uptime_s(),
                "handle_count": win::handle_count(),
                "memory": {
                    "working_set_bytes": mem.working_set,
                    "private_bytes": mem.private,
                    "peak_private_bytes": mem.peak_private,
                },
            },
            "terminals": st.live_terminals(),
            "terminals_total": st.reg.terminals.len(),
            "panes": st.rt.len(),
            "clients": st.conns.len(),
            "pane": {"output_bytes_total": out, "input_bytes_total": input},
            "sessions": {
                "created_total": s.terminals_created,
                "spawn_failures_total": s.spawn_failures,
                "removed_total": s.terminals_removed,
            },
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

    fn stop(&self, conn: u64, req: &Request) -> Reply {
        let (dry_run, kill) = match (
            flag(&req.params, "dry_run"),
            flag(&req.params, "kill_terminals"),
        ) {
            (Ok(d), Ok(k)) => (d, k),
            (Err(e), _) | (_, Err(e)) => return Reply::Now(Err(e)),
        };
        let (panes, would_end) = {
            let st = self.lock();
            let live: Vec<&session::Terminal> = st
                .reg
                .terminals
                .iter()
                .filter(|t| t.state.is_live())
                .collect();
            let panes: Vec<String> = live.iter().map(|t| t.active_pane().id.clone()).collect();
            let would_end: Vec<Value> = live
                .iter()
                .map(|t| {
                    let p = t.active_pane();
                    let l = st.live(p);
                    json!({
                        "terminal_id": t.id, "name": t.name.display, "pane_id": p.id,
                        "pid": p.pid, "processes": l.processes,
                        "busy": session::busy(&p.command.program, l.processes),
                    })
                })
                .collect();
            (panes, would_end)
        };
        if dry_run {
            return Reply::Now(Ok(json!({
                "dry_run": true, "would_kill": panes, "would_end": would_end,
                "would_stop": self.status(),
            })));
        }
        if !panes.is_empty() && !kill {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::Busy,
                "the Host owns live Terminals; set kill_terminals to end them",
            )
            .details(json!({ "live": panes, "terminals": would_end }))));
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

    /// `pane.attach`: a target (any level; a Terminal or window means its active pane), or none
    /// when the Host has exactly one running pane.
    fn attach(&self, conn: u64, req: &Request) -> Reply {
        let p = &req.params;
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
        let rt = {
            let st = self.lock();
            match self.attach_target(&st, &p["target"]) {
                Ok(rt) => rt,
                Err(e) => return Reply::Now(Err(e)),
            }
        };
        if let Some((c, r)) = size
            && let Err(e) = rt.pty.resize(c, r)
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
        // Re-checked under the lock that ends attachments, so no client stays attached to a
        // pane that ended between the check above and here.
        if !st.rt.contains_key(&rt.pane_id) {
            return Reply::Now(Err(IpcError::new(
                ErrorCode::InvalidState,
                "the pane's process has exited",
            )));
        }
        let Some(t) = st.reg.get_mut(&rt.terminal_id) else {
            return Reply::Queued;
        };
        if let Some(s) = size {
            t.active_pane_mut().size = s;
        }
        let pane = t.active_pane().clone();
        let Some(out) = st.out.get(&rt.pane_id) else {
            return Reply::Queued;
        };
        let (seq, replay) = (out.seq, out.ring.replay());
        let Some(c) = st.conns.get_mut(&conn) else {
            return Reply::Queued;
        };
        c.attached.insert(rt.stream, interactive);
        let result = json!({
            "stream_id": rt.stream,
            "pane_id": rt.pane_id,
            "terminal_id": rt.terminal_id,
            "seq": seq,
            "interactive": interactive,
            "pane": {"id": rt.pane_id, "pid": rt.pty.pid(),
                     "size": {"cols": pane.size.0, "rows": pane.size.1}},
            "replay_bytes": replay.len(),
        });
        c.sink.control(&proto::response(req.id, Ok(result)));
        if !replay.is_empty() {
            c.sink.push(
                proto::encode_data(rt.stream, seq - replay.len() as u64, FLAG_SNAPSHOT, &replay)
                    .into(),
                None,
            );
        }
        Reply::Queued
    }

    fn attach_target(&self, st: &State, target: &Value) -> Result<Arc<PaneRt>, IpcError> {
        let pane_id = match target {
            Value::Null => {
                let live: Vec<&Arc<PaneRt>> = st.rt.values().collect();
                match live.as_slice() {
                    [] => {
                        return Err(IpcError::new(
                            ErrorCode::TargetNotFound,
                            "this Host owns no running pane (it was started with --no-shell, or every Terminal ended)",
                        ));
                    }
                    [one] => one.pane_id.clone(),
                    many => {
                        return Err(IpcError::new(
                            ErrorCode::AmbiguousTarget,
                            "this Host owns several running Terminals; name one as the target",
                        )
                        .details(json!({
                            "candidates": many.iter().map(|r| r.terminal_id.clone()).collect::<Vec<_>>()
                        })));
                    }
                }
            }
            Value::String(s) => {
                let r = st.reg.resolve(s)?;
                st.reg.terminals[r.terminal].windows[r.window].panes[r.pane]
                    .id
                    .clone()
            }
            _ => {
                return Err(IpcError::param(
                    "target",
                    "target must be a Terminal, window or pane",
                ));
            }
        };
        let t = st
            .reg
            .owner_of_pane(&pane_id)
            .ok_or_else(|| IpcError::new(ErrorCode::TargetNotFound, "no such pane"))?;
        if t.state != session::State::Running || t.ending.is_some() {
            return Err(IpcError::new(
                ErrorCode::InvalidState,
                format!(
                    "Terminal `{}` is {}; only a running Terminal can be attached",
                    t.name.display,
                    if t.ending.is_some() {
                        "stopping"
                    } else {
                        t.state.as_str()
                    }
                ),
            )
            .details(json!({"terminal_id": t.id, "state": t.state.as_str()})));
        }
        st.rt
            .get(&pane_id)
            .cloned()
            .ok_or_else(|| IpcError::new(ErrorCode::InvalidState, "the pane's process has exited"))
    }

    fn detach(&self, conn: u64, params: &Value) -> Result<Value, IpcError> {
        let mut st = self.lock();
        let known = |st: &State, s: u32| st.reg.pane_of_stream(s).is_some();
        match &params["stream_id"] {
            Value::Null => {
                if let Some(c) = st.conns.get_mut(&conn) {
                    c.attached.clear();
                }
            }
            v => {
                let s = v
                    .as_u64()
                    .and_then(|s| u32::try_from(s).ok())
                    .ok_or_else(|| IpcError::param("stream_id", "no such stream"))?;
                let was = st
                    .conns
                    .get_mut(&conn)
                    .is_some_and(|c| c.attached.remove(&s).is_some());
                if !was && !known(&st, s) {
                    return Err(IpcError::param("stream_id", "no such stream"));
                }
            }
        }
        Ok(json!({}))
    }

    fn resize(&self, conn: u64, params: &Value) -> Result<Value, IpcError> {
        let (c, r) = size_param(params, "cols")?
            .ok_or_else(|| IpcError::param("cols", "cols and rows are required"))?;
        let rt = {
            let st = self.lock();
            if st.rt.is_empty() {
                return Err(IpcError::new(
                    ErrorCode::TargetNotFound,
                    "this Host owns no running pane",
                ));
            }
            let mine: Vec<u32> = st
                .conns
                .get(&conn)
                .map(|c| {
                    c.attached
                        .iter()
                        .filter(|(_, i)| **i)
                        .map(|(s, _)| *s)
                        .collect()
                })
                .unwrap_or_default();
            let stream = match &params["stream_id"] {
                Value::Null => match mine.as_slice() {
                    [one] => *one,
                    [] => {
                        return Err(IpcError::new(
                            ErrorCode::InvalidState,
                            "attach interactively before resizing the pane",
                        ));
                    }
                    _ => {
                        return Err(IpcError::param(
                            "stream_id",
                            "attached to several panes: name the stream_id to resize",
                        ));
                    }
                },
                v => {
                    let s = v
                        .as_u64()
                        .and_then(|s| u32::try_from(s).ok())
                        .ok_or_else(|| IpcError::param("stream_id", "no such stream"))?;
                    if !mine.contains(&s) {
                        return Err(IpcError::new(
                            ErrorCode::InvalidState,
                            "attach interactively to this stream before resizing it",
                        ));
                    }
                    s
                }
            };
            st.rt_of_stream(stream)
                .cloned()
                .ok_or_else(|| IpcError::new(ErrorCode::InvalidState, "the pane has ended"))?
        };
        rt.pty
            .resize(c, r)
            .map_err(|e| IpcError::new(ErrorCode::InternalError, format!("resize failed: {e}")))?;
        if let Some(t) = self.lock().reg.get_mut(&rt.terminal_id) {
            t.active_pane_mut().size = (c, r);
        }
        Ok(json!({"cols": c, "rows": r, "stream_id": rt.stream}))
    }

    fn list(&self, params: &Value) -> Result<Value, IpcError> {
        let full = match optional_string(params, "detail")? {
            None | Some("summary") => false,
            Some("full") => true,
            Some(_) => {
                return Err(IpcError::param("detail", "detail must be summary or full"));
            }
        };
        let st = self.lock();
        let terminals: Vec<Value> = st
            .reg
            .terminals
            .iter()
            .map(|t| {
                if full {
                    st.terminal_json(t)
                } else {
                    st.summary_json(t)
                }
            })
            .collect();
        Ok(json!({ "terminals": terminals }))
    }

    fn info(&self, params: &Value) -> Result<Value, IpcError> {
        let target = target(params)?;
        let st = self.lock();
        let i = st.reg.resolve_terminal(target)?;
        Ok(json!({ "terminal": st.terminal_json(&st.reg.terminals[i]) }))
    }

    fn create_request(self: &Arc<Self>, p: &Value) -> Result<Value, IpcError> {
        if !p["profile"].is_null() {
            return Err(IpcError::param(
                "profile",
                "`profile` is part of IPC v1 but not implemented by this Host yet (Step 33)",
            ));
        }
        let env = EnvOverrides::from_json(&p["env"]).map_err(|m| IpcError::param("env", m))?;
        let name = optional_string(p, "name")?
            .map(session::parse_name)
            .transpose()?;
        let command = match &p["command"] {
            Value::Null => session::split_command_line(&default_shell()),
            Value::Object(c) => {
                let program = c
                    .get("program")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty() && !s.contains('\0'))
                    .ok_or_else(|| {
                        IpcError::param("command", "command.program must be a non-empty string")
                    })?;
                let args = match c.get("args") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| v.as_str().filter(|s| !s.contains('\0')).map(String::from))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            IpcError::param("command", "command.args must be a list of strings")
                        })?,
                    Some(_) => {
                        return Err(IpcError::param(
                            "command",
                            "command.args must be a list of strings",
                        ));
                    }
                };
                Command {
                    program: program.to_string(),
                    args,
                }
            }
            _ => {
                return Err(IpcError::param(
                    "command",
                    "command must be {program, args}",
                ));
            }
        };
        let line = win::launch::command_line(&command.program, &command.args);
        if line.encode_utf16().count() >= MAX_COMMAND_LINE {
            return Err(IpcError::param(
                "command",
                "the command line is longer than 32767 characters",
            ));
        }
        let cwd = match optional_string(p, "cwd")? {
            Some(c) if !Path::new(c).is_absolute() => {
                return Err(IpcError::param("cwd", "cwd must be an absolute path"));
            }
            Some(c) => Some(c.to_string()),
            None => self.home.as_ref().map(|h| h.display().to_string()),
        };
        let size = size_param(&p["size"], "size")?.unwrap_or(DEFAULT_SIZE);
        let remain_on_exit = flag(p, "remain_on_exit")?;
        let keep = flag(p, "keep_on_failure")?;
        if flag(p, "dry_run")? {
            let st = self.lock();
            let name = match &name {
                Some(n) => {
                    st.reg.check_free(n, None)?;
                    n.display.clone()
                }
                None => st.reg.default_name().display,
            };
            drop(st);
            // The same checks as a real start (working directory, program on the pane's PATH,
            // scripts), with nothing started.
            let dir = cwd
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            let executable = pty::plan(&pty::Spec {
                program: &command.program,
                args: &command.args,
                raw_line: None,
                cwd: &dir,
                env: &env,
                host_vars: &[],
                size,
            })
            .map_err(|e| spawn_failed(&e, &command.program, false, None))?;
            return Ok(json!({"dry_run": true, "would_create": {
                "name": name,
                "command": {"program": command.program, "args": session::redact_args(&command.args)},
                "executable": executable.display().to_string(),
                "cwd": cwd,
                "env": session::env_json(&env.names()),
                "size": {"cols": size.0, "rows": size.1},
                "remain_on_exit": remain_on_exit,
                "keep_on_failure": keep,
            }}));
        }
        let spec = Spec {
            name,
            command,
            cwd,
            env: env.names(),
            size,
            remain_on_exit,
        };
        self.create(
            spec,
            &Launch {
                raw_line: None,
                env,
            },
            keep,
        )
    }

    fn rename(&self, p: &Value) -> Result<Value, IpcError> {
        let target = target(p)?;
        let raw = optional_string(p, "name")?
            .ok_or_else(|| IpcError::param("name", "name is required"))?;
        let name = session::parse_name(raw)?;
        let dry_run = flag(p, "dry_run")?;
        let mut st = self.lock();
        let i = st.reg.resolve_terminal(target)?;
        let id = st.reg.terminals[i].id.clone();
        st.reg.check_free(&name, Some(&id))?;
        let previous = st.reg.terminals[i].name.display.clone();
        if dry_run {
            return Ok(json!({"dry_run": true, "would_rename": {
                "terminal_id": id, "from": previous, "to": name.display,
            }}));
        }
        let changed = previous != name.display;
        if changed {
            st.reg.terminals[i].name = name.clone();
            let data = json!({"terminal_id": id, "name": name.display, "previous_name": previous});
            st.notify("terminals", "terminal.renamed", &data);
        }
        Ok(json!({
            "terminal_id": id, "name": name.display, "previous_name": previous, "changed": changed,
        }))
    }

    /// `terminal.stop`, `terminal.kill` and `terminal.remove` (docs/SESSION_MODEL.md §4).
    fn end_request(&self, p: &Value, op: Op) -> Result<Value, IpcError> {
        let target = target(p)?;
        let force = flag(p, "force")?;
        let dry_run = flag(p, "dry_run")?;
        let (tid, pane_id, name) = {
            let mut st = self.lock();
            let i = st.reg.resolve_terminal(target)?;
            let t = &st.reg.terminals[i];
            let pane = t.active_pane();
            let live = st.live(pane);
            let busy = t.state == session::State::Running
                && session::busy(&pane.command.program, live.processes);
            let plan = json!({
                "terminal_id": t.id, "name": t.name.display, "state": t.state.as_str(),
                "pane_id": pane.id, "pid": pane.pid, "program": pane.command.program,
                "processes": live.processes, "busy": busy,
                "ends_processes": t.state.is_live() && !(op == Op::Remove && !force),
                "removes_terminal": op != Op::Stop,
            });
            let (tid, pane_id, name, state) = (
                t.id.clone(),
                pane.id.clone(),
                t.name.display.clone(),
                t.state,
            );
            match state {
                session::State::Starting => {
                    return Err(IpcError::new(
                        ErrorCode::InvalidState,
                        format!("Terminal `{name}` is still starting; try again"),
                    )
                    .details(json!({"terminal_id": tid, "state": "starting"})));
                }
                session::State::Running | session::State::Stopping
                    if op == Op::Remove && !force =>
                {
                    return Err(IpcError::new(
                        ErrorCode::InvalidState,
                        format!(
                            "Terminal `{name}` is {}; stop it first, or set force to end its processes and remove it",
                            state.as_str()
                        ),
                    )
                    .details(json!({"terminal_id": tid, "state": state.as_str()})));
                }
                session::State::Running if busy && !force => {
                    return Err(IpcError::new(
                        ErrorCode::Busy,
                        format!(
                            "Terminal `{name}` runs more than an idle shell ({} process(es) in its job); set force to end them",
                            live.processes
                        ),
                    )
                    .details(plan));
                }
                _ => {}
            }
            if dry_run {
                let key = match op {
                    Op::Stop => "would_stop",
                    Op::Kill => "would_kill",
                    Op::Remove => "would_remove",
                };
                return Ok(json!({"dry_run": true, key: plan}));
            }
            if !state.is_live() {
                // Nothing runs: stop is a no-op; kill and remove delete the stored record.
                if op == Op::Stop {
                    return Ok(json!({
                        "affected": [], "terminal": st.terminal_json(t), "removed": false,
                        "terminal_id": tid, "name": name,
                    }));
                }
                st.reg.apply(&tid, Transition::Remove)?;
                st.removed(&tid, &name, "removed");
                drop(st);
                let _ = self.events.send(Ev::Changed);
                return Ok(json!({
                    "affected": [], "terminal": null, "removed": true,
                    "terminal_id": tid, "name": name,
                }));
            }
            (tid, pane_id, name)
        };
        let report = match op {
            Op::Stop => self.end_pane(&pane_id, EndCause::Stop, false),
            Op::Kill | Op::Remove => self.end_pane(&pane_id, EndCause::Kill, true),
        };
        let st = self.lock();
        let after = st.reg.get(&tid);
        Ok(json!({
            "affected": if report.is_null() { vec![] } else { vec![report] },
            "terminal": after.map(|t| st.terminal_json(t)),
            "removed": after.is_none(),
            "terminal_id": tid,
            "name": name,
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Stop,
    Kill,
    Remove,
}
