//! The `subak` package: the Subak terminal CLI and Host (binary `subak`) and, since Step 16, the
//! Subak window (binary `subak-gui`, [`gui`]). Both binaries are thin `main`s over this library;
//! it is not a public API for other packages.
//!
//! Pre-alpha. Besides `capabilities` (docs/CLI_CONTRACT.md §9) the CLI contains the **Host
//! supervisor and IPC v1** (Step 09, ADR 0019, docs/contracts/IPC_PROTOCOL.md), the **Terminal
//! session lifecycle** (Step 10, ADR 0020, docs/SESSION_MODEL.md) and the **Subak CLI** (Step 12,
//! ADR 0027, docs/SUBAK_CLI.md): a detached per-user Host owns any number of named Terminals, each
//! with one window and one pane in its own pseudo console and job, and the tmux-familiar verbs
//! `subak new|ls|info|status|attach|detach|rename|stop|kill|rm` manage them headlessly. The Host
//! serves an identity-checked named pipe, starts on demand under a start lock, and exits after an
//! idle grace period. Since Step 13 (ADR 0030, docs/SCROLLBACK.md) the Host models each pane's
//! terminal with a bounded scrollback, so an attach reproduces the screen, and
//! `subak capture|history|clear-history` read and clear it. Since Step 16 (ADR 0039,
//! docs/SUBAK_GUI.md) `subak-gui` shows one Terminal in a native window as an IPC client; the
//! Host and the CLI never need it; since Step 17 (ADR 0042) it shows the Host's Terminals as
//! tabs. There are no splits, persistence or recovery yet.
//! Nothing starts by itself, and no `subak` command opens a window.

// Portable modules: compiled and unit-tested on every target, but until the Linux Host (M4,
// ADR 0011) only the Windows Host and clients use most of them.
#[cfg_attr(not(windows), allow(dead_code))]
mod cli;
/// `config.toml`: the window's settings and the basic shell profiles (Step 20, ADR 0051).
/// Portable; read by the window and the `subak config`/`profile` commands, never by the Host.
mod config;
/// `subak config …` and `subak profile …` (Step 20): portable, never contacts a Host.
mod config_cmd;
/// Subak's keyboard shortcuts (hotkey contract; `subak hotkeys`, the window's dispatch).
pub mod hotkeys;
/// `subak hotkeys …` (Step 17): portable, never contacts a Host.
mod hotkeys_cmd;
#[cfg_attr(not(windows), allow(dead_code))]
mod output;
#[cfg_attr(not(windows), allow(dead_code))]
mod proto;
#[cfg_attr(not(windows), allow(dead_code))]
mod pty;
#[cfg_attr(not(windows), allow(dead_code))]
mod redact;
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

/// The Subak window (`subak-gui`, Step 16): an IPC client that renders one Terminal's pane.
pub mod gui;

/// The pane terminal model lives in `subak-term` (ADR 0030, ADR 0036), shared with client views.
use subak_term::screen;

use cli::{Args, Cmd};
use output::{CliError, Code};
use serde_json::Value;
use std::io::Write;
use std::process::ExitCode;

const HELP: &str = "\
subak — Subak persistent terminal (pre-alpha; Host, IPC v1, the Terminal lifecycle and its CLI)

