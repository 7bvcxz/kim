//! The Terminal manager's side of the Host conversation and its list, as a state machine without
//! I/O (docs/SUBAK_GUI.md §11.4): told what arrived (a connection, a response, an event) and what
//! the user did (search, move, open, new, rename, delete, answer), it answers with requests to
//! send and what the floating UI must do. The Win32 module does the pipe and the drawing.
//!
//! Rules (SESSION_MODEL §4, ADR 0020, ADR 0045, `SUBAK-FR-003`, `SUBAK-FR-014`,
//! `SUBAK-FR-030`, `SUBAK-FR-032`):
//!
//! - **It lists every Terminal of the Host**, also the ones no window shows (a hidden tab, or no
//!   window at all), in the Host's order, with its state, its program, whether it is busy and
//!   how many clients show it; it follows every client's create, rename, state change and
//!   removal through the `terminals` events.
//! - **It never starts a Host by itself.** It connects without starting one; without a Host it
//!   shows "not running" and retries with a growing delay (0.5 s … 10 s). Only an explicit
//!   "New Terminal" starts a Host on demand, as `subak new` does.
//! - **Opening is never destructive**: choosing a Terminal asks a window to show it.
//! - **Delete is guarded and by id.** Every delete asks first, naming what ends, with Cancel the
//!   default. A running Terminal is ended with `terminal.kill` by id — with `force` only when the
//!   question named it busy; if it turned busy since, the Host refuses (`busy`) and the manager
//!   asks again with the Host's details. A stopped or failed one is removed with
//!   `terminal.remove` (no `force`: the Host refuses anything running).
//! - Closing the manager or the icon sends nothing: the connection's end is a detach.

use super::art::{Claude, HostState, IconState};
use crate::gui::model::reconnect_delay;
use crate::gui::screens::Choice;
use crate::gui::tabs::Status;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

/// A Terminal as the manager lists it (`terminal.list` with `detail: "full"`, the `terminal`
/// of `terminal.create` and `terminal.created`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// `starting`, `running`, `stopping`, `stopped` or `failed`.
    pub state: String,
    /// The pane's program file name (redacted by the Host), e.g. `cmd.exe`.
    pub program: String,
    /// It runs more than an idle shell (the Host's busy rule, SESSION_MODEL §4).
    pub busy: bool,
    /// Processes in its pane's job.
    pub processes: u64,
    /// Clients attached to it (windows that show it, `subak attach`).
    pub clients: u64,
    /// Its aggregate Claude state (`none` until the Host reports Claude states, Steps 26–28).
    pub claude: Claude,
}

impl Entry {
    pub fn from_full(t: &Value) -> Option<Self> {
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
            program,
            busy: t["busy"].as_bool().unwrap_or(false),
            processes: pane["processes"].as_u64().unwrap_or(0),
            clients: t["clients"]
                .as_u64()
                .or_else(|| pane["clients"].as_u64())
                .unwrap_or(0),
            claude: t["claude_state"]
                .as_str()
                .and_then(Claude::parse)
                .unwrap_or(Claude::None),
        })
    }

    pub fn status(&self) -> Status {
        Status::from_state(&self.state)
    }

    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// Kept by the Host without a process (`stopped`, `failed`).
    pub fn stored(&self) -> bool {
        matches!(self.state.as_str(), "stopped" | "failed")
    }

    /// The row's detail after the name: the state in words and what it runs, e.g.
    /// "running · cmd.exe · 1 window" or "running · busy: PING.EXE (2 processes)".
    pub fn detail(&self) -> String {
        let mut d = self.status().word().to_string();
        if self.running() || self.state == "starting" || self.state == "stopping" {
            if self.busy {
                d.push_str(&format!(
                    " · busy: {}{}",
                    if self.program.is_empty() {
                        "a program"
                    } else {
                        &self.program
                    },
                    if self.processes > 1 {
                        format!(" ({} processes)", self.processes)
                    } else {
                        String::new()
                    }
                ));
            } else if !self.program.is_empty() {
                d.push_str(&format!(" · {}", self.program));
            }
        }
        match self.clients {
            0 => {}
            1 => d.push_str(" · shown in 1 window"),
            n => d.push_str(&format!(" · shown in {n} windows")),
        }
        if self.claude != Claude::None {
            d.push_str(&format!(" · Claude {}", self.claude.as_str()));
        }
        d
    }

    /// The row's text for assistive technology (the list box item string, the UIA name):
    /// "build, running · cmd.exe · shown in 1 window".
    pub fn accessible(&self) -> String {
        format!("{}, {}", self.name, self.detail())
    }

    fn matches(&self, q: &str) -> bool {
        q.is_empty()
            || fold(&self.name).contains(q)
            || self.id.starts_with(q)
            || fold(&self.program).contains(q)
            || self.state.starts_with(q)
    }
}

/// Search folding: Unicode NFC (composed and decomposed Hangul are the same text) and lower case.
pub fn fold(s: &str) -> String {
    s.nfc().collect::<String>().to_lowercase()
}

/// The connection to the Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Connecting,
    Connected,
    /// No Host serves the endpoint (or it went away).
    NoHost {
        reason: String,
    },
    /// The endpoint is held by something that is not this user's Subak Host, or another failure.
    Failed {
        code: String,
        message: String,
    },
}

