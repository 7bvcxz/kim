//! The window's side of the Host conversation (IPC v1, docs/contracts/IPC_PROTOCOL.md), as a
//! state machine without I/O: it is told what arrived (a connection, a response, an event, a
//! data frame, a disconnect, a key in a chooser) and answers with requests to send and what the
//! window must do. The window module does the pipe and the drawing; this module decides, and is
//! unit-tested on its own.
//!
//! Rules it keeps (ADR 0009, ADR 0020, `SUBAK-FR-003`, `SUBAK-FR-015`):
//!
//! - it never asks the Host to create, stop, kill or remove anything: it lists, subscribes,
//!   attaches (interactively, at the window's grid size), resizes and detaches;
//! - it attaches to an existing Terminal only — the one named on the command line, else the one
//!   shown last if it still runs, else the only running one, else the user chooses;
//! - when the Host goes away it keeps the last screen, retries with a growing delay without
//!   starting a Host, and reattaches to the same Terminal (by id) when a Host serves it again.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

/// A Terminal as the chooser and the title show it (`terminal.list` with `detail: "full"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermInfo {
    pub id: String,
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub program: String,
    /// The pane's history limit: the view keeps as many lines.
    pub history_limit: usize,
}

impl TermInfo {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    fn from_full(t: &Value) -> Option<Self> {
        let pane = &t["windows"][0]["panes"][0];
        let program = pane["executable"]
            .as_str()
            .or_else(|| pane["command"]["program"].as_str())
            .unwrap_or("");
        let program = program
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(program)
            .to_string();
        Some(Self {
            id: t["id"].as_str()?.to_string(),
            name: t["name"].as_str()?.to_string(),
            state: t["state"].as_str().unwrap_or("unknown").to_string(),
            pid: pane["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
            program,
            history_limit: pane["history"]["limit_lines"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(2000),
        })
    }
}

/// Why the shown Terminal is no longer live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// Its program exited.
    Exited(Option<i64>),
    /// Another client detached this window (`subak detach`); the Terminal still runs.
    DetachedBy(Option<String>),
    /// It was removed (killed or removed by a client).
    Removed,
    /// It was stopped and is kept as `stopped`.
    Stopped,
}

/// What the window shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// Connecting (and starting a Host on demand on the first connection).
    Connecting,
    /// Connected; no Terminal runs. A Terminal created later opens by itself.
    Empty { notice: Option<String> },
    /// Several Terminals run and none was named: the user chooses.
    Choosing {
        list: Vec<TermInfo>,
        selected: usize,
        notice: Option<String>,
    },
    /// `pane.attach` is on its way.
    Attaching(TermInfo),
    /// The pane is shown live.
    Attached {
        term: TermInfo,
        stream: u32,
        pane_pid: u32,
        pane_id: String,
    },
    /// The pane was live and is not any more; its last screen stays.
    Ended { term: TermInfo, why: EndReason },
    /// The connection to the Host is gone; reconnecting (the last screen stays).
    Disconnected {
        last: Option<TermInfo>,
        attempt: u32,
        reason: String,
    },
    /// The first connection failed (no Host could be reached or started).
    Failed {
        code: String,
        message: String,
        hint: Option<String>,
    },
}

impl Phase {
    /// A short name for logs and tests.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Empty { .. } => "empty",
            Self::Choosing { .. } => "choosing",
            Self::Attaching(_) => "attaching",
            Self::Attached { .. } => "attached",
            Self::Ended { .. } => "ended",
            Self::Disconnected { .. } => "disconnected",
            Self::Failed { .. } => "failed",
        }
    }

    /// Whether the window shows a pane's screen (live or the last one) rather than a state screen.
    pub fn shows_pane(&self) -> bool {
        matches!(
            self,
            Self::Attached { .. } | Self::Ended { .. } | Self::Disconnected { last: Some(_), .. }
        )
    }
}

/// What the window must do after the model handled something.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send this control message (a request with its id).
    Send(Value),
    /// The phase changed: redraw, retitle, report it (tests).
    Phase,
    /// Start a fresh view of `cols × rows` keeping `history` lines: a snapshot begins.
    ResetView {
        cols: u16,
        rows: u16,
        history: usize,
    },
    /// Remember this Terminal as the one shown last (preferences).
    Remember(TermInfo),
    /// Try to connect again after this delay (never starting a Host).
    Reconnect(Duration),
    /// Connect again now, starting a Host on demand (the user retried a failed start).
    Connect,
}

