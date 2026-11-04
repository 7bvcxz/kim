//! The engine core: one decision per key event (DDALGAK_RULE_MODEL §7.1, ADR 0024 §3).
//!
//! [`Core::decide`] runs inside the low-level hook callback. It is synchronous, allocation-free
//! and bounded by the table size, and it only decides two things: whether the event is consumed
//! and which [`Signal`] (if any) the worker thread gets. Running steps never happens here.
//!
//! Pipeline for one event:
//!
//! 1. **Emergency chord** on *physical* key state, before anything else (`DDALGAK-SEC-003`). The
//!    event passes on; the engine trips and lets everything through until the hook is removed.
//! 2. **Own input** (`LLKHF_INJECTED` plus Ddalgak's marker) passes untouched and is never
//!    matched, so remaps that point at each other are swaps, not loops (`DDALGAK-SEC-004`).
//! 3. **Consumed down, consumed up**: a key-up (and auto-repeat) is consumed exactly when its
//!    key-down was, so applications never see half a key press (`DDALGAK-SEC-005`).
//! 4. **Candidates** in precedence order; other programs' injected input only reaches rules with
//!    `injected: accept`; while paused only engine-control rules remain.
//! 5. **Stepping aside**: secure desktop or session lock, a password field, an IME composition,
//!    or (for injecting rules) an elevated foreground window skip the rule and let the key pass
//!    (`skipped_*`, `DDALGAK-SEC-002`, `DDALGAK-NFR-006`).
//! 6. **Rate limit** per rule (`max_runs_per_second`), which also stops storms of auto-repeat or
//!    of another program's injected input (`rate_limited`).
//!
//! The core keeps only what matching needs (keys down, one pending tap, hold or sequence step).
//! It records no keys (`DDALGAK-SEC-001`).

use crate::event::{KeyEvent, KeySet, Normalizer, RawKey, Source, Transition};
use crate::table::{CChord, Compiled, Injected, KeyMatch, On, Table};
use crate::vk;
use kim_hotkeys::keys::{Chord, Key};
use kim_hotkeys::scope::Scope;

/// Why a rule stepped aside (DDALGAK_RULE_MODEL §12).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Skip {
    Paused,
    PasswordField,
    SecureDesktop,
    ElevatedTarget,
    ImeComposition,
    InjectedInput,
}

impl Skip {
    pub fn as_str(self) -> &'static str {
        match self {
            Skip::Paused => "skipped_paused",
            Skip::PasswordField => "skipped_password_field",
            Skip::SecureDesktop => "skipped_secure_desktop",
            Skip::ElevatedTarget => "skipped_elevated_target",
            Skip::ImeComposition => "skipped_ime_composition",
            Skip::InjectedInput => "skipped_injected_input",
        }
    }
}

/// How a rule fired.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FireKind {
    Press,
    Repeat,
    Release,
    Sequence,
}

/// What the hook hands to the worker. Small and `Copy`: it goes through a bounded channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// The emergency chord: turn the engine off now.
    Emergency,
    Fire {
        binding: u16,
        kind: FireKind,
    },
    Skipped {
        binding: u16,
        code: Skip,
    },
    RateLimited {
        binding: u16,
    },
    /// A hold trigger went down: fire after `hold_ms` unless cancelled.
    HoldArmed {
        binding: u16,
        hold_ms: u32,
    },
    HoldCancelled {
        binding: u16,
    },
}

/// The context the hook sees at this moment (kept current by the host's watchers).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Flags {
    /// Session locked or the secure desktop is active.
    pub secure_desktop: bool,
    /// The focused control is a password field.
    pub password_field: bool,
    /// An IME composition is in progress.
    pub ime_composing: bool,
    /// The foreground window belongs to a process with a higher integrity level.
    pub elevated_foreground: bool,
    /// Changes whenever focus moves; a pending sequence is cancelled by focus loss.
    pub focus_epoch: u32,
}

/// The engine's running state as the hook sees it (the hook does not exist while off).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Running,
    Paused,
}

/// The result for one event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    pub consume: bool,
    pub signal: Option<Signal>,
    pub source: Source,
}

const NONE: u16 = u16::MAX;
const SEQ_WORDS: usize = 16; // 1024 bits ≥ MAX_BINDINGS

#[derive(Clone, Copy, Debug)]
struct Seq {
    step: u8,
    cand: [u64; SEQ_WORDS],
    epoch: u32,
}

fn bit(set: &[u64; SEQ_WORDS], i: usize) -> bool {
    set[i / 64] & (1 << (i % 64)) != 0
}

fn set_bit(set: &mut [u64; SEQ_WORDS], i: usize) {
    set[i / 64] |= 1 << (i % 64);
}

/// Per-rule rate limit: the times of the last `max` runs in a ring (preallocated at compile).
#[derive(Clone, Debug, Default)]
struct Rate {
    max: usize,
    times: Vec<u32>,
    len: Vec<u8>,
    head: Vec<u8>,
}

