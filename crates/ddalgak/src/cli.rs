//! Argument parsing for the `ddalgak` CLI (docs/CLI_CONTRACT.md §2–§4). Hand-written like
//! Subak's: the binary stays dependency-free. Flags may appear anywhere; each command accepts
//! only its documented options, and words after the command are its positional arguments.

use crate::output::CliError;
use ddalgak::lifecycle::Request;
use std::collections::BTreeMap;

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
    RuleList,
    RuleShow,
    RuleCheck,
    RuleAdd,
    RuleEdit,
    RuleDelete,
    RuleDuplicate,
    RuleEnable,
    RuleDisable,
    RuleMove,
    RuleRevoke,
    RulesHistory,
    RulesBackup,
    RulesRestore,
    RulesReload,
    RulesDraft,
    RulesDraftApply,
    RulesDraftDiscard,
    // Step 72: modes (definitions in rules.json; the active mode in the running app).
    ModeList,
    ModeShow,
    ModeAdd,
    ModeEdit,
    ModeRename,
    ModeCopy,
    ModeDelete,
    ModeSet,
    ModeToggle,
    ModeReset,
    ModeStatus,
}

/// Options of the mode fields (`mode add`, `mode edit`; Step 72).
const MODE_FIELDS: &[(&str, Kind)] = &[
    ("--title", Value),
    ("--icon", Value),
    ("--color", Value),
    ("--parent", Value),
    ("--one-shot", Flag),
    ("--timeout-ms", Value),
    ("--default", Flag),
    ("--id", Value),
    ("--profile", Value),
];

/// Value-taking options of the mode commands that are not rule fields.
const MODE_VALUES: [&str; 4] = ["--icon", "--color", "--parent", "--timeout-ms"];

/// How an option takes its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Flag,
    Value,
    /// Repeatable value.
    Multi,
}

use Kind::{Flag, Multi, Value};

/// Options of the rule fields (`rule add`, `rule edit`).
const RULE_FIELDS: &[(&str, Kind)] = &[
    ("--trigger", Value),
    ("--title", Value),
    ("--description", Value),
    ("--on", Value),
    ("--hold-ms", Value),
    ("--repeat", Flag),
    ("--pass-through", Flag),
    ("--accept-injected", Flag),
    ("--mode", Multi),
    ("--priority", Value),
    ("--on-error", Value),
    ("--allow", Multi),
    ("--action", Value),
    ("--step", Multi),
    ("--to", Value),
    ("--tap", Value),
    ("--tap-ms", Value),
    ("--keys", Value),
    ("--step-delay-ms", Value),
    ("--text", Value),
    ("--escapes", Flag),
    ("--variables", Flag),
    ("--method", Value),
    ("--key-delay-ms", Value),
    ("--hotstring", Value),
    ("--case", Value),
    ("--inside-words", Flag),
    ("--immediate", Flag),
    ("--end-chars", Value),
    ("--omit-end", Flag),
    ("--keep-abbreviation", Flag),
    ("--path", Value),
    ("--app-id", Value),
    ("--arg", Multi),
    ("--working-dir", Value),
    ("--env", Multi),
    ("--window", Value),
    ("--if-running", Value),
    ("--url", Value),
    ("--op", Value),
    ("--target-mode", Multi),
    ("--ms", Value),
    ("--dx", Value),
    ("--dy", Value),
    ("--x", Value),
    ("--y", Value),
    ("--relative-to", Value),
    ("--grid", Value),
    ("--speed", Value),
    ("--acceleration", Value),
    ("--repeat-ms", Value),
    ("--delay-ms", Value),
    ("--direction", Value),
    ("--notches", Value),
    ("--button", Value),
];

