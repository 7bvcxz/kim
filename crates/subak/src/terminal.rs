//! `subak terminal new | list | info | rename | attach | stop | kill | rm` — the headless Terminal
//! lifecycle (docs/SESSION_MODEL.md, ADR 0020) over IPC v1.
//!
//! Every command is a short-lived client of the per-user Host. Only `terminal new` starts a Host
//! on demand (with no Terminal of its own); the read commands never start one, and `terminal
//! list` without a Host simply lists nothing. What each verb does to processes is fixed:
//!
//! | verb | processes | stored Terminal |
//! | --- | --- | --- |
//! | `attach`, detach (`Ctrl+B d`), closing this window | untouched | untouched |
//! | `stop` | graceful (`CTRL_CLOSE_EVENT`, 5 s), then forced | kept as `stopped` |
//! | `kill` | forced at once | removed |
//! | `rm` | refused while it runs; `--force` = `kill` | removed |
//!
//! `stop`, `kill` and `rm` are destructive (CLI_CONTRACT §4): they ask first, or need `--yes`
//! when they cannot ask. Only after that consent does the client send `force`, which the Host
//! requires to end a Terminal that runs more than an idle shell.

use crate::cli::{Args, EnvArg};
use crate::client::{self, Connected, Failure, Plan, Session, Why, confirm};
use crate::output::{CliError, Code};
use crate::{pty, session};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a create may take (pseudo console and process start).
const CREATE_TIMEOUT: Duration = Duration::from_secs(20);

fn host_not_running(endpoint: &str) -> CliError {
    CliError::new(
        Code::HostNotRunning,
        format!(
            "no Subak Host is serving `{}`, so there are no Terminals",
            crate::win::pipe::path(endpoint)
        ),
    )
    .hint("create one with `subak terminal new`")
}

/// Connects without starting a Host; `None` when none runs.
fn connect(endpoint: &str) -> Result<Option<Session>, CliError> {
    match Session::try_open(endpoint, Duration::from_secs(2)) {
        Ok(s) => Ok(Some(s)),
        Err(Failure {
            why: Why::NotRunning,
            ..
        }) => Ok(None),
        Err(f) => Err(f.err),
    }
}

fn require(endpoint: &str) -> Result<Session, CliError> {
    connect(endpoint)?.ok_or_else(|| host_not_running(endpoint))
}

fn target(a: &Args) -> &str {
    a.positional.first().map_or("", String::as_str)
}

fn size_json(a: &Args) -> Value {
    a.size
        .map_or(Value::Null, |(c, r)| json!({"cols": c, "rows": r}))
}

/// The `env` parameter of `terminal.create` from `--env` / `--unset-env`. `--env NAME` copies
/// this process's value, so a token never has to appear on a command line. No message ever
/// contains a value.
fn env_param(a: &Args) -> Result<Value, CliError> {
    if a.env.is_empty() {
        return Ok(Value::Null);
    }
    let mut map = serde_json::Map::new();
    let mut seen: Vec<&str> = Vec::new();
    for e in &a.env {
        let name = e.name();
        pty::check_env_name(name).map_err(CliError::usage)?;
        if seen.iter().any(|s| pty::same_var(s, name)) {
            return Err(CliError::usage(format!(
                "the variable `{name}` is given twice (names ignore case)"
            )));
        }
        seen.push(name);
        let value = match e {
            EnvArg::Set(_, v) => Value::from(v.as_str()),
            EnvArg::Unset(_) => Value::Null,
            EnvArg::Copy(_) => match std::env::var(name) {
                Ok(v) => Value::from(v),
                Err(std::env::VarError::NotPresent) => {
                    return Err(CliError::usage(format!(
                        "`--env {name}` copies `{name}` from this environment, but it is not set here"
                    ))
                    .hint(format!("give a value with --env {name}=VALUE")));
                }
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(CliError::usage(format!(
                        "the value of `{name}` in this environment is not valid Unicode"
                    )));
                }
            },
        };
        map.insert(name.to_string(), value);
    }
    Ok(Value::Object(map))
}