impl Rate {
    fn new(rules: usize, max: u32) -> Rate {
        let max = max.clamp(1, 100) as usize;
        Rate {
            max,
            times: vec![0; rules * max],
            len: vec![0; rules],
            head: vec![0; rules],
        }
    }

    /// Records a run at `now` unless the rule already ran `max` times in the last second.
    fn allow(&mut self, rule: usize, now: u32) -> bool {
        let Some(len) = self.len.get(rule).copied() else {
            return false;
        };
        let base = rule * self.max;
        let len = usize::from(len);
        if len == self.max {
            let oldest = self.times[base + usize::from(self.head[rule])];
            if now.wrapping_sub(oldest) < 1000 {
                return false;
            }
        }
        let slot = (usize::from(self.head[rule]) + len) % self.max;
        if len == self.max {
            self.times[base + usize::from(self.head[rule])] = now;
            self.head[rule] =
                u8::try_from((usize::from(self.head[rule]) + 1) % self.max).expect("max <= 100");
        } else {
            self.times[base + slot] = now;
            self.len[rule] = u8::try_from(len + 1).expect("max <= 100");
        }
        true
    }
}

/// The engine core. See the module documentation.
#[derive(Debug)]
pub struct Core {
    table: Table,
    norm: Normalizer,
    consumed: KeySet,
    owner: [u16; 256],
    release: Option<(u16, u16)>,
    hold: Option<(u16, u16)>,
    seq: Option<Seq>,
    rate: Rate,
    tripped: bool,
}

impl Core {
    pub fn new(table: Table) -> Core {
        let rate = Rate::new(table.len(), table.max_runs_per_second);
        Core {
            table,
            norm: Normalizer::new(),
            consumed: KeySet::default(),
            owner: [NONE; 256],
            release: None,
            hold: None,
            seq: None,
            rate,
            tripped: false,
        }
    }

