//! `ddalgak` — Ddalgak's headless CLI (docs/DDALGAK_ENGINE.md §6, docs/CLI_CONTRACT.md).
//!
//! Pre-alpha (Step 66): engine status and health, turning the engine on, pausing, resuming and
//! turning it off in the running tray app (`ddalgak-gui.exe`) over an identity-checked local pipe,
//! ending the app, and opt-in start at sign-in. The CLI **never** starts the app, installs a
//! hook, opens a window or shows a tray icon (`DDALGAK-FR-060`, `DDALGAK-SEC-010`); with no app
//! running, commands that need it fail with `engine_not_running`.

mod cli;
// Portable, but until the Linux engine (D7, ADR 0011) only the Windows client uses every code.
#[cfg_attr(not(windows), allow(dead_code))]
mod output;

use cli::{Args, Cmd};
use output::{CliError, Code};
use serde_json::Value;
use std::io::Write;
use std::process::ExitCode;

const HELP: &str = "\
ddalgak — Ddalgak automation engine (pre-alpha: engine core, Step 66)

USAGE:
    ddalgak [--help | --version]
    ddalgak capabilities [--json]
    ddalgak status [--endpoint <name>] [--json]
    ddalgak health [--endpoint <name>] [--json]
    ddalgak engine start|pause|resume|stop [--endpoint <name>] [--dry-run] [--json]
    ddalgak pause | resume  [--endpoint <name>] [--dry-run] [--json]
    ddalgak quit [--endpoint <name>] [--dry-run] [--json]
    ddalgak startup status|enable|disable [--dry-run] [--json]

COMMANDS:
    status        Engine state (off, running, paused) and why it is off, the rules summary
                  (active rule IDs; inactive rules with their codes), Ddalgak's own global
                  shortcuts, recent activity (rule IDs and outcome codes only, never keys) and
                  start at sign-in. Works without the app (it then reads the files).
    health        Ask the running app: is its keyboard hook thread alive, how long do hook
                  callbacks take, were signals dropped, how often did its threads wake up and
                  how much CPU has it used. On demand only; nothing runs on a timer. Problems
                  are listed in `data.healthy` and `data.problems`. Ddalgak never reinstalls a
                  hook by itself.
    engine start  Turn the engine on in the running app (alias: engine on). Reads rules.json
                  again; an invalid file keeps it off (`rules_invalid`). Refused while the app
                  runs as administrator (`engine_elevated`).
    engine pause  Pause rules; engine-control rules keep working; held keys are released.
    engine resume Resume a paused engine. Never turns an engine on that is off.
    engine stop   Turn the engine off (alias: engine off): the keyboard hook is removed.
    quit          End the tray app (the engine is turned off first).
    startup ...   Opt-in start of the tray app when you sign in, as one value under
                  HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run. No service, no task.

    The CLI never starts the app: start ddalgak-gui.exe yourself (it shows a tray icon). Every
    command checks, before sending anything, that the pipe is served by ddalgak-gui.exe of this
    user (`app_identity_mismatch` otherwise). The emergency chord Ctrl+Alt+Shift+Escape always
    turns the engine off from the keyboard, whatever the rules say.

OPTIONS:
    --endpoint <name>  Pipe name under \\\\.\\pipe\\ (default: mouv-ddalgak-<user SID>-s<session>).
    --dry-run          Show what would happen; change nothing.
    --json             One JSON document on stdout (docs/CLI_CONTRACT.md §7).
    -q, --quiet / -v, --verbose   Less / more diagnostics on stderr.

FILES:
    %APPDATA%\\mouv\\ddalgak\\rules.json     rules (ddalgak.rules v1; editing commands: Step 67)
    %APPDATA%\\mouv\\ddalgak\\hotkeys.json   Ddalgak's own shortcuts (engine.toggle, engine.pause)
    %LOCALAPPDATA%\\mouv\\ddalgak\\          crash and emergency markers

Pre-alpha: rules can only control the engine (pause, resume, turn off); remaps, text, launches,
mouse and modes arrive in later steps and stay inactive until then. Linux is not supported yet.
No command opens a window, installs anything or uses the network.
";