USAGE:
    subak [--help | --version]
    subak new     [<name>] [--cwd <dir>] [--env NAME[=VALUE]]... [--unset-env NAME]...
                  [--size <cols>x<rows>] [--remain-on-exit] [--keep-on-failure] [--no-start]
                  [--history-limit <lines>] [--attach [--read-only]] [--dry-run] [--json]
                  [-- <program> [args...] | --command-line <line>]
    subak ls      [--json]                                   (also: subak list)
    subak status  [--json]
    subak info    <target> [--json]
    subak attach  [<target>] [--size <cols>x<rows>] [--read-only] [--dry-run [--json]]
    subak detach  [<target>] [--client <id>] [--dry-run] [--json]
    subak rename  <target> <new-name> [--dry-run] [--json]
    subak stop    <target> [--yes] [--dry-run] [--json]
    subak kill    <target> [--yes] [--dry-run] [--json]
    subak rm      <target> [--force] [--yes] [--dry-run] [--json]
    subak capture [<target>] [--start <line>|-] [--end <line>|-] [--history] [--vt] [--join]
                  [--no-redact] [--json]                     (also: subak capture-pane)
    subak history [<target>] [--json]
    subak clear-history [<target>] [--yes] [--dry-run] [--json]
    subak host start  [--shell <command> | --no-shell] [--idle-exit <secs>] [--dry-run] [--json]
    subak host status [--json]
    subak host health [--json]
    subak host attach [--size <cols>x<rows>] [--read-only] [--dry-run [--json]]
    subak host stop   [--kill-terminals] [--yes] [--dry-run] [--json]
    subak host serve  [--shell <command> | --no-shell] [--idle-exit <secs>] [--dry-run] [--json]
    subak hotkeys list [--scope <scope>] [--profile <name>] [--json]
    subak hotkeys show <action> [--profile <name>] [--json]
    subak hotkeys set <action> <keys>... [--scope global|app] [--trigger press|release|hold]
                  [--hold-ms <ms>] [--repeat] [--profile <name>] [--replace] [--allow-conflict]
                  [--dry-run] [--json]
    subak hotkeys disable (<action> [--profile <name>] | --all | --scope <family>) [--dry-run]
    subak hotkeys enable (--all | --scope <family>) [--dry-run] [--json]
    subak hotkeys reset (<action> [--profile <name>] | --all [--yes]) [--dry-run] [--json]
    subak hotkeys check [--all-tools] [--min-severity error|warning|info] [--profile <name>]
    subak hotkeys export [--json]  |  subak hotkeys import <file> [--dry-run] [--json]
    subak config show|check|export [--json]
    subak config set <key> <value> [--dry-run] [--json]      (also <key>=<value>)
    subak config reset (<key> | <section> | --all [--yes]) [--dry-run] [--json]
    subak config import <file> [--dry-run] [--json]
    subak profile list [--json]  |  subak profile show <name> [--json]
    subak capabilities [--json]
    Every command except `hotkeys`, `config` and `profile` also takes --endpoint <name>. `subak terminal new|list|info|rename|attach|
    stop|kill|rm` is the deprecated Step 10 spelling of the same commands (a warning says so).

TERMINALS (Windows only; docs/SUBAK_CLI.md, docs/SESSION_MODEL.md):
    A Terminal is a named workspace in the Host with a stable id (t-...), one window and one pane
    (Steps 21-22 add more). Each pane runs in its own pseudo console and kill-on-close job, so
    Terminals never share processes. <target> is a Terminal name (exact; case is ignored) or id.
    new      Create a Terminal running <program> (default: %ComSpec%) in your profile folder or
             --cwd, detached (--attach attaches at once). Without a name it gets the lowest free
             number. Starts a Host if none runs (--no-start fails with `host_not_running`
             instead). A name already in use (ignoring case) is refused with `name_conflict`,
             never suffixed. A program that cannot start leaves nothing behind, unless
             --keep-on-failure keeps it as `failed`.
             <program> and every argument are passed as separate words (quoted for Windows),
             never through a shell. A bare name is looked up on the pane's PATH (cmd,
             powershell, pwsh if you installed it; Subak installs nothing). Batch (.bat/.cmd)
             and PowerShell (.ps1) scripts are refused: name their interpreter
             (-- cmd.exe /d /c script.cmd). --command-line <line> passes one Windows command
             line verbatim instead (only its first word is looked up), for programs that parse
             their own command line, such as cmd.exe /c with inner quotes. The pane gets your
             logon environment (not this terminal's), SUBAK_PANE_ID and SUBAK_TERMINAL_ID, and
             the --env/--unset-env overrides; their values are never shown.
    ls       Every Terminal with its state, pid and clients. Starts nothing.
    status   The Host (or that none runs), every Terminal and every client. Starts nothing;
             exits 0 also without a Host.
    info     One Terminal with its window and pane; exit 3 (`target_not_found`) if none.
    attach   Connect this terminal to a Terminal's pane (without <target>: the only running
             one). Detaching or closing this window never stops anything (see ATTACH).
    detach   Detach every client of <target>, or the client --client <id> (ids: `subak status`),
             from another terminal or script. Inside a pane, no target means its own Terminal.
             Processes and Terminals are never touched; detached clients exit with a notice.
    rename   Rename a Terminal; its id never changes.
    stop     End its processes: close the pseudo console (programs get CTRL_CLOSE_EVENT), wait
             5 s, then end whatever is left in its job. The Terminal is kept as `stopped`.
             Destructive: asks first, or needs --yes.
    kill     End its processes at once and remove the Terminal. Asks, or needs --yes.
    rm       Remove a Terminal that runs nothing (stopped or failed). --force also ends a
             running one (as kill). Asks, or needs --yes.
    There is no ambiguous `close`: detach looks away, stop ends processes and keeps the record,
    kill and rm remove it. A Terminal whose program exits by itself is removed, unless it was
    created with --remain-on-exit (then `stopped`). Until Step 29 nothing is saved: Terminals
    end with their Host, and stopped ones vanish when the Host exits.