/// Options of every command that writes rules.
const WRITE: &[(&str, Kind)] = &[
    ("--dry-run", Flag),
    ("--draft", Flag),
    ("--if-revision", Value),
    ("--endpoint", Value),
];

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
            Self::RuleList => "rule.list".into(),
            Self::RuleShow => "rule.show".into(),
            Self::RuleCheck => "rule.check".into(),
            Self::RuleAdd => "rule.add".into(),
            Self::RuleEdit => "rule.edit".into(),
            Self::RuleDelete => "rule.delete".into(),
            Self::RuleDuplicate => "rule.duplicate".into(),
            Self::RuleEnable => "rule.enable".into(),
            Self::RuleDisable => "rule.disable".into(),
            Self::RuleMove => "rule.move".into(),
            Self::RuleRevoke => "rule.revoke".into(),
            Self::RulesHistory => "rules.history".into(),
            Self::RulesBackup => "rules.backup".into(),
            Self::RulesRestore => "rules.restore".into(),
            Self::RulesReload => "rules.reload".into(),
            Self::RulesDraft => "rules.draft".into(),
            Self::RulesDraftApply => "rules.draft.apply".into(),
            Self::RulesDraftDiscard => "rules.draft.discard".into(),
            Self::ModeList => "mode.list".into(),
            Self::ModeShow => "mode.show".into(),
            Self::ModeAdd => "mode.add".into(),
            Self::ModeEdit => "mode.edit".into(),
            Self::ModeRename => "mode.rename".into(),
            Self::ModeCopy => "mode.copy".into(),
            Self::ModeDelete => "mode.delete".into(),
            Self::ModeSet => "mode.set".into(),
            Self::ModeToggle => "mode.toggle".into(),
            Self::ModeReset => "mode.reset".into(),
            Self::ModeStatus => "mode.status".into(),
        }
    }

    /// A mode command that reads or writes rules.json (the others ask the running app).
    #[cfg_attr(not(windows), allow(dead_code))] // only the Windows client runs commands (ADR 0011)
    pub fn is_mode_file(self) -> bool {
        matches!(
            self,
            Self::ModeList
                | Self::ModeShow
                | Self::ModeAdd
                | Self::ModeEdit
                | Self::ModeRename
                | Self::ModeCopy
                | Self::ModeDelete
        )
    }

    /// The options the command accepts (besides `--json`, `-q`, `-v`, `-h`, `--no-input`).
    fn options(self) -> Vec<(&'static str, Kind)> {
        let mut o: Vec<(&'static str, Kind)> = match self {
            Self::Help | Self::Version | Self::Capabilities | Self::StartupStatus => vec![],
            Self::Status | Self::Health => vec![("--endpoint", Value)],
            Self::Engine(_) | Self::Quit | Self::RulesReload => {
                vec![("--endpoint", Value), ("--dry-run", Flag)]
            }
            Self::StartupEnable | Self::StartupDisable => vec![("--dry-run", Flag)],
            Self::RuleList => vec![("--profile", Value), ("--mode", Value), ("--draft", Flag)],
            Self::RuleShow => vec![
                ("--draft", Flag),
                ("--show-text", Flag),
                ("--show-targets", Flag),
            ],
            Self::RuleCheck => vec![("--draft", Flag)],
            Self::RulesDraft | Self::RulesHistory => vec![],
            Self::RuleAdd => {
                let mut v = RULE_FIELDS.to_vec();
                v.extend([
                    ("--id", Value),
                    ("--profile", Value),
                    ("--disabled", Flag),
                    ("--before", Value),
                    ("--after", Value),
                    ("--rule", Value),
                ]);
                v.extend_from_slice(WRITE);
                v
            }
            Self::RuleEdit => {
                let mut v = RULE_FIELDS.to_vec();
                v.extend([("--all-modes", Flag), ("--patch", Value)]);
                v.extend_from_slice(WRITE);
                v
            }
            Self::RuleDelete => {
                let mut v = vec![("--yes", Flag)];
                v.extend_from_slice(WRITE);
                v
            }
            Self::RuleDuplicate => {
                let mut v = vec![("--id", Value), ("--title", Value), ("--trigger", Value)];
                v.extend_from_slice(WRITE);
                v
            }
            Self::RuleEnable => {
                let mut v = vec![("--allow", Multi)];
                v.extend_from_slice(WRITE);
                v
            }
            Self::RuleDisable | Self::RuleRevoke => WRITE.to_vec(),
            Self::RuleMove => {
                let mut v = vec![("--before", Value), ("--after", Value), ("--to", Value)];
                v.extend_from_slice(WRITE);
                v
            }
            Self::RulesBackup => vec![("--dry-run", Flag)],
            Self::RulesRestore => vec![
                ("--backup", Flag),
                ("--yes", Flag),
                ("--dry-run", Flag),
                ("--endpoint", Value),
            ],
            Self::RulesDraftApply => vec![("--dry-run", Flag), ("--endpoint", Value)],
            Self::RulesDraftDiscard => vec![("--yes", Flag), ("--dry-run", Flag)],
            Self::ModeList | Self::ModeShow => vec![
                ("--profile", Value),
                ("--draft", Flag),
                ("--endpoint", Value),
            ],
            Self::ModeAdd => {
                let mut v = MODE_FIELDS.to_vec();
                v.extend_from_slice(WRITE);
                v
            }
            Self::ModeEdit => {
                let mut v = MODE_FIELDS.to_vec();
                v.retain(|(n, _)| *n != "--id");
                v.extend([
                    ("--no-icon", Flag),
                    ("--no-color", Flag),
                    ("--no-parent", Flag),
                    ("--no-one-shot", Flag),
                    ("--no-timeout", Flag),
                ]);
                v.extend_from_slice(WRITE);
                v
            }
            Self::ModeRename => {
                let mut v = vec![("--profile", Value)];
                v.extend_from_slice(WRITE);
                v
            }
            Self::ModeCopy => {
                let mut v = vec![
                    ("--id", Value),
                    ("--title", Value),
                    ("--without-rules", Flag),
                    ("--profile", Value),
                ];
                v.extend_from_slice(WRITE);
                v
            }
            Self::ModeDelete => {
                let mut v = vec![
                    ("--delete-rules", Flag),
                    ("--yes", Flag),
                    ("--profile", Value),
                ];
                v.extend_from_slice(WRITE);
                v
            }
            Self::ModeSet | Self::ModeToggle | Self::ModeReset => {
                vec![("--endpoint", Value), ("--dry-run", Flag)]
            }
            Self::ModeStatus => vec![("--endpoint", Value)],
        };
        // Every rule command may name the app endpoint (writes ask that app to reload; scripts
        // can pass the same options to every command).
        if self.name().starts_with("rule") {
            o.push(("--endpoint", Value));
        }
        o.sort_by_key(|(n, _)| *n);
        o.dedup_by_key(|(n, _)| *n);
        o
    }

    /// How many positional arguments the command takes: (at least, at most).
    fn positionals(self) -> (usize, usize) {
        match self {
            Self::RuleShow | Self::RuleEdit | Self::RuleDuplicate | Self::RuleMove => (1, 1),
            Self::RuleDelete | Self::RuleEnable | Self::RuleDisable => (1, 1000),
            Self::RuleRevoke | Self::ModeRename => (2, 2),
            Self::ModeShow
            | Self::ModeEdit
            | Self::ModeCopy
            | Self::ModeDelete
            | Self::ModeSet
            | Self::ModeToggle => (1, 1),
            Self::RulesRestore => (0, 1),
            _ => (0, 0),
        }
    }
}

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub dry_run: bool,
    pub yes: bool,
    pub no_input: bool,
    pub endpoint: Option<String>,
    /// Words after the command (rule IDs, …).
    pub positional: Vec<String>,
    /// Option values by option name (a flag has one empty value).
    pub opts: BTreeMap<String, Vec<String>>,
}

