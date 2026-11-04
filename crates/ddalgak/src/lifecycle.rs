//! The engine lifecycle: `off`, `running`, `paused` (`DDALGAK-FR-002…004`, DDALGAK_SAFETY §4).
//!
//! A pure state machine plus the start policy and the crash/emergency markers. The host applies
//! the side effects of each [`Change`] (install the hook, release held keys, remove the hook).
//!
//! - The engine starts `off` unless the user chose `engine_on_start: on`; after an unclean exit
//!   or an emergency off it always starts `off` and says why (`DDALGAK-FR-003`).
//! - A rule can pause, resume or turn the engine off, **never on** (`DDALGAK-FR-004`).
//! - The emergency chord turns it off from any state (`DDALGAK-SEC-003`).
//! - Step 73: an explicit turn-off by the user (the tray, the CLI, a shortcut or a rule) is
//!   remembered (`engine.disabled`) until the user turns the engine on again, so a restart, a
//!   sign-out or a reboot never turns it back on by itself, not even with `engine_on_start: on`
//!   (DDALGAK_SAFETY §4). A turn-off by the app itself (quitting, the session ending, a lost
//!   hook) is not a user choice and is not remembered.

use std::fmt;
use std::path::{Path, PathBuf};

/// The engine state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Off,
    Running,
    Paused,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Off => "off",
            State::Running => "running",
            State::Paused => "paused",
        }
    }

    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "off" => State::Off,
            "running" => State::Running,
            "paused" => State::Paused,
            _ => return None,
        })
    }

    /// The hook exists in `running` and `paused` (`DDALGAK-FR-002`).
    pub fn has_hook(self) -> bool {
        self != State::Off
    }
}

/// What is asked of the engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    Start,
    Pause,
    Resume,
    TogglePause,
    Stop,
    Emergency,
}

impl Request {
    pub fn as_str(self) -> &'static str {
        match self {
            Request::Start => "start",
            Request::Pause => "pause",
            Request::Resume => "resume",
            Request::TogglePause => "toggle_pause",
            Request::Stop => "stop",
            Request::Emergency => "emergency",
        }
    }

    pub fn parse(s: &str) -> Option<Request> {
        Some(match s {
            "start" => Request::Start,
            "pause" => Request::Pause,
            "resume" => Request::Resume,
            "toggle_pause" => Request::TogglePause,
            "stop" => Request::Stop,
            "emergency" => Request::Emergency,
            _ => return None,
        })
    }
}

/// Who asked. Activity and status name the origin, never keys.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    /// The tray menu.
    Tray,
    /// `ddalgak engine …` through the local IPC.
    Cli,
    /// One of Ddalgak's own global shortcuts (`engine.toggle`, `engine.pause`).
    Shortcut(&'static str),
    /// An engine-control rule.
    Rule(String),
    /// The emergency chord.
    Emergency,
    /// `engine_on_start: on` at app start.
    Startup,
    /// The app is exiting.
    Shutdown,
    /// The session was locked or the user signed out.
    Session,
    /// Step 73: a hook callback took as long as Windows' `LowLevelHooksTimeout`, after which
    /// Windows removes a low-level hook silently; the engine turned itself off.
    HookLost,
}

impl Origin {
    pub fn as_str(&self) -> String {
        match self {
            Origin::Tray => "tray".into(),
            Origin::Cli => "cli".into(),
            Origin::Shortcut(a) => format!("shortcut:{a}"),
            Origin::Rule(id) => format!("rule:{id}"),
            Origin::Emergency => "emergency".into(),
            Origin::Startup => "startup".into(),
            Origin::Shutdown => "shutdown".into(),
            Origin::Session => "session".into(),
            Origin::HookLost => "hook_lost".into(),
        }
    }

