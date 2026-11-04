//! Argument parsing for the `subak` CLI (docs/CLI_CONTRACT.md §2–§4).
//!
//! Hand-written on purpose: the CLI is small until Step 12, and staying dependency-free keeps the
//! binary small. Flags may appear anywhere before `--`; everything after `--` is the command of
//! `terminal new`. Each command accepts only its documented options and positional arguments.

use crate::output::CliError;

/// What to run.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    TerminalNew,
    TerminalList,
    TerminalInfo,
    TerminalRename,
    TerminalAttach,
    TerminalStop,
    TerminalKill,
    TerminalRm,
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
            Self::TerminalNew => "terminal.new",
            Self::TerminalList => "terminal.list",
            Self::TerminalInfo => "terminal.info",
            Self::TerminalRename => "terminal.rename",
            Self::TerminalAttach => "terminal.attach",
            Self::TerminalStop => "terminal.stop",
            Self::TerminalKill => "terminal.kill",
            Self::TerminalRm => "terminal.rm",
        }
    }

    /// The positional arguments after the command words, and how many are required.
    fn positionals(&self) -> (&'static [&'static str], usize) {
        match self {
            Self::TerminalNew => (&["name"], 0),
            Self::TerminalInfo
            | Self::TerminalAttach
            | Self::TerminalStop
            | Self::TerminalKill
            | Self::TerminalRm => (&["target"], 1),
            Self::TerminalRename => (&["target", "new name"], 2),
            _ => (&[], 0),
        }
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
            Self::HostStatus | Self::HostHealth => &["--endpoint"],
            Self::HostAttach => &["--endpoint", "--size", "--dry-run"],
            Self::HostStop => &["--endpoint", "--dry-run", "--yes", "--kill-terminals"],
            Self::TerminalNew => &[
                "--endpoint",
                "--cwd",
                "--size",
                "--remain-on-exit",
                "--keep-on-failure",
                "--dry-run",
            ],
            Self::TerminalList | Self::TerminalInfo => &["--endpoint"],
            Self::TerminalRename => &["--endpoint", "--dry-run"],
            Self::TerminalAttach => &["--endpoint", "--size", "--dry-run"],
            Self::TerminalStop | Self::TerminalKill => &["--endpoint", "--dry-run", "--yes"],
            Self::TerminalRm => &["--endpoint", "--dry-run", "--yes", "--force"],
        };
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
    /// `terminal rm`: also end a Terminal that still runs.
    pub force: bool,
    /// `terminal new`: working directory of the pane.
    pub cwd: Option<String>,
    pub remain_on_exit: bool,
    pub keep_on_failure: bool,
    /// Positional arguments after the command words (a target, a name).
    pub positional: Vec<String>,
    /// `terminal new -- <program> [args…]`.
    pub command: Vec<String>,
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
            "--no-shell" => {
                a.no_shell = true;
                opts.push("--no-shell");
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
        ["terminal", "new", ..] => (Cmd::TerminalNew, 2),
        ["terminal", "list" | "ls", ..] => (Cmd::TerminalList, 2),
        ["terminal", "info", ..] => (Cmd::TerminalInfo, 2),
        ["terminal", "rename", ..] => (Cmd::TerminalRename, 2),
        ["terminal", "attach", ..] => (Cmd::TerminalAttach, 2),
        ["terminal", "stop", ..] => (Cmd::TerminalStop, 2),
        ["terminal", "kill", ..] => (Cmd::TerminalKill, 2),
        ["terminal", "rm", ..] => (Cmd::TerminalRm, 2),
        ["terminal", ..] => {
            return Err((
                "terminal".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" "))).hint(
                    "terminal commands: new, list, info, rename, attach, stop, kill, rm (see `subak --help`)",
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
                    "Subak is pre-alpha: only `capabilities`, `terminal` and `host` commands exist; see `subak --help`",
                ),
            ));
        }
    };
    let (names, min) = cmd.positionals();
    let rest = &words[n..];
    if rest.len() > names.len() || rest.len() < min {
        let want = if names.is_empty() {
            "no arguments".to_string()
        } else {
            names
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    if i < min {
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
            CliError::usage(format!("`{}` takes {want}", words[..n].join(" "))),
        ));
    }
    a.positional = rest.iter().map(|s| (*s).to_string()).collect();
    if dashdash && cmd != Cmd::TerminalNew {
        return Err((
            cmd.name().into(),
            CliError::usage("only `terminal new` takes a command after `--`"),
        ));
    }
    if dashdash && a.command.first().is_none_or(|p| p.trim().is_empty()) {
        return Err((
            cmd.name().into(),
            CliError::usage("`--` must be followed by a program"),
        ));
    }
    if a.no_shell && a.shell.is_some() {
        return Err((
            cmd.name().into(),
            CliError::usage("--shell and --no-shell conflict"),
        ));
    }
    if let Some(e) = bad {
        return Err((cmd.name().into(), e));
    }
    if let Some(o) = opts.iter().find(|o| !cmd.accepts(o)) {
        return Err((
            cmd.name().into(),
            CliError::usage(format!("`{o}` is not an option of `{}`", words.join(" "))),
        ));
    }
    if a.quiet && a.verbose {
        return Err((
            cmd.name().into(),
            CliError::usage("--quiet and --verbose conflict"),
        ));
    }
    if matches!(cmd, Cmd::HostAttach | Cmd::TerminalAttach) && a.json && !a.dry_run {
        return Err((
            cmd.name().into(),
            CliError::usage(format!(
                "`{}` streams the terminal; --json works only with --dry-run",
                words[..n].join(" ")
            )),
        ));
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

    #[test]
    fn rejects_bad_usage_with_the_longest_known_command() {
        let usage = |args: &[&str]| p(args).unwrap_err();
        assert_eq!(usage(&["attach"]).0, "subak");
        assert_eq!(usage(&["host", "frob"]).0, "host");
        assert_eq!(usage(&["host", "status", "--yes"]).0, "host.status");
        assert_eq!(
            usage(&["host", "start", "--endpoint", "a\\b"]).0,
            "host.start"
        );
        assert_eq!(usage(&["host", "attach", "--size", "1x1"]).0, "host.attach");
        assert_eq!(usage(&["host", "attach", "--json"]).0, "host.attach");
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
        for (_, e) in [usage(&["attach"]), usage(&["host", "frob"])] {
            assert_eq!(e.code, crate::output::Code::Usage);
        }
    }

    #[test]
    fn help_version_and_bare_invocation() {
        assert_eq!(p(&[]).unwrap().0, Cmd::Help);
        assert_eq!(p(&["host", "--help"]).unwrap().0, Cmd::Help);
        assert_eq!(p(&["-V"]).unwrap().0, Cmd::Version);
        assert_eq!(
            p(&["host", "attach", "--dry-run", "--json"]).unwrap().0,
            Cmd::HostAttach
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