/// What a data frame is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frame {
    /// Not the shown pane's stream (or nothing is attached): drop it.
    Ignore,
    /// Feed it to the view.
    Feed,
    /// A snapshot begins: reset the view first (see [`Effect::ResetView`]), then feed it.
    Reset {
        cols: u16,
        rows: u16,
        history: usize,
    },
}

/// What the user asked for on the command line or what is remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Want {
    /// Nothing: the last one, the only one, or a choice.
    Any,
    /// `subak-gui <target>`: a name or an id.
    Target(String),
    /// Reopen this Terminal (remembered from the last run, or the one shown before a disconnect).
    Last { id: String, name: String },
}

#[derive(Debug, Clone)]
enum Pending {
    List,
    Subscribe,
    Attach(Option<TermInfo>),
    Resize,
    Detach,
}

/// Retry delays after a disconnect: 0.5, 1, 2, 4, 8 s, then every 10 s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    let ms = 500u64.saturating_mul(1 << attempt.min(5));
    Duration::from_millis(ms.min(10_000))
}

/// The model.
#[derive(Debug)]
pub struct Model {
    pub phase: Phase,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    want: Want,
    /// The grid the window shows (what attach and resize ask for).
    grid: (u16, u16),
    /// Inside a run of snapshot frames.
    in_snapshot: bool,
    /// The size of the snapshot that follows an attach or a resync.
    snapshot_size: Option<(u16, u16)>,
    /// The Host's reason when it announced that it stops.
    host_stopping: Option<String>,
    /// Whether a connection is up.
    connected: bool,
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

impl Model {
    pub fn new(want: Want, grid: (u16, u16)) -> Self {
        Self {
            phase: Phase::Connecting,
            next_id: 1,
            pending: HashMap::new(),
            want,
            grid,
            in_snapshot: false,
            snapshot_size: None,
            host_stopping: None,
            connected: false,
        }
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    /// Inside a run of snapshot frames (or waiting for the snapshot of an attach or resync): the
    /// view keeps the snapshot's size until it is complete.
    pub fn in_snapshot(&self) -> bool {
        self.in_snapshot || self.snapshot_size.is_some()
    }

    /// The shown Terminal, live or not.
    pub fn terminal(&self) -> Option<&TermInfo> {
        match &self.phase {
            Phase::Attached { term, .. } | Phase::Ended { term, .. } | Phase::Attaching(term) => {
                Some(term)
            }
            Phase::Disconnected { last, .. } => last.as_ref(),
            _ => None,
        }
    }

    /// The stream to send input to, while the pane is live.
    pub fn input_stream(&self) -> Option<u32> {
        match self.phase {
            Phase::Attached { stream, .. } => Some(stream),
            _ => None,
        }
    }

    fn request(&mut self, method: &str, params: Value, p: Pending) -> Effect {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id, p);
        let mut msg = json!({"type": "request", "id": id, "method": method});
        msg["params"] = params;
        Effect::Send(msg)
    }

    fn set(&mut self, phase: Phase, out: &mut Vec<Effect>) {
        if self.phase != phase {
            self.phase = phase;
            out.push(Effect::Phase);
        }
    }

    /// A connection is up (handshake done): subscribe, then list.
    pub fn on_connected(&mut self) -> Vec<Effect> {
        self.connected = true;
        self.pending.clear();
        self.host_stopping = None;
        self.in_snapshot = false;
        let mut out = vec![
            self.request(
                "events.subscribe",
                json!({"topics": ["terminals", "panes", "host"]}),
                Pending::Subscribe,
            ),
            self.request("terminal.list", json!({"detail": "full"}), Pending::List),
        ];
        if !self.phase.shows_pane() {
            self.set(Phase::Connecting, &mut out);
        }
        out
    }

    /// A connection attempt failed. The first connection shows the error and waits for the user;
    /// a reconnection tries again later.
    pub fn on_connect_failed(
        &mut self,
        code: &str,
        message: &str,
        hint: Option<&str>,
    ) -> Vec<Effect> {
        let mut out = Vec::new();
        match &self.phase {
            Phase::Disconnected {
                last,
                attempt,
                reason,
            } => {
                let next = Phase::Disconnected {
                    last: last.clone(),
                    attempt: attempt + 1,
                    reason: reason.clone(),
                };
                out.push(Effect::Reconnect(reconnect_delay(attempt + 1)));
                self.set(next, &mut out);
            }
            _ => self.set(
                Phase::Failed {
                    code: code.to_string(),
                    message: message.to_string(),
                    hint: hint.map(String::from),
                },
                &mut out,
            ),
        }
        out
    }

