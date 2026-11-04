//! Truthful sync with the running engine (docs/DDALGAK_GUI_JOURNEYS.md §2 status line,
//! `DDALGAK-FR-052`, ADR 0049).
//!
//! The editor never says a rule is saved before the store wrote it, nor active before the
//! running app confirmed it loaded that revision:
//!
//! 1. After every save (and whenever the folder watcher sees `rules.json` change, e.g. by the
//!    CLI), the editor asks the app `rules.reload` over the identity-checked pipe, exactly like
//!    the CLI after a write. The app re-reads the file, applies it when it is usable and answers
//!    with the saved and the active revision and the engine state.
//! 2. The status line then says **saved and active (revision N)** only when the app's active
//!    revision is N and the engine runs; *saved and loaded; the engine is off/paused*;
//!    *saved; Ddalgak is not running*; *saved; Ddalgak kept its previous rules* (with the
//!    reason); or *saved; Ddalgak did not confirm the reload*.
//!
//! IPC runs on one worker thread, so the window never waits for the pipe; a reply wakes the UI.
//! Nothing polls: requests are sent only after a save, a file change, an engine button or the
//! window opening.

use ddalgak::ipc::Method;
use ddalgak::lifecycle::Request;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Why a call to the app failed (`engine_not_running`, `app_unresponsive`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

/// Something that answers the CLI ↔ app protocol: the real pipe client, or a fake in tests.
pub trait Engine: Send + Sync {
    fn call(&self, method: &Method, dry_run: bool) -> Result<Value, CallError>;
}

/// The running app behind its identity-checked pipe (`ddalgak::win::client`).
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct PipeEngine {
    pub endpoint: String,
}

#[cfg(windows)]
impl Engine for PipeEngine {
    fn call(&self, method: &Method, dry_run: bool) -> Result<Value, CallError> {
        ddalgak::win::client::call(&self.endpoint, method, dry_run)
            .map(|(v, _)| v)
            .map_err(|e| CallError {
                code: e.code,
                message: e.message,
            })
    }
}

/// No app at all (other platforms; tests of the "not running" state).
#[derive(Debug, Clone, Copy)]
pub struct NoEngine;

impl Engine for NoEngine {
    fn call(&self, _: &Method, _: bool) -> Result<Value, CallError> {
        Err(CallError {
            code: "engine_not_running".into(),
            message: "Ddalgak is not running".into(),
        })
    }
}

/// A request for the worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Status,
    /// Ask the app to reload after the file reached `revision` (a save or a seen change).
    Reload {
        revision: u64,
    },
    Engine(Request),
}

/// A worker reply.
#[derive(Clone, Debug)]
pub struct Reply {
    pub job: Job,
    pub result: Result<Value, CallError>,
}

/// The worker thread and its queues.
pub struct Link {
    tx: Sender<Job>,
    rx: Receiver<Reply>,
    inflight: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("inflight", &self.inflight.load(Ordering::SeqCst))
            .finish()
    }
}

/// Wakes the UI when a reply arrives (`egui::Context::request_repaint`).
pub type Wake = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

impl Link {
    pub fn new(engine: Arc<dyn Engine>, wake: Wake) -> Link {
        let (tx, jobs) = channel::<Job>();
        let (out, rx) = channel::<Reply>();
        let inflight = Arc::new(AtomicUsize::new(0));
        let n = Arc::clone(&inflight);
        std::thread::Builder::new()
            .name("ddalgak-editor-ipc".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let result = match &job {
                        Job::Status => engine.call(&Method::Status, false),
                        Job::Reload { .. } => engine.call(&Method::RulesReload, false),
                        Job::Engine(r) => engine.call(&Method::Engine(*r), false),
                    };
                    let _ = out.send(Reply { job, result });
                    n.fetch_sub(1, Ordering::SeqCst);
                    if let Ok(w) = wake.lock()
                        && let Some(w) = w.as_ref()
                    {
                        w();
                    }
                }
            })
            .expect("spawn the editor's IPC thread");
        Link { tx, rx, inflight }
    }

    pub fn send(&self, job: Job) {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(job).is_err() {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// The replies that arrived since the last call.
    pub fn poll(&self) -> Vec<Reply> {
        self.rx.try_iter().collect()
    }

    pub fn busy(&self) -> bool {
        self.inflight.load(Ordering::SeqCst) > 0
    }

    /// Waits until every request was answered (tests and the window's first frame budget).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        while self.busy() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

/// What the app said about itself.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum EngineView {
    /// Not asked yet.
    #[default]
    Unknown,
    NotRunning,
    Running {
        /// `off`, `running`, `paused`.
        state: String,
        /// The revision of the active rules (`None`: no usable rules loaded).
        active: Option<u64>,
        profile: Option<String>,
        mode: Option<String>,
    },
    /// The app answered with an error (`app_unresponsive`, `app_identity_mismatch`, …).
    Error {
        code: String,
        message: String,
    },
}

impl EngineView {
    /// Reads `status` or the `engine` part of any reply.
    pub fn from_reply(prev: &EngineView, job: &Job, r: &Result<Value, CallError>) -> EngineView {
        match r {
            Err(e) if e.code == "engine_not_running" => EngineView::NotRunning,
            Err(e) => EngineView::Error {
                code: e.code.clone(),
                message: e.message.clone(),
            },
            Ok(v) => {
                let (state, active, profile, mode) = match job {
                    Job::Status => (
                        v["engine"]["state"].as_str(),
                        v["rules"]["active_set"]["revision"].as_u64(),
                        v["profile"]["id"].as_str().map(str::to_string),
                        v["profile"]["mode_state"]["title"]
                            .as_str()
                            .or_else(|| v["profile"]["mode"].as_str())
                            .map(str::to_string),
                    ),
                    Job::Reload { .. } => (
                        v["engine"]["state"].as_str(),
                        v["active_set"]["revision"].as_u64(),
                        None,
                        None,
                    ),
                    Job::Engine(_) => (v["engine"]["state"].as_str(), None, None, None),
                };
                let (p_profile, p_mode, p_active) = match prev {
                    EngineView::Running {
                        profile,
                        mode,
                        active,
                        ..
                    } => (profile.clone(), mode.clone(), *active),
                    _ => (None, None, None),
                };
                EngineView::Running {
                    state: state.unwrap_or("off").to_string(),
                    active: if matches!(job, Job::Engine(_)) {
                        p_active
                    } else {
                        active
                    },
                    profile: profile.or(p_profile),
                    mode: mode.or(p_mode),
                }
            }
        }
    }

    /// The header's state label key.
    pub fn label_key(&self) -> &'static str {
        match self {
            EngineView::Unknown => "engine.unknown",
            EngineView::NotRunning | EngineView::Error { .. } => "engine.not_running",
            EngineView::Running { state, .. } => match state.as_str() {
                "running" => "engine.running",
                "paused" => "engine.paused",
                _ => "engine.off",
            },
        }
    }
}

