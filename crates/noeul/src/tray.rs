//! The optional notification-area icon: its menu model (NOEUL-FR-004).
//!
//! The menu is computed from the instance state each time it opens, so items appear only once
//! they are implemented and only when they apply. The Win32 icon (`win::tray`) renders this
//! model and reports clicks; automated tests never create a real icon (they test this model and
//! the core's reactions, `app::tests`). With the icon off (`[general] tray_icon = false`, or
//! `noeul start --no-tray`), the CLI and the hotkeys still reach everything.
//!
//! Capture region (and left click to capture) since Step 50; Show or hide all pins and Bring
//! pins on screen since Step 53 (the pin engine, ADR 0029); Make all pins clickable (while a pin
//! lets clicks through) and one entry per group (while there are several, the current one
//! checked; choosing one switches to it) since Step 54 (ADR 0044). Not in this build: Settings
//! (N5).

/// One menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    CaptureRegion,
    PinClipboard,
    /// Shown while every open pin is visible.
    HideAllPins,
    /// Shown while a pin is hidden.
    ShowAllPins,
    /// Shown while a pin lies where no monitor shows it (NOEUL-FR-067): shows and moves it into
    /// the nearest work area.
    BringPinsOnScreen,
    ReopenClosedPin,
    /// Shown while a pin lets clicks through (NOEUL-FR-063): makes every pin clickable again.
    MakeAllPinsClickable,
    /// Switches to the group at this index of the group list (shown while there are several).
    Group(u16),
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
            MenuItem::HideAllPins => "Hide all pins",
            MenuItem::ShowAllPins => "Show all pins",
            MenuItem::BringPinsOnScreen => "Bring pins on screen",
            MenuItem::ReopenClosedPin => "Reopen closed pin",
            MenuItem::MakeAllPinsClickable => "Make all pins clickable",
            // The core names the group (`App::tray_entries`).
            MenuItem::Group(_) => "Group",
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
            MenuItem::HideAllPins => 5,
            MenuItem::ShowAllPins => 6,
            MenuItem::BringPinsOnScreen => 7,
            MenuItem::ReopenClosedPin => 2,
            MenuItem::MakeAllPinsClickable => 8,
            MenuItem::Group(i) => 100 + u32::from(i),
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
    /// Open pins, how many of them are hidden, and how many no monitor shows.
    pub open: usize,
    pub hidden: usize,
    pub off_screen: usize,
    /// Open pins that let clicks through.
    pub click_through: usize,
    /// How many groups there are (the current one included).
    pub groups: usize,
}

/// One menu entry as shown: its item, its text and whether it is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub item: MenuItem,
    pub label: String,
    pub checked: bool,
}

pub fn menu(s: &MenuState) -> Vec<MenuItem> {
    let mut items = vec![MenuItem::CaptureRegion, MenuItem::PinClipboard];
    if s.hidden > 0 {
        items.push(MenuItem::ShowAllPins);
    } else if s.open > 0 {
        items.push(MenuItem::HideAllPins);
    }
    if s.click_through > 0 {
        items.push(MenuItem::MakeAllPinsClickable);
    }
    if s.off_screen > 0 {
        items.push(MenuItem::BringPinsOnScreen);
    }
    if s.reopenable {
        items.push(MenuItem::ReopenClosedPin);
    }
    if s.groups > 1 {
        items.extend((0..s.groups.min(32)).map(|i| MenuItem::Group(i as u16)));
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
            open: 0,
            hidden: 0,
            off_screen: 0,
            click_through: 0,
            groups: 1,
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
            ..base
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
        // Pin items follow the pins: hide while all show, show while one is hidden, bring
        // back while one is lost.
        let with = |open, hidden, off_screen| {
            menu(&MenuState {
                open,
                hidden,
                off_screen,
                ..base
            })
        };
        assert!(with(2, 0, 0).contains(&MenuItem::HideAllPins));
        assert!(!with(2, 0, 0).contains(&MenuItem::ShowAllPins));
        assert!(with(2, 1, 0).contains(&MenuItem::ShowAllPins));
        assert!(!with(2, 1, 0).contains(&MenuItem::HideAllPins));
        assert!(with(2, 0, 1).contains(&MenuItem::BringPinsOnScreen));
        assert!(!with(2, 0, 0).contains(&MenuItem::BringPinsOnScreen));
        let all = with(3, 1, 1);
        let ids: std::collections::BTreeSet<u32> = all.iter().map(|i| i.command_id()).collect();
        assert_eq!(ids.len(), all.len(), "command ids stay unique");
        // Step 54: the way back from click-through, and one entry per group while there are
        // several.
        let m = menu(&MenuState {
            open: 3,
            click_through: 1,
            groups: 3,
            ..base
        });
        assert!(m.contains(&MenuItem::MakeAllPinsClickable));
        assert_eq!(
            m.iter().filter(|i| matches!(i, MenuItem::Group(_))).count(),
            3
        );
        let ids: std::collections::BTreeSet<u32> = m.iter().map(|i| i.command_id()).collect();
        assert_eq!(ids.len(), m.len(), "command ids stay unique");
        assert!(!with(2, 0, 0).contains(&MenuItem::MakeAllPinsClickable));
        assert!(
            !with(2, 0, 0)
                .iter()
                .any(|i| matches!(i, MenuItem::Group(_))),
            "one group: no group entries"
        );
    }
}