/// A dry run without a Host: the same checks the Host would make (name, working directory,
/// the program on the pane's PATH), on this machine, as this user, with nothing started.
fn plan_without_host(a: &Args, params: &Value) -> Result<Value, CliError> {
    let name = match a.positional.first() {
        Some(n) => {
            session::parse_name(n)
                .map_err(|e| CliError::new(Code::InvalidName, e.message))?
                .display
        }
        None => "1".into(),
    };
    let command = match a.command.split_first() {
        Some((program, args)) => session::Command {
            program: program.clone(),
            args: args.to_vec(),
        },
        None => session::split_command_line(&crate::host::default_shell()),
    };
    let env = pty::EnvOverrides::from_json(&params["env"]).map_err(CliError::usage)?;
    let cwd = match &a.cwd {
        Some(c) if !Path::new(c).is_absolute() => {
            return Err(CliError::usage("--cwd must be an absolute path"));
        }
        Some(c) => PathBuf::from(c),
        None => crate::host::profile_dir(crate::win::EnvBlock::for_current_user().ok().as_ref())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default(),
    };
    let executable = pty::plan(&pty::Spec {
        program: &command.program,
        args: &command.args,
        raw_line: None,
        cwd: &cwd,
        env: &env,
        host_vars: &[],
        size: a.size.unwrap_or(crate::host::DEFAULT_SIZE),
    })
    .map_err(|e| {
        let mut err = CliError::new(Code::SpawnFailed, e.message)
            .detail(format!("reason: {}", e.reason.as_str()));
        if let Some(h) = e.hint {
            err = err.hint(h);
        }
        err
    })?;
    Ok(
        json!({"dry_run": true, "would_start_host": true, "would_create": {
            "name": name,
            "command": {"program": command.program, "args": session::redact_args(&command.args)},
            "executable": executable.display().to_string(),
            "cwd": cwd.display().to_string(),
            "env": session::env_json(&env.names()),
            "size": params["size"].as_object().map_or_else(
                || json!({"cols": crate::host::DEFAULT_SIZE.0, "rows": crate::host::DEFAULT_SIZE.1}),
                |s| Value::Object(s.clone())),
            "remain_on_exit": a.remain_on_exit, "keep_on_failure": a.keep_on_failure,
        }}),
    )
}

/// `terminal new [<name>] [--cwd <dir>] [--env NAME[=VALUE]]… [--unset-env NAME]…
/// [--size CxR] [--remain-on-exit] [--keep-on-failure] [-- <program> [args…]]`.
pub fn new(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let command = match a.command.split_first() {
        Some((program, args)) => json!({"program": program, "args": args}),
        None => Value::Null,
    };
    let params = json!({
        "name": a.positional.first(),
        "command": command,
        "cwd": a.cwd,
        "env": env_param(a)?,
        "size": size_json(a),
        "remain_on_exit": a.remain_on_exit,
        "keep_on_failure": a.keep_on_failure,
    });
    if a.dry_run {
        let mut p = params;
        p["dry_run"] = json!(true);
        return match connect(endpoint)? {
            Some(mut s) => {
                let mut plan = s.request("terminal.create", &p)?;
                plan["would_start_host"] = json!(false);
                Ok(plan)
            }
            // Without a Host there is no name to collide with; everything else is checked.
            None => plan_without_host(a, &p),
        };
    }
    // Start on demand: a Host without a Terminal of its own (ADR 0019 §5).
    let plan = Plan {
        shell: None,
        idle_exit_s: crate::host::DEFAULT_IDLE_EXIT_S,
    };
    let Connected {
        mut session,
        started,
        ..
    } = client::connect_or_start(endpoint, &plan)?;
    let mut created = session.request_within("terminal.create", &params, CREATE_TIMEOUT)?;
    created["host"] = json!({"pid": session.host_pid, "started": started.is_some()});
    Ok(created)
}