SCROLLBACK (docs/SCROLLBACK.md):
    The Host keeps each pane's screen and the rows that scrolled off its top (its history), in
    its own memory only: up to --history-limit rows (default 2000, 0 keeps none) and 4 MiB per
    pane, the oldest dropped first. Nothing is written to disk or logged. It is gone when the
    Terminal is removed or the Host exits; a stopped Terminal keeps it for `capture`.
    An attach repaints the screen (also a full-screen program on the alternate screen) and puts
    the history into this terminal's scrollback, then streams live output.
    capture  The screen as text, or a range: lines count from 0 on the screen and from -1 (the
             newest history line) down; `-` is the oldest history line (--start) or the last
             screen row (--end); --history = --start -. --vt keeps colours, --join joins
             wrapped rows. Secret-looking values (tokens, passwords, keys) are shown as ***
             unless --no-redact. Inside a pane, no target means that pane.
    history  How many lines and bytes are kept, the limits, evictions, and the screen; never
             the content.
    clear-history  Delete the kept history (the screen stays); asks, or needs --yes. Copies a
             terminal already shows in its own scrollback are not touched.

ATTACH (docs/SUBAK_CLI.md §5):
    Ctrl+B d        Detach (Ctrl+B Ctrl+B sends one Ctrl+B to the program). Fixed in the CLI
                    for now; it follows the Ctrl+B prefix of the hotkey contract.
    Ctrl+C          Goes to the program as a key (it interrupts what runs there); it never ends
                    this client.
    Ctrl+Break      Detaches this client cleanly (it is not sent to the program).
    Closing this window, or killing this client, is a detach too: the Terminal keeps running.
    The console's modes and code page are put back on detach, at the program's end, on
    Ctrl+Break, when the window closes and after a crash; modes the program left on in this
    terminal (alternate screen, hidden cursor, mouse, bracketed paste) are switched off.
    --read-only     Watch only: keys are not sent and the pane is never resized.
    With redirected input the bytes are sent as they are and the end of input detaches; a
    read-only attach reads no redirected input. With redirected output, nothing is restored or
    switched off, because nothing was changed.

HOTKEYS (docs/SUBAK_CLI.md §3.12, docs/HOTKEY_CONTRACT.md):
    The window's keyboard shortcuts (Ctrl+T new Terminal, Ctrl+W close Terminal, Ctrl+Tab /
    Ctrl+Shift+Tab next/previous tab, Ctrl+Shift+R rename, copy, paste, zoom, scroll) follow
    the Kim hotkey contract. Your changes live in %APPDATA%\\mouv\\subak\\hotkeys.json; an
    action you never changed keeps the current default. A broken or newer file is never
    rewritten implicitly: the window uses the defaults (safe mode) and changes are refused
    (`hotkeys_invalid`) until it is fixed or `reset --all --yes` replaces it (keeping a .bak).
    A conflicting change is refused (`hotkey_conflict`) and nothing is written; --replace takes
    the keys away from the other Subak action. Disable or rebind Ctrl+T/Ctrl+W to let programs
    receive them (the Ctrl+B prefix pass-through comes with Step 23). `check --all-tools` reads
    the other installed Kim tools' files only. These commands never contact a Host, open a
    window or register a hotkey; an open window applies a change when it regains focus.
    KIM_NO_HOTKEYS=1 (or subak-gui --no-hotkeys) starts the window with every shortcut off.