    /// The connection ended (the Host stopped or crashed, or the pipe broke).
    pub fn on_disconnected(&mut self) -> Vec<Effect> {
        self.connected = false;
        self.pending.clear();
        self.in_snapshot = false;
        let last = match &self.phase {
            Phase::Attached { term, .. } | Phase::Attaching(term) => Some(term.clone()),
            Phase::Ended { term, .. } => Some(term.clone()),
            Phase::Disconnected { last, .. } => last.clone(),
            _ => None,
        };
        if let Some(t) = &last {
            self.want = Want::Last {
                id: t.id.clone(),
                name: t.name.clone(),
            };
        }
        let reason = match self.host_stopping.take().as_deref() {
            Some("idle") => "the Host exited (idle)".to_string(),
            Some(_) => "the Host was stopped".to_string(),
            None => "the connection to the Host was lost".to_string(),
        };
        let mut out = vec![Effect::Reconnect(reconnect_delay(0))];
        self.set(
            Phase::Disconnected {
                last,
                attempt: 0,
                reason,
            },
            &mut out,
        );
        out
    }

    /// The user pressed Enter (or clicked the action) on a state screen or banner.
    pub fn activate(&mut self) -> Vec<Effect> {
        let mut out = Vec::new();
        match self.phase.clone() {
            Phase::Choosing { list, selected, .. } => {
                if let Some(t) = list.get(selected) {
                    out.push(self.attach(Some(t.clone()), &t.id));
                    self.set(Phase::Attaching(t.clone()), &mut out);
                }
            }
            Phase::Failed { .. } => {
                out.push(Effect::Connect);
                self.set(Phase::Connecting, &mut out);
            }
            Phase::Disconnected { .. } => out.push(Effect::Reconnect(Duration::ZERO)),
            Phase::Ended {
                term,
                why: EndReason::DetachedBy(_),
            } if self.connected => {
                out.push(self.attach(Some(term.clone()), &term.id));
                self.set(Phase::Attaching(term), &mut out);
            }
            Phase::Ended { .. } | Phase::Empty { .. } if self.connected => {
                self.want = Want::Any;
                out.push(self.request("terminal.list", json!({"detail": "full"}), Pending::List));
            }
            _ => {}
        }
        out
    }

    /// Moves the chooser's selection by `delta` rows (or to `to`, a row index).
    pub fn choose(&mut self, delta: isize, to: Option<usize>) -> Vec<Effect> {
        let mut out = Vec::new();
        if let Phase::Choosing {
            list,
            selected,
            notice,
        } = &self.phase
            && !list.is_empty()
        {
            let n = list.len();
            let next = to.unwrap_or_else(|| {
                (selected.cast_signed() + delta).clamp(0, n.cast_signed() - 1) as usize
            });
            let next = next.min(n - 1);
            let phase = Phase::Choosing {
                list: list.clone(),
                selected: next,
                notice: notice.clone(),
            };
            self.set(phase, &mut out);
        }
        out
    }

    fn attach(&mut self, term: Option<TermInfo>, target: &str) -> Effect {
        let (cols, rows) = self.grid;
        self.in_snapshot = false;
        self.snapshot_size = None;
        self.request(
            "pane.attach",
            json!({"target": target, "interactive": true, "size": {"cols": cols, "rows": rows}}),
            Pending::Attach(term),
        )
    }

    /// The window's grid changed (a resize, a DPI or font change). A live pane is resized to it;
    /// the view is resized by the window at once.
    pub fn on_grid(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        if self.grid == (cols, rows) {
            return Vec::new();
        }
        self.grid = (cols, rows);
        match self.phase {
            Phase::Attached { stream, .. } => vec![self.request(
                "pane.resize",
                json!({"cols": cols, "rows": rows, "stream_id": stream}),
                Pending::Resize,
            )],
            _ => Vec::new(),
        }
    }

    /// Closing the window: detach (the Terminal keeps running).
    pub fn on_close(&mut self) -> Vec<Effect> {
        match self.phase {
            Phase::Attached { .. } | Phase::Attaching(_) if self.connected => {
                vec![self.request("pane.detach", json!({}), Pending::Detach)]
            }
            _ => Vec::new(),
        }
    }

    /// A data frame arrived.
    pub fn on_data(&mut self, stream: u32, snapshot: bool) -> Frame {
        let Phase::Attached { stream: mine, .. } = self.phase else {
            return Frame::Ignore;
        };
        if stream != mine {
            return Frame::Ignore;
        }
        let begins = snapshot && !self.in_snapshot;
        self.in_snapshot = snapshot;
        if begins {
            let (cols, rows) = self.snapshot_size.take().unwrap_or(self.grid);
            let history = self.terminal().map_or(2000, |t| t.history_limit);
            return Frame::Reset {
                cols,
                rows,
                history,
            };
        }
        Frame::Feed
    }

