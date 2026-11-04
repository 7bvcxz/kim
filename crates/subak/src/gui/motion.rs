//! The window's calm motion (Step 20, DESIGN_SYSTEM §2.5 and §4.5): the active tab's marker
//! fades in when another tab is shown (`duration.base`, 120 ms) and a banner fades in rising
//! 4 px (`duration.slow`, 160 ms), both on `easing.standard` (cubic-bezier 0.2, 0, 0, 1).
//! Terminal content, pane layout and typing are never animated.
//!
//! **Reduced motion**: with Windows' "Show animations in Windows" off, or the setting
//! `appearance.motion = "reduced"`, every duration is 0 — the final state is drawn at once and
//! no timer runs. While a transition runs the window redraws on a short timer; when none runs
//! there is no timer at all, so an idle window costs nothing (`SUBAK-NFR-002`).
//!
//! Pure and portable: the window asks [`Motion::progress`] for each transition's eased progress
//! at a given instant.

use std::time::{Duration, Instant};

/// `duration.base`: the tab switch highlight.
pub const TAB_MS: u64 = 120;
/// `duration.slow`: a banner's fade and rise.
pub const BANNER_MS: u64 = 160;
/// How far a banner rises while it fades in, in logical pixels.
pub const RISE_PX: f32 = 4.0;
/// The redraw interval while a transition runs (about 60 frames a second).
pub const FRAME_MS: u32 = 16;

/// What moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum What {
    /// The active tab's lilac marker and fill.
    Tab,
    /// The banner over the pane.
    Banner,
}

impl What {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tab => "tab",
            Self::Banner => "banner",
        }
    }

    pub fn duration(self) -> Duration {
        Duration::from_millis(match self {
            Self::Tab => TAB_MS,
            Self::Banner => BANNER_MS,
        })
    }
}

/// The transitions running now.
#[derive(Debug, Clone, Default)]
pub struct Motion {
    /// Whether transitions run at all (false: reduced motion).
    pub enabled: bool,
    running: Vec<(What, Instant)>,
}

impl Motion {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            running: Vec::new(),
        }
    }

    /// Starts (or restarts) a transition at `now`. Returns its duration in milliseconds, 0 when
    /// motion is reduced (nothing runs).
    pub fn start(&mut self, what: What, now: Instant) -> u64 {
        self.running.retain(|(w, _)| *w != what);
        if !self.enabled {
            return 0;
        }
        self.running.push((what, now));
        what.duration().as_millis() as u64
    }

    /// The eased progress of `what` at `now`, from 0 to 1 (1 when it does not run).
    pub fn progress(&self, what: What, now: Instant) -> f32 {
        self.running
            .iter()
            .find(|(w, _)| *w == what)
            .map_or(1.0, |(_, t0)| {
                let d = what.duration().as_secs_f32();
                ease(now.saturating_duration_since(*t0).as_secs_f32() / d)
            })
    }

    /// Ends the transitions that are complete at `now`; returns whether any still runs.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.running
            .retain(|(w, t0)| now.saturating_duration_since(*t0) < w.duration());
        !self.running.is_empty()
    }

    /// The transitions that run, by name.
    pub fn active(&self) -> Vec<&'static str> {
        self.running.iter().map(|(w, _)| w.as_str()).collect()
    }

    pub fn is_running(&self) -> bool {
        !self.running.is_empty()
    }

    /// Stops everything at once (reduced motion turned on, the window closing).
    pub fn stop(&mut self) {
        self.running.clear();
    }
}

/// `easing.standard`: cubic-bezier(0.2, 0, 0, 1) at time `t` (clamped to 0–1), solved for x by
/// bisection (monotonic in x), then y.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 || t >= 1.0 {
        return t;
    }
    // Control points P1 = (0.2, 0), P2 = (0, 1).
    let (x1, y1, x2, y2) = (0.2f32, 0.0f32, 0.0f32, 1.0f32);
    let bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

/// `amount` (0–255) of a transition at `progress`.
pub fn mix_amount(progress: f32) -> u8 {
    (progress.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_easing_decelerates() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
        let mut last = 0.0;
        for i in 1..20 {
            let v = ease(i as f32 / 20.0);
            assert!(v > last, "monotonic");
            last = v;
        }
        // Decelerating: most of the way is covered early.
        assert!(ease(0.25) > 0.5, "{}", ease(0.25));
        assert!(ease(0.5) > 0.85, "{}", ease(0.5));
    }

    #[test]
    fn transitions_run_their_duration_and_stop() {
        let t0 = Instant::now();
        let mut m = Motion::new(true);
        assert_eq!(m.start(What::Tab, t0), TAB_MS);
        assert_eq!(m.start(What::Banner, t0), BANNER_MS);
        assert_eq!(m.progress(What::Tab, t0), 0.0);
        assert!(m.is_running());
        assert_eq!(m.active(), ["tab", "banner"]);
        let mid = t0 + Duration::from_millis(60);
        let p = m.progress(What::Tab, mid);
        assert!(p > 0.5 && p < 1.0, "{p}");
        assert!(m.tick(mid));
        let after_tab = t0 + Duration::from_millis(TAB_MS);
        assert!(m.tick(after_tab), "the banner still runs");
        assert_eq!(m.active(), ["banner"]);
        assert_eq!(m.progress(What::Tab, after_tab), 1.0);
        assert!(!m.tick(t0 + Duration::from_millis(BANNER_MS)));
        assert!(!m.is_running());
        // Restarting replaces the running one.
        m.start(What::Tab, t0);
        m.start(What::Tab, mid);
        assert_eq!(m.active(), ["tab"]);
        m.stop();
        assert!(!m.is_running());
    }

    #[test]
    fn reduced_motion_runs_nothing() {
        let t0 = Instant::now();
        let mut m = Motion::new(false);
        assert_eq!(m.start(What::Tab, t0), 0);
        assert_eq!(m.start(What::Banner, t0), 0);
        assert!(!m.is_running());
        assert_eq!(m.progress(What::Banner, t0), 1.0, "the final state at once");
        assert_eq!(mix_amount(1.0), 255);
        assert_eq!(mix_amount(0.0), 0);
    }
}
