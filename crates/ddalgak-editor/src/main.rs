//! `ddalgak-editor` — Ddalgak's rule editor window (Step 74, ADR 0049).
//!
//! Started on demand: by the tray app (*Open rule editor*, the `editor.open` shortcut) or by the
//! user. One editor per user session: a second start brings the open window to the front and
//! exits. Closing the window ends the process. The editor never starts the tray app or turns the
//! engine on by itself; it edits `rules.json` through the rule store and asks a running app to
//! load what it saved.
//!
//! Options: `--lang en|ko`, `--no-hotkeys` (or `KIM_NO_HOTKEYS`: the rule list's shortcuts are
//! off), `--help`, `--version`. Debug builds only: `--endpoint <name>` (talk to a test host).

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

const HELP: &str = "\
ddalgak-editor — Ddalgak rule editor

USAGE:
    ddalgak-editor [--lang en|ko] [--no-hotkeys]
    ddalgak-editor --help | --version

Opens the rule editor window. It edits rules.json (the same store as `ddalgak rule …`) and asks
a running Ddalgak (ddalgak-gui.exe) to load what you saved; it never starts Ddalgak and never
turns the engine on by itself. One editor per user session.
";

#[cfg(windows)]
#[allow(unsafe_code)]
mod single {
    //! One editor per user session: a named mutex marks the owner, a named event asks it to show
    //! its window. The second process lets the first take the foreground, signals and exits.

    use std::io;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateMutexW, INFINITE, SetEvent, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The owner's handles (kept for the process lifetime).
    pub struct Owner {
        _mutex: HANDLE,
        pub event: HANDLE,
    }

    // SAFETY: kernel handles are process-wide and usable from any thread.
    unsafe impl Send for Owner {}

    pub enum Claim {
        Owner(Owner),
        /// Another editor owns the session; it was asked to show itself.
        Shown,
    }

    pub fn claim(tag: Option<&str>) -> io::Result<Claim> {
        let base = format!(
            "Local\\mouv-ddalgak-editor-{}-s{}{}",
            ddalgak::win::user_sid()?,
            ddalgak::win::session_id(),
            tag.map(|t| format!("-{t}")).unwrap_or_default()
        );
        let m = wide(&base);
        let e = wide(&format!("{base}-show"));
        // SAFETY: NUL-terminated names; default security; auto-reset event, not signaled.
        let (mutex, existed, event) = unsafe {
            let mutex = CreateMutexW(std::ptr::null(), 0, m.as_ptr());
            let existed = GetLastError() == ERROR_ALREADY_EXISTS;
            let event = CreateEventW(std::ptr::null(), 0, 0, e.as_ptr());
            (mutex, existed, event)
        };
        if mutex.is_null() || event.is_null() {
            return Err(io::Error::last_os_error());
        }
        if existed {
            // SAFETY: valid handles from above; the first editor may take the foreground this
            // process got from its starter; both handles are closed here.
            unsafe {
                AllowSetForegroundWindow(ASFW_ANY);
                SetEvent(event);
                CloseHandle(event);
                CloseHandle(mutex);
            }
            return Ok(Claim::Shown);
        }
        Ok(Claim::Owner(Owner {
            _mutex: mutex,
            event,
        }))
    }

    /// Whether the user's Windows display language is Korean (primary language 0x12).
    pub fn ui_language_is_korean() -> bool {
        // SAFETY: no arguments; returns the user's UI language ID.
        let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
        id & 0x3ff == 0x12
    }

