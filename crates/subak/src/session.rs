//! The session model of the Host (docs/SESSION_MODEL.md, ADR 0007, ADR 0020): Terminals with
//! their internal Window and Pane, unique human-facing names, targets, and the one Terminal
//! lifecycle state machine.
//!
//! Portable: no OS calls. The Host (`host.rs`) keeps everything that is an OS resource (pseudo
//! consoles, jobs, threads, output rings) next to this model, keyed by pane id; this module decides
//! names, ids, targets and which state follows which.
//!
//! Every Terminal has exactly one Window with exactly one Pane until Steps 21–22 add more; the
//! model already has the three levels, so ids and targets do not change when they arrive.

use crate::proto::{ErrorCode, IpcError};
use crate::pty::EnvNames;
use crate::redact;
use crate::screen::HistoryInfo;
use serde_json::{Value, json};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

/// Longest Terminal name, in Unicode scalar values after NFC normalization and trimming.
pub const NAME_MAX: usize = 64;

/// Invisible formatting characters that would let two different names look identical
/// (bidirectional overrides, zero-width characters, the byte order mark). `char::is_control`
/// covers the C0/C1 controls.
const INVISIBLE: &[(char, char)] = &[
    ('\u{00AD}', '\u{00AD}'),
    ('\u{061C}', '\u{061C}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{2028}', '\u{202E}'),
    ('\u{2060}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
];

/// A validated Terminal name: what is shown, and the key names are compared by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// NFC-normalized and trimmed; the case the user typed.
    pub display: String,
    /// `display` lowercased (locale-independent Unicode mapping) and NFC-normalized again.
    pub key: String,
}

/// Whether `text` has the shape of an object id (`t-`/`w-`/`p-` and 12 hex digits), in any case.
pub fn looks_like_id(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 14
        && matches!(b[0].to_ascii_lowercase(), b't' | b'w' | b'p')
        && b[1] == b'-'
        && b[2..].iter().all(u8::is_ascii_hexdigit)
}

/// Whether `text` is an object id with `prefix` exactly as the Host issues them (lowercase).
pub fn is_id(text: &str, prefix: char) -> bool {
    looks_like_id(text)
        && text.starts_with(prefix)
        && text[2..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The comparison key of a name (see [`Name::key`]).
pub fn name_key(display: &str) -> String {
    display.to_lowercase().nfc().collect()
}

fn invalid_name(message: impl Into<String>) -> IpcError {
    IpcError::new(ErrorCode::InvalidName, message).details(json!({"field": "name"}))
}

/// Validates a Terminal name (ADR 0007, docs/SESSION_MODEL.md §3).
pub fn parse_name(raw: &str) -> Result<Name, IpcError> {
    let nfc: String = raw.nfc().collect();
    let display = nfc.trim().to_string();
    let len = display.chars().count();
    if len == 0 {
        return Err(invalid_name(
            "a Terminal name needs 1-64 visible characters",
        ));
    }
    if len > NAME_MAX {
        return Err(invalid_name(format!(
            "a Terminal name has at most {NAME_MAX} characters, this one has {len}"
        )));
    }
    if let Some(c) = display
        .chars()
        .find(|c| c.is_control() || INVISIBLE.iter().any(|(lo, hi)| (lo..=hi).contains(&c)))
    {
        return Err(invalid_name(format!(
            "a Terminal name must not contain control or invisible characters (U+{:04X})",
            u32::from(c)
        )));
    }
    if display.contains([':', '.']) {
        return Err(invalid_name(
            "a Terminal name must not contain `:` or `.` (they separate the parts of a target)",
        ));
    }
    if display.starts_with('-') {
        return Err(invalid_name(
            "a Terminal name must not start with `-` (it would read as an option)",
        ));
    }
    if looks_like_id(&display) {
        return Err(invalid_name(
            "a Terminal name must not look like an id (t-, w- or p- and 12 hex digits)",
        ));
    }
    let key = name_key(&display);
    Ok(Name { display, key })
}

/// The lifecycle state of a Terminal (docs/SESSION_MODEL.md §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Ids and name are reserved; its pane process is being started.
    Starting,
    /// Its pane process runs.
    Running,
    /// Its processes are being ended (stop, kill, Host stop, or the shell exited by itself).
    Stopping,
    /// Kept without a live process: stopped explicitly, or exited with remain-on-exit.
    Stopped,
    /// Kept after its start failed (only when the creator asked for `keep_on_failure`).
    Failed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// A live Terminal owns (or is about to own, or still owns) processes; it keeps the Host.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }
}

/// Why a Terminal's processes end. When several causes meet (a kill during a graceful stop), the
/// strongest one decides what happens to the Terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EndCause {
    /// The Host itself stops (`host.stop` with `kill_terminals`, or idle exit).
    HostStop,
    /// The pane's root process exited by itself.
    Exited,
    /// `terminal.stop`: keep the Terminal.
    Stop,
    /// `terminal.kill`, or `terminal.remove` with `force`: remove the Terminal.
    Kill,
}

