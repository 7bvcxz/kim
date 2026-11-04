//! Argument parsing for the `subak` CLI (docs/CLI_CONTRACT.md §2–§4, docs/SUBAK_CLI.md).
//!
//! Hand-written on purpose: staying dependency-free keeps the binary small. Flags may appear
//! anywhere before `--`; everything after `--` is the program of `new`. Each command accepts only
//! its documented options and positional arguments.
//!
//! Since Step 12 (ADR 0027) the Terminal commands are top-level, tmux-familiar verbs: `subak new`,
//! `ls`, `info`, `status`, `attach`, `detach`, `rename`, `stop`, `kill`, `rm`. The Step 10 spelling
//! `subak terminal <verb>` still works as a **deprecated alias**: it runs the same command, reports
//! the same command name, and adds a deprecation warning.
//!
//! Step 13 (ADR 0030, docs/SCROLLBACK.md) adds `capture`, `history` and `clear-history`
//! (tmux `capture-pane`, the `history-limit` counters, `clear-history`) and `new --history-limit`.
//!
//! Step 21 (ADR 0054) adds the internal windows and panes of a Terminal: `subak window
//! new|ls|select|rename|kill` and `subak pane new|ls|select|kill`, with tmux's long command names
//! (`new-window`, `list-windows`, `select-window`, `rename-window`, `kill-window`, `list-panes`,
//! `select-pane`, `kill-pane`) as aliases.
//!
//! Step 17 adds the `hotkeys` command group of the hotkey contract (docs/HOTKEY_CONTRACT.md §10,
//! `SUBAK-FR-034`): `subak hotkeys list|show|set|disable|enable|reset|check|export|import`.

use crate::output::{CliError, Code};
use std::io::{self, IsTerminal};

/// What to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Help,
    Version,
    Capabilities,
    HostServe,
    HostStart,
    HostStatus,
    HostHealth,
    HostAttach,
    HostStop,
    New,
    Ls,
    Info,
    Status,
    Attach,
    Detach,
    Rename,
    Stop,
    Kill,
    Rm,
    Capture,
    History,
    ClearHistory,
    HotkeysList,
    HotkeysShow,
    HotkeysSet,
    HotkeysDisable,
    HotkeysEnable,
    HotkeysReset,
    HotkeysCheck,
    HotkeysExport,
    HotkeysImport,
    // Step 20: settings and profiles (config.toml).
    ConfigShow,
    ConfigCheck,
    ConfigSet,
    ConfigReset,
    ConfigExport,
    ConfigImport,
    ProfileList,
    ProfileShow,
    // Step 21: internal windows and panes.
    WindowNew,
    WindowLs,
    WindowSelect,
    WindowRename,
    WindowKill,
    PaneNew,
    PaneLs,
    PaneSelect,
    PaneKill,
}

