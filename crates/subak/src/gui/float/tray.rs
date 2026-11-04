//! The tray icon behind a narrow interface (ADR 0045): the floating UI says what the tray shows,
//! and an implementation shows it. Windows uses `Shell_NotifyIconW` (the window module); the
//! off-screen test mode uses [`FakeTray`], which only records what it was told, so automated
//! runs never put an icon into the user's notification area.

use super::art::Picture;

/// One request to the tray (what [`FakeTray`] records).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayCall {
    /// Shown, with its tip and the picture's hash.
    Add { tip: String, picture: u64 },
    /// Its tip or picture changed.
    Modify { tip: String, picture: u64 },
    /// Taken away.
    Delete,
}

/// The notification-area icon.
pub trait Tray {
    /// Shows the icon (again: Explorer restarted). `Err`: the shell refused.
    fn add(&mut self, tip: &str, picture: &Picture) -> Result<(), String>;
    /// Updates the tip and the picture of a shown icon.
    fn modify(&mut self, tip: &str, picture: &Picture) -> Result<(), String>;
    /// Removes the icon (nothing happens when it is not shown).
    fn delete(&mut self);
    /// Whether it is shown.
    fn shown(&self) -> bool;
    /// The calls so far (only the fake keeps them; test reports).
    fn calls(&self) -> &[TrayCall] {
        &[]
    }
}

/// A tray that only records (the off-screen test mode, unit tests).
#[derive(Debug, Default)]
pub struct FakeTray {
    shown: bool,
    calls: Vec<TrayCall>,
}

impl Tray for FakeTray {
    fn add(&mut self, tip: &str, picture: &Picture) -> Result<(), String> {
        self.shown = true;
        self.calls.push(TrayCall::Add {
            tip: tip.to_string(),
            picture: picture.hash(),
        });
        Ok(())
    }

    fn modify(&mut self, tip: &str, picture: &Picture) -> Result<(), String> {
        if !self.shown {
            return Err("not shown".into());
        }
        self.calls.push(TrayCall::Modify {
            tip: tip.to_string(),
            picture: picture.hash(),
        });
        Ok(())
    }

    fn delete(&mut self) {
        if self.shown {
            self.shown = false;
            self.calls.push(TrayCall::Delete);
        }
    }

    fn shown(&self) -> bool {
        self.shown
    }

    fn calls(&self) -> &[TrayCall] {
        &self.calls
    }
}

/// The tray's tip: Windows keeps 127 characters; a longer tip is cut with `…`.
pub fn tip(text: &str) -> String {
    if text.chars().count() <= 127 {
        return text.to_string();
    }
    let mut t: String = text.chars().take(126).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::float::art::{IconState, Palette, render};

    #[test]
    fn the_fake_records_and_follows_shown() {
        let p = render(&IconState::default(), 16, &Palette::subak());
        let mut t = FakeTray::default();
        assert!(!t.shown());
        assert!(t.modify("x", &p).is_err());
        t.add("Subak", &p).unwrap();
        t.modify("Subak — 1 Terminal", &p).unwrap();
        t.delete();
        t.delete();
        assert_eq!(
            t.calls(),
            [
                TrayCall::Add {
                    tip: "Subak".into(),
                    picture: p.hash()
                },
                TrayCall::Modify {
                    tip: "Subak — 1 Terminal".into(),
                    picture: p.hash()
                },
                TrayCall::Delete,
            ]
        );
    }

    #[test]
    fn long_tips_are_cut() {
        assert_eq!(tip("short"), "short");
        let long = "가".repeat(200);
        let t = tip(&long);
        assert_eq!(t.chars().count(), 127);
        assert!(t.ends_with('…'));
    }
}
