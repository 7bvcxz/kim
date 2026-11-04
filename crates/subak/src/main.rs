//! `subak` — Subak terminal CLI.
//!
//! Pre-alpha. Besides `capabilities` (docs/CLI_CONTRACT.md §9) it contains the **Host
//! supervisor and IPC v1** (Step 09, ADR 0019, docs/contracts/IPC_PROTOCOL.md) and the **Terminal
//! session lifecycle** (Step 10, ADR 0020, docs/SESSION_MODEL.md): a detached per-user Host owns
//! any number of named Terminals, each with one window and one pane in its own pseudo console and
//! job, and `subak terminal new|list|info|rename|attach|stop|kill|rm` manage them headlessly. The
//! Host serves an identity-checked named pipe, starts on demand under a start lock, and exits
//! after an idle grace period. There is no GUI, split, persistence or recovery yet. Nothing starts
//! by itself, and no command opens a window. Step 12 extends this CLI under the same contract.

// Portable modules: compiled and unit-tested on every target, but until the Linux Host (M4,
// ADR 0011) only the Windows Host and clients use most of them.
#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
#[cfg_attr(not(windows), allow(dead_code))]
mod output;
#[cfg_attr(not(windows), allow(dead_code))]
mod proto;
#[cfg_attr(not(windows), allow(dead_code))]
mod session;

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
mod terminal;
#[cfg(windows)]
#[allow(unsafe_code)]
mod win;

use cli::{Args, Cmd};
use output::{CliError, Code};
use serde_json::Value;
use std::io::Write;
use std::process::ExitCode;

const HELP: &str = "\
subak — Subak persistent terminal (pre-alpha; Host, IPC v1 and the Terminal lifecycle)

USAGE:
    subak [--help | --version]
    subak capabilities [--json]
    subak terminal new    [<name>] [--cwd <dir>] [--size <cols>x<rows>] [--remain-on-exit]
                          [--keep-on-failure] [--dry-run] [--json] [-- <program> [args...]]
    subak terminal list   [--json]
    subak terminal info   <target> [--json]
    subak terminal rename <target> <new-name> [--dry-run] [--json]
    subak terminal attach <target> [--size <cols>x<rows>] [--dry-run [--json]]
    subak terminal stop   <target> [--yes] [--dry-run] [--json]
    subak terminal kill   <target> [--yes] [--dry-run] [--json]
    subak terminal rm     <target> [--force] [--yes] [--dry-run] [--json]
    subak host start  [--shell <command> | --no-shell] [--idle-exit <secs>] [--dry-run] [--json]
    subak host status [--json]
    subak host health [--json]
    subak host attach [--size <cols>x<rows>] [--dry-run [--json]]
    subak host stop   [--kill-terminals] [--yes] [--dry-run] [--json]
    subak host serve  [--shell <command> | --no-shell] [--idle-exit <secs>] [--dry-run] [--json]
    (every command also takes --endpoint <name>)

TERMINALS (Windows only; docs/SESSION_MODEL.md):
    A Terminal is a named workspace in the Host with a stable id (t-...), one window and one pane
    (Steps 21-22 add more). Each pane runs in its own pseudo console and kill-on-close job, so
    Terminals never share processes. <target> is a Terminal name (exact; case is ignored) or id.
    terminal new     Create a Terminal running <program> (default: %ComSpec%) in your profile
                     folder or --cwd. Without a name it gets the lowest free number. Starts a
                     Host if none runs. A name already in use (ignoring case) is refused with
                     `name_conflict`, never suffixed. A program that cannot start leaves nothing
                     behind, unless --keep-on-failure keeps it as `failed`.
    terminal list    Every Terminal with its state, pid and clients. Starts nothing.
    terminal info    One Terminal with its window and pane.
    terminal rename  Rename a Terminal; its id never changes.
    terminal attach  Connect this terminal to the Terminal's pane. Ctrl+B d detaches; detaching
                     or closing this window never stops anything.
    terminal stop    End its processes: close the pseudo console (programs get CTRL_CLOSE_EVENT),
                     wait 5 s, then end whatever is left in its job. The Terminal is kept as
                     `stopped`. Destructive: asks first, or needs --yes.
    terminal kill    End its processes at once and remove the Terminal. Asks, or needs --yes.
    terminal rm      Remove a Terminal that runs nothing (stopped or failed). --force also ends
                     a running one (as kill). Asks, or needs --yes.
    There is no ambiguous `close`: detach looks away, stop ends processes and keeps the record,
    kill and rm remove it. A Terminal whose program exits by itself is removed, unless it was
    created with --remain-on-exit (then `stopped`). Until Step 29 nothing is saved: Terminals
    end with their Host, and stopped ones vanish when the Host exits.