/// The capabilities document (`kim.capabilities` v1); a test keeps it equal to
/// docs/contracts/examples/tool-capabilities-v1/ddalgak.json.
const CAPABILITIES: &str = include_str!("capabilities.json");

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let json_flag = raw.iter().any(|a| a == "--json");
    let (cmd, args) = match cli::parse(&raw) {
        Ok(p) => p,
        Err((name, e)) => return fail(&name, &e, json_flag),
    };
    match cmd {
        Cmd::Help => {
            let _ = write!(std::io::stdout().lock(), "{HELP}");
            ExitCode::SUCCESS
        }
        Cmd::Version => {
            let _ = writeln!(std::io::stdout().lock(), "ddalgak {}", ddalgak::VERSION);
            ExitCode::SUCCESS
        }
        _ => match run(cmd, &args) {
            Ok((data, warnings)) => {
                let mut out = std::io::stdout().lock();
                if args.json {
                    let _ = writeln!(out, "{}", output::success(&cmd.name(), &data, &warnings));
                } else {
                    if !args.quiet {
                        for w in &warnings {
                            eprintln!("ddalgak: warning: {w}");
                        }
                    }
                    let _ = write!(out, "{}", human(cmd, &data));
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                if args.verbose {
                    eprintln!(
                        "ddalgak: {} failed with {} (exit {})",
                        cmd.name(),
                        e.code.as_str(),
                        e.code.exit_code()
                    );
                }
                fail(&cmd.name(), &e, args.json)
            }
        },
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

type Outcome = Result<(Value, Vec<String>), CliError>;

#[cfg(windows)]
fn run(cmd: Cmd, a: &Args) -> Outcome {
    use ddalgak::ipc::Method;
    if cmd == Cmd::Capabilities {
        let data: Value = serde_json::from_str(CAPABILITIES).expect("valid built-in JSON");
        return Ok((data, Vec::new()));
    }
    let endpoint = match &a.endpoint {
        Some(e) => e.clone(),
        None => ddalgak::win::pipe::default_endpoint()
            .map_err(|e| CliError::new(Code::Io, format!("cannot name the local endpoint: {e}")))?,
    };
    if a.verbose {
        eprintln!("ddalgak: endpoint \\\\.\\pipe\\{endpoint}");
    }
    match cmd {
        Cmd::Status => client::status(&endpoint),
        Cmd::Health => client::health(&endpoint),
        Cmd::Engine(r) => {
            client::call(&endpoint, Method::Engine(r), a.dry_run).map(|(v, _)| (v, Vec::new()))
        }
        Cmd::Quit => client::call(&endpoint, Method::Quit, a.dry_run).map(|(v, _)| (v, Vec::new())),
        Cmd::StartupStatus => client::startup_status().map(|v| (v, Vec::new())),
        Cmd::StartupEnable => client::startup_change(true, a.dry_run),
        Cmd::StartupDisable => client::startup_change(false, a.dry_run),
        Cmd::Help | Cmd::Version | Cmd::Capabilities => unreachable!("handled above"),
    }
}

#[cfg(not(windows))]
fn run(cmd: Cmd, a: &Args) -> Outcome {
    // The options that select an app endpoint have nothing to talk to here.
    let _ = (&a.endpoint, a.dry_run);
    if cmd == Cmd::Capabilities {
        let data: Value = serde_json::from_str(CAPABILITIES).expect("valid built-in JSON");
        return Ok((data, Vec::new()));
    }
    // ADR 0011: no Linux engine before milestone D7 (DDALGAK-NFR-009).
    Err(CliError::new(
        Code::UnsupportedPlatform,
        "the Ddalgak engine needs Windows; Linux support is planned for milestone D7",
    ))
}

#[cfg(windows)]
mod client {
    use crate::output::{CliError, Code};
    use ddalgak::ipc::{self, Method};
    use ddalgak::paths::Paths;
    use ddalgak::rules;
    use ddalgak::shortcuts::Shortcuts;
    use ddalgak::startup::{self, Store};
    use ddalgak::win::{self, pipe, startup::Registry};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// Who answered, as verified before sending.
    pub struct Peer {
        pub pid: u32,
        pub image: String,
        pub latency_ms: u128,
    }

    fn map_code(code: &str) -> Code {
        Code::parse(code).unwrap_or(match code {
            "app_unresponsive" => Code::AppUnresponsive,
            _ => Code::Internal,
        })
    }

    /// One request to the running app, after verifying who serves the pipe.
    pub fn call(endpoint: &str, method: Method, dry_run: bool) -> Result<(Value, Peer), CliError> {
        let started = Instant::now();
        let conn = pipe::connect(endpoint, Duration::from_secs(2)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CliError::new(Code::EngineNotRunning, "Ddalgak is not running")
                    .hint("start ddalgak-gui.exe (it shows a tray icon); the CLI never starts it")
            } else {
                CliError::new(
                    Code::AppUnresponsive,
                    format!("cannot connect to the app: {e}"),
                )
            }
        })?;
        let pid = conn.server_pid().ok_or_else(|| {
            CliError::new(
                Code::AppIdentityMismatch,
                "the endpoint's server cannot be identified",
            )
        })?;
        let image = win::process_image(pid).map_err(|e| {
            CliError::new(
                Code::AppIdentityMismatch,
                format!("cannot verify the process serving the endpoint (pid {pid}): {e}"),
            )
        })?;
        let name = image
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let same_user = win::process_user_sid(pid).ok() == win::user_sid().ok();
        if !name.eq_ignore_ascii_case(ddalgak::GUI_EXE) || !same_user {
            return Err(CliError::new(
                Code::AppIdentityMismatch,
                format!(
                    "the endpoint is served by `{name}` (pid {pid}){}, not by ddalgak-gui.exe of this user; nothing was sent",
                    if same_user { "" } else { " of another user" }
                ),
            ));
        }
        conn.write_all_bytes(ipc::request(method, dry_run).as_bytes())
            .map_err(|e| CliError::new(Code::AppUnresponsive, format!("cannot send: {e}")))?;
        let deadline = Instant::now() + Duration::from_millis(ipc::TIMEOUT_MS + 500);
        let line = pipe::read_line(&conn, ipc::MAX_RESPONSE, deadline).map_err(|e| {
            CliError::new(
                Code::AppUnresponsive,
                format!("the app did not answer ({e}); it is neither stopped nor restarted"),
            )
        })?;
        let latency_ms = started.elapsed().as_millis();
        let result = ipc::parse_response(&line)
            .map_err(|(code, msg)| CliError::new(map_code(&code), msg))?;
        Ok((
            result,
            Peer {
                pid,
                image: image.display().to_string(),
                latency_ms,
            },
        ))
    }

    fn gui_path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(ddalgak::GUI_EXE)))
            .unwrap_or_else(|| PathBuf::from(ddalgak::GUI_EXE))
    }

    pub fn startup_status() -> Result<Value, CliError> {
        let store = Registry::for_cli();
        let gui = gui_path();
        let s = startup::state(&store, &gui).map_err(|e| {
            CliError::new(Code::Io, format!("cannot read {}: {e}", store.location()))
        })?;
        Ok(json!({
            "enabled": s.enabled,
            "command": s.command,
            "foreign": s.foreign,
            "location": store.location(),
            "program": gui.display().to_string(),
        }))
    }

    pub fn startup_change(enable: bool, dry_run: bool) -> crate::Outcome {
        let mut store = Registry::for_cli();
        let gui = gui_path();
        let r = if enable {
            startup::enable(&mut store, &gui, dry_run)
        } else {
            startup::disable(&mut store, dry_run)
        };
        let o = r.map_err(|e| {
            if enable && !gui.is_file() {
                CliError::new(Code::NotInstalled, e)
            } else {
                CliError::new(Code::PermissionDenied, e)
            }
        })?;
        let mut warnings = Vec::new();
        if enable && o.changed && !dry_run {
            warnings.push(
                "ddalgak-gui.exe will start with its tray icon when you sign in; the engine stays \
                 off unless rules.json says engine_on_start: on"
                    .to_string(),
            );
        }
        Ok((
            json!({
                "dry_run": dry_run,
                "changed": o.changed,
                "before": o.before,
                "after": o.after,
                "location": store.location(),
            }),
            warnings,
        ))
    }

    /// Status from the app, or from the files when it is not running.
    pub fn status(endpoint: &str) -> crate::Outcome {
        let startup = startup_status().unwrap_or_else(|e| json!({ "error": e.message }));
        match call(endpoint, Method::Status, false) {
            Ok((mut v, _)) => {
                v["startup"] = startup;
                Ok((v, Vec::new()))
            }
            Err(e) if e.code == Code::EngineNotRunning => {
                let paths = Paths::from_env().map_err(|e| CliError::new(Code::Io, e))?;
                let loaded = rules::load(&paths.rules());
                let shortcuts = Shortcuts::load(&paths, false);
                let mut warnings = Vec::new();
                if !loaded.state.usable() {
                    warnings.push(
                        "rules.json is not usable; the engine cannot be turned on with it".into(),
                    );
                }
                Ok((
                    json!({
                        "app": { "running": false },
                        "engine": {
                            "state": "off",
                            "off_reason": "app_not_running",
                            "last_change": null,
                            "hook": { "installed": false, "observe_only": false },
                        },
                        "profile": { "id": loaded.profile, "mode": loaded.mode },
                        "rules": rules::to_json(&loaded),
                        "shortcuts": shortcuts.to_json(&|_| Some("not_running")),
                        "activity": [],
                        "startup": startup,
                    }),
                    warnings,
                ))
            }
            Err(e) => Err(e),
        }
    }

    pub fn health(endpoint: &str) -> crate::Outcome {
        let (mut v, peer) = call(endpoint, Method::Health, false)?;
        v["app"] = json!({
            "pid": peer.pid,
            "image": peer.image,
            "verified": true,
            "latency_ms": u64::try_from(peer.latency_ms).unwrap_or(u64::MAX),
        });
        Ok((v, Vec::new()))
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

/// Human output (stdout).
fn human(cmd: Cmd, d: &Value) -> String {
    let mut o = String::new();
    match cmd {
        Cmd::Capabilities => {
            o.push_str(&format!(
                "ddalgak {} — pre-alpha engine (CLI contract v1)\ncommands: {}\n",
                ddalgak::VERSION,
                d["commands"]
                    .as_array()
                    .map(|c| c
                        .iter()
                        .map(|c| s(&c["name"]))
                        .collect::<Vec<_>>()
                        .join(", "))
                    .unwrap_or_default()
            ));
        }
        Cmd::Status => {
            let app = if d["app"]["running"] == true {
                format!(
                    "running (pid {}, up {} s{}{})",
                    d["app"]["pid"],
                    d["app"]["uptime_s"],
                    if d["app"]["elevated"] == true {
                        ", elevated"
                    } else {
                        ""
                    },
                    if d["app"]["test_mode"] == true {
                        ", test mode"
                    } else {
                        ""
                    }
                )
            } else {
                "not running".into()
            };
            o.push_str(&format!("app:      {app}\n"));
            let reason = match &d["engine"]["off_reason"] {
                Value::String(r) => format!(" ({r})"),
                _ => String::new(),
            };
            o.push_str(&format!("engine:   {}{reason}\n", s(&d["engine"]["state"])));
            o.push_str(&format!(
                "profile:  {} (mode {})\n",
                s(&d["profile"]["id"]),
                s(&d["profile"]["mode"])
            ));
            let r = &d["rules"];
            o.push_str(&format!(
                "rules:    {} — {} active, {} disabled, {} inactive, {} in other modes\n",
                s(&r["file"]),
                r["active"],
                r["disabled"],
                r["inactive"].as_array().map_or(0, Vec::len),
                r["other_modes"]
            ));
            if let Some(p) = r["problem"].as_str() {
                o.push_str(&format!("          problem: {p}\n"));
            }
            for i in r["inactive"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "          inactive {} ({}): {}\n",
                    s(&i["rule"]),
                    s(&i["code"]),
                    s(&i["reason"])
                ));
            }
            let sc = d["shortcuts"]["global"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if sc.is_empty() {
                o.push_str("shortcuts: none bound (hotkeys.json)\n");
            }
            for x in sc {
                o.push_str(&format!(
                    "shortcut: {} = {} ({})\n",
                    s(&x["action"]),
                    s(&x["keys"]),
                    s(&x["registration"])
                ));
            }
            o.push_str(&format!(
                "startup:  {}\n",
                if d["startup"]["enabled"] == true {
                    "starts at sign-in"
                } else {
                    "off"
                }
            ));
            for a in d["activity"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "activity: {} {} {}\n",
                    s(&a["at"]),
                    s(&a["rule"]),
                    s(&a["outcome"])
                ));
            }
        }
        Cmd::Health => {
            o.push_str(&format!(
                "{}: app pid {} verified as {}; answered in {} ms\n",
                if d["healthy"] == true {
                    "healthy"
                } else {
                    "unhealthy"
                },
                d["app"]["pid"],
                s(&d["app"]["image"]),
                d["app"]["latency_ms"]
            ));
            o.push_str(&format!(
                "engine {}, hook installed {}, hook thread alive {}\n",
                s(&d["engine"]["state"]),
                d["engine"]["hook"]["installed"],
                d["engine"]["hook"]["thread_alive"]
            ));
            let c = &d["counters"];
            o.push_str(&format!(
                "hook callbacks {} (max {} µs, p99 ≤ {} µs), consumed {}, dropped signals {}\n",
                c["callbacks"],
                c["callback_us"]["max"],
                s(&c["callback_us"]["p99_at_most"]),
                c["consumed"],
                c["dropped_signals"]
            ));
            o.push_str(&format!(
                "wake-ups: hook thread {}, worker {}, app {}; CPU {} ms in {} s\n",
                c["wakeups"]["hook_thread"],
                c["wakeups"]["worker"],
                c["wakeups"]["host"],
                d["process"]["cpu_ms"],
                d["process"]["uptime_s"]
            ));
            for p in d["problems"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "problem {}: {}\n",
                    s(&p["code"]),
                    s(&p["message"])
                ));
            }
        }
        Cmd::Engine(_) | Cmd::Quit => {
            if d["dry_run"] == true {
                let w = if cmd == Cmd::Quit {
                    &d["engine"]
                } else {
                    &d["would"]
                };
                o.push_str(&format!(
                    "dry run, nothing changed: {}\n",
                    if cmd == Cmd::Quit {
                        format!("would end the app (engine {})", s(&w["state"]))
                    } else {
                        format!("engine would go {} → {}", s(&w["from"]), s(&w["to"]))
                    }
                ));
            } else if cmd == Cmd::Quit {
                o.push_str(&format!("Ddalgak (pid {}) is quitting\n", d["pid"]));
            } else {
                let c = &d["change"];
                o.push_str(&if c["changed"] == true {
                    format!("engine {} → {}\n", s(&c["from"]), s(&c["to"]))
                } else {
                    format!("engine already {}\n", s(&c["to"]))
                });
            }
        }
        Cmd::StartupStatus => {
            o.push_str(&if d["enabled"] == true {
                format!(
                    "starts at sign-in: {}{}\n",
                    s(&d["command"]),
                    if d["foreign"] == true {
                        " (not this installation)"
                    } else {
                        ""
                    }
                )
            } else {
                format!("does not start at sign-in ({})\n", s(&d["location"]))
            });
        }
        Cmd::StartupEnable | Cmd::StartupDisable => {
            let prefix = if d["dry_run"] == true {
                "dry run: would "
            } else {
                ""
            };
            o.push_str(&match (cmd == Cmd::StartupEnable, d["changed"] == true) {
                (_, false) => "nothing to change\n".to_string(),
                (true, true) => format!("{prefix}start at sign-in: {}\n", s(&d["after"])),
                (false, true) => format!("{prefix}no longer start at sign-in\n"),
            });
        }
        Cmd::Help | Cmd::Version => {}
    }
    o
}