impl Cmd {
    /// The dotted command name used in the JSON envelope.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Help | Self::Version => "subak",
            Self::Capabilities => "capabilities",
            Self::HostServe => "host.serve",
            Self::HostStart => "host.start",
            Self::HostStatus => "host.status",
            Self::HostHealth => "host.health",
            Self::HostAttach => "host.attach",
            Self::HostStop => "host.stop",
            Self::New => "new",
            Self::Ls => "ls",
            Self::Info => "info",
            Self::Status => "status",
            Self::Attach => "attach",
            Self::Detach => "detach",
            Self::Rename => "rename",
            Self::Stop => "stop",
            Self::Kill => "kill",
            Self::Rm => "rm",
            Self::Capture => "capture",
            Self::History => "history",
            Self::ClearHistory => "clear-history",
            Self::HotkeysList => "hotkeys.list",
            Self::HotkeysShow => "hotkeys.show",
            Self::HotkeysSet => "hotkeys.set",
            Self::HotkeysDisable => "hotkeys.disable",
            Self::HotkeysEnable => "hotkeys.enable",
            Self::HotkeysReset => "hotkeys.reset",
            Self::HotkeysCheck => "hotkeys.check",
            Self::HotkeysExport => "hotkeys.export",
            Self::HotkeysImport => "hotkeys.import",
            Self::ConfigShow => "config.show",
            Self::ConfigCheck => "config.check",
            Self::ConfigSet => "config.set",
            Self::ConfigReset => "config.reset",
            Self::ConfigExport => "config.export",
            Self::ConfigImport => "config.import",
            Self::ProfileList => "profile.list",
            Self::ProfileShow => "profile.show",
            Self::WindowNew => "window.new",
            Self::WindowLs => "window.ls",
            Self::WindowSelect => "window.select",
            Self::WindowRename => "window.rename",
            Self::WindowKill => "window.kill",
            Self::PaneNew => "pane.new",
            Self::PaneLs => "pane.ls",
            Self::PaneSelect => "pane.select",
            Self::PaneKill => "pane.kill",
        }
    }

    /// The command words in the canonical spelling (`host status`, `new`).
    pub fn words(&self) -> String {
        self.name().replace('.', " ")
    }

    /// Whether the command is one of the `hotkeys` group (portable, never contacts a Host).
    pub fn is_hotkeys(&self) -> bool {
        matches!(
            self,
            Self::HotkeysList
                | Self::HotkeysShow
                | Self::HotkeysSet
                | Self::HotkeysDisable
                | Self::HotkeysEnable
                | Self::HotkeysReset
                | Self::HotkeysCheck
                | Self::HotkeysExport
                | Self::HotkeysImport
        )
    }

    /// Whether the command is one of the `config` or `profile` groups (Step 20; portable, never
    /// contacts a Host).
    pub fn is_settings(&self) -> bool {
        matches!(
            self,
            Self::ConfigShow
                | Self::ConfigCheck
                | Self::ConfigSet
                | Self::ConfigReset
                | Self::ConfigExport
                | Self::ConfigImport
                | Self::ProfileList
                | Self::ProfileShow
        )
    }

    /// Whether the command streams a terminal instead of printing one document.
    pub fn streams(&self, a: &Args) -> bool {
        match self {
            Self::HostAttach | Self::Attach => !a.dry_run,
            Self::New => a.attach && !a.dry_run,
            _ => false,
        }
    }

    /// The positional arguments after the command words, and how many are required.
    fn positionals(&self) -> (&'static [&'static str], usize) {
        match self {
            Self::New => (&["name"], 0),
            Self::Attach | Self::Detach | Self::Capture | Self::History | Self::ClearHistory => {
                (&["target"], 0)
            }
            Self::Info | Self::Stop | Self::Kill | Self::Rm => (&["target"], 1),
            Self::Rename => (&["target", "new name"], 2),
            Self::HotkeysShow => (&["action"], 1),
            Self::HotkeysSet => (&["action", "keys"], 2),
            Self::HotkeysDisable | Self::HotkeysReset => (&["action"], 0),
            Self::HotkeysImport | Self::ConfigImport => (&["file"], 1),
            Self::ConfigSet => (&["key", "value"], 1),
            Self::ConfigReset => (&["key or section"], 0),
            Self::ProfileShow => (&["name"], 1),
            Self::WindowNew | Self::WindowLs => (&["terminal"], 1),
            Self::WindowSelect | Self::WindowKill => (&["window"], 1),
            Self::WindowRename => (&["window", "new name"], 2),
            Self::PaneNew | Self::PaneLs => (&["terminal or window"], 1),
            Self::PaneSelect | Self::PaneKill => (&["pane"], 1),
            _ => (&[], 0),
        }
    }

    /// Whether the last positional argument repeats (`hotkeys set <action> <keys>...`).
    fn variadic(&self) -> bool {
        *self == Self::HotkeysSet
    }

    /// Options (beyond the global ones) this command accepts.
    fn accepts(&self, opt: &str) -> bool {
        let allowed: &[&str] = match self {
            Self::Help | Self::Version | Self::Capabilities => &[],
            Self::HostServe => &[
                "--endpoint",
                "--shell",
                "--no-shell",
                "--idle-exit",
                "--job-breakaway",
                "--dry-run",
            ],
            Self::HostStart => &[
                "--endpoint",
                "--shell",
                "--no-shell",
                "--idle-exit",
                "--dry-run",
            ],
            Self::HostStatus | Self::HostHealth | Self::Ls | Self::Info | Self::Status => {
                &["--endpoint"]
            }
            Self::HostAttach | Self::Attach => {
                &["--endpoint", "--size", "--read-only", "--dry-run"]
            }
            Self::HostStop => &["--endpoint", "--dry-run", "--yes", "--kill-terminals"],
            Self::New => &[
                "--endpoint",
                "--cwd",
                "--env",
                "--unset-env",
                "--size",
                "--remain-on-exit",
                "--keep-on-failure",
                "--command-line",
                "--no-start",
                "--attach",
                "--read-only",
                "--history-limit",
                "--profile",
                "--no-profile",
                "--dry-run",
            ],
            Self::Detach => &["--endpoint", "--client", "--dry-run"],
            Self::Capture => &[
                "--endpoint",
                "--start",
                "--end",
                "--history",
                "--vt",
                "--join",
                "--no-redact",
            ],
            Self::History => &["--endpoint"],
            Self::ClearHistory => &["--endpoint", "--dry-run", "--yes"],
            Self::Rename => &["--endpoint", "--dry-run"],
            Self::Stop | Self::Kill => &["--endpoint", "--dry-run", "--yes"],
            Self::Rm => &["--endpoint", "--dry-run", "--yes", "--force"],
            Self::HotkeysList => &["--scope", "--profile"],
            Self::HotkeysShow => &["--profile"],
            Self::HotkeysSet => &[
                "--scope",
                "--trigger",
                "--hold-ms",
                "--repeat",
                "--profile",
                "--replace",
                "--allow-conflict",
                "--dry-run",
            ],
            Self::HotkeysDisable => &["--all", "--scope", "--profile", "--dry-run"],
            Self::HotkeysEnable => &["--all", "--scope", "--dry-run"],
            Self::HotkeysReset => &["--all", "--yes", "--profile", "--dry-run"],
            Self::HotkeysCheck => &["--all-tools", "--min-severity", "--profile"],
            Self::HotkeysExport => &[],
            Self::HotkeysImport => &["--dry-run"],
            Self::ConfigShow
            | Self::ConfigCheck
            | Self::ConfigExport
            | Self::ProfileList
            | Self::ProfileShow => &[],
            Self::ConfigSet | Self::ConfigImport => &["--dry-run"],
            Self::ConfigReset => &["--all", "--yes", "--dry-run"],
            Self::WindowNew | Self::PaneNew => &[
                "--endpoint",
                "--name",
                "--cwd",
                "--env",
                "--unset-env",
                "--size",
                "--command-line",
                "--history-limit",
                "--no-select",
                "--dry-run",
            ],
            Self::WindowLs | Self::PaneLs => &["--endpoint"],
            Self::WindowSelect | Self::PaneSelect | Self::WindowRename => {
                &["--endpoint", "--dry-run"]
            }
            Self::WindowKill | Self::PaneKill => &["--endpoint", "--dry-run", "--yes"],
        };
        if *self == Self::PaneNew && opt == "--name" {
            return false;
        }
        allowed.contains(&opt)
    }
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub no_input: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub endpoint: Option<String>,
    pub shell: Option<String>,
    /// Start the Host without its prototype pane (it then owns no Terminal).
    pub no_shell: bool,
    /// Seconds the Host waits, owning no Terminal and with no client, before it exits.
    pub idle_exit: Option<u64>,
    pub size: Option<(u16, u16)>,
    pub job_breakaway: Option<bool>,
    /// `host stop`: consent to end every live Terminal (ADR 0003 §7).
    pub kill_terminals: bool,
    /// `rm`: also end a Terminal that still runs.
    pub force: bool,
    /// `new`: working directory of the pane.
    pub cwd: Option<String>,
    /// `new --env` / `--unset-env`, in the order given.
    pub env: Vec<EnvArg>,
    pub remain_on_exit: bool,
    pub keep_on_failure: bool,
    /// `new --command-line <line>`: a verbatim Windows command line (ADR 0027 §4).
    pub command_line: Option<String>,
    /// `new --no-start`: fail with `host_not_running` instead of starting a Host.
    pub no_start: bool,
    /// `new --attach`: attach to the new Terminal at once.
    pub attach: bool,
    /// `attach --read-only`: a viewer whose keys (except the detach keys) are not sent.
    pub read_only: bool,
    /// `detach --client <id>`: one client, by the id `subak status` lists.
    pub client: Option<u64>,
    /// `new --history-limit <lines>`: history rows the pane keeps (Step 13).
    pub history_limit: Option<u64>,
    /// `capture --start <line>|-` and `--end <line>|-` (tmux `-S`/`-E`).
    pub start: Option<Line>,
    pub end: Option<Line>,
    /// `capture --history`: from the oldest history line (`--start -`).
    pub history: bool,
    /// `capture --vt`: keep colours and attributes as escape sequences (tmux `-e`).
    pub vt: bool,
    /// `capture --join`: join wrapped rows into one line (tmux `-J`).
    pub join: bool,
    /// `capture --no-redact`: show secret-looking values as they are.
    pub no_redact: bool,
    /// The deprecated spelling that was used (`terminal new`), if any (ADR 0027).
    pub alias: Option<String>,
    /// `hotkeys list|set|disable|enable --scope <scope>`.
    pub scope: Option<String>,
    /// `hotkeys … --profile <name>`: a `profiles` overlay of the user layer; `new --profile
    /// <name>`: the shell profile of `config.toml` to start (Step 20).
    pub profile: Option<String>,
    /// `new --no-profile`: the Host's default shell even when a default profile is set.
    pub no_profile: bool,
    /// `hotkeys set --trigger press|release|hold`.
    pub trigger: Option<String>,
    /// `hotkeys set --hold-ms <ms>` (with `--trigger hold`).
    pub hold_ms: Option<u64>,
    /// `hotkeys set --repeat`: auto-repeat fires the action again.
    pub repeat: bool,
    /// `hotkeys set --replace`: take the keys away from another action of Subak.
    pub replace: bool,
    /// `hotkeys set --allow-conflict`: accept a cross-tool global conflict.
    pub allow_conflict: bool,
    /// `hotkeys disable|enable|reset --all`.
    pub all: bool,
    /// `hotkeys check --all-tools`: also every installed Kim tool's files.
    pub all_tools: bool,
    /// `hotkeys check --min-severity error|warning|info`.
    pub min_severity: Option<String>,
    /// Positional arguments after the command words (a target, a name).
    pub positional: Vec<String>,
    /// `new -- <program> [args…]`.
    pub command: Vec<String>,
    /// `window new --name <name>` (Step 21).
    pub name: Option<String>,
    /// `window new|pane new --no-select`: do not make it the active one (tmux `-d`).
    pub no_select: bool,
}