    /// Replaces the rules (a reload). Pending taps, holds and sequences are dropped; keys whose
    /// down was consumed keep having their up consumed.
    pub fn set_table(&mut self, table: Table) {
        self.rate = Rate::new(table.len(), table.max_runs_per_second);
        self.table = table;
        self.owner = [NONE; 256];
        self.release = None;
        self.hold = None;
        self.seq = None;
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Whether the emergency chord fired; the core passes everything until it is rebuilt.
    pub fn tripped(&self) -> bool {
        self.tripped
    }

    /// Forgets the key state after input went to another desktop (lock screen, UAC,
    /// Ctrl+Alt+Del) or the session came back: key-ups made there never reached the hook. Keys
    /// whose down was consumed keep having their up consumed.
    pub fn reset_keys(&mut self) {
        self.norm.reset();
        self.release = None;
        self.hold = None;
        self.seq = None;
    }

    /// Drops pending taps, holds and sequences (pause, session lock).
    pub fn cancel_pending(&mut self) -> Option<Signal> {
        self.release = None;
        self.seq = None;
        self.hold
            .take()
            .map(|(binding, _)| Signal::HoldCancelled { binding })
    }

    fn pass(&self, ev: &KeyEvent, signal: Option<Signal>) -> Decision {
        Decision {
            consume: false,
            signal,
            source: ev.source,
        }
    }

    fn take(&mut self, ev: &KeyEvent, binding: usize, signal: Option<Signal>) -> Decision {
        self.consumed.insert(ev.vk);
        self.owner[usize::from(ev.vk & 0xFF)] = u16::try_from(binding).unwrap_or(NONE);
        Decision {
            consume: true,
            signal,
            source: ev.source,
        }
    }

    fn fire(&mut self, binding: usize, kind: FireKind, now: u32) -> Signal {
        let b = u16::try_from(binding).unwrap_or(NONE);
        if self.rate.allow(binding, now) {
            Signal::Fire { binding: b, kind }
        } else {
            Signal::RateLimited { binding: b }
        }
    }

    /// Whether the held modifiers plus this key form a chord reserved for triggers (a widened
    /// remap never captures one: with `Tab` remapped, `Alt+Tab` still switches windows).
    fn reserved_now(&self, ev: &KeyEvent) -> bool {
        let Some(name) = vk::vk_name(ev.vk, ev.extended) else {
            return false;
        };
        if name == "CapsLock" || name == "NumLock" {
            return false;
        }
        let chord = Chord {
            mods: self.norm.mods().as_mods(),
            key: Key::Named(name),
        };
        kim_hotkeys::reserved::chord(&chord, &Scope::Context(String::new()), false).is_some()
            || name == "F12"
    }

    fn chord_matches(&self, c: &CChord, ev: &KeyEvent, widen: bool) -> bool {
        let key_ok = match c.key {
            KeyMatch::Modifier(m) => {
                return ev.modifier == Some(m) && self.norm.mods().matches_without(&c.mods, m);
            }
            _ if ev.modifier.is_some() || ev.altgr_fake_ctrl => false,
            KeyMatch::Vk(v, ext) => ev.vk == v && ext.is_none_or(|e| e == ev.extended),
            KeyMatch::Scan(s) => ev.scan == s && !ev.extended,
        };
        if !key_ok {
            return false;
        }
        if widen && c.mods.is_empty() {
            return !self.reserved_now(ev);
        }
        self.norm.mods().matches(&c.mods)
    }

    fn guard(flags: &Flags) -> Option<Skip> {
        if flags.secure_desktop {
            Some(Skip::SecureDesktop)
        } else if flags.password_field {
            Some(Skip::PasswordField)
        } else if flags.ime_composing {
            Some(Skip::ImeComposition)
        } else {
            None
        }
    }

    /// Decides one event. Never allocates, never blocks.
    pub fn decide(&mut self, raw: &RawKey, mode: Mode, flags: &Flags) -> Decision {
        let ev = self.norm.normalize(raw);
        // 1. Emergency chord, on physical state only.
        if ev.source == Source::Physical
            && ev.transition == Transition::Down
            && self.norm.is_down_physical(vk::VK_ESCAPE)
            && self.norm.mods_physical().emergency_mods()
        {
            self.tripped = true;
            self.release = None;
            self.hold = None;
            self.seq = None;
            return self.pass(&ev, Some(Signal::Emergency));
        }
        if self.tripped {
            return self.pass(&ev, None);
        }
        // 2. Ddalgak's own input is invisible to rules.
        if ev.source == Source::Own {
            return self.pass(&ev, None);
        }
        let vk_i = usize::from(ev.vk & 0xFF);
        match ev.transition {
            // 3. Consumed down, consumed up.
            Transition::Up => {
                let consume = self.consumed.remove(ev.vk);
                self.owner[vk_i] = NONE;
                let mut signal = None;
                if let Some((b, k)) = self.release
                    && k == ev.vk
                {
                    self.release = None;
                    signal = Some(self.fire(usize::from(b), FireKind::Release, ev.time_ms));
                }
                if let Some((b, k)) = self.hold
                    && k == ev.vk
                {
                    self.hold = None;
                    signal = signal.or(Some(Signal::HoldCancelled { binding: b }));
                }
                Decision {
                    consume,
                    signal,
                    source: ev.source,
                }
            }
            Transition::Repeat => {
                if !self.consumed.contains(ev.vk) {
                    return self.pass(&ev, None);
                }
                let b = self.owner[vk_i];
                let signal = self
                    .table
                    .compiled
                    .get(usize::from(b))
                    .filter(|c| c.repeat && c.on == On::Press)
                    .map(|_| ())
                    .map(|()| self.fire(usize::from(b), FireKind::Repeat, ev.time_ms));
                Decision {
                    consume: true,
                    signal,
                    source: ev.source,
                }
            }
            Transition::Down => self.down(&ev, mode, flags),
        }
    }

    fn down(&mut self, ev: &KeyEvent, mode: Mode, flags: &Flags) -> Decision {
        // Any other key between down and up makes a release trigger not a tap.
        if let Some((_, k)) = self.release
            && k != ev.vk
        {
            self.release = None;
        }
        let guard = Self::guard(flags);
        if let Some(seq) = self.seq {
            if ev.modifier.is_some() || ev.altgr_fake_ctrl {
                return self.pass(ev, None); // modifiers form the next chord
            }
            if guard.is_some() || seq.epoch != flags.focus_epoch {
                self.seq = None;
            } else {
                return self.sequence_step(ev, seq);
            }
        }
        let paused = mode == Mode::Paused;
        let mut winner: Option<usize> = None;
        let mut skipped: Option<(usize, Skip)> = None;
        let mut seq_start = [0u64; SEQ_WORDS];
        let mut any_seq = false;
        for (i, c) in self.table.compiled.iter().enumerate() {
            if !self.chord_matches(&c.steps[0], ev, c.widen && c.len == 1) {
                continue;
            }
            if ev.source == Source::Other && c.injected == Injected::Ignore {
                skipped.get_or_insert((i, Skip::InjectedInput));
                continue;
            }
            if paused && !c.engine_control {
                skipped.get_or_insert((i, Skip::Paused));
                continue;
            }
            if c.len > 1 {
                if c.on == On::Press {
                    set_bit(&mut seq_start, i);
                    any_seq = true;
                }
                continue;
            }
            winner = Some(i);
            break;
        }
        let Some(i) = winner else {
            if any_seq {
                if let Some(s) = guard {
                    let first = (0..self.table.len()).find(|i| bit(&seq_start, *i));
                    let signal = first.map(|b| Signal::Skipped {
                        binding: u16::try_from(b).unwrap_or(NONE),
                        code: s,
                    });
                    return self.pass(ev, signal);
                }
                self.seq = Some(Seq {
                    step: 1,
                    cand: seq_start,
                    epoch: flags.focus_epoch,
                });
                self.consumed.insert(ev.vk);
                return Decision {
                    consume: true,
                    signal: None,
                    source: ev.source,
                };
            }
            let signal = skipped.map(|(b, code)| Signal::Skipped {
                binding: u16::try_from(b).unwrap_or(NONE),
                code,
            });
            return self.pass(ev, signal);
        };
        let c: Compiled = self.table.compiled[i];
        let b = u16::try_from(i).unwrap_or(NONE);
        // 5. Stepping aside: the key passes unchanged.
        if let Some(code) = guard {
            return self.pass(ev, Some(Signal::Skipped { binding: b, code }));
        }
        if flags.elevated_foreground && c.injects {
            return self.pass(
                ev,
                Some(Signal::Skipped {
                    binding: b,
                    code: Skip::ElevatedTarget,
                }),
            );
        }
        match c.on {
            On::Press => {
                let signal = self.fire(i, FireKind::Press, ev.time_ms);
                if c.pass_through {
                    self.pass(ev, Some(signal))
                } else {
                    self.take(ev, i, Some(signal))
                }
            }
            On::Release => {
                self.release = Some((b, ev.vk));
                // A bare modifier keeps working in chords: its down and up pass.
                if c.pass_through || matches!(c.steps[0].key, KeyMatch::Modifier(_)) {
                    self.pass(ev, None)
                } else {
                    self.take(ev, i, None)
                }
            }
            On::Hold(ms) => {
                self.hold = Some((b, ev.vk));
                let signal = Signal::HoldArmed {
                    binding: b,
                    hold_ms: ms,
                };
                if c.pass_through {
                    self.pass(ev, Some(signal))
                } else {
                    self.take(ev, i, Some(signal))
                }
            }
        }
    }

    fn sequence_step(&mut self, ev: &KeyEvent, seq: Seq) -> Decision {
        let swallow = |core: &mut Core, signal: Option<Signal>| {
            core.consumed.insert(ev.vk);
            Decision {
                consume: true,
                signal,
                source: ev.source,
            }
        };
        if ev.vk == vk::VK_ESCAPE && self.norm.mods().is_empty() {
            self.seq = None;
            return swallow(self, None);
        }
        let step = usize::from(seq.step);
        let mut next = [0u64; SEQ_WORDS];
        let mut complete: Option<usize> = None;
        let mut any = false;
        for (i, c) in self.table.compiled.iter().enumerate() {
            if !bit(&seq.cand, i) || usize::from(c.len) <= step {
                continue;
            }
            if self.chord_matches(&c.steps[step], ev, false) {
                if usize::from(c.len) == step + 1 {
                    complete.get_or_insert(i);
                } else {
                    set_bit(&mut next, i);
                    any = true;
                }
            }
        }
        if let Some(i) = complete {
            self.seq = None;
            let signal = self.fire(i, FireKind::Sequence, ev.time_ms);
            return swallow(self, Some(signal));
        }
        self.seq = any.then_some(Seq {
            step: seq.step + 1,
            cand: next,
            epoch: seq.epoch,
        });
        // A step that matches nothing cancels the sequence and is swallowed (HOTKEY_CONTRACT §5.3).
        swallow(self, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::OWN_MARKER;
    use crate::table::{Action, Binding, EngineOp};
    use crate::trigger::Trigger;
    use crate::vk::*;

    fn binding(rule: &str, keys: &str) -> Binding {
        Binding {
            rule: rule.into(),
            trigger: Trigger::parse(keys).unwrap(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: false,
            action: Action::Engine(vec![EngineOp::Pause]),
        }
    }

    /// A test driver: presses keys by name and returns the decisions.
    struct T {
        core: Core,
        mode: Mode,
        flags: Flags,
        t: u32,
    }

    fn vk_of(key: &str) -> (u16, u16, bool) {
        if let Some(m) = ModKey::from_name(key) {
            let scan = match m {
                ModKey::LCtrl | ModKey::RCtrl => 0x1D,
                ModKey::LAlt | ModKey::RAlt => 0x38,
                ModKey::LShift => 0x2A,
                ModKey::RShift => 0x36,
                ModKey::LWin => 0x5B,
                ModKey::RWin => 0x5C,
            };
            let ext = matches!(
                m,
                ModKey::RCtrl | ModKey::RAlt | ModKey::LWin | ModKey::RWin
            );
            return (m.vk(), scan, ext);
        }
        if let Some(code) = key.strip_prefix("code:") {
            let (s, e) = code_scan(code).unwrap();
            return (0, s, e);
        }
        (named_vk(key).unwrap(), 0, false)
    }

    impl T {
        fn new(bindings: Vec<Binding>) -> T {
            T {
                core: Core::new(Table::compile(bindings, 20)),
                mode: Mode::Running,
                flags: Flags::default(),
                t: 1000,
            }
        }

        fn raw(&mut self, key: &str, up: bool, source: Source) -> RawKey {
            let (vk, scan, extended) = vk_of(key);
            self.t += 10;
            RawKey {
                vk,
                scan,
                extended,
                up,
                injected: source != Source::Physical,
                extra_info: if source == Source::Own { OWN_MARKER } else { 0 },
                time_ms: self.t,
                ..RawKey::default()
            }
        }

        fn ev(&mut self, key: &str, up: bool) -> Decision {
            self.src(key, up, Source::Physical)
        }

        fn src(&mut self, key: &str, up: bool, source: Source) -> Decision {
            let raw = self.raw(key, up, source);
            self.core.decide(&raw, self.mode, &self.flags)
        }

        /// Logical key with a given layout's scan code.
        fn key_with_scan(&mut self, vk: u16, scan: u16, up: bool) -> Decision {
            self.t += 10;
            let raw = RawKey::physical(vk, scan, up, self.t);
            self.core.decide(&raw, self.mode, &self.flags)
        }

        fn down(&mut self, keys: &[&str]) -> Decision {
            let mut last = None;
            for k in keys {
                last = Some(self.ev(k, false));
            }
            last.unwrap()
        }

        fn up(&mut self, keys: &[&str]) -> Decision {
            let mut last = None;
            for k in keys.iter().rev() {
                last = Some(self.ev(k, true));
            }
            last.unwrap()
        }
    }

    fn fired(d: &Decision) -> Option<(u16, FireKind)> {
        match d.signal {
            Some(Signal::Fire { binding, kind }) => Some((binding, kind)),
            _ => None,
        }
    }

    #[test]
    fn exact_modifiers_and_consumed_pairs() {
        let mut t = T::new(vec![binding("a", "Ctrl+Shift+F13")]);
        let d = t.down(&["LCtrl", "LShift", "F13"]);
        assert!(d.consume);
        assert_eq!(fired(&d), Some((0, FireKind::Press)));
        // The key-up of a consumed down is consumed; modifiers always pass.
        assert!(t.ev("F13", true).consume);
        assert!(!t.ev("LShift", true).consume);
        // Ctrl+F13 (Shift missing) and Ctrl+Alt+Shift+F13 (extra Alt) do not match.
        assert!(!t.ev("F13", false).consume);
        t.ev("F13", true);
        let d = t.down(&["LAlt", "LShift", "F13"]);
        assert!(!d.consume && d.signal.is_none());
        t.up(&["LCtrl", "LAlt", "LShift", "F13"]);
        // Unmatched keys pass in both directions.
        assert!(!t.ev("F14", false).consume && !t.ev("F14", true).consume);
    }

    #[test]
    fn multiple_modifiers_and_sides() {
        let mut t = T::new(vec![
            binding("left", "LCtrl+RAlt+F13"),
            binding("right", "RCtrl+Shift+F14"),
        ]);
        assert_eq!(
            fired(&t.down(&["LCtrl", "RAlt", "F13"])),
            Some((0, FireKind::Press))
        );
        t.up(&["LCtrl", "RAlt", "F13"]);
        assert_eq!(
            fired(&t.down(&["RCtrl", "RAlt", "F13"])),
            None,
            "wrong Ctrl side"
        );
        t.up(&["RCtrl", "RAlt", "F13"]);
        assert_eq!(
            fired(&t.down(&["RCtrl", "RShift", "F14"])),
            Some((1, FireKind::Press))
        );
        t.up(&["RCtrl", "RShift", "F14"]);
        assert_eq!(
            fired(&t.down(&["RCtrl", "LShift", "F14"])),
            Some((1, FireKind::Press))
        );
        t.up(&["RCtrl", "LShift", "F14"]);
        assert_eq!(fired(&t.down(&["LCtrl", "LShift", "F14"])), None);
    }

    #[test]
    fn logical_and_physical_keys_follow_the_layout() {
        // `Ctrl+Z` is logical (virtual key Z); `Ctrl+code:KeyZ` is the key at scan 0x2C.
        let mut t = T::new(vec![
            binding("logical", "Ctrl+Z"),
            binding("physical", "Ctrl+code:KeyY"),
        ]);
        let z = u16::from(b'Z');
        let y = u16::from(b'Y');
        t.ev("LCtrl", false);
        // US QWERTY: the key at 0x2C is Z.
        assert_eq!(
            fired(&t.key_with_scan(z, 0x2C, false)),
            Some((0, FireKind::Press))
        );
        t.key_with_scan(z, 0x2C, true);
        // German QWERTZ: the key labeled Z sits at 0x15 (US Y position) and sends VK_Z, so the
        // logical rule follows the label, and the physical rule `code:KeyY` (scan 0x15) is
        // shadowed by precedence; the key at 0x2C sends VK_Y and matches neither.
        assert_eq!(
            fired(&t.key_with_scan(z, 0x15, false)),
            Some((0, FireKind::Press))
        );
        t.key_with_scan(z, 0x15, true);
        assert_eq!(fired(&t.key_with_scan(y, 0x2C, false)), None);
        t.key_with_scan(y, 0x2C, true);
        // With only the physical rule, VK_Y at 0x15 (US layout) matches by position.
        let mut p = T::new(vec![binding("physical", "Ctrl+code:KeyY")]);
        p.ev("LCtrl", false);
        assert_eq!(
            fired(&p.key_with_scan(y, 0x15, false)),
            Some((0, FireKind::Press))
        );
        p.key_with_scan(y, 0x15, true);
        assert_eq!(
            fired(&p.key_with_scan(z, 0x15, false)),
            Some((0, FireKind::Press))
        );
    }

    #[test]
    fn repeat_fires_only_with_repeat_true_and_is_rate_limited() {
        let mut once = binding("once", "F13");
        once.repeat = false;
        let mut rep = binding("rep", "F14");
        rep.repeat = true;
        let mut t = T::new(vec![once, rep]);
        assert!(fired(&t.ev("F13", false)).is_some());
        let r = t.ev("F13", false);
        assert!(
            r.consume && r.signal.is_none(),
            "repeat consumed, not fired"
        );
        t.ev("F13", true);
        assert!(fired(&t.ev("F14", false)).is_some());
        let mut fires = 1;
        let mut limited = 0;
        for _ in 0..30 {
            // 30 repeats within 300 ms
            match t.ev("F14", false).signal {
                Some(Signal::Fire {
                    kind: FireKind::Repeat,
                    ..
                }) => fires += 1,
                Some(Signal::RateLimited { .. }) => limited += 1,
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(fires, 20, "max_runs_per_second = 20");
        assert_eq!(limited, 11);
        t.ev("F14", true);
        t.t += 2000;
        assert!(fired(&t.ev("F14", false)).is_some(), "the window moved on");
    }

    #[test]
    fn release_fires_on_a_tap_and_bare_modifiers_keep_working() {
        let mut tap = binding("tap", "RAlt");
        tap.on = On::Release;
        let mut t = T::new(vec![tap]);
        let d = t.ev("RAlt", false);
        assert!(!d.consume, "a bare modifier's down passes");
        let u = t.ev("RAlt", true);
        assert!(!u.consume);
        assert_eq!(fired(&u), Some((0, FireKind::Release)));
        // RAlt used in a chord is not a tap.
        t.down(&["RAlt", "F13"]);
        let u = t.up(&["RAlt", "F13"]);
        assert_eq!(fired(&u), None);
        // While another modifier is held, a bare RAlt is not the trigger.
        t.down(&["LShift", "RAlt"]);
        assert_eq!(fired(&t.up(&["LShift", "RAlt"])), None);
    }

    #[test]
    fn hold_arms_and_cancels() {
        let mut h = binding("hold", "F15");
        h.on = On::Hold(500);
        let mut t = T::new(vec![h]);
        let d = t.ev("F15", false);
        assert!(d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::HoldArmed {
                binding: 0,
                hold_ms: 500
            })
        );
        assert!(
            t.ev("F15", false).consume,
            "repeats while held are consumed"
        );
        let u = t.ev("F15", true);
        assert!(u.consume);
        assert_eq!(u.signal, Some(Signal::HoldCancelled { binding: 0 }));
    }

    #[test]
    fn own_injected_input_is_never_matched_so_swaps_cannot_loop() {
        let mut t = T::new(vec![binding("a", "F13"), binding("b", "F14")]);
        for _ in 0..100 {
            let d = t.src("F13", false, Source::Own);
            assert!(!d.consume && d.signal.is_none());
            assert_eq!(d.source, Source::Own);
            t.src("F13", true, Source::Own);
        }
        // Other programs' input is skipped by `injected: ignore` rules ...
        let d = t.src("F14", false, Source::Other);
        assert!(!d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::Skipped {
                binding: 1,
                code: Skip::InjectedInput
            })
        );
        t.src("F14", true, Source::Other);
        // ... and reaches `injected: accept` rules, which are still rate-limited.
        let mut acc = binding("acc", "F16");
        acc.injected = Injected::Accept;
        let mut t = T::new(vec![acc]);
        let mut fires = 0;
        for _ in 0..50 {
            if let Some(Signal::Fire { .. }) = t.src("F16", false, Source::Other).signal {
                fires += 1;
            }
            t.src("F16", true, Source::Other);
        }
        assert_eq!(
            fires, 20,
            "a loop through another program is cut by the rate limit"
        );
    }

    #[test]
    fn emergency_chord_on_physical_keys_beats_every_rule() {
        let mut t = T::new(vec![binding("esc", "Escape")]);
        let d = t.down(&["LCtrl", "LAlt", "LShift", "Escape"]);
        assert_eq!(d.signal, Some(Signal::Emergency));
        assert!(!d.consume, "the chord itself passes");
        assert!(t.core.tripped());
        t.up(&["LCtrl", "LAlt", "LShift", "Escape"]);
        // After tripping, nothing is matched any more.
        assert!(t.ev("Escape", false).signal.is_none());
        // Any order counts (physical state), and injected modifiers do not.
        let mut t = T::new(vec![]);
        t.down(&["Escape", "RShift", "RCtrl"]);
        assert_eq!(t.ev("RAlt", false).signal, Some(Signal::Emergency));
        let mut t = T::new(vec![]);
        t.src("LCtrl", false, Source::Other);
        t.src("LAlt", false, Source::Other);
        t.src("LShift", false, Source::Other);
        assert_eq!(
            t.ev("Escape", false).signal,
            None,
            "injected modifiers never count"
        );
        let mut t = T::new(vec![]);
        t.down(&["LCtrl", "LAlt", "LShift"]);
        assert_eq!(
            t.src("Escape", false, Source::Other).signal,
            None,
            "an injected Escape never counts"
        );
    }

    #[test]
    fn decision_cost_with_1000_rules_stays_far_below_the_hook_budget() {
        // DDALGAK-NFR-002 (< 1 ms p99 in the callback) and DDALGAK-NFR-004 (load 1000 rules).
        let mods = [
            "",
            "Ctrl+",
            "Alt+",
            "Shift+",
            "Ctrl+Shift+",
            "Ctrl+Alt+",
            "Alt+Shift+",
            "Ctrl+Alt+Shift+",
        ];
        let keys: Vec<String> = (1..=24)
            .filter(|n| *n != 12)
            .map(|n| format!("F{n}"))
            .chain(('A'..='Z').map(|c| c.to_string()))
            .collect();
        let mut bindings = Vec::new();
        'outer: for m in mods {
            for k in &keys {
                if bindings.len() == 1000 {
                    break 'outer;
                }
                let t = format!("{m}{k}");
                if Trigger::parse(&t).is_ok() {
                    bindings.push(binding(&format!("r-{}", bindings.len()), &t));
                }
            }
        }
        while bindings.len() < 1000 {
            let n = bindings.len();
            bindings.push(binding(
                &format!("s-{n}"),
                &format!("Ctrl+K code:Key{}", (b'A' + (n % 26) as u8) as char),
            ));
        }
        let t = std::time::Instant::now();
        let table = Table::compile(bindings, 20);
        let compile = t.elapsed();
        let mut core = Core::new(table);
        let flags = Flags::default();
        // Worst case: a key no rule uses scans every binding.
        let raw_down = RawKey::physical(0x91, 0x46, false, 0); // ScrollLock
        let raw_up = RawKey::physical(0x91, 0x46, true, 0);
        let n = 20_000u32;
        let t = std::time::Instant::now();
        for _ in 0..n / 2 {
            assert!(!core.decide(&raw_down, Mode::Running, &flags).consume);
            core.decide(&raw_up, Mode::Running, &flags);
        }
        let per = t.elapsed() / n;
        eprintln!(
            "1000 rules: compile {compile:?}; decide (no match, full scan) {per:?} per event ({} build)",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        );
        assert!(
            per < std::time::Duration::from_micros(500),
            "{per:?} per event"
        );
        assert!(
            compile < std::time::Duration::from_millis(100),
            "{compile:?}"
        );
    }

    #[test]
    fn stepping_aside_and_paused() {
        let mut t = T::new(vec![binding("a", "F13")]);
        for (flags, code) in [
            (
                Flags {
                    password_field: true,
                    ..Flags::default()
                },
                Skip::PasswordField,
            ),
            (
                Flags {
                    secure_desktop: true,
                    ..Flags::default()
                },
                Skip::SecureDesktop,
            ),
            (
                Flags {
                    ime_composing: true,
                    ..Flags::default()
                },
                Skip::ImeComposition,
            ),
        ] {
            t.flags = flags;
            let d = t.ev("F13", false);
            assert!(!d.consume, "{code:?}: the key passes unchanged");
            assert_eq!(d.signal, Some(Signal::Skipped { binding: 0, code }));
            assert!(!t.ev("F13", true).consume);
        }
        // Elevated foreground: only injecting rules step aside.
        t.flags = Flags {
            elevated_foreground: true,
            ..Flags::default()
        };
        assert!(fired(&t.ev("F13", false)).is_some());
        t.ev("F13", true);
        let mut inj = binding("inj", "F14");
        inj.injects = true;
        inj.action = Action::Engine(vec![]);
        let mut e = T::new(vec![inj]);
        e.flags = t.flags;
        assert_eq!(
            e.ev("F14", false).signal,
            Some(Signal::Skipped {
                binding: 0,
                code: Skip::ElevatedTarget
            })
        );
    }

    #[test]
    fn paused_keeps_engine_control_rules_only() {
        let mut t = T::new(vec![binding("pause", "F13")]);
        t.mode = Mode::Paused;
        assert!(
            fired(&t.ev("F13", false)).is_some(),
            "engine-control rules work while paused"
        );
    }

    #[test]
    fn widened_matching_never_captures_reserved_chords() {
        let mut w = binding("tab", "Tab");
        w.widen = true;
        let mut t = T::new(vec![w]);
        assert!(
            fired(&t.down(&["LShift", "Tab"])).is_some(),
            "Shift+Tab widens"
        );
        t.up(&["LShift", "Tab"]);
        let d = t.down(&["LAlt", "Tab"]);
        assert!(
            !d.consume && d.signal.is_none(),
            "Alt+Tab still switches windows"
        );
        t.up(&["LAlt", "Tab"]);
        let mut f4 = binding("f4", "F4");
        f4.widen = true;
        let mut t = T::new(vec![f4]);
        assert!(
            !t.down(&["LAlt", "F4"]).consume,
            "Alt+F4 still closes the window"
        );
    }

    #[test]
    fn sequences_advance_cancel_and_swallow() {
        let mut t = T::new(vec![binding("seq", "Ctrl+K Ctrl+U"), binding("one", "F13")]);
        let d = t.down(&["LCtrl", "K"]);
        assert!(d.consume && d.signal.is_none(), "first step pending");
        t.ev("K", true);
        let d = t.ev("U", false);
        assert_eq!(fired(&d), Some((0, FireKind::Sequence)));
        assert!(d.consume);
        t.up(&["LCtrl", "U"]);
        // A wrong second step cancels and is swallowed; the next key is normal again.
        t.down(&["LCtrl", "K"]);
        t.up(&["LCtrl", "K"]);
        let d = t.ev("F14", false);
        assert!(d.consume && d.signal.is_none());
        t.ev("F14", true);
        assert!(fired(&t.ev("F13", false)).is_some());
        t.ev("F13", true);
        // Escape cancels; focus loss cancels.
        t.down(&["LCtrl", "K"]);
        t.up(&["LCtrl", "K"]);
        assert!(t.ev("Escape", false).consume);
        t.ev("Escape", true);
        t.down(&["LCtrl", "K"]);
        t.up(&["LCtrl", "K"]);
        t.flags.focus_epoch += 1;
        t.ev("LCtrl", false);
        let d = t.ev("U", false);
        assert_eq!(fired(&d), None, "focus moved: the sequence was cancelled");
    }

    #[test]
    fn key_state_is_forgotten_after_another_desktop() {
        // Win+L: the hook sees LWin and L go down, then the lock screen takes over and the ups
        // never arrive. Without a reset, every trigger would miss (Win is held "forever").
        let mut t = T::new(vec![binding("a", "Ctrl+Shift+F13")]);
        t.down(&["LWin"]);
        t.ev("L", false);
        assert!(
            fired(&t.down(&["LCtrl", "LShift", "F13"])).is_none(),
            "stale Win blocks"
        );
        t.up(&["LCtrl", "LShift", "F13"]);
        // EVENT_SYSTEM_DESKTOPSWITCH / unlock: the hook forgets the key state.
        t.core.reset_keys();
        assert_eq!(
            fired(&t.down(&["LCtrl", "LShift", "F13"])),
            Some((0, FireKind::Press))
        );
        t.up(&["LCtrl", "LShift", "F13"]);
        // Pairs survive: a consumed down's up is still consumed after a reset.
        assert!(t.down(&["LCtrl", "LShift", "F13"]).consume);
        t.core.reset_keys();
        assert!(t.ev("F13", true).consume);
    }

    #[test]
    fn reload_keeps_pairs_of_consumed_keys() {
        let mut t = T::new(vec![binding("a", "F13")]);
        assert!(t.ev("F13", false).consume);
        t.core.set_table(Table::compile(vec![], 20));
        assert!(
            t.ev("F13", true).consume,
            "the up of a consumed down stays consumed"
        );
        assert!(!t.ev("F13", false).consume, "the new table has no rule");
    }

    #[test]
    fn rate_ring_handles_many_rules() {
        let mut r = Rate::new(3, 2);
        assert!(r.allow(2, 0) && r.allow(2, 10) && !r.allow(2, 20));
        assert!(r.allow(0, 20));
        assert!(r.allow(2, 1001) && !r.allow(2, 1005) && r.allow(2, 1011));
        assert!(!r.allow(7, 0), "unknown rules never run");
    }
}
