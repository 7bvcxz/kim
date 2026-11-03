//! `subak` — Subak terminal CLI.
//!
//! Pre-alpha. Besides `capabilities` (docs/CLI_CONTRACT.md §9) it contains the **Step 08 ConPTY
//! Host prototype** (docs/CONPTY_PROOF.md, ADR 0016): `subak host serve|start|status|attach|stop`.
//! A detached per-user Host owns one pseudo console with one shell inside a kill-on-close job;
//! independent CLI clients talk to it over a prototype named pipe. There are no Terminals,
//! panes, sessions model, GUI or recovery yet (Steps 09+). Nothing starts by itself: the Host
//! runs only after an explicit `subak host start` (or `host serve`), and no command opens a
//! window. The real CLI (Step 12) follows the same contract.

// Portable modules: compiled and unit-tested on every target, but until the Linux Host (M4,
// ADR 0011) only the Windows Host and clients use most of them.
#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
#[cfg_attr(not(windows), allow(dead_code))]
mod output;
#[cfg_attr(not(windows), allow(dead_code))]
mod proto;

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
subak — Subak persistent terminal (pre-alpha; Step 08 ConPTY Host prototype)

USAGE:
    subak [--help | --version]
    subak capabilities [--json]
    subak host start  [--endpoint <name>] [--shell <command>] [--dry-run] [--json]
    subak host status [--endpoint <name>] [--json]
    subak host attach [--endpoint <name>] [--size <cols>x<rows>] [--dry-run [--json]]
    subak host stop   [--endpoint <name>] [--yes] [--dry-run] [--json]
    subak host serve  [--endpoint <name>] [--shell <command>] [--dry-run] [--json]

HOST PROTOTYPE (Windows only; docs/CONPTY_PROOF.md):
    host start    Start a detached Host that owns one pseudo console (ConPTY) running one
                  shell (default %ComSpec%) inside a kill-on-close job. Returns when the Host
                  serves its pipe. The Host keeps running after this command, its console
                  window or any client closes; only `host stop`, the shell exiting, logoff or
                  shutdown end it.
    host status   Show the Host and shell process ids, job breakaway, size and clients.
    host attach   Connect this terminal to the shell. Ctrl+B d detaches (Ctrl+B Ctrl+B sends
                  Ctrl+B). With redirected input, the bytes are sent as typed and the end of
                  input detaches. Detaching or closing this window leaves the shell running.
    host stop     End the shell and every process in its job, then the Host. Destructive:
                  asks first, or needs --yes when it cannot ask.
    host serve    Run the Host in this process (what `host start` launches detached).

OPTIONS:
    --endpoint <name>  Pipe name under \\\\.\\pipe\\ (default: mouv-subak-proto-<user SID>-s<session>).
                       Use your own name for experiments; it never touches the default Host.
    --shell <command>  Command line of the shell (start/serve).
    --size <c>x<r>     Pseudo console size when attaching from a script (2..1000 each).
    --dry-run          Show what would happen; change nothing.
    -y, --yes          Confirm a destructive command (stop) without asking.
    --json             One JSON document on stdout (docs/CLI_CONTRACT.md §7).
    --no-input         Never ask; fail with `confirmation_required` instead.
    -q, --quiet / -v, --verbose   Less / more diagnostics on stderr.

This is a prototype: the IPC is a local, owner-only named pipe without the Step 09 identity
checks; there are no Terminals, panes, GUI or recovery yet (docs/PRODUCT_REQUIREMENTS.md).
No command opens a window, installs anything or uses the network.
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
                "subak {} — pre-alpha (CLI contract v1)\n\
                 Commands: capabilities; Step 08 ConPTY Host prototype: host start|status|attach|stop|serve.\n\
                 No Terminals, panes, GUI or recovery yet.\n",
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
            let opts = host::ServeOpts {
                endpoint,
                shell: a.shell.clone().unwrap_or_else(host::default_shell),
                job_breakaway: a.job_breakaway,
            };
            if a.dry_run {
                return Ok(serde_json::json!({"dry_run": true, "would_serve": {
                    "endpoint": win::pipe::path(&opts.endpoint),
                    "shell": opts.shell,
                    "cwd": host::profile_dir().map(|p| p.display().to_string()),
                }}));
            }
            host::serve(&opts)
        }
        Cmd::HostStart => client::start(a, &endpoint),
        Cmd::HostStatus => client::status(&endpoint),
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
    if !matches!(cmd, Cmd::HostStart | Cmd::HostStatus) || d["host"]["in_job"] != true {
        return Vec::new();
    }
    vec![if d["host"]["job_breakaway"] == false {
        "a job of the launching terminal forbids breakaway, so the Host started inside it; \
         closing that terminal or job may end the Host and its shell (risk R-03)"
            .to_string()
    } else {
        "the Host runs inside a job object it could not leave; if that job is closed with \
         kill-on-close, the Host and its shell end (risk R-03)"
            .to_string()
    }]
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
        Cmd::HostStatus => writeln!(
            out,
            "Host pid {} since {} (session {}, job breakaway: {}, in a job: {})\n\
             shell pid {} running: {} ({}), {}x{}, {} process(es) in its job\n\
             clients: {} connected, {} attached\nendpoint: {}",
            d["host"]["pid"],
            d["host"]["started_at"].as_str().unwrap_or("?"),
            d["host"]["session_id"],
            d["host"]["job_breakaway"],
            d["host"]["in_job"],
            d["pane"]["pid"],
            d["pane"]["running"],
            d["pane"]["command"].as_str().unwrap_or("?"),
            d["pane"]["size"]["cols"],
            d["pane"]["size"]["rows"],
            d["pane"]["job_processes"],
            d["clients"]["connected"],
            d["clients"]["attached"],
            d["endpoint"].as_str().unwrap_or("?"),
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
                } else {
                    eprintln!(
                        "[subak: the shell ended (exit code {}); the Host stopped ({})]",
                        d["shell_exit_code"],
                        d["host"]["stopped"].as_str().unwrap_or("?")
                    );
                }
            }
            Ok(())
        }
        _ => Ok(()),
    };
}
