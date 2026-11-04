//! Engine counters for `ddalgak health` and the idle benchmark (ADR 0012: local only, never
//! sent anywhere). Counters hold numbers only: no keys, no rule content (`DDALGAK-SEC-001`).

use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Upper bounds (µs) of the callback-duration histogram buckets; the last bucket is open.
pub const BUCKETS_US: [u64; 7] = [10, 50, 100, 500, 1_000, 10_000, 100_000];

/// Shared, lock-free counters updated by the hook callback, the hook thread, the worker and the
/// host.
#[derive(Debug, Default)]
pub struct Stats {
    /// Hook callbacks (one per key event the hook saw).
    pub callbacks: AtomicU64,
    /// Events the hook consumed.
    pub consumed: AtomicU64,
    /// Events injected by Ddalgak itself (passed on, never matched).
    pub own_injected: AtomicU64,
    /// Events injected by other programs.
    pub other_injected: AtomicU64,
    /// Step 75: events from a device (not injected: `LLKHF_INJECTED` clear), counted only; no
    /// key is kept. The real-input tests use it to tell an attempt a person typed into apart
    /// from a failed one.
    pub physical: AtomicU64,
    /// Signals dropped because the worker queue was full (an event storm).
    pub dropped: AtomicU64,
    /// Callbacks that panicked (the event was passed on).
    pub panics: AtomicU64,
    /// Longest callback, µs.
    pub max_us: AtomicU64,
    /// Callback duration histogram (see [`BUCKETS_US`]).
    pub buckets: [AtomicU64; 8],
    /// Events that reached the callback more than 200 ms after they happened (queueing delay;
    /// Windows removes a hook whose callbacks take longer than `LowLevelHooksTimeout`).
    pub late: AtomicU64,
    /// Longest delay from event time to callback, ms.
    pub max_delay_ms: AtomicU64,
    /// Times the hook thread woke for a posted message (not counting callbacks).
    pub hook_wakeups: AtomicU64,
    /// Times the worker thread woke.
    pub worker_wakeups: AtomicU64,
    /// Times the host's main thread woke.
    pub host_wakeups: AtomicU64,
    /// Tick (ms) of the last callback.
    pub last_callback_tick: AtomicU32,
    /// Step 73: callbacks (or physical events' waits) that reached the hook timeout, after which
    /// Windows may have removed the hook (each turned the engine off).
    pub hook_timeouts: AtomicU64,
}

impl Stats {
    /// Records one callback's duration. Allocation-free.
    pub fn record_callback(&self, us: u64) {
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        let i = BUCKETS_US
            .iter()
            .position(|b| us <= *b)
            .unwrap_or(BUCKETS_US.len());
        self.buckets[i].fetch_add(1, Ordering::Relaxed);
    }

    /// Records how long an event waited before the callback saw it.
    pub fn record_delay(&self, ms: u64) {
        self.max_delay_ms.fetch_max(ms, Ordering::Relaxed);
        if ms > 200 {
            self.late.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The bucket bound below which at least 99 % of callbacks finished, µs (`None` without
    /// callbacks; `u64::MAX` when it is the open bucket).
    pub fn p99_us(&self) -> Option<u64> {
        let counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.load(Ordering::Relaxed))
            .collect();
        let total: u64 = counts.iter().sum();
        if total == 0 {
            return None;
        }
        let need = total - total / 100;
        let mut acc = 0;
        for (i, c) in counts.iter().enumerate() {
            acc += c;
            if acc >= need {
                return Some(BUCKETS_US.get(i).copied().unwrap_or(u64::MAX));
            }
        }
        Some(u64::MAX)
    }

    pub fn to_json(&self) -> Value {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        json!({
            "callbacks": l(&self.callbacks),
            "consumed": l(&self.consumed),
            "own_injected": l(&self.own_injected),
            "other_injected": l(&self.other_injected),
            "physical": l(&self.physical),
            "dropped_signals": l(&self.dropped),
            "panics": l(&self.panics),
            "callback_us": {
                "max": l(&self.max_us),
                "p99_at_most": self.p99_us().map(|v| if v == u64::MAX { Value::Null } else { json!(v) }),
                "histogram": BUCKETS_US.iter().map(|b| json!(format!("<={b}"))).chain([json!(">100000")])
                    .zip(self.buckets.iter().map(l))
                    .map(|(k, v)| json!({ "bucket_us": k, "count": v }))
                    .collect::<Vec<_>>(),
            },
            "late_events": l(&self.late),
            "max_delay_ms": l(&self.max_delay_ms),
            "hook_timeouts": l(&self.hook_timeouts),
            "wakeups": {
                "hook_thread": l(&self.hook_wakeups),
                "worker": l(&self.worker_wakeups),
                "host": l(&self.host_wakeups),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_and_p99() {
        let s = Stats::default();
        assert_eq!(s.p99_us(), None);
        for _ in 0..99 {
            s.record_callback(3);
        }
        s.record_callback(700);
        assert_eq!(s.p99_us(), Some(10));
        s.record_callback(700);
        assert_eq!(
            s.p99_us(),
            Some(1_000),
            "2 of 101 are slow: p99 is in their bucket"
        );
        assert_eq!(s.max_us.load(Ordering::Relaxed), 700);
        s.record_delay(5);
        s.record_delay(250);
        assert_eq!(s.late.load(Ordering::Relaxed), 1);
        let j = s.to_json();
        assert_eq!(j["callbacks"], 101);
        assert_eq!(j["callback_us"]["histogram"].as_array().unwrap().len(), 8);
    }
}