HOST (Windows only; docs/contracts/IPC_PROTOCOL.md, docs/CONPTY_PROOF.md):
    host start    Start a detached per-user Host, unless one already serves the endpoint
                  (`host_already_running`). Starters take a per-endpoint lock, so several at
                  once give one Host. It gets your logon environment, not this terminal's.
                  With --shell (default %ComSpec%) it starts with one Terminal running that
                  command line; --no-shell starts it with none. The Host runs while it owns a
                  live Terminal or has a client, then exits after the idle grace period.
                  Closing this window or any client never ends it.
    host status   Host process id, protocol, job breakaway, environment, Terminals, clients.
    host health   Heartbeat: check that the endpoint is served by subak.exe of this user, do
                  the handshake and report the answer time. Exit 0 only when healthy.
    host attach   Connect this terminal to the Host's only running pane (with several, use
                  `terminal attach <target>`). Ctrl+B d detaches (Ctrl+B Ctrl+B sends Ctrl+B).
                  With redirected input, the bytes are sent as typed and the end of input
                  detaches.
    host stop     Stop the Host. With live Terminals it refuses (`terminals_running`) unless
                  --kill-terminals is given, and then asks first, or needs --yes.
    host serve    Run the Host in this process (what `host start` launches detached).

    Every client checks, before sending anything, that the pipe is served by subak.exe of
    this user (`host_identity_mismatch` otherwise); the Host checks each client's token. A
    Host that does not answer is reported (`host_unresponsive`), never ended or replaced.

OPTIONS:
    --endpoint <name>  Pipe name under \\\\.\\pipe\\ (default: mouv-subak-<user SID>-s<session>).
                       Use your own name for experiments; it never touches the default Host.
    --shell <command>  Command line of the Host's first Terminal (start/serve).
    --no-shell         Start a Host that owns no Terminal (start/serve).
    --idle-exit <s>    Seconds the Host waits with no live Terminal and no client before it
                       exits (0..86400, default 10).
    --size <c>x<r>     Pseudo console size (new, attach; 2..1000 each).
    --cwd <dir>        Working directory of a new Terminal (an absolute path).
    --remain-on-exit   Keep a new Terminal as `stopped` when its program exits by itself.
    --keep-on-failure  Keep a Terminal whose program could not start, as `failed`.
    --force            rm: also end a running Terminal.
    --kill-terminals   host stop: end every live Terminal with the Host.
    --dry-run          Show what would happen; change nothing.
    -y, --yes          Confirm a destructive command without asking.
    --json             One JSON document on stdout (docs/CLI_CONTRACT.md §7).
    --no-input         Never ask; fail with `confirmation_required` instead.
    -q, --quiet / -v, --verbose   Less / more diagnostics on stderr.

Pre-alpha: no GUI, windows/panes splits, persistence or recovery yet (docs/PRODUCT_REQUIREMENTS.md).
No command opens a window, installs anything or uses the network; no Windows service exists.
";

