//! Starting a pane's program on Windows (docs/PTY_PROCESSES.md, ADR 0025): the Windows side of
//! [`crate::pty::spawn`].
//!
//! In this order, before any OS object exists:
//! 1. the **working directory** must be an absolute, existing directory this user may enter
//!    (`bad_cwd` otherwise);
//! 2. the **environment** is the user's logon environment, built fresh for every pane
//!    (`CreateEnvironmentBlock`, so a variable the user set with `setx` or System Properties is
//!    seen by the next new pane, as in a new Explorer window), then the overrides, then the Host's
//!    `SUBAK_PANE_ID` / `SUBAK_TERMINAL_ID`;
//! 3. the **program** is resolved by the Host itself: a path is taken as given (relative to the
//!    pane's working directory), a bare name is looked up on the `PATH` *of that environment*
//!    with `PATHEXT`. `CreateProcessW`'s own search would look in the Host's executable folder
//!    and the Host's working directory first, and on the Host's `PATH`, so a file planted in the
//!    user profile could shadow `cmd.exe`, and a `PATH` override would not count;
//! 4. batch and PowerShell scripts are refused (`not_executable`): `CreateProcessW` would run a
//!    `.bat`/`.cmd` through `cmd.exe /c` with cmd's own parsing of every argument, a shell the
//!    user did not ask for. Naming `cmd.exe /d /c script.cmd` explicitly is the way to run one;
//!    a file this user may not execute (`access_denied`) or without an executable header
//!    (`not_executable`) is refused here too, before a pseudo console exists ([`check_image`]);
//! 5. the **command line** is the program as named plus every argument quoted for
//!    `CommandLineToArgvW` / the MSVC runtime ([`super::quote_arg`]), or, for `host serve
//!    --shell`, the user's line verbatim;
//!
//! then [`ConPty::spawn`] creates the pseudo console and the job and starts the process with the
//! resolved file as `lpApplicationName`.

use super::conpty::ConPty;
use super::{EnvBlock, quote_arg, wide};
use crate::pty::{Reason, SpawnError, Spec, Started};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_TRAVERSE, OPEN_EXISTING, SYNCHRONIZE,
};

/// Longest command line `CreateProcessW` accepts, terminator included.
pub const MAX_COMMAND_LINE: usize = 32_767;
/// `PATHEXT` when the environment has none.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// A checked start: what [`ConPty::spawn`] gets.
#[derive(Debug)]
pub struct Prepared {
    /// The resolved program file (`lpApplicationName`).
    pub app: PathBuf,
    pub line: String,
    pub env: EnvBlock,
    pub cwd: PathBuf,
}

/// Steps 1–5 of the module docs; starts nothing.
pub fn prepare(spec: &Spec) -> Result<Prepared, SpawnError> {
    check_cwd(spec.cwd)?;
    let base = EnvBlock::for_current_user().unwrap_or_else(|_| EnvBlock::from_process());
    let env = base.with_overrides(spec.env, spec.host_vars);
    let app = resolve(spec.program, spec.cwd, &env)?;
    check_image(spec.program, &app)?;
    let line = match spec.raw_line {
        Some(l) => l.to_string(),
        None => command_line(spec.program, spec.args),
    };
    if line.encode_utf16().count() >= MAX_COMMAND_LINE {
        return Err(SpawnError::new(
            Reason::Other,
            format!(
                "the command line of `{}` is longer than {} characters",
                spec.program,
                MAX_COMMAND_LINE - 1
            ),
        ));
    }
    Ok(Prepared {
        app,
        line,
        env,
        cwd: spec.cwd.to_path_buf(),
    })
}

/// Starts `spec` in a new pseudo console and kill-on-close job.
pub fn spawn(spec: &Spec) -> Result<Started, SpawnError> {
    let p = prepare(spec)?;
    let pty = ConPty::spawn(
        Some(&p.app),
        &p.line,
        Some(&p.cwd),
        spec.size.0,
        spec.size.1,
        Some(&p.env),
    )
    .map_err(|e| os_failure(spec.program, &p.app, &e))?;
    Ok(Started {
        pty: Arc::new(pty),
        executable: p.app,
    })
}

/// `program` and `args` as one command line: every word quoted only when it needs it.
pub fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

fn bad_cwd(message: String) -> SpawnError {
    SpawnError::new(Reason::BadCwd, message)
}