    /// The user asked (tray, CLI, one of Ddalgak's shortcuts, a rule the user wrote): a turn-off
    /// from here is remembered across restarts (Step 73).
    pub fn is_user(&self) -> bool {
        matches!(
            self,
            Origin::Tray | Origin::Cli | Origin::Shortcut(_) | Origin::Rule(_)
        )
    }
}

/// A refused request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// No rule turns the engine on (`DDALGAK-FR-004`).
    RuleCannotStart,
    /// Pause or resume needs the engine on; resume never turns it on.
    EngineOff,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::RuleCannotStart => "rule_cannot_start",
            Refusal::EngineOff => "engine_off",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::RuleCannotStart => "a rule can pause or turn the engine off, never on",
            Refusal::EngineOff => "the engine is off; turn it on first (`ddalgak engine start`)",
        })
    }
}

/// The outcome of an accepted request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub from: State,
    pub to: State,
    pub request: Request,
}

impl Change {
    pub fn changed(&self) -> bool {
        self.from != self.to
    }

    /// The hook must be installed now.
    pub fn installs_hook(&self) -> bool {
        !self.from.has_hook() && self.to.has_hook()
    }

    /// The hook must be removed now.
    pub fn removes_hook(&self) -> bool {
        self.from.has_hook() && !self.to.has_hook()
    }

    /// Every key and button Ddalgak holds must be released now (`DDALGAK-SEC-005`): on pause,
    /// turn-off and emergency.
    pub fn releases_keys(&self) -> bool {
        self.request == Request::Emergency
            || (self.changed() && matches!(self.to, State::Off | State::Paused))
    }
}

/// Why the engine is off.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OffReason {
    /// Default at start.
    Default,
    /// The previous run ended without a clean stop (crash, killed, power loss).
    UncleanExit,
    /// The last stop was the emergency chord.
    Emergency,
    /// `--no-hotkeys` or `KIM_NO_HOTKEYS`.
    NoHotkeys,
    /// Started elevated: Ddalgak refuses to run its engine as administrator (DDALGAK_SAFETY §3).
    Elevated,
    /// Turned off by the user or a rule.
    Stopped,
    /// Step 73: the user turned the engine off last time and has not turned it on since; a
    /// restart keeps it off even with `engine_on_start: on`.
    Disabled,
    /// Step 73: a hook callback took as long as `LowLevelHooksTimeout`; Windows may have removed
    /// the hook silently, so the engine turned itself off and released everything.
    HookLost,
}

impl OffReason {
    pub fn as_str(self) -> &'static str {
        match self {
            OffReason::Default => "default",
            OffReason::UncleanExit => "unclean_exit",
            OffReason::Emergency => "emergency",
            OffReason::NoHotkeys => "no_hotkeys",
            OffReason::Elevated => "elevated",
            OffReason::Stopped => "stopped",
            OffReason::Disabled => "disabled",
            OffReason::HookLost => "hook_lost",
        }
    }
}

/// The state machine.
#[derive(Clone, Debug)]
pub struct Lifecycle {
    state: State,
    off_reason: Option<OffReason>,
    last: Option<(Request, String)>,
}

