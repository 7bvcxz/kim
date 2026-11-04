//! The worker thread: everything that happens after the hook decided (ADR 0024 §3).
//!
//! The hook callback only pushes a small [`Signal`] into a bounded queue (`try_send`; a full
//! queue drops the signal and counts it, it never blocks the callback). The worker runs the
//! fired rules' steps, times `hold` triggers, and records outcomes in the [`Activity`] list.
//! It sleeps in a blocking receive; a timeout exists only while a hold trigger is down, so an
//! idle engine has no timer (`DDALGAK-NFR-001`).
//!
//! Engine-control steps are handed to the controller (the host's main thread), which owns the
//! lifecycle; the worker never changes the engine state itself.

use crate::core::Signal;
use crate::stats::Stats;
use crate::table::{Action, Binding, EngineOp};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Capacity of the hook → worker queue. A burst larger than this is an event storm.
pub const QUEUE: usize = 1024;

/// Most activity entries kept in memory (DDALGAK_RULE_MODEL §12).
pub const ACTIVITY_MAX: usize = 200;

/// One activity entry: rule ID and outcome code only (`DDALGAK-SEC-001`, `DDALGAK-SEC-011`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub at: String,
    pub rule: String,
    pub profile: String,
    pub mode: String,
    pub outcome: String,
}

/// The recent outcomes, newest last.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    entries: VecDeque<Entry>,
}

impl Activity {
    pub fn push(&mut self, e: Entry) {
        if self.entries.len() == ACTIVITY_MAX {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last `n` entries as JSON.
    pub fn to_json(&self, n: usize) -> Value {
        let skip = self.entries.len().saturating_sub(n);
        Value::Array(
            self.entries
                .iter()
                .skip(skip)
                .map(|e| {
                    json!({ "at": e.at, "rule": e.rule, "profile": e.profile, "mode": e.mode,
                            "outcome": e.outcome })
                })
                .collect(),
        )
    }
}

/// What the worker asks the controller to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Control {
    /// An engine-control step of a rule.
    Engine { op: EngineOp, rule: String },
    /// The emergency chord fired.
    Emergency,
}

/// The rules the worker runs, with the generation the hook's table has.
#[derive(Clone, Debug)]
pub struct Rules {
    pub generation: u32,
    pub bindings: Arc<Vec<Binding>>,
    pub profile: String,
    pub mode: String,
}

/// Messages to the worker.
#[derive(Debug)]
pub enum Msg {
    Signal {
        generation: u32,
        signal: Signal,
    },
    Rules(Rules),
    /// Cancel timers (pause, stop, profile switch).
    Cancel,
    Stop,
}

/// The sending end the hook uses: never blocks.
#[derive(Clone, Debug)]
pub struct Queue {
    tx: SyncSender<Msg>,
    stats: Arc<Stats>,
}

impl Queue {
    /// Pushes a signal from the hook callback. A full queue drops it (counted).
    pub fn signal(&self, generation: u32, signal: Signal) {
        match self.tx.try_send(Msg::Signal { generation, signal }) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Control messages from the host (may wait briefly when the queue is full).
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }
}

/// The running worker; stops and joins on drop.
#[derive(Debug)]
pub struct Worker {
    queue: Queue,
    join: Option<std::thread::JoinHandle<()>>,
    pub activity: Arc<Mutex<Activity>>,
}

type Sink = Box<dyn Fn(Control) + Send>;

impl Worker {
    /// Starts the worker. `control` receives engine-control requests (and must wake the
    /// controller); `activity` is shared with status reporting.
    pub fn start(stats: Arc<Stats>, control: Sink, activity: Arc<Mutex<Activity>>) -> Worker {
        let (tx, rx) = sync_channel::<Msg>(QUEUE);
        let act = Arc::clone(&activity);
        let st = Arc::clone(&stats);
        let join = std::thread::Builder::new()
            .name("ddalgak-worker".into())
            .spawn(move || run(&rx, &control, &act, &st))
            .expect("spawn the worker thread");
        Worker {
            queue: Queue { tx, stats },
            join: Some(join),
            activity,
        }
    }

    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.queue.send(Msg::Stop);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

struct Hold {
    generation: u32,
    binding: u16,
    due: Instant,
}

fn run(rx: &Receiver<Msg>, control: &Sink, activity: &Mutex<Activity>, stats: &Stats) {
    let mut rules: Option<Rules> = None;
    let mut holds: Vec<Hold> = Vec::new();
    loop {
        let msg = match holds.iter().map(|h| h.due).min() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(due) => rx.recv_timeout(due.saturating_duration_since(Instant::now())),
        };
        stats.worker_wakeups.fetch_add(1, Ordering::Relaxed);
        match msg {
            Err(RecvTimeoutError::Disconnected) | Ok(Msg::Stop) => break,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let due: Vec<Hold> = {
                    let (due, keep): (Vec<Hold>, Vec<Hold>) =
                        holds.drain(..).partition(|h| h.due <= now);
                    holds = keep;
                    due
                };
                for h in due {
                    fire(rules.as_ref(), h.generation, h.binding, control, activity);
                }
            }
            Ok(Msg::Rules(r)) => {
                holds.clear();
                rules = Some(r);
            }
            Ok(Msg::Cancel) => holds.clear(),
            Ok(Msg::Signal { generation, signal }) => match signal {
                Signal::Emergency => {
                    holds.clear();
                    control(Control::Emergency);
                }
                Signal::Fire { binding, .. } => {
                    fire(rules.as_ref(), generation, binding, control, activity);
                }
                Signal::Skipped { binding, code } => {
                    record(rules.as_ref(), generation, binding, code.as_str(), activity);
                }
                Signal::RateLimited { binding } => {
                    record(
                        rules.as_ref(),
                        generation,
                        binding,
                        "rate_limited",
                        activity,
                    );
                }
                Signal::HoldArmed { binding, hold_ms } => holds.push(Hold {
                    generation,
                    binding,
                    due: Instant::now() + Duration::from_millis(u64::from(hold_ms)),
                }),
                Signal::HoldCancelled { binding } => {
                    holds.retain(|h| !(h.binding == binding && h.generation == generation));
                }
            },
        }
    }
}