/// The working directory must exist, be a directory, and be enterable: `CreateProcessW` opens
/// it for traversal, so the same open is tried here to name the problem precisely.
pub fn check_cwd(cwd: &Path) -> Result<(), SpawnError> {
    let shown = cwd.display();
    if !cwd.is_absolute() {
        return Err(bad_cwd(format!(
            "the working directory `{shown}` is not an absolute path"
        )));
    }
    match std::fs::metadata(cwd) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(bad_cwd(format!(
                "the working directory `{shown}` is not a directory"
            )));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(bad_cwd(format!(
                "the working directory `{shown}` does not exist"
            )));
        }
        Err(e) => {
            return Err(bad_cwd(format!(
                "the working directory `{shown}` cannot be read: {e}"
            ))
            .os(e.raw_os_error()));
        }
    }
    let w = wide(cwd);
    // SAFETY: NUL-terminated path; a directory handle needs FILE_FLAG_BACKUP_SEMANTICS; checked
    // and closed by Handle.
    let h = super::Handle::new(unsafe {
        CreateFileW(
            w.as_ptr(),
            FILE_TRAVERSE | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    });
    match h {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(5) => Err(bad_cwd(format!(
            "access to the working directory `{shown}` is denied"
        ))
        .os(Some(5))),
        Err(e) => Err(bad_cwd(format!(
            "the working directory `{shown}` cannot be entered: {e}"
        ))
        .os(e.raw_os_error())),
    }
}

fn is_file(p: &Path) -> bool {
    // symlink_metadata: an App Execution Alias (`WindowsApps\pwsh.exe`) is a reparse point that
    // `metadata` cannot follow, yet CreateProcessW starts it.
    std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir())
}

/// The lowercase extensions of `PATHEXT` (`.com`, `.exe`, …).
fn pathext(env: &EnvBlock) -> Vec<String> {
    env.get("PATHEXT")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PATHEXT.into())
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.len() > 1 && e.starts_with('.'))
        .collect()
}

fn extension(p: &Path) -> Option<String> {
    p.extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_ascii_lowercase()))
}

/// The files `base` may mean: itself when its extension is in `PATHEXT`; otherwise `base`
/// with each `PATHEXT` extension appended (`python3.11` → `python3.11.exe`), and finally `base`
/// itself when `exact` (an explicit path, where Windows decides what the file is).
fn candidates(base: &Path, exts: &[String], exact: bool) -> Vec<PathBuf> {
    if extension(base).is_some_and(|e| exts.contains(&e)) {
        return vec![base.to_path_buf()];
    }
    let mut v: Vec<PathBuf> = exts
        .iter()
        .map(|e| {
            let mut s = base.as_os_str().to_os_string();
            s.push(e);
            PathBuf::from(s)
        })
        .collect();
    if exact {
        v.push(base.to_path_buf());
    }
    v
}

/// Whether `program` names a path rather than a bare name to look up.
fn is_path(program: &str) -> bool {
    program.contains(['\\', '/']) || program.as_bytes().get(1) == Some(&b':')
}

/// Resolves `program` as described in the module docs; `not_found` or `not_executable` when it
/// cannot be a pane's program.
pub fn resolve(program: &str, cwd: &Path, env: &EnvBlock) -> Result<PathBuf, SpawnError> {
    let exts = pathext(env);
    let found = if is_path(program) {
        let p = Path::new(program);
        let base = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        candidates(&base, &exts, true)
            .into_iter()
            .find(|c| is_file(c))
    } else {
        let path = env.get("PATH").unwrap_or_default();
        path.split(';')
            .map(|d| d.trim().trim_matches('"'))
            // Relative entries (`.`, `bin`) would depend on the Host's working directory.
            .filter(|d| !d.is_empty() && Path::new(d).is_absolute())
            .flat_map(|d| candidates(&Path::new(d).join(program), &exts, false))
            .find(|c| is_file(c))
    };
    let Some(found) = found else {
        return Err(not_found(program));
    };
    match extension(&found).as_deref() {
        Some(".bat" | ".cmd") => Err(SpawnError::new(
            Reason::NotExecutable,
            format!(
                "`{program}` is the batch script `{}`, which only runs through cmd.exe",
                found.display()
            ),
        )
        .hint(format!(
            "Subak never adds a shell you did not ask for; run it explicitly: -- cmd.exe /d /c {} [args...]",
            quote_arg(&found.to_string_lossy())
        ))),
        Some(".ps1") => Err(SpawnError::new(
            Reason::NotExecutable,
            format!(
                "`{program}` is the PowerShell script `{}`, which only runs through PowerShell",
                found.display()
            ),
        )
        .hint(format!(
            "run it explicitly: -- powershell.exe -NoLogo -File {} [args...]",
            quote_arg(&found.to_string_lossy())
        ))),
        _ => Ok(found),
    }
}