impl EndCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostStop => "host_stopped",
            Self::Exited => "exited",
            Self::Stop => "stopped",
            Self::Kill => "killed",
        }
    }
}

/// What happens to a Terminal (docs/SESSION_MODEL.md §2: the state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// The pane process started.
    Spawned,
    /// The pane process could not start.
    SpawnFailed { keep: bool },
    /// Ending its processes begins.
    EndBegins,
    /// Its processes have ended for `cause`.
    Ended {
        cause: EndCause,
        remain_on_exit: bool,
    },
    /// An explicit remove of a Terminal that owns no process.
    Remove,
}

/// The state after `t`, `None` when the Terminal is removed, or `Err` when `t` is not allowed in
/// `from`. This table is the state machine; the Host applies nothing else.
pub fn next(from: State, t: Transition) -> Result<Option<State>, IpcError> {
    use State::{Failed, Running, Starting, Stopped, Stopping};
    let to = match (from, t) {
        (Starting, Transition::Spawned) => Some(Running),
        (Starting, Transition::SpawnFailed { keep: true }) => Some(Failed),
        (Starting, Transition::SpawnFailed { keep: false }) => None,
        (Running, Transition::EndBegins) => Some(Stopping),
        (
            Stopping,
            Transition::Ended {
                cause,
                remain_on_exit,
            },
        ) => match cause {
            EndCause::Stop | EndCause::HostStop => Some(Stopped),
            EndCause::Exited if remain_on_exit => Some(Stopped),
            EndCause::Exited | EndCause::Kill => None,
        },
        (Stopped | Failed, Transition::Remove) => None,
        (s, t) => {
            return Err(IpcError::new(
                ErrorCode::InvalidState,
                format!("a {} Terminal cannot {}", s.as_str(), transition_verb(t)),
            )
            .details(json!({"state": s.as_str()})));
        }
    };
    Ok(to)
}

fn transition_verb(t: Transition) -> &'static str {
    match t {
        Transition::Spawned | Transition::SpawnFailed { .. } => "finish starting",
        Transition::EndBegins => "begin to end its processes",
        Transition::Ended { .. } => "finish ending its processes",
        Transition::Remove => "be removed without ending its processes first",
    }
}

/// A pane's root command: what is shown and what decides "idle shell".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
}

/// Programs that count as a shell for the "idle shell" rule (ADR 0009): a pane whose root
/// process is one of these and has no other process in its job runs nothing that a stop would
/// interrupt. Everything else is *busy*.
const SHELLS: &[&str] = &[
    "cmd",
    "powershell",
    "pwsh",
    "bash",
    "sh",
    "zsh",
    "fish",
    "nu",
];

/// Whether `program` (a path or a bare name) is a known interactive shell.
pub fn is_shell(program: &str) -> bool {
    let base = program
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    SHELLS.contains(&base)
}

/// Whether ending a pane would interrupt work: its root is not a shell, or the shell has other
/// processes in its job.
pub fn busy(program: &str, processes: u32) -> bool {
    !is_shell(program) || processes > 1
}

/// Splits a Windows command line into its program and arguments for display and the idle-shell
/// rule (the rules of `CommandLineToArgvW`: whitespace separates, double quotes group, a
/// backslash run before a quote escapes it).
pub fn split_command_line(line: &str) -> Command {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut backslashes = 0usize;
    for c in line.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                in_word = true;
            }
            '"' => {
                cur.extend(std::iter::repeat_n('\\', backslashes / 2));
                if backslashes % 2 == 1 {
                    cur.push('"');
                } else {
                    quoted = !quoted;
                }
                backslashes = 0;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                cur.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                cur.push(c);
                in_word = true;
            }
        }
    }
    cur.extend(std::iter::repeat_n('\\', backslashes));
    if in_word {
        words.push(cur);
    }
    let mut it = words.into_iter();
    Command {
        program: it.next().unwrap_or_default(),
        args: it.collect(),
    }
}

/// Why a start failed (`spawn_failed`, IPC_PROTOCOL §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: &'static str,
    pub message: String,
}

/// How a Terminal's processes last ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastExit {
    pub pid: Option<u32>,
    pub exit_code: Option<u32>,
    pub forced: bool,
    pub cause: EndCause,
}

/// One pane of a window.
#[derive(Debug, Clone)]
pub struct Pane {
    pub id: String,
    pub index: usize,
    pub command: Command,
    /// The verbatim command line it was started with (`terminal.create` `command_line`, or the
    /// `--shell` line of `host serve`); `None` when `command` was quoted word by word.
    pub command_line: Option<String>,
    /// The working directory it was started in.
    pub cwd: Option<String>,
    /// The program file that runs: `command.program` resolved on the pane's `PATH` (Step 11).
    /// `None` until it started.
    pub executable: Option<String>,
    /// The names (never the values) of the environment overrides it was started with.
    pub env: EnvNames,
    pub size: (u16, u16),
    /// The data-frame stream id of its output (unique per Host).
    pub stream: u32,
    /// The root process while it runs.
    pub pid: Option<u32>,
    /// History rows its terminal model keeps (Step 13, tmux `history-limit`).
    pub history_limit: usize,
}

