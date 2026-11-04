//! Starting programs and handing URLs to the shell on Windows (docs/DDALGAK_RULE_MODEL.md §5.5,
//! §5.6; ADR 0037; `DDALGAK-FR-013`, `DDALGAK-FR-014`, `DDALGAK-SEC-007`, `DDALGAK-SEC-008`).
//!
//! - **Programs**: `CreateProcessW` with the resolved file as the application name (nothing is
//!   searched: no `PATH`, no current folder, no splitting at a space of the path), the command line
//!   Ddalgak quoted, no inherited handles, the working folder, `STARTF_USESHOWWINDOW` with the
//!   window state, and an environment block only when the rule lists overrides. The process runs
//!   with Ddalgak's own token: the signed-in user at medium integrity. A program whose manifest asks
//!   for administrator rights fails with `ERROR_ELEVATION_REQUIRED` (*documented*:
//!   `CreateProcess` never shows a UAC prompt) and Ddalgak reports `program_requires_elevation`;
//!   it never retries with the `runas` verb or any other way to elevate. After the start,
//!   `AllowSetForegroundWindow(pid)` passes on Ddalgak's right to set the foreground window, if it
//!   has that right at that moment (the foreground lock decides; see ADR 0037).
//! - **Websites and packaged apps**: `ShellExecuteExW` with the verb `open`, on the checked
//!   `http`/`https` URL (the user's default browser through its registered association) or on
//!   `shell:AppsFolder\<AppUserModelID>`; `SEE_MASK_FLAG_NO_UI` (Ddalgak shows no error dialog;
//!   the failure becomes an outcome), `SEE_MASK_NOASYNC` (the launch thread ends right after), COM
//!   initialized as an apartment on the launch thread as the shell requires.
//! - **`if_running: activate`**: the visible, unowned top-level windows (no tool windows) whose
//!   process image is the program are found with `EnumWindows` and `QueryFullProcessImageNameW`;
//!   the first is restored if minimized and passed to `SetForegroundWindow`. Window titles are
//!   never read.
//! - **Icon preview** for the editor: `ExtractIconExW` (the file is read, never run), converted to
//!   RGBA with `GetIconInfo`/`GetDIBits`.