/// Where the last saved change stands (the status line, `DDALGAK-FR-052`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SaveState {
    /// Nothing saved in this session yet; the file's revision is shown.
    #[default]
    Idle,
    /// Written; waiting for the app's answer.
    Waiting {
        revision: u64,
    },
    /// The app runs this revision and the engine is on.
    Active {
        revision: u64,
    },
    /// The app loaded this revision, but the engine is `off` or `paused`.
    Loaded {
        revision: u64,
        engine: String,
    },
    NotRunning {
        revision: u64,
    },
    /// The app could not use the file and kept its previous rules.
    KeptPrevious {
        revision: u64,
        reason: String,
    },
    /// The app did not confirm (an error, or it reports another revision).
    Unconfirmed {
        revision: u64,
        why: String,
    },
}

impl SaveState {
    /// The state after the app's answer to `Reload { revision }`.
    pub fn after_reload(revision: u64, r: &Result<Value, CallError>) -> SaveState {
        match r {
            Err(e) if e.code == "engine_not_running" => SaveState::NotRunning { revision },
            Err(e) => SaveState::Unconfirmed {
                revision,
                why: format!("{}: {}", e.code, e.message),
            },
            Ok(v) => {
                let active = v["active_set"]["revision"].as_u64();
                let saved = v["saved"]["revision"].as_u64();
                let engine = v["engine"]["state"].as_str().unwrap_or("off").to_string();
                if v["outcome"] == "kept_previous" {
                    SaveState::KeptPrevious {
                        revision,
                        reason: v["reason"]
                            .as_str()
                            .unwrap_or("the file is not usable")
                            .to_string(),
                    }
                } else if active == Some(revision) && v["in_sync"] == true {
                    if engine == "running" {
                        SaveState::Active { revision }
                    } else {
                        SaveState::Loaded { revision, engine }
                    }
                } else {
                    SaveState::Unconfirmed {
                        revision,
                        why: format!(
                            "the app reports saved revision {} and active revision {}",
                            saved.map_or_else(|| "none".into(), |s| s.to_string()),
                            active.map_or_else(|| "none".into(), |s| s.to_string())
                        ),
                    }
                }
            }
        }
    }

    /// The label key and the revision of the status line.
    pub fn label(&self) -> Option<(&'static str, u64)> {
        Some(match self {
            SaveState::Idle => return None,
            SaveState::Waiting { revision } => ("status.waiting", *revision),
            SaveState::Active { revision } => ("status.active", *revision),
            SaveState::Loaded { revision, engine } => (
                if engine == "paused" {
                    "status.loaded_paused"
                } else {
                    "status.loaded_off"
                },
                *revision,
            ),
            SaveState::NotRunning { revision } => ("status.not_running", *revision),
            SaveState::KeptPrevious { revision, .. } => ("status.kept_previous", *revision),
            SaveState::Unconfirmed { revision, .. } => ("status.unconfirmed", *revision),
        })
    }

    /// Whether the rules of `revision` run in the engine now.
    pub fn is_active(&self, revision: u64) -> bool {
        matches!(self, SaveState::Active { revision: r } if *r == revision)
    }
}