    /// A control message (response or event) arrived.
    pub fn on_control(&mut self, m: &Value) -> Vec<Effect> {
        match m["type"].as_str() {
            Some("response") => {
                let Some(p) = m["id"].as_u64().and_then(|id| self.pending.remove(&id)) else {
                    return Vec::new();
                };
                if m["ok"] == true {
                    self.on_result(p, &m["result"])
                } else {
                    self.on_error(p, &m["error"])
                }
            }
            Some("event") => self.on_event(m["event"].as_str().unwrap_or(""), &m["data"]),
            _ => Vec::new(),
        }
    }

    fn on_result(&mut self, p: Pending, r: &Value) -> Vec<Effect> {
        let mut out = Vec::new();
        match p {
            Pending::List => {
                let list: Vec<TermInfo> = r["terminals"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(TermInfo::from_full)
                    .collect();
                self.decide(&list, &mut out);
            }
            Pending::Attach(known) => {
                let (Some(stream), Some(tid)) = (
                    r["stream_id"].as_u64().and_then(|s| u32::try_from(s).ok()),
                    r["terminal_id"].as_str(),
                ) else {
                    return out;
                };
                let mut term = known.unwrap_or_else(|| TermInfo {
                    id: tid.to_string(),
                    name: tid.to_string(),
                    state: "running".into(),
                    pid: None,
                    program: String::new(),
                    history_limit: 2000,
                });
                term.id = tid.to_string();
                let pane_pid = r["pane"]["pid"]
                    .as_u64()
                    .and_then(|p| u32::try_from(p).ok())
                    .unwrap_or(0);
                term.pid = Some(pane_pid);
                let size = &r["snapshot"]["size"];
                self.snapshot_size = match (size["cols"].as_u64(), size["rows"].as_u64()) {
                    // A snapshot of no frames would never begin: nothing to wait for.
                    _ if r["snapshot"]["frames"].as_u64() == Some(0) => None,
                    (Some(c), Some(r)) => Some((
                        u16::try_from(c).unwrap_or(self.grid.0),
                        u16::try_from(r).unwrap_or(self.grid.1),
                    )),
                    _ => None,
                };
                self.in_snapshot = false;
                self.want = Want::Last {
                    id: term.id.clone(),
                    name: term.name.clone(),
                };
                out.push(Effect::Remember(term.clone()));
                // The snapshot frames follow; until then the view starts fresh at their size.
                let (cols, rows) = match (size["cols"].as_u64(), size["rows"].as_u64()) {
                    (Some(c), Some(r)) => (
                        u16::try_from(c).unwrap_or(self.grid.0),
                        u16::try_from(r).unwrap_or(self.grid.1),
                    ),
                    _ => self.grid,
                };
                out.push(Effect::ResetView {
                    cols,
                    rows,
                    history: term.history_limit,
                });
                let phase = Phase::Attached {
                    term,
                    stream,
                    pane_pid,
                    pane_id: r["pane_id"].as_str().unwrap_or("").to_string(),
                };
                self.set(phase, &mut out);
                // The window may have changed size while the attach was on its way.
                if (cols, rows) != self.grid {
                    let g = self.grid;
                    self.grid = (cols, rows);
                    out.extend(self.on_grid(g.0, g.1));
                }
            }
            Pending::Subscribe | Pending::Resize | Pending::Detach => {}
        }
        out
    }

    fn on_error(&mut self, p: Pending, e: &Value) -> Vec<Effect> {
        let mut out = Vec::new();
        let code = e["code"].as_str().unwrap_or("internal_error");
        let message = e["message"].as_str().unwrap_or("request failed");
        match p {
            Pending::Attach(term) => {
                // The Terminal is gone or not running any more: choose again.
                let what = term.map_or_else(
                    || match &self.want {
                        Want::Target(t) => format!("`{t}`"),
                        _ => "the Terminal".into(),
                    },
                    |t| format!("`{}`", t.name),
                );
                let notice = match code {
                    "target_not_found" => format!("No Terminal {what} exists"),
                    "invalid_state" => format!("{what} is not running"),
                    _ => format!("Cannot open {what}: {message} ({code})"),
                };
                self.want = Want::Any;
                out.push(self.request("terminal.list", json!({"detail": "full"}), Pending::List));
                self.set(
                    Phase::Empty {
                        notice: Some(notice),
                    },
                    &mut out,
                );
            }
            Pending::List => self.set(
                Phase::Failed {
                    code: code.to_string(),
                    message: message.to_string(),
                    hint: None,
                },
                &mut out,
            ),
            Pending::Subscribe | Pending::Resize | Pending::Detach => {}
        }
        out
    }

    /// Picks what to show from a Terminal list.
    fn decide(&mut self, list: &[TermInfo], out: &mut Vec<Effect>) {
        let notice = match &self.phase {
            Phase::Empty { notice } | Phase::Choosing { notice, .. } => notice.clone(),
            _ => None,
        };
        // A live or last-shown pane stays as it is; only a reattach changes it.
        if matches!(self.phase, Phase::Attached { .. } | Phase::Attaching(_)) {
            return;
        }
        let running: Vec<&TermInfo> = list.iter().filter(|t| t.running()).collect();
        match self.want.clone() {
            Want::Target(t) => {
                self.want = Want::Any;
                let known = list
                    .iter()
                    .find(|x| x.id == t || x.name.to_lowercase() == t.to_lowercase())
                    .cloned();
                let shown = known.unwrap_or_else(|| TermInfo {
                    id: t.clone(),
                    name: t.clone(),
                    state: "unknown".into(),
                    pid: None,
                    program: String::new(),
                    history_limit: 2000,
                });
                out.push(self.attach(Some(shown.clone()), &t));
                self.set(Phase::Attaching(shown), out);
                return;
            }
            Want::Last { id, name } => {
                let hit = running
                    .iter()
                    .find(|x| x.id == id)
                    .or_else(|| running.iter().find(|x| x.name == name));
                if let Some(t) = hit {
                    let t = (*t).clone();
                    out.push(self.attach(Some(t.clone()), &t.id));
                    self.set(Phase::Attaching(t), out);
                    return;
                }
                self.want = Want::Any;
                if matches!(self.phase, Phase::Disconnected { .. } | Phase::Connecting)
                    && self.terminal().is_some()
                {
                    // It ended while the Host was away (or with it).
                    let gone = Some(format!("`{name}` is no longer running"));
                    self.choose_from(&running, gone, out);
                    return;
                }
            }
            Want::Any => {}
        }
        if running.len() == 1 && notice.is_none() {
            let t = running[0].clone();
            out.push(self.attach(Some(t.clone()), &t.id));
            self.set(Phase::Attaching(t), out);
            return;
        }
        self.choose_from(&running, notice, out);
    }

    fn choose_from(
        &mut self,
        running: &[&TermInfo],
        notice: Option<String>,
        out: &mut Vec<Effect>,
    ) {
        if running.is_empty() {
            self.set(Phase::Empty { notice }, out);
        } else {
            let selected = match &self.phase {
                Phase::Choosing { list, selected, .. } => list
                    .get(*selected)
                    .and_then(|s| running.iter().position(|t| t.id == s.id))
                    .unwrap_or(0),
                _ => 0,
            };
            self.set(
                Phase::Choosing {
                    list: running.iter().map(|t| (*t).clone()).collect(),
                    selected,
                    notice,
                },
                out,
            );
        }
    }

    fn on_event(&mut self, name: &str, d: &Value) -> Vec<Effect> {
        let mut out = Vec::new();
        let ours_pane = |m: &Model| match &m.phase {
            Phase::Attached { pane_id, .. } => d["pane_id"].as_str() == Some(pane_id.as_str()),
            _ => false,
        };
        let ours_term = |m: &Model| {
            m.terminal()
                .is_some_and(|t| d["terminal_id"].as_str() == Some(t.id.as_str()))
        };
        match name {
            "pane.exited" if ours_pane(self) => {
                let term = self.terminal().cloned().expect("attached");
                self.set(
                    Phase::Ended {
                        term,
                        why: EndReason::Exited(d["exit_code"].as_i64()),
                    },
                    &mut out,
                );
            }
            "pane.detached" if ours_pane(self) => {
                let term = self.terminal().cloned().expect("attached");
                let by = d["by"]["kind"].as_str().map(|k| {
                    format!(
                        "{k} client{}",
                        d["by"]["pid"]
                            .as_u64()
                            .map_or(String::new(), |p| format!(" (pid {p})"))
                    )
                });
                self.set(
                    Phase::Ended {
                        term,
                        why: EndReason::DetachedBy(by),
                    },
                    &mut out,
                );
            }
            "pane.resync" if ours_pane(self) => {
                // A snapshot at the pane's current size follows (the window's grid).
                self.in_snapshot = false;
                self.snapshot_size = Some(self.grid);
            }
            "terminal.renamed" if ours_term(self) => {
                if let Some(n) = text(&d["name"]) {
                    let mut phase = self.phase.clone();
                    match &mut phase {
                        Phase::Attached { term, .. }
                        | Phase::Ended { term, .. }
                        | Phase::Attaching(term) => term.name = n,
                        _ => {}
                    }
                    if let Some(t) = match &phase {
                        Phase::Attached { term, .. } => Some(term.clone()),
                        _ => None,
                    } {
                        out.push(Effect::Remember(t));
                    }
                    self.set(phase, &mut out);
                }
            }
            "terminal.removed" if ours_term(self) => {
                if let Phase::Attached { term, .. } = &self.phase {
                    let term = term.clone();
                    self.set(
                        Phase::Ended {
                            term,
                            why: EndReason::Removed,
                        },
                        &mut out,
                    );
                }
            }
            "terminal.state_changed" if ours_term(self) => {
                if d["state"] == "stopped"
                    && let Phase::Attached { term, .. } = &self.phase
                {
                    let term = term.clone();
                    self.set(
                        Phase::Ended {
                            term,
                            why: EndReason::Stopped,
                        },
                        &mut out,
                    );
                }
            }
            "host.stopping" => self.host_stopping = text(&d["reason"]),
            "terminal.created"
            | "terminal.removed"
            | "terminal.state_changed"
            | "terminal.renamed" => {
                // The chooser and the empty screen follow other clients' changes.
                if matches!(self.phase, Phase::Empty { .. } | Phase::Choosing { .. }) {
                    out.push(self.request(
                        "terminal.list",
                        json!({"detail": "full"}),
                        Pending::List,
                    ));
                }
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(id: &str, name: &str, state: &str, pid: u32) -> Value {
        json!({
            "id": id, "name": name, "state": state,
            "windows": [{"panes": [{
                "pid": pid, "executable": "C:\\Windows\\System32\\cmd.exe",
                "command": {"program": "cmd.exe", "args": []},
                "history": {"limit_lines": 5000}
            }]}]
        })
    }

    fn sent(effects: &[Effect]) -> Vec<(u64, String, Value)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(m) => Some((
                    m["id"].as_u64().unwrap(),
                    m["method"].as_str().unwrap().to_string(),
                    m["params"].clone(),
                )),
                _ => None,
            })
            .collect()
    }

    fn respond(m: &mut Model, id: u64, result: Value) -> Vec<Effect> {
        let mut msg = json!({"type": "response", "id": id, "ok": true});
        msg["result"] = result;
        m.on_control(&msg)
    }

    fn list_id(effects: &[Effect]) -> u64 {
        sent(effects)
            .into_iter()
            .find(|(_, method, _)| method == "terminal.list")
            .expect("a list request")
            .0
    }

    fn attach_result(stream: u32, tid: &str, pid: u32) -> Value {
        json!({
            "stream_id": stream, "terminal_id": tid, "pane_id": "p-000000000001",
            "pane": {"id": "p-000000000001", "pid": pid, "size": {"cols": 100, "rows": 30}},
            "snapshot": {"size": {"cols": 100, "rows": 30}, "bytes": 10, "frames": 1}
        })
    }

    /// Connects and answers the list; returns the effects of the list response.
    fn connect_with(m: &mut Model, terminals: Value) -> Vec<Effect> {
        let fx = m.on_connected();
        let methods: Vec<String> = sent(&fx).into_iter().map(|s| s.1).collect();
        assert_eq!(methods, ["events.subscribe", "terminal.list"]);
        let mut result = json!({});
        result["terminals"] = terminals;
        respond(m, list_id(&fx), result)
    }

    #[test]
    fn the_only_running_terminal_opens_by_itself_interactively_at_the_window_size() {
        let mut m = Model::new(Want::Any, (100, 30));
        let fx = connect_with(
            &mut m,
            json!([
                term("t-a", "build", "running", 11),
                term("t-b", "old", "stopped", 0)
            ]),
        );
        let s = sent(&fx);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].1, "pane.attach");
        assert_eq!(
            s[0].2,
            json!({"target": "t-a", "interactive": true, "size": {"cols": 100, "rows": 30}})
        );
        assert_eq!(m.phase.name(), "attaching");
        let fx = respond(&mut m, s[0].0, attach_result(7, "t-a", 11));
        assert!(fx.contains(&Effect::ResetView {
            cols: 100,
            rows: 30,
            history: 5000
        }));
        assert!(matches!(&fx[0], Effect::Remember(t) if t.name == "build"));
        let Phase::Attached {
            stream, pane_pid, ..
        } = m.phase
        else {
            panic!("{:?}", m.phase)
        };
        assert_eq!((stream, pane_pid), (7, 11));
        assert_eq!(m.input_stream(), Some(7));
        // The model never asked to create, stop, kill or remove anything.
        for (_, method, _) in s {
            assert!(!method.starts_with("terminal.create") && !method.contains("kill"));
        }
    }

