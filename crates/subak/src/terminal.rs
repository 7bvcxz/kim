//! `subak new | ls | info | status | attach | detach | rename | stop | kill | rm` — the headless
//! Terminal lifecycle (docs/SESSION_MODEL.md, ADR 0020, ADR 0027, docs/SUBAK_CLI.md) over IPC v1.
//! (`subak terminal <verb>` is their deprecated Step 10 spelling.)
//!
//! Every command is a short-lived client of the per-user Host. Only `new` starts a Host on demand
//! (with no Terminal of its own, unless `--no-start`); the read commands never start one, and
//! `ls` or `status` without a Host simply report none. What each verb does to processes is fixed:
//!
//! | verb | processes | stored Terminal |
//! | --- | --- | --- |
//! | `attach`, detach (`Ctrl+B d`, `subak detach`), closing this window | untouched | untouched |
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
    .hint("create one with `subak new`")
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
    let command = match (a.command.split_first(), &a.command_line) {
        (Some((program, args)), _) => session::Command {
            program: program.clone(),
            args: args.to_vec(),
        },
        (None, Some(line)) => session::split_command_line(line),
        (None, None) => session::split_command_line(&crate::host::default_shell()),
    };
    if a.command_line.is_some() && command.program.is_empty() {
        return Err(CliError::usage("--command-line must start with a program"));
    }
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
        raw_line: a.command_line.as_deref(),
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
        json!({"dry_run": true, "would_start_host": !a.no_start, "would_create": {
            "name": name,
            "command": {"program": command.program, "args": session::redact_args(&command.args)},
            "command_line": a.command_line.as_deref().map(session::redact_line),
            "executable": executable.display().to_string(),
            "cwd": cwd.display().to_string(),
            "env": session::env_json(&env.names()),
            "size": params["size"].as_object().map_or_else(
                || json!({"cols": crate::host::DEFAULT_SIZE.0, "rows": crate::host::DEFAULT_SIZE.1}),
                |s| Value::Object(s.clone())),
            "remain_on_exit": a.remain_on_exit, "keep_on_failure": a.keep_on_failure,
            "history_limit": a.history_limit.unwrap_or(crate::screen::DEFAULT_HISTORY_LINES as u64),
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
        "command_line": a.command_line,
        "cwd": a.cwd,
        "env": env_param(a)?,
        "size": size_json(a),
        "remain_on_exit": a.remain_on_exit,
        "keep_on_failure": a.keep_on_failure,
        "history_limit": a.history_limit,
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
            // The error the real run would hit (CLI_CONTRACT §4.2).
            None if a.no_start => Err(no_start(endpoint)),
            // Without a Host there is no name to collide with; everything else is checked.
            None => plan_without_host(a, &p),
        };
    }
    let (mut session, started) = if a.no_start {
        (connect(endpoint)?.ok_or_else(|| no_start(endpoint))?, None)
    } else {
        // Start on demand: a Host without a Terminal of its own (ADR 0019 §5), under the
        // per-endpoint start lock, so concurrent `new` calls share one Host.
        let plan = Plan {
            shell: None,
            idle_exit_s: crate::host::DEFAULT_IDLE_EXIT_S,
        };
        let Connected {
            session, started, ..
        } = client::connect_or_start(endpoint, &plan)?;
        (session, started)
    };
    let mut created = session.request_within("terminal.create", &params, CREATE_TIMEOUT)?;
    created["host"] = json!({"pid": session.host_pid, "started": started.is_some()});
    if !a.attach {
        return Ok(created);
    }
    // `new --attach`: the creation is reported as a notice, then this terminal attaches.
    drop(session);
    if !a.quiet {
        eprintln!(
            "[subak: created Terminal `{}` ({}){}]",
            created["name"].as_str().unwrap_or("?"),
            created["terminal_id"].as_str().unwrap_or("?"),
            if started.is_some() {
                format!("; started Host pid {}", created["host"]["pid"])
            } else {
                String::new()
            }
        );
    }
    let id = created["terminal_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let attached = client::attach(a, endpoint, Some(&id))?;
    Ok(json!({"created": created, "attach": attached}))
}

fn no_start(endpoint: &str) -> CliError {
    CliError::new(
        Code::HostNotRunning,
        format!(
            "no Subak Host is serving `{}`, and --no-start forbids starting one",
            crate::win::pipe::path(endpoint)
        ),
    )
    .hint("run without --no-start to start one on demand, or start it with `subak host start --no-shell`")
}

/// `ls`: every Terminal of the Host, in creation order. No Host: an empty list.
pub fn list(endpoint: &str) -> Result<Value, CliError> {
    let Some(mut s) = connect(endpoint)? else {
        return Ok(json!({"terminals": [], "host": null}));
    };
    let mut r = s.request("terminal.list", &json!({"detail": "summary"}))?;
    r["host"] = json!({"pid": s.host_pid});
    Ok(r)
}

/// `status`: the Host (or that none runs), every Terminal, and every client, in one answer.
/// Starts nothing; exits 0 also without a Host.
pub fn status(endpoint: &str) -> Result<Value, CliError> {
    let Some(mut s) = connect(endpoint)? else {
        return Ok(json!({
            "running": false, "endpoint": crate::win::pipe::path(endpoint),
            "host": null, "terminals": [], "clients": null,
        }));
    };
    let st = s.request("host.status", &json!({}))?;
    let list = s.request("terminal.list", &json!({"detail": "summary"}))?;
    Ok(json!({
        "running": true, "endpoint": st["endpoint"], "host": st["host"],
        "terminals": list["terminals"], "clients": st["clients"],
        "scrollback": st["scrollback"],
    }))
}

/// `info <target>`: one Terminal; exit 3 (`target_not_found`) when there is none (tmux
/// `has-session`).
pub fn info(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    require(endpoint)?.request("terminal.info", &json!({"target": target(a)}))
}

/// `detach [<target>] [--client <id>]`: ends attachments of clients — every client of the
/// Terminal, one client, or that client's attachment to the Terminal — and nothing else. Inside a
/// pane, no target means the pane's own Terminal (`SUBAK_TERMINAL_ID`, ADR 0010 §1). Detaching
/// never stops a process, a Terminal or the Host.
pub fn detach(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let target = match (a.positional.first(), a.client) {
        (Some(t), _) => Some(t.clone()),
        (None, Some(_)) => None,
        (None, None) => match std::env::var("SUBAK_TERMINAL_ID") {
            Ok(id) if session::is_id(&id, 't') => Some(id),
            _ => {
                return Err(CliError::usage(
                    "`detach` needs a Terminal or --client <id> outside a Subak pane",
                )
                .hint("e.g. `subak detach work` detaches every client of `work`; `subak status` lists client ids"));
            }
        },
    };
    let mut s = require(endpoint)?;
    let mut r = s.request(
        "client.detach",
        &json!({"target": target, "client_id": a.client, "dry_run": a.dry_run}),
    )?;
    r["host"] = json!({"pid": s.host_pid});
    Ok(r)
}

/// The pane `capture`, `history` and `clear-history` mean: the named target; inside a pane
/// without one, that pane (`SUBAK_PANE_ID`, ADR 0010 §1, like tmux's current pane); else none
/// (the Host's only Terminal).
fn screen_target(a: &Args) -> Value {
    match a.positional.first() {
        Some(t) => json!(t),
        None => match std::env::var("SUBAK_PANE_ID") {
            Ok(id) if session::is_id(&id, 'p') => json!(id),
            _ => Value::Null,
        },
    }
}

fn line_json(l: Option<crate::cli::Line>) -> Value {
    match l {
        None => Value::Null,
        Some(crate::cli::Line::Edge) => json!("-"),
        Some(crate::cli::Line::Number(n)) => json!(n),
    }
}

/// Most `pane.capture` requests one `capture` makes to collect a long history.
const CAPTURE_PAGES: usize = 64;

/// `capture [<target>]` (tmux `capture-pane -p`): the screen, or a range of history and screen
/// lines, as text (or with `--vt` escape sequences); secret-looking values are hidden unless
/// `--no-redact`. A long range is collected in several requests (each answer is one message).
pub fn capture(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = require(endpoint)?;
    let start = if a.history {
        json!("-")
    } else {
        line_json(a.start)
    };
    let mut params = json!({
        "target": screen_target(a),
        "start": start,
        "end": line_json(a.end),
        "format": if a.vt { "vt" } else { "text" },
        "join": a.join,
        "redact": !a.no_redact,
    });
    let mut r = s.request("pane.capture", &params)?;
    // Later pages name the pane by id, so they read the same pane.
    params["target"] = r["pane_id"].clone();
    let mut lines: Vec<Value> = r["lines"].as_array().cloned().unwrap_or_default();
    let mut redacted = r["redacted"].as_u64().unwrap_or(0);
    let mut requests = 1;
    let end = r["end"].clone();
    while r["truncated"] == true && requests < CAPTURE_PAGES {
        let Some(first) = r["start"].as_i64() else {
            break;
        };
        params["end"] = json!(first - 1);
        r = s.request("pane.capture", &params)?;
        requests += 1;
        redacted += r["redacted"].as_u64().unwrap_or(0);
        let mut page = r["lines"].as_array().cloned().unwrap_or_default();
        page.extend(lines);
        lines = page;
    }
    let name =
        s.request("terminal.info", &json!({"target": r["terminal_id"]}))?["terminal"]["name"]
            .clone();
    r["lines"] = Value::Array(lines);
    r["end"] = end;
    r["redacted"] = json!(redacted);
    r["name"] = name;
    r["requests"] = json!(requests);
    Ok(r)
}

/// `history [<target>]`: what the Host keeps of a Terminal's output — counters, limits and the
/// screen, never content (docs/SCROLLBACK.md).
pub fn history(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = require(endpoint)?;
    // An empty range (0 to -1) returns the counters and the screen without any line.
    let r = s.request(
        "pane.capture",
        &json!({"target": screen_target(a), "start": 0, "end": -1}),
    )?;
    let t = s.request("terminal.info", &json!({"target": r["terminal_id"]}))?["terminal"].clone();
    Ok(json!({
        "terminal_id": r["terminal_id"], "name": t["name"], "state": t["state"],
        "pane_id": r["pane_id"], "history": r["history"], "screen": r["screen"],
    }))
}

/// `clear-history [<target>]` (tmux `clear-history`): deletes the history the Host keeps for a
/// pane; the screen stays. It deletes data, so it asks first (or needs `--yes`).
pub fn clear_history(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let mut s = require(endpoint)?;
    let plan = s.request(
        "pane.clear_history",
        &json!({"target": screen_target(a), "dry_run": true}),
    )?;
    let mut would = plan["would_clear"].clone();
    let name =
        s.request("terminal.info", &json!({"target": would["terminal_id"]}))?["terminal"]["name"]
            .clone();
    would["name"] = name.clone();
    if a.dry_run {
        return Ok(json!({"dry_run": true, "would_clear": would}));
    }
    let shown = name.as_str().unwrap_or("?");
    let lines = would["lines"].as_u64().unwrap_or(0);
    if lines > 0 {
        confirm(
            a,
            &format!(
                "Delete the {lines} line(s) of history the Host keeps for Terminal `{shown}`?"
            ),
            &format!("clearing deletes {lines} line(s) of history of Terminal `{shown}`"),
            "re-run with --yes, or with --dry-run to see what would be cleared",
        )?;
    }
    // The pane the user confirmed, by id.
    let mut r = s.request(
        "pane.clear_history",
        &json!({"target": would["pane_id"], "dry_run": false}),
    )?;
    r["name"] = name;
    Ok(r)
}

/// `rename <target> <new name>`.
pub fn rename(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    let name = a.positional.get(1).map_or("", String::as_str);
    require(endpoint)?.request(
        "terminal.rename",
        &json!({"target": target(a), "name": name, "dry_run": a.dry_run}),
    )
}

/// `attach [<target>]`: without a target, the Host's only running Terminal
/// (`ambiguous_target` with the candidates when there are several).
pub fn attach(a: &Args, endpoint: &str) -> Result<Value, CliError> {
    if connect(endpoint)?.is_none() {
        return Err(host_not_running(endpoint));
    }
    client::attach(a, endpoint, a.positional.first().map(String::as_str))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Stop,
    Kill,
    Rm,
}

/// `stop | kill | rm <target>`: shows the plan with `--dry-run`; otherwise asks for consent (or
/// `--yes`) whenever processes would end or the Terminal would be removed.
pub fn end(a: &Args, endpoint: &str, cmd: crate::cli::Cmd) -> Result<Value, CliError> {
    use crate::cli::Cmd;
    let verb = match cmd {
        Cmd::Stop => Verb::Stop,
        Cmd::Kill => Verb::Kill,
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
            .hint("stop it first with `subak stop`, or pass --force (with --yes) to end its processes and remove it"));
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