/// The schema validator of `cargo xtask schema-check` (test-only; a top-level module so the
/// relative path resolves on every OS).
#[cfg(test)]
#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

#[cfg(test)]
mod tests {
    use crate::jsonschema;

    #[test]
    fn capabilities_match_the_published_example_and_the_schema() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let example = std::fs::read_to_string(
            root.join("docs/contracts/examples/tool-capabilities-v1/ddalgak.json"),
        )
        .unwrap();
        let mine: serde_json::Value = serde_json::from_str(super::CAPABILITIES).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&example).unwrap(),
            mine
        );
        assert_eq!(mine["version"], ddalgak::VERSION);
        let schema: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join("docs/contracts/schemas/tool-capabilities-v1.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let errors = jsonschema::Validator::new(&schema).validate(&mine).unwrap();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn the_cli_never_reaches_the_hook_tray_or_host() {
        // DDALGAK-SEC-010 by inspection: the CLI's sources do not name the hook, the tray, the
        // host loop or the injector. Only `host::rules_json` (a pure formatter) is used.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for f in ["main.rs", "cli.rs", "output.rs"] {
            let text = std::fs::read_to_string(dir.join(f)).unwrap();
            let shipped = text.split("#[cfg(test)]").next().unwrap_or_default();
            let code: String = shipped
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for forbidden in [
                "HookThread",
                "win::hook",
                "SetWindowsHookEx",
                "Tray",
                "host::run",
                "SendInput",
                "Injector",
                "CreateWindow",
                "Shell_NotifyIcon",
                "Watcher",
            ] {
                assert!(!code.contains(forbidden), "{f} mentions {forbidden}");
            }
        }
    }
}