impl Lifecycle {
    pub fn new(reason: OffReason) -> Lifecycle {
        Lifecycle {
            state: State::Off,
            off_reason: Some(reason),
            last: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn off_reason(&self) -> Option<OffReason> {
        (self.state == State::Off)
            .then_some(self.off_reason)
            .flatten()
    }

    /// The last accepted request and its origin.
    pub fn last(&self) -> Option<&(Request, String)> {
        self.last.as_ref()
    }

    /// What `request` would do, without doing it (`--dry-run`).
    pub fn plan(&self, request: Request, origin: &Origin) -> Result<Change, Refusal> {
        let from = self.state;
        let to = match (request, from) {
            (Request::Start, _) if matches!(origin, Origin::Rule(_)) => {
                return Err(Refusal::RuleCannotStart);
            }
            (Request::Start, State::Off) => State::Running,
            (Request::Start, s) => s,
            (Request::Pause | Request::Resume | Request::TogglePause, State::Off) => {
                return Err(Refusal::EngineOff);
            }
            (Request::Pause, _) => State::Paused,
            (Request::Resume, _) => State::Running,
            (Request::TogglePause, State::Running) => State::Paused,
            (Request::TogglePause, _) => State::Running,
            (Request::Stop | Request::Emergency, _) => State::Off,
        };
        Ok(Change { from, to, request })
    }

    /// Applies `request`.
    pub fn apply(&mut self, request: Request, origin: &Origin) -> Result<Change, Refusal> {
        let change = self.plan(request, origin)?;
        if change.to == State::Off && (change.changed() || request == Request::Emergency) {
            self.off_reason = Some(if request == Request::Emergency {
                OffReason::Emergency
            } else if *origin == Origin::HookLost {
                OffReason::HookLost
            } else {
                OffReason::Stopped
            });
        }
        self.state = change.to;
        self.last = Some((request, origin.as_str()));
        Ok(change)
    }
}

/// Inputs of the start policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StartFacts {
    /// `settings.engine_on_start` is `on`.
    pub engine_on_start: bool,
    /// The run marker was left behind by the previous run.
    pub unclean_exit: bool,
    /// The emergency marker is present.
    pub emergency: bool,
    /// Step 73: the user turned the engine off last time (`engine.disabled`).
    pub disabled: bool,
    /// `--no-hotkeys` / `KIM_NO_HOTKEYS`.
    pub no_hotkeys: bool,
    /// The app runs elevated.
    pub elevated: bool,
}

/// Whether the app turns the engine on at start, or why not (`DDALGAK-FR-003`). The checks are
/// ordered by how much the user needs to know about them.
pub fn start_policy(f: StartFacts) -> Result<(), OffReason> {
    if f.elevated {
        Err(OffReason::Elevated)
    } else if f.emergency {
        Err(OffReason::Emergency)
    } else if f.unclean_exit {
        Err(OffReason::UncleanExit)
    } else if f.disabled {
        // Never silently re-enabled (Step 73): the user's last choice wins over the setting.
        Err(OffReason::Disabled)
    } else if f.no_hotkeys {
        Err(OffReason::NoHotkeys)
    } else if f.engine_on_start {
        Ok(())
    } else {
        Err(OffReason::Default)
    }
}

/// The run and emergency markers in Ddalgak's local data folder (DDALGAK_SAFETY §4). They hold
/// a timestamp only.
#[derive(Clone, Debug)]
pub struct Markers {
    dir: PathBuf,
}

/// Present while the engine is on; removed on every clean stop.
pub const RUN_MARKER: &str = "engine.running";
/// Written by the emergency off; removed when the user turns the engine on again.
pub const EMERGENCY_MARKER: &str = "engine.emergency";
/// Step 73: written when the user turns the engine off; removed when the user turns it on again.
pub const DISABLED_MARKER: &str = "engine.disabled";

/// What the markers said at app start.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Found {
    /// The run marker was left behind: the previous run did not stop cleanly.
    pub unclean_exit: bool,
    pub emergency: bool,
    /// The user turned the engine off last time.
    pub disabled: bool,
}