/// Refuses a program file this user may not execute, or one without an executable header, before
/// any pseudo console exists. `CreateProcessW` would report the same, but only after the pseudo
/// console was created, and Windows 10's inbox ConPTY keeps one handle per pseudo console even
/// when no process ever ran in it (measured; docs/PTY_PROCESSES.md §9). Anything this check cannot
/// decide (an App Execution Alias, a file it may execute but not read) is left to
/// `CreateProcessW`.
pub fn check_image(program: &str, app: &Path) -> Result<(), SpawnError> {
    use std::io::Read;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_EXECUTE};
    let Ok(meta) = std::fs::symlink_metadata(app) else {
        return Ok(());
    };
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(()); // App Execution Aliases are resolved by CreateProcessW alone
    }
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    if let Err(e) = std::fs::OpenOptions::new()
        .access_mode(FILE_EXECUTE | SYNCHRONIZE)
        .share_mode(share)
        .open(app)
        && e.raw_os_error() == Some(5)
    {
        return Err(SpawnError::new(
            Reason::AccessDenied,
            format!("cannot start `{program}` ({}): {e}", app.display()),
        )
        .os(Some(5)));
    }
    let mut head = [0u8; 2];
    let readable = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(share)
        .open(app)
        .and_then(|mut f| {
            let mut n = 0;
            while n < head.len() {
                match f.read(&mut head[n..])? {
                    0 => break,
                    m => n += m,
                }
            }
            Ok(n)
        });
    match readable {
        Ok(2) if &head == b"MZ" => Ok(()),
        Ok(_) => Err(SpawnError::new(
            Reason::NotExecutable,
            format!(
                "`{program}` ({}) is not a program Windows can start (it has no executable header)",
                app.display()
            ),
        )
        .hint("run a script or document through its interpreter, e.g. -- cmd.exe /d /c <file>")),
        // Unreadable but executable, or vanished: CreateProcessW decides.
        Err(_) => Ok(()),
    }
}

fn not_found(program: &str) -> SpawnError {
    let e = if is_path(program) {
        SpawnError::new(Reason::NotFound, format!("`{program}` was not found"))
            .hint("check the path; a relative path is relative to the Terminal's working directory")
    } else {
        SpawnError::new(
            Reason::NotFound,
            format!("`{program}` was not found on the PATH of the new pane"),
        )
        .hint("the pane's PATH is your logon PATH (plus --env overrides); give the program's full path, or set PATH with --env")
    };
    let base = program.to_ascii_lowercase();
    if matches!(base.as_str(), "pwsh" | "pwsh.exe") {
        return e.hint(
            "PowerShell 7 (`pwsh`) is not installed or not on your PATH. Subak never installs it; \
             Windows PowerShell 5.1 is `powershell`",
        );
    }
    e
}