/// One internal window of a Terminal.
#[derive(Debug, Clone)]
pub struct Window {
    pub id: String,
    pub index: usize,
    pub name: String,
    pub automatic_name: bool,
    pub panes: Vec<Pane>,
    pub active_pane: usize,
}

/// One Terminal: a named workspace with its windows, panes and lifecycle state.
#[derive(Debug, Clone)]
pub struct Terminal {
    pub id: String,
    pub name: Name,
    pub created_at: String,
    pub state: State,
    pub remain_on_exit: bool,
    pub windows: Vec<Window>,
    pub active_window: usize,
    /// Set while its processes are being ended: the strongest cause so far.
    pub ending: Option<EndCause>,
    pub failure: Option<Failure>,
    pub last_exit: Option<LastExit>,
}

impl Terminal {
    /// The pane a Terminal-level target means: its active window's active pane.
    pub fn active_pane(&self) -> &Pane {
        let w = &self.windows[self.active_window];
        &w.panes[w.active_pane]
    }

    pub fn active_pane_mut(&mut self) -> &mut Pane {
        let w = &mut self.windows[self.active_window];
        let i = w.active_pane;
        &mut w.panes[i]
    }

    pub fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.windows.iter().flat_map(|w| w.panes.iter())
    }
}

/// What the Host knows about a pane's processes and clients right now (volatile, never stored).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Live {
    /// The root process still runs.
    pub running: bool,
    /// Processes in the pane's job.
    pub processes: u32,
    /// Connections attached to its stream.
    pub clients: usize,
    /// Output bytes produced so far.
    pub output_bytes: u64,
    /// Its history, while the Host keeps its screen (Step 13).
    pub history: Option<HistoryInfo>,
}

/// Which level of the hierarchy a target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Terminal,
    Window,
    Pane,
}

/// A resolved target: indexes into the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub terminal: usize,
    pub window: usize,
    pub pane: usize,
    pub level: Level,
}

fn not_found(target: &str) -> IpcError {
    IpcError::new(
        ErrorCode::TargetNotFound,
        format!(
            "no Terminal, window or pane matches `{}`",
            target.chars().take(80).collect::<String>()
        ),
    )
}

/// Every Terminal of one Host, in creation order (the order of GUI tabs), plus every id this
/// Host ever issued, so no id is reused for another object (ADR 0007).
#[derive(Debug, Default)]
pub struct Registry {
    pub terminals: Vec<Terminal>,
    issued: HashSet<String>,
    next_stream: u32,
}

/// What `terminal.create` asks for, after parameter validation.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: Option<Name>,
    pub command: Command,
    /// A verbatim command line; `command` is then its split, for display and the busy rule.
    pub command_line: Option<String>,
    pub cwd: Option<String>,
    /// Names of the environment overrides; their values never reach the model.
    pub env: EnvNames,
    pub size: (u16, u16),
    pub remain_on_exit: bool,
    /// History rows the pane keeps.
    pub history_limit: usize,
}

impl Registry {
    /// A fresh id with `prefix` that this Host never issued; `random` yields candidates.
    pub fn issue_id(&mut self, prefix: char, mut random: impl FnMut(char) -> String) -> String {
        loop {
            let id = random(prefix);
            if is_id(&id, prefix) && self.issued.insert(id.clone()) {
                return id;
            }
        }
    }

    #[cfg(test)]
    pub fn issued(&self, id: &str) -> bool {
        self.issued.contains(id)
    }

    /// The Terminal that uses `key`, other than `except`.
    pub fn holder(&self, key: &str, except: Option<&str>) -> Option<&Terminal> {
        self.terminals
            .iter()
            .find(|t| t.name.key == key && Some(t.id.as_str()) != except)
    }

    /// The default name: the lowest positive integer no Terminal uses (ADR 0007).
    pub fn default_name(&self) -> Name {
        let mut n = 1u64;
        loop {
            let s = n.to_string();
            if self.holder(&s, None).is_none() {
                return Name {
                    display: s.clone(),
                    key: s,
                };
            }
            n += 1;
        }
    }

    /// `name_conflict` when another Terminal uses `name` (case-insensitive, NFC).
    pub fn check_free(&self, name: &Name, except: Option<&str>) -> Result<(), IpcError> {
        match self.holder(&name.key, except) {
            None => Ok(()),
            Some(t) => Err(IpcError::new(
                ErrorCode::NameConflict,
                format!(
                    "a Terminal named `{}` already exists (names ignore case)",
                    t.name.display
                ),
            )
            .details(json!({"existing": t.id, "name": t.name.display}))),
        }
    }