fn current(rules: Option<&Rules>, generation: u32, binding: u16) -> Option<(&Rules, &Binding)> {
    let r = rules.filter(|r| r.generation == generation)?;
    r.bindings.get(usize::from(binding)).map(|b| (r, b))
}

fn record(
    rules: Option<&Rules>,
    generation: u32,
    binding: u16,
    outcome: &str,
    activity: &Mutex<Activity>,
) {
    let Some((r, b)) = current(rules, generation, binding) else {
        return;
    };
    if let Ok(mut a) = activity.lock() {
        a.push(Entry {
            at: crate::time::now_rfc3339(),
            rule: b.rule.clone(),
            profile: r.profile.clone(),
            mode: r.mode.clone(),
            outcome: outcome.to_string(),
        });
    }
}

/// Runs a fired rule. Signals of an older table generation are dropped (a reload happened).
fn fire(
    rules: Option<&Rules>,
    generation: u32,
    binding: u16,
    control: &Sink,
    activity: &Mutex<Activity>,
) {
    let Some((_, b)) = current(rules, generation, binding) else {
        return;
    };
    match &b.action {
        Action::Engine(ops) => {
            for op in ops {
                control(Control::Engine {
                    op: *op,
                    rule: b.rule.clone(),
                });
            }
        }
    }
    record(rules, generation, binding, "ok", activity);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FireKind, Skip};
    use crate::table::{Injected, On};
    use crate::trigger::Trigger;
    use std::sync::mpsc::channel;

    fn rules(generation: u32) -> Rules {
        let b = |id: &str, op| Binding {
            rule: id.into(),
            trigger: Trigger::parse("F13").unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: false,
            action: Action::Engine(vec![op]),
        };
        Rules {
            generation,
            bindings: Arc::new(vec![
                b("pause-it", EngineOp::Pause),
                b("off-it", EngineOp::Disable),
            ]),
            profile: "default".into(),
            mode: "base".into(),
        }
    }

    fn worker() -> (Worker, Receiver<Control>, Arc<Stats>) {
        let (tx, rx) = channel();
        let tx = Mutex::new(tx);
        let stats = Arc::new(Stats::default());
        let w = Worker::start(
            Arc::clone(&stats),
            Box::new(move |c| {
                let _ = tx.lock().unwrap().send(c);
            }),
            Arc::new(Mutex::new(Activity::default())),
        );
        (w, rx, stats)
    }

    #[test]
    fn fired_rules_reach_the_controller_and_the_activity_list() {
        let (w, rx, _) = worker();
        let q = w.queue();
        q.send(Msg::Rules(rules(1)));
        q.signal(
            1,
            Signal::Fire {
                binding: 1,
                kind: FireKind::Press,
            },
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Engine {
                op: EngineOp::Disable,
                rule: "off-it".into()
            }
        );
        q.signal(
            1,
            Signal::Skipped {
                binding: 0,
                code: Skip::PasswordField,
            },
        );
        q.signal(1, Signal::RateLimited { binding: 0 });
        // A signal from an older table is dropped.
        q.signal(
            0,
            Signal::Fire {
                binding: 0,
                kind: FireKind::Press,
            },
        );
        q.signal(1, Signal::Emergency);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Emergency
        );
        let a = w.activity.lock().unwrap().clone();
        let outcomes: Vec<(&str, &str)> = a
            .entries()
            .map(|e| (e.rule.as_str(), e.outcome.as_str()))
            .collect();
        assert_eq!(
            outcomes,
            [
                ("off-it", "ok"),
                ("pause-it", "skipped_password_field"),
                ("pause-it", "rate_limited")
            ]
        );
        // Entries hold IDs and codes only.
        let j = a.to_json(10).to_string();
        assert!(!j.contains("F13"), "{j}");
    }

    #[test]
    fn holds_fire_after_their_time_unless_cancelled() {
        let (w, rx, _) = worker();
        let q = w.queue();
        q.send(Msg::Rules(rules(1)));
        q.signal(
            1,
            Signal::HoldArmed {
                binding: 0,
                hold_ms: 200,
            },
        );
        q.signal(1, Signal::HoldCancelled { binding: 0 });
        assert!(
            rx.recv_timeout(Duration::from_millis(400)).is_err(),
            "cancelled before 200 ms"
        );
        let t = Instant::now();
        q.signal(
            1,
            Signal::HoldArmed {
                binding: 0,
                hold_ms: 200,
            },
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            Control::Engine {
                op: EngineOp::Pause,
                rule: "pause-it".into()
            }
        );
        assert!(
            t.elapsed() >= Duration::from_millis(190),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn an_idle_worker_does_not_wake_up() {
        let (w, _rx, stats) = worker();
        w.queue().send(Msg::Rules(rules(1)));
        std::thread::sleep(Duration::from_millis(100));
        let before = stats.worker_wakeups.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            stats.worker_wakeups.load(Ordering::Relaxed),
            before,
            "no timer while idle"
        );
    }

    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        let stats = Arc::new(Stats::default());
        let (tx, _rx) = sync_channel::<Msg>(2);
        let q = Queue {
            tx,
            stats: Arc::clone(&stats),
        };
        let t = Instant::now();
        for _ in 0..10 {
            q.signal(1, Signal::Emergency);
        }
        assert!(t.elapsed() < Duration::from_millis(100));
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 8);
    }

    #[test]
    fn activity_is_capped() {
        let mut a = Activity::default();
        for i in 0..(ACTIVITY_MAX + 5) {
            a.push(Entry {
                at: String::new(),
                rule: format!("r{i}"),
                profile: String::new(),
                mode: String::new(),
                outcome: "ok".into(),
            });
        }
        assert_eq!(a.len(), ACTIVITY_MAX);
        assert_eq!(a.entries().next().unwrap().rule, "r5");
        assert_eq!(a.to_json(3).as_array().unwrap().len(), 3);
    }
}