/// `terminal list`: every Terminal of the Host, in creation order. No Host: an empty list.
pub fn list(endpoint: &str) -> Result<Value, CliError> {
    let Some(mut s) = connect(endpoint)? else {
        return Ok(json!({"terminals": [], "host": null}));
    };
    let mut r = s.request("terminal.list", &json!({"detail": "summary"}))?;
    r["host"] = json!({"pid": s.host_pid});
    Ok(r)
}

/// `terminal info <target>`.
pub fn info(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    require(endpoint)?.request("terminal.info", &json!({"target": target(a)}))
}

/// `terminal rename <target> <new name>`.
pub fn rename(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let name = a.positional.get(1).map_or("", String::as_str);
    require(endpoint)?.request(
        "terminal.rename",
        &json!({"target": target(a), "name": name, "dry_run": a.dry_run}),
    )
}

/// `terminal attach <target>`.
pub fn attach(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    if connect(endpoint)?.is_none() {
        return Err(host_not_running(endpoint));
    }
    client::attach(a, endpoint, Some(target(a)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Stop,
    Kill,
    Rm,
}

/// `terminal stop | kill | rm <target>`: shows the plan with `--dry-run`; otherwise asks for
/// consent (or `--yes`) whenever processes would end or the Terminal would be removed.
pub fn end(a: &Args, endpoint: &str, cmd: &crate::cli::Cmd) -> Result<Value, CliError> {
    use crate::cli::Cmd;
    let verb = match cmd {
        Cmd::TerminalStop => Verb::Stop,
        Cmd::TerminalKill => Verb::Kill,
        _ => Verb::Rm,
    };
    let method = match verb {
        Verb::Stop => "terminal.stop",
        Verb::Kill => "terminal.kill",
        Verb::Rm => "terminal.remove",
    };
    let mut s = require(endpoint)?;
    let t = s.request("terminal.info", &json!({"target": target(a)}))?["terminal"].clone();
    let force = verb != Verb::Rm || a.force;
    if a.dry_run {
        return s.request(
            method,
            &json!({"target": t["id"], "dry_run": true, "force": force}),
        );
    }
    let state = t["state"].as_str().unwrap_or("?");
    let live = matches!(state, "starting" | "running" | "stopping");
    let name = t["name"].as_str().unwrap_or("?");
    let id = t["id"].as_str().unwrap_or("?");
    let pane = &t["windows"][0]["panes"][0];
    let what = format!(
        "{} process(es), root pid {} ({})",
        pane["processes"],
        pane["pid"],
        pane["command"]["program"].as_str().unwrap_or("?")
    );
    match verb {
        Verb::Stop if !live => {} // nothing runs: a no-op, no consent needed
        Verb::Rm if live && !a.force => {
            return Err(CliError::new(
                Code::InvalidState,
                format!("Terminal `{name}` ({id}) is {state}; `rm` removes only Terminals that run nothing"),
            )
            .hint("stop it first with `subak terminal stop`, or pass --force (with --yes) to end its processes and remove it"));
        }
        Verb::Stop => confirm(
            a,
            &format!("Stop Terminal `{name}` ({id}) and end {what}? It is kept as stopped."),
            &format!("stopping Terminal `{name}` ends {what}"),
            "re-run with --yes, or with --dry-run to see what would end",
        )?,
        Verb::Kill | Verb::Rm if live => confirm(
            a,
            &format!("End {what} of Terminal `{name}` ({id}) now and remove the Terminal?"),
            &format!("this ends {what} of Terminal `{name}` at once and removes it"),
            "re-run with --yes, or with --dry-run to see what would end",
        )?,
        Verb::Kill | Verb::Rm => confirm(
            a,
            &format!("Remove the {state} Terminal `{name}` ({id})?"),
            &format!("removing Terminal `{name}` deletes its record"),
            "re-run with --yes, or with --dry-run to see what would be removed",
        )?,
    }
    // Target the id the user confirmed, never a name that could meanwhile mean another Terminal.
    s.request_within(
        method,
        &json!({"target": id, "force": force, "dry_run": false}),
        client::STOP_TIMEOUT,
    )
}