/// The `spawn_failed` of a `CreateProcessW` (or pseudo console) failure.
pub fn os_failure(program: &str, app: &Path, e: &io::Error) -> SpawnError {
    let code = e.raw_os_error();
    let (reason, hint) = match code {
        // The file vanished or its name is unusable.
        Some(2 | 3 | 123 | 161) => (Reason::NotFound, None),
        Some(740) => (
            Reason::AccessDenied,
            Some("the program requires elevation (run as administrator); Subak never elevates"),
        ),
        // Access denied; blocked by software restriction or application control policy; blocked
        // by code integrity; blocked by antivirus.
        Some(5 | 1260 | 4551 | 225 | 226) => (Reason::AccessDenied, None),
        Some(267) => (Reason::BadCwd, None),
        // Bad format, not a valid Win32 application, invalid or 16-bit executable, or one for
        // another machine type (Windows 10 also reports a text file named `.exe` that way).
        Some(11 | 129 | 191 | 193 | 216) => (
            Reason::NotExecutable,
            Some("it is not a program Windows can start; run a script through its interpreter"),
        ),
        _ => (Reason::Other, None),
    };
    let mut err = SpawnError::new(
        reason,
        format!("cannot start `{program}` ({}): {e}", app.display()),
    )
    .os(code);
    if let Some(h) = hint {
        err = err.hint(h);
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::EnvOverrides;

    fn env_with(vars: &[(&str, Option<&str>)]) -> EnvBlock {
        let o = EnvOverrides {
            vars: vars
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.map(String::from)))
                .collect(),
        };
        EnvBlock::for_current_user()
            .unwrap()
            .with_overrides(&o, &[])
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("subak-launch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn bare_names_resolve_on_the_panes_path_only() {
        let sys = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        let env = env_with(&[]);
        let cwd = tmp("bare");
        let cmd = resolve("cmd", &cwd, &env).unwrap();
        assert!(
            cmd.to_string_lossy()
                .to_lowercase()
                .ends_with(r"\system32\cmd.exe"),
            "{}",
            cmd.display()
        );
        assert_eq!(
            resolve("CMD.EXE", &cwd, &env)
                .unwrap()
                .parent()
                .map(|p| p.to_string_lossy().to_lowercase()),
            Some(sys.to_string_lossy().to_lowercase()),
            "PATH entries compare as Windows does (case-insensitively)"
        );
        // A program in the working directory is never found by its bare name (no cwd search),
        // so a planted `cmd.exe` there cannot shadow the real one.
        std::fs::copy(sys.join("whoami.exe"), cwd.join("planted-subak.exe")).unwrap();
        std::fs::copy(sys.join("whoami.exe"), cwd.join("cmd.exe")).unwrap();
        assert_eq!(
            resolve("planted-subak", &cwd, &env).unwrap_err().reason,
            Reason::NotFound
        );
        assert_eq!(resolve("cmd", &cwd, &env).unwrap(), cmd);
        // …but with a relative or absolute path it is.
        assert_eq!(
            resolve(r".\planted-subak", &cwd, &env).unwrap(),
            cwd.join(r".\planted-subak.exe")
        );
        // A PATH override decides the lookup: the pane's PATH, not the Host's.
        let env2 = env_with(&[("PATH", Some(&cwd.to_string_lossy()))]);
        assert_eq!(
            resolve("planted-subak", &cwd, &env2).unwrap(),
            cwd.join("planted-subak.exe")
        );
        let env3 = env_with(&[("PATH", None)]);
        let e = resolve("cmd", &cwd, &env3).unwrap_err();
        assert_eq!(e.reason, Reason::NotFound);
        assert!(e.message.contains("PATH"), "{}", e.message);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn paths_extensions_and_scripts() {
        let env = env_with(&[]);
        let cwd = tmp("ext");
        let sys = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        std::fs::copy(sys.join("whoami.exe"), cwd.join("tool.v2.exe")).unwrap();
        std::fs::write(cwd.join("build.cmd"), "@echo off\r\n").unwrap();
        std::fs::write(cwd.join("setup.ps1"), "Write-Output 1\r\n").unwrap();
        std::fs::write(cwd.join("notes.txt"), "text").unwrap();
        // `tool.v2` has an extension that is not in PATHEXT: `.exe` is appended.
        assert_eq!(
            resolve(&cwd.join("tool.v2").to_string_lossy(), &cwd, &env).unwrap(),
            cwd.join("tool.v2.exe")
        );
        for (name, hint) in [("build", "cmd.exe /d /c"), ("build.cmd", "cmd.exe /d /c")] {
            let e = resolve(&format!(r".\{name}"), &cwd, &env).unwrap_err();
            assert_eq!(e.reason, Reason::NotExecutable, "{name}");
            assert!(e.hint.as_deref().unwrap().contains(hint), "{e:?}");
        }
        let e = resolve(r".\setup.ps1", &cwd, &env).unwrap_err();
        assert_eq!(e.reason, Reason::NotExecutable);
        assert!(e.hint.unwrap().contains("powershell.exe"));
        // A document is found as given; CreateProcessW decides (not_executable at start).
        assert_eq!(
            resolve(r".\notes.txt", &cwd, &env).unwrap(),
            cwd.join(r".\notes.txt")
        );
        assert_eq!(
            resolve(r"C:\subak-no-such-dir\x.exe", &cwd, &env)
                .unwrap_err()
                .reason,
            Reason::NotFound
        );
        let e = resolve("pwsh-subak-missing", &cwd, &env).unwrap_err();
        assert!(e.hint.unwrap().contains("--env"), "generic hint");
        // pwsh gets the "never installs it" hint only when it is really missing.
        if let Err(e) = resolve("pwsh", &cwd, &env) {
            assert!(e.hint.unwrap().contains("never installs"), "pwsh hint");
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn working_directories_are_checked_precisely() {
        let cwd = tmp("cwd");
        assert!(check_cwd(&cwd).is_ok());
        let file = cwd.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        for (p, why) in [
            (file, "not a directory"),
            (cwd.join("missing"), "does not exist"),
            (PathBuf::from(r"relative\dir"), "not an absolute path"),
        ] {
            let e = check_cwd(&p).unwrap_err();
            assert_eq!(e.reason, Reason::BadCwd);
            assert!(e.message.contains(why), "{}", e.message);
        }
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn command_lines_quote_every_word_once() {
        assert_eq!(
            command_line(
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                &["-NoLogo".into(), "a b".into(), String::new()]
            ),
            r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo "a b" """#
        );
        assert_eq!(command_line("cmd.exe", &[]), "cmd.exe");
    }

    #[test]
    fn unusable_program_files_are_refused_before_any_os_object_exists() {
        use std::os::windows::process::CommandExt;
        let d = tmp("image");
        let sys = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        std::fs::copy(sys.join("whoami.exe"), d.join("denied.exe")).unwrap();
        std::fs::copy(sys.join("whoami.exe"), d.join("fine.exe")).unwrap();
        std::fs::write(d.join("fake.exe"), "not a program").unwrap();
        std::fs::write(d.join("tiny.exe"), "M").unwrap();
        let icacls = |args: &[&str]| {
            let st = std::process::Command::new("icacls")
                .arg(d.join("denied.exe"))
                .args(args)
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(st.success());
        };
        icacls(&["/deny", "*S-1-1-0:(X)"]);
        let e = check_image("denied", &d.join("denied.exe")).unwrap_err();
        assert_eq!(e.reason, Reason::AccessDenied, "{e:?}");
        assert_eq!(e.os_error, Some(5));
        for f in ["fake.exe", "tiny.exe"] {
            let e = check_image("fake", &d.join(f)).unwrap_err();
            assert_eq!(e.reason, Reason::NotExecutable, "{f}: {e:?}");
            assert!(
                e.message.contains("not a program Windows can start"),
                "{e:?}"
            );
        }
        assert!(check_image("fine", &d.join("fine.exe")).is_ok());
        assert!(check_image("cmd", &sys.join("cmd.exe")).is_ok());
        icacls(&["/remove:d", "*S-1-1-0"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preparing_a_start_holds_no_handle() {
        // Building the logon environment, checking the directory and resolving the program
        // open and close handles; 100 preparations leave none behind.
        let _serial = super::super::handle_test_lock();
        let count = || {
            (0..5)
                .map(|_| {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    super::super::handle_count()
                })
                .min()
                .unwrap()
        };
        let cwd = std::env::temp_dir();
        let env = EnvOverrides::default();
        let spec = Spec {
            program: "cmd.exe",
            args: &[],
            raw_line: None,
            cwd: &cwd,
            env: &env,
            host_vars: &[],
            size: (80, 25),
        };
        prepare(&spec).unwrap();
        let before = count();
        for _ in 0..100 {
            prepare(&spec).unwrap();
            assert!(check_cwd(&cwd.join("subak-missing")).is_err());
        }
        let after = count();
        // Other tests of this binary open handles concurrently; the minimum filters most of
        // them, and a leak of one handle per preparation would add 100.
        assert!(after <= before + 24, "handles {before} -> {after}");
    }

    #[test]
    fn os_errors_map_to_stable_reasons() {
        let app = Path::new(r"C:\x\app.exe");
        for (code, reason) in [
            (2, Reason::NotFound),
            (5, Reason::AccessDenied),
            (740, Reason::AccessDenied),
            (1260, Reason::AccessDenied),
            (267, Reason::BadCwd),
            (193, Reason::NotExecutable),
            (216, Reason::NotExecutable),
            (87, Reason::Other),
        ] {
            let e = os_failure("app", app, &io::Error::from_raw_os_error(code));
            assert_eq!(e.reason, reason, "os error {code}");
            assert_eq!(e.os_error, Some(code));
        }
        assert!(
            os_failure("app", app, &io::Error::from_raw_os_error(740))
                .hint
                .unwrap()
                .contains("never elevates")
        );
    }
}
