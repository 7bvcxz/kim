//! `subak-gui`: the Subak window (Step 16, ADR 0039, docs/SUBAK_GUI.md).
//!
//! A pure client of the per-user Host (ADR 0009): it connects like the CLI (identity checks,
//! IPC v1, starting a Host on demand — one that starts no shell), attaches interactively to one
//! existing Terminal's pane and renders it with the `subak-term` view and CPU renderer in a plain
//! Win32 window (ADR 0036: no GPU, no GUI framework). Closing the window, or this process ending
//! in any way, is a detach: the Host and its Terminals keep running (`SUBAK-FR-003`,
//! `SUBAK-FR-015`).
//!
//! | Module | Role | Platform |
//! | --- | --- | --- |
//! | [`model`] | the Host conversation as a state machine: what to attach, phases, reconnects | portable, unit-tested |
//! | [`screens`] | loading, empty, chooser and error screens; banners | portable, unit-tested |
//! | [`layout`] | DPI scale, grid ↔ client area, saved placements on monitors | portable, unit-tested |
//! | [`keys`] | the window's shortcuts (hotkey-contract syntax) and key mapping | portable, unit-tested |
//! | [`prefs`] | `%APPDATA%\mouv\subak\gui.json` | portable, unit-tested |
//! | `net` | the connection: connect or start, read frames, write frames | Windows |
//! | `window` | the Win32 window: paint, keyboard, IME, mouse, clipboard, DPI, test hooks | Windows |

mod keys;
mod layout;
mod model;
mod prefs;
mod screens;

#[cfg(windows)]
mod net;
#[cfg(windows)]
#[allow(unsafe_code)]
mod window;

use std::process::ExitCode;

const HELP: &str = "\
subak-gui — the Subak window (pre-alpha, Step 16)

USAGE:
    subak-gui [<terminal>] [--endpoint <name>]
    subak-gui --help | --version

Shows one Subak Terminal in a native window. <terminal> is a Terminal name (case is ignored) or
id; without it the window reopens the Terminal it showed last, or the only running one, or lets
you choose. A Host is started if none runs (it starts no shell); the window never creates,
stops or kills a Terminal. Closing the window, or this program ending in any way, only detaches:
every Terminal keeps running in the Host (`subak ls`).

Keys go to the program, except: Ctrl+Shift+C / Ctrl+Insert copy, Ctrl+Shift+V / Shift+Insert
paste, Ctrl+Plus / Ctrl+Minus / Ctrl+0 text size, Ctrl+Shift+PageUp/PageDown/Up/Down/Home/End
scroll. The mouse selects (double-click: word, triple-click: line, Alt: block); right-click
copies a selection or pastes. Preferences: %APPDATA%\\mouv\\subak\\gui.json (docs/SUBAK_GUI.md).

OPTIONS:
    --endpoint <name>   Use this Host endpoint instead of your default one (as `subak --endpoint`).
    -h, --help          This text.
    -V, --version       The version.
";

/// What `subak-gui` was asked to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// `--endpoint`; `None` is the per-user default endpoint.
    pub endpoint: Option<String>,
    /// The Terminal to show.
    pub target: Option<String>,
    /// Test hooks (debug builds, or the `test-hooks` feature): the window is placed off every
    /// monitor, never activated, without a taskbar button, reports its state as JSON lines on
    /// stdout, takes WM_COPYDATA test commands, never touches the system clipboard, and closes
    /// itself after `test_max_seconds`.
    pub test: bool,
    pub test_max_seconds: u32,
}

/// Whether this build has the test hooks (`--test-offscreen`).
pub const TEST_HOOKS: bool = cfg!(any(debug_assertions, feature = "test-hooks"));

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Options),
    Help,
    Version,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut o = Options {
        test_max_seconds: 120,
        ..Options::default()
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--endpoint" => {
                i += 1;
                let ep = args
                    .get(i)
                    .filter(|e| !e.is_empty() && !e.starts_with('-'))
                    .ok_or("--endpoint needs a name")?;
                o.endpoint = Some(ep.clone());
            }
            "--test-offscreen" if TEST_HOOKS => o.test = true,
            "--test-max-seconds" if TEST_HOOKS => {
                i += 1;
                o.test_max_seconds = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .filter(|n| (1..=3600).contains(n))
                    .ok_or("--test-max-seconds needs 1-3600")?;
            }
            _ if a.starts_with('-') => return Err(format!("unknown option `{a}`")),
            _ if o.target.is_none() => o.target = Some(a.to_string()),
            _ => {
                return Err(format!(
                    "unexpected argument `{a}` (one Terminal at a time)"
                ));
            }
        }
        i += 1;
    }
    Ok(Parsed::Run(o))
}

/// Prints to the console `subak-gui` was started from, if any (a GUI-subsystem program has
/// none of its own; output that is redirected works as usual).
fn say(text: &str, error: bool) {
    #[cfg(windows)]
    window::attach_parent_console();
    if error {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

/// The `subak-gui` binary's `main`.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(Parsed::Help) => {
            say(HELP, false);
            ExitCode::SUCCESS
        }
        Ok(Parsed::Version) => {
            say(&format!("subak-gui {}\n", env!("CARGO_PKG_VERSION")), false);
            ExitCode::SUCCESS
        }
        Err(e) => {
            say(
                &format!("subak-gui: error: {e} (usage)\nhint: run `subak-gui --help`\n"),
                true,
            );
            ExitCode::from(2)
        }
        Ok(Parsed::Run(o)) => run(o),
    }
}

#[cfg(windows)]
fn run(o: Options) -> ExitCode {
    window::run(o)
}

#[cfg(not(windows))]
fn run(o: Options) -> ExitCode {
    // ADR 0011: no Linux window before M4; the portable modules compile and are tested.
    let _ = o;
    say(
        "subak-gui: error: the Subak window needs Windows; Linux support is planned for M4 (unsupported_platform)\n",
        true,
    );
    ExitCode::from(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Parsed, String> {
        parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn command_line() {
        let Ok(Parsed::Run(o)) = p(&["build", "--endpoint", "subak-x"]) else {
            panic!()
        };
        assert_eq!(o.target.as_deref(), Some("build"));
        assert_eq!(o.endpoint.as_deref(), Some("subak-x"));
        assert!(!o.test);
        assert_eq!(p(&["--help"]), Ok(Parsed::Help));
        assert_eq!(p(&["-V"]), Ok(Parsed::Version));
        assert!(p(&["a", "b"]).unwrap_err().contains("one Terminal"));
        assert!(p(&["--endpoint"]).is_err());
        assert!(p(&["--bogus"]).unwrap_err().contains("unknown option"));
        // The test hooks exist in debug builds (this test's build) only.
        assert_eq!(p(&["--test-offscreen"]).is_ok(), TEST_HOOKS);
        assert!(HELP.contains("only detaches"));
    }
}