    #[test]
    fn several_terminals_are_chosen_with_the_keyboard() {
        let mut m = Model::new(Want::Any, (80, 24));
        let fx = connect_with(
            &mut m,
            json!([
                term("t-a", "one", "running", 1),
                term("t-b", "two", "running", 2)
            ]),
        );
        assert!(sent(&fx).is_empty());
        assert_eq!(m.phase.name(), "choosing");
        m.choose(1, None);
        m.choose(5, None); // clamped to the last row
        let Phase::Choosing { selected, .. } = &m.phase else {
            panic!()
        };
        assert_eq!(*selected, 1);
        let fx = m.activate();
        let s = sent(&fx);
        assert_eq!(s[0].1, "pane.attach");
        assert_eq!(s[0].2["target"], "t-b");
    }

    #[test]
    fn a_named_or_remembered_terminal_wins_and_a_missing_one_falls_back() {
        let both = json!([
            term("t-a", "one", "running", 1),
            term("t-b", "Two", "running", 2)
        ]);
        // Named on the command line (case is ignored, as by the Host).
        let mut m = Model::new(Want::Target("two".into()), (80, 24));
        let fx = connect_with(&mut m, both.clone());
        assert_eq!(sent(&fx)[0].2["target"], "two");
        // Remembered by id from the last run.
        let mut m = Model::new(
            Want::Last {
                id: "t-b".into(),
                name: "renamed".into(),
            },
            (80, 24),
        );
        let fx = connect_with(&mut m, both.clone());
        assert_eq!(sent(&fx)[0].2["target"], "t-b");
        // A name the Host does not know: the attach fails and the chooser explains it.
        let mut m = Model::new(Want::Target("nope".into()), (80, 24));
        let fx = connect_with(&mut m, both.clone());
        let id = sent(&fx)[0].0;
        let fx = m.on_control(&json!({"type": "response", "id": id, "ok": false,
            "error": {"code": "target_not_found", "message": "no such target"}}));
        assert_eq!(m.phase.name(), "empty");
        let fx = respond(&mut m, list_id(&fx), json!({"terminals": both}));
        assert!(sent(&fx).is_empty(), "a notice keeps the chooser up");
        let Phase::Choosing { notice, list, .. } = &m.phase else {
            panic!("{:?}", m.phase)
        };
        assert_eq!(list.len(), 2);
        assert!(notice.as_deref().unwrap().contains("`nope`"), "{notice:?}");
    }

