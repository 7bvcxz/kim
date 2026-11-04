//! `subak-gui`: the Subak window (Steps 16–17, ADR 0039, ADR 0042, docs/SUBAK_GUI.md), and with
//! `--float` the floating icon, tray icon and Terminal manager (Step 18, ADR 0045, [`float`]).
//!
//! A pure client of the per-user Host (ADR 0009): it connects like the CLI (identity checks,
//! IPC v1, starting a Host on demand — one that starts no shell), shows the Host's Terminals as
//! tabs (Step 17), attaches interactively to the active tab's pane and renders it with the
//! `subak-term` view and CPU renderer in a plain Win32 window (ADR 0036: no GPU, no GUI
//! framework). Closing the window, a tab's `×`, or this process ending in any way, is a detach:
//! the Host and its Terminals keep running (`SUBAK-FR-003`, `SUBAK-FR-015`). Only the explicit
//! `Ctrl+T` creates and `Ctrl+W` ends a Terminal (confirmed when it is busy, `SUBAK-FR-014`).
//!
//! | Module | Role | Platform |
//! | --- | --- | --- |
//! | [`model`] | the Host conversation as a state machine: what to attach, phases, reconnects | portable, unit-tested |
//! | [`screens`] | loading, empty, stored and error screens; banners; the busy confirmation | portable, unit-tested |
//! | [`tabs`] | the tab bar: layout, scrolling, clickable spans, the rename field | portable, unit-tested |
//! | [`layout`] | DPI scale, grid ↔ client area, saved placements on monitors | portable, unit-tested |
//! | [`keys`] | the window's shortcuts (Subak's hotkey manifest + `hotkeys.json`) and key mapping | portable, unit-tested |
//! | `find` | the search bar: query, options, matches, the current one (Step 19) | portable, unit-tested |
//! | `ask` | questions before a paste, a link or an export (Step 19) | portable, unit-tested |
//! | `clip` | the clipboard, link opener and save dialog as traits, and their recording fakes (Step 19) | portable, unit-tested |
//! | `export` | "Save output…": the pane's output, the secret detector, the file (Step 19) | portable, unit-tested |
//! | [`prefs`] | `%APPDATA%\mouv\subak\gui.json` | portable, unit-tested |
//! | `net` | the connection: connect or start, read frames, write frames | Windows |
//! | `window` | the Win32 window: paint, keyboard, IME, mouse, clipboard, DPI, test hooks | Windows |
//! | [`float`] | `--float`: the floating icon, the tray icon and the Terminal manager (Step 18) | portable parts + Windows |

// Portable modules: compiled and unit-tested on every target; until the Linux window (M4,
// ADR 0011) only the Windows window uses them.
#[cfg_attr(not(windows), allow(dead_code))]
mod ask;
#[cfg_attr(not(windows), allow(dead_code))]
mod clip;
#[cfg_attr(not(windows), allow(dead_code))]
mod compose;
#[cfg_attr(not(windows), allow(dead_code))]
mod export;
#[cfg_attr(not(windows), allow(dead_code))]
mod find;
pub mod float;
#[cfg_attr(not(windows), allow(dead_code))]
mod keys;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod model;
#[cfg_attr(not(windows), allow(dead_code))]
mod motion;
#[cfg_attr(not(windows), allow(dead_code))]
mod prefs;
#[cfg_attr(not(windows), allow(dead_code))]
mod screens;
#[cfg_attr(not(windows), allow(dead_code))]
mod tabs;

#[cfg(windows)]
mod net;
#[cfg(windows)]
#[allow(unsafe_code)]
mod window;

use std::process::ExitCode;

const HELP: &str = "\
subak-gui — the Subak window (pre-alpha, Steps 16-19)

USAGE:
    subak-gui [<terminal>] [--endpoint <name>] [--no-hotkeys]
    subak-gui --float [--icon] [--tray] [--manager] [--endpoint <name>] [--no-hotkeys]
    subak-gui --manager [--endpoint <name>]
    subak-gui --help | --version

