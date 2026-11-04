//! Real-input attempts on a desktop that a person uses at the same time (Step 75).
//!
//! The real-input tests inject obscure keys (F13–F20) with `SendInput` while the maintainer keeps
//! working on the same PC. Physical input can legitimately change what the engine does during an
//! attempt:
//!
//! - a **held modifier** (Shift, Ctrl, Alt, Win) makes an exact trigger such as `F13` not match
//!   (DDALGAK_RULE_MODEL §3.2), so the rule under test never fires;
//! - **any key going down** decides an undecided dual role at once (§5.1), so a hold resolves
//!   before its tap time or a tap becomes a hold.
//!
//! An attempt is therefore judged together with the physical input the engine's own hook saw
//! during it (`Stats::physical`, non-injected events: `LLKHF_INJECTED` clear) and the modifiers
//! held around it. A failed attempt with physical input is **disturbed**: reported, cleaned up and
//! retried (at most [`ATTEMPTS`] times). A failed attempt without physical input is a real failure
//! and panics, so real bugs are never hidden. When every attempt was disturbed the step is
//! reported as **skipped (disturbed)**, never as verified.

#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Attempts per step before it is reported as skipped (disturbed).
pub const ATTEMPTS: u32 = 5;

/// Why an attempt did not verify its step.
#[derive(Debug)]
pub enum Fail {
    /// `SendInput` was refused (locked desktop, an elevated window has focus): ends the test's
    /// injection part with what was verified so far.
    Refused(String),
    /// An expectation did not hold.
    Failed(String),
    /// The attempt itself saw input that was not its own (for example another program's
    /// injected key between its events): retried like an attempt with physical input.
    Disturbed(String),
}

/// The result of one attempt.
pub type Step<T> = Result<T, Fail>;

/// `Ok(())` when `cond` holds, otherwise a failed attempt with `msg`.
pub fn check(cond: bool, msg: impl FnOnce() -> String) -> Step<()> {
    if cond {
        Ok(())
    } else {
        Err(Fail::Failed(msg()))
    }
}

/// `Ok(())` when `SendInput` accepted the events, otherwise a refused attempt at `step`.
pub fn sent(ok: bool, step: &str) -> Step<()> {
    if ok {
        Ok(())
    } else {
        Err(Fail::Refused(step.to_string()))
    }
}

/// Waits until `cond` holds, at most `ms`.
pub fn eventually(ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

#[allow(unsafe_code)]
fn down(vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    // SAFETY: reads the async state of one key; no pointers.
    (unsafe { GetAsyncKeyState(i32::from(vk)) } as u16) & 0x8000 != 0
}

/// The modifiers held right now. None of the real-input tests ever injects a modifier, so a held
/// one is a person's.
pub fn modifiers_held() -> Vec<&'static str> {
    [
        (0xA0, "LShift"),
        (0xA1, "RShift"),
        (0xA2, "LCtrl"),
        (0xA3, "RCtrl"),
        (0xA4, "LAlt"),
        (0xA5, "RAlt"),
        (0x5B, "LWin"),
        (0x5C, "RWin"),
    ]
    .into_iter()
    .filter(|(vk, _)| down(*vk))
    .map(|(_, name)| name)
    .collect()
}

/// Waits (at most 3 s) until no modifier is held, so an attempt starts on a quiet keyboard.
pub fn settle() -> bool {
    eventually(3000, || modifiers_held().is_empty())
}

/// What a step came to.
#[derive(Debug)]
pub enum Outcome<T> {
    /// An attempt verified the step.
    Verified(T),
    /// Every attempt failed while a person was typing; nothing was verified.
    Disturbed,
    /// `SendInput` was refused at this step.
    Refused(String),
}

impl<T> Outcome<T> {
    /// The verified value, printing why there is none.
    pub fn verified(self, what: &str) -> Option<T> {
        match self {
            Outcome::Verified(t) => Some(t),
            Outcome::Disturbed => None,
            Outcome::Refused(step) => {
                eprintln!(
                    "skipped (SendInput refused) at `{step}` ({what}): locked desktop or an \
                     elevated window has focus"
                );
                None
            }
        }
    }
}

/// Runs `attempt` until it verifies its step, retrying only attempts disturbed by physical input.
///
/// `physical` reads the number of physical (non-injected) input events the engine's hook has seen
/// so far; `reset` puts things back between attempts (releases the test's keys, lets rate limits
/// expire). A failed attempt without physical input and without a held modifier panics.
pub fn attempts<T>(
    what: &str,
    physical: impl FnMut() -> u64,
    attempt: impl FnMut(u32) -> Step<T>,
    reset: impl FnMut(),
) -> Outcome<T> {
    attempts_with(what, physical, modifiers_held, attempt, reset)
}

/// [`attempts`] with the probe of held modifiers as a parameter (the harness's own tests use a
/// fake one).
pub fn attempts_with<T>(
    what: &str,
    mut physical: impl FnMut() -> u64,
    mut modifiers_held: impl FnMut() -> Vec<&'static str>,
    mut attempt: impl FnMut(u32) -> Step<T>,
    mut reset: impl FnMut(),
) -> Outcome<T> {
    for n in 1..=ATTEMPTS {
        let quiet_start = eventually(3000, || modifiers_held().is_empty());
        let held_before = modifiers_held();
        let p0 = physical();
        let result = attempt(n);
        let foreign = physical().saturating_sub(p0);
        let held_after = modifiers_held();
        let disturbed = foreign > 0 || !quiet_start || !held_after.is_empty();
        let why = || {
            let mut held = held_before.clone();
            held.extend(held_after.iter().filter(|m| !held_before.contains(m)));
            if held.is_empty() {
                format!("{foreign} physical input events reached the hook")
            } else {
                format!(
                    "{foreign} physical input events reached the hook, modifiers held: {held:?}"
                )
            }
        };
        match result {
            Ok(t) => {
                if disturbed {
                    eprintln!(
                        "note: `{what}` verified in attempt {n} although disturbed ({})",
                        why()
                    );
                }
                return Outcome::Verified(t);
            }
            Err(Fail::Refused(step)) => return Outcome::Refused(step),
            Err(Fail::Failed(msg)) if !disturbed => {
                panic!("`{what}`, attempt {n} (no physical input during it): {msg}")
            }
            Err(Fail::Failed(msg) | Fail::Disturbed(msg)) => {
                eprintln!(
                    "disturbed: `{what}`, attempt {n} of {ATTEMPTS}: {msg}; {}. A person is \
                     using this desktop; trying again",
                    why()
                );
                reset();
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    eprintln!(
        "skipped (disturbed): `{what}` was not observed without concurrent physical input in \
         {ATTEMPTS} attempts; nothing about it is claimed as verified"
    );
    Outcome::Disturbed
}