    #[test]
    fn no_terminal_is_empty_and_a_new_one_opens_by_itself() {
        let mut m = Model::new(Want::Any, (80, 24));
        connect_with(&mut m, json!([]));
        assert_eq!(m.phase, Phase::Empty { notice: None });
        let fx = m.on_control(
            &json!({"type": "event", "seq": 1, "event": "terminal.created",
            "data": {"terminal": {"id": "t-n"}}}),
        );
        let fx = respond(
            &mut m,
            list_id(&fx),
            json!({"terminals": [term("t-n", "new", "running", 9)]}),
        );
        assert_eq!(sent(&fx)[0].1, "pane.attach");
    }

    fn attached(m: &mut Model) {
        let fx = connect_with(m, json!([term("t-a", "build", "running", 11)]));
        let id = sent(&fx)[0].0;
        respond(m, id, attach_result(7, "t-a", 11));
        assert_eq!(m.phase.name(), "attached");
    }

    #[test]
    fn snapshots_reset_the_view_and_other_streams_are_ignored() {
        let mut m = Model::new(Want::Any, (100, 30));
        attached(&mut m);
        assert_eq!(
            m.on_data(7, true),
            Frame::Reset {
                cols: 100,
                rows: 30,
                history: 5000
            }
        );
        assert_eq!(
            m.on_data(7, true),
            Frame::Feed,
            "the same snapshot continues"
        );
        assert_eq!(m.on_data(7, false), Frame::Feed);
        assert_eq!(m.on_data(8, false), Frame::Ignore);
        // A resync: the next snapshot resets again, at the window's grid.
        m.on_grid(90, 20);
        m.on_control(&json!({"type": "event", "seq": 2, "event": "pane.resync",
            "data": {"pane_id": "p-000000000001", "terminal_id": "t-a", "stream_id": 7, "seq": 99}}));
        assert_eq!(
            m.on_data(7, true),
            Frame::Reset {
                cols: 90,
                rows: 20,
                history: 5000
            }
        );
    }

