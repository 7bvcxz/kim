//! The status indicator (Step 73, ADR 0046; `DDALGAK-FR-002`, `DDALGAK-SEC-010`): what the tray
//! icon, its tooltip and the optional floating badge show, as data. The tray draws it
//! (`win::tray`), `ddalgak status --json` returns it (`indicator`), and the editor (Step 74)
//! draws the floating badge from the same model.
//!
//! - **Subtle by default.** The tray icon's shape and tooltip carry the state; nothing pops up for
//!   ordinary changes. After an emergency off, a lost hook or an unclean exit the off icon is
//!   struck through ([`Shape::OffAlert`], DDALGAK_GUI_JOURNEYS J8) until the engine is turned on
//!   again, and the tooltip names the reason in words.
//! - **Never color alone.** Every state has its own shape and words (`DDALGAK-NFR-007`).
//! - **The floating badge is opt-in** (`settings.floating_indicator`: `off` by default, `paused`
//!   only while paused, `always`). It never takes focus and is not drawn before Step 74.
//! - **Event-driven.** The host rebuilds the indicator when something changes (a state change, a
//!   mode switch, a reload, a session event); nothing polls (`DDALGAK-NFR-001`).

use crate::lifecycle::{OffReason, State};
use crate::model::FloatingIndicator;
use crate::modes::Indicator;
use serde_json::{Value, json};

/// The tray icon's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// A ring: the engine is off.
    Off,
    /// A struck-through ring: off after an emergency, a lost hook or an unclean exit.
    OffAlert,
    /// A filled disc: on.
    Running,
    /// The disc with a notch: on, outside the default mode (Step 72).
    RunningAway,
    /// Two bars: paused.
    Paused,
}

impl Shape {
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Off => "ring",
            Shape::OffAlert => "ring_struck",
            Shape::Running => "disc",
            Shape::RunningAway => "disc_notched",
            Shape::Paused => "bars",
        }
    }
}

/// What the indicator is built from (all known to the host without asking anyone).
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub state: Option<State>,
    /// Why the engine is off (only while off).
    pub off_reason: Option<OffReason>,
    /// The mode indicator (Step 72), while the engine is on.
    pub mode: Option<Indicator>,
    pub profile_enabled: bool,
    pub session_locked: bool,
    /// The running app kept its previous rules because `rules.json` is not usable.
    pub rules_kept_previous: bool,
    /// One of Ddalgak's own shortcuts could not be registered (`taken`).
    pub shortcut_taken: bool,
    pub floating: Option<FloatingIndicator>,
}

/// The floating badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Floating {
    pub setting: FloatingIndicator,
    pub visible: bool,
    pub label: String,
}

/// The whole indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub shape: Shape,
    /// At most 127 characters (the notification area's limit).
    pub tooltip: String,
    /// What needs the user's attention, as a code: `emergency`, `hook_lost`, `unclean_exit`,
    /// `profile_disabled`, `rules_kept_previous`, `shortcut_taken`, `session_locked`.
    pub attention: Option<&'static str>,
    pub floating: Floating,
}

fn words(code: &str) -> &'static str {
    match code {
        "emergency" => "turned off by the emergency chord",
        "hook_lost" => "turned off: Windows may have removed its keyboard hook",
        "unclean_exit" => "off: the last run did not end cleanly",
        "profile_disabled" => "the profile is disabled, no rule runs",
        "rules_kept_previous" => "rules.json has a problem, the previous rules run",
        "shortcut_taken" => "a shortcut is used by another program",
        "session_locked" => "rules wait while the session is locked",
        _ => "",
    }
}

fn clip(s: String) -> String {
    if s.chars().count() > 127 {
        s.chars().take(126).collect::<String>() + "…"
    } else {
        s
    }
}

/// Builds the indicator.
pub fn build(f: &Facts) -> Status {
    let state = f.state.unwrap_or(State::Off);
    let alert = match (state, f.off_reason) {
        (State::Off, Some(OffReason::Emergency)) => Some("emergency"),
        (State::Off, Some(OffReason::HookLost)) => Some("hook_lost"),
        (State::Off, Some(OffReason::UncleanExit)) => Some("unclean_exit"),
        _ => None,
    };
    let attention = alert.or_else(|| {
        [
            (
                state != State::Off && !f.profile_enabled,
                "profile_disabled",
            ),
            (f.rules_kept_previous, "rules_kept_previous"),
            (f.shortcut_taken, "shortcut_taken"),
            (state != State::Off && f.session_locked, "session_locked"),
        ]
        .into_iter()
        .find_map(|(on, code)| on.then_some(code))
    });
    let away = f.mode.as_ref().is_some_and(|m| m.visible && m.away);
    let shape = match state {
        State::Off if alert.is_some() => Shape::OffAlert,
        State::Off => Shape::Off,
        State::Running if away => Shape::RunningAway,
        State::Running => Shape::Running,
        State::Paused => Shape::Paused,
    };
    let base = crate::modes::tooltip(state, f.mode.as_ref());
    let tooltip = clip(match attention {
        Some(code) => format!("{base} ({})", words(code)),
        None => base,
    });
    let setting = f.floating.unwrap_or(FloatingIndicator::Off);
    let visible = match setting {
        FloatingIndicator::Off => false,
        FloatingIndicator::Paused => state == State::Paused,
        FloatingIndicator::Always => true,
    };
    let mut label = match state {
        State::Off => "Ddalgak off".to_string(),
        State::Running => "Ddalgak on".to_string(),
        State::Paused => "Ddalgak paused".to_string(),
    };
    if let Some(m) = f.mode.as_ref().filter(|m| m.visible && state != State::Off) {
        label.push_str(&format!(" · {}", m.label));
    }
    Status {
        shape,
        tooltip,
        attention,
        floating: Floating {
            setting,
            visible,
            label,
        },
    }
}