    /// Calls `show` every time a second start asks (a thread sleeping in the wait; no polling).
    pub fn on_show(owner: &Owner, show: impl Fn() + Send + 'static) {
        let event = owner.event as usize;
        let _ = std::thread::Builder::new()
            .name("ddalgak-editor-show".into())
            .spawn(move || {
                loop {
                    // SAFETY: the event handle lives as long as the process (the owner is kept).
                    let r = unsafe { WaitForSingleObject(event as HANDLE, INFINITE) };
                    if r != WAIT_OBJECT_0 {
                        break;
                    }
                    show();
                }
            });
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut lang = None;
    let mut no_hotkeys = kim_hotkeys::no_hotkeys_env();
    let mut endpoint: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("ddalgak-editor {}", ddalgak::VERSION);
                return ExitCode::SUCCESS;
            }
            "--no-hotkeys" => no_hotkeys = true,
            "--lang" => {
                i += 1;
                match args
                    .get(i)
                    .and_then(|l| ddalgak_editor::i18n::Lang::parse(l))
                {
                    Some(l) => lang = Some(l),
                    None => {
                        eprintln!("ddalgak-editor: error: --lang takes en or ko (usage)");
                        return ExitCode::from(2);
                    }
                }
            }
            #[cfg(debug_assertions)]
            "--endpoint" => {
                i += 1;
                endpoint = args.get(i).cloned();
            }
            other => {
                eprintln!("ddalgak-editor: error: unknown option `{other}` (usage)");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    run(lang, no_hotkeys, endpoint)
}

#[cfg(windows)]
fn run(
    lang: Option<ddalgak_editor::i18n::Lang>,
    no_hotkeys: bool,
    endpoint: Option<String>,
) -> ExitCode {
    use ddalgak_editor::sync::PipeEngine;
    use ddalgak_editor::{EditorApp, Options};
    use eframe::egui;
    use std::sync::Arc;

    let owner = match single::claim(endpoint.as_deref()) {
        Ok(single::Claim::Owner(o)) => o,
        Ok(single::Claim::Shown) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ddalgak-editor: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let paths = match ddalgak::paths::Paths::from_env() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ddalgak-editor: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let endpoint = match endpoint.map_or_else(ddalgak::win::pipe::default_endpoint, Ok) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("ddalgak-editor: error: {e} (io)");
            return ExitCode::from(1);
        }
    };
    let lang = lang.unwrap_or_else(system_lang);
    // Reading the monitor layout moves nothing; the mouse form's simulation draws on it.
    let layout =
        ddalgak::win::mouse::layout().unwrap_or_else(|| ddalgak::mouse::Layout::single(1920, 1080));
    let cursor = layout.primary().rect;
    let cursor = ddalgak::mouse::Point::new(
        cursor.left + cursor.width() / 2,
        cursor.top + cursor.height() / 2,
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ddalgak rules")
            .with_app_id("mouv.ddalgak.editor")
            .with_inner_size([1060.0, 680.0])
            .with_min_inner_size([420.0, 360.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "ddalgak-editor",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            single::on_show(&owner, move || {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.request_repaint();
            });
            std::mem::forget(owner);
            let app = EditorApp::new(Options {
                paths,
                engine: Arc::new(PipeEngine { endpoint }),
                lang,
                no_hotkeys,
                watch: true,
                layout,
                cursor,
            });
            Ok(Box::new(app))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ddalgak-editor: error: the window could not open: {e} (io)");
            ExitCode::from(1)
        }
    }
}

/// Korean when Windows' display language is Korean, English otherwise (`--lang` overrides).
#[cfg(windows)]
fn system_lang() -> ddalgak_editor::i18n::Lang {
    if single::ui_language_is_korean() {
        ddalgak_editor::i18n::Lang::Ko
    } else {
        ddalgak_editor::i18n::Lang::En
    }
}

#[cfg(not(windows))]
fn run(_: Option<ddalgak_editor::i18n::Lang>, _: bool, _: Option<String>) -> ExitCode {
    // ADR 0011: no Linux Ddalgak before milestone D7 (DDALGAK-NFR-009).
    eprintln!(
        "ddalgak-editor: error: the Ddalgak editor needs Windows for now (unsupported_platform)"
    );
    ExitCode::from(5)
}