    /// Reserves ids and the name of a new Terminal in state `starting`. The name is checked and
    /// taken in one step, so of several concurrent creates with one name exactly one succeeds.
    pub fn reserve(
        &mut self,
        spec: Spec,
        created_at: String,
        mut random: impl FnMut(char) -> String,
    ) -> Result<usize, IpcError> {
        let name = match spec.name {
            Some(n) => {
                self.check_free(&n, None)?;
                n
            }
            None => self.default_name(),
        };
        let tid = self.issue_id('t', &mut random);
        let wid = self.issue_id('w', &mut random);
        let pid = self.issue_id('p', &mut random);
        self.next_stream += 1;
        let window_name = window_name(&spec.command.program);
        self.terminals.push(Terminal {
            id: tid,
            name,
            created_at,
            state: State::Starting,
            remain_on_exit: spec.remain_on_exit,
            windows: vec![Window {
                id: wid,
                index: 0,
                name: window_name,
                automatic_name: true,
                panes: vec![Pane {
                    id: pid,
                    index: 0,
                    command: spec.command,
                    command_line: spec.command_line,
                    cwd: spec.cwd,
                    executable: None,
                    env: spec.env,
                    size: spec.size,
                    stream: self.next_stream,
                    pid: None,
                    history_limit: spec.history_limit,
                }],
                active_pane: 0,
            }],
            active_window: 0,
            ending: None,
            failure: None,
            last_exit: None,
        });
        Ok(self.terminals.len() - 1)
    }

    pub fn index_of(&self, terminal_id: &str) -> Option<usize> {
        self.terminals.iter().position(|t| t.id == terminal_id)
    }

    pub fn get(&self, terminal_id: &str) -> Option<&Terminal> {
        self.terminals.iter().find(|t| t.id == terminal_id)
    }

    pub fn get_mut(&mut self, terminal_id: &str) -> Option<&mut Terminal> {
        self.terminals.iter_mut().find(|t| t.id == terminal_id)
    }

    /// The Terminal that owns pane `pane_id`.
    pub fn owner_of_pane(&self, pane_id: &str) -> Option<&Terminal> {
        self.terminals
            .iter()
            .find(|t| t.panes().any(|p| p.id == pane_id))
    }

    /// The pane (and its Terminal) whose output is `stream`.
    pub fn pane_of_stream(&self, stream: u32) -> Option<(&Terminal, &Pane)> {
        self.terminals
            .iter()
            .find_map(|t| t.panes().find(|p| p.stream == stream).map(|p| (t, p)))
    }

    /// Applies a state-machine transition. Returns the new state, or `None` when the Terminal
    /// was removed (its ids stay issued).
    pub fn apply(&mut self, terminal_id: &str, t: Transition) -> Result<Option<State>, IpcError> {
        let i = self
            .index_of(terminal_id)
            .ok_or_else(|| not_found(terminal_id))?;
        let to = next(self.terminals[i].state, t)?;
        match to {
            Some(s) => self.terminals[i].state = s,
            None => {
                self.terminals.remove(i);
            }
        }
        Ok(to)
    }

    /// Resolves a target (ADR 0007): an id alone, or `<terminal>[:<window>[.<pane>]]` where the
    /// Terminal is a name or id, the window an index, name or id, the pane an index or id.
    /// Names match exactly, ignoring case (NFC); nothing matches by prefix.
    pub fn resolve(&self, target: &str) -> Result<Resolved, IpcError> {
        let bad = |why: &str| IpcError::param("target", format!("invalid target: {why}"));
        if target.is_empty() || target.chars().count() > 200 {
            return Err(bad("a target has 1-200 characters"));
        }
        if is_id(target, 'w') || is_id(target, 'p') {
            for (ti, t) in self.terminals.iter().enumerate() {
                for (wi, w) in t.windows.iter().enumerate() {
                    if w.id == target {
                        return Ok(Resolved {
                            terminal: ti,
                            window: wi,
                            pane: w.active_pane,
                            level: Level::Window,
                        });
                    }
                    if let Some(pi) = w.panes.iter().position(|p| p.id == target) {
                        return Ok(Resolved {
                            terminal: ti,
                            window: wi,
                            pane: pi,
                            level: Level::Pane,
                        });
                    }
                }
            }
            return Err(not_found(target));
        }
        let (term, rest) = match target.split_once(':') {
            Some((t, r)) => (t, Some(r)),
            None => (target, None),
        };
        let ti = if is_id(term, 't') {
            self.index_of(term)
        } else {
            let key = name_key(&term.nfc().collect::<String>());
            self.terminals.iter().position(|t| t.name.key == key)
        }
        .ok_or_else(|| not_found(target))?;
        let t = &self.terminals[ti];
        let Some(rest) = rest else {
            return Ok(Resolved {
                terminal: ti,
                window: t.active_window,
                pane: t.windows[t.active_window].active_pane,
                level: Level::Terminal,
            });
        };
        let (win, pane) = match rest.split_once('.') {
            Some((w, p)) => (w, Some(p)),
            None => (rest, None),
        };
        if win.is_empty() {
            return Err(bad("the window part after `:` is empty"));
        }
        let wi = if let Ok(n) = win.parse::<usize>() {
            t.windows.iter().position(|w| w.index == n)
        } else if is_id(win, 'w') {
            t.windows.iter().position(|w| w.id == win)
        } else {
            let matches: Vec<usize> = t
                .windows
                .iter()
                .enumerate()
                .filter(|(_, w)| w.name == win)
                .map(|(i, _)| i)
                .collect();
            if matches.len() > 1 {
                return Err(IpcError::new(
                    ErrorCode::AmbiguousTarget,
                    format!("several windows of `{}` are named `{win}`", t.name.display),
                )
                .details(json!({
                    "candidates": matches.iter().map(|&i| t.windows[i].id.clone()).collect::<Vec<_>>()
                })));
            }
            matches.first().copied()
        }
        .ok_or_else(|| not_found(target))?;
        let w = &t.windows[wi];
        let Some(pane) = pane else {
            return Ok(Resolved {
                terminal: ti,
                window: wi,
                pane: w.active_pane,
                level: Level::Window,
            });
        };
        let pi = if let Ok(n) = pane.parse::<usize>() {
            w.panes.iter().position(|p| p.index == n)
        } else if is_id(pane, 'p') {
            w.panes.iter().position(|p| p.id == pane)
        } else {
            return Err(bad("a pane is an index or a p- id"));
        }
        .ok_or_else(|| not_found(target))?;
        Ok(Resolved {
            terminal: ti,
            window: wi,
            pane: pi,
            level: Level::Pane,
        })
    }

