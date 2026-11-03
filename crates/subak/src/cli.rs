//! Argument parsing for the `subak` CLI (docs/CLI_CONTRACT.md §2–§4).
//!
//! Hand-written on purpose: the CLI is tiny until Step 12, and staying dependency-free keeps the
//! binary small. Flags may appear anywhere; each command accepts only its documented options.

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
    HostAttach,
    HostStop,
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
            Self::HostAttach => "host.attach",
            Self::HostStop => "host.stop",
        }
    }

    /// Options (beyond the global ones) this command accepts.
    fn accepts(&self, opt: &str) -> bool {
        let allowed: &[&str] = match self {
            Self::Help | Self::Version | Self::Capabilities => &[],
            Self::HostServe => &["--endpoint", "--shell", "--job-breakaway", "--dry-run"],
            Self::HostStart => &["--endpoint", "--shell", "--dry-run"],
            Self::HostStatus => &["--endpoint"],
            Self::HostAttach => &["--endpoint", "--size", "--dry-run"],
            Self::HostStop => &["--endpoint", "--dry-run", "--yes"],
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
    pub size: Option<(u16, u16)>,
    pub job_breakaway: Option<bool>,
}

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
    while i < raw.len() {
        let arg = raw[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        let mut value = |name: &str| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            i += 1;
            let v = raw.get(i).cloned();
            if v.is_none() {
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
    let cmd = match words.as_slice() {
        [] if !a.json => Cmd::Help,
        [] => {
            return Err((
                "subak".into(),
                CliError::usage("no command given; --json needs a command"),
            ));
        }
        ["capabilities"] => Cmd::Capabilities,
        ["host", "serve"] => Cmd::HostServe,
        ["host", "start"] => Cmd::HostStart,
        ["host", "status"] => Cmd::HostStatus,
        ["host", "attach"] => Cmd::HostAttach,
        ["host", "stop"] => Cmd::HostStop,
        ["host", ..] => {
            return Err((
                "host".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" ")))
                    .hint("host commands: serve, start, status, attach, stop (see `subak --help`)"),
            ));
        }
        _ => {
            return Err((
                "subak".into(),
                CliError::usage(format!("unknown command `{}`", words.join(" "))).hint(
                    "Subak is pre-alpha: only `capabilities` and the Step 08 `host` prototype exist; see `subak --help`",
                ),
            ));
        }
    };
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
    if cmd == Cmd::HostAttach && a.json && !a.dry_run {
        return Err((
            cmd.name().into(),
            CliError::usage("`host attach` streams the terminal; --json works only with --dry-run"),
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
        let (_, a) = p(&["host", "serve", "--shell", "cmd.exe /d", "--job-breakaway", "false"])
            .unwrap();
        assert_eq!(a.shell.as_deref(), Some("cmd.exe /d"));
        assert_eq!(a.job_breakaway, Some(false));
    }

    #[test]
    fn rejects_bad_usage_with_the_longest_known_command() {
        let usage = |args: &[&str]| p(args).unwrap_err();
        assert_eq!(usage(&["attach"]).0, "subak");
        assert_eq!(usage(&["host", "frob"]).0, "host");
        assert_eq!(usage(&["host", "status", "--yes"]).0, "host.status");
        assert_eq!(usage(&["host", "start", "--endpoint", "a\\b"]).0, "host.start");
        assert_eq!(usage(&["host", "attach", "--size", "1x1"]).0, "host.attach");
        assert_eq!(usage(&["host", "attach", "--json"]).0, "host.attach");
        assert_eq!(usage(&["host", "stop", "--endpoint"]).0, "host.stop");
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
        assert!(valid_endpoint("mouv-subak-proto-S-1-5-21-1-s1"));
        for bad in ["", "a\\b", "a/b", "a b", &"x".repeat(201)] {
            assert!(!valid_endpoint(bad), "{bad}");
        }
    }
}