/// A fake app for tests: it reloads with the engine's own loader (`rules::load_with`, as the
/// tray app does) and answers in the app's reply shape. It can be stopped, made slow, or told to
/// answer for another revision.
#[derive(Debug)]
pub struct FakeEngine {
    pub rules: std::path::PathBuf,
    pub state: Mutex<FakeState>,
}

#[derive(Debug, Default, Clone)]
pub struct FakeState {
    pub running: bool,
    pub engine: String,
    pub active: Option<u64>,
    pub calls: Vec<String>,
    /// Delay before answering a reload (the engine's reload acknowledgement arrives later).
    pub delay_ms: u64,
}

impl FakeEngine {
    pub fn new(rules: std::path::PathBuf) -> FakeEngine {
        FakeEngine {
            rules,
            state: Mutex::new(FakeState {
                running: true,
                engine: "running".into(),
                ..FakeState::default()
            }),
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        f(&mut self.state.lock().expect("fake engine"))
    }
}

impl Engine for FakeEngine {
    fn call(&self, method: &Method, _dry_run: bool) -> Result<Value, CallError> {
        let (running, delay) = self.with(|s| {
            s.calls.push(method.name());
            (s.running, s.delay_ms)
        });
        if !running {
            return Err(CallError {
                code: "engine_not_running".into(),
                message: "Ddalgak is not running".into(),
            });
        }
        std::thread::sleep(Duration::from_millis(delay));
        let loaded = ddalgak::rules::load_with(&self.rules, &ddalgak::validate::Context::default());
        let usable = loaded.state.usable();
        self.with(|s| {
            if let Method::Engine(r) = method {
                s.engine = match r {
                    Request::Start | Request::Resume => "running",
                    Request::Pause => "paused",
                    Request::TogglePause if s.engine == "paused" => "running",
                    Request::TogglePause => "paused",
                    _ => "off",
                }
                .into();
            }
            if usable && matches!(method, Method::RulesReload | Method::Engine(Request::Start)) {
                s.active = loaded.revision;
            }
            let engine = serde_json::json!({ "state": s.engine });
            let active = s.active.map(|r| serde_json::json!({ "revision": r }));
            Ok(match method {
                Method::RulesReload => serde_json::json!({
                    "saved": { "revision": loaded.revision },
                    "active_set": active,
                    "in_sync": usable && s.active == loaded.revision,
                    "outcome": if usable { "applied" } else { "kept_previous" },
                    "reason": loaded.state.problem(),
                    "engine": engine,
                }),
                _ => serde_json::json!({
                    "engine": engine,
                    "rules": { "active_set": active },
                    "profile": { "id": loaded.profile, "mode": loaded.mode },
                }),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn saved_and_active_only_when_the_app_runs_that_revision() {
        let ok = |v: Value| Ok::<Value, CallError>(v);
        let reply = |active: u64, engine: &str, in_sync: bool| {
            ok(
                json!({ "saved": { "revision": 7 }, "active_set": { "revision": active },
                "in_sync": in_sync, "outcome": "applied", "engine": { "state": engine } }),
            )
        };
        assert_eq!(
            SaveState::after_reload(7, &reply(7, "running", true)),
            SaveState::Active { revision: 7 }
        );
        assert_eq!(
            SaveState::after_reload(7, &reply(7, "off", true)),
            SaveState::Loaded {
                revision: 7,
                engine: "off".into()
            }
        );
        assert!(matches!(
            SaveState::after_reload(7, &reply(6, "running", false)),
            SaveState::Unconfirmed { .. }
        ));
        assert!(!SaveState::after_reload(7, &reply(6, "running", false)).is_active(7));
        let kept = ok(
            json!({ "outcome": "kept_previous", "reason": "not valid JSON", "active_set": { "revision": 6 } }),
        );
        assert_eq!(
            SaveState::after_reload(7, &kept),
            SaveState::KeptPrevious {
                revision: 7,
                reason: "not valid JSON".into()
            }
        );
        let gone = Err(CallError {
            code: "engine_not_running".into(),
            message: String::new(),
        });
        assert_eq!(
            SaveState::after_reload(7, &gone),
            SaveState::NotRunning { revision: 7 }
        );
        let odd = Err(CallError {
            code: "app_identity_mismatch".into(),
            message: "x".into(),
        });
        assert!(matches!(
            SaveState::after_reload(7, &odd),
            SaveState::Unconfirmed { .. }
        ));
    }

    #[test]
    fn the_worker_answers_off_the_ui_thread_and_wakes_it() {
        let woken = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&woken);
        let wake: Wake = Arc::new(Mutex::new(Some(Box::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }))));
        let link = Link::new(Arc::new(NoEngine), wake);
        link.send(Job::Status);
        assert!(link.wait_idle(Duration::from_secs(5)));
        let r = link.poll();
        assert_eq!(r.len(), 1);
        assert_eq!(
            EngineView::from_reply(&EngineView::Unknown, &r[0].job, &r[0].result),
            EngineView::NotRunning
        );
        assert_eq!(woken.load(Ordering::SeqCst), 1);
    }
}