Shows the Host's Terminals as tabs in a native window. <terminal> is a Terminal name (case is
ignored) or id to open first; without it the window reopens the Terminal it showed last, or the
first running one. A Host is started if none runs (it starts no shell); opening the window never
creates or ends a Terminal. Closing the window, a tab's x, or this program ending in any way,
only detaches: every Terminal keeps running in the Host (`subak ls`).

TABS (docs/SUBAK_GUI.md §2):
    Ctrl+T          New Terminal (the Host's default shell, in your profile folder).
    Ctrl+W          Close Terminal: ends it. When it runs more than an idle shell the window
                    asks first (the safe answer, Cancel, is the default). On a stopped Terminal
                    it removes the record; on one that already ended it closes the tab.
    Ctrl+Tab / Ctrl+Shift+Tab   Next / previous tab.
    Ctrl+Shift+R    Rename the Terminal (Enter saves, Esc cancels).
    x on a tab, middle click   Hide the tab: a detach; the Terminal keeps running.
    Right click, the Menu key   The tab menu: rename, hide, move, close, new, hidden tabs.
    Tabs follow other clients: `subak new`, `rename`, `stop` and `kill` show up at once.

Other keys go to the program, except: Ctrl+Shift+C / Ctrl+Insert copy, Ctrl+Shift+V /
Shift+Insert paste, Ctrl+Plus / Ctrl+Minus / Ctrl+0 text size, Ctrl+Shift+PageUp/PageDown/Up/
Down/Home/End scroll. Every shortcut can be changed or turned off with `subak hotkeys`
(%APPDATA%\\mouv\\subak\\hotkeys.json, re-read when the window gets focus); Ctrl+W and Ctrl+T
reach the program only once you disable or rebind them (the Ctrl+B pass-through is Step 23).
The mouse selects (double-click: word, triple-click: line, Alt: block); right-click copies a
selection or pastes. Preferences: %APPDATA%\\mouv\\subak\\gui.json (docs/SUBAK_GUI.md).

SEARCH, SELECTION, CLIPBOARD AND LINKS (docs/SUBAK_GUI.md §12):
    Ctrl+Shift+F    Find in the output (the history the Host keeps and the screen): type to
                    search; Enter/F3 older, Shift+Enter/Shift+F3 newer; Alt+C case, Alt+R
                    regular expression, Alt+W whole word; Esc closes (the match stays selected).
    Ctrl+Shift+M    Select with the keyboard: arrows, Shift grows, Ctrl by words, Home/End,
                    PageUp/PageDown; Enter copies, Esc leaves.
    Ctrl+Shift+A    Select all.
    Paste           Plain text only. Asks first when lines would run at once, the text hides
                    control characters (removed) or is large; Cancel is the default; As one
                    line joins the lines.
    Ctrl+click      Open a link: http/https only, through Windows' open handler; a hyperlink
                    shows its target first.
    Menu key, Ctrl+right-click   The pane menu: copy with colours (HTML), paste as one line,
                    find, select, save output (asks first, hides secret-looking values).

FLOATING ICON AND TERMINAL MANAGER (optional, off unless you start it; docs/SUBAK_GUI.md §11):
    --float         Run the small always-on-top floating icon (and/or a tray icon). Click it for
                    the Terminal manager: every Terminal of the Host with its state, quick
                    search, Open (Enter), New (Ctrl+N), Rename (F2), Delete (Delete, asks
                    first). Drag it anywhere; its place is kept. Right-click for its menu.
                    It never starts a Host by itself and closing it ends nothing.
    --icon, --tray  With --float: show the floating icon, the tray icon, or both (saved in
                    %APPDATA%\\mouv\\subak\\float.json); without either, the saved choice.
    --manager       Open the Terminal manager (starting the floating UI if it does not run).

OPTIONS:
    --endpoint <name>   Use this Host endpoint instead of your default one (as `subak --endpoint`).
    --no-hotkeys        Start with every shortcut off (also KIM_NO_HOTKEYS); keys go to programs.
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
    /// `--no-hotkeys`: every shortcut off for this run (HOTKEY_CONTRACT §8.3 4).
    pub no_hotkeys: bool,
    /// `--float` / `--manager`: the floating icon and Terminal manager instead of a window.
    pub float: Option<float::FloatOptions>,
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
            "--no-hotkeys" => o.no_hotkeys = true,
            "--float" => {
                o.float.get_or_insert_with(Default::default);
            }
            "--icon" | "--tray" | "--manager" => {
                let f = o.float.get_or_insert_with(Default::default);
                match a {
                    "--icon" => f.icon = true,
                    "--tray" => f.tray = true,
                    _ => f.manager = true,
                }
            }
            "--test-monitors" if TEST_HOOKS => {
                i += 1;
                let m = args
                    .get(i)
                    .and_then(|v| float::parse_monitors(v))
                    .ok_or("--test-monitors needs left,top,right,bottom,dpi[;…]")?;
                o.float.get_or_insert_with(Default::default).test_monitors = Some(m);
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
    if let Some(f) = &o.float {
        if let Some(t) = &o.target {
            return Err(format!(
                "`{t}`: the floating icon shows every Terminal; open one from its manager"
            ));
        }
        if (f.icon || f.tray || f.test_monitors.is_some())
            && !f.manager
            && !args.iter().any(|a| a == "--float")
        {
            return Err("--icon, --tray and --test-monitors go with --float".into());
        }
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
    if o.float.is_some() {
        return float::win::run(o);
    }
    window::run(o)
}

#[cfg(not(windows))]
fn run(o: Options) -> ExitCode {
    // ADR 0011: no Linux window before M4; the portable modules compile and are tested.
    drop(o);
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
        assert!(!o.test && !o.no_hotkeys);
        let Ok(Parsed::Run(o)) = p(&["--no-hotkeys"]) else {
            panic!()
        };
        assert!(o.no_hotkeys);
        assert_eq!(p(&["--help"]), Ok(Parsed::Help));
        assert_eq!(p(&["-V"]), Ok(Parsed::Version));
        assert!(p(&["a", "b"]).unwrap_err().contains("one Terminal"));
        assert!(p(&["--endpoint"]).is_err());
        assert!(p(&["--bogus"]).unwrap_err().contains("unknown option"));
        // The test hooks exist in debug builds (this test's build) only.
        assert_eq!(p(&["--test-offscreen"]).is_ok(), TEST_HOOKS);
        assert!(HELP.contains("only detaches"));
        assert!(HELP.contains("Ctrl+Tab") && HELP.contains("asks first"));
        assert!(HELP.contains("--float") && HELP.contains("closing it ends nothing"));
    }

    #[test]
    fn floating_options() {
        let Ok(Parsed::Run(o)) = p(&["--float", "--endpoint", "e"]) else {
            panic!()
        };
        assert_eq!(o.float, Some(float::FloatOptions::default()));
        let Ok(Parsed::Run(o)) = p(&["--float", "--tray", "--icon"]) else {
            panic!()
        };
        let f = o.float.unwrap();
        assert!(f.icon && f.tray && !f.manager);
        let Ok(Parsed::Run(o)) = p(&["--manager"]) else {
            panic!()
        };
        assert!(o.float.unwrap().manager);
        assert!(p(&["--tray"]).unwrap_err().contains("go with --float"));
        assert!(
            p(&["--float", "work"])
                .unwrap_err()
                .contains("open one from its manager")
        );
        assert_eq!(
            p(&[
                "--float",
                "--test-monitors",
                "-20000,-20000,-18000,-19000,96"
            ])
            .is_ok(),
            TEST_HOOKS
        );
        // A window (no --float) is not floating.
        let Ok(Parsed::Run(o)) = p(&["work"]) else {
            panic!()
        };
        assert_eq!(o.float, None);
    }
}
