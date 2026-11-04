//! The floating icon, the tray icon and the compact Terminal manager (Step 18, ADR 0045,
//! docs/SUBAK_GUI.md §11).
//!
//! `subak-gui --float` runs them as a **tiny process of the same binary**, beside the windows:
//! the same plain Win32 + GDI stack (no GUI framework, no terminal renderer, no GPU), one window
//! thread, and one IPC connection of kind `gui` (`subak-float`) that lists the Host's Terminals,
//! follows their events and sends the manager's create, rename, kill and remove requests. It
//! never attaches a pane. It is optional and off unless the user starts it (or asks a window
//! for it); the Host and the CLI never start it and never need it, and closing it — or the
//! manager — is a disconnect that ends nothing.
//!
//! | Module | Role | Platform |
//! | --- | --- | --- |
//! | [`place`] | icon size at a DPI, default place, drag with edge snapping, off-screen recovery, the manager's rectangle | portable, unit-tested |
//! | [`art`] | the icon's picture from its state (the Step 44 interface), its words | portable, unit-tested |
//! | [`list`] | the manager's list, search, selection and Host conversation, guarded delete | portable, unit-tested |
//! | [`fprefs`] | `%APPDATA%\mouv\subak\float.json` | portable, unit-tested |
//! | [`tray`] | the tray behind a trait, with a recording fake for tests | portable, unit-tested |
//! | `win` | the icon window, the tray, the manager window, opening Terminals in windows, test hooks | Windows |

#[cfg_attr(not(windows), allow(dead_code))]
pub mod art;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod fprefs;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod list;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod place;
#[cfg_attr(not(windows), allow(dead_code))]
pub mod tray;

#[cfg(windows)]
#[allow(unsafe_code)]
pub(super) mod win;

/// `dwData` of a request between Subak's own windows (`WM_COPYDATA`, not a test hook): "SUBO".
/// A JSON body `{"op": "probe" | "open" | "show" | "manager", "endpoint": …, …}`; the reply is 1
/// when the receiver serves that endpoint and did it, else 0. It can only show or select
/// things — never create, end or change a Terminal.
pub const COPYDATA_OPEN: usize = 0x5355_424F;

/// The longest request body accepted (bytes).
pub const COPYDATA_MAX: usize = 4096;

/// What `--float` was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FloatOptions {
    /// `--icon` / `--tray` given: show exactly these (and save the choice); neither: the saved
    /// choice.
    pub icon: bool,
    pub tray: bool,
    /// `--manager`: open the Terminal manager (of the running floating UI, or of this one).
    pub manager: bool,
    /// Test hooks: a synthetic monitor layout `(left, top, right, bottom, dpi)` instead of the
    /// real one (it must lie off every real monitor).
    pub test_monitors: Option<Vec<(i32, i32, i32, i32, u32)>>,
}

/// Parses `--test-monitors`: `left,top,right,bottom,dpi` work areas separated by `;`.
pub fn parse_monitors(s: &str) -> Option<Vec<(i32, i32, i32, i32, u32)>> {
    let v: Option<Vec<_>> = s
        .split(';')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let n: Vec<&str> = p.split(',').map(str::trim).collect();
            let [l, t, r, b, d] = n.as_slice() else {
                return None;
            };
            let (l, t, r, b) = (
                l.parse().ok()?,
                t.parse().ok()?,
                r.parse().ok()?,
                b.parse().ok()?,
            );
            let d: u32 = d.parse().ok()?;
            (r > l && b > t && (48..=960).contains(&d)).then_some((l, t, r, b, d))
        })
        .collect();
    v.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_monitor_layouts_parse() {
        assert_eq!(
            parse_monitors("-20000,-20000,-18080,-18960,96; -18080,-20000,-15520,-18600,144"),
            Some(vec![
                (-20000, -20000, -18080, -18960, 96),
                (-18080, -20000, -15520, -18600, 144)
            ])
        );
        assert_eq!(parse_monitors(""), None);
        assert_eq!(parse_monitors("1,2,3"), None);
        assert_eq!(parse_monitors("10,10,5,20,96"), None);
        assert_eq!(parse_monitors("0,0,10,10,5"), None);
    }
}