impl Conn {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::NoHost { .. } => "no-host",
            Self::Failed { .. } => "failed",
        }
    }
}

/// What a delete question asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// End a live Terminal: its program, the processes in its job, and whether it is busy (then
    /// consent sends `force`).
    End {
        program: String,
        processes: u64,
        busy: bool,
    },
    /// Remove a stored Terminal's record (nothing runs in it).
    Remove,
}

/// The delete question (Cancel is the default answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub id: String,
    pub name: String,
    pub kind: AskKind,
    pub focus: Choice,
}

impl Ask {
    /// The question in words (DESIGN_SYSTEM §3.8: it names exactly what ends).
    pub fn text(&self) -> String {
        match &self.kind {
            AskKind::End {
                program,
                processes,
                busy: true,
            } => format!(
                "End `{}`? It runs {}{}; they will be ended.",
                self.name,
                if program.is_empty() {
                    "a program"
                } else {
                    program
                },
                if *processes > 1 {
                    format!(" ({processes} processes)")
                } else {
                    String::new()
                }
            ),
            AskKind::End { program, .. } => format!(
                "End `{}`? Its shell{} will be closed.",
                self.name,
                if program.is_empty() {
                    String::new()
                } else {
                    format!(" ({program})")
                }
            ),
            AskKind::Remove => format!(
                "Remove `{}`? Nothing runs in it; its record is deleted.",
                self.name
            ),
        }
    }

    /// The destructive button's label.
    pub fn action(&self) -> &'static str {
        match self.kind {
            AskKind::End { .. } => "End Terminal",
            AskKind::Remove => "Remove",
        }
    }
}

/// What the manager is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// The selected Terminal's name is an edit field.
    Rename {
        id: String,
        before: String,
    },
    /// A delete question is open.
    Ask(Ask),
}

/// What the floating UI must do after the model handled something.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send this control message (a request with its id).
    Send(Value),
    /// The list, the selection, the mode, the status or the icon state changed: redraw.
    Changed,
    /// Connect again after this delay (never starting a Host).
    Reconnect(Duration),
    /// Connect now; `start`: start a Host on demand (only for an explicit New Terminal).
    Connect { start: bool },
    /// Show this Terminal in a window (an existing window of this Host, or a new one); `None`:
    /// just bring a window up.
    Open(Option<Entry>),
    /// Tell the user something in the manager's status line.
    Notice(String),
    /// The rename finished (`Err`: why it was refused; the field stays open).
    Renamed(Result<(), String>),
}

#[derive(Debug, Clone)]
enum Pending {
    Subscribe,
    List,
    Create,
    Rename,
    Kill {
        id: String,
        name: String,
        force: bool,
    },
    Remove {
        id: String,
        name: String,
    },
}

