//! Where Save writes (Step 51; NOEUL-FR-031, NOEUL-FR-032): the user's Pictures folder, the
//! local time for file names, and the save dialog.
//!
//! The save dialog is the system's own (`GetSaveFileNameW`, Explorer style), owned by the
//! capture overlay so it stays above it, starting in the quick-save folder with the suggested
//! name `noeul-YYYYMMDD-HHMMSS.png`; it asks before replacing a file. It is modal, so the
//! runtime shows it with the instance core released. It is never shown by automated runs (the
//! debug-build test hook answers without it, see `runtime`).

use crate::app::SaveRequest;
use crate::win::wide;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::{HWND, SYSTEMTIME};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath};

/// The user's Pictures folder (follows a redirected or OneDrive folder), if Windows knows it.
pub fn pictures_dir() -> Option<PathBuf> {
    let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
    let id = FOLDERID_Pictures;
    // SAFETY: the documented call; `p` receives a CoTaskMem string we free below.
    let hr = unsafe { SHGetKnownFolderPath(&raw const id, 0, std::ptr::null_mut(), &raw mut p) };
    if p.is_null() {
        return None;
    }
    // SAFETY: on success `p` is a NUL-terminated wide string.
    let path = (hr >= 0).then(|| unsafe {
        let len = (0..).take_while(|&i| *p.add(i) != 0).count();
        PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(p, len)))
    });
    // SAFETY: frees the string the call allocated (also after a failure, as documented).
    unsafe { CoTaskMemFree(p.cast()) };
    path
}

/// The local time `[year, month, day, hour, minute, second]`.
pub fn local_time() -> [u16; 6] {
    let mut t = SYSTEMTIME::default();
    // SAFETY: `t` is writable.
    unsafe { GetLocalTime(&raw mut t) };
    [t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond]
}

/// Shows the save dialog for `req`, owned by `owner` (the capture overlay), and returns the
/// chosen file, or `None` when the user cancelled.
pub fn save_dialog(owner: Option<HWND>, req: &SaveRequest) -> Option<PathBuf> {
    let mut file: Vec<u16> = req.name.encode_utf16().collect();
    file.resize(32_768, 0);
    let filter: Vec<u16> = "PNG image (*.png)\0*.png\0\0".encode_utf16().collect();
    let dir = req.folder.as_ref().map(|d| wide(d.as_os_str()));
    let ext = wide("png");
    let title = wide("Save the selection");
    let mut ofn = OPENFILENAMEW {
        lStructSize: u32::try_from(size_of::<OPENFILENAMEW>()).expect("small"),
        hwndOwner: owner.unwrap_or(std::ptr::null_mut()),
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: u32::try_from(file.len()).expect("small"),
        lpstrInitialDir: dir.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: ext.as_ptr(),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every pointer in `ofn` points to a live buffer of the stated size; modal.
    if unsafe { GetSaveFileNameW(&raw mut ofn) } == 0 {
        return None;
    }
    let len = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    (len > 0).then(|| PathBuf::from(OsString::from_wide(&file[..len])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pictures_folder_and_local_time_are_known() {
        let p = pictures_dir().expect("Windows knows the Pictures folder");
        assert!(p.is_absolute(), "{}", p.display());
        let t = local_time();
        assert!(t[0] >= 2024 && (1..=12).contains(&t[1]) && (1..=31).contains(&t[2]));
    }
}