    #[test]
    fn resizes_go_to_a_live_pane_only_and_closing_detaches() {
        let mut m = Model::new(Want::Any, (100, 30));
        assert!(m.on_grid(90, 25).is_empty(), "nothing attached");
        attached(&mut m);
        let s = sent(&m.on_grid(120, 40));
        assert_eq!(s[0].1, "pane.resize");
        assert_eq!(s[0].2, json!({"cols": 120, "rows": 40, "stream_id": 7}));
        assert!(m.on_grid(120, 40).is_empty(), "unchanged");
        let s = sent(&m.on_close());
        assert_eq!(s[0].1, "pane.detach");
    }

    #[test]
    fn the_pane_ending_or_being_detached_keeps_its_screen() {
        let mut m = Model::new(Want::Any, (100, 30));
        attached(&mut m);
        m.on_control(&json!({"type": "event", "seq": 3, "event": "pane.detached",
            "data": {"pane_id": "p-000000000001", "terminal_id": "t-a", "stream_id": 7,
                     "by": {"client_id": 4, "pid": 4242, "kind": "cli"}}}));
        assert!(matches!(
            &m.phase,
            Phase::Ended { why: EndReason::DetachedBy(Some(by)), .. } if by == "cli client (pid 4242)"
        ));
        assert!(m.phase.shows_pane());
        assert_eq!(m.input_stream(), None, "no input after a detach");
        // Enter reattaches the same Terminal.
        let s = sent(&m.activate());
        assert_eq!(s[0].2["target"], "t-a");
        respond(&mut m, s[0].0, attach_result(8, "t-a", 11));
        m.on_control(&json!({"type": "event", "seq": 4, "event": "pane.exited",
            "data": {"pane_id": "p-000000000001", "terminal_id": "t-a", "pid": 11, "exit_code": 0}}));
        assert!(matches!(
            m.phase,
            Phase::Ended {
                why: EndReason::Exited(Some(0)),
                ..
            }
        ));
    }