/// A line of `capture --start/--end`: a number (history lines are negative) or `-`, the oldest
/// history line (as start) or the last screen row (as end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    Number(i64),
    Edge,
}

impl Line {
    fn parse(text: &str) -> Option<Self> {
        if text == "-" {
            return Some(Self::Edge);
        }
        text.parse::<i64>()
            .ok()
            .filter(|n| n.unsigned_abs() <= 1_000_000_000)
            .map(Self::Number)
    }
}

/// One environment override of `new` (docs/PTY_PROCESSES.md §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvArg {
    /// `--env NAME=VALUE`.
    Set(String, String),
    /// `--env NAME`: the value this CLI process has, so a secret never appears on a command line.
    Copy(String),
    /// `--unset-env NAME`.
    Unset(String),
}

impl EnvArg {
    pub fn name(&self) -> &str {
        match self {
            Self::Set(n, _) | Self::Copy(n) | Self::Unset(n) => n,
        }
    }
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

/// Longest idle grace period accepted by `--idle-exit` (one day).
pub const MAX_IDLE_EXIT_S: u64 = 86_400;

/// Endpoint names are a single pipe-name component: `\\.\pipe\<name>`.
pub fn valid_endpoint(name: &str) -> bool {
    (1..=200).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `120x30` → (cols, rows); both within 2..=1000.
pub fn parse_size(text: &str) -> Option<(u16, u16)> {
    let (c, r) = text.split_once(['x', 'X'])?;
    let c: u16 = c.parse().ok()?;
    let r: u16 = r.parse().ok()?;
    ((2..=1000).contains(&c) && (2..=1000).contains(&r)).then_some((c, r))
}

/// The Terminal verbs and the command each one runs (top level, and after `terminal`).
fn terminal_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "new" => Cmd::New,
        "ls" | "list" => Cmd::Ls,
        "info" => Cmd::Info,
        "status" => Cmd::Status,
        "attach" => Cmd::Attach,
        "detach" => Cmd::Detach,
        "rename" => Cmd::Rename,
        "stop" => Cmd::Stop,
        "kill" => Cmd::Kill,
        "rm" => Cmd::Rm,
        "capture" | "capture-pane" => Cmd::Capture,
        "history" => Cmd::History,
        "clear-history" => Cmd::ClearHistory,
        // Step 21: tmux's long names for the window and pane commands.
        "new-window" => Cmd::WindowNew,
        "list-windows" => Cmd::WindowLs,
        "select-window" => Cmd::WindowSelect,
        "rename-window" => Cmd::WindowRename,
        "kill-window" => Cmd::WindowKill,
        "list-panes" => Cmd::PaneLs,
        "select-pane" => Cmd::PaneSelect,
        "kill-pane" => Cmd::PaneKill,
        _ => return None,
    })
}

/// The `subak window <verb>` commands (Step 21).
fn window_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "new" => Cmd::WindowNew,
        "ls" | "list" => Cmd::WindowLs,
        "select" => Cmd::WindowSelect,
        "rename" => Cmd::WindowRename,
        "kill" => Cmd::WindowKill,
        _ => return None,
    })
}

/// The `subak pane <verb>` commands (Step 21; splitting is Step 22).
fn pane_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "new" => Cmd::PaneNew,
        "ls" | "list" => Cmd::PaneLs,
        "select" => Cmd::PaneSelect,
        "kill" => Cmd::PaneKill,
        _ => return None,
    })
}

/// The `subak hotkeys <verb>` commands (docs/HOTKEY_CONTRACT.md §10.1).
fn hotkeys_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "list" | "ls" => Cmd::HotkeysList,
        "show" => Cmd::HotkeysShow,
        "set" => Cmd::HotkeysSet,
        "disable" => Cmd::HotkeysDisable,
        "enable" => Cmd::HotkeysEnable,
        "reset" => Cmd::HotkeysReset,
        "check" => Cmd::HotkeysCheck,
        "export" => Cmd::HotkeysExport,
        "import" => Cmd::HotkeysImport,
        _ => return None,
    })
}

/// The option combinations of the `hotkeys` commands (what Kim's clap definitions enforce).
fn check_hotkeys(cmd: Cmd, a: &Args) -> Result<(), CliError> {
    let action = !a.positional.is_empty();
    let scope = a.scope.as_deref();
    let family = |s: &str| matches!(s, "global" | "app" | "context");
    match cmd {
        Cmd::HotkeysList => {
            if let Some(s) = scope
                && !(family(s) || kim_hotkeys::Scope::parse(s).is_some())
            {
                return Err(CliError::usage(format!(
                    "`{s}` is not a scope (global, app, context or context:<id>)"
                )));
            }
        }
        Cmd::HotkeysSet => {
            if let Some(s) = scope
                && !matches!(s, "global" | "app")
            {
                return Err(CliError::usage(format!(
                    "`{s}` is not a binding scope (global or app)"
                ))
                .hint("context actions take no --scope"));
            }
            if a.hold_ms.is_some() && a.trigger.is_none() {
                return Err(CliError::usage("--hold-ms needs --trigger hold"));
            }
        }
        Cmd::HotkeysDisable => {
            if usize::from(action) + usize::from(a.all) + usize::from(scope.is_some()) != 1 {
                return Err(CliError::usage(
                    "give exactly one of <action>, --all or --scope <global|app|context>",
                ));
            }
            if a.profile.is_some() && !action {
                return Err(CliError::usage("--profile needs an <action>"));
            }
        }
        Cmd::HotkeysEnable => {
            if a.all == scope.is_some() {
                return Err(CliError::usage(
                    "give exactly one of --all or --scope <global|app|context>",
                )
                .hint("to restore one action's default use `subak hotkeys reset <action>`"));
            }
        }
        Cmd::HotkeysReset => {
            if action == a.all {
                return Err(CliError::usage("give exactly one of <action> or --all"));
            }
            if a.profile.is_some() && !action {
                return Err(CliError::usage("--profile needs an <action>"));
            }
        }
        _ => {}
    }
    if matches!(cmd, Cmd::HotkeysDisable | Cmd::HotkeysEnable)
        && let Some(s) = scope
        && !family(s)
    {
        return Err(CliError::usage(format!(
            "`{s}` is not a scope family (global, app or context)"
        )));
    }
    Ok(())
}

