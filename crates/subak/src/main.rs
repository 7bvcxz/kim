//! `subak` — Subak terminal CLI.
//!
//! Pre-alpha. Besides `capabilities` (docs/CLI_CONTRACT.md §9) it contains the **Host
//! supervisor and IPC v1** (Step 09, ADR 0019, docs/contracts/IPC_PROTOCOL.md) built on the
//! Step 08 ConPTY proof (ADR 0016): `subak host start|status|health|attach|stop|serve`. A
//! detached per-user Host serves an identity-checked named pipe, starts on demand under a start
//! lock, and exits after an idle grace period; until the session model (Step 10) it owns at most
//! one prototype pane. There are no Terminals, windows, GUI or recovery yet. Nothing starts by
//! itself, and no command opens a window. The real CLI (Step 12) follows the same contract.

// Portable modules: compiled and unit-tested on every target, but until the Linux Host (M4,
// ADR 0011) only the Windows Host and clients use most of them.
#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
#[cfg_attr(not(windows), allow(dead_code))]
mod output;
#[cfg_attr(not(windows), allow(dead_code))]
mod proto;

/// The schema validator of `cargo xtask schema-check`, so unit tests can check every IPC message
/// this build constructs against docs/contracts/schemas (ADR 0015).
#[cfg(test)]
#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

#[cfg(windows)]
mod client;
#[cfg(windows)]
mod host;
#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

use cli::{Args, Cmd};
use output::{CliError, Code};
use serde_json::Value;
use std::io::Write;
use std::process::ExitCode;

const HELP: &str = "\
subak — Subak persistent terminal (pre-alpha; Host supervisor and IPC v1, one prototype pane)

USAGE:
    subak [--help | --version]
    subak capabilities [--json]
    subak host start  [--endpoint <name>] [--shell <command> | --no-shell] [--idle-exit <secs>]
                      [--dry-run] [--json]
    subak host status [--endpoint <name>] [--json]
    subak host health [--endpoint <name>] [--json]
    subak host attach [--endpoint <name>] [--size <cols>x<rows>] [--dry-run [--json]]
    subak host stop   [--endpoint <name>] [--yes] [--dry-run] [--json]
    subak host serve  [--endpoint <name>] [--shell <command> | --no-shell] [--idle-exit <secs>]
                      [--dry-run] [--json]

HOST (Windows only; docs/contracts/IPC_PROTOCOL.md, docs/CONPTY_PROOF.md):
    host start    Start a detached per-user Host, unless one already serves the endpoint
                  (`host_already_running`). Starters take a per-endpoint lock, so several at
                  once give one Host. It gets your logon environment, not this terminal's.
                  Until Step 10 it owns at most one pane: a pseudo console (ConPTY) running one
                  shell (default %ComSpec%) in a kill-on-close job, or none with --no-shell.
                  The Host runs while it owns a live pane or has a client, then exits after the
                  idle grace period. Closing this window or any client never ends it.
    host status   Host and pane process ids, protocol, job breakaway, environment and clients.
    host health   Heartbeat: check that the endpoint is served by subak.exe of this user, do
                  the handshake and report the answer time. Exit 0 only when healthy.
    host attach   Connect this terminal to the pane. Ctrl+B d detaches (Ctrl+B Ctrl+B sends
                  Ctrl+B). With redirected input, the bytes are sent as typed and the end of
                  input detaches. Detaching or closing this window leaves the shell running.
    host stop     Stop the Host. Ending a live pane and every process in its job is
                  destructive: asks first, or needs --yes when it cannot ask.
    host serve    Run the Host in this process (what `host start` launches detached).

    Every client checks, before sending anything, that the pipe is served by subak.exe of
    this user (`host_identity_mismatch` otherwise); the Host checks each client's token. A
    Host that does not answer is reported (`host_unresponsive`), never ended or replaced.

OPTIONS:
    --endpoint <name>  Pipe name under \\\\.\\pipe\\ (default: mouv-subak-<user SID>-s<session>).
                       Use your own name for experiments; it never touches the default Host.
    --shell <command>  Command line of the pane's shell (start/serve).
    --no-shell         Start a Host that owns no pane (start/serve).
    --idle-exit <s>    Seconds the Host waits with no pane and no client before it exits
                       (0..86400, default 10).
    --size <c>x<r>     Pseudo console size when attaching from a script (2..1000 each).
    --dry-run          Show what would happen; change nothing.
    -y, --yes          Confirm a destructive command (stop) without asking.
    --json             One JSON document on stdout (docs/CLI_CONTRACT.md §7).
    --no-input         Never ask; fail with `confirmation_required` instead.
    -q, --quiet / -v, --verbose   Less / more diagnostics on stderr.