use super::{Handle, wide};
use crate::launch::{
    Activation, IconPreview, LaunchError, Launcher, ShellRequest, Show, SpawnRequest,
};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HBITMAP,
};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::Threading::{
    CREATE_DEFAULT_ERROR_MODE, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    PROCESS_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows_sys::Win32::UI::Shell::{
    ExtractIconExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, DestroyIcon, EnumWindows, GW_OWNER, GWL_EXSTYLE,
    GetIconInfo, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId, HICON, ICONINFO, IsIconic,
    IsWindowVisible, SW_HIDE, SW_RESTORE, SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNORMAL,
    SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
};

fn show_cmd(s: Show) -> i32 {
    match s {
        Show::Normal => SW_SHOWNORMAL,
        Show::Maximized => SW_SHOWMAXIMIZED,
        Show::Minimized => SW_SHOWMINNOACTIVE,
        Show::Hidden => SW_HIDE,
    }
}

/// The environment block for `CREATE_UNICODE_ENVIRONMENT`: `NAME=value\0` entries, sorted, and a
/// final `\0`.
fn env_block(overrides: &[(String, String)]) -> Vec<u16> {
    let merged = crate::launch::merge_env(std::env::vars_os(), overrides);
    let mut block = Vec::new();
    for (n, v) in merged {
        let mut entry = n;
        entry.push("=");
        entry.push(v);
        block.extend(wide(&entry));
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// The real platform: the app's launcher.
#[derive(Clone, Copy, Debug, Default)]
pub struct WinLauncher;

impl Launcher for WinLauncher {
    fn spawn(&self, r: &SpawnRequest) -> Result<u32, LaunchError> {
        let app = wide(&r.program);
        let mut cmd = wide(&r.command_line);
        let dir = wide(&r.working_dir);
        let env = (!r.env.is_empty()).then(|| env_block(&r.env));
        let si = STARTUPINFOW {
            cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
            dwFlags: STARTF_USESHOWWINDOW,
            wShowWindow: u16::try_from(show_cmd(r.show)).unwrap_or(1),
            ..STARTUPINFOW::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let mut flags = CREATE_DEFAULT_ERROR_MODE | CREATE_UNICODE_ENVIRONMENT;
        if r.show == Show::Hidden {
            // The debug test host's own console test program: no console window either.
            flags |= CREATE_NO_WINDOW;
        }
        // SAFETY: every string is NUL-terminated UTF-16 that outlives the call; `cmd` is a
        // writable buffer as CreateProcessW requires; the environment block (when given) is a
        // double-NUL-terminated Unicode block matching CREATE_UNICODE_ENVIRONMENT; `si` and `pi`
        // are valid for the call; no handles are inherited.
        let ok = unsafe {
            CreateProcessW(
                app.as_ptr(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                env.as_ref()
                    .map_or(std::ptr::null(), |e| e.as_ptr().cast::<c_void>()),
                dir.as_ptr(),
                &raw const si,
                &raw mut pi,
            )
        };
        if ok == 0 {
            // SAFETY: reads this thread's last error right after the failing call.
            return Err(LaunchError::from_os(unsafe { GetLastError() }));
        }
        // The handles are not needed: Ddalgak never waits for or controls the program.
        drop(Handle::new(pi.hThread));
        drop(Handle::new(pi.hProcess));
        // SAFETY: plain call with a process id; harmless when Ddalgak has no foreground right.
        unsafe { AllowSetForegroundWindow(pi.dwProcessId) };
        Ok(pi.dwProcessId)
    }

    fn shell_open(&self, r: &ShellRequest) -> Result<(), LaunchError> {
        // Only what `crate::launch` produced reaches here: the verb `open` on a checked URL or an
        // AppsFolder item. Defense in depth: nothing else is handed to the shell.
        let target_ok = r.verb == "open"
            && (crate::launch::check_url(&r.target).is_ok()
                || r.target
                    .strip_prefix("shell:AppsFolder\\")
                    .is_some_and(crate::launch::app_id_ok));
        if !target_ok || r.show == Show::Hidden {
            return Err(LaunchError::Refused);
        }
        let verb = wide(r.verb);
        let file = wide(&r.target);
        // SAFETY: initializes COM on this (launch) thread as an apartment, as ShellExecuteExW
        // requires for shell extensions; balanced by CoUninitialize below when it succeeded.
        let com = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        } >= 0;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0),
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: verb.as_ptr(),
            lpFile: file.as_ptr(),
            nShow: show_cmd(r.show),
            ..SHELLEXECUTEINFOW::default()
        };
        // SAFETY: lets the browser (often an already running process that we do not know) take
        // the foreground when Ddalgak itself may; a no-op otherwise.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        // SAFETY: `info` is fully initialized with NUL-terminated strings that outlive the call.
        let ok = unsafe { ShellExecuteExW(&raw mut info) } != 0;
        // SAFETY: reads this thread's last error right after the call.
        let err = unsafe { GetLastError() };
        if com {
            // SAFETY: balances the successful CoInitializeEx above, on the same thread.
            unsafe { CoUninitialize() };
        }
        if ok {
            Ok(())
        } else {
            // With SEE_MASK_FLAG_NO_UI the shell reports through GetLastError; `hInstApp` holds
            // an SE_ERR_* value (≤ 32) when it does not.
            let se = info.hInstApp as usize;
            let code = if err != 0 {
                err
            } else {
                u32::try_from(se).unwrap_or(0)
            };
            Err(LaunchError::from_os(code))
        }
    }

    fn activate(&self, program: &Path, real: &Path) -> Activation {
        let Some(hwnd) = find_window(program, real) else {
            return Activation::NotRunning;
        };
        // SAFETY: plain calls on a window handle that was just enumerated; a window that closed
        // meanwhile makes them fail harmlessly.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            Activation::Activated {
                foreground: SetForegroundWindow(hwnd) != 0,
            }
        }
    }
}

struct Search {
    want: Vec<PathBuf>,
    found: HWND,
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: `lparam` is the `&mut Search` passed to EnumWindows below, alive for the call.
    let s = unsafe { &mut *(lparam as *mut Search) };
    // SAFETY: plain queries on an enumerated top-level window.
    let candidate = unsafe {
        IsWindowVisible(hwnd) != 0
            && GetWindow(hwnd, GW_OWNER).is_null()
            && (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW) == 0
    };
    if !candidate {
        return 1;
    }
    let mut pid = 0u32;
    // SAFETY: writes the owning process id into `pid`.
    unsafe { GetWindowThreadProcessId(hwnd, &raw mut pid) };
    if pid == 0 || pid == std::process::id() {
        return 1;
    }
    match super::process_image(pid) {
        Ok(image) if s.want.iter().any(|w| same_file(w, &image)) => {
            s.found = hwnd;
            0
        }
        _ => 1,
    }
}