/// The `subak config <verb>` commands (Step 20).
fn config_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "show" => Cmd::ConfigShow,
        "check" => Cmd::ConfigCheck,
        "set" => Cmd::ConfigSet,
        "reset" => Cmd::ConfigReset,
        "export" => Cmd::ConfigExport,
        "import" => Cmd::ConfigImport,
        _ => return None,
    })
}

/// The `subak profile <verb>` commands (Step 20).
fn profile_verb(word: &str) -> Option<Cmd> {
    Some(match word {
        "list" | "ls" => Cmd::ProfileList,
        "show" => Cmd::ProfileShow,
        _ => return None,
    })
}

/// The verbs `subak terminal …` had in Steps 10–11; only these are deprecated aliases.
const STEP10_VERBS: [&str; 9] = [
    "new", "list", "ls", "info", "rename", "attach", "stop", "kill", "rm",
];

/// Parses `args` (without the program name). On failure returns the longest recognized
/// command name and a `usage` error, so `--json` callers still get one document.
pub fn parse(raw: &[String]) -> Result<(Cmd, Args), (String, CliError)> {
    let mut a = Args::default();
    let mut words: Vec<&str> = Vec::new();
    let mut opts: Vec<&'static str> = Vec::new();
    let mut help = false;
    let mut version = false;
    let mut i = 0;
    let mut bad: Option<CliError> = None;
    let mut dashdash = false;
    while i < raw.len() {
        let arg = raw[i].as_str();
        if arg == "--" {
            dashdash = true;
            a.command = raw[i + 1..].to_vec();
            break;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        let mut value = |name: &str| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            // A following `--flag` is never taken as a value (`--opt=--x` passes such a value).
            let v = raw.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
            if v.is_some() {
                i += 1;
            } else {
                bad.get_or_insert(CliError::usage(format!("{name} needs a value")));
            }
            v
        };
        match flag {
            "--json" => a.json = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "--no-input" => a.no_input = true,
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            "--dry-run" => {
                a.dry_run = true;
                opts.push("--dry-run");
            }
            "-y" | "--yes" => {
                a.yes = true;
                opts.push("--yes");
            }
            "--endpoint" => {
                opts.push("--endpoint");
                if let Some(v) = value("--endpoint") {
                    if !valid_endpoint(&v) {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid endpoint name `{v}`")).hint(
                                "use 1-200 characters from A-Z a-z 0-9 - _ . (a pipe name, not a path)",
                            ),
                        );
                    }
                    a.endpoint = Some(v);
                }
            }
            "--shell" => {
                opts.push("--shell");
                if let Some(v) = value("--shell") {
                    if v.trim().is_empty() {
                        bad.get_or_insert(CliError::usage("--shell needs a command line"));
                    }
                    a.shell = Some(v);
                }
            }
            "--command-line" => {
                opts.push("--command-line");
                if let Some(v) = value("--command-line") {
                    if v.trim().is_empty() || v.contains('\0') {
                        bad.get_or_insert(
                            CliError::usage("--command-line needs a command line").hint(
                                "e.g. --command-line \"cmd.exe /d /s /c \\\"echo \\\"a  b\\\" & ver\\\"\"",
                            ),
                        );
                    }
                    a.command_line = Some(v);
                }
            }
            "--client" => {
                opts.push("--client");
                if let Some(v) = value("--client") {
                    a.client = v.parse::<u64>().ok().filter(|c| *c > 0);
                    if a.client.is_none() {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid client id `{v}`")).hint(
                                "a client id is a positive number; `subak status` lists them",
                            ),
                        );
                    }
                }
            }
            "--history-limit" => {
                opts.push("--history-limit");
                if let Some(v) = value("--history-limit") {
                    a.history_limit = v.parse::<u64>().ok().filter(|n| *n <= 100_000);
                    if a.history_limit.is_none() {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid --history-limit `{v}`"))
                                .hint("a number of lines from 0 to 100000 (default 2000)"),
                        );
                    }
                }
            }
            f @ ("--start" | "--end") => {
                opts.push(if f == "--start" { "--start" } else { "--end" });
                // A negative line number (`-5`) is a value here, not an option.
                match value(f).as_deref().map(Line::parse) {
                    Some(Some(l)) if f == "--start" => a.start = Some(l),
                    Some(Some(l)) => a.end = Some(l),
                    _ => {
                        bad.get_or_insert(
                            CliError::usage(format!("{f} needs a line number or `-`")).hint(
                                "screen rows count from 0, history lines from -1 (the newest) down; `-` is the oldest history line (--start) or the last screen row (--end)",
                            ),
                        );
                    }
                }
            }
            "--history" => {
                a.history = true;
                opts.push("--history");
            }
            "--scope" => {
                opts.push("--scope");
                a.scope = value("--scope");
            }
            "--profile" => {
                opts.push("--profile");
                if let Some(v) = value("--profile") {
                    if v.trim().is_empty() {
                        bad.get_or_insert(CliError::usage("--profile needs a name"));
                    }
                    a.profile = Some(v);
                }
            }
            "--trigger" => {
                opts.push("--trigger");
                if let Some(v) = value("--trigger") {
                    if !matches!(v.as_str(), "press" | "release" | "hold") {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid --trigger `{v}`"))
                                .hint("press, release or hold"),
                        );
                    }
                    a.trigger = Some(v);
                }
            }
            "--hold-ms" => {
                opts.push("--hold-ms");
                if let Some(v) = value("--hold-ms") {
                    a.hold_ms = v.parse::<u64>().ok();
                    if a.hold_ms.is_none() {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid --hold-ms `{v}`"))
                                .hint("milliseconds from 200 to 5000"),
                        );
                    }
                }
            }
            "--min-severity" => {
                opts.push("--min-severity");
                if let Some(v) = value("--min-severity") {
                    if !matches!(v.as_str(), "error" | "warning" | "info") {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid --min-severity `{v}`"))
                                .hint("error, warning or info"),
                        );
                    }
                    a.min_severity = Some(v);
                }
            }
            "--repeat" => {
                a.repeat = true;
                opts.push("--repeat");
            }
            "--name" => {
                opts.push("--name");
                if let Some(v) = value("--name") {
                    if v.trim().is_empty() {
                        bad.get_or_insert(CliError::usage("--name needs a window name"));
                    }
                    a.name = Some(v);
                }
            }
            "--no-select" => {
                a.no_select = true;
                opts.push("--no-select");
            }
            "--replace" => {
                a.replace = true;
                opts.push("--replace");
            }
            "--allow-conflict" => {
                a.allow_conflict = true;
                opts.push("--allow-conflict");
            }
            "--all" => {
                a.all = true;
                opts.push("--all");
            }
            "--no-profile" => {
                a.no_profile = true;
                opts.push("--no-profile");
            }
            "--all-tools" => {
                a.all_tools = true;
                opts.push("--all-tools");
            }
            "--vt" => {
                a.vt = true;
                opts.push("--vt");
            }
            "--join" => {
                a.join = true;
                opts.push("--join");
            }
            "--no-redact" => {
                a.no_redact = true;
                opts.push("--no-redact");
            }
            "--no-shell" => {
                a.no_shell = true;
                opts.push("--no-shell");
            }
            "--no-start" => {
                a.no_start = true;
                opts.push("--no-start");
            }
            "--attach" => {
                a.attach = true;
                opts.push("--attach");
            }
            "--read-only" => {
                a.read_only = true;
                opts.push("--read-only");
            }
            "--kill-terminals" => {
                a.kill_terminals = true;
                opts.push("--kill-terminals");
            }
            "--force" => {
                a.force = true;
                opts.push("--force");
            }
            "--remain-on-exit" => {
                a.remain_on_exit = true;
                opts.push("--remain-on-exit");
            }
            "--keep-on-failure" => {
                a.keep_on_failure = true;
                opts.push("--keep-on-failure");
            }
            "--cwd" => {
                opts.push("--cwd");
                if let Some(v) = value("--cwd") {
                    if v.trim().is_empty() {
                        bad.get_or_insert(CliError::usage("--cwd needs a directory"));
                    }
                    a.cwd = Some(v);
                }
            }
            "--env" => {
                opts.push("--env");
                if let Some(v) = value("--env") {
                    // Never echo the value: it may be a secret.
                    match v.split_once('=') {
                        Some(("", _)) => {
                            bad.get_or_insert(
                                CliError::usage("--env needs NAME=VALUE or NAME")
                                    .hint("e.g. --env RUST_LOG=debug, or --env GITHUB_TOKEN to copy it from this environment"),
                            );
                        }
                        Some((name, value)) => a.env.push(EnvArg::Set(name.into(), value.into())),
                        None if v.is_empty() => {
                            bad.get_or_insert(CliError::usage("--env needs NAME=VALUE or NAME"));
                        }
                        None => a.env.push(EnvArg::Copy(v)),
                    }
                }
            }
            "--unset-env" => {
                opts.push("--unset-env");
                if let Some(v) = value("--unset-env") {
                    if v.is_empty() || v.contains('=') {
                        bad.get_or_insert(CliError::usage("--unset-env needs a variable NAME"));
                    } else {
                        a.env.push(EnvArg::Unset(v));
                    }
                }
            }
            "--idle-exit" => {
                opts.push("--idle-exit");
                if let Some(v) = value("--idle-exit") {
                    a.idle_exit = v.parse::<u64>().ok().filter(|s| *s <= MAX_IDLE_EXIT_S);
                    if a.idle_exit.is_none() {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid --idle-exit `{v}`"))
                                .hint("seconds from 0 to 86400 (default 10)"),
                        );
                    }
                }
            }
            "--size" => {
                opts.push("--size");
                if let Some(v) = value("--size") {
                    a.size = parse_size(&v);
                    if a.size.is_none() {
                        bad.get_or_insert(
                            CliError::usage(format!("invalid size `{v}`"))
                                .hint("use <cols>x<rows>, e.g. 120x30 (2..1000 each)"),
                        );
                    }
                }
            }
            "--job-breakaway" => {
                opts.push("--job-breakaway");
                if let Some(v) = value("--job-breakaway") {
                    a.job_breakaway = match v.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => {
                            bad.get_or_insert(CliError::usage("--job-breakaway is true or false"));
                            None
                        }
                    };
                }
            }
            f if f.starts_with('-') && f.len() > 1 => {
                bad.get_or_insert(CliError::usage(format!("unknown option `{f}`")));
            }
            w => words.push(w),
        }
        i += 1;
    }

    if help {
        return Ok((Cmd::Help, a));
    }
    if version {
        return Ok((Cmd::Version, a));
    }
    let (cmd, n) = match words.as_slice() {
        [] if !a.json && !dashdash => (Cmd::Help, 0),
        [] => {
            return Err((
                "subak".into(),
                CliError::usage("no command given; --json needs a command"),
            ));
        }
        ["capabilities", ..] => (Cmd::Capabilities, 1),
        ["host", "serve", ..] => (Cmd::HostServe, 2),
        ["host", "start", ..] => (Cmd::HostStart, 2),
        ["host", "status", ..] => (Cmd::HostStatus, 2),
        ["host", "health", ..] => (Cmd::HostHealth, 2),
        ["host", "attach", ..] => (Cmd::HostAttach, 2),
        ["host", "stop", ..] => (Cmd::HostStop, 2),
        ["hotkeys", sub, ..] if hotkeys_verb(sub).is_some() => {
            (hotkeys_verb(sub).expect("checked above"), 2)
        }
        ["hotkeys", ..] => {
            return Err((
                "hotkeys".into(),
                CliError::usage(if words.len() == 1 {
                    "`subak hotkeys` needs a command".to_string()
                } else {
                    format!("unknown command `{}`", words.join(" "))
                })
                .hint(
                    "hotkeys commands: list, show, set, disable, enable, reset, check, export, import (see `subak --help`)",
                ),
            ));
        }
        ["config", sub, ..] if config_verb(sub).is_some() => {
            (config_verb(sub).expect("checked above"), 2)
        }
        ["config", ..] => {
            return Err((
                "config".into(),
                CliError::usage(if words.len() == 1 {
                    "`subak config` needs a command".to_string()
                } else {
                    format!("unknown command `{}`", words.join(" "))
                })
                .hint(
                    "config commands: show, check, set, reset, export, import (see `subak --help`)",
                ),
            ));
        }
        ["profile" | "profiles", sub, ..] if profile_verb(sub).is_some() => {
            (profile_verb(sub).expect("checked above"), 2)
        }
        ["profile" | "profiles", ..] => {
            return Err((
                "profile".into(),
                CliError::usage(if words.len() == 1 {
                    "`subak profile` needs a command".to_string()
                } else {
                    format!("unknown command `{}`", words.join(" "))
                })
                .hint("profile commands: list, show (profiles are defined in config.toml; see `subak --help`)"),
            ));
        }
        ["window", sub, ..] if window_verb(sub).is_some() => {
            (window_verb(sub).expect("checked above"), 2)
        }
        ["window", ..] => {
            return Err((
                "window".into(),
                CliError::usage(if words.len() == 1 {
                    "`subak window` needs a command".to_string()
                } else {
                    format!("unknown command `{}`", words.join(" "))
                })
                .hint("window commands: new, ls, select, rename, kill (see `subak --help`)"),
            ));
        }
        ["pane", sub, ..] if pane_verb(sub).is_some() => {
            (pane_verb(sub).expect("checked above"), 2)
        }
        ["pane", ..] => {
            return Err((
                "pane".into(),
                CliError::usage(if words.len() == 1 {
                    "`subak pane` needs a command".to_string()
                } else {
                    format!("unknown command `{}`", words.join(" "))
                })
                .hint("pane commands: new, ls, select, kill (splitting is Step 22; see `subak --help`)"),
            ));
        }
        ["terminal", verb, ..] if STEP10_VERBS.contains(verb) => {
            let cmd = terminal_verb(verb).expect("every Step 10 verb is a Terminal verb");
            a.alias = Some(format!("terminal {verb}"));
            (cmd, 2)
        }
        [verb, ..] if terminal_verb(verb).is_some() => {
            (terminal_verb(verb).expect("checked above"), 1)
        }
        ["terminal", ..] => {
            return Err((
                "terminal".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" "))).hint(
                    "`subak terminal <verb>` is the deprecated spelling of the Terminal commands; use `subak new|ls|info|status|attach|detach|rename|stop|kill|rm` (see `subak --help`)",
                ),
            ));
        }
        ["host", ..] => {
            return Err((
                "host".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" "))).hint(
                    "host commands: serve, start, status, health, attach, stop (see `subak --help`)",
                ),
            ));
        }
        _ => {
            return Err((
                "subak".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" "))).hint(
                    "commands: new, ls, info, status, attach, detach, rename, stop, kill, rm, capture, history, clear-history, window, pane, host, hotkeys, config, profile, capabilities (see `subak --help`)",
                ),
            ));
        }
    };
    let typed = words[..n].join(" ");
    let (names, min) = cmd.positionals();
    let rest = &words[n..];
    if (rest.len() > names.len() && !cmd.variadic()) || rest.len() < min {
        let want = if names.is_empty() {
            "no arguments".to_string()
        } else {
            names
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    if i < min && cmd.variadic() && i + 1 == names.len() {
                        format!("<{w}>...")
                    } else if i < min {
                        format!("<{w}>")
                    } else {
                        format!("[<{w}>]")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Err((
            cmd.name().into(),
            CliError::usage(format!("`{typed}` takes {want}")),
        ));
    }
    a.positional = rest.iter().map(|s| (*s).to_string()).collect();
    let fail = |e: CliError| Err((cmd.name().to_string(), e));
    if dashdash && !matches!(cmd, Cmd::New | Cmd::WindowNew | Cmd::PaneNew) {
        return fail(CliError::usage(
            "only `new`, `window new` and `pane new` take a program after `--`",
        ));
    }
    if dashdash && a.command.first().is_none_or(|p| p.trim().is_empty()) {
        return fail(CliError::usage("`--` must be followed by a program"));
    }
    if a.no_shell && a.shell.is_some() {
        return fail(CliError::usage("--shell and --no-shell conflict"));
    }
    if let Some(e) = bad {
        return fail(e);
    }
    if let Some(o) = opts.iter().find(|o| !cmd.accepts(o)) {
        return fail(CliError::usage(format!(
            "`{o}` is not an option of `{typed}`"
        )));
    }
    if a.quiet && a.verbose {
        return fail(CliError::usage("--quiet and --verbose conflict"));
    }
    if dashdash && a.command_line.is_some() {
        return fail(
            CliError::usage("give the program either after `--` or with --command-line, not both")
                .hint("`-- <program> [args…]` quotes every word for Windows; --command-line passes one line verbatim (for cmd.exe /c)"),
        );
    }
    if cmd == Cmd::New && a.profile.is_some() && a.no_profile {
        return fail(CliError::usage("--profile and --no-profile conflict"));
    }
    if cmd == Cmd::New && a.read_only && !a.attach {
        return fail(CliError::usage("--read-only needs --attach"));
    }
    if a.history && a.start.is_some() {
        return fail(
            CliError::usage("--history and --start conflict")
                .hint("--history is the same as --start -"),
        );
    }
    if cmd.is_hotkeys() {
        check_hotkeys(cmd, &a).map_err(|e| (cmd.name().to_string(), e))?;
    }
    if cmd.streams(&a) && a.json {
        return fail(CliError::usage(format!(
            "`{typed}{}` streams the terminal; --json works only with --dry-run",
            if cmd == Cmd::New { " --attach" } else { "" }
        )));
    }
    Ok((cmd, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<(Cmd, Args), (String, CliError)> {
        parse(&args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_host_commands_with_options_anywhere() {
        let (cmd, a) = p(&["--json", "host", "--endpoint", "t-1", "status"]).unwrap();
        assert_eq!(cmd, Cmd::HostStatus);
        assert!(a.json);
        assert_eq!(a.endpoint.as_deref(), Some("t-1"));
        let (cmd, a) = p(&["host", "attach", "--size=100x40"]).unwrap();
        assert_eq!(cmd, Cmd::HostAttach);
        assert_eq!(a.size, Some((100, 40)));
        let (cmd, a) = p(&["host", "stop", "-y", "--dry-run"]).unwrap();
        assert_eq!(cmd, Cmd::HostStop);
        assert!(a.yes && a.dry_run);
        let (_, a) = p(&[
            "host",
            "serve",
            "--shell",
            "cmd.exe /d",
            "--job-breakaway",
            "false",
        ])
        .unwrap();
        assert_eq!(a.shell.as_deref(), Some("cmd.exe /d"));
        assert_eq!(a.job_breakaway, Some(false));
        let (cmd, a) = p(&["host", "start", "--no-shell", "--idle-exit", "0"]).unwrap();
        assert_eq!(cmd, Cmd::HostStart);
        assert!(a.no_shell);
        assert_eq!(a.idle_exit, Some(0));
        assert_eq!(p(&["host", "health", "--json"]).unwrap().0, Cmd::HostHealth);
    }

    #[test]
    fn host_lifetime_options_are_validated() {
        let usage = |args: &[&str]| p(args).unwrap_err().0;
        assert_eq!(usage(&["host", "start", "--idle-exit", "-1"]), "host.start");
        assert_eq!(
            usage(&["host", "start", "--idle-exit", "86401"]),
            "host.start"
        );
        assert_eq!(
            usage(&["host", "serve", "--idle-exit", "ten"]),
            "host.serve"
        );
        assert_eq!(
            usage(&["host", "start", "--no-shell", "--shell", "cmd.exe"]),
            "host.start"
        );
        assert_eq!(usage(&["host", "health", "--no-shell"]), "host.health");
        assert_eq!(usage(&["host", "stop", "--idle-exit", "5"]), "host.stop");
        assert_eq!(
            p(&["host", "start", "--idle-exit=86400"])
                .unwrap()
                .1
                .idle_exit,
            Some(86_400)
        );
    }

    /// ADR 0027: the Terminal verbs are top level; `terminal <verb>` is a deprecated alias of
    /// the very same command, and `list` is an alias of `ls`.
    #[test]
    fn terminal_verbs_are_top_level_and_the_step10_spelling_is_an_alias() {
        for (args, cmd) in [
            (&["new"][..], Cmd::New),
            (&["new", "work"], Cmd::New),
            (&["ls"], Cmd::Ls),
            (&["list"], Cmd::Ls),
            (&["info", "work"], Cmd::Info),
            (&["status"], Cmd::Status),
            (&["attach"], Cmd::Attach),
            (&["attach", "work"], Cmd::Attach),
            (&["detach"], Cmd::Detach),
            (&["detach", "work"], Cmd::Detach),
            (&["rename", "a", "b"], Cmd::Rename),
            (&["stop", "a"], Cmd::Stop),
            (&["kill", "a"], Cmd::Kill),
            (&["rm", "a"], Cmd::Rm),
        ] {
            let (got, a) = p(args).unwrap_or_else(|(_, e)| panic!("{args:?}: {}", e.message));
            assert_eq!(got, cmd, "{args:?}");
            assert_eq!(a.alias, None, "{args:?}");
        }
        for (args, cmd, alias) in [
            (&["terminal", "new", "x"][..], Cmd::New, "terminal new"),
            (&["terminal", "list"], Cmd::Ls, "terminal list"),
            (&["terminal", "ls"], Cmd::Ls, "terminal ls"),
            (&["terminal", "info", "x"], Cmd::Info, "terminal info"),
            (
                &["terminal", "rename", "x", "y"],
                Cmd::Rename,
                "terminal rename",
            ),
            (&["terminal", "attach", "x"], Cmd::Attach, "terminal attach"),
            (&["terminal", "stop", "x"], Cmd::Stop, "terminal stop"),
            (&["terminal", "kill", "x"], Cmd::Kill, "terminal kill"),
            (&["terminal", "rm", "x", "--force"], Cmd::Rm, "terminal rm"),
        ] {
            let (got, a) = p(args).unwrap_or_else(|(_, e)| panic!("{args:?}: {}", e.message));
            assert_eq!(got, cmd, "{args:?}");
            assert_eq!(a.alias.as_deref(), Some(alias));
            assert_eq!(
                got.name(),
                cmd.name(),
                "the alias reports the canonical name"
            );
        }
        // Verbs that never existed under `terminal` are not invented there.
        assert_eq!(p(&["terminal", "status"]).unwrap_err().0, "terminal");
        assert_eq!(p(&["terminal", "detach", "x"]).unwrap_err().0, "terminal");
        assert_eq!(p(&["terminal", "close", "x"]).unwrap_err().0, "terminal");
    }

    #[test]
    fn new_options_and_their_conflicts() {
        let (cmd, a) = p(&[
            "new",
            "build",
            "--command-line",
            r#"cmd.exe /d /s /c "echo "a  b" & ver""#,
            "--no-start",
            "--attach",
            "--read-only",
        ])
        .unwrap();
        assert_eq!(cmd, Cmd::New);
        assert_eq!(
            a.command_line.as_deref(),
            Some(r#"cmd.exe /d /s /c "echo "a  b" & ver""#)
        );
        assert!(a.no_start && a.attach && a.read_only);
        assert!(cmd.streams(&a));
        let usage = |args: &[&str]| {
            let (name, e) = p(args).unwrap_err();
            assert_eq!(e.code, Code::Usage, "{args:?}");
            name
        };
        assert_eq!(
            usage(&["new", "--command-line", "cmd", "--", "cmd.exe"]),
            "new"
        );
        assert_eq!(usage(&["new", "--command-line", "  "]), "new");
        assert_eq!(usage(&["new", "--read-only"]), "new");
        assert_eq!(usage(&["new", "--attach", "--json"]), "new");
        assert!(p(&["new", "--attach", "--json", "--dry-run"]).is_ok());
        assert_eq!(usage(&["ls", "--command-line", "x"]), "ls");
        assert_eq!(usage(&["attach", "--no-start"]), "attach");
        assert_eq!(usage(&["info", "x", "--", "cmd.exe"]), "info");
    }

    #[test]
    fn detach_takes_a_target_or_a_client() {
        let (cmd, a) = p(&["detach", "work", "--client", "7", "--dry-run"]).unwrap();
        assert_eq!(cmd, Cmd::Detach);
        assert_eq!(a.client, Some(7));
        assert_eq!(a.positional, ["work"]);
        assert!(a.dry_run);
        for bad in ["0", "-1", "x", "1.5"] {
            assert_eq!(
                p(&["detach", &format!("--client={bad}")]).unwrap_err().0,
                "detach",
                "{bad}"
            );
        }
        assert_eq!(p(&["detach", "a", "b"]).unwrap_err().0, "detach");
        assert_eq!(p(&["detach", "--yes"]).unwrap_err().0, "detach");
        assert_eq!(p(&["attach", "--client", "3"]).unwrap_err().0, "attach");
    }

    #[test]
    fn rejects_bad_usage_with_the_longest_known_command() {
        let usage = |args: &[&str]| p(args).unwrap_err();
        assert_eq!(usage(&["frobnicate"]).0, "subak");
        assert_eq!(usage(&["host", "frob"]).0, "host");
        assert_eq!(usage(&["host", "status", "--yes"]).0, "host.status");
        assert_eq!(
            usage(&["host", "start", "--endpoint", "a\\b"]).0,
            "host.start"
        );
        assert_eq!(usage(&["host", "attach", "--size", "1x1"]).0, "host.attach");
        assert_eq!(usage(&["host", "attach", "--json"]).0, "host.attach");
        assert_eq!(usage(&["attach", "--json"]).0, "attach");
        assert_eq!(usage(&["host", "stop", "--endpoint"]).0, "host.stop");
        let (name, e) = usage(&["host", "stop", "--endpoint", "--json"]);
        assert_eq!(name, "host.stop");
        assert!(e.message.contains("needs a value"), "{}", e.message);
        assert_eq!(
            p(&["host", "start", "--shell=--odd", "--json"])
                .unwrap()
                .1
                .shell
                .as_deref(),
            Some("--odd")
        );
        assert_eq!(usage(&["host", "status", "-q", "-v"]).0, "host.status");
        assert_eq!(usage(&["--json"]).0, "subak");
        assert_eq!(usage(&["capabilities", "--bogus"]).0, "capabilities");
        assert_eq!(usage(&["info"]).0, "info");
        assert_eq!(usage(&["rename", "a"]).0, "rename");
        assert_eq!(usage(&["ls", "extra"]).0, "ls");
        assert_eq!(usage(&["stop", "a", "--force"]).0, "stop");
        assert_eq!(usage(&["kill", "a", "--kill-terminals"]).0, "kill");
        for (_, e) in [usage(&["frobnicate"]), usage(&["host", "frob"])] {
            assert_eq!(e.code, Code::Usage);
        }
    }

    #[test]
    fn help_version_and_bare_invocation() {
        assert_eq!(p(&[]).unwrap().0, Cmd::Help);
        assert_eq!(p(&["host", "--help"]).unwrap().0, Cmd::Help);
        assert_eq!(p(&["new", "--help"]).unwrap().0, Cmd::Help);
        assert_eq!(p(&["-V"]).unwrap().0, Cmd::Version);
        assert_eq!(
            p(&["host", "attach", "--dry-run", "--json"]).unwrap().0,
            Cmd::HostAttach
        );
    }

    #[test]
    fn env_overrides_parse_in_order_and_never_echo_values() {
        let (cmd, a) = p(&[
            "new",
            "--env",
            "RUST_LOG=debug",
            "--env=URL=https://x/?a=b",
            "--env",
            "GITHUB_TOKEN",
            "--unset-env",
            "HTTP_PROXY",
            "--env",
            "EMPTY=",
            "--",
            "pwsh",
            "--env",
            "not-ours",
        ])
        .unwrap();
        assert_eq!(cmd, Cmd::New);
        assert_eq!(
            a.env,
            vec![
                EnvArg::Set("RUST_LOG".into(), "debug".into()),
                EnvArg::Set("URL".into(), "https://x/?a=b".into()),
                EnvArg::Copy("GITHUB_TOKEN".into()),
                EnvArg::Unset("HTTP_PROXY".into()),
                EnvArg::Set("EMPTY".into(), String::new()),
            ]
        );
        assert_eq!(
            a.command,
            ["pwsh", "--env", "not-ours"],
            "after -- it is the program's"
        );
        let secret = "s3cr3t-never-echoed";
        for bad in [
            vec!["new", "--env", "=x"],
            vec!["new", "--env"],
            vec!["new", "--unset-env", "A=B"],
            vec!["ls", "--env", "TOKEN=s3cr3t-never-echoed"],
            vec!["terminal", "list", "--env", "TOKEN=s3cr3t-never-echoed"],
        ] {
            let (_, e) = p(&bad).unwrap_err();
            assert_eq!(e.code, Code::Usage, "{bad:?}");
            assert!(!e.message.contains(secret), "{}", e.message);
        }
    }

    #[test]
    fn hotkeys_commands_parse_with_their_options_only() {
        let (cmd, a) = p(&[
            "hotkeys",
            "set",
            "tab.next",
            "Ctrl+PageDown",
            "Ctrl+K Ctrl+N",
            "--replace",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        assert_eq!(cmd, Cmd::HotkeysSet);
        assert_eq!(cmd.name(), "hotkeys.set");
        assert!(cmd.is_hotkeys() && a.replace && a.dry_run && a.json);
        assert_eq!(a.positional, ["tab.next", "Ctrl+PageDown", "Ctrl+K Ctrl+N"]);
        let (cmd, a) = p(&["hotkeys", "list", "--scope", "context:terminal"]).unwrap();
        assert_eq!(cmd, Cmd::HotkeysList);
        assert_eq!(a.scope.as_deref(), Some("context:terminal"));
        let (cmd, a) = p(&[
            "hotkeys",
            "check",
            "--all-tools",
            "--min-severity",
            "warning",
        ])
        .unwrap();
        assert_eq!(cmd, Cmd::HotkeysCheck);
        assert!(a.all_tools);
        assert_eq!(a.min_severity.as_deref(), Some("warning"));
        let (cmd, a) = p(&["hotkeys", "reset", "--all", "-y"]).unwrap();
        assert_eq!(cmd, Cmd::HotkeysReset);
        assert!(a.all && a.yes);
        assert_eq!(
            p(&["hotkeys", "disable", "--scope", "app"]).unwrap().0,
            Cmd::HotkeysDisable
        );
        assert_eq!(
            p(&["hotkeys", "enable", "--all"]).unwrap().0,
            Cmd::HotkeysEnable
        );
        assert_eq!(p(&["hotkeys", "export"]).unwrap().0, Cmd::HotkeysExport);
        assert_eq!(
            p(&["hotkeys", "import", "x.json"]).unwrap().0,
            Cmd::HotkeysImport
        );
        assert_eq!(
            p(&["hotkeys", "show", "terminal.new"]).unwrap().0,
            Cmd::HotkeysShow
        );
        let usage = |args: &[&str]| {
            let (name, e) = p(args).unwrap_err();
            assert_eq!(e.code, Code::Usage, "{args:?}");
            name
        };
        assert_eq!(usage(&["hotkeys"]), "hotkeys");
        assert_eq!(usage(&["hotkeys", "frob"]), "hotkeys");
        assert_eq!(usage(&["hotkeys", "set", "tab.next"]), "hotkeys.set");
        assert_eq!(usage(&["hotkeys", "show"]), "hotkeys.show");
        assert_eq!(usage(&["hotkeys", "show", "a", "b"]), "hotkeys.show");
        assert_eq!(
            usage(&["hotkeys", "set", "a", "F5", "--scope", "context"]),
            "hotkeys.set"
        );
        assert_eq!(
            usage(&["hotkeys", "set", "a", "F5", "--hold-ms", "500"]),
            "hotkeys.set"
        );
        assert_eq!(
            usage(&["hotkeys", "set", "a", "F5", "--trigger", "tap"]),
            "hotkeys.set"
        );
        assert_eq!(
            usage(&["hotkeys", "set", "a", "F5", "--yes"]),
            "hotkeys.set"
        );
        assert_eq!(usage(&["hotkeys", "disable"]), "hotkeys.disable");
        assert_eq!(
            usage(&["hotkeys", "disable", "a", "--all"]),
            "hotkeys.disable"
        );
        assert_eq!(
            usage(&["hotkeys", "disable", "--scope", "window"]),
            "hotkeys.disable"
        );
        assert_eq!(
            usage(&["hotkeys", "disable", "--all", "--profile", "x"]),
            "hotkeys.disable"
        );
        assert_eq!(usage(&["hotkeys", "enable"]), "hotkeys.enable");
        assert_eq!(
            usage(&["hotkeys", "enable", "--all", "--scope", "app"]),
            "hotkeys.enable"
        );
        assert_eq!(usage(&["hotkeys", "reset"]), "hotkeys.reset");
        assert_eq!(usage(&["hotkeys", "reset", "a", "--all"]), "hotkeys.reset");
        assert_eq!(
            usage(&["hotkeys", "list", "--scope", "window"]),
            "hotkeys.list"
        );
        assert_eq!(
            usage(&["hotkeys", "check", "--min-severity", "fatal"]),
            "hotkeys.check"
        );
        assert_eq!(
            usage(&["hotkeys", "export", "--endpoint", "x"]),
            "hotkeys.export"
        );
    }

    #[test]
    fn sizes_and_endpoints() {
        assert_eq!(parse_size("120x30"), Some((120, 30)));
        assert_eq!(parse_size("80X25"), Some((80, 25)));
        for bad in ["", "x", "0x10", "10x", "1001x10", "a x b", "-1x5"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
        assert!(valid_endpoint("mouv-subak-S-1-5-21-1-s1"));
        for bad in ["", "a\\b", "a/b", "a b", &"x".repeat(201)] {
            assert!(!valid_endpoint(bad), "{bad}");
        }
    }
}