Pre-alpha: there are no Terminals, windows, GUI or recovery yet (docs/PRODUCT_REQUIREMENTS.md).
No command opens a window, installs anything or uses the network; no Windows service exists.
";

/// The capabilities document (`kim.capabilities` v1). Its `version` must equal the package
/// version; a test checks it and keeps docs/contracts/examples/tool-capabilities-v1/subak.json
/// identical.
const CAPABILITIES: &str = include_str!("capabilities.json");

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json_flag = raw.iter().any(|a| a == "--json");
    let (cmd, args) = match cli::parse(&raw) {
        Ok(p) => p,
        Err((name, e)) => return fail(&name, &e, json_flag),
    };
    let mut out = std::io::stdout().lock();
    match cmd {
        Cmd::Help => {
            let _ = write!(out, "{HELP}");
            ExitCode::SUCCESS
        }
        Cmd::Version => {
            let _ = writeln!(out, "subak {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Cmd::Capabilities if args.json => {
            let data: Value = serde_json::from_str(CAPABILITIES).expect("valid built-in JSON");
            let _ = writeln!(out, "{}", output::success("capabilities", &data, &[]));
            ExitCode::SUCCESS
        }
        Cmd::Capabilities => {
            let _ = write!(
                out,
                "subak {} — pre-alpha (CLI contract v1, IPC protocol v1)\n\
                 Commands: capabilities; host start|status|health|attach|stop|serve \
                 (Host supervisor, one prototype pane per Host).\n\
                 No Terminals, windows, GUI or recovery yet.\n",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        _ => {
            drop(out);
            match run_host(&cmd, &args) {
                Ok(data) => {
                    let warnings = host_warnings(&cmd, &data);
                    if args.json {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{}", output::success(cmd.name(), &data, &warnings));
                    } else {
                        if !args.quiet {
                            for w in &warnings {
                                eprintln!("subak: warning: {w}");
                            }
                        }
                        human(&cmd, &data, &args);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    if args.verbose {
                        eprintln!(
                            "subak: {} failed with {} (exit {})",
                            cmd.name(),
                            e.code.as_str(),
                            e.code.exit_code()
                        );
                    }
                    fail(cmd.name(), &e, args.json)
                }
            }
        }
    }
}

fn fail(command: &str, e: &CliError, json: bool) -> ExitCode {
    if json {
        let _ = writeln!(std::io::stdout().lock(), "{}", output::failure(command, e));
    } else {
        eprint!("{}", output::human_error(e));
    }
    ExitCode::from(e.code.exit_code())
}

#[cfg(windows)]
fn run_host(cmd: &Cmd, a: &Args) -> Result<Value, CliError> {
    let endpoint = client::endpoint(a)?;
    if a.verbose {
        eprintln!("subak: endpoint {}", win::pipe::path(&endpoint));
    }
    match cmd {
        Cmd::HostServe => {
            let opts = client::serve_opts(a, endpoint);
            if a.dry_run {
                return Ok(client::serve_plan(&opts));
            }
            host::serve(&opts)
        }
        Cmd::HostStart => client::start(a, &endpoint),
        Cmd::HostStatus => client::status(&endpoint),
        Cmd::HostHealth => client::health(&endpoint),
        Cmd::HostStop => client::stop(a, &endpoint),
        Cmd::HostAttach => client::attach(a, &endpoint),
        _ => Err(CliError::new(Code::Internal, "not a host command")),
    }
}

#[cfg(not(windows))]
fn run_host(_cmd: &Cmd, _a: &Args) -> Result<Value, CliError> {
    // ADR 0011: no Linux implementation before M4; the interface compiles to this stub.
    Err(CliError::new(
        Code::UnsupportedPlatform,
        "the Subak Host prototype needs Windows (ConPTY); Linux support is planned for M4",
    ))
}

/// Risk R-03 (measured in Step 08, ADR 0016): a Host inside a job it could not leave dies when
/// that job is closed with kill-on-close.
fn host_warnings(cmd: &Cmd, d: &Value) -> Vec<String> {
    let mut w = Vec::new();
    if matches!(cmd, Cmd::HostStart | Cmd::HostStatus) && d["host"]["environment"] == "inherited" {
        w.push(
            "the user's logon environment could not be built, so the Host inherited its \
             launcher's environment"
                .to_string(),
        );
    }
    if !matches!(cmd, Cmd::HostStart | Cmd::HostStatus) || d["host"]["in_job"] != true {
        return w;
    }
    w.push(if d["host"]["job_breakaway"] == false {
        "a job of the launching terminal forbids breakaway, so the Host started inside it; \
         closing that terminal or job may end the Host and its shell (risk R-03)"
            .to_string()
    } else if d["host"]["job"]["kill_on_close"] == false {
        "the Host runs inside a job object it could not leave; that job does not end its \
         processes when closed, but an enclosing job may (risk R-03)"
            .to_string()
    } else {
        "the Host runs inside a job object it could not leave; if that job is closed with \
         kill-on-close, the Host and its shell end (risk R-03)"
            .to_string()
    });
    w
}

/// Human output on stdout (results) and stderr (notices).
fn human(cmd: &Cmd, d: &Value, a: &Args) {
    let mut out = std::io::stdout().lock();
    if d["dry_run"] == true {
        let plan = d
            .as_object()
            .and_then(|o| o.iter().find(|(k, _)| k.starts_with("would_")))
            .map(|(k, v)| (k.clone(), v.clone()));
        if let Some((k, v)) = plan {
            let _ = writeln!(out, "dry run ({}), nothing changed:", k.replace('_', " "));
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_default()
            );
        }
        return;
    }
    let _ = match cmd {
        Cmd::HostStart => writeln!(
            out,
            "Host started: pid {} (job breakaway: {}), shell pid {} ({})\nendpoint: {}",
            d["host"]["pid"],
            d["host"]["job_breakaway"],
            d["pane"]["pid"],
            d["pane"]["command"].as_str().unwrap_or("?"),
            d["endpoint"].as_str().unwrap_or("?"),
        ),
        Cmd::HostStatus => {
            let pane = if d["pane"].is_null() {
                "no pane (started with --no-shell)".to_string()
            } else {
                format!(
                    "shell pid {} running: {} ({}), {}x{}, {} process(es) in its job",
                    d["pane"]["pid"],
                    d["pane"]["running"],
                    d["pane"]["command"].as_str().unwrap_or("?"),
                    d["pane"]["size"]["cols"],
                    d["pane"]["size"]["rows"],
                    d["pane"]["job_processes"],
                )
            };
            writeln!(
                out,
                "Host pid {} since {} (session {}, protocol {}, job breakaway: {}, in a job: {}, \
                 idle exit after {} s)\n{pane}\nclients: {} connected, {} attached\nendpoint: {}",
                d["host"]["pid"],
                d["host"]["started_at"].as_str().unwrap_or("?"),
                d["host"]["session_id"],
                d["protocol"],
                d["host"]["job_breakaway"],
                d["host"]["in_job"],
                d["host"]["idle_exit_s"],
                d["clients"]["connected"],
                d["clients"]["attached"],
                d["endpoint"].as_str().unwrap_or("?"),
            )
        }
        Cmd::HostHealth => writeln!(
            out,
            "healthy: Host pid {} (protocol {}, up {} s), verified as {} of this user; answered in {} ms",
            d["host"]["pid"],
            d["host"]["protocol"],
            d["host"]["uptime_s"],
            d["identity"]["image"].as_str().unwrap_or("?"),
            d["latency_ms"],
        ),
        Cmd::HostStop if d["shell"].is_null() => writeln!(
            out,
            "Host pid {} stopped (exited: {}); it owned no pane",
            d["host"]["pid"], d["host"]["exited"],
        ),
        Cmd::HostStop => writeln!(
            out,
            "Host pid {} stopped (exited: {}); shell pid {} ended with code {}{}",
            d["host"]["pid"],
            d["host"]["exited"],
            d["shell"]["pid"],
            d["shell"]["exit_code"],
            if d["shell"]["forced"] == true {
                " (forced after the grace period)"
            } else {
                ""
            },
        ),
        Cmd::HostServe => writeln!(
            out,
            "Host ended ({}); shell pid {} exit code {}",
            d["reason"].as_str().unwrap_or("?"),
            d["shell"]["pid"],
            d["shell"]["exit_code"],
        ),
        Cmd::HostAttach => {
            if !a.quiet {
                if d["detached"] == true {
                    eprintln!(
                        "[subak: detached; the shell keeps running in Host pid {}]",
                        d["host"]["pid"]
                    );
                } else if let Some(reason) = d["host"]["stopped"].as_str() {
                    eprintln!(
                        "[subak: the shell ended (exit code {}); the Host stopped ({reason})]",
                        d["shell_exit_code"],
                    );
                } else {
                    eprintln!(
                        "[subak: the shell ended (exit code {}); the Host keeps running until \
                         its idle grace period ends]",
                        d["shell_exit_code"],
                    );
                }
            }
            Ok(())
        }
        _ => Ok(()),
    };
}