impl Markers {
    pub fn new(dir: impl Into<PathBuf>) -> Markers {
        Markers { dir: dir.into() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write(&self, name: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let at = crate::time::now_rfc3339();
        std::fs::write(self.path(name), format!("{at}\n"))
    }

    fn remove(&self, name: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn running(&self) -> bool {
        self.path(RUN_MARKER).exists()
    }

    pub fn emergency(&self) -> bool {
        self.path(EMERGENCY_MARKER).exists()
    }

    pub fn disabled(&self) -> bool {
        self.path(DISABLED_MARKER).exists()
    }

    /// The engine turned on: a later start without a clean stop sees an unclean exit.
    pub fn engine_on(&self) -> std::io::Result<()> {
        self.remove(EMERGENCY_MARKER)?;
        self.remove(DISABLED_MARKER)?;
        self.write(RUN_MARKER)
    }

    /// The engine turned off cleanly.
    pub fn engine_off(&self) -> std::io::Result<()> {
        self.remove(RUN_MARKER)
    }

    /// The user turned the engine off (Step 73): a restart keeps it off.
    pub fn engine_off_by_user(&self) -> std::io::Result<()> {
        self.write(DISABLED_MARKER)?;
        self.remove(RUN_MARKER)
    }

    /// The emergency chord turned the engine off.
    pub fn emergency_off(&self) -> std::io::Result<()> {
        self.write(EMERGENCY_MARKER)?;
        self.remove(RUN_MARKER)
    }

    /// At app start: reads the facts and clears the run marker (it described the previous run).
    pub fn take_start_facts(&self) -> Found {
        let found = Found {
            unclean_exit: self.running(),
            emergency: self.emergency(),
            disabled: self.disabled(),
        };
        let _ = self.remove(RUN_MARKER);
        found
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_follow_the_state_machine() {
        let mut l = Lifecycle::new(OffReason::Default);
        assert_eq!(l.state(), State::Off);
        assert_eq!(l.off_reason(), Some(OffReason::Default));
        assert_eq!(
            l.apply(Request::Pause, &Origin::Cli),
            Err(Refusal::EngineOff)
        );
        assert_eq!(
            l.apply(Request::Resume, &Origin::Cli),
            Err(Refusal::EngineOff),
            "resume never turns the engine on"
        );
        let c = l.apply(Request::Start, &Origin::Cli).unwrap();
        assert!(c.installs_hook() && !c.releases_keys());
        assert_eq!(l.off_reason(), None);
        let c = l.apply(Request::Start, &Origin::Tray).unwrap();
        assert!(!c.changed(), "start is idempotent");
        let c = l
            .apply(Request::Pause, &Origin::Shortcut("engine.pause"))
            .unwrap();
        assert!(
            c.releases_keys() && !c.removes_hook(),
            "pause keeps the hook"
        );
        assert_eq!(l.state(), State::Paused);
        let c = l
            .apply(Request::TogglePause, &Origin::Rule("r".into()))
            .unwrap();
        assert_eq!(c.to, State::Running);
        assert_eq!(l.last().unwrap().1, "rule:r");
        let c = l.apply(Request::Stop, &Origin::Rule("r".into())).unwrap();
        assert!(c.removes_hook() && c.releases_keys());
        assert_eq!(l.off_reason(), Some(OffReason::Stopped));
        assert!(!l.apply(Request::Stop, &Origin::Cli).unwrap().changed());
        // Step 73: a lost hook turns the engine off and says so; it is not a user turn-off.
        l.apply(Request::Start, &Origin::Cli).unwrap();
        let c = l.apply(Request::Stop, &Origin::HookLost).unwrap();
        assert!(c.removes_hook() && c.releases_keys());
        assert_eq!(l.off_reason(), Some(OffReason::HookLost));
        assert_eq!(l.last().unwrap().1, "hook_lost");
        for (o, user) in [
            (Origin::Tray, true),
            (Origin::Cli, true),
            (Origin::Shortcut("engine.toggle"), true),
            (Origin::Rule("r".into()), true),
            (Origin::Shutdown, false),
            (Origin::Session, false),
            (Origin::HookLost, false),
            (Origin::Startup, false),
            (Origin::Emergency, false),
        ] {
            assert_eq!(o.is_user(), user, "{o:?}");
        }
    }

    #[test]
    fn no_rule_turns_the_engine_on() {
        let mut l = Lifecycle::new(OffReason::Default);
        assert_eq!(
            l.apply(Request::Start, &Origin::Rule("x".into())),
            Err(Refusal::RuleCannotStart)
        );
        assert_eq!(l.state(), State::Off);
        l.apply(Request::Start, &Origin::Cli).unwrap();
        // Even when already on, a rule's start is refused (it is never a valid rule step).
        assert!(l.plan(Request::Start, &Origin::Rule("x".into())).is_err());
    }

    #[test]
    fn emergency_turns_off_from_any_state_and_is_remembered() {
        for start in [None, Some(Request::Start), Some(Request::Pause)] {
            let mut l = Lifecycle::new(OffReason::Default);
            if let Some(r) = start {
                l.apply(Request::Start, &Origin::Cli).unwrap();
                l.apply(r, &Origin::Cli).unwrap();
            }
            let c = l.apply(Request::Emergency, &Origin::Emergency).unwrap();
            assert!(c.releases_keys(), "emergency always releases held keys");
            assert_eq!(l.state(), State::Off);
            assert_eq!(l.off_reason(), Some(OffReason::Emergency));
        }
    }

    #[test]
    fn plan_changes_nothing() {
        let l = Lifecycle::new(OffReason::Default);
        let c = l.plan(Request::Start, &Origin::Cli).unwrap();
        assert_eq!((c.from, c.to), (State::Off, State::Running));
        assert_eq!(l.state(), State::Off);
    }

    #[test]
    fn start_policy_prefers_off() {
        let on = StartFacts {
            engine_on_start: true,
            ..StartFacts::default()
        };
        assert_eq!(start_policy(on), Ok(()));
        assert_eq!(start_policy(StartFacts::default()), Err(OffReason::Default));
        for (facts, reason) in [
            (
                StartFacts {
                    unclean_exit: true,
                    ..on
                },
                OffReason::UncleanExit,
            ),
            (
                StartFacts {
                    emergency: true,
                    unclean_exit: true,
                    ..on
                },
                OffReason::Emergency,
            ),
            (
                StartFacts {
                    no_hotkeys: true,
                    ..on
                },
                OffReason::NoHotkeys,
            ),
            (
                StartFacts {
                    elevated: true,
                    ..on
                },
                OffReason::Elevated,
            ),
            // Step 73: never silently re-enabled after the user turned it off.
            (
                StartFacts {
                    disabled: true,
                    ..on
                },
                OffReason::Disabled,
            ),
            (
                StartFacts {
                    disabled: true,
                    unclean_exit: true,
                    ..on
                },
                OffReason::UncleanExit,
            ),
        ] {
            assert_eq!(start_policy(facts), Err(reason), "{facts:?}");
        }
    }

    #[test]
    fn markers_record_unclean_exits_and_emergencies() {
        let dir = std::env::temp_dir().join(format!(
            "ddalgak-markers-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let m = Markers::new(&dir);
        let none = Found::default();
        let f = |unclean_exit, emergency, disabled| Found {
            unclean_exit,
            emergency,
            disabled,
        };
        assert_eq!(m.take_start_facts(), none);
        m.engine_on().unwrap();
        // The process "crashes" here: the next start sees the marker once.
        assert_eq!(m.take_start_facts(), f(true, false, false));
        assert_eq!(m.take_start_facts(), none);
        m.engine_on().unwrap();
        m.engine_off().unwrap();
        assert_eq!(m.take_start_facts(), none);
        m.engine_on().unwrap();
        m.emergency_off().unwrap();
        assert_eq!(m.take_start_facts(), f(false, true, false));
        assert!(
            m.emergency(),
            "the emergency marker stays until the engine is turned on"
        );
        m.engine_on().unwrap();
        assert!(!m.emergency());
        // Step 73: the user's turn-off is remembered over any number of restarts (a reboot)
        // until the user turns the engine on again; quitting is no turn-off.
        m.engine_off_by_user().unwrap();
        assert_eq!(m.take_start_facts(), f(false, false, true));
        assert_eq!(m.take_start_facts(), f(false, false, true));
        m.engine_on().unwrap();
        assert!(!m.disabled());
        m.engine_off().unwrap();
        assert_eq!(m.take_start_facts(), none);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
