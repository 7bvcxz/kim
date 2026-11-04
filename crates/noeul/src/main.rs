//! `noeul` — Noeul's CLI and resident instance (pre-alpha, Steps 49, 50 and 53; ADR 0023,
//! ADR 0026, ADR 0029).
//!
//! Follows docs/CLI_CONTRACT.md: results on stdout, diagnostics on stderr, exactly one JSON
//! document with `--json`, documented exit codes and registered error codes, `--dry-run` for every
//! state-changing command, no prompts when Noeul cannot ask. Headless commands (`status`,
//! `health`, `capabilities`, `version`, `config`, `hotkeys …`, `pin list`) never open a window,
//! a tray icon or a prompt, never register a hotkey and never start the instance
//! (NOEUL-FR-111). Only `start` and `run` start the instance, which owns the tray icon, the pins
//! and the global hotkeys; `capture`, `pin …` and `quit` ask it over the identity-checked local
//! pipe. Noeul makes no network connection (NOEUL-NFR-008).

mod config_cmd;
mod hotkeys_cmd;
mod instance;

use clap::{Args, CommandFactory, Parser, Subcommand};
use noeul::error::{Code, NoeulError};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;

const AFTER_HELP: &str = "\
While Noeul runs, F1 opens the region selection overlay: drag a rectangle, then double-click
inside it to pin it at once (also Enter, the toolbar's Pin button or a middle-click). Ctrl+C or
Copy copies it, Ctrl+S or Save saves it where you choose, Ctrl+Shift+S quick-saves it, Esc or
right-click outside cancels; `noeul capture --interactive` opens the same overlay. F3 pins the
clipboard image. The double-click can copy, save or do nothing instead
(`noeul config set gestures.double_click copy`), and `noeul config set gestures.preset snipaste`
switches to Snipaste-style gestures (double-click and Enter copy, Ctrl+T pins).
Annotate before you pin, copy or save: the toolbar's rectangle, ellipse, line, arrow, pen,
marker and text tools draw inside the selection (Ctrl+Z undoes, Ctrl+Y redoes, [ and ] or the
wheel change the width, Esc leaves the tool and keeps the drawing); a label takes Korean and
other input methods. Enter still pins, with the annotations.
Annotation also has mosaic, blur, an eraser and an irreversible solid redaction.
Pins float on top: drag to move, drag the border (or its shadow) to resize, wheel or Plus/Minus
to zoom, Ctrl+wheel or Ctrl+Plus/Ctrl+Minus for opacity (never below 10 %), 1/2 rotate right
and left, 3/4 flip, T or Shift+double-click for a thumbnail, Ctrl+Shift+T lets clicks pass
through (the tray's Make all pins clickable, a second `noeul start` or `noeul pin set --all
--click-through off` bring them back), 0 or middle-click for 100 % and opaque, H hides, Esc
closes, Shift+Esc destroys, Space edits the pin's annotations (Enter keeps them, Esc leaves the
pin as it was), right-click (or Shift+F10) opens the pin's menu. `noeul pin
list/hide/show/close/set` and `noeul pin group list/switch` reach them from scripts. Pins live in
memory only and do not survive a restart.
Not in this build (labeled planned): animated GIF pins (a pin of an animated GIF shows its first
frame and says so), settings window (61), capture to a file or the clipboard from the command
line and `noeul pin <file>` (62).

Exit codes: 0 ok · 1 failure (I/O, permission) · 2 usage · 3 not found (not running, no pin)
            4 integrity (protocol, identity) · 5 incompatible or unsupported platform
            6 unavailable (no unlocked desktop, unresponsive, clipboard busy)
            7 conflict or confirmation needed · 8 cancelled
--json prints exactly one JSON document on stdout, also for errors. Agents: add --json and
--no-input; every state-changing command supports --dry-run. Contract: docs/CLI_CONTRACT.md";

#[derive(Debug, Parser)]
#[command(
    name = "noeul",
    bin_name = "noeul",
    version,
    about = "Noeul — small native screenshot and floating-pin tool (pre-alpha)",
    long_about = "Noeul pins pictures to the desktop as floating windows. This pre-alpha build \
        has the resident instance (`noeul start`), its optional tray icon, F1 to select a region \
        over a frozen picture of every monitor and pin it with a double-click inside it (or \
        Enter), copy it or save it, F3 to pin the clipboard image, pins of a screen region or \
        the clipboard requested from the command line, and the floating pin engine: \
        always-on-top pins that move, zoom, resize, rotate, flip, fade, shrink to thumbnails, let \
        clicks through, hide, show, group and close independently, each with its own context \
        menu.\n\n\
        Noeul never captures on its own, never watches the clipboard, writes a picture to disk \
        only when you save one, uses no network, installs no hook or service, and never \
        captures a locked, secure or other user's desktop.",
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
struct Global {
    /// Print exactly one JSON document on stdout instead of human-readable text
    #[arg(long, global = true)]
    json: bool,
    /// Print only results and errors: no warnings on stderr
    #[arg(long, short, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Print extra diagnostics on stderr (stdout is unchanged)
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Never ask questions, even on a terminal (also KIM_NO_INPUT=1); confirmations need --yes
    #[arg(long, global = true)]
    no_input: bool,
    /// Pipe name under \\.\pipe\ (default: mouv-noeul-<user SID>-s<session>); for experiments
    /// and tests, it never touches the default instance
    #[arg(long, global = true, value_name = "NAME")]
    endpoint: Option<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Whether Noeul runs, its pins, hotkey registrations and desktop state (starts nothing)
    Status,
    /// Check that the running instance is Noeul of this user and answers (exit 0 when healthy)
    Health,
    /// Start the resident instance detached, or hand over to the running one
    Start(StartArgs),
    /// Run the instance in this process until it quits (what `start` launches)
    Run(StartArgs),
    /// End the instance: release its hotkeys and close its pins
    Quit(QuitArgs),
    /// Ask the running instance to capture a region and pin it, or to open (or cancel) the
    /// region selection overlay as F1 does
    Capture(CaptureArgs),
    /// Pins of the running instance: `noeul pin --clipboard` pins the clipboard image; the
    /// subcommands list, hide, show, close, reopen and change pins, and switch groups
    Pin(PinArgs),
    /// Noeul's keyboard shortcuts: list, change, disable, reset, check conflicts (never
    /// registers a hotkey or opens a window)
    #[command(subcommand)]
    Hotkeys(hotkeys_cmd::HotkeysCommand),
    /// Settings and paths: show them, change one (`config set`), reset them (`config reset`)
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Noeul's capabilities document: its commands and what each may change (read-only)
    Capabilities,
    /// Noeul's version and the contract, schema and IPC versions it speaks (read-only)
    Version,
}

#[derive(Debug, Args)]
pub struct StartArgs {
    /// Do not show the notification-area icon this time
    #[arg(long)]
    pub no_tray: bool,
    /// Start with every shortcut off (also KIM_NO_HOTKEYS=1); hotkeys.json is not touched
    #[arg(long)]
    pub no_hotkeys: bool,
    /// Show what would happen; start nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct QuitArgs {
    /// Confirm closing open pins without asking
    #[arg(long, short)]
    pub yes: bool,
    /// Show what would happen; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct CaptureArgs {
    /// The region in device pixels of the virtual desktop: x,y,width,height
    #[arg(
        long,
        value_name = "X,Y,W,H",
        allow_hyphen_values = true,
        required_unless_present_any = ["interactive", "cancel"],
        conflicts_with_all = ["interactive", "cancel"]
    )]
    pub region: Option<String>,
    /// Pin the captured region (required with --region in this build; --output and --clipboard
    /// are planned)
    #[arg(long, requires = "region")]
    pub pin: bool,
    /// Open the region selection overlay on the user's desktop, as F1 does (a visible window;
    /// the user selects, Enter pins, Esc cancels)
    #[arg(long, conflicts_with = "cancel")]
    pub interactive: bool,
    /// Close an open selection overlay without pinning, copying or saving anything
    #[arg(long)]
    pub cancel: bool,
    /// Start Noeul first when it is not running
    #[arg(long, conflicts_with = "cancel")]
    pub start: bool,
    /// Check the region (or whether an overlay could open) and the desktop; capture nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct PinArgs {
    #[command(subcommand)]
    command: Option<PinCommand>,
    #[command(flatten)]
    pub create: PinCreateArgs,
}

#[derive(Debug, Args)]
pub struct PinCreateArgs {
    /// Read the clipboard once and pin its image, as F3 does (without taking focus; inside the
    /// work area of the monitor, zoomed out when larger)
    #[arg(long)]
    pub clipboard: bool,
    /// Where the picture's top-left corner goes: x,y in device pixels (default: the pointer)
    #[arg(
        long,
        value_name = "X,Y",
        allow_hyphen_values = true,
        requires = "clipboard"
    )]
    pub at: Option<String>,
    /// Start Noeul first when it is not running
    #[arg(long, requires = "clipboard")]
    pub start: bool,
    /// Check the desktop; read nothing and pin nothing
    #[arg(long, requires = "clipboard")]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
enum PinCommand {
    /// The open pins (IDs, geometry, zoom, visibility; never pixels)
    List,
    /// Hide a pin (or --all) without closing it; `noeul pin show` brings it back
    Hide(PinHideArgs),
    /// Show hidden pins again; --on-screen also moves pins no monitor shows onto one
    Show(PinShowArgs),
    /// Close a pin (or --all); only the last closed one can be reopened
    Close(PinCloseArgs),
    /// Reopen the last closed pin
    Reopen(DryRun),
    /// Change pins precisely: place, zoom, opacity, rotation and flips, thumbnail,
    /// click-through, group (every change can be undone the same way)
    Set(Box<PinSetArgs>),
    /// Pin groups: list them, or switch to one (its pins show, the others hide)
    #[command(subcommand)]
    Group(PinGroupCommand),
}

#[derive(Debug, Subcommand)]
enum PinGroupCommand {
    /// The groups, their pins and the current group
    List,
    /// Switch to a group (created when new): its pins show, every other pin hides, new pins go
    /// to it
    Switch(PinGroupSwitchArgs),
}

#[derive(Debug, Args)]
pub struct PinGroupSwitchArgs {
    /// The group's name (1–32 letters, digits, spaces, `-`, `_` or `.`)
    pub name: String,
    /// Show what would change; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PinSetArgs {
    /// The pin ID, e.g. pin-1
    #[arg(required_unless_present_any = ["all", "group"], conflicts_with_all = ["all", "group"])]
    pub id: Option<String>,
    /// Every pin
    #[arg(long, conflicts_with = "group")]
    pub all: bool,
    /// Every pin of this group
    #[arg(long, value_name = "NAME")]
    pub group: Option<String>,
    /// Put the picture's top-left corner exactly here: x,y in device pixels (one pin)
    #[arg(long, value_name = "X,Y", allow_hyphen_values = true, requires = "id")]
    pub at: Option<String>,
    /// Zoom in percent, 10–800 (100: one image pixel per device pixel)
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u32).range(10..=800))]
    pub zoom: Option<u32>,
    /// Opacity in percent, 10–100
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u32).range(10..=100))]
    pub opacity: Option<u32>,
    /// Turn a quarter: cw (right) or ccw (left)
    #[arg(long, value_parser = ["cw", "ccw"])]
    pub rotate: Option<String>,
    /// The rotation in degrees clockwise: 0, 90, 180 or 270
    #[arg(long, value_parser = ["0", "90", "180", "270"])]
    pub rotation: Option<String>,
    /// Flip as shown: horizontal or vertical
    #[arg(long, value_parser = ["horizontal", "vertical"])]
    pub flip: Option<String>,
    /// Mirrored left-right before the rotation: on or off (`--rotation 0 --mirrored off` is the
    /// picture as captured)
    #[arg(long, value_parser = ["on", "off"])]
    pub mirrored: Option<String>,
    /// Thumbnail mode: on or off
    #[arg(long, value_parser = ["on", "off"])]
    pub thumbnail: Option<String>,
    /// Let mouse clicks pass through the pin: on or off
    #[arg(long, value_parser = ["on", "off"])]
    pub click_through: Option<String>,
    /// Move the pins to this group
    #[arg(long, value_name = "NAME")]
    pub move_to_group: Option<String>,
    /// Show what would change; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PinHideArgs {
    /// The pin ID, e.g. pin-1
    #[arg(required_unless_present_any = ["all", "group"], conflicts_with_all = ["all", "group"])]
    pub id: Option<String>,
    /// Every pin of this group
    #[arg(long, value_name = "NAME", conflicts_with = "all")]
    pub group: Option<String>,
    /// Every pin
    #[arg(long)]
    pub all: bool,
    /// Show what would change; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PinShowArgs {
    /// The pin ID, e.g. pin-1
    #[arg(required_unless_present_any = ["all", "group"], conflicts_with_all = ["all", "group"])]
    pub id: Option<String>,
    /// Every pin of this group
    #[arg(long, value_name = "NAME", conflicts_with = "all")]
    pub group: Option<String>,
    /// Every pin
    #[arg(long)]
    pub all: bool,
    /// Also move pins that no monitor shows (after a monitor was removed) into the nearest
    /// monitor's work area ("Bring pins on screen")
    #[arg(long)]
    pub on_screen: bool,
    /// Show what would change; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PinCloseArgs {
    /// The pin ID, e.g. pin-1
    #[arg(required_unless_present_any = ["all", "group"], conflicts_with_all = ["all", "group"])]
    pub id: Option<String>,
    /// Every pin of this group
    #[arg(long, value_name = "NAME", conflicts_with = "all")]
    pub group: Option<String>,
    /// Close every pin
    #[arg(long)]
    pub all: bool,
    /// Confirm without asking
    #[arg(long, short)]
    pub yes: bool,
    /// Show what would close; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DryRun {
    /// Show what would happen; change nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Effective settings and the per-user paths
    Show,
    /// Change one setting in config.toml (e.g. `gestures.double_click copy`, or
    /// `gestures.preset snipaste` for the opt-in Snipaste-style gestures)
    Set(config_cmd::SetArgs),
    /// Reset settings to their defaults: one section (general, capture, gestures, pins) or all
    Reset(config_cmd::ResetArgs),
}

/// Output options shared by every command (docs/CLI_CONTRACT.md §3–§4).
#[derive(Debug, Clone)]
pub struct Ctx {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub no_input: bool,
    pub endpoint: Option<String>,
}

impl Ctx {
    fn new(g: &Global) -> Ctx {
        let env_no_input = std::env::var_os("KIM_NO_INPUT")
            .is_some_and(|v| !v.is_empty() && v != "0" && v != "false");
        Ctx {
            json: g.json,
            quiet: g.quiet,
            verbose: g.verbose,
            no_input: g.no_input || env_no_input,
            endpoint: g.endpoint.clone(),
        }
    }

    /// Whether Noeul may ask a question now: never with `--json` or `--no-input`, and only when
    /// both stdin and stderr are terminals.
    pub fn can_prompt(&self) -> bool {
        !self.json
            && !self.no_input
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal()
    }
}

/// Asks a yes/no question on stderr; `None` when Noeul cannot ask.
pub fn confirm(ctx: &Ctx, question: &str, expected: &str) -> Option<bool> {
    if !ctx.can_prompt() {
        return None;
    }
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line.trim().eq_ignore_ascii_case(expected))
}

/// Result of a command: JSON data plus a human rendering.
#[derive(Debug)]
pub struct Outcome {
    pub data: Value,
    pub human: String,
    pub warnings: Vec<String>,
}

impl Outcome {
    pub fn new(data: Value, human: impl Into<String>) -> Outcome {
        Outcome {
            data,
            human: human.into(),
            warnings: Vec::new(),
        }
    }
}

/// The capabilities document; its `version` must equal the package version (tested).
pub const CAPABILITIES: &str = include_str!("capabilities.json");

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let json_requested = argv.iter().skip(1).any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(e) => return parse_failure(&e, &argv, json_requested),
    };
    let ctx = Ctx::new(&cli.global);
    // Coordinates in output and arguments are device pixels, as in the instance (NOEUL-FR-090).
    #[cfg(windows)]
    noeul::win::per_monitor_dpi_awareness();
    let Some(command) = cli.command else {
        if ctx.json {
            let err = NoeulError::new(Code::Usage, "no command given")
                .hint("run `noeul --help` to list the commands");
            print_json(&error_doc("noeul", &err));
            return ExitCode::from(Code::Usage.exit_code());
        }
        let _ = Cli::command().print_help();
        return ExitCode::SUCCESS;
    };
    let name = command_name(&command);
    if ctx.verbose {
        eprintln!(
            "noeul: {name}: noeul {} (CLI contract v1, IPC v1)",
            noeul::VERSION
        );
    }
    let result = match &command {
        Command::Status => instance::status(&ctx),
        Command::Health => instance::health(&ctx),
        Command::Start(a) => instance::start(&ctx, a),
        Command::Run(a) => instance::run(&ctx, a),
        Command::Quit(a) => instance::quit(&ctx, a),
        Command::Capture(a) => instance::capture(&ctx, a),
        Command::Pin(p) => match &p.command {
            None => instance::pin_create(&ctx, &p.create),
            Some(PinCommand::List) => instance::pin_list(&ctx),
            Some(PinCommand::Hide(a)) => instance::pin_hide(&ctx, a),
            Some(PinCommand::Show(a)) => instance::pin_show(&ctx, a),
            Some(PinCommand::Close(a)) => instance::pin_close(&ctx, a),
            Some(PinCommand::Reopen(a)) => instance::pin_reopen(&ctx, a.dry_run),
            Some(PinCommand::Set(a)) => instance::pin_set(&ctx, a),
            Some(PinCommand::Group(PinGroupCommand::List)) => instance::pin_group_list(&ctx),
            Some(PinCommand::Group(PinGroupCommand::Switch(a))) => {
                instance::pin_group_switch(&ctx, a)
            }
        },
        Command::Hotkeys(c) => hotkeys_cmd::run(c, &ctx),
        Command::Config(ConfigCommand::Show) => instance::config_show(),
        Command::Config(ConfigCommand::Set(a)) => config_cmd::set(&ctx, a),
        Command::Config(ConfigCommand::Reset(a)) => config_cmd::reset(&ctx, a),
        Command::Capabilities => Ok(capabilities()),
        Command::Version => Ok(version()),
    };
    finish(name, result, &ctx)
}

/// The dotted command path used as the envelope's `command` (docs/CLI_CONTRACT.md §7).
fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Status => "status",
        Command::Health => "health",
        Command::Start(_) => "start",
        Command::Run(_) => "run",
        Command::Quit(_) => "quit",
        Command::Capture(_) => "capture",
        Command::Pin(p) => match p.command {
            None => "pin",
            Some(PinCommand::List) => "pin.list",
            Some(PinCommand::Hide(_)) => "pin.hide",
            Some(PinCommand::Show(_)) => "pin.show",
            Some(PinCommand::Close(_)) => "pin.close",
            Some(PinCommand::Reopen(_)) => "pin.reopen",
            Some(PinCommand::Set(_)) => "pin.set",
            Some(PinCommand::Group(PinGroupCommand::List)) => "pin.group.list",
            Some(PinCommand::Group(PinGroupCommand::Switch(_))) => "pin.group.switch",
        },
        Command::Hotkeys(c) => hotkeys_cmd::command_name(c),
        Command::Config(ConfigCommand::Show) => "config.show",
        Command::Config(ConfigCommand::Set(_)) => "config.set",
        Command::Config(ConfigCommand::Reset(_)) => "config.reset",
        Command::Capabilities => "capabilities",
        Command::Version => "version",
    }
}

fn capabilities() -> Outcome {
    let data: Value = serde_json::from_str(CAPABILITIES).expect("valid built-in JSON");
    let mut human = format!(
        "noeul {} — pre-alpha (CLI contract v1, IPC v1)\n",
        noeul::VERSION
    );
    for c in data["commands"].as_array().into_iter().flatten() {
        human.push_str(&format!(
            "  {:<16} {}\n",
            c["name"].as_str().unwrap_or(""),
            c["summary"].as_str().unwrap_or("")
        ));
    }
    Outcome::new(data, human)
}

fn version() -> Outcome {
    Outcome::new(
        json!({
            "tool": "noeul",
            "version": noeul::VERSION,
            "cli_contract": 1,
            "json_schema_version": 1,
            "ipc_protocol": {"min": noeul::ipc::PROTOCOL_MIN, "max": noeul::ipc::PROTOCOL_MAX},
            "hotkey_contract": kim_hotkeys::manifest::HOTKEY_CONTRACT,
            "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        }),
        format!("noeul {}\n", noeul::VERSION),
    )
}

/// Best-effort command path of arguments that failed to parse: the leading subcommand words.
fn parsed_path(argv: &[OsString]) -> Vec<String> {
    let mut cmd = Cli::command();
    let mut path = Vec::new();
    for arg in argv.iter().skip(1) {
        let Some(a) = arg.to_str() else { break };
        if a.starts_with('-') {
            continue;
        }
        let Some(sub) = cmd.find_subcommand(a).cloned() else {
            break;
        };
        path.push(sub.get_name().to_string());
        cmd = sub;
    }
    path
}

fn parse_failure(e: &clap::Error, argv: &[OsString], json: bool) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    let path = parsed_path(argv);
    let name = if path.is_empty() {
        "noeul".to_string()
    } else {
        path.join(".")
    };
    if json {
        let words: Vec<&str> = std::iter::once("noeul")
            .chain(path.iter().map(String::as_str))
            .collect();
        let first = e.to_string();
        let first = first
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches("error: ")
            .to_string();
        let err =
            NoeulError::new(Code::Usage, first).hint(format!("see `{} --help`", words.join(" ")));
        print_json(&error_doc(&name, &err));
    } else {
        let _ = e.print();
    }
    ExitCode::from(Code::Usage.exit_code())
}

fn finish(name: &str, result: Result<Outcome, NoeulError>, ctx: &Ctx) -> ExitCode {
    match result {
        Ok(out) => {
            if ctx.json {
                print_json(&json!({
                    "ok": true,
                    "command": name,
                    "schema_version": 1,
                    "data": out.data,
                    "warnings": out.warnings,
                }));
            } else {
                if !ctx.quiet {
                    let mut err = std::io::stderr().lock();
                    for w in &out.warnings {
                        let _ = writeln!(err, "noeul: warning: {w}");
                    }
                }
                let _ = write!(std::io::stdout().lock(), "{}", out.human);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            if ctx.json {
                print_json(&error_doc(name, &err));
            } else {
                let mut out = std::io::stderr().lock();
                let _ = writeln!(out, "noeul: error: {}", err.message);
                for d in &err.details {
                    let _ = writeln!(out, "  - {d}");
                }
                if let Some(h) = &err.hint {
                    let _ = writeln!(out, "hint: {h}");
                }
            }
            if ctx.verbose {
                eprintln!(
                    "noeul: error code `{}` (exit code {})",
                    err.code,
                    err.code.exit_code()
                );
            }
            ExitCode::from(err.code.exit_code())
        }
    }
}

fn error_doc(command: &str, err: &NoeulError) -> Value {
    json!({
        "ok": false,
        "command": command,
        "schema_version": 1,
        "error": {
            "code": err.code.as_str(),
            "exit_code": err.code.exit_code(),
            "message": err.message,
            "hint": err.hint,
            "details": err.details,
        },
    })
}

fn print_json(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer_pretty(&mut out, v);
    let _ = writeln!(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn leaf_commands() -> BTreeSet<String> {
        fn walk(cmd: &clap::Command, prefix: &str, out: &mut BTreeSet<String>) {
            for sub in cmd.get_subcommands() {
                let name = if prefix.is_empty() {
                    sub.get_name().to_string()
                } else {
                    format!("{prefix}.{}", sub.get_name())
                };
                // A command with subcommands is runnable on its own too when it has options of
                // its own (`noeul pin --clipboard`).
                let own = sub
                    .get_arguments()
                    .any(|a| a.get_long().is_some_and(|l| l != "help"));
                if sub.get_subcommands().next().is_none() {
                    out.insert(name);
                } else {
                    if own {
                        out.insert(name.clone());
                    }
                    walk(sub, &name, out);
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(&Cli::command(), "", &mut out);
        out
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn capabilities_list_exactly_the_real_commands_with_dry_run_flags() {
        let caps: Value = serde_json::from_str(CAPABILITIES).unwrap();
        assert_eq!(
            caps["version"],
            noeul::VERSION,
            "bump capabilities.json with the crate"
        );
        let listed: BTreeSet<String> = caps["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        let mut real = leaf_commands();
        real.remove("help");
        assert_eq!(
            listed, real,
            "capabilities.json lists every command, nothing planned"
        );
        for c in caps["commands"].as_array().unwrap() {
            let path: Vec<&str> = c["name"].as_str().unwrap().split('.').collect();
            let mut cmd = Cli::command();
            for p in &path {
                cmd = cmd.find_subcommand(p).unwrap().clone();
            }
            let has = |long: &str| cmd.get_arguments().any(|a| a.get_long() == Some(long));
            assert_eq!(c["dry_run"] == true, has("dry-run"), "{path:?} --dry-run");
            if c["requires_yes"] == true && c["name"] != "hotkeys.reset" {
                assert!(has("yes"), "{path:?} --yes");
            }
        }
    }

    #[test]
    fn the_capabilities_example_is_this_document() {
        let ours: Value = serde_json::from_str(CAPABILITIES).unwrap();
        let example: Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../docs/contracts/examples/tool-capabilities-v1/noeul.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ours, example, "keep the published example identical");
    }

    #[test]
    fn failed_parses_report_the_command_path() {
        let argv: Vec<OsString> = ["noeul", "pin", "close", "--bogus"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(parsed_path(&argv), ["pin", "close"]);
    }
}