/// The first visible, unowned, non-tool top-level window of a process started from `program`
/// (as written or as its real file).
pub fn find_window(program: &Path, real: &Path) -> Option<HWND> {
    let mut s = Search {
        want: vec![program.to_path_buf(), real.to_path_buf()],
        found: std::ptr::null_mut(),
    };
    // SAFETY: the callback only reads windows and writes into `s`, which outlives the call.
    unsafe { EnumWindows(Some(enum_proc), (&raw mut s) as LPARAM) };
    (!s.found.is_null()).then_some(s.found)
}

/// Reads the large icon of a program file (index 0) as RGBA pixels, without running it. `None`
/// when the file has no icon.
pub fn icon_preview(path: &Path) -> Option<IconPreview> {
    let file = wide(path);
    let mut large: HICON = std::ptr::null_mut();
    // SAFETY: NUL-terminated path; one icon handle is written into `large`.
    let n = unsafe { ExtractIconExW(file.as_ptr(), 0, &raw mut large, std::ptr::null_mut(), 1) };
    if n == 0 || n == u32::MAX || large.is_null() {
        return None;
    }
    let out = icon_pixels(large);
    // SAFETY: destroys the icon ExtractIconExW created for us.
    unsafe { DestroyIcon(large) };
    out
}

fn icon_pixels(icon: HICON) -> Option<IconPreview> {
    let mut info = ICONINFO::default();
    // SAFETY: fills `info` with two bitmaps this function then owns and deletes.
    if unsafe { GetIconInfo(icon, &raw mut info) } == 0 {
        return None;
    }
    let read = |bmp: HBITMAP| -> Option<(u32, u32, Vec<u8>)> {
        if bmp.is_null() {
            return None;
        }
        let mut b = BITMAP::default();
        // SAFETY: `b` is a BITMAP of the size passed.
        let got = unsafe {
            GetObjectW(
                bmp,
                i32::try_from(size_of::<BITMAP>()).unwrap_or(0),
                (&raw mut b).cast::<c_void>(),
            )
        };
        if got == 0 || b.bmWidth <= 0 || b.bmHeight <= 0 {
            return None;
        }
        let (w, h) = (b.bmWidth, b.bmHeight);
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(0),
                biWidth: w,
                biHeight: -h, // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let (wu, hu) = (u32::try_from(w).ok()?, u32::try_from(h).ok()?);
        let mut px = vec![0u8; (wu as usize) * (hu as usize) * 4];
        // SAFETY: a memory DC for GetDIBits, deleted below.
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        // SAFETY: `px` holds w × h 32-bit pixels as described by `bmi`.
        let lines = unsafe {
            GetDIBits(
                dc,
                bmp,
                0,
                hu,
                px.as_mut_ptr().cast::<c_void>(),
                &raw mut bmi,
                DIB_RGB_COLORS,
            )
        };
        // SAFETY: deletes the DC created above.
        unsafe { DeleteDC(dc) };
        (lines > 0).then_some((wu, hu, px))
    };
    let color = read(info.hbmColor);
    let mask = read(info.hbmMask);
    // SAFETY: GetIconInfo gave us these bitmaps; each is deleted once.
    unsafe {
        if !info.hbmColor.is_null() {
            DeleteObject(info.hbmColor);
        }
        if !info.hbmMask.is_null() {
            DeleteObject(info.hbmMask);
        }
    }
    let (width, height, mut bgra) = color?;
    let has_alpha = bgra.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
    for (i, p) in bgra.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        p.swap(0, 2); // BGRA → RGBA
        if !has_alpha {
            // An icon without alpha: the mask says which pixels are transparent.
            let opaque = mask
                .as_ref()
                .filter(|(mw, mh, _)| *mw == width && *mh >= height)
                .is_none_or(|(_, _, m)| m.get(i * 4).is_some_and(|v| *v == 0));
            p[3] = if opaque { 255 } else { 0 };
        }
    }
    Some(IconPreview {
        width,
        height,
        rgba: bgra,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{command_line, quote_arg};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    /// Splits a command line exactly as Windows does (`CommandLineToArgvW`).
    fn argv(line: &str) -> Vec<String> {
        let w = wide(line);
        let mut n = 0i32;
        // SAFETY: NUL-terminated input; the returned array is freed with LocalFree.
        let list = unsafe { CommandLineToArgvW(w.as_ptr(), &raw mut n) };
        assert!(!list.is_null());
        let out = (0..usize::try_from(n).unwrap())
            // SAFETY: `n` valid NUL-terminated strings.
            .map(|i| unsafe { super::super::from_wide_ptr(*list.add(i)) })
            .collect();
        // SAFETY: frees the array CommandLineToArgvW allocated.
        unsafe { LocalFree(list.cast()) };
        out
    }

    #[test]
    fn windows_itself_splits_the_quoted_command_line_back_into_the_arguments() {
        let args: Vec<String> = [
            "plain",
            "",
            "with space",
            "한글 인자",
            "tab\there",
            r#"say "hi""#,
            r"C:\path with space\",
            r"C:\no-space\",
            r"back\\slash",
            r#"\"quoted\""#,
            r"trailing\\",
            "--flag=a b",
            "emoji 👍",
            "%PATH%",
            "&|<>^",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let program = Path::new(r"C:\프로그램 폴더 (x86)\my app.exe");
        let line = command_line(program, &args);
        let got = argv(&line);
        assert_eq!(got[0], program.to_string_lossy());
        assert_eq!(&got[1..], &args[..], "{line}");
        for a in &args {
            let mut s = String::new();
            quote_arg(a, &mut s);
            assert_eq!(
                &argv(&format!("x.exe {s}"))[1..],
                std::slice::from_ref(a),
                "{s}"
            );
        }
    }

    #[test]
    fn a_missing_program_or_a_non_program_fails_cleanly_and_starts_nothing() {
        let dir = std::env::temp_dir().join(format!("ddalgak-win-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("not a program.exe");
        std::fs::write(&fake, "this is text, not a program").unwrap();
        let req = |p: &Path| SpawnRequest {
            program: p.to_path_buf(),
            command_line: command_line(p, &[]),
            working_dir: dir.clone(),
            show: Show::Hidden,
            env: Vec::new(),
        };
        assert_eq!(
            WinLauncher.spawn(&req(&dir.join("missing.exe"))),
            Err(LaunchError::NotFound)
        );
        assert_eq!(WinLauncher.spawn(&req(&fake)), Err(LaunchError::BadFormat));
        let mut bad_dir = req(&fake);
        bad_dir.working_dir = dir.join("no such folder");
        assert!(WinLauncher.spawn(&bad_dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shell_is_never_given_anything_but_open_on_a_checked_target() {
        // Refused before ShellExecuteExW is called: nothing opens.
        for (verb, target) in [
            ("open", "javascript:alert(1)"),
            ("open", "file:///C:/Windows/win.ini"),
            ("open", "ms-settings:privacy"),
            ("open", r"C:\Windows\System32\notepad.exe"),
            ("runas", "https://example.com/"),
            ("edit", "https://example.com/"),
            ("open", r"shell:AppsFolder\not an id"),
        ] {
            let r = ShellRequest {
                verb,
                target: target.into(),
                show: Show::Normal,
            };
            assert_eq!(
                WinLauncher.shell_open(&r),
                Err(LaunchError::Refused),
                "{verb} {target}"
            );
        }
        let hidden = ShellRequest {
            verb: "open",
            target: "https://example.com/".into(),
            show: Show::Hidden,
        };
        assert_eq!(WinLauncher.shell_open(&hidden), Err(LaunchError::Refused));
    }

    #[test]
    fn environment_blocks_are_sorted_and_double_terminated() {
        let b = env_block(&[("DDALGAK_TEST_VAR".into(), "값 value".into())]);
        assert_eq!(&b[b.len() - 2..], &[0, 0]);
        let text = String::from_utf16_lossy(&b);
        assert!(text.contains("DDALGAK_TEST_VAR=값 value\0"), "{text}");
    }

    #[test]
    fn icons_are_read_from_program_files_without_running_them() {
        let notepad = Path::new(&std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("notepad.exe");
        let icon = icon_preview(&notepad).expect("notepad has an icon");
        assert!(icon.width >= 16 && icon.height >= 16, "{icon:?}");
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
        assert!(
            icon.rgba.as_chunks::<4>().0.iter().any(|p| p[3] > 0),
            "some opaque pixels"
        );
        let txt = std::env::temp_dir().join(format!("ddalgak-noicon-{}.txt", std::process::id()));
        std::fs::write(&txt, "x").unwrap();
        assert!(
            icon_preview(&txt).is_none(),
            "a text file has no icon resource"
        );
        let _ = std::fs::remove_file(&txt);
        assert!(icon_preview(Path::new(r"C:\ddalgak\no\such\file.exe")).is_none());
    }

    #[test]
    fn the_window_search_reads_the_desktop_and_finds_nothing_for_a_program_without_windows() {
        // Walks every top-level window of the desktop read-only (process images only, never
        // titles); a file no running process was started from has no window. Nothing is
        // activated: other programs' windows are never touched by the tests.
        let nowhere = Path::new(r"C:\ddalgak-test\never started\nothing.exe");
        let t = std::time::Instant::now();
        assert!(find_window(nowhere, nowhere).is_none());
        eprintln!("window search over the desktop: {:?}", t.elapsed());
    }
}