/// The capabilities document (`kim.capabilities` v1). Its `version` must equal the package
/// version; a test checks it and keeps docs/contracts/examples/tool-capabilities-v1/subak.json
/// identical.
const CAPABILITIES: &str = include_str!("capabilities.json");

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json_flag = raw
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--json");
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
                 Commands: capabilities; terminal new|list|info|rename|attach|stop|kill|rm \
                 (named Terminals in the Host); host start|status|health|attach|stop|serve.\n\
                 No GUI, splits, persistence or recovery yet.\n",
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
        Cmd::HostAttach => client::attach(a, &endpoint, None),
        Cmd::TerminalNew => terminal::new(a, &endpoint),
        Cmd::TerminalList => terminal::list(&endpoint),
        Cmd::TerminalInfo => terminal::info(a, &endpoint),
        Cmd::TerminalRename => terminal::rename(a, &endpoint),
        Cmd::TerminalAttach => terminal::attach(a, &endpoint),
        Cmd::TerminalStop | Cmd::TerminalKill | Cmd::TerminalRm => terminal::end(a, &endpoint, cmd),
        _ => Err(CliError::new(Code::Internal, "not a host command")),
    }
}

#[cfg(not(windows))]
fn run_host(_cmd: &Cmd, _a: &Args) -> Result<Value, CliError> {
    // ADR 0011: no Linux implementation before M4; the interface compiles to this stub.
    Err(CliError::new(
        Code::UnsupportedPlatform,
        "the Subak Host needs Windows (ConPTY); Linux support is planned for M4",
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

fn text(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), String::from)
}

/// One `terminal list` row.
fn row(t: &Value) -> String {
    format!(
        "{:<14}  {:<20}  {:<8}  {:>7}  {:>7}  {}{}",
        text(&t["id"]),
        text(&t["name"]),
        text(&t["state"]),
        if t["pid"].is_null() {
            "-".into()
        } else {
            text(&t["pid"])
        },
        text(&t["clients"]),
        text(&t["program"]),
        if t["busy"] == true { " (busy)" } else { "" },
    )
}

/// How a pane ended, for stop/kill/rm.
fn ended(report: &Value) -> String {
    format!(
        "pid {} ended (exit code {}{})",
        report["pid"],
        report["exit_code"],
        if report["forced"] == true {
            ", forced"
        } else {
            ""
        }
    )
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
        Cmd::HostStart if d["pane"].is_null() => writeln!(
            out,
            "Host started: pid {} (job breakaway: {}), no Terminal\nendpoint: {}",
            d["host"]["pid"],
            d["host"]["job_breakaway"],
            d["endpoint"].as_str().unwrap_or("?"),
        ),
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
                "no first Terminal (started with --no-shell, or it ended)".to_string()
            } else {
                format!(
                    "first Terminal: shell pid {} running: {} ({}), {}x{}, {} process(es) in its job",
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
                 idle exit after {} s)\nTerminals: {} live, {} in total (`subak terminal list`)\n\
                 {pane}\nclients: {} connected, {} attached\nendpoint: {}",
                d["host"]["pid"],
                d["host"]["started_at"].as_str().unwrap_or("?"),
                d["host"]["session_id"],
                d["protocol"],
                d["host"]["job_breakaway"],
                d["host"]["in_job"],
                d["host"]["idle_exit_s"],
                d["terminals"],
                d["terminals_total"],
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
        Cmd::HostStop => {
            let ended: Vec<String> = d["terminals"]
                .as_array()
                .map(|v| v.iter().map(ended).collect())
                .unwrap_or_default();
            if ended.is_empty() {
                writeln!(
                    out,
                    "Host pid {} stopped (exited: {}); it owned no live Terminal",
                    d["host"]["pid"], d["host"]["exited"],
                )
            } else {
                writeln!(
                    out,
                    "Host pid {} stopped (exited: {}); {} Terminal(s) ended: {}",
                    d["host"]["pid"],
                    d["host"]["exited"],
                    ended.len(),
                    ended.join("; "),
                )
            }
        }
        Cmd::HostServe => writeln!(
            out,
            "Host ended ({}); {} Terminal(s) ended",
            d["reason"].as_str().unwrap_or("?"),
            d["terminals"].as_array().map_or(0, Vec::len),
        ),
        Cmd::HostAttach | Cmd::TerminalAttach => {
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
        Cmd::TerminalNew => {
            let t = &d["terminal"];
            let pane = &t["windows"][0]["panes"][0];
            writeln!(
                out,
                "Terminal `{}` ({}) created: pid {} ({}){}\nattach with: subak terminal attach {}",
                text(&d["name"]),
                text(&d["terminal_id"]),
                pane["pid"],
                text(&pane["command"]["program"]),
                if d["host"]["started"] == true {
                    format!("; started Host pid {}", d["host"]["pid"])
                } else {
                    String::new()
                },
                text(&d["terminal_id"]),
            )
        }
        Cmd::TerminalList => {
            let list = d["terminals"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                writeln!(
                    out,
                    "no Terminals{}",
                    if d["host"].is_null() {
                        " (no Host is running)"
                    } else {
                        ""
                    }
                )
            } else {
                let _ = writeln!(
                    out,
                    "{:<14}  {:<20}  {:<8}  {:>7}  {:>7}  PROGRAM",
                    "ID", "NAME", "STATE", "PID", "CLIENTS"
                );
                for t in &list {
                    let _ = writeln!(out, "{}", row(t));
                }
                Ok(())
            }
        }
        Cmd::TerminalInfo => {
            let t = &d["terminal"];
            let w = &t["windows"][0];
            let p = &w["panes"][0];
            writeln!(
                out,
                "Terminal `{}` ({}) — {}{} since {}\nwindow {} `{}` ({}), pane {} ({}): {} {}\n\
                 pid {}, {} process(es), {}x{}, {} client(s), cwd {}{}",
                text(&t["name"]),
                text(&t["id"]),
                text(&t["state"]),
                if t["busy"] == true { ", busy" } else { "" },
                text(&t["created_at"]),
                w["index"],
                text(&w["name"]),
                text(&w["id"]),
                p["index"],
                text(&p["id"]),
                text(&p["command"]["program"]),
                p["command"]["args"]
                    .as_array()
                    .map(|v| v.iter().map(text).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default(),
                text(&p["pid"]),
                p["processes"],
                p["size"]["cols"],
                p["size"]["rows"],
                p["clients"],
                text(&p["cwd"]),
                if t["last_exit"].is_null() {
                    String::new()
                } else {
                    format!(
                        "\nlast exit: code {} ({}{})",
                        text(&t["last_exit"]["exit_code"]),
                        text(&t["last_exit"]["cause"]),
                        if t["last_exit"]["forced"] == true {
                            ", forced"
                        } else {
                            ""
                        }
                    )
                },
            )
        }
        Cmd::TerminalRename if d["changed"] == false => writeln!(
            out,
            "Terminal {} is already named `{}`",
            text(&d["terminal_id"]),
            text(&d["name"]),
        ),
        Cmd::TerminalRename => writeln!(
            out,
            "Terminal {} renamed: `{}` -> `{}`",
            text(&d["terminal_id"]),
            text(&d["previous_name"]),
            text(&d["name"]),
        ),
        Cmd::TerminalStop | Cmd::TerminalKill | Cmd::TerminalRm => {
            let name = text(&d["name"]);
            let ended: Vec<String> = d["affected"]
                .as_array()
                .map(|v| v.iter().map(ended).collect())
                .unwrap_or_default();
            let what = if ended.is_empty() {
                "nothing was running".to_string()
            } else {
                ended.join("; ")
            };
            if d["removed"] == true {
                writeln!(out, "Terminal `{name}` removed; {what}")
            } else {
                writeln!(
                    out,
                    "Terminal `{name}` is {}; {what}",
                    text(&d["terminal"]["state"])
                )
            }
        }
        _ => Ok(()),
    };
}