    #[test]
    fn a_lost_host_is_retried_with_backoff_and_the_same_terminal_reattached() {
        let mut m = Model::new(Want::Any, (100, 30));
        attached(&mut m);
        m.on_control(&json!({"type": "event", "seq": 5, "event": "host.stopping",
            "data": {"reason": "stop_requested"}}));
        let fx = m.on_disconnected();
        assert_eq!(fx[0], Effect::Reconnect(Duration::from_millis(500)));
        let Phase::Disconnected { last, reason, .. } = &m.phase else {
            panic!()
        };
        assert_eq!(last.as_ref().unwrap().id, "t-a");
        assert_eq!(reason, "the Host was stopped");
        assert!(m.phase.shows_pane(), "the last screen stays");
        // Attempts fail (no Host) with growing delays, capped at 10 s.
        let mut delays = Vec::new();
        for _ in 0..7 {
            for e in m.on_connect_failed("host_not_running", "no Host", None) {
                if let Effect::Reconnect(d) = e {
                    delays.push(d.as_millis());
                }
            }
        }
        assert_eq!(delays, [1000, 2000, 4000, 8000, 10000, 10000, 10000]);
        // A Host serves the endpoint again and the Terminal still runs: reattached by id.
        let fx = connect_with(&mut m, json!([term("t-a", "build", "running", 11)]));
        let s = sent(&fx);
        assert_eq!(s[0].1, "pane.attach");
        assert_eq!(s[0].2["target"], "t-a");
        // If it is gone, the chooser says so.
        let mut m = Model::new(Want::Any, (100, 30));
        attached(&mut m);
        m.on_disconnected();
        let fx = connect_with(&mut m, json!([term("t-x", "other", "running", 3)]));
        assert!(sent(&fx).is_empty());
        let Phase::Choosing { notice, .. } = &m.phase else {
            panic!("{:?}", m.phase)
        };
        assert_eq!(notice.as_deref(), Some("`build` is no longer running"));
    }

    #[test]
    fn a_failed_first_connection_waits_for_the_user() {
        let mut m = Model::new(Want::Any, (100, 30));
        let fx = m.on_connect_failed(
            "host_identity_mismatch",
            "not a Subak Host",
            Some("use --endpoint"),
        );
        assert_eq!(fx, [Effect::Phase]);
        assert_eq!(m.phase.name(), "failed");
        assert_eq!(m.activate(), [Effect::Connect, Effect::Phase]);
    }
}