/// The model.
#[derive(Debug)]
pub struct Manager {
    entries: Vec<Entry>,
    pub conn: Conn,
    pub mode: Mode,
    filter: String,
    selected: Option<String>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// A New Terminal waiting for the Host this manager is starting.
    create_after_connect: bool,
    attempt: u32,
    host_stopping: Option<String>,
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            conn: Conn::Connecting,
            mode: Mode::Browse,
            filter: String::new(),
            selected: None,
            next_id: 1,
            pending: HashMap::new(),
            create_after_connect: false,
            attempt: 0,
            host_stopping: None,
        }
    }

    pub fn connected(&self) -> bool {
        self.conn == Conn::Connected
    }

    /// Every Terminal, in the Host's order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The Terminals the search lets through, in the Host's order.
    pub fn visible(&self) -> Vec<&Entry> {
        let q = fold(self.filter.trim());
        self.entries.iter().filter(|e| e.matches(&q)).collect()
    }

    /// The selected Terminal (always a visible one, or none).
    pub fn selected(&self) -> Option<&Entry> {
        let id = self.selected.as_ref()?;
        self.visible().into_iter().find(|e| &e.id == id)
    }

    /// The selected row's index among the visible ones.
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected.as_ref()?;
        self.visible().iter().position(|e| &e.id == id)
    }

    /// What the icon shows.
    pub fn icon_state(&self) -> IconState {
        IconState {
            host: match self.conn {
                Conn::Connected => HostState::Connected,
                Conn::Connecting => HostState::Connecting,
                Conn::NoHost { .. } | Conn::Failed { .. } => HostState::NoHost,
            },
            terminals: self.entries.len(),
            running: self.entries.iter().filter(|e| e.running()).count(),
            failed: self.entries.iter().filter(|e| e.state == "failed").count(),
            claude: super::art::aggregate(self.entries.iter().map(|e| e.claude)),
        }
    }

    /// The header's summary, e.g. "3 Terminals · 2 running · 1 stopped".
    pub fn summary(&self) -> String {
        match &self.conn {
            Conn::Connecting => return "Connecting…".into(),
            Conn::NoHost { .. } => return "Host not running".into(),
            Conn::Failed { code, .. } => return format!("Host unreachable ({code})"),
            Conn::Connected => {}
        }
        let n = self.entries.len();
        if n == 0 {
            return "No Terminals".into();
        }
        let mut s = format!("{n} Terminal{}", if n == 1 { "" } else { "s" });
        for (state, word) in [
            ("running", "running"),
            ("starting", "starting"),
            ("stopped", "stopped"),
            ("failed", "failed"),
        ] {
            let k = self.entries.iter().filter(|e| e.state == state).count();
            if k > 0 {
                s.push_str(&format!(" · {k} {word}"));
            }
        }
        s
    }

    /// The sentence that stands in for an empty list (never a blank window, DESIGN_SYSTEM
    /// §3.8), naming New Terminal's keys as bound; `None` when rows are shown.
    pub fn empty_text(&self, new_keys: Option<&str>) -> Option<String> {
        let keys = new_keys.map_or_else(String::new, |k| format!(" ({k})"));
        match &self.conn {
            Conn::Connecting => Some("Connecting to the Subak Host…".into()),
            Conn::NoHost { reason } => Some(format!(
                "The Subak Host is not running ({reason}). New Terminal{keys} starts it with a Terminal."
            )),
            Conn::Failed { code, message } => Some(format!(
                "Cannot reach the Subak Host: {message} ({code}). It is retried in a moment."
            )),
            Conn::Connected if self.entries.is_empty() => {
                Some(format!("No Terminals yet. New Terminal{keys} creates one."))
            }
            Conn::Connected if self.visible().is_empty() => {
                Some(format!("No Terminal matches “{}”.", self.filter.trim()))
            }
            Conn::Connected => None,
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

    fn list(&mut self) -> Effect {
        self.request("terminal.list", json!({"detail": "full"}), Pending::List)
    }

    /// Keeps the selection on a visible row: the same Terminal if it is still visible, else the
    /// first visible one.
    fn fix_selection(&mut self) {
        let visible: Vec<String> = self.visible().iter().map(|e| e.id.clone()).collect();
        if !self.selected.as_ref().is_some_and(|s| visible.contains(s)) {
            self.selected = visible.first().cloned();
        }
    }

    fn upsert(&mut self, e: Entry) {
        match self.entries.iter_mut().find(|x| x.id == e.id) {
            Some(x) => *x = e,
            None => self.entries.push(e),
        }
    }

    fn drop_entry(&mut self, id: &str) {
        self.entries.retain(|e| e.id != id);
        if matches!(&self.mode, Mode::Rename { id: r, .. } if r == id)
            || matches!(&self.mode, Mode::Ask(a) if a.id == id)
        {
            self.mode = Mode::Browse;
        }
        self.fix_selection();
    }

    // -----------------------------------------------------------------------------------------
    // The connection.

    /// A connection is up: subscribe, then list (and create, when New Terminal started the Host).
    pub fn on_connected(&mut self) -> Vec<Effect> {
        self.conn = Conn::Connected;
        self.attempt = 0;
        self.pending.clear();
        self.host_stopping = None;
        let mut out = vec![
            self.request(
                "events.subscribe",
                json!({"topics": ["terminals", "host"]}),
                Pending::Subscribe,
            ),
            self.list(),
        ];
        if std::mem::take(&mut self.create_after_connect) {
            out.push(self.request("terminal.create", json!({}), Pending::Create));
        }
        out.push(Effect::Changed);
        out
    }

    /// A connection attempt failed: without a Host, "not running"; else the failure. Either way
    /// it is retried later, never starting a Host.
    pub fn on_connect_failed(&mut self, code: &str, message: &str) -> Vec<Effect> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.create_after_connect) {
            out.push(Effect::Notice(format!(
                "No Terminal was created: {message} ({code})"
            )));
        }
        self.conn = if code == "host_not_running" {
            Conn::NoHost {
                reason: "no Host serves this endpoint".into(),
            }
        } else {
            Conn::Failed {
                code: code.into(),
                message: message.into(),
            }
        };
        self.entries.clear();
        self.mode = Mode::Browse;
        self.fix_selection();
        self.attempt = self.attempt.saturating_add(1);
        out.push(Effect::Reconnect(reconnect_delay(self.attempt)));
        out.push(Effect::Changed);
        out
    }

    /// The connection ended. The Host's Terminals end with it (ADR 0003), so the list empties.
    pub fn on_disconnected(&mut self) -> Vec<Effect> {
        self.pending.clear();
        self.entries.clear();
        self.mode = Mode::Browse;
        self.fix_selection();
        let reason = match self.host_stopping.take().as_deref() {
            Some("idle") => "it exited when idle",
            Some(_) => "it was stopped",
            None => "the connection was lost",
        };
        self.conn = Conn::NoHost {
            reason: reason.into(),
        };
        self.attempt = 0;
        vec![Effect::Reconnect(reconnect_delay(0)), Effect::Changed]
    }

    /// The user is looking (the manager opened, the icon was clicked or hovered): refresh the
    /// list, or retry the connection at once.
    pub fn refresh(&mut self) -> Vec<Effect> {
        if self.connected() {
            vec![self.list()]
        } else {
            vec![Effect::Reconnect(Duration::ZERO)]
        }
    }

    // -----------------------------------------------------------------------------------------
    // The list.

    pub fn set_filter(&mut self, text: &str) -> Vec<Effect> {
        if self.filter == text {
            return Vec::new();
        }
        self.filter = text.to_string();
        self.fix_selection();
        vec![Effect::Changed]
    }

    /// Moves the selection by `delta` visible rows, stopping at the ends (`isize::MIN`/`MAX`:
    /// the first/last row).
    pub fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let ids: Vec<String> = self.visible().iter().map(|e| e.id.clone()).collect();
        if ids.is_empty() {
            return Vec::new();
        }
        let cur = self.selected_index().map_or(-1, usize::cast_signed);
        let next = if delta == isize::MIN {
            0
        } else if delta == isize::MAX {
            ids.len() - 1
        } else {
            (cur.saturating_add(delta)).clamp(0, ids.len().cast_signed() - 1) as usize
        };
        if self.selected.as_ref() == Some(&ids[next]) {
            return Vec::new();
        }
        self.selected = Some(ids[next].clone());
        vec![Effect::Changed]
    }

    /// Selects the visible row `i` (a click).
    pub fn select_index(&mut self, i: usize) -> Vec<Effect> {
        let Some(id) = self.visible().get(i).map(|e| e.id.clone()) else {
            return Vec::new();
        };
        if self.selected.as_ref() == Some(&id) {
            return Vec::new();
        }
        self.selected = Some(id);
        vec![Effect::Changed]
    }

    // -----------------------------------------------------------------------------------------
    // Actions.

    /// Open: show the selected Terminal in a window.
    pub fn open_selected(&mut self) -> Vec<Effect> {
        match self.selected().cloned() {
            Some(e) => vec![Effect::Open(Some(e))],
            None => Vec::new(),
        }
    }

    /// New Terminal: created with the Host's defaults, selected and opened in a window. Without
    /// a Host, one is started on demand first (the only path that starts a Host).
    pub fn create(&mut self) -> Vec<Effect> {
        if self.connected() {
            return vec![self.request("terminal.create", json!({}), Pending::Create)];
        }
        self.create_after_connect = true;
        self.conn = Conn::Connecting;
        vec![
            Effect::Connect { start: true },
            Effect::Notice("Starting the Subak Host…".into()),
            Effect::Changed,
        ]
    }

    /// Rename: the selected Terminal's name becomes an edit field.
    pub fn start_rename(&mut self) -> Vec<Effect> {
        let Some(e) = self.selected().cloned() else {
            return Vec::new();
        };
        if !self.connected() {
            return vec![Effect::Notice(
                "Not connected to the Subak Host; nothing can be renamed".into(),
            )];
        }
        self.mode = Mode::Rename {
            id: e.id,
            before: e.name,
        };
        vec![Effect::Changed]
    }

    /// The rename field's Enter: `terminal.rename` by id (an unchanged name just closes it).
    pub fn commit_rename(&mut self, name: &str) -> Vec<Effect> {
        let Mode::Rename { id, before } = self.mode.clone() else {
            return Vec::new();
        };
        if name == before {
            self.mode = Mode::Browse;
            return vec![Effect::Renamed(Ok(())), Effect::Changed];
        }
        if !self.connected() {
            return vec![Effect::Renamed(Err(
                "not connected to the Subak Host".into()
            ))];
        }
        vec![self.request(
            "terminal.rename",
            json!({"target": id, "name": name}),
            Pending::Rename,
        )]
    }

    pub fn cancel_rename(&mut self) -> Vec<Effect> {
        if matches!(self.mode, Mode::Rename { .. }) {
            self.mode = Mode::Browse;
            return vec![Effect::Changed];
        }
        Vec::new()
    }

    /// Delete: asks first (Cancel focused), naming what ends.
    pub fn ask_delete(&mut self) -> Vec<Effect> {
        let Some(e) = self.selected().cloned() else {
            return Vec::new();
        };
        if !self.connected() {
            return vec![Effect::Notice(
                "Not connected to the Subak Host; nothing was deleted".into(),
            )];
        }
        let kind = if e.stored() {
            AskKind::Remove
        } else {
            AskKind::End {
                program: e.program.clone(),
                processes: e.processes,
                busy: e.busy,
            }
        };
        self.mode = Mode::Ask(Ask {
            id: e.id,
            name: e.name,
            kind,
            focus: Choice::Cancel,
        });
        vec![Effect::Changed]
    }

    /// Moves the question's focus (Tab, arrows).
    pub fn ask_focus(&mut self, c: Choice) -> Vec<Effect> {
        if let Mode::Ask(a) = &mut self.mode
            && a.focus != c
        {
            a.focus = c;
            return vec![Effect::Changed];
        }
        Vec::new()
    }

    /// The answer to the question: Cancel ends nothing; the destructive choice sends exactly one
    /// request for exactly that Terminal, by id.
    pub fn answer(&mut self, c: Choice) -> Vec<Effect> {
        let Mode::Ask(a) = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return Vec::new();
        };
        if c == Choice::Cancel {
            return vec![Effect::Changed];
        }
        if !self.connected() || !self.entries.iter().any(|e| e.id == a.id) {
            return vec![
                Effect::Notice(format!(
                    "`{}` is no longer there; nothing was ended",
                    a.name
                )),
                Effect::Changed,
            ];
        }
        let fx = match a.kind {
            AskKind::End { busy, .. } => self.request(
                "terminal.kill",
                json!({"target": a.id, "force": busy}),
                Pending::Kill {
                    id: a.id.clone(),
                    name: a.name.clone(),
                    force: busy,
                },
            ),
            AskKind::Remove => self.request(
                "terminal.remove",
                json!({"target": a.id}),
                Pending::Remove {
                    id: a.id.clone(),
                    name: a.name.clone(),
                },
            ),
        };
        vec![fx, Effect::Changed]
    }

    // -----------------------------------------------------------------------------------------
    // Messages from the Host.

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
                self.entries = r["terminals"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Entry::from_full)
                    .collect();
                if let Mode::Ask(a) = &self.mode
                    && !self.entries.iter().any(|e| e.id == a.id)
                {
                    self.mode = Mode::Browse;
                }
                self.fix_selection();
                out.push(Effect::Changed);
            }
            Pending::Create => {
                if let Some(e) = Entry::from_full(&r["terminal"]) {
                    self.upsert(e.clone());
                    // The new Terminal is shown, even if the search would hide it.
                    if !self.visible().iter().any(|x| x.id == e.id) {
                        self.filter.clear();
                    }
                    self.selected = Some(e.id.clone());
                    out.push(Effect::Open(Some(e)));
                    out.push(Effect::Changed);
                }
            }
            Pending::Rename => {
                if let (Mode::Rename { id, .. }, Some(name)) = (&self.mode, r["name"].as_str()) {
                    let id = id.clone();
                    if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
                        e.name = name.to_string();
                    }
                }
                self.mode = Mode::Browse;
                out.push(Effect::Renamed(Ok(())));
                out.push(Effect::Changed);
            }
            Pending::Kill { id, .. } | Pending::Remove { id, .. } => {
                self.drop_entry(&id);
                out.push(Effect::Changed);
            }
            Pending::Subscribe => {}
        }
        out
    }

    fn on_error(&mut self, p: Pending, e: &Value) -> Vec<Effect> {
        let code = e["code"].as_str().unwrap_or("internal_error");
        let message = e["message"].as_str().unwrap_or("request failed");
        let mut out = Vec::new();
        match p {
            Pending::List => out.push(Effect::Notice(format!(
                "The Terminals could not be listed: {message} ({code})"
            ))),
            Pending::Create => out.push(Effect::Notice(format!(
                "No Terminal was created: {message} ({code})"
            ))),
            Pending::Rename => out.push(Effect::Renamed(Err(format!("{message} ({code})")))),
            Pending::Kill { id, name, force } => match code {
                // It turned busy after the question said it was idle: ask again, with the Host's
                // details (ADR 0020 §3).
                "busy" if !force => {
                    self.mode = Mode::Ask(Ask {
                        id,
                        name: name.clone(),
                        kind: AskKind::End {
                            program: e["details"]["program"].as_str().unwrap_or("").to_string(),
                            processes: e["details"]["processes"].as_u64().unwrap_or(0),
                            busy: true,
                        },
                        focus: Choice::Cancel,
                    });
                    out.push(Effect::Notice(format!(
                        "`{name}` runs more than its shell now; nothing was ended yet"
                    )));
                    out.push(Effect::Changed);
                }
                "target_not_found" => {
                    self.drop_entry(&id);
                    out.push(Effect::Changed);
                }
                _ => out.push(Effect::Notice(format!(
                    "`{name}` was not ended: {message} ({code})"
                ))),
            },
            Pending::Remove { id, name } => match code {
                "target_not_found" => {
                    self.drop_entry(&id);
                    out.push(Effect::Changed);
                }
                _ => out.push(Effect::Notice(format!(
                    "`{name}` was not removed: {message} ({code})"
                ))),
            },
            Pending::Subscribe => {}
        }
        out
    }

    fn on_event(&mut self, name: &str, d: &Value) -> Vec<Effect> {
        let tid = d["terminal_id"]
            .as_str()
            .or_else(|| d["terminal"]["id"].as_str())
            .unwrap_or("")
            .to_string();
        match name {
            "terminal.created" => {
                if let Some(e) = Entry::from_full(&d["terminal"]) {
                    self.upsert(e);
                    self.fix_selection();
                    return vec![Effect::Changed];
                }
            }
            "terminal.renamed" => {
                if let (Some(e), Some(n)) = (
                    self.entries.iter_mut().find(|e| e.id == tid),
                    d["name"].as_str(),
                ) {
                    e.name = n.to_string();
                    if let Mode::Ask(a) = &mut self.mode
                        && a.id == tid
                    {
                        a.name = n.to_string();
                    }
                    self.fix_selection();
                    return vec![Effect::Changed];
                }
            }
            "terminal.state_changed" => {
                if let (Some(e), Some(s)) = (
                    self.entries.iter_mut().find(|e| e.id == tid),
                    d["state"].as_str(),
                ) {
                    e.state = s.to_string();
                    // A question about ending a live Terminal no longer fits a stored one.
                    if let Mode::Ask(a) = &self.mode
                        && a.id == tid
                        && matches!(a.kind, AskKind::End { .. })
                        && matches!(s, "stopped" | "failed")
                    {
                        self.mode = Mode::Browse;
                    }
                    return vec![Effect::Changed];
                }
            }
            "terminal.removed" => {
                if self.entries.iter().any(|e| e.id == tid) {
                    self.drop_entry(&tid);
                    return vec![Effect::Changed];
                }
            }
            "host.stopping" => self.host_stopping = d["reason"].as_str().map(String::from),
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(id: &str, name: &str, state: &str, busy: bool, clients: u64) -> Value {
        json!({
            "id": id, "name": name, "state": state, "busy": busy, "clients": clients,
            "windows": [{"panes": [{
                "pid": 10, "executable": if busy { "C:\\Windows\\System32\\PING.EXE" }
                    else { "C:\\Windows\\System32\\cmd.exe" },
                "processes": if busy { 2 } else { 1 },
            }]}]
        })
    }

    fn sent(fx: &[Effect]) -> Vec<Value> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Send(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn respond(m: &mut Manager, req: &Value, ok: bool, body: Value) -> Vec<Effect> {
        let mut r = json!({"type": "response", "id": req["id"], "ok": ok});
        r[if ok { "result" } else { "error" }] = body;
        m.on_control(&r)
    }

    /// A connected manager listing `list`.
    fn connected(list: Vec<Value>) -> Manager {
        let mut m = Manager::new();
        let fx = m.on_connected();
        let reqs = sent(&fx);
        assert_eq!(reqs[0]["method"], "events.subscribe");
        assert_eq!(reqs[0]["params"]["topics"], json!(["terminals", "host"]));
        assert_eq!(reqs[1]["method"], "terminal.list");
        respond(&mut m, &reqs[1], true, json!({"terminals": list}));
        m
    }

    #[test]
    fn no_host_is_a_state_with_words_and_retries_without_starting_one() {
        let mut m = Manager::new();
        assert_eq!(m.icon_state().host, HostState::Connecting);
        assert_eq!(m.empty_text(None).unwrap(), "Connecting to the Subak Host…");
        let fx = m.on_connect_failed("host_not_running", "no Subak Host");
        assert!(fx.contains(&Effect::Reconnect(reconnect_delay(1))));
        assert!(
            !fx.iter()
                .any(|e| matches!(e, Effect::Connect { start: true }))
        );
        assert_eq!(m.conn.name(), "no-host");
        assert_eq!(m.icon_state().host, HostState::NoHost);
        assert_eq!(m.summary(), "Host not running");
        let t = m.empty_text(Some("Ctrl+N")).unwrap();
        assert!(t.contains("not running") && t.contains("(Ctrl+N)"), "{t}");
        // Retries grow to 10 s and never start a Host.
        for _ in 0..10 {
            let fx = m.on_connect_failed("host_not_running", "x");
            assert!(fx.iter().all(|e| !matches!(e, Effect::Connect { .. })));
        }
        assert!(
            m.on_connect_failed("host_not_running", "x")
                .contains(&Effect::Reconnect(Duration::from_secs(10)))
        );
        // Looking at it retries at once.
        assert_eq!(m.refresh(), vec![Effect::Reconnect(Duration::ZERO)]);
        // Another failure is reported as such.
        m.on_connect_failed("host_identity_mismatch", "an impostor holds the pipe");
        assert!(
            m.empty_text(None)
                .unwrap()
                .contains("host_identity_mismatch")
        );
        // Nothing to open, rename or delete.
        assert!(m.open_selected().is_empty());
        assert!(m.start_rename().is_empty());
        assert!(m.ask_delete().is_empty());
    }

    #[test]
    fn new_terminal_without_a_host_starts_one_then_creates_and_opens_it() {
        let mut m = Manager::new();
        m.on_connect_failed("host_not_running", "none");
        let fx = m.create();
        assert!(fx.contains(&Effect::Connect { start: true }));
        assert_eq!(m.conn, Conn::Connecting);
        let fx = m.on_connected();
        let reqs = sent(&fx);
        assert_eq!(reqs[2]["method"], "terminal.create");
        assert_eq!(reqs[2]["params"], json!({}));
        respond(&mut m, &reqs[1], true, json!({"terminals": []}));
        let fx = respond(
            &mut m,
            &reqs[2],
            true,
            json!({"terminal": term("t-1", "1", "starting", false, 0)}),
        );
        let Some(Effect::Open(Some(e))) = fx.iter().find(|e| matches!(e, Effect::Open(_))) else {
            panic!("{fx:?}")
        };
        assert_eq!(e.id, "t-1");
        assert_eq!(m.selected().unwrap().id, "t-1");
        // A Host that cannot start: the create is dropped and said so.
        let mut m = Manager::new();
        m.create();
        let fx = m.on_connect_failed("spawn_failed", "subak.exe not found");
        assert!(
            fx.iter()
                .any(|e| matches!(e, Effect::Notice(n) if n.contains("No Terminal was created")))
        );
        // Connected: one request, nothing started.
        let mut m = connected(vec![]);
        let fx = m.create();
        assert_eq!(sent(&fx)[0]["method"], "terminal.create");
        assert!(!fx.iter().any(|e| matches!(e, Effect::Connect { .. })));
    }

    #[test]
    fn the_list_has_every_terminal_with_state_words_and_follows_other_clients() {
        let mut m = connected(vec![
            term("t-a", "build", "running", false, 1),
            term("t-b", "서버", "running", true, 0),
            term("t-c", "old", "stopped", false, 0),
        ]);
        assert_eq!(m.entries().len(), 3);
        assert_eq!(m.selected().unwrap().id, "t-a", "the first row is selected");
        assert_eq!(m.summary(), "3 Terminals · 2 running · 1 stopped");
        assert_eq!(m.empty_text(None), None);
        let e = &m.entries()[0];
        assert_eq!(e.detail(), "running · cmd.exe · shown in 1 window");
        assert_eq!(
            e.accessible(),
            "build, running · cmd.exe · shown in 1 window"
        );
        assert_eq!(
            m.entries()[1].detail(),
            "running · busy: PING.EXE (2 processes)"
        );
        assert_eq!(m.entries()[2].detail(), "stopped");
        assert_eq!(m.entries()[2].status().glyph(), '■');
        let s = m.icon_state();
        assert_eq!((s.terminals, s.running, s.failed), (3, 2, 0));
        // Another client creates, renames, stops and kills.
        let ev = |m: &mut Manager, name: &str, data: Value| {
            m.on_control(&json!({"type": "event", "event": name, "data": data}))
        };
        ev(
            &mut m,
            "terminal.created",
            json!({"terminal": term("t-d", "new", "starting", false, 0)}),
        );
        assert_eq!(m.entries().len(), 4);
        assert_eq!(
            m.selected().unwrap().id,
            "t-a",
            "a new Terminal does not take the selection"
        );
        ev(
            &mut m,
            "terminal.renamed",
            json!({"terminal_id": "t-a", "name": "빌드"}),
        );
        assert_eq!(m.entries()[0].name, "빌드");
        ev(
            &mut m,
            "terminal.state_changed",
            json!({"terminal_id": "t-d", "state": "failed"}),
        );
        assert_eq!(m.icon_state().failed, 1);
        ev(&mut m, "terminal.removed", json!({"terminal_id": "t-a"}));
        assert_eq!(m.selected().unwrap().id, "t-b", "the selection moves on");
        // The Host goes away: the list empties, "not running", a retry is scheduled.
        ev(&mut m, "host.stopping", json!({"reason": "stop_requested"}));
        let fx = m.on_disconnected();
        assert!(fx.contains(&Effect::Reconnect(reconnect_delay(0))));
        assert!(m.entries().is_empty());
        assert!(m.empty_text(None).unwrap().contains("it was stopped"));
    }

    #[test]
    fn quick_search_folds_case_and_hangul_and_keeps_the_selection() {
        let mut m = connected(vec![
            term("t-a1", "Build", "running", false, 0),
            term("t-b2", "서버", "running", true, 0),
            term("t-c3", "docs", "stopped", false, 0),
        ]);
        m.move_selection(1);
        assert_eq!(m.selected().unwrap().name, "서버");
        m.set_filter("BUI");
        assert_eq!(m.visible().len(), 1);
        assert_eq!(
            m.selected().unwrap().name,
            "Build",
            "the hidden selection moves"
        );
        // Decomposed Hangul (NFD) finds the composed name.
        let nfd: String = "서".nfd().collect();
        m.set_filter(&nfd);
        assert_eq!(
            m.visible().iter().map(|e| &e.name).collect::<Vec<_>>(),
            ["서버"]
        );
        // Program, state and id prefix also match.
        m.set_filter("ping");
        assert_eq!(m.visible()[0].name, "서버");
        m.set_filter("stopped");
        assert_eq!(m.visible()[0].name, "docs");
        m.set_filter("t-c");
        assert_eq!(m.visible()[0].name, "docs");
        m.set_filter("zzz");
        assert!(m.visible().is_empty() && m.selected().is_none());
        assert_eq!(m.empty_text(None).unwrap(), "No Terminal matches “zzz”.");
        m.set_filter("");
        assert_eq!(m.visible().len(), 3);
        // Moving stops at the ends; MIN/MAX jump.
        m.move_selection(isize::MAX);
        assert_eq!(m.selected_index(), Some(2));
        assert!(m.move_selection(1).is_empty());
        m.move_selection(isize::MIN);
        assert_eq!(m.selected_index(), Some(0));
        m.select_index(1);
        assert_eq!(m.selected().unwrap().name, "서버");
        assert_eq!(
            m.open_selected(),
            vec![Effect::Open(Some(m.entries()[1].clone()))]
        );
    }

    #[test]
    fn rename_is_by_id_and_a_refusal_keeps_the_field() {
        let mut m = connected(vec![term("t-a", "build", "running", false, 0)]);
        m.start_rename();
        assert_eq!(
            m.mode,
            Mode::Rename {
                id: "t-a".into(),
                before: "build".into()
            }
        );
        let fx = m.commit_rename("빌드");
        let req = &sent(&fx)[0];
        assert_eq!(req["method"], "terminal.rename");
        assert_eq!(req["params"], json!({"target": "t-a", "name": "빌드"}));
        let fx = respond(
            &mut m,
            req,
            false,
            json!({"code": "name_conflict", "message": "in use"}),
        );
        assert_eq!(
            fx,
            vec![Effect::Renamed(Err("in use (name_conflict)".into()))]
        );
        assert!(matches!(m.mode, Mode::Rename { .. }), "the field stays");
        let fx = m.commit_rename("빌드2");
        let req = sent(&fx)[0].clone();
        respond(
            &mut m,
            &req,
            true,
            json!({"terminal_id": "t-a", "name": "빌드2"}),
        );
        assert_eq!(m.mode, Mode::Browse);
        assert_eq!(m.entries()[0].name, "빌드2");
        // An unchanged name closes the field without a request; Esc cancels.
        m.start_rename();
        assert!(sent(&m.commit_rename("빌드2")).is_empty());
        m.start_rename();
        m.cancel_rename();
        assert_eq!(m.mode, Mode::Browse);
    }

    #[test]
    fn delete_always_asks_cancel_is_the_default_and_consent_ends_only_that_id() {
        let mut m = connected(vec![
            term("t-a", "idle", "running", false, 0),
            term("t-b", "busy", "running", true, 0),
            term("t-c", "old", "stopped", false, 0),
        ]);
        // An idle shell: asked; Cancel ends nothing.
        m.ask_delete();
        let Mode::Ask(a) = m.mode.clone() else {
            panic!()
        };
        assert_eq!(a.focus, Choice::Cancel);
        assert_eq!(a.text(), "End `idle`? Its shell (cmd.exe) will be closed.");
        assert_eq!(a.action(), "End Terminal");
        assert!(sent(&m.answer(Choice::Cancel)).is_empty());
        assert_eq!(m.mode, Mode::Browse);
        // Consent for an idle one: kill without force, by id.
        m.ask_delete();
        let fx = m.answer(Choice::End);
        let req = sent(&fx)[0].clone();
        assert_eq!(req["method"], "terminal.kill");
        assert_eq!(req["params"], json!({"target": "t-a", "force": false}));
        // It turned busy meanwhile: the Host refuses and the manager asks again.
        let fx = respond(
            &mut m,
            &req,
            false,
            json!({"code": "busy", "message": "busy", "details": {"program": "claude.exe", "processes": 3}}),
        );
        assert!(fx.iter().any(|e| matches!(e, Effect::Notice(_))));
        let Mode::Ask(a) = m.mode.clone() else {
            panic!()
        };
        assert_eq!(a.id, "t-a");
        assert_eq!(
            a.text(),
            "End `idle`? It runs claude.exe (3 processes); they will be ended."
        );
        let fx = m.answer(Choice::End);
        let req = sent(&fx)[0].clone();
        assert_eq!(req["params"], json!({"target": "t-a", "force": true}));
        respond(
            &mut m,
            &req,
            true,
            json!({"removed": true, "terminal_id": "t-a"}),
        );
        assert_eq!(m.entries().len(), 2);
        // A busy one: the question names it; consent sends force by id.
        m.select_index(0);
        assert_eq!(m.selected().unwrap().id, "t-b");
        m.ask_delete();
        let Mode::Ask(a) = m.mode.clone() else {
            panic!()
        };
        assert!(a.text().contains("PING.EXE (2 processes)"));
        let req = sent(&m.answer(Choice::End))[0].clone();
        assert_eq!(req["params"], json!({"target": "t-b", "force": true}));
        // A stored one: remove (no force).
        m.select_index(1);
        m.ask_delete();
        let Mode::Ask(a) = m.mode.clone() else {
            panic!()
        };
        assert_eq!(a.kind, AskKind::Remove);
        assert_eq!(a.action(), "Remove");
        let req = sent(&m.answer(Choice::End))[0].clone();
        assert_eq!(req["method"], "terminal.remove");
        assert_eq!(req["params"], json!({"target": "t-c"}));
        // A question about a Terminal that another client removed meanwhile goes away.
        let mut m = connected(vec![term("t-x", "x", "running", false, 0)]);
        m.ask_delete();
        m.on_control(&json!({"type": "event", "event": "terminal.removed",
            "data": {"terminal_id": "t-x"}}));
        assert_eq!(m.mode, Mode::Browse);
        // Rename by another client updates the question's words.
        let mut m = connected(vec![term("t-y", "y", "running", false, 0)]);
        m.ask_delete();
        m.on_control(&json!({"type": "event", "event": "terminal.renamed",
            "data": {"terminal_id": "t-y", "name": "와이"}}));
        let Mode::Ask(a) = &m.mode else { panic!() };
        assert!(a.text().contains("`와이`"));
        // Focus moves only between the two answers.
        m.ask_focus(Choice::End);
        let Mode::Ask(a) = &m.mode else { panic!() };
        assert_eq!(a.focus, Choice::End);
    }

    #[test]
    fn claude_states_from_the_host_reach_the_rows_and_the_icon() {
        let mut t = term("t-a", "agent", "running", true, 1);
        t["claude_state"] = json!("need-input");
        let m = connected(vec![t, term("t-b", "b", "running", false, 0)]);
        assert_eq!(m.entries()[0].claude, Claude::NeedInput);
        assert!(m.entries()[0].detail().ends_with("Claude need-input"));
        assert_eq!(m.icon_state().claude, Claude::NeedInput);
        assert_eq!(m.entries()[1].claude, Claude::None);
    }
}
