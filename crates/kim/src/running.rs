//! Detection of running programs from an install directory, so Kim can refuse to uninstall (or
//! warn on update) instead of ever stopping a tool by itself (KIM-FR-034).

use std::path::Path;

/// A running process whose executable lives under a given directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunningProcess {
    pub pid: u32,
    pub image: String,
}

/// Processes whose image path is inside `dir` (case-insensitive on Windows).
pub fn processes_under(dir: &Path) -> Vec<RunningProcess> {
    let Ok(root) = std::fs::canonicalize(dir) else {
        return Vec::new();
    };
    let prefix = normalize(&root.display().to_string());
    imp::all_images()
        .into_iter()
        .filter(|(_, image)| {
            let n = normalize(image);
            n.starts_with(&prefix) && n[prefix.len()..].starts_with('\\')
        })
        .map(|(pid, image)| RunningProcess { pid, image })
        .collect()
}

/// Whether a process with this id is still running.
pub fn pid_alive(pid: u32) -> bool {
    imp::pid_alive(pid)
}

fn normalize(p: &str) -> String {
    p.trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    /// (pid, full image path) for every process whose path can be queried.
    pub fn all_images() -> Vec<(u32, String)> {
        let mut out = Vec::new();
        // SAFETY: plain Toolhelp snapshot; the handle is closed below on every path.
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0),
            ..Default::default()
        };
        // SAFETY: `entry` is initialized with the correct dwSize as the API requires.
        let mut ok = unsafe { Process32FirstW(snap, &raw mut entry) } != 0;
        while ok {
            let pid = entry.th32ProcessID;
            if let Some(image) = image_path(pid) {
                out.push((pid, image));
            }
            // SAFETY: same snapshot handle and initialized entry.
            ok = unsafe { Process32NextW(snap, &raw mut entry) } != 0;
        }
        // SAFETY: `snap` is a valid handle returned above.
        unsafe { CloseHandle(snap) };
        out
    }

    pub fn pid_alive(pid: u32) -> bool {
        use windows_sys::Win32::System::Threading::GetExitCodeProcess;
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: limited query access; null handle is checked; handle closed below.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        // SAFETY: `h` is valid and `code` is a writable u32.
        let ok = unsafe { GetExitCodeProcess(h, &raw mut code) } != 0;
        // SAFETY: `h` was opened above.
        unsafe { CloseHandle(h) };
        ok && code == STILL_ACTIVE
    }

    fn image_path(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        // SAFETY: limited query access only; null handle is checked.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 32_768];
        let mut len = u32::try_from(buf.len()).unwrap_or(0);
        // SAFETY: `buf`/`len` describe a writable buffer; `h` is a valid process handle.
        let ok = unsafe {
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &raw mut len)
        } != 0;
        // SAFETY: `h` was opened above.
        unsafe { CloseHandle(h) };
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(not(windows))]
mod imp {
    /// Not implemented before milestone M4 (Linux); reports nothing running.
    pub fn all_images() -> Vec<(u32, String)> {
        Vec::new()
    }

    /// Without process inspection, assume a recorded owner may still be alive.
    pub fn pid_alive(_pid: u32) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn finds_a_process_started_from_a_directory() {
        // Copy a known console program into a temp dir, start it, and look for it.
        let dir = std::env::temp_dir().join(format!("kim-running-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ping = dir.join("kimtestping.exe");
        std::fs::copy(r"C:\Windows\System32\PING.EXE", &ping).unwrap();
        let mut child = std::process::Command::new(&ping)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let found = processes_under(&dir);
        let alive_before = pid_alive(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert!(alive_before);
        assert!(!pid_alive(child.id()), "a reaped process is not alive");
        assert!(found.iter().any(|p| p.pid == child.id()), "{found:?}");
        assert!(processes_under(&dir.join("nonexistent")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prefix_must_end_at_a_path_separator() {
        assert_eq!(normalize(r"\\?\C:/Users/A"), r"c:\users\a");
    }
}
