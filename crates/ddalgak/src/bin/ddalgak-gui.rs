//! `ddalgak-gui` — the Ddalgak tray app that owns the engine (ADR 0022 §3, ADR 0024 §4).
//!
//! Started by the user (or by the opt-in sign-in entry with `--startup`). It shows a tray icon,
//! keeps the engine `off` until the user turns it on (unless `engine_on_start: on` and the last
//! run ended cleanly), registers Ddalgak's own global shortcuts and serves the local pipe that
//! `ddalgak` talks to. One instance per user session. The rule editor arrives in Step 74; until
//! then the app has no window, only its tray icon.
//!
//! Options: `--startup` (started at sign-in), `--no-hotkeys` (or `KIM_NO_HOTKEYS`: every shortcut
//! off and the engine off), `--help`, `--version`. Debug builds only (compiled out of release
//! builds, so a shipped app always has its tray icon): `--endpoint <name>`, `--test-host`
//! (no tray icon, the hook never consumes a key, the app ends by itself after
//! `--test-max-seconds`, default 60) for automated tests.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

const HELP: &str = "\
ddalgak-gui — Ddalgak tray app (owns the automation engine)

USAGE:
    ddalgak-gui [--startup] [--no-hotkeys]
    ddalgak-gui --help | --version

Shows a tray icon. The engine stays off until you turn it on (tray menu, `ddalgak engine
start`, or your engine.toggle shortcut). Ctrl+Alt+Shift+Escape always turns it off.
One instance per user session; a second start exits with app_already_running (exit 7).
";

#[cfg(windows)]
fn main() -> ExitCode {
    use ddalgak::inject;
    use ddalgak::paths::Paths;
    use ddalgak::win::{host, inject::SendInputBackend, instance, pipe};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut startup = false;
    let mut no_hotkeys = kim_hotkeys::no_hotkeys_env();
    let mut endpoint: Option<String> = None;
    let mut test_host = false;
    let mut max_secs: u64 = 60;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--startup" => startup = true,
            "--no-hotkeys" => no_hotkeys = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("ddalgak-gui {}", ddalgak::VERSION);
                return ExitCode::SUCCESS;
            }
            #[cfg(debug_assertions)]
            "--endpoint" => {
                i += 1;
                endpoint = args.get(i).cloned();
            }
            #[cfg(debug_assertions)]
            "--test-host" => test_host = true,
            #[cfg(debug_assertions)]
            "--test-max-seconds" => {
                i += 1;
                max_secs = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60)
                    .clamp(1, 600);
            }
            other => {
                eprintln!("ddalgak-gui: error: unknown option `{other}` (usage)");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    // A panic anywhere releases every key Ddalgak still holds before the process ends; Windows
    // removes the hook when the process exits.
    inject::set_panic_backend(Box::new(SendInputBackend));
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        inject::release_all_global();
        default_hook(info);
    }));
    let paths = match Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ddalgak-gui: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let custom = endpoint.is_some();
    let endpoint = match endpoint.map_or_else(pipe::default_endpoint, Ok) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ddalgak-gui: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let instance = match instance::name(custom.then_some(endpoint.as_str())) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("ddalgak-gui: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let opts = host::Options {
        paths,
        endpoint,
        instance,
        tray: !test_host,
        observe_only: test_host,
        no_hotkeys,
        startup,
        max_lifetime: test_host.then(|| std::time::Duration::from_secs(max_secs)),
    };
    match host::run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(host::RunError::AlreadyRunning) => {
            eprintln!(
                "ddalgak-gui: error: Ddalgak already runs in this session (app_already_running)"
            );
            ExitCode::from(7)
        }
        Err(host::RunError::Io(e)) => {
            eprintln!("ddalgak-gui: error: {e} (io)");
            ExitCode::from(1)
        }
    }
}

#[cfg(not(windows))]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    // ADR 0011: no Linux engine before milestone D7 (DDALGAK-NFR-009).
    eprintln!(
        "ddalgak-gui: error: the Ddalgak engine needs Windows for now (unsupported_platform)"
    );
    ExitCode::from(5)
}