impl Args {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.opts
            .get(name)
            .and_then(|v| v.last())
            .map(String::as_str)
    }

    pub fn all(&self, name: &str) -> &[String] {
        self.opts.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn has(&self, name: &str) -> bool {
        self.opts.contains_key(name)
    }
}

/// Endpoint names are one pipe-name component: `\\.\pipe\<name>`.
pub fn valid_endpoint(name: &str) -> bool {
    (1..=200).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The command words at the start of `words`, and how many words they used.
fn command(words: &[&str]) -> Result<(Cmd, usize), String> {
    Ok(match words {
        [] => (Cmd::Help, 0),
        ["capabilities", ..] => (Cmd::Capabilities, 1),
        ["status", ..] => (Cmd::Status, 1),
        ["health", ..] => (Cmd::Health, 1),
        ["engine", "start" | "on", ..] => (Cmd::Engine(Request::Start), 2),
        ["engine", "pause", ..] => (Cmd::Engine(Request::Pause), 2),
        ["pause", ..] => (Cmd::Engine(Request::Pause), 1),
        ["engine", "resume", ..] => (Cmd::Engine(Request::Resume), 2),
        ["resume", ..] => (Cmd::Engine(Request::Resume), 1),
        ["engine", "stop" | "off", ..] => (Cmd::Engine(Request::Stop), 2),
        ["quit", ..] => (Cmd::Quit, 1),
        ["startup", "status", ..] => (Cmd::StartupStatus, 2),
        ["startup", "enable", ..] => (Cmd::StartupEnable, 2),
        ["startup", "disable", ..] => (Cmd::StartupDisable, 2),
        ["startup"] => (Cmd::StartupStatus, 1),
        ["rule", "list", ..] => (Cmd::RuleList, 2),
        ["rule", "show", ..] => (Cmd::RuleShow, 2),
        ["rule", "check", ..] => (Cmd::RuleCheck, 2),
        ["rule", "add", ..] => (Cmd::RuleAdd, 2),
        ["rule", "edit", ..] => (Cmd::RuleEdit, 2),
        ["rule", "delete", ..] => (Cmd::RuleDelete, 2),
        ["rule", "duplicate", ..] => (Cmd::RuleDuplicate, 2),
        ["rule", "enable", ..] => (Cmd::RuleEnable, 2),
        ["rule", "disable", ..] => (Cmd::RuleDisable, 2),
        ["rule", "move", ..] => (Cmd::RuleMove, 2),
        ["rule", "revoke", ..] => (Cmd::RuleRevoke, 2),
        ["rules", "history", ..] => (Cmd::RulesHistory, 2),
        ["rules", "backup", ..] => (Cmd::RulesBackup, 2),
        ["rules", "restore", ..] => (Cmd::RulesRestore, 2),
        ["rules", "reload", ..] => (Cmd::RulesReload, 2),
        ["rules", "draft", "apply", ..] => (Cmd::RulesDraftApply, 3),
        ["rules", "draft", "discard", ..] => (Cmd::RulesDraftDiscard, 3),
        ["rules", "draft", "show", ..] => (Cmd::RulesDraft, 3),
        ["rules", "draft"] => (Cmd::RulesDraft, 2),
        ["mode", "list", ..] => (Cmd::ModeList, 2),
        ["mode", "show", ..] => (Cmd::ModeShow, 2),
        ["mode", "add", ..] => (Cmd::ModeAdd, 2),
        ["mode", "edit", ..] => (Cmd::ModeEdit, 2),
        ["mode", "rename", ..] => (Cmd::ModeRename, 2),
        ["mode", "copy" | "duplicate", ..] => (Cmd::ModeCopy, 2),
        ["mode", "delete", ..] => (Cmd::ModeDelete, 2),
        ["mode", "set", ..] => (Cmd::ModeSet, 2),
        ["mode", "toggle", ..] => (Cmd::ModeToggle, 2),
        ["mode", "reset", ..] => (Cmd::ModeReset, 2),
        ["mode", "status", ..] => (Cmd::ModeStatus, 2),
        ["mode"] | ["mode", ..] => {
            return Err(
                "`mode` needs list, show, add, edit, rename, copy, delete, set, toggle, reset or status"
                    .into(),
            );
        }
        ["engine", ..] => return Err("`engine` needs start, pause, resume or stop".into()),
        ["rule"] | ["rule", ..] => {
            return Err(
                "`rule` needs list, show, check, add, edit, delete, duplicate, enable, disable, move or revoke"
                    .into(),
            );
        }
        ["rules"] | ["rules", ..] => {
            return Err("`rules` needs history, backup, restore, reload or draft".into());
        }
        _ => return Err(format!("unknown command `{}`", words.join(" "))),
    })
}

/// Parses `args` (without the program name). On failure returns the longest recognized
/// command name and a `usage` error, so `--json` callers still get one document.
pub fn parse(raw: &[String]) -> Result<(Cmd, Args), (String, CliError)> {
    // Pass 1: find the command words (the leading words that are not option values).
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let a = raw[i].as_str();
        if a.starts_with('-') && a.len() > 1 {
            // An option: with a known value-taking name, skip its value.
            let name = a.split_once('=').map_or(a, |(n, _)| n);
            let takes_value = !a.contains('=')
                && (RULE_FIELDS
                    .iter()
                    .chain(WRITE)
                    .any(|(n, k)| *n == name && *k != Flag)
                    || MODE_VALUES.contains(&name)
                    || matches!(
                        name,
                        "--endpoint"
                            | "--profile"
                            | "--id"
                            | "--before"
                            | "--after"
                            | "--rule"
                            | "--patch"
                            | "--to"
                    ));
            i += if takes_value { 2 } else { 1 };
            continue;
        }
        words.push(a);
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
            "rule",
            "rules",
            "mode",
        ];
        match words.first() {
            Some(w) if known.contains(w) => {
                let mut n = w.to_string();
                if let Some(second) = words.get(1)
                    && ["engine", "startup", "rule", "rules", "mode"].contains(w)
                    && second.chars().all(|c| c.is_ascii_lowercase())
                {
                    n = format!("{n}.{second}");
                }
                n
            }
            _ => "ddalgak".into(),
        }
    };
    let help = raw.iter().any(|a| a == "-h" || a == "--help");
    let version = raw.iter().any(|a| a == "-V" || a == "--version");
    let mut a = Args::default();
    if help {
        return Ok((Cmd::Help, a));
    }
    if version && words.is_empty() {
        return Ok((Cmd::Version, a));
    }
    let (cmd, used) = match command(&words) {
        Ok(c) => c,
        Err(e) => return Err((partial(&words), CliError::usage(e))),
    };
    let name = cmd.name();
    let usage = |m: String| Err((name.clone(), CliError::usage(m)));
    let allowed = cmd.options();
    // Pass 2: options and positional arguments.
    let mut seen_words = 0;
    let mut i = 0;
    while i < raw.len() {
        let arg = raw[i].as_str();
        i += 1;
        if !(arg.starts_with('-') && arg.len() > 1) {
            seen_words += 1;
            if seen_words > used {
                a.positional.push(arg.to_string());
            }
            continue;
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg, None),
        };
        match flag {
            "--json" => a.json = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "--no-input" => a.no_input = true,
            "-h" | "--help" | "-V" | "--version" => {}
            _ => {
                let f = if flag == "-y" { "--yes" } else { flag };
                let Some((_, kind)) = allowed.iter().find(|(n, _)| *n == f) else {
                    if f.starts_with('-') && !allowed.iter().any(|(n, _)| *n == f) {
                        let known = RULE_FIELDS
                            .iter()
                            .chain(WRITE)
                            .chain(MODE_FIELDS)
                            .any(|(n, _)| *n == f)
                            || [
                                "--no-icon",
                                "--no-color",
                                "--no-parent",
                                "--no-one-shot",
                                "--no-timeout",
                                "--without-rules",
                                "--delete-rules",
                                "--endpoint",
                                "--dry-run",
                                "--yes",
                                "--profile",
                                "--id",
                                "--disabled",
                                "--before",
                                "--after",
                                "--rule",
                                "--patch",
                                "--all-modes",
                                "--backup",
                            ]
                            .contains(&f);
                        return usage(if known {
                            format!("`{}` does not take {f}", name.replace('.', " "))
                        } else {
                            format!("unknown option `{f}`")
                        });
                    }
                    continue;
                };
                let value = match kind {
                    Flag => {
                        if inline.is_some() {
                            return usage(format!("{f} takes no value"));
                        }
                        String::new()
                    }
                    Value | Multi => match inline {
                        Some(v) => v,
                        None => match raw.get(i) {
                            Some(v) => {
                                i += 1;
                                v.clone()
                            }
                            None => return usage(format!("{f} needs a value")),
                        },
                    },
                };
                if *kind == Value && a.opts.contains_key(f) {
                    return usage(format!("{f} is given twice"));
                }
                a.opts.entry(f.to_string()).or_default().push(value);
            }
        }
    }
    a.dry_run = a.has("--dry-run");
    a.yes = a.has("--yes");
    if let Some(e) = a.get("--endpoint") {
        if !valid_endpoint(e) {
            return usage(
                "--endpoint takes a pipe name of letters, digits, `-`, `_` and `.`".into(),
            );
        }
        a.endpoint = Some(e.to_string());
    }
    if a.quiet && a.verbose {
        return usage("--quiet and --verbose conflict".into());
    }
    let (min, max) = cmd.positionals();
    if a.positional.len() < min {
        return usage(format!(
            "`{}` needs {}",
            name.replace('.', " "),
            match cmd {
                Cmd::RuleRevoke => "a rule ID and a permission".to_string(),
                Cmd::ModeRename => "a mode ID and a new title".to_string(),
                c if c.name().starts_with("mode.") => "a mode ID".to_string(),
                _ => "a rule ID".to_string(),
            }
        ));
    }
    if a.positional.len() > max {
        return usage(format!(
            "unexpected `{}` after `{}`",
            a.positional[max],
            name.replace('.', " ")
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

    fn pv(v: &[&str]) -> Result<(Cmd, Args), (String, CliError)> {
        parse(&v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>())
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
    fn rule_commands_take_ids_values_and_repeatable_options() {
        let (c, a) = pv(&[
            "rule",
            "add",
            "--trigger",
            "Ctrl+Shift+F13",
            "--action",
            "engine",
            "--op",
            "pause",
            "--mode",
            "base",
            "--mode",
            "nav",
            "--priority",
            "-5",
            "--title",
            "--weird title",
            "--json",
        ])
        .unwrap();
        assert_eq!(c, Cmd::RuleAdd);
        assert_eq!(a.get("--trigger"), Some("Ctrl+Shift+F13"));
        assert_eq!(a.all("--mode"), ["base", "nav"]);
        assert_eq!(
            a.get("--priority"),
            Some("-5"),
            "values may start with a dash"
        );
        assert_eq!(a.get("--title"), Some("--weird title"));
        assert!(a.positional.is_empty());
        let (c, a) = p("rule delete r-1 r-2 --yes --dry-run").unwrap();
        assert_eq!(c, Cmd::RuleDelete);
        assert_eq!(a.positional, ["r-1", "r-2"]);
        assert!(a.yes && a.dry_run);
        assert!(p("rule delete one -y").unwrap().1.yes);
        let (c, a) = p("rule revoke r-1 input-injection").unwrap();
        assert_eq!((c, a.positional.len()), (Cmd::RuleRevoke, 2));
        assert_eq!(p("rules draft").unwrap().0, Cmd::RulesDraft);
        assert_eq!(p("rules draft apply").unwrap().0, Cmd::RulesDraftApply);
        let (c, a) = p("rules restore 20261104T000000Z-r3.json --yes").unwrap();
        assert_eq!((c, a.positional.len()), (Cmd::RulesRestore, 1));
        assert_eq!(p("rule move r-1 --to 0").unwrap().1.get("--to"), Some("0"));
        let (_, a) = p("rule edit r-1 --title=New").unwrap();
        assert_eq!(
            (a.positional[0].as_str(), a.get("--title")),
            ("r-1", Some("New"))
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
        let (name, e) = p("rule show").unwrap_err();
        assert_eq!(name, "rule.show");
        assert!(e.message.contains("rule ID"));
        assert_eq!(p("rule fly").unwrap_err().0, "rule.fly");
        assert!(p("rule show a b").is_err());
        assert!(p("rule add --title a --title b").is_err());
        assert!(p("rule list --yes").is_err());
        assert!(p("rule add --trigger").is_err());
    }
}