SETTINGS AND PROFILES (docs/SUBAK_CLI.md §3.13, docs/SUBAK_GUI.md §13):
    %APPDATA%\\mouv\\subak\\config.toml holds the window's appearance (theme, font, size, line
    height, padding, opacity, motion), mouse and keyboard choices (copy on select, smart
    Ctrl+C, right/middle click, word separators) and basic shell profiles ([[profiles]]: name,
    program, args, cwd, env, unset_env). Hand-edit it or use these commands; the window's
    settings menu writes the same keys. An invalid value falls back to its default alone and is
    named with its line (`config check`); a broken or newer file is never rewritten by an edit.
    `reset --all` and `import` keep a backup in backups\\ (the newest 5). Secrets do not belong
    in profiles: import refuses them (`secret_in_config`) and every value that looks secret is
    shown as ***. Nothing here contacts a Host or changes a running Terminal: the window applies
    changes when it gets the focus, and a profile is used only when a Terminal is created
    (`new --profile <name>`, Ctrl+T, the window's New Terminal menu).

HOST (Windows only; docs/contracts/IPC_PROTOCOL.md, docs/CONPTY_PROOF.md):
    host start    Start a detached per-user Host, unless one already serves the endpoint
                  (`host_already_running`). Starters take a per-endpoint lock, so several at
                  once give one Host. It gets your logon environment, not this terminal's.
                  With --shell (default %ComSpec%) it starts with one Terminal running that
                  command line; --no-shell starts it with none. The Host runs while it owns a
                  live Terminal or has a client, then exits after the idle grace period.
                  Closing this window or any client never ends it. `subak new` starts one on
                  demand, so this is rarely needed.
    host status   Host process id, protocol, job breakaway, environment, Terminals, clients.
    host health   Heartbeat: check that the endpoint is served by subak.exe of this user, do
                  the handshake and report the answer time. Exit 0 only when healthy.
    host attach   Same as `subak attach` without a target.
    host stop     Stop the Host. With live Terminals it refuses (`terminals_running`) unless
                  --kill-terminals is given, and then asks first, or needs --yes.
    host serve    Run the Host in this process (what `host start` launches detached).

    Every client checks, before sending anything, that the pipe is served by subak.exe of
    this user (`host_identity_mismatch` otherwise); the Host checks each client's token. A
    Host that does not answer is reported (`host_unresponsive`), never ended or replaced.
    The Host listens only on that local, same-user named pipe: no network listener.

OPTIONS:
    --endpoint <name>  Pipe name under \\\\.\\pipe\\ (default: mouv-subak-<user SID>-s<session>).
                       Use your own name for experiments; it never touches the default Host.
    --shell <command>  Command line of the Host's first Terminal (host start/serve).
    --no-shell         Start a Host that owns no Terminal (host start/serve).
    --idle-exit <s>    Seconds the Host waits with no live Terminal and no client before it
                       exits (0..86400, default 10).
    --size <c>x<r>     Pseudo console size (new, attach; 2..1000 each).
    --cwd <dir>        Working directory of a new Terminal (an absolute path).
    --env NAME=VALUE   Set a variable in a new Terminal (repeatable). --env NAME copies this
                       terminal's value, so a token never appears on a command line.
    --unset-env NAME   Remove a variable from a new Terminal's environment (repeatable).
    --command-line <l> A verbatim command line for a new Terminal (instead of -- <program>).
    --remain-on-exit   Keep a new Terminal as `stopped` when its program exits by itself.
    --keep-on-failure  Keep a Terminal whose program could not start, as `failed`.
    --history-limit <n> new: history rows the Host keeps for the pane (0..100000, default 2000).
    --profile <name>   new: start a profile of config.toml (its program, args, cwd and env;
                       -- <program>, --command-line, --cwd and --env given here win). Without
                       a program, new uses terminal.default_profile unless --no-profile.
    --start/--end <l>  capture: first/last line (a number, negative for history, or -).
    --history          capture: from the oldest history line.
    --vt / --join      capture: keep escape sequences / join wrapped rows.
    --no-redact        capture: show secret-looking values instead of ***.
    --no-start         new: never start a Host; fail with `host_not_running` instead.
    --attach           new: attach to the new Terminal at once.
    --read-only        attach: watch without sending keys.
    --client <id>      detach: one client, by the id `subak status` lists.
    --force            rm: also end a running Terminal.
    --kill-terminals   host stop: end every live Terminal with the Host.
    --dry-run          Show what would happen; change nothing.
    -y, --yes          Confirm a destructive command without asking.
    --json             One JSON document on stdout (docs/CLI_CONTRACT.md §7).
    --no-input         Never ask; fail with `confirmation_required` instead (also KIM_NO_INPUT).
    -q, --quiet / -v, --verbose   Less / more diagnostics on stderr.

The window is a separate program, `subak-gui` (docs/SUBAK_GUI.md); no command here starts it.
Pre-alpha: no windows/panes splits, persistence or recovery yet (docs/PRODUCT_REQUIREMENTS.md);
history is never persisted.
No command opens a window, installs anything or uses the network; no Windows service exists.
";

/// The capabilities document (`kim.capabilities` v1). Its `version` must equal the package
/// version; a test checks it and keeps docs/contracts/examples/tool-capabilities-v1/subak.json
/// identical.
const CAPABILITIES: &str = include_str!("capabilities.json");

/// The `subak` command line (the `subak` binary's `main`).
pub fn cli_main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json_flag = raw
        .iter()
        .take_while(|a| a.as_str() != "--")
        .any(|a| a == "--json");
    let (cmd, args) = match cli::parse(&raw) {
        Ok(p) => p,
        Err((name, e)) => return fail(&name, &e, json_flag),
    };
    // ADR 0027: the Step 10 spelling still works, and says that it is deprecated.
    let deprecation = args.alias.as_ref().map(|alias| {
        format!(
            "`subak {alias}` is deprecated; use `subak {}` (same options and output, ADR 0027)",
            cmd.words()
        )
    });
    if let Some(d) = &deprecation
        && !args.json
        && !args.quiet
    {
        eprintln!("subak: warning: {d}");
    }
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
                 Commands: new, ls, info, status, attach, detach, rename, stop, kill, rm \
                 (named Terminals in the Host); capture, history, clear-history (scrollback); \
                 host start|status|health|attach|stop|serve; hotkeys list|show|set|disable|\
                 enable|reset|check|export|import (the window's shortcuts); config show|check|\
                 set|reset|export|import and profile list|show (settings and shell profiles);\
                 capabilities.\n\
                 The window is the separate program subak-gui (Terminals as tabs); no splits,\n\
                 persistence or recovery yet.\n",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        _ if cmd.is_hotkeys() || cmd.is_settings() => {
            drop(out);
            let ran = if cmd.is_hotkeys() {
                hotkeys_cmd::run(cmd, &args)
            } else {
                config_cmd::run(cmd, &args).map(|o| hotkeys_cmd::Outcome {
                    data: o.data,
                    human: o.human,
                    warnings: o.warnings,
                })
            };
            match ran {
                Ok(o) => {
                    let mut warnings = o.warnings;
                    warnings.extend(deprecation);
                    let mut out = std::io::stdout().lock();
                    if args.json {
                        let _ =
                            writeln!(out, "{}", output::success(cmd.name(), &o.data, &warnings));
                    } else {
                        if !args.quiet {
                            for w in &warnings {
                                eprintln!("subak: warning: {w}");
                            }
                        }
                        let _ = write!(out, "{}", o.human);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(cmd.name(), &e, args.json),
            }
        }
        _ => {
            drop(out);
            match run_host(cmd, &args) {
                Ok(data) => {
                    let mut warnings = host_warnings(cmd, &data);
                    warnings.extend(deprecation);
                    if args.json {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{}", output::success(cmd.name(), &data, &warnings));
                    } else {
                        if !args.quiet {
                            for w in host_warnings(cmd, &data) {
                                eprintln!("subak: warning: {w}");
                            }
                        }
                        human(cmd, &data, &args);
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
fn run_host(cmd: Cmd, a: &Args) -> Result<Value, CliError> {
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
        Cmd::New => terminal::new(a, &endpoint),
        Cmd::Ls => terminal::list(&endpoint),
        Cmd::Status => terminal::status(&endpoint),
        Cmd::Info => terminal::info(a, &endpoint),
        Cmd::Rename => terminal::rename(a, &endpoint),
        Cmd::Attach => terminal::attach(a, &endpoint),
        Cmd::Detach => terminal::detach(a, &endpoint),
        Cmd::Stop | Cmd::Kill | Cmd::Rm => terminal::end(a, &endpoint, cmd),
        Cmd::Capture => terminal::capture(a, &endpoint),
        Cmd::History => terminal::history(a, &endpoint),
        Cmd::ClearHistory => terminal::clear_history(a, &endpoint),
        // Help, version, capabilities and the portable `hotkeys` commands run elsewhere.
        _ => Err(CliError::new(Code::Internal, "not a host command")),
    }
}

#[cfg(not(windows))]
fn run_host(_cmd: Cmd, _a: &Args) -> Result<Value, CliError> {
    // ADR 0011: no Linux implementation before M4; the interface compiles to this stub.
    Err(CliError::new(
        Code::UnsupportedPlatform,
        "the Subak Host needs Windows (ConPTY); Linux support is planned for M4",
    ))
}

/// Warnings a command found while it ran (Step 20: a default profile that cannot be used),
/// reported with its result.
static LATER: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Adds a warning to the running command's result.
#[cfg_attr(not(windows), allow(dead_code))]
fn warn_later(w: String) {
    if let Ok(mut l) = LATER.lock() {
        l.push(w);
    }
}

/// Risk R-03 (measured in Step 08, ADR 0016): a Host inside a job it could not leave dies when
/// that job is closed with kill-on-close.
fn host_warnings(cmd: Cmd, d: &Value) -> Vec<String> {
    let mut w: Vec<String> = LATER.lock().map(|l| l.clone()).unwrap_or_default();
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

/// One `ls` row.
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

/// The `ls` table (header and rows), or `None` for no Terminals.
fn table(list: &[Value]) -> Option<String> {
    if list.is_empty() {
        return None;
    }
    let mut s = format!(
        "{:<14}  {:<20}  {:<8}  {:>7}  {:>7}  PROGRAM\n",
        "ID", "NAME", "STATE", "PID", "CLIENTS"
    );
    for t in list {
        s.push_str(&row(t));
        s.push('\n');
    }
    Some(s)
}

/// An exit status as shown to people: NTSTATUS-like values (≥ 0x80000000, e.g. `0xC000013A`
/// for a console closed with Ctrl+C or Close) also in hex.
fn exit_text(v: &Value) -> String {
    match v.as_u64() {
        Some(c) if c >= 0x8000_0000 => format!("{c} (0x{c:08X})"),
        Some(c) => c.to_string(),
        None => "unknown".into(),
    }
}

/// The names of a pane's environment overrides (`""` when there are none).
fn env_text(env: &Value) -> String {
    let names = |k: &str| {
        env[k]
            .as_array()
            .map(|v| v.iter().map(text).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    let (set, unset) = (names("set"), names("unset"));
    match (set.is_empty(), unset.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("\nenvironment overrides (values hidden): set {set}"),
        (true, false) => format!("\nenvironment overrides: unset {unset}"),
        (false, false) => {
            format!("\nenvironment overrides (values hidden): set {set}; unset {unset}")
        }
    }
}

/// How a pane ended, for stop/kill/rm.
fn ended(report: &Value) -> String {
    format!(
        "pid {} ended (exit code {}{})",
        report["pid"],
        exit_text(&report["exit_code"]),
        if report["forced"] == true {
            ", forced"
        } else {
            ""
        }
    )
}

/// One client of `host.status`, for `status`.
fn client_row(c: &Value) -> String {
    format!(
        "  client {} pid {} ({} {} {}){}",
        c["id"],
        text(&c["pid"]),
        text(&c["kind"]),
        text(&c["name"]),
        text(&c["version"]),
        if c["attached"] == true {
            format!(
                ", attached{} to stream(s) {}",
                if c["interactive"] == true {
                    ""
                } else {
                    " read-only"
                },
                c["streams"]
            )
        } else {
            String::new()
        }
    )
}

/// Bytes as people read them.
fn bytes_text(v: &Value) -> String {
    let n = v.as_u64().unwrap_or(0);
    if n >= 1 << 20 {
        format!("{:.1} MiB", n as f64 / f64::from(1u32 << 20))
    } else if n >= 1 << 10 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} bytes")
    }
}

/// The notice an attach prints when it ends (stderr).
fn attach_notice(d: &Value) -> String {
    let name = text(&d["name"]);
    if d["detached"] == true {
        if d["detached_by"].is_object() {
            format!(
                "[subak: detached from `{name}` by another client (pid {}, {}); it keeps running in Host pid {}]",
                text(&d["detached_by"]["pid"]),
                text(&d["detached_by"]["kind"]),
                d["host"]["pid"]
            )
        } else {
            format!(
                "[subak: detached; the shell keeps running in Host pid {} (Terminal `{name}`)]",
                d["host"]["pid"]
            )
        }
    } else if let Some(reason) = d["host"]["stopped"].as_str() {
        format!(
            "[subak: the program in `{name}` ended (exit code {}); the Host stopped ({reason})]",
            exit_text(&d["shell_exit_code"]),
        )
    } else {
        format!(
            "[subak: the program in `{name}` ended (exit code {}); the Host keeps running until \
             its idle grace period ends]",
            exit_text(&d["shell_exit_code"]),
        )
    }
}

/// Human output on stdout (results) and stderr (notices).
fn human(cmd: Cmd, d: &Value, a: &Args) {
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
            let clients: String = d["clients"]["list"]
                .as_array()
                .map(|l| l.iter().map(|c| format!("\n{}", client_row(c))).collect())
                .unwrap_or_default();
            let scrollback = if d["scrollback"].is_object() {
                format!(
                    "\nscrollback: {} pane(s), {} line(s) of history ({}) in the Host's memory, never on disk",
                    d["scrollback"]["panes"],
                    d["scrollback"]["lines"],
                    bytes_text(&d["scrollback"]["bytes"]),
                )
            } else {
                String::new()
            };
            writeln!(
                out,
                "Host pid {} since {} (session {}, protocol {}, job breakaway: {}, in a job: {}, \
                 idle exit after {} s)\nTerminals: {} live, {} in total (`subak ls`)\n\
                 {pane}\nclients: {} connected, {} attached{clients}{scrollback}\nendpoint: {}",
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
        Cmd::HostAttach | Cmd::Attach => {
            if !a.quiet {
                eprintln!("{}", attach_notice(d));
            }
            Ok(())
        }
        Cmd::New if d["attach"].is_object() => {
            if !a.quiet {
                eprintln!("{}", attach_notice(&d["attach"]));
            }
            Ok(())
        }
        Cmd::New => {
            let t = &d["terminal"];
            let pane = &t["windows"][0]["panes"][0];
            writeln!(
                out,
                "Terminal `{}` ({}) created: pid {} ({}){}{}\nattach with: subak attach {}",
                text(&d["name"]),
                text(&d["terminal_id"]),
                pane["pid"],
                if pane["executable"].is_string() {
                    text(&pane["executable"])
                } else {
                    text(&pane["command"]["program"])
                },
                if d["host"]["started"] == true {
                    format!("; started Host pid {}", d["host"]["pid"])
                } else {
                    String::new()
                },
                env_text(&pane["env"]),
                text(&d["terminal_id"]),
            )
        }
        Cmd::Ls => match table(d["terminals"].as_array().map_or(&[][..], Vec::as_slice)) {
            None => writeln!(
                out,
                "no Terminals{}",
                if d["host"].is_null() {
                    " (no Host is running)"
                } else {
                    ""
                }
            ),
            Some(t) => write!(out, "{t}"),
        },
        Cmd::Status if d["running"] != true => writeln!(
            out,
            "no Host is running on {}; no Terminals (`subak new` starts one)",
            text(&d["endpoint"])
        ),
        Cmd::Status => {
            let list = d["terminals"].as_array().map_or(&[][..], Vec::as_slice);
            let live = list
                .iter()
                .filter(|t| {
                    matches!(
                        t["state"].as_str(),
                        Some("starting" | "running" | "stopping")
                    )
                })
                .count();
            let attached: Vec<String> = d["clients"]["list"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .filter(|c| c["attached"] == true)
                        .map(client_row)
                        .collect()
                })
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "Host pid {} (version {}, up {} s, idle exit after {} s) on {}\n\
                 Terminals: {} ({live} live); clients: {} connected (this command included), {} attached",
                d["host"]["pid"],
                text(&d["host"]["version"]),
                d["host"]["uptime_s"],
                d["host"]["idle_exit_s"],
                text(&d["endpoint"]),
                list.len(),
                d["clients"]["connected"],
                d["clients"]["attached"],
            );
            if d["scrollback"].is_object() {
                let _ = writeln!(
                    out,
                    "scrollback: {} line(s) of history ({}) in the Host's memory, never on disk",
                    d["scrollback"]["lines"],
                    bytes_text(&d["scrollback"]["bytes"]),
                );
            }
            if let Some(t) = table(list) {
                let _ = write!(out, "{t}");
            }
            for c in &attached {
                let _ = writeln!(out, "{c}");
            }
            Ok(())
        }
        Cmd::Info => {
            let t = &d["terminal"];
            let w = &t["windows"][0];
            let p = &w["panes"][0];
            writeln!(
                out,
                "Terminal `{}` ({}) — {}{} since {}\nwindow {} `{}` ({}), pane {} ({}): {}\n\
                 executable {}\npid {}, {} process(es), {}x{}, {} client(s), cwd {}{}{}",
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
                if p["command_line"].is_string() {
                    format!("{} (verbatim)", text(&p["command_line"]))
                } else {
                    std::iter::once(text(&p["command"]["program"]))
                        .chain(
                            p["command"]["args"]
                                .as_array()
                                .map(|v| v.iter().map(text).collect::<Vec<_>>())
                                .unwrap_or_default(),
                        )
                        .collect::<Vec<_>>()
                        .join(" ")
                },
                text(&p["executable"]),
                text(&p["pid"]),
                p["processes"],
                p["size"]["cols"],
                p["size"]["rows"],
                p["clients"],
                text(&p["cwd"]),
                env_text(&p["env"]),
                if t["last_exit"].is_null() {
                    String::new()
                } else {
                    format!(
                        "\nlast exit: code {} ({}{})",
                        exit_text(&t["last_exit"]["exit_code"]),
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
        Cmd::Detach => {
            let list = d["detached"].as_array().map_or(&[][..], Vec::as_slice);
            let whom = if d["terminal"].is_object() {
                format!(
                    "`{}` ({})",
                    text(&d["terminal"]["name"]),
                    text(&d["terminal"]["id"])
                )
            } else {
                format!("client {}", a.client.unwrap_or_default())
            };
            if list.is_empty() {
                writeln!(out, "no client was attached to {whom}; nothing changed")
            } else {
                let rows: Vec<String> = list
                    .iter()
                    .map(|e| {
                        format!(
                            "client {} pid {} ({} {}) from {}",
                            e["client"]["id"],
                            text(&e["client"]["pid"]),
                            text(&e["client"]["kind"]),
                            text(&e["client"]["name"]),
                            text(&e["terminal_id"]),
                        )
                    })
                    .collect();
                writeln!(
                    out,
                    "detached {} attachment(s) of {whom}: {}; nothing was stopped",
                    list.len(),
                    rows.join("; ")
                )
            }
        }
        Cmd::Rename if d["changed"] == false => writeln!(
            out,
            "Terminal {} is already named `{}`",
            text(&d["terminal_id"]),
            text(&d["name"]),
        ),
        Cmd::Rename => writeln!(
            out,
            "Terminal {} renamed: `{}` -> `{}`",
            text(&d["terminal_id"]),
            text(&d["previous_name"]),
            text(&d["name"]),
        ),
        Cmd::Stop | Cmd::Kill | Cmd::Rm => {
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
        Cmd::Capture => {
            for l in d["lines"].as_array().map_or(&[][..], Vec::as_slice) {
                let _ = writeln!(out, "{}", text(l));
            }
            if !a.quiet && d["redacted"].as_u64().unwrap_or(0) > 0 {
                eprintln!(
                    "subak: {} secret-looking value(s) shown as *** (--no-redact shows them)",
                    d["redacted"]
                );
            }
            Ok(())
        }
        Cmd::History => {
            let h = &d["history"];
            writeln!(
                out,
                "Terminal `{}` ({}) — {}: {} line(s) of history ({}) kept in the Host's memory, \
                 limit {} lines / {}; {} evicted, {} cleared, {} in total; never written to \
                 disk\nscreen {}x{}{}, cursor at row {} column {}",
                text(&d["name"]),
                text(&d["terminal_id"]),
                text(&d["state"]),
                h["lines"],
                bytes_text(&h["bytes"]),
                h["limit_lines"],
                bytes_text(&h["limit_bytes"]),
                h["evicted_lines"],
                h["cleared_lines"],
                h["total_lines"],
                d["screen"]["size"]["cols"],
                d["screen"]["size"]["rows"],
                if d["screen"]["alternate"] == true {
                    " (a full-screen program is on the alternate screen)"
                } else {
                    ""
                },
                d["screen"]["cursor"]["y"],
                d["screen"]["cursor"]["x"],
            )
        }
        Cmd::ClearHistory => writeln!(
            out,
            "cleared {} line(s) ({}) of history of Terminal `{}`; its screen and processes are unchanged",
            d["cleared_lines"],
            bytes_text(&d["cleared_bytes"]),
            text(&d["name"]),
        ),
        _ => Ok(()),
    };
}