    /// Resolves a target that must name a Terminal (a name or a `t-` id).
    pub fn resolve_terminal(&self, target: &str) -> Result<usize, IpcError> {
        let r = self.resolve(target)?;
        if r.level != Level::Terminal {
            return Err(IpcError::param(
                "target",
                "this operation takes a Terminal (its name or t- id), not a window or pane",
            ));
        }
        Ok(r.terminal)
    }
}

/// A window's automatic name: its program's file name without `.exe` (Step 21 follows the
/// foreground program).
pub fn window_name(program: &str) -> String {
    let base = program.rsplit(['\\', '/']).next().unwrap_or(program);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base);
    if base.is_empty() {
        "shell".into()
    } else {
        base.to_string()
    }
}

fn command_json(c: &Command) -> Value {
    json!({"program": redact::program(&c.program), "args": redact::args(&c.args)})
}

/// The names of a pane's environment overrides (values are never shown, CLI_CONTRACT §10).
pub fn env_json(e: &EnvNames) -> Value {
    json!({"set": e.set, "unset": e.unset})
}

/// A pane's history counters (Step 13, docs/SCROLLBACK.md): numbers only, never content.
pub fn history_json(h: &HistoryInfo) -> Value {
    json!({
        "lines": h.lines, "bytes": h.bytes,
        "limit_lines": h.limit_lines, "limit_bytes": h.limit_bytes,
        "evicted_lines": h.evicted_lines, "cleared_lines": h.cleared_lines,
        "total_lines": h.total_lines, "persisted": false,
    })
}

/// The `Terminal` object of IPC v1 (docs/SESSION_MODEL.md §5): the stored shape of
/// docs/contracts/STATE_SCHEMA.md plus volatile fields. `live` reports a pane's processes.
pub fn terminal_json(t: &Terminal, live: &dyn Fn(&Pane) -> Live) -> Value {
    let mut clients = 0;
    let mut busy = false;
    let windows: Vec<Value> = t
        .windows
        .iter()
        .map(|w| {
            let panes: Vec<Value> = w
                .panes
                .iter()
                .map(|p| {
                    let l = live(p);
                    clients += l.clients;
                    let running = t.state.is_live() && l.running;
                    busy |= running && self::busy(&p.command.program, l.processes);
                    json!({
                        "id": p.id,
                        "index": p.index,
                        "cwd": p.cwd,
                        "command": command_json(&p.command),
                        "command_line": p.command_line.as_deref().map(redact::command_line),
                        "executable": p.executable,
                        "env": env_json(&p.env),
                        "title": Value::Null,
                        "stream_id": p.stream,
                        "pid": p.pid.filter(|_| running),
                        "running": running,
                        "processes": if running { l.processes } else { 0 },
                        "size": {"cols": p.size.0, "rows": p.size.1},
                        "clients": l.clients,
                        "output_bytes": l.output_bytes,
                        "history": l.history.as_ref().map(history_json),
                    })
                })
                .collect();
            json!({
                "id": w.id,
                "index": w.index,
                "name": w.name,
                "automatic_name": w.automatic_name,
                "active_pane": w.panes[w.active_pane].id,
                "layout": {"type": "pane", "pane": w.panes[w.active_pane].id},
                "panes": panes,
            })
        })
        .collect();
    json!({
        "id": t.id,
        "name": t.name.display,
        "created_at": t.created_at,
        "state": t.state.as_str(),
        "profile": Value::Null,
        "remain_on_exit": t.remain_on_exit,
        "busy": busy,
        "clients": clients,
        "active_window": t.windows[t.active_window].id,
        "windows": windows,
        "failure": t.failure.as_ref().map(|f| json!({
            "code": "spawn_failed", "reason": f.reason, "message": f.message,
        })),
        "last_exit": t.last_exit.as_ref().map(|e| json!({
            "pid": e.pid, "exit_code": e.exit_code, "forced": e.forced, "cause": e.cause.as_str(),
        })),
    })
}

