//! Argument parsing for the `ddalgak` CLI (docs/CLI_CONTRACT.md §2–§4). Hand-written like
//! Subak's: the command set is small and the binary stays dependency-free. Flags may appear
//! anywhere; each command accepts only its documented options.

use crate::output::CliError;
use ddalgak::lifecycle::Request;

/// What to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Help,
    Version,
    Capabilities,
    Status,
    Health,
    Engine(Request),
    Quit,
    StartupStatus,
    StartupEnable,
    StartupDisable,
}

impl Cmd {
    /// The dotted command name used in the JSON envelope.
    pub fn name(self) -> String {
        match self {
            Self::Help | Self::Version => "ddalgak".into(),
            Self::Capabilities => "capabilities".into(),
            Self::Status => "status".into(),
            Self::Health => "health".into(),
            Self::Engine(r) => format!("engine.{}", r.as_str()),
            Self::Quit => "quit".into(),
            Self::StartupStatus => "startup.status".into(),
            Self::StartupEnable => "startup.enable".into(),
            Self::StartupDisable => "startup.disable".into(),
        }
    }

    fn accepts(self, opt: &str) -> bool {
        let allowed: &[&str] = match self {
            Self::Help | Self::Version | Self::Capabilities | Self::StartupStatus => &[],
            Self::Status | Self::Health => &["--endpoint"],
            Self::Engine(_) | Self::Quit => &["--endpoint", "--dry-run"],
            Self::StartupEnable | Self::StartupDisable => &["--dry-run"],
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
    pub dry_run: bool,
    pub endpoint: Option<String>,
}

/// Endpoint names are one pipe-name component: `\\.\pipe\<name>`.
pub fn valid_endpoint(name: &str) -> bool {
    (1..=200).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn command(words: &[&str]) -> Result<Cmd, String> {
    Ok(match words {
        [] => Cmd::Help,
        ["capabilities"] => Cmd::Capabilities,
        ["status"] => Cmd::Status,
        ["health"] => Cmd::Health,
        ["engine", "start" | "on"] => Cmd::Engine(Request::Start),
        ["engine", "pause"] | ["pause"] => Cmd::Engine(Request::Pause),
        ["engine", "resume"] | ["resume"] => Cmd::Engine(Request::Resume),
        ["engine", "stop" | "off"] => Cmd::Engine(Request::Stop),
        ["quit"] => Cmd::Quit,
        ["startup"] | ["startup", "status"] => Cmd::StartupStatus,
        ["startup", "enable"] => Cmd::StartupEnable,
        ["startup", "disable"] => Cmd::StartupDisable,
        ["engine"] => return Err("`engine` needs start, pause, resume or stop".into()),
        _ => return Err(format!("unknown command `{}`", words.join(" "))),
    })
}

/// Parses `args` (without the program name). On failure returns the longest recognized
/// command name and a `usage` error, so `--json` callers still get one document.
pub fn parse(raw: &[String]) -> Result<(Cmd, Args), (String, CliError)> {
    let mut a = Args::default();
    let mut words: Vec<&str> = Vec::new();
    let mut opts: Vec<&'static str> = Vec::new();
    let (mut help, mut version) = (false, false);
    let mut bad: Option<CliError> = None;
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        match flag {
            "--json" => a.json = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            "--dry-run" => {
                a.dry_run = true;
                opts.push("--dry-run");
            }
            "--endpoint" => {
                opts.push("--endpoint");
                let v = inline.or_else(|| {
                    let v = raw.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
                    if v.is_some() {
                        i += 1;
                    }
                    v
                });
                match v {
                    Some(v) if valid_endpoint(&v) => a.endpoint = Some(v),
                    Some(_) => {
                        bad.get_or_insert(CliError::usage(
                            "--endpoint takes a pipe name of letters, digits, `-`, `_` and `.`",
                        ));
                    }
                    None => {
                        bad.get_or_insert(CliError::usage("--endpoint needs a value"));
                    }
                }
            }
            f if f.starts_with('-') => {
                bad.get_or_insert(CliError::usage(format!("unknown option `{f}`")));
            }
            w => words.push(w),
        }
        i += 1;
    }
    let partial = |words: &[&str]| {
        let known = [
            "capabilities",
            "status",
            "health",
            "engine",
            "quit",
            "startup",
            "pause",
            "resume",
        ];
        match words.first() {
            Some(w) if known.contains(w) => {
                let mut n = w.to_string();
                if let Some(second) = words.get(1)
                    && ["engine", "startup"].contains(w)
                    && second.chars().all(|c| c.is_ascii_lowercase())
                {
                    n = format!("{n}.{second}");
                }
                n
            }
            _ => "ddalgak".into(),
        }
    };
    if a.quiet && a.verbose {
        bad.get_or_insert(CliError::usage("--quiet and --verbose conflict"));
    }
    if help {
        return Ok((Cmd::Help, a));
    }
    if version && words.is_empty() {
        return Ok((Cmd::Version, a));
    }
    let cmd = match command(&words) {
        Ok(c) => c,
        Err(e) => return Err((partial(&words), CliError::usage(e))),
    };
    if let Some(e) = bad {
        return Err((cmd.name(), e));
    }
    if let Some(o) = opts.iter().find(|o| !cmd.accepts(o)) {
        return Err((
            cmd.name(),
            CliError::usage(format!(
                "`{}` does not take {o}",
                cmd.name().replace('.', " ")
            )),
        ));
    }
    Ok((cmd, a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<(Cmd, Args), (String, CliError)> {
        parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn commands_aliases_and_flags() {
        assert_eq!(p("").unwrap().0, Cmd::Help);
        assert_eq!(p("--version").unwrap().0, Cmd::Version);
        assert_eq!(p("status --json").unwrap().0, Cmd::Status);
        assert!(p("status --json").unwrap().1.json);
        assert_eq!(p("engine start").unwrap().0, Cmd::Engine(Request::Start));
        assert_eq!(p("engine on").unwrap().0, Cmd::Engine(Request::Start));
        assert_eq!(p("engine off").unwrap().0, Cmd::Engine(Request::Stop));
        assert_eq!(p("pause").unwrap().0, Cmd::Engine(Request::Pause));
        assert_eq!(
            p("resume --dry-run").unwrap().0,
            Cmd::Engine(Request::Resume)
        );
        assert_eq!(p("startup").unwrap().0, Cmd::StartupStatus);
        assert_eq!(p("startup enable --dry-run").unwrap().0, Cmd::StartupEnable);
        let (_, a) = p("health --endpoint my-test.1").unwrap();
        assert_eq!(a.endpoint.as_deref(), Some("my-test.1"));
        assert_eq!(
            p("engine --dry-run stop").unwrap().0,
            Cmd::Engine(Request::Stop)
        );
    }

    #[test]
    fn usage_errors_name_the_longest_command() {
        let (name, e) = p("engine fly").unwrap_err();
        assert_eq!((name.as_str(), e.code.as_str()), ("engine.fly", "usage"));
        let (name, _) = p("nope --json").unwrap_err();
        assert_eq!(name, "ddalgak");
        let (name, e) = p("status --dry-run").unwrap_err();
        assert_eq!(name, "status");
        assert!(e.message.contains("--dry-run"));
        let (name, _) = p("startup enable --endpoint x").unwrap_err();
        assert_eq!(name, "startup.enable");
        assert!(p("status --endpoint bad/name").is_err());
        assert!(p("status --bogus").is_err());
        assert!(p("status -q -v").is_err());
    }
}