impl Status {
    pub fn to_json(&self) -> Value {
        json!({
            "shape": self.shape.as_str(),
            "tooltip": self.tooltip,
            "attention": self.attention,
            "floating": {
                "setting": match self.floating.setting {
                    FloatingIndicator::Off => "off",
                    FloatingIndicator::Paused => "paused",
                    FloatingIndicator::Always => "always",
                },
                "visible": self.floating.visible,
                "label": self.floating.label,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(state: State) -> Facts {
        Facts {
            state: Some(state),
            profile_enabled: true,
            ..Facts::default()
        }
    }

    fn nav(away: bool) -> Indicator {
        Indicator {
            visible: true,
            away,
            title: "Navigation".into(),
            icon: Some("N".into()),
            color: None,
            label: "N Navigation".into(),
        }
    }

    #[test]
    fn every_state_has_its_own_shape_and_words() {
        let off = build(&facts(State::Off));
        let on = build(&facts(State::Running));
        let paused = build(&facts(State::Paused));
        assert_eq!(
            (off.shape, on.shape, paused.shape),
            (Shape::Off, Shape::Running, Shape::Paused)
        );
        assert_eq!(off.tooltip, "Ddalgak: engine off");
        assert_eq!(paused.tooltip, "Ddalgak: engine paused");
        assert!(off.attention.is_none() && on.attention.is_none());
        let away = build(&Facts {
            mode: Some(nav(true)),
            ..facts(State::Running)
        });
        assert_eq!(away.shape, Shape::RunningAway);
        assert_eq!(away.tooltip, "Ddalgak: engine on · mode N Navigation");
    }

    #[test]
    fn an_emergency_a_lost_hook_and_a_crash_strike_the_icon_through() {
        for (reason, code) in [
            (OffReason::Emergency, "emergency"),
            (OffReason::HookLost, "hook_lost"),
            (OffReason::UncleanExit, "unclean_exit"),
        ] {
            let s = build(&Facts {
                off_reason: Some(reason),
                ..facts(State::Off)
            });
            assert_eq!(s.shape, Shape::OffAlert, "{code}");
            assert_eq!(s.attention, Some(code));
            assert!(
                s.tooltip.starts_with("Ddalgak: engine off ("),
                "{}",
                s.tooltip
            );
        }
        // A plain turn-off, the user's remembered turn-off and the default start are not alerts.
        for reason in [
            OffReason::Stopped,
            OffReason::Disabled,
            OffReason::Default,
            OffReason::NoHotkeys,
        ] {
            let s = build(&Facts {
                off_reason: Some(reason),
                ..facts(State::Off)
            });
            assert_eq!(s.shape, Shape::Off, "{reason:?}");
        }
    }

    #[test]
    fn attention_is_named_in_words_in_order_of_importance() {
        let s = build(&Facts {
            profile_enabled: false,
            shortcut_taken: true,
            ..facts(State::Running)
        });
        assert_eq!(s.attention, Some("profile_disabled"));
        assert!(s.tooltip.contains("no rule runs"), "{}", s.tooltip);
        let s = build(&Facts {
            session_locked: true,
            ..facts(State::Paused)
        });
        assert_eq!(s.attention, Some("session_locked"));
        assert_eq!(s.shape, Shape::Paused, "the lock never changes the state");
        let s = build(&Facts {
            profile_enabled: false,
            session_locked: true,
            ..facts(State::Off)
        });
        assert_eq!(
            s.attention, None,
            "an engine that is off runs no rule anyway"
        );
        let long = build(&Facts {
            mode: Some(Indicator {
                label: "x".repeat(200),
                ..nav(true)
            }),
            rules_kept_previous: true,
            ..facts(State::Running)
        });
        assert_eq!(long.tooltip.chars().count(), 127);
    }

    #[test]
    fn the_floating_badge_is_opt_in() {
        let s = build(&facts(State::Paused));
        assert!(!s.floating.visible, "off by default");
        assert_eq!(s.floating.setting, FloatingIndicator::Off);
        for (setting, state, visible) in [
            (FloatingIndicator::Paused, State::Paused, true),
            (FloatingIndicator::Paused, State::Running, false),
            (FloatingIndicator::Paused, State::Off, false),
            (FloatingIndicator::Always, State::Off, true),
            (FloatingIndicator::Always, State::Running, true),
            (FloatingIndicator::Off, State::Paused, false),
        ] {
            let s = build(&Facts {
                floating: Some(setting),
                mode: Some(nav(true)),
                ..facts(state)
            });
            assert_eq!(s.floating.visible, visible, "{setting:?} {state:?}");
        }
        let s = build(&Facts {
            floating: Some(FloatingIndicator::Always),
            mode: Some(nav(true)),
            ..facts(State::Running)
        });
        assert_eq!(s.floating.label, "Ddalgak on · N Navigation");
        let j = s.to_json();
        assert_eq!(j["shape"], "disc_notched");
        assert_eq!(j["floating"]["setting"], "always");
    }
}