/// The summary object of `terminal.list` (`detail: "summary"`).
pub fn summary_json(t: &Terminal, live: &dyn Fn(&Pane) -> Live) -> Value {
    let p = t.active_pane();
    let l = live(p);
    let running = t.state.is_live() && l.running;
    let clients: usize = t.panes().map(|p| live(p).clients).sum();
    json!({
        "id": t.id,
        "name": t.name.display,
        "state": t.state.as_str(),
        "created_at": t.created_at,
        "busy": running && busy(&p.command.program, l.processes),
        "clients": clients,
        "windows": t.windows.len(),
        "panes": t.panes().count(),
        "pid": p.pid.filter(|_| running),
        "program": redact::program(&p.command.program),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> Name {
        parse_name(s).unwrap_or_else(|e| panic!("{s:?}: {}", e.message))
    }

    fn bad(s: &str) -> ErrorCode {
        parse_name(s).expect_err(s).code
    }

    /// Deterministic id source for tests.
    fn ids() -> impl FnMut(char) -> String {
        let mut n = 0u64;
        move |p| {
            n += 1;
            format!("{p}-{n:012x}")
        }
    }

    fn spec(name: Option<&str>) -> Spec {
        Spec {
            name: name.map(self::name),
            command: Command {
                program: r"C:\Windows\system32\cmd.exe".into(),
                args: vec![],
            },
            command_line: None,
            cwd: None,
            env: EnvNames::default(),
            size: (120, 30),
            remain_on_exit: false,
            history_limit: crate::screen::DEFAULT_HISTORY_LINES,
        }
    }

    #[test]
    fn names_follow_adr_0007() {
        assert_eq!(name("  work  ").display, "work", "trimmed");
        assert_eq!(name("Work").key, "work", "case is kept for display only");
        assert_eq!(name("서버").display, "서버");
        assert_eq!(
            name("my build 2").display,
            "my build 2",
            "inner spaces are fine"
        );
        assert_eq!(name(&"가".repeat(64)).display.chars().count(), 64);
        // NFC: a decomposed Hangul syllable and an accented letter equal their composed forms.
        assert_eq!(name("\u{1109}\u{1165}\u{1107}\u{1165}").display, "서버");
        assert_eq!(name("Cafe\u{301}").key, name("CAFÉ").key);
        for s in [
            "",
            "   ",
            "a:b",
            "a.b",
            "-x",
            "t-3fa9c0d21b7e",
            "P-ABCDEF012345",
            "w-000000000000",
            "tab\there",
            "line\nbreak",
            "bell\u{7}",
            "rtl\u{202E}txt",
            "zero\u{200B}width",
            "bom\u{FEFF}",
        ] {
            assert_eq!(bad(s), ErrorCode::InvalidName, "{s:?}");
        }
        assert_eq!(bad(&"x".repeat(65)), ErrorCode::InvalidName);
        // Not id-shaped: wrong length or not hex.
        assert_eq!(name("t-3fa9c0d21b7").display, "t-3fa9c0d21b7");
        assert_eq!(name("t-3fa9c0d21b7g").display, "t-3fa9c0d21b7g");
        let e = parse_name("a:b").unwrap_err();
        assert_eq!(e.details["field"], "name");
    }

    #[test]
    fn ids_are_recognized_exactly() {
        assert!(is_id("t-3fa9c0d21b7e", 't'));
        assert!(!is_id("t-3FA9C0D21B7E", 't'), "issued ids are lowercase");
        assert!(!is_id("p-3fa9c0d21b7e", 't'));
        assert!(looks_like_id("T-3FA9C0D21B7E"));
        assert!(!looks_like_id("t-3fa9c0d21b7"));
    }

    #[test]
    fn default_names_are_the_lowest_free_integer() {
        let mut r = Registry::default();
        let mut g = ids();
        for want in ["1", "2", "3"] {
            let i = r.reserve(spec(None), "x".into(), &mut g).unwrap();
            assert_eq!(r.terminals[i].name.display, want);
        }
        let two = r.terminals[1].id.clone();
        r.terminals.retain(|t| t.id != two);
        let i = r.reserve(spec(None), "x".into(), &mut g).unwrap();
        assert_eq!(r.terminals[i].name.display, "2", "a freed number is reused");
        // An explicit name that is a number takes that number away from the default rule.
        r.reserve(spec(Some("4")), "x".into(), &mut g).unwrap();
        let i = r.reserve(spec(None), "x".into(), &mut g).unwrap();
        assert_eq!(r.terminals[i].name.display, "5");
    }

    #[test]
    fn names_are_unique_ignoring_case_and_ids_are_never_reused() {
        let mut r = Registry::default();
        let mut g = ids();
        let i = r.reserve(spec(Some("Work")), "x".into(), &mut g).unwrap();
        let work = r.terminals[i].id.clone();
        for clash in ["work", "WORK", " work "] {
            let e = r
                .reserve(spec(Some(clash)), "x".into(), &mut g)
                .unwrap_err();
            assert_eq!(e.code, ErrorCode::NameConflict, "{clash}");
            assert_eq!(e.details["existing"], work.as_str());
        }
        assert!(
            r.check_free(&name("WORK"), Some(&work)).is_ok(),
            "a Terminal may change the case of its own name"
        );
        // A removed Terminal's ids stay issued: a generator that repeats them is skipped.
        r.terminals.clear();
        let mut repeat = {
            let work = work.clone();
            let mut first = true;
            move |p: char| {
                if p == 't' && std::mem::take(&mut first) {
                    work.clone()
                } else {
                    g(p)
                }
            }
        };
        let i = r
            .reserve(spec(Some("work")), "x".into(), &mut repeat)
            .unwrap();
        assert_ne!(r.terminals[i].id, work, "ids are never reassigned");
        assert!(r.issued(&work));
    }

    #[test]
    fn the_state_machine_allows_exactly_the_documented_transitions() {
        use EndCause::{Exited, HostStop, Kill, Stop};
        use State::{Failed, Running, Starting, Stopped, Stopping};
        let ended = |cause, remain_on_exit| Transition::Ended {
            cause,
            remain_on_exit,
        };
        let ok = [
            (Starting, Transition::Spawned, Some(Running)),
            (
                Starting,
                Transition::SpawnFailed { keep: true },
                Some(Failed),
            ),
            (Starting, Transition::SpawnFailed { keep: false }, None),
            (Running, Transition::EndBegins, Some(Stopping)),
            (Stopping, ended(Stop, false), Some(Stopped)),
            (Stopping, ended(HostStop, false), Some(Stopped)),
            (Stopping, ended(Exited, true), Some(Stopped)),
            (Stopping, ended(Exited, false), None),
            (Stopping, ended(Kill, true), None),
            (Stopped, Transition::Remove, None),
            (Failed, Transition::Remove, None),
        ];
        for (from, t, to) in ok {
            assert_eq!(next(from, t).unwrap(), to, "{from:?} {t:?}");
        }
        // Everything else is refused with invalid_state, e.g. removing a live Terminal without
        // ending it first, or starting twice.
        let all_states = [Starting, Running, Stopping, Stopped, Failed];
        let all_transitions = [
            Transition::Spawned,
            Transition::SpawnFailed { keep: true },
            Transition::SpawnFailed { keep: false },
            Transition::EndBegins,
            ended(Stop, false),
            ended(Exited, true),
            ended(Exited, false),
            ended(Kill, false),
            ended(HostStop, false),
            Transition::Remove,
        ];
        let mut refused = 0;
        for from in all_states {
            for t in all_transitions {
                let allowed = ok.iter().any(|(f, tt, _)| *f == from && *tt == t)
                    || (from == Stopping && matches!(t, Transition::Ended { .. }));
                match next(from, t) {
                    Ok(_) => assert!(allowed, "{from:?} {t:?} must be refused"),
                    Err(e) => {
                        assert!(!allowed, "{from:?} {t:?}");
                        assert_eq!(e.code, ErrorCode::InvalidState);
                        assert_eq!(e.details["state"], from.as_str());
                        refused += 1;
                    }
                }
            }
        }
        assert!(refused >= 35, "{refused}");
        assert!(Kill > Stop && Stop > Exited && Exited > HostStop);
        assert!(Starting.is_live() && Running.is_live() && Stopping.is_live());
        assert!(!Stopped.is_live() && !Failed.is_live());
    }

    #[test]
    fn targets_resolve_names_ids_and_indexes_exactly() {
        let mut r = Registry::default();
        let mut g = ids();
        r.reserve(spec(Some("Work")), "x".into(), &mut g).unwrap();
        r.reserve(spec(Some("서버")), "x".into(), &mut g).unwrap();
        let (tid, wid, pid) = {
            let t = &r.terminals[1];
            (
                t.id.clone(),
                t.windows[0].id.clone(),
                t.windows[0].panes[0].id.clone(),
            )
        };
        let at = |s: &str| r.resolve(s).unwrap();
        assert_eq!(at("work").terminal, 0, "names ignore case");
        assert_eq!(at("WORK").level, Level::Terminal);
        assert_eq!(at("서버").terminal, 1);
        assert_eq!(at(&tid).terminal, 1);
        assert_eq!((at(&wid).terminal, at(&wid).level), (1, Level::Window));
        assert_eq!((at(&pid).terminal, at(&pid).level), (1, Level::Pane));
        assert_eq!(at("서버:0").level, Level::Window);
        assert_eq!(at("서버:0.0").level, Level::Pane);
        assert_eq!(at("서버:cmd.0").window, 0, "a window by its name");
        assert_eq!(at(&format!("{tid}:{wid}.{pid}")).pane, 0);
        let code = |s: &str| r.resolve(s).unwrap_err().code;
        for missing in [
            "wor",
            "nope",
            "t-000000000999",
            "p-000000000999",
            "서버:1",
            "서버:0.1",
            "Work:zsh",
        ] {
            assert_eq!(code(missing), ErrorCode::TargetNotFound, "{missing}");
        }
        for malformed in ["", "서버:", "서버:0.x"] {
            assert_eq!(code(malformed), ErrorCode::InvalidParams, "{malformed:?}");
        }
        assert_eq!(r.resolve_terminal("work").unwrap(), 0);
        assert_eq!(
            r.resolve_terminal(&pid).unwrap_err().code,
            ErrorCode::InvalidParams,
            "terminal operations never act on a whole Terminal through a pane id"
        );
        // Duplicate window names are ambiguous (tmux semantics: window names need not be unique).
        let w = r.terminals[0].windows[0].clone();
        let mut w2 = w;
        w2.id = "w-0000000000ff".into();
        w2.index = 1;
        r.terminals[0].windows.push(w2);
        let e = r.resolve("work:cmd").unwrap_err();
        assert_eq!(e.code, ErrorCode::AmbiguousTarget);
        assert_eq!(e.details["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn shells_busy_and_command_lines() {
        for s in [
            r"C:\Windows\system32\cmd.exe",
            "pwsh",
            "PowerShell.EXE",
            "/usr/bin/bash",
        ] {
            assert!(is_shell(s), "{s}");
        }
        assert!(!is_shell("cargo.exe") && !is_shell("vim"));
        assert!(!busy("cmd.exe", 1), "an idle shell");
        assert!(busy("cmd.exe", 2), "a shell with a child process");
        assert!(
            busy("ping.exe", 1),
            "not a shell: the program itself is the work"
        );
        assert_eq!(
            split_command_line(r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo -c "a b""#),
            Command {
                program: r"C:\Program Files\PowerShell\7\pwsh.exe".into(),
                args: vec!["-NoLogo".into(), "-c".into(), "a b".into()],
            }
        );
        assert_eq!(split_command_line("cmd.exe /d /q").args, ["/d", "/q"]);
        assert_eq!(split_command_line(r#"x "a\"b" c\\"#).args, ["a\"b", r"c\\"]);
        assert_eq!(split_command_line("").program, "");
        assert_eq!(window_name(r"C:\Windows\system32\cmd.exe"), "cmd");
        assert_eq!(window_name("pwsh"), "pwsh");
    }

    #[test]
    fn terminal_objects_carry_the_model_and_volatile_fields() {
        let mut r = Registry::default();
        let mut g = ids();
        let i = r
            .reserve(spec(Some("work")), "2026-11-04T01:02:03Z".into(), &mut g)
            .unwrap();
        let tid = r.terminals[i].id.clone();
        r.apply(&tid, Transition::Spawned).unwrap();
        r.terminals[i].active_pane_mut().pid = Some(4242);
        let live = |_: &Pane| Live {
            running: true,
            processes: 2,
            clients: 1,
            output_bytes: 10,
            history: None,
        };
        let j = terminal_json(&r.terminals[i], &live);
        crate::proto::assert_ipc_def("terminal", &j);
        crate::proto::assert_ipc_def("terminal_summary", &summary_json(&r.terminals[i], &live));
        assert_eq!(j["state"], "running");
        assert_eq!(j["busy"], true, "cmd with a child process");
        assert_eq!(j["windows"][0]["panes"][0]["pid"], 4242);
        assert_eq!(j["windows"][0]["layout"]["type"], "pane");
        let s = summary_json(&r.terminals[i], &live);
        assert_eq!(
            (s["panes"].as_u64(), s["clients"].as_u64()),
            (Some(1), Some(1))
        );
        // Ending: the pid is no longer reported once nothing runs.
        r.apply(&tid, Transition::EndBegins).unwrap();
        r.apply(
            &tid,
            Transition::Ended {
                cause: EndCause::Stop,
                remain_on_exit: false,
            },
        )
        .unwrap();
        let j = terminal_json(&r.terminals[i], &live);
        crate::proto::assert_ipc_def("terminal", &j);
        assert_eq!(j["state"], "stopped");
        assert_eq!(j["windows"][0]["panes"][0]["pid"], Value::Null);
        assert_eq!(j["busy"], false);
        assert_eq!(r.apply(&tid, Transition::Remove).unwrap(), None);
        assert!(r.terminals.is_empty());
    }
}
