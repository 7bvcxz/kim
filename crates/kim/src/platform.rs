//! Detection of the current platform for catalog compatibility checks.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Platform {
    /// `windows` or `linux` (catalog vocabulary).
    pub os: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    /// Windows build number (e.g. 19045), when known.
    pub os_build: Option<u32>,
}

impl Platform {
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            os_build: os_build(),
        }
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn os_build() -> Option<u32> {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(size_of::<OSVERSIONINFOW>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `info` is a properly sized, initialized OSVERSIONINFOW owned by this frame;
    // RtlGetVersion only writes into it and returns an NTSTATUS. Unlike GetVersionExW it is not
    // subject to compatibility shims, so it reports the real build number.
    let status = unsafe { RtlGetVersion(&raw mut info) };
    (status == 0).then_some(info.dwBuildNumber)
}

#[cfg(not(windows))]
fn os_build() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_platform_uses_catalog_vocabulary() {
        let p = Platform::current();
        assert!(
            ["windows", "linux", "macos"].contains(&p.os.as_str()),
            "{p:?}"
        );
        assert!(!p.arch.is_empty());
        #[cfg(windows)]
        assert!(
            p.os_build.is_some_and(|b| b >= 17_763),
            "Windows 10 1809+ expected, got {p:?}"
        );
    }
}
