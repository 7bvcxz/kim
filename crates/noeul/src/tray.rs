//! The optional notification-area icon: its menu model (NOEUL-FR-004).
//!
//! The menu is computed from the instance state each time it opens, so items appear only once
//! they are implemented and only when they apply. The Win32 icon (`win::tray`) renders this
//! model and reports clicks; automated tests never create a real icon (they test this model and
//! the core's reactions, `app::tests`). With the icon off (`[general] tray_icon = false`, or
//! `noeul start --no-tray`), the CLI and the hotkeys still reach everything.
//!
//! Capture region (and left click to capture) since Step 50. Not in this build: Show or hide all
//! pins, Make all pins clickable, Bring pins on screen (N2/N4), Settings (N5).

/// One menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    CaptureRegion,
    PinClipboard,
    ReopenClosedPin,
    /// The full shortcut switch (HOTKEY_CONTRACT §12.4); `on` is the current state.
    Shortcuts {
        on: bool,
    },
    Quit,
}

impl MenuItem {
    pub fn label(self) -> &'static str {
        match self {
            MenuItem::CaptureRegion => "Capture region\tF1",
            MenuItem::PinClipboard => "Pin clipboard\tF3",
            MenuItem::ReopenClosedPin => "Reopen closed pin",
            MenuItem::Shortcuts { on: true } => "Keyboard shortcuts: on",
            MenuItem::Shortcuts { on: false } => "Keyboard shortcuts: off",
            MenuItem::Quit => "Quit Noeul",
        }
    }

    /// Stable menu command IDs (Win32 `TrackPopupMenu` returns them).
    pub fn command_id(self) -> u32 {
        match self {
            MenuItem::CaptureRegion => 4,
            MenuItem::PinClipboard => 1,
            MenuItem::ReopenClosedPin => 2,
            MenuItem::Shortcuts { .. } => 3,
            MenuItem::Quit => 9,
        }
    }
}

/// A mouse click on the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Left,
    Middle,
    Right,
}

/// What the menu depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuState {
    pub reopenable: bool,
    pub shortcuts_on: bool,
    /// `false` when shortcuts are off by a start flag or safe mode: the tray cannot change that.
    pub shortcuts_switchable: bool,
}

pub fn menu(s: &MenuState) -> Vec<MenuItem> {
    let mut items = vec![MenuItem::CaptureRegion, MenuItem::PinClipboard];
    if s.reopenable {
        items.push(MenuItem::ReopenClosedPin);
    }
    if s.shortcuts_switchable {
        items.push(MenuItem::Shortcuts { on: s.shortcuts_on });
    }
    items.push(MenuItem::Quit);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_appear_only_when_they_apply() {
        let base = MenuState {
            reopenable: false,
            shortcuts_on: true,
            shortcuts_switchable: true,
        };
        assert_eq!(
            menu(&base),
            [
                MenuItem::CaptureRegion,
                MenuItem::PinClipboard,
                MenuItem::Shortcuts { on: true },
                MenuItem::Quit
            ]
        );
        let m = menu(&MenuState {
            reopenable: true,
            shortcuts_on: false,
            shortcuts_switchable: true,
        });
        assert_eq!(m[2], MenuItem::ReopenClosedPin);
        assert_eq!(m[3].label(), "Keyboard shortcuts: off");
        let ids: std::collections::BTreeSet<u32> = m.iter().map(|i| i.command_id()).collect();
        assert_eq!(ids.len(), m.len(), "command ids are unique");
        assert!(
            !menu(&MenuState {
                shortcuts_switchable: false,
                ..base
            })
            .iter()
            .any(|i| matches!(i, MenuItem::Shortcuts { .. }))
        );
    }
}
