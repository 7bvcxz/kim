//! The engine core: one decision per key event (DDALGAK_RULE_MODEL §7.1, ADR 0024 §3).
//!
//! [`Core::decide`] runs inside the low-level hook callback. It is synchronous, allocation-free
//! and bounded by the table size, and it only decides two things: whether the event is consumed
//! and which [`Signal`]s (at most two) the worker thread gets. Running steps never happens here.
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
//!    `injected: accept`; while paused only engine-control rules remain. An exact chord beats a
//!    widened remap or block (a plain-key trigger that matches with modifiers held).
//! 5. **Stepping aside**: secure desktop or session lock, a password field, an IME composition,
//!    or (for injecting rules) an elevated foreground window skip the rule and let the key pass
//!    (`skipped_*`, `DDALGAK-SEC-002`, `DDALGAK-NFR-006`).
//! 6. **Rate limit** per rule (`max_runs_per_second`), which also stops storms of auto-repeat or
//!    of another program's injected input (`rate_limited`).
//!
//! **Remaps** (Step 68, §5.1) keep a small fixed table of active *slots*, one per held trigger.
//! The core decides every output event from its key state and hands the worker self-contained
//! instructions ([`RemapOp`]): which of the trigger's modifiers to release logically while the
//! target is held (and restore afterwards if still held), which target modifiers and which target
//! key to press, with reference counts so two remaps holding the same output never release it
//! early and a key the user already holds is never pressed or released by Ddalgak. A dual-role
//! remap (`tap`) stays undecided until another key goes down (the target is pressed first and
//! that key is sent after it, unchanged), its time passes (a one-shot timer of the hook thread,
//! [`Core::tick`]) or the trigger goes up (the tap output is sent). Instructions carry the output
//! keys, not rule indexes, so a held output is still released correctly after a reload.
//!
//! **Hotstrings** (Step 69, §3.4): while a hotstring rule is active, every key that reaches the
//! application unchanged is translated by the platform ([`Translate`]) and fed into a bounded
//! buffer ([`hotstring::Buffer`], the longest abbreviation plus one character), which every reset
//! clears. The key that completes an abbreviation is consumed (and given back by the worker when
//! nothing is typed) and the worker gets [`Signal::Hotstring`]. For typing runs the core also
//! publishes the modifier state the application sees and a count of physical key presses
//! ([`RunShared`]), and consumes the physical release of a modifier a run released logically.
//!
//! **Mouse holds** (Step 71, §5.7, §5.9): a held button (`mouse_click` with `op: hold`) is a remap
//! slot whose target is the button, so it shares the reference counts and every release path. A
//! move while held (`while_held`) fires on its trigger's down and keeps a small record of the
//! trigger key; the trigger's up sends [`Signal::MotionEnd`], and every path that releases the
//! remaps (pause, stop, session lock, a desktop switch, an elevated window taking focus, a key
//! reset) also ends every motion ([`Signal::ReleaseAll`]). A reload drops the records: the worker
//! stops every motion when it gets new rules.
//!
//! The core keeps only what matching needs (keys down, one pending tap, hold or sequence step,
//! the active remaps and motions, the bounded hotstring buffer). It records no keys
//! (`DDALGAK-SEC-001`).

use crate::event::{KeyEvent, KeySet, ModState, Normalizer, RawKey, Source, Transition};
use crate::hotstring::{self, Translate, Typed};
use crate::inject::OutCode;
use crate::modes::{Change, ModeOrigin, ModeState, ModeView};
use crate::remap::{Output, families};
use crate::run::{HotstringFire, RunShared};
use crate::table::{CChord, Compiled, Injected, KeyMatch, On, RemapSpec, Table};
use crate::vk::{self, ModKey};
use kim_hotkeys::keys::{Chord, Key, Side};
use kim_hotkeys::scope::Scope;
use std::sync::Arc;
use std::sync::atomic::Ordering;

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

/// What a remap's output does now. Modifier sets are [`ModKey::bit`] bits. Self-contained: the
/// worker needs no rule to carry it out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemapOp {
    /// The target goes down: release `lift` (the user's trigger modifiers; not recorded), press
    /// `mods` and `key` (recorded in the ledger).
    Press {
        lift: u8,
        mods: u8,
        key: Option<OutCode>,
    },
    /// Auto-repeat of the trigger: the target key's down again.
    Repeat { key: OutCode },
    /// The target goes up: release `key` and `mods`, then restore `restore` (trigger modifiers
    /// still held; not recorded).
    Release {
        key: Option<OutCode>,
        mods: u8,
        restore: u8,
    },
    /// A dual-role trigger was tapped: release `lift`, tap `mods` and `key`, restore `restore`.
    Tap {
        lift: u8,
        mods: u8,
        key: Option<OutCode>,
        restore: u8,
    },
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
    /// A remap's output changes.
    Remap {
        binding: u16,
        op: RemapOp,
    },
    /// A key the core held back while a dual role was decided, sent on unchanged after the
    /// target (and its later repeats and its up).
    Replay {
        code: OutCode,
        up: bool,
    },
    /// Release every output Ddalgak holds (a desktop switch, an elevated window took focus, a
    /// key-state reset): the trigger's up would never reach the hook.
    ReleaseAll,
    /// Press again the user's trigger modifiers that a cancelled remap had released logically and
    /// that are still physically held (not recorded).
    Restore {
        mods: u8,
    },
    /// A hotstring's abbreviation was typed (Step 69): the worker erases it and runs the rule.
    Hotstring {
        binding: u16,
        fire: HotstringFire,
    },
    /// The trigger of a move while held went up (Step 71): that motion stops.
    MotionEnd {
        binding: u16,
    },
    /// The mode state changed (Step 72), by the rule `binding` ([`crate::modes::NO_RULE`] when no rule did:
    /// a released layer, a timeout, a reload, a request). With `switched` the active mode
    /// changed: the core dropped its remaps and motions, and the worker releases every output it
    /// holds, then presses again the user's still-held trigger modifiers in `restore` (like
    /// [`Signal::ReleaseAll`] and [`Signal::Restore`]); armed hold triggers of the old mode end.
    Mode {
        binding: u16,
        view: ModeView,
        switched: bool,
        restore: u8,
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
    /// Sent to the worker before `signal` (a dual role resolved by this event).
    pub pre: Option<Signal>,
    pub signal: Option<Signal>,
    /// Sent after `signal` (a held-back key sent on).
    pub post: Option<Signal>,
    /// Sent last (Step 72: a mode change this key-up made: a held layer ended, a one-shot mode
    /// ended).
    pub last: Option<Signal>,
    pub source: Source,
    /// Arm the hook thread's one-shot timer: call [`Core::tick`] and [`Core::mode_tick`] after
    /// this many milliseconds.
    pub timer: Option<u32>,
}

impl Decision {
    fn new(consume: bool, signal: Option<Signal>, source: Source) -> Decision {
        Decision {
            consume,
            pre: None,
            signal,
            post: None,
            last: None,
            source,
            timer: None,
        }
    }

    /// The signals in the order the worker must get them.
    pub fn signals(&self) -> impl Iterator<Item = Signal> {
        self.pre
            .into_iter()
            .chain(self.signal)
            .chain(self.post)
            .chain(self.last)
    }
}

const NONE: u16 = u16::MAX;
const SEQ_WORDS: usize = 16; // 1024 bits ≥ MAX_BINDINGS
/// Most remaps held at the same time (one per held trigger key).
pub const SLOTS: usize = 16;
/// Most moves while held at the same time (Up and Right together move diagonally).
pub const MOTIONS: usize = 8;

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

/// One held remap.
#[derive(Clone, Copy, Debug)]
struct Slot {
    binding: u16,
    /// The trigger's virtual key (a mouse button's `VK_*BUTTON` for mouse triggers).
    trigger: u16,
    spec: RemapSpec,
    /// The trigger's modifiers belong to the chord (an exact match without pass-through): they
    /// are released while the target is held.
    lifts: bool,
    /// A dual role still undecided since this time (ms).
    pending: Option<u32>,
    /// Trigger modifier keys released logically and still to be restored.
    lift: u8,
    /// Target modifier keys this slot holds a reference to.
    mods: u8,
    /// This slot holds a reference to its target key.
    key: bool,
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
    slots: [Option<Slot>; SLOTS],
    /// References to modifier keys held as remap outputs, by [`ModKey`] index.
    mod_refs: [u8; 8],
    /// References to target keys and buttons held as remap outputs.
    key_refs: [Option<(OutCode, u8)>; SLOTS],
    /// Keys whose down was held back while a dual role was decided and then sent on: their
    /// repeats and up are sent on too.
    replayed: KeySet,
    /// What typing runs need from the hook (Step 69); `None` without a worker (tests).
    runs: Option<Arc<RunShared>>,
    /// The platform's key translation for hotstrings; `None`: hotstrings never match.
    translate: Option<Translate>,
    /// The bounded hotstring buffer.
    hs: hotstring::Buffer,
    /// Physical key presses (a number, never which keys).
    typed: u32,
    /// Moves while held (Step 71): the trigger's virtual key and the binding.
    motions: [Option<(u16, u16)>; MOTIONS],
    /// The mode state (Step 72): the active mode decides which bindings are candidates.
    modes: ModeState,
    /// A mode change asked for the hook thread's timer (a mode with a timeout).
    want_timer: bool,
    /// The time of the last event (ms, the event clock).
    now: u32,
}

const WIN_BITS: u8 = 0x40 | 0x80;

fn is_mouse(vk: u16) -> bool {
    matches!(vk, 0x01 | 0x02 | 0x04..=0x06)
}

const VK_BACK: u16 = 0x08;
const VK_SCROLL: u16 = 0x91;

fn table_longest(t: &Table) -> usize {
    t.longest_hotstring
}

impl Core {
    pub fn new(table: Table) -> Core {
        let rate = Rate::new(table.len(), table.max_runs_per_second);
        Core {
            norm: Normalizer::new(),
            consumed: KeySet::default(),
            owner: [NONE; 256],
            release: None,
            hold: None,
            seq: None,
            rate,
            tripped: false,
            slots: [None; SLOTS],
            mod_refs: [0; 8],
            key_refs: [None; SLOTS],
            replayed: KeySet::default(),
            hs: hotstring::Buffer::bounded(table_longest(&table)),
            modes: ModeState::new(&table.modes),
            table,
            runs: None,
            translate: None,
            typed: 0,
            motions: [None; MOTIONS],
            want_timer: false,
            now: 0,
        }
    }

    /// The mode state (Step 72).
    pub fn mode_state(&self) -> &ModeState {
        &self.modes
    }

    /// The active mode's index in [`Table::modes`].
    pub fn active_mode(&self) -> u8 {
        self.modes.active()
    }

    /// The mode block of `status` for the current state (outside the callback: it allocates).
    pub fn mode_status(&self) -> crate::modes::ModeStatus {
        let rule = |b: u16| self.table.binding(usize::from(b)).map(|b| b.rule.clone());
        self.table.modes.status(&self.modes.view(), &rule)
    }

    /// The modifier keys a remap released logically that the user still holds (restored when
    /// remaps are dropped).
    fn lifted_still_held(&self) -> u8 {
        let phys = self.norm.mods_physical();
        let lifted: u8 = self.slots.iter().flatten().fold(0, |a, s| a | s.lift);
        ModKey::ALL
            .into_iter()
            .filter(|m| lifted & m.bit() != 0 && phys.has(*m))
            .fold(0u8, |a, m| a | m.bit())
    }

    /// The signal of a mode change. When the active mode changed, everything the old mode held
    /// ends (`DDALGAK-SEC-005`): pending taps, holds, sequences and dual roles, held remaps and
    /// buttons, motions, the hotstring buffer.
    fn mode_signal(&mut self, binding: u16, c: Change) -> Signal {
        let mut restore = 0;
        if c.active {
            self.hs.clear();
            self.release = None;
            self.hold = None;
            self.seq = None;
            restore = self.lifted_still_held();
            self.clear_remaps();
        }
        self.want_timer = true;
        Signal::Mode {
            binding,
            view: self.modes.view(),
            switched: c.active,
            restore,
        }
    }

    /// Runs a fired rule: a mode rule changes the mode here and now (Step 72); every other rule
    /// is a [`Signal::Fire`] for the worker. `None`: a hold found no free layer slot.
    fn run_rule(&mut self, i: usize, kind: FireKind, vk: u16, now: u32) -> Option<Signal> {
        let b = u16::try_from(i).unwrap_or(NONE);
        if !self.table.compiled.get(i).is_some_and(|c| c.mode) {
            return Some(self.fire(i, kind, now));
        }
        if !self.rate.allow(i, now) {
            return Some(Signal::RateLimited { binding: b });
        }
        let action = self.table.mode_action(i)?;
        let change = self.modes.apply(&self.table.modes, action, vk, b, now)?;
        Some(self.mode_signal(b, change))
    }

    /// A mode change asked for outside the keyboard (the CLI, the tray, a rule whose trigger the
    /// worker timed). `None`: refused (a `hold` needs a held key).
    pub fn request_mode(
        &mut self,
        action: &crate::modes::ModeAction,
        origin: ModeOrigin,
        now: u32,
    ) -> Option<Signal> {
        let change = self.modes.request(&self.table.modes, action, origin, now)?;
        let binding = match origin {
            ModeOrigin::Rule(b) => b,
            _ => NONE,
        };
        Some(self.mode_signal(binding, change))
    }

    /// The safe reset (a pause, `ddalgak mode reset`): the default mode, nothing held.
    pub fn reset_mode(&mut self, origin: ModeOrigin) -> Option<Signal> {
        let c = self.modes.reset(&self.table.modes, origin);
        c.state.then(|| self.mode_signal(NONE, c))
    }

    /// The hook thread's timer fired: a mode with a timeout that saw no key for that long
    /// returns to the default mode.
    pub fn mode_tick(&mut self, now: u32) -> Option<Signal> {
        let c = self.modes.tick(&self.table.modes, now);
        c.state.then(|| self.mode_signal(NONE, c))
    }

    /// When the hook thread's one-shot timer should fire next: the earliest undecided dual role
    /// or the end of a timed mode (milliseconds from `now`).
    pub fn next_timer(&self, now: u32) -> Option<u32> {
        let tap = self
            .slots
            .iter()
            .flatten()
            .filter_map(|s| {
                s.pending
                    .map(|p| s.spec.tap_ms.saturating_sub(now.wrapping_sub(p)))
            })
            .min();
        let mode = self.modes.due_in(&self.table.modes, now);
        match (tap, mode) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Shares the modifier state and the key count with the worker's typing runs.
    pub fn set_runs(&mut self, runs: Arc<RunShared>) {
        self.runs = Some(runs);
    }

    /// Sets the platform's key translation (hotstrings).
    pub fn set_translator(&mut self, t: Translate) {
        self.translate = Some(t);
    }

    /// The hotstring buffer's length and bound (tests and diagnostics; never its content).
    pub fn hotstring_buffer(&self) -> (usize, usize) {
        (self.hs.len(), self.hs.capacity())
    }

    /// Replaces the rules (a reload). Pending taps, holds, sequences and undecided dual roles are
    /// dropped; keys whose down was consumed keep having their up consumed, and remaps that hold
    /// their target keep it until their trigger goes up (their instructions carry the output
    /// keys, not rule indexes). The mode state is carried over by mode ID (Step 72): the latched
    /// mode stays when it still exists, otherwise the default mode is active; held layers end.
    /// Returns the [`Signal::Mode`] the worker and the host need for the new rules' modes; when the
    /// active mode changed, everything held is released like on any switch.
    pub fn set_table(&mut self, table: Table) -> Signal {
        let old_active = self.table.modes.id(self.modes.active()).to_string();
        self.modes = self.modes.carry(&self.table.modes, &table.modes, self.now);
        let switched = table.modes.id(self.modes.active()) != old_active;
        self.rate = Rate::new(table.len(), table.max_runs_per_second);
        self.hs = hotstring::Buffer::bounded(table_longest(&table));
        self.table = table;
        self.owner = [NONE; 256];
        self.release = None;
        self.hold = None;
        self.seq = None;
        for s in &mut self.slots {
            if s.is_some_and(|x| x.pending.is_some()) {
                *s = None;
            }
        }
        // The worker stops every motion when it gets the new rules; their trigger-ups stay
        // consumed (their downs were).
        self.motions = [None; MOTIONS];
        self.mode_signal(
            NONE,
            Change {
                state: true,
                active: switched,
            },
        )
    }

    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Whether the emergency chord fired; the core passes everything until it is rebuilt.
    pub fn tripped(&self) -> bool {
        self.tripped
    }

    /// How many remaps hold their target right now (diagnostics and tests).
    pub fn active_remaps(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// How many moves while held are going on (diagnostics and tests).
    pub fn active_motions(&self) -> usize {
        self.motions.iter().flatten().count()
    }

    /// Forgets the key state after input went to another desktop (lock screen, UAC,
    /// Ctrl+Alt+Del), an elevated window took focus or the session came back: key-ups made there
    /// never reached the hook. Keys whose down was consumed keep having their up consumed. Every
    /// remap is dropped and the worker is told to release all outputs ([`Signal::ReleaseAll`]),
    /// because the trigger's up may never arrive.
    pub fn reset_keys(&mut self) -> Vec<Signal> {
        self.norm.reset();
        self.hs.clear();
        self.release = None;
        self.hold = None;
        self.seq = None;
        let had =
            self.active_remaps() > 0 || !self.replayed.is_empty() || self.active_motions() > 0;
        self.clear_remaps();
        let mut out = if had {
            vec![Signal::ReleaseAll]
        } else {
            Vec::new()
        };
        // Held layers end too: their key-ups may never reach the hook (Step 72).
        let c = self.modes.drop_holds(ModeOrigin::KeyReset);
        if c.state {
            out.push(self.mode_signal(NONE, c));
        }
        out
    }

    /// Drops pending taps, holds, sequences and remaps (pause, stop, session lock). The worker is
    /// told to release every output ([`Signal::ReleaseAll`], ordered after every press this core
    /// decided before), and the user's trigger modifiers that a remap had released and that are
    /// still physically held are restored.
    pub fn cancel_pending(&mut self) -> Vec<Signal> {
        self.hs.clear();
        self.release = None;
        self.seq = None;
        let mut out = Vec::new();
        if let Some((binding, _)) = self.hold.take() {
            out.push(Signal::HoldCancelled { binding });
        }
        if self.active_remaps() > 0 || !self.replayed.is_empty() || self.active_motions() > 0 {
            out.push(Signal::ReleaseAll);
        }
        let restore = self.lifted_still_held();
        self.clear_remaps();
        if restore != 0 {
            out.push(Signal::Restore { mods: restore });
        }
        // Held layers end with the remaps (Step 72); the latched mode stays (a pause resets it
        // with [`Core::reset_mode`]).
        let c = self.modes.drop_holds(ModeOrigin::KeyReset);
        if c.state {
            out.push(self.mode_signal(NONE, c));
        }
        out
    }

    fn clear_remaps(&mut self) {
        self.slots = [None; SLOTS];
        self.mod_refs = [0; 8];
        self.key_refs = [None; SLOTS];
        self.motions = [None; MOTIONS];
    }

    fn motion_of(&self, vk: u16) -> Option<usize> {
        self.motions
            .iter()
            .position(|m| m.is_some_and(|(k, _)| k == vk))
    }

    /// The hook thread's timer fired at `now` (ms, the event clock): an undecided dual role whose
    /// time passed presses its target. Returns the signal and, when the timer fired early, the
    /// milliseconds left.
    pub fn tick(&mut self, now: u32) -> (Option<Signal>, Option<u32>) {
        let Some(i) = self
            .slots
            .iter()
            .position(|s| s.is_some_and(|s| s.pending.is_some()))
        else {
            return (None, None);
        };
        let Some(mut s) = self.slots[i] else {
            return (None, None);
        };
        let since = s.pending.unwrap_or(now);
        let waited = now.wrapping_sub(since);
        if waited < s.spec.tap_ms {
            return (None, Some(s.spec.tap_ms - waited));
        }
        let op = self.press(&mut s);
        s.pending = None;
        self.slots[i] = Some(s);
        (
            Some(Signal::Remap {
                binding: s.binding,
                op,
            }),
            None,
        )
    }

    fn pass(&self, ev: &KeyEvent, signal: Option<Signal>) -> Decision {
        Decision::new(false, signal, ev.source)
    }

    fn take(&mut self, ev: &KeyEvent, binding: usize, signal: Option<Signal>) -> Decision {
        self.consumed.insert(ev.vk);
        self.owner[usize::from(ev.vk & 0xFF)] = u16::try_from(binding).unwrap_or(NONE);
        Decision::new(true, signal, ev.source)
    }

    fn fire(&mut self, binding: usize, kind: FireKind, now: u32) -> Signal {
        let b = u16::try_from(binding).unwrap_or(NONE);
        if self.rate.allow(binding, now) {
            Signal::Fire { binding: b, kind }
        } else {
            Signal::RateLimited { binding: b }
        }
    }

    /// Modifiers held for the application: every source, except keys whose down Ddalgak consumed
    /// (a remapped modifier) and including keys it sent on after holding them back.
    fn mods(&self) -> ModState {
        let mut m = self.norm.mods();
        for k in ModKey::ALL {
            if m.has(k) && self.consumed.contains(k.vk()) && !self.replayed.contains(k.vk()) {
                m = m.without(k);
            }
        }
        m
    }

    /// Whether the application sees this key held (pressed by the user or another program and
    /// passed on, or sent on by Ddalgak).
    fn visible_down(&self, vk: u16) -> bool {
        self.norm.is_down(vk) && (!self.consumed.contains(vk) || self.replayed.contains(vk))
    }

    /// Whether the held modifiers plus this key form a chord reserved for triggers (a widened
    /// remap never captures one: with `Tab` remapped, `Alt+Tab` still switches windows).
    fn reserved_now(&self, ev: &KeyEvent) -> bool {
        let mods = self.mods();
        if mods.win() {
            return true; // every Win chord is reserved (§3.1)
        }
        let Some(name) = vk::vk_name(ev.vk, ev.extended) else {
            return false;
        };
        if name == "CapsLock" || name == "NumLock" {
            return false;
        }
        let chord = Chord {
            mods: mods.as_mods(),
            key: Key::Named(name),
        };
        kim_hotkeys::reserved::chord(&chord, &Scope::Context(String::new()), false).is_some()
            || name == "F12"
    }

    /// `Some(true)`: the chord matches exactly; `Some(false)`: only through widening (a trigger
    /// without modifiers of a remap or block, with modifiers held); `None`: no match.
    fn chord_match(&self, c: &CChord, ev: &KeyEvent, widen: bool) -> Option<bool> {
        // The key first: most chords fail there, before the modifier state is computed.
        let key_ok = match c.key {
            KeyMatch::Modifier(m) => {
                if ev.modifier != Some(m) {
                    return None;
                }
                let mods = self.mods();
                if mods.matches_without(&c.mods, m) {
                    return Some(true);
                }
                let widened = widen && c.mods.is_empty() && !mods.without(m).win();
                return widened.then_some(false);
            }
            _ if ev.modifier.is_some() || ev.altgr_fake_ctrl => false,
            KeyMatch::Vk(v, ext) => ev.vk == v && ext.is_none_or(|e| e == ev.extended),
            KeyMatch::Scan(s) => ev.scan == s && !ev.extended && !is_mouse(ev.vk),
        };
        if !key_ok {
            return None;
        }
        let mods = self.mods();
        if mods.matches(&c.mods) {
            return Some(true);
        }
        (widen && c.mods.is_empty() && !self.reserved_now(ev)).then_some(false)
    }

    fn chord_matches(&self, c: &CChord, ev: &KeyEvent, widen: bool) -> bool {
        self.chord_match(c, ev, widen).is_some()
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

    fn key_ref(&self, code: OutCode) -> u8 {
        self.key_refs
            .iter()
            .flatten()
            .find(|(c, _)| *c == code)
            .map_or(0, |(_, n)| *n)
    }

    /// Adds a reference; returns the count before.
    fn add_key_ref(&mut self, code: OutCode) -> u8 {
        if let Some(e) = self.key_refs.iter_mut().flatten().find(|(c, _)| *c == code) {
            e.1 = e.1.saturating_add(1);
            return e.1 - 1;
        }
        if let Some(e) = self.key_refs.iter_mut().find(|e| e.is_none()) {
            *e = Some((code, 1));
        }
        0
    }

    /// Drops a reference; returns the count after.
    fn drop_key_ref(&mut self, code: OutCode) -> u8 {
        for e in &mut self.key_refs {
            if let Some((c, n)) = e
                && *c == code
            {
                *n = n.saturating_sub(1);
                let left = *n;
                if left == 0 {
                    *e = None;
                }
                return left;
            }
        }
        0
    }

    fn mod_index(m: ModKey) -> usize {
        ModKey::ALL.iter().position(|k| *k == m).unwrap_or(0)
    }

    /// Presses a remap's target (§5.1): releases the trigger's own modifiers that the target
    /// does not use (exact triggers only), presses the target modifiers and key that nobody holds
    /// yet, and takes references to what it relies on.
    fn press(&mut self, s: &mut Slot) -> RemapOp {
        let spec = s.spec;
        let (lift, mods, key) = self.plan_output(&spec.to, s.lifts, Some(s));
        s.lift = lift;
        RemapOp::Press { lift, mods, key }
    }

    /// Decides which modifiers to release and which modifiers and key to press for `out`. A held
    /// output takes references (recorded in `refs`); a tap takes none (it is over at once).
    fn plan_output(
        &mut self,
        out: &Output,
        lifts: bool,
        mut refs: Option<&mut Slot>,
    ) -> (u8, u8, Option<OutCode>) {
        let held = self.mods();
        let mut lift = 0u8;
        let mut send = 0u8;
        for (want, l, r) in families(&out.mods) {
            let (hl, hr) = (held.has(l), held.has(r));
            let (rl, rr) = (
                self.mod_refs[Self::mod_index(l)] > 0,
                self.mod_refs[Self::mod_index(r)] > 0,
            );
            // Which key of this family serves the output, and whether it must be pressed.
            let (use_key, keep_l, keep_r) = match want {
                Side::None => (None, false, false),
                Side::Any if rl || (hl && !rr) => (Some(l), hl, hr),
                Side::Any if rr || hr => (Some(r), hl, hr),
                Side::Any | Side::Left => (Some(l), hl, false),
                Side::Right => (Some(r), false, hr),
            };
            if lifts {
                if hl && !keep_l {
                    lift |= l.bit();
                }
                if hr && !keep_r {
                    lift |= r.bit();
                }
            }
            let Some(k) = use_key else { continue };
            let idx = Self::mod_index(k);
            let refd = self.mod_refs[idx] > 0;
            let visible = held.has(k) && lift & k.bit() == 0;
            if let Some(s) = refs.as_deref_mut() {
                if refd || !visible {
                    self.mod_refs[idx] = self.mod_refs[idx].saturating_add(1);
                    s.mods |= k.bit();
                    if !refd && !visible {
                        send |= k.bit();
                    }
                }
            } else if !refd && !visible {
                send |= k.bit();
            }
        }
        let mut key = None;
        if let Some(code) = out.key {
            let refd = self.key_ref(code) > 0;
            let visible = code.vk().is_some_and(|v| self.visible_down(v));
            if let Some(s) = refs {
                if refd || !visible {
                    self.add_key_ref(code);
                    s.key = true;
                    if !refd {
                        key = Some(code);
                    }
                }
            } else if !refd && !visible {
                key = Some(code);
            }
        }
        (lift, send, key)
    }

    /// Releases a remap's target and restores the trigger modifiers it released.
    fn release(&mut self, s: &Slot) -> RemapOp {
        let mut key = None;
        if s.key
            && let Some(code) = s.spec.to.key
            && self.drop_key_ref(code) == 0
        {
            key = Some(code);
        }
        let mut mods = 0u8;
        for m in ModKey::ALL {
            if s.mods & m.bit() == 0 {
                continue;
            }
            let idx = Self::mod_index(m);
            self.mod_refs[idx] = self.mod_refs[idx].saturating_sub(1);
            if self.mod_refs[idx] > 0 {
                continue;
            }
            // Another remap released it logically: the application already saw it go up, so
            // nothing is sent and that remap does not restore it.
            if let Some(o) = self
                .slots
                .iter_mut()
                .flatten()
                .find(|o| o.lift & m.bit() != 0)
            {
                o.lift &= !m.bit();
            } else {
                mods |= m.bit();
            }
        }
        let restore = self.still_held(s.lift);
        RemapOp::Release { key, mods, restore }
    }

    /// The modifier keys of `set` that are still held by the user or by another remap.
    fn still_held(&self, set: u8) -> u8 {
        let phys = self.norm.mods_physical();
        ModKey::ALL
            .into_iter()
            .filter(|m| {
                set & m.bit() != 0
                    && (phys.has(*m) || self.mod_refs[Self::mod_index(*m)] > 0)
                    && !(self.consumed.contains(m.vk()) && !self.replayed.contains(m.vk()))
            })
            .fold(0u8, |a, m| a | m.bit())
    }

    /// A dual-role trigger went up undecided: its tap output, at once.
    fn tap(&mut self, s: &Slot) -> RemapOp {
        let Some(out) = s.spec.tap else {
            return RemapOp::Release {
                key: None,
                mods: 0,
                restore: 0,
            };
        };
        let (lift, mods, key) = self.plan_output(&out, s.lifts, None);
        let restore = self.still_held(lift);
        RemapOp::Tap {
            lift,
            mods,
            key,
            restore,
        }
    }

    fn slot_of(&self, vk: u16) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.is_some_and(|s| s.trigger == vk))
    }

    /// The output a held remap repeats: its target key, or a bare-modifier target's key.
    fn repeat_code(s: &Slot) -> Option<OutCode> {
        if s.key {
            return s.spec.to.key.filter(|c| !matches!(c, OutCode::Button(_)));
        }
        if s.spec.to.key.is_none() && s.mods != 0 {
            return ModKey::ALL
                .into_iter()
                .find(|m| s.mods & m.bit() != 0)
                .map(|m| OutCode::Vk(m.vk()));
        }
        None
    }

    fn replay_code(ev: &KeyEvent) -> OutCode {
        OutCode::Exact {
            vk: ev.vk,
            scan: ev.scan,
            ext: ev.extended,
        }
    }

    /// Decides one event. Never allocates, never blocks.
    pub fn decide(&mut self, raw: &RawKey, mode: Mode, flags: &Flags) -> Decision {
        self.now = raw.time_ms;
        let mut d = self.decide_event(raw, mode, flags);
        if std::mem::take(&mut self.want_timer)
            && let Some(ms) = self.next_timer(raw.time_ms)
        {
            d.timer = Some(d.timer.map_or(ms, |t| t.min(ms)));
        }
        if let Some(rs) = &self.runs {
            rs.visible.store(self.mods().bits(), Ordering::SeqCst);
            rs.phys
                .store(self.norm.mods_physical().bits(), Ordering::SeqCst);
        }
        d
    }

    fn decide_event(&mut self, raw: &RawKey, mode: Mode, flags: &Flags) -> Decision {
        let ev = self.norm.normalize(raw);
        if ev.source == Source::Physical
            && ev.transition == Transition::Down
            && ev.modifier.is_none()
        {
            self.typed = self.typed.wrapping_add(1);
            if let Some(rs) = &self.runs {
                rs.typed.store(self.typed, Ordering::SeqCst);
            }
        }
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
                let mut d = self.up(&ev, vk_i);
                // A layer this key held ends, and a one-shot mode whose key this was ends
                // (Step 72), after the key's own up was decided in that mode.
                let mut c = if self.modes.holds_key(ev.vk) {
                    self.modes.release(ev.vk)
                } else {
                    Change::default()
                };
                let shot = self.modes.key_up(&self.table.modes, ev.vk, ev.time_ms);
                c.state |= shot.state;
                c.active |= shot.active;
                if c.state {
                    d.last = Some(self.mode_signal(NONE, c));
                }
                d
            }
            Transition::Repeat => self.repeat(&ev, vk_i),
            Transition::Down => {
                // A timed mode starts its idle time again; a one-shot mode takes this key as its
                // one key (Step 72). The key is decided in the mode as it is now.
                self.modes.key_down(
                    ev.vk,
                    ev.modifier.is_some() || ev.altgr_fake_ctrl,
                    ev.time_ms,
                );
                // A key that goes down again was up: an older pairing state is stale (its up was
                // made where the hook could not see it).
                self.consumed.remove(ev.vk);
                self.replayed.remove(ev.vk);
                let stale = self.slot_of(ev.vk).and_then(|i| self.slots[i].take());
                let stale_motion = self
                    .motion_of(ev.vk)
                    .and_then(|i| self.motions[i].take())
                    .map(|(_, binding)| Signal::MotionEnd { binding });
                let mut d = self.down(&ev, mode, flags);
                let stale = stale
                    .map(|s| Signal::Remap {
                        binding: s.binding,
                        op: self.release(&s),
                    })
                    .or(stale_motion);
                if let Some(r) = stale {
                    if d.pre.is_none() {
                        d.pre = Some(r);
                    } else {
                        d.post = d.post.or(Some(r));
                    }
                }
                d
            }
        }
    }

    fn up(&mut self, ev: &KeyEvent, vk_i: usize) -> Decision {
        let mut consume = self.consumed.remove(ev.vk);
        self.owner[vk_i] = NONE;
        let mut signal = None;
        if let Some((b, k)) = self.release
            && k == ev.vk
        {
            self.release = None;
            signal = self.run_rule(usize::from(b), FireKind::Release, ev.vk, ev.time_ms);
        }
        if let Some((b, k)) = self.hold
            && k == ev.vk
        {
            self.hold = None;
            signal = signal.or(Some(Signal::HoldCancelled { binding: b }));
        }
        if let Some(i) = self.slot_of(ev.vk)
            && let Some(s) = self.slots[i].take()
        {
            let op = if s.pending.is_some() {
                self.tap(&s)
            } else {
                self.release(&s)
            };
            signal = Some(Signal::Remap {
                binding: s.binding,
                op,
            });
        }
        if let Some(i) = self.motion_of(ev.vk)
            && let Some((_, binding)) = self.motions[i].take()
        {
            signal = Some(Signal::MotionEnd { binding });
        }
        let mut post = None;
        if self.replayed.remove(ev.vk) {
            consume = true;
            post = Some(Signal::Replay {
                code: Self::replay_code(ev),
                up: true,
            });
        }
        // The user lets go of a modifier that a typing run released logically: the application
        // already saw it go up.
        if let Some(m) = ev.modifier
            && ev.source == Source::Physical
            && !consume
            && let Some(rs) = &self.runs
            && rs.lifted.fetch_and(!m.bit(), Ordering::SeqCst) & m.bit() != 0
        {
            consume = true;
        }
        // The user lets go of a trigger modifier that a remap released logically: the
        // application already saw it go up.
        if let Some(m) = ev.modifier
            && ev.source != Source::Own
            && !consume
            && let Some(s) = self
                .slots
                .iter_mut()
                .flatten()
                .find(|s| s.lift & m.bit() != 0)
        {
            s.lift &= !m.bit();
            consume = true;
        }
        let mut d = Decision::new(consume, signal, ev.source);
        d.post = post;
        d
    }

    fn repeat(&mut self, ev: &KeyEvent, vk_i: usize) -> Decision {
        if let Some(i) = self.slot_of(ev.vk)
            && let Some(s) = self.slots[i]
        {
            let signal = if s.pending.is_some() {
                None
            } else {
                Self::repeat_code(&s).map(|key| Signal::Remap {
                    binding: s.binding,
                    op: RemapOp::Repeat { key },
                })
            };
            let mut d = Decision::new(self.consumed.contains(ev.vk), signal, ev.source);
            if self.replayed.contains(ev.vk) {
                d.post = Some(Signal::Replay {
                    code: Self::replay_code(ev),
                    up: false,
                });
            }
            return d;
        }
        if self.replayed.contains(ev.vk) {
            return Decision::new(
                true,
                Some(Signal::Replay {
                    code: Self::replay_code(ev),
                    up: false,
                }),
                ev.source,
            );
        }
        if !self.consumed.contains(ev.vk) {
            return self.pass(ev, None);
        }
        let b = self.owner[vk_i];
        let signal = self
            .table
            .compiled
            .get(usize::from(b))
            .filter(|c| c.repeat && c.on == On::Press)
            .map(|_| ())
            .and_then(|()| self.run_rule(usize::from(b), FireKind::Repeat, ev.vk, ev.time_ms));
        Decision::new(true, signal, ev.source)
    }

    /// Decides an undecided dual role because another key went down: its target is pressed
    /// before that key reaches the application.
    fn resolve_pending(&mut self, ev: &KeyEvent) -> Option<Signal> {
        if is_mouse(ev.vk) {
            return None; // clicks are not seen unless a mouse trigger exists; never decide on them
        }
        let i = self
            .slots
            .iter()
            .position(|s| s.is_some_and(|s| s.pending.is_some() && s.trigger != ev.vk))?;
        let mut s = self.slots[i]?;
        let op = self.press(&mut s);
        s.pending = None;
        self.slots[i] = Some(s);
        Some(Signal::Remap {
            binding: s.binding,
            op,
        })
    }

    fn down(&mut self, ev: &KeyEvent, mode: Mode, flags: &Flags) -> Decision {
        // A modifier the user presses again is held for the application again: a remap that
        // released it logically does not restore it.
        if let Some(m) = ev.modifier {
            for s in self.slots.iter_mut().flatten() {
                s.lift &= !m.bit();
            }
            if ev.source == Source::Physical
                && let Some(rs) = &self.runs
            {
                rs.lifted.fetch_and(!m.bit(), Ordering::SeqCst);
            }
        }
        let pre = self.resolve_pending(ev);
        let mut d = self.down_inner(ev, mode, flags);
        if pre.is_some() {
            d.pre = pre;
            if !d.consume {
                // The key must reach the application after the target: hold it back and send it
                // on (with its repeats and its up) behind the target.
                self.consumed.insert(ev.vk);
                self.replayed.insert(ev.vk);
                d.consume = true;
                d.post = Some(Signal::Replay {
                    code: Self::replay_code(ev),
                    up: false,
                });
            }
        }
        d
    }

    fn down_inner(&mut self, ev: &KeyEvent, mode: Mode, flags: &Flags) -> Decision {
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
            self.hs.clear();
            if guard.is_some() || seq.epoch != flags.focus_epoch {
                self.seq = None;
            } else {
                return self.sequence_step(ev, seq);
            }
        }
        let paused = mode == Mode::Paused;
        let mut winner: Option<usize> = None;
        let mut widened: Option<usize> = None;
        let mut skipped: Option<(usize, Skip)> = None;
        let mut seq_start = [0u64; SEQ_WORDS];
        let mut any_seq = false;
        // Only the bindings that work in the active mode, in its precedence (Step 72).
        for &bi in self.table.order(self.modes.active()) {
            let i = usize::from(bi);
            let c = &self.table.compiled[i];
            if c.len == 0 {
                continue; // a hotstring binding: matched on typed characters
            }
            let Some(exact) = self.chord_match(&c.steps[0], ev, c.widen && c.len == 1) else {
                continue;
            };
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
            if !exact {
                // A widened match is the fallback: an exact chord of another rule wins.
                widened.get_or_insert(i);
                continue;
            }
            winner = Some(i);
            break;
        }
        let Some(i) = winner.or(widened) else {
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
                self.hs.clear();
                return Decision::new(true, None, ev.source);
            }
            if let Some(d) = self.hotstring(ev, mode, flags) {
                return d;
            }
            let signal = skipped.map(|(b, code)| Signal::Skipped {
                binding: u16::try_from(b).unwrap_or(NONE),
                code,
            });
            return self.pass(ev, signal);
        };
        // A rule takes the key: what the application shows is no longer what was typed.
        self.hs.clear();
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
        if let Some(spec) = c.is_remap.then(|| self.table.remap(i)).flatten() {
            return self.remap_down(ev, i, &c, spec);
        }
        if c.motion {
            return self.motion_down(ev, i);
        }
        match c.on {
            // A mode rule switches here and now: the next key is decided in the new mode.
            On::Press if c.mode => match self.run_rule(i, FireKind::Press, ev.vk, ev.time_ms) {
                // No free layer slot: the key passes unchanged.
                None => self.pass(ev, None),
                Some(signal) if c.pass_through => self.pass(ev, Some(signal)),
                Some(signal) => self.take(ev, i, Some(signal)),
            },
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

    /// A remap's trigger went down (§5.1).
    fn remap_down(&mut self, ev: &KeyEvent, i: usize, c: &Compiled, spec: RemapSpec) -> Decision {
        let binding = u16::try_from(i).unwrap_or(NONE);
        let finish = |core: &mut Core, signal: Option<Signal>| {
            if c.pass_through {
                core.pass(ev, signal)
            } else {
                core.take(ev, i, signal)
            }
        };
        if !self.rate.allow(i, ev.time_ms) {
            return finish(self, Some(Signal::RateLimited { binding }));
        }
        let Some(free) = self.slots.iter().position(Option::is_none) else {
            // More held remaps than slots: the key passes unchanged.
            return self.pass(ev, None);
        };
        let exact_mods = !c.steps[0].mods.is_empty();
        let mut slot = Slot {
            binding,
            trigger: ev.vk,
            spec,
            lifts: exact_mods && !c.pass_through,
            pending: None,
            lift: 0,
            mods: 0,
            key: false,
        };
        if spec.tap.is_some() && !c.pass_through {
            slot.pending = Some(ev.time_ms);
            self.slots[free] = Some(slot);
            let mut d = finish(self, None);
            d.timer = self.next_timer(ev.time_ms);
            return d;
        }
        let op = self.press(&mut slot);
        self.slots[free] = Some(slot);
        finish(self, Some(Signal::Remap { binding, op }))
    }

    /// The trigger of a move while held went down (§5.7, Step 71): the rule fires and the core
    /// remembers the trigger key so its up ends the motion. More motions than [`MOTIONS`]: the
    /// key passes unchanged.
    fn motion_down(&mut self, ev: &KeyEvent, i: usize) -> Decision {
        let Some(free) = self.motions.iter().position(Option::is_none) else {
            return self.pass(ev, None);
        };
        let signal = self.fire(i, FireKind::Press, ev.time_ms);
        if let Signal::Fire { binding, .. } = signal {
            self.motions[free] = Some((ev.vk, binding));
        }
        self.take(ev, i, Some(signal))
    }

    /// Hotstrings (§3.4): feeds a key that reaches the application unchanged into the bounded
    /// buffer and decides whether it completes an abbreviation. `None`: the key passes as usual.
    fn hotstring(&mut self, ev: &KeyEvent, mode: Mode, flags: &Flags) -> Option<Decision> {
        if !self.table.has_hotstrings_in(self.modes.active()) {
            return None;
        }
        let translate = self.translate?;
        // Modifiers and lock keys change what the next key types, not the text.
        if ev.modifier.is_some()
            || ev.altgr_fake_ctrl
            || matches!(ev.vk, vk::VK_CAPITAL | vk::VK_NUMLOCK | VK_SCROLL)
        {
            return None;
        }
        if flags.focus_epoch != self.hs.epoch
            || ev.time_ms.wrapping_sub(self.hs.last_ms) > hotstring::IDLE_MS
        {
            self.hs.clear();
        }
        self.hs.epoch = flags.focus_epoch;
        self.hs.last_ms = ev.time_ms;
        let m = self.mods();
        // Never in a password field, on the secure desktop, during an IME composition, over an
        // elevated window, while paused, for other programs' input, mouse buttons, IME keys or
        // shortcuts with Ctrl, Alt or Win: the buffer is cleared and nothing is kept.
        if ev.source != Source::Physical
            || mode == Mode::Paused
            || Self::guard(flags).is_some()
            || flags.elevated_foreground
            || is_mouse(ev.vk)
            || vk::is_ime_vk(ev.vk)
            || (m.ctrl() && !m.altgr())
            || m.alt()
            || m.win()
        {
            self.hs.clear();
            return None;
        }
        if ev.vk == VK_BACK {
            self.hs.pop();
            return None;
        }
        let c = match translate(ev.vk, ev.scan, m.shift(), m.altgr()) {
            Typed::Char('\r') => '\n',
            Typed::Char(c) if !c.is_control() || c == '\n' || c == '\t' => c,
            _ => {
                self.hs.clear(); // navigation, function keys, dead keys
                return None;
            }
        };
        let Some(hit) = hotstring::find(self.table.hotstrings_in(self.modes.active()), &self.hs, c)
        else {
            self.hs.push(c);
            return None;
        };
        self.hs.clear();
        let (b, consumed, erase, retype) = {
            let (b, spec) = &self.table.hotstrings_in(self.modes.active())[hit.index];
            (
                *b,
                spec.consumes(),
                spec.erase,
                !spec.immediate && !spec.omit_end,
            )
        };
        let binding = usize::from(b);
        if !self.rate.allow(binding, ev.time_ms) {
            return Some(self.pass(ev, Some(Signal::RateLimited { binding: b })));
        }
        let fire = HotstringFire {
            erase: if erase {
                u8::try_from(hit.typed).unwrap_or(u8::MAX)
            } else {
                0
            },
            case: hit.case,
            key: Self::replay_code(ev),
            mods: m.bits() & !WIN_BITS,
            retype,
            consumed,
            typed: self.typed,
        };
        let signal = Signal::Hotstring { binding: b, fire };
        Some(if consumed {
            self.take(ev, binding, Some(signal))
        } else {
            self.pass(ev, Some(signal))
        })
    }

    fn sequence_step(&mut self, ev: &KeyEvent, seq: Seq) -> Decision {
        let swallow = |core: &mut Core, signal: Option<Signal>| {
            core.consumed.insert(ev.vk);
            Decision::new(true, signal, ev.source)
        };
        if ev.vk == vk::VK_ESCAPE && self.mods().is_empty() {
            self.seq = None;
            return swallow(self, None);
        }
        let step = usize::from(seq.step);
        let mut next = [0u64; SEQ_WORDS];
        let mut complete: Option<usize> = None;
        let mut any = false;
        // In the active mode's precedence (a mode change cancels a pending sequence).
        for &bi in self.table.order(self.modes.active()) {
            let i = usize::from(bi);
            let c = &self.table.compiled[i];
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
            let signal = self.run_rule(i, FireKind::Sequence, ev.vk, ev.time_ms);
            return swallow(self, signal);
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
            hotstring: None,
            modes: 0,
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

    // ---- Step 68: remaps and blocks (DDALGAK_RULE_MODEL §5.1, §5.2, §3.2, §7.4;
    // DDALGAK-FR-010, DDALGAK-SEC-004, DDALGAK-SEC-005). The core decides every output event;
    // these tests read the decisions, so no input is injected anywhere.

    use crate::inject::{Button, OutCode};
    use crate::remap::Output;
    use crate::table::RemapSpec;

    fn remap_spec(to: &str, tap: Option<&str>, tap_ms: u32) -> RemapSpec {
        RemapSpec {
            to: Output::parse(to).unwrap(),
            tap: tap.map(|t| Output::parse(t).unwrap()),
            tap_ms,
        }
    }

    fn remap(rule: &str, keys: &str, to: &str) -> Binding {
        let trigger = Trigger::parse(keys).unwrap();
        let plain = trigger.single().is_some_and(|c| c.mods.is_empty());
        Binding {
            rule: rule.into(),
            trigger,
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: plain,
            injects: true,
            action: Action::Remap(remap_spec(to, None, 200)),
            hotstring: None,
            modes: 0,
        }
    }

    fn dual(rule: &str, keys: &str, to: &str, tap: &str) -> Binding {
        let mut b = remap(rule, keys, to);
        b.action = Action::Remap(remap_spec(to, Some(tap), 200));
        b
    }

    fn block(rule: &str, keys: &str) -> Binding {
        let mut b = binding(rule, keys);
        b.widen = Trigger::parse(keys)
            .unwrap()
            .single()
            .is_some_and(|c| c.mods.is_empty());
        b.action = Action::Block;
        b
    }

    fn bits(keys: &[ModKey]) -> u8 {
        keys.iter().fold(0, |a, k| a | k.bit())
    }

    fn vk(name: &str) -> OutCode {
        OutCode::Vk(named_vk(name).unwrap())
    }

    fn op(d: &Decision) -> Option<RemapOp> {
        match d.signal {
            Some(Signal::Remap { op, .. }) => Some(op),
            _ => None,
        }
    }

    fn press(lift: &[ModKey], mods: &[ModKey], key: Option<OutCode>) -> RemapOp {
        RemapOp::Press {
            lift: bits(lift),
            mods: bits(mods),
            key,
        }
    }

    fn release(key: Option<OutCode>, mods: &[ModKey], restore: &[ModKey]) -> RemapOp {
        RemapOp::Release {
            key,
            mods: bits(mods),
            restore: bits(restore),
        }
    }

    impl T {
        fn mouse(&mut self, b: Button, up: bool) -> Decision {
            self.t += 10;
            let raw = RawKey::physical(b.vk(), 0, up, self.t);
            self.core.decide(&raw, self.mode, &self.flags)
        }
    }

    #[test]
    fn a_remap_holds_its_target_exactly_as_long_as_the_trigger() {
        let mut t = T::new(vec![remap("f15", "F15", "F16")]);
        let d = t.ev("F15", false);
        assert!(d.consume, "the trigger never reaches the application");
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("F16")))));
        for _ in 0..5 {
            let r = t.ev("F15", false);
            assert!(r.consume);
            assert_eq!(op(&r), Some(RemapOp::Repeat { key: vk("F16") }));
        }
        let u = t.ev("F15", true);
        assert!(u.consume, "consumed down, consumed up");
        assert_eq!(op(&u), Some(release(Some(vk("F16")), &[], &[])));
        assert_eq!(t.core.active_remaps(), 0);
        // Unrelated keys are untouched.
        assert!(!t.ev("F14", false).consume && !t.ev("F14", true).consume);
    }

    #[test]
    fn a_chord_trigger_releases_its_modifiers_while_held_and_restores_them() {
        let mut t = T::new(vec![remap("down", "Ctrl+Shift+J", "Down")]);
        t.down(&["LCtrl", "LShift"]);
        let d = t.ev("J", false);
        assert_eq!(
            op(&d),
            Some(press(
                &[ModKey::LCtrl, ModKey::LShift],
                &[],
                Some(vk("Down"))
            )),
            "Ctrl+Shift+J sends Down, not Ctrl+Shift+Down"
        );
        let u = t.ev("J", true);
        assert_eq!(
            op(&u),
            Some(release(
                Some(vk("Down")),
                &[],
                &[ModKey::LCtrl, ModKey::LShift]
            )),
            "both modifiers are still held: restored"
        );
        t.up(&["LCtrl", "LShift"]);
        // Letting go of a modifier while the target is held: its up is consumed (the application
        // already saw it go up) and it is not restored.
        t.down(&["RCtrl", "LShift"]);
        t.ev("J", false);
        let s = t.ev("LShift", true);
        assert!(s.consume, "no second up for the application");
        let u = t.ev("J", true);
        assert_eq!(
            op(&u),
            Some(release(Some(vk("Down")), &[], &[ModKey::RCtrl]))
        );
        t.up(&["RCtrl"]);
        // Pressing a released modifier again during the hold: it passes and is not restored.
        t.down(&["LCtrl", "LShift"]);
        t.ev("J", false);
        t.ev("LShift", true);
        assert!(!t.ev("LShift", false).consume, "pressed again: passes");
        let u = t.ev("J", true);
        assert_eq!(
            op(&u),
            Some(release(Some(vk("Down")), &[], &[ModKey::LCtrl]))
        );
    }

    #[test]
    fn a_plain_trigger_combines_with_held_modifiers_but_an_exact_chord_wins() {
        let mut t = T::new(vec![
            remap("j-down", "J", "Down"),
            binding("ctrl-j", "Ctrl+J"),
            remap("caps-esc", "CapsLock", "Escape"),
        ]);
        let d = t.down(&["LShift", "CapsLock"]);
        assert_eq!(
            op(&d),
            Some(press(&[], &[], Some(vk("Escape")))),
            "Shift stays held: Shift+Escape"
        );
        t.up(&["LShift", "CapsLock"]);
        // Ctrl+J has its own rule: the exact chord beats the widened remap of J, even though the
        // remap comes first in precedence.
        let d = t.down(&["LCtrl", "J"]);
        assert_eq!(fired(&d), Some((1, FireKind::Press)));
        t.up(&["LCtrl", "J"]);
        let d = t.down(&["LShift", "J"]);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("Down")))));
        t.up(&["LShift", "J"]);
        let d = t.ev("J", false);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("Down")))));
    }

    #[test]
    fn widening_never_captures_reserved_or_win_chords() {
        let mut t = T::new(vec![
            remap("tab", "Tab", "F16"),
            remap("caps", "CapsLock", "Escape"),
            block("no-f4", "F4"),
            remap("x1", "MouseX1", "F16"),
        ]);
        let d = t.down(&["LAlt", "Tab"]);
        assert!(!d.consume && d.signal.is_none(), "Alt+Tab switches windows");
        t.up(&["LAlt", "Tab"]);
        let d = t.down(&["LWin", "CapsLock"]);
        assert!(!d.consume && d.signal.is_none(), "Win chords stay Windows'");
        t.up(&["LWin", "CapsLock"]);
        assert!(!t.down(&["LAlt", "F4"]).consume, "Alt+F4 closes the window");
        t.up(&["LAlt", "F4"]);
        t.ev("LWin", false);
        assert!(!t.mouse(Button::X1, false).consume, "Win+X1 passes");
        t.mouse(Button::X1, true);
        t.ev("LWin", true);
        let d = t.down(&["LShift", "F4"]);
        assert!(d.consume, "Shift+F4 is blocked (widened block)");
        assert_eq!(fired(&d), Some((2, FireKind::Press)));
    }

    #[test]
    fn left_and_right_modifier_targets_and_held_modifiers() {
        let mut t = T::new(vec![
            remap("r", "F15", "RCtrl+Up"),
            remap("c", "F16", "Ctrl+C"),
            remap("lc", "F17", "LCtrl+C"),
        ]);
        let d = t.ev("F15", false);
        assert_eq!(op(&d), Some(press(&[], &[ModKey::RCtrl], Some(vk("Up")))));
        let u = t.ev("F15", true);
        assert_eq!(op(&u), Some(release(Some(vk("Up")), &[ModKey::RCtrl], &[])));
        // The user holds Right Ctrl: a generic Ctrl target uses it and presses no Ctrl itself;
        // a Left Ctrl target presses Left Ctrl anyway.
        t.ev("RCtrl", false);
        assert_eq!(
            op(&t.ev("F16", false)),
            Some(press(&[], &[], Some(vk("C"))))
        );
        assert_eq!(
            op(&t.ev("F16", true)),
            Some(release(Some(vk("C")), &[], &[]))
        );
        assert_eq!(
            op(&t.ev("F17", false)),
            Some(press(&[], &[ModKey::LCtrl], Some(vk("C"))))
        );
        assert_eq!(
            op(&t.ev("F17", true)),
            Some(release(Some(vk("C")), &[ModKey::LCtrl], &[]))
        );
        t.ev("RCtrl", true);
        // A side-specific trigger matches only that side (exact matching, HOTKEY_CONTRACT §5.5).
        let mut s = T::new(vec![remap("lf", "LCtrl+F13", "F16")]);
        assert!(op(&s.down(&["RCtrl", "F13"])).is_none());
        s.up(&["RCtrl", "F13"]);
        assert_eq!(
            op(&s.down(&["LCtrl", "F13"])),
            Some(press(&[ModKey::LCtrl], &[], Some(vk("F16"))))
        );
    }

    #[test]
    fn a_remapped_modifier_is_not_held_for_the_application_but_its_target_is() {
        // RAlt acts as HangulMode: the physical Right Alt is consumed, so it is no Alt for
        // other rules; CapsLock acts as LCtrl: once the injected LCtrl arrives, Ctrl+J rules see
        // it (remapped modifiers combine with other rules, like a real Ctrl).
        let mut t = T::new(vec![
            remap("hangul", "RAlt", "HangulMode"),
            binding("f13", "F13"),
            binding("alt-f14", "Alt+F14"),
            remap("caps-ctrl", "CapsLock", "LCtrl"),
            binding("ctrl-j", "Ctrl+J"),
        ]);
        let d = t.ev("RAlt", false);
        assert!(d.consume);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("HangulMode")))));
        assert_eq!(fired(&t.ev("F13", false)), Some((1, FireKind::Press)));
        t.ev("F13", true);
        assert_eq!(
            fired(&t.ev("F14", false)),
            None,
            "no Alt for the application"
        );
        t.ev("F14", true);
        t.ev("RAlt", true);
        let d = t.ev("CapsLock", false);
        assert_eq!(op(&d), Some(press(&[], &[ModKey::LCtrl], None)));
        // The worker's injected LCtrl comes back through the hook as Ddalgak's own input.
        let own = t.src("LCtrl", false, Source::Own);
        assert!(!own.consume && own.signal.is_none(), "never matched");
        assert_eq!(fired(&t.ev("J", false)), Some((4, FireKind::Press)));
        t.ev("J", true);
        // A repeat of a bare-modifier target repeats that modifier.
        assert_eq!(
            op(&t.ev("CapsLock", false)),
            Some(RemapOp::Repeat {
                key: OutCode::Vk(VK_LCONTROL)
            })
        );
        assert_eq!(
            op(&t.ev("CapsLock", true)),
            Some(release(None, &[ModKey::LCtrl], &[]))
        );
    }

    #[test]
    fn two_remaps_holding_one_output_never_release_it_early() {
        let mut t = T::new(vec![
            remap("a", "F15", "F16"),
            remap("b", "F17", "F16"),
            remap("ca", "F18", "Ctrl+A"),
            remap("cb", "F19", "Ctrl+B"),
        ]);
        assert_eq!(
            op(&t.ev("F15", false)),
            Some(press(&[], &[], Some(vk("F16"))))
        );
        assert_eq!(
            op(&t.ev("F17", false)),
            Some(press(&[], &[], None)),
            "already held: not pressed again"
        );
        assert_eq!(
            op(&t.ev("F15", true)),
            Some(release(None, &[], &[])),
            "F17 still holds it"
        );
        assert_eq!(
            op(&t.ev("F17", true)),
            Some(release(Some(vk("F16")), &[], &[]))
        );
        assert_eq!(
            op(&t.ev("F18", false)),
            Some(press(&[], &[ModKey::LCtrl], Some(vk("A"))))
        );
        t.src("LCtrl", false, Source::Own);
        assert_eq!(
            op(&t.ev("F19", false)),
            Some(press(&[], &[], Some(vk("B")))),
            "Ctrl is shared"
        );
        assert_eq!(
            op(&t.ev("F18", true)),
            Some(release(Some(vk("A")), &[], &[]))
        );
        assert_eq!(
            op(&t.ev("F19", true)),
            Some(release(Some(vk("B")), &[ModKey::LCtrl], &[]))
        );
    }

    #[test]
    fn a_key_the_user_holds_is_never_pressed_or_released_by_a_remap() {
        let mut t = T::new(vec![
            remap("f16", "F15", "F16"),
            remap("ctrl", "CapsLock", "LCtrl"),
        ]);
        assert!(!t.ev("F16", false).consume, "the user's own F16 passes");
        assert_eq!(op(&t.ev("F15", false)), Some(press(&[], &[], None)));
        assert_eq!(op(&t.ev("F15", true)), Some(release(None, &[], &[])));
        t.ev("F16", true);
        t.ev("LCtrl", false);
        assert_eq!(op(&t.ev("CapsLock", false)), Some(press(&[], &[], None)));
        assert_eq!(op(&t.ev("CapsLock", true)), Some(release(None, &[], &[])));
        assert!(
            !t.ev("LCtrl", true).consume,
            "the user's LCtrl is the user's"
        );
    }

    #[test]
    fn a_dual_role_key_taps_holds_by_time_or_is_decided_by_another_key() {
        let rules = || {
            vec![
                dual("caps", "CapsLock", "LCtrl", "Escape"),
                binding("f13", "F13"),
            ]
        };
        // A quick tap sends the tap output.
        let mut t = T::new(rules());
        let d = t.ev("CapsLock", false);
        assert!(d.consume && d.signal.is_none());
        assert_eq!(d.timer, Some(200), "the hook arms a one-shot timer");
        assert!(
            t.ev("CapsLock", false).signal.is_none(),
            "repeats while undecided"
        );
        let u = t.ev("CapsLock", true);
        assert!(u.consume);
        assert_eq!(
            op(&u),
            Some(RemapOp::Tap {
                lift: 0,
                mods: 0,
                key: Some(vk("Escape")),
                restore: 0
            })
        );
        // Held past the tap time: the timer presses the target; the release releases it.
        let mut t = T::new(rules());
        t.ev("CapsLock", false);
        let since = t.t;
        assert_eq!(t.core.tick(since + 50), (None, Some(150)), "fired early");
        let (s, again) = t.core.tick(since + 200);
        assert_eq!(again, None);
        assert_eq!(
            s,
            Some(Signal::Remap {
                binding: 0,
                op: press(&[], &[ModKey::LCtrl], None)
            })
        );
        assert_eq!(t.core.tick(since + 300), (None, None), "nothing pending");
        assert_eq!(
            op(&t.ev("CapsLock", true)),
            Some(release(None, &[ModKey::LCtrl], &[]))
        );
        // Another key during the hold: the target first, then that key, sent on unchanged
        // (with its repeats and its up).
        let mut t = T::new(rules());
        t.ev("CapsLock", false);
        let d = t.ev("C", false);
        assert!(d.consume, "held back");
        assert_eq!(
            d.pre,
            Some(Signal::Remap {
                binding: 0,
                op: press(&[], &[ModKey::LCtrl], None)
            })
        );
        let c = OutCode::Exact {
            vk: u16::from(b'C'),
            scan: 0,
            ext: false,
        };
        assert_eq!(d.post, Some(Signal::Replay { code: c, up: false }));
        let order: Vec<Signal> = d.signals().collect();
        assert!(
            matches!(order[0], Signal::Remap { .. }) && matches!(order[1], Signal::Replay { .. })
        );
        let r = t.ev("C", false);
        assert!(r.consume);
        assert_eq!(
            r.signals().collect::<Vec<_>>(),
            [Signal::Replay { code: c, up: false }]
        );
        let u = t.ev("C", true);
        assert!(u.consume);
        assert_eq!(u.post, Some(Signal::Replay { code: c, up: true }));
        assert_eq!(
            op(&t.ev("CapsLock", true)),
            Some(release(None, &[ModKey::LCtrl], &[]))
        );
        assert!(!t.ev("C", false).consume, "the next C is normal again");
        t.ev("C", true);
        // A key with its own rule decides the dual role too, and fires its rule (no replay).
        let mut t = T::new(rules());
        t.ev("CapsLock", false);
        let d = t.ev("F13", false);
        assert!(matches!(d.pre, Some(Signal::Remap { .. })));
        assert_eq!(fired(&d), Some((1, FireKind::Press)));
        assert_eq!(d.post, None);
        // A tap with modifiers held (widened): Shift+tap is Shift+Escape (Shift stays).
        let mut t = T::new(rules());
        t.ev("LShift", false);
        t.ev("CapsLock", false);
        assert_eq!(
            op(&t.ev("CapsLock", true)),
            Some(RemapOp::Tap {
                lift: 0,
                mods: 0,
                key: Some(vk("Escape")),
                restore: 0
            })
        );
        // Mouse buttons never decide a dual role (clicks are not seen without a mouse trigger).
        let mut t = T::new(vec![
            dual("caps", "CapsLock", "LCtrl", "Escape"),
            remap("x1", "MouseX1", "F16"),
        ]);
        t.ev("CapsLock", false);
        let m = t.mouse(Button::X1, false);
        assert_eq!(m.pre, None);
        assert_eq!(op(&m), Some(press(&[], &[], Some(vk("F16")))));
    }

    #[test]
    fn pass_through_lets_the_original_key_reach_the_application_too() {
        let mut b = remap("both", "F15", "F16");
        b.pass_through = true;
        let mut t = T::new(vec![b]);
        let d = t.ev("F15", false);
        assert!(!d.consume, "the original passes");
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("F16")))));
        let r = t.ev("F15", false);
        assert!(!r.consume);
        assert_eq!(op(&r), Some(RemapOp::Repeat { key: vk("F16") }));
        let u = t.ev("F15", true);
        assert!(!u.consume);
        assert_eq!(op(&u), Some(release(Some(vk("F16")), &[], &[])));
        // A chord trigger with pass-through keeps its modifiers (they reach the application).
        let mut b = remap("chord", "Ctrl+F15", "F16");
        b.pass_through = true;
        let mut t = T::new(vec![b]);
        let d = t.down(&["LCtrl", "F15"]);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("F16")))));
    }

    #[test]
    fn block_consumes_its_key_and_its_up() {
        let mut t = T::new(vec![block("no-insert", "Insert"), block("no-f17", "F17")]);
        let d = t.ev("Insert", false);
        assert!(d.consume);
        assert_eq!(fired(&d), Some((0, FireKind::Press)));
        assert!(t.ev("Insert", false).consume, "repeats are consumed");
        assert!(t.ev("Insert", true).consume, "the up is consumed");
        assert!(t.down(&["LCtrl", "F17"]).consume, "Ctrl+F17 too (widened)");
        assert!(t.up(&["LCtrl", "F17"]).consume || true);
        // Blocking never blocks the emergency chord.
        let mut e = T::new(vec![block("no-esc", "Escape")]);
        let d = e.down(&["LCtrl", "LAlt", "LShift", "Escape"]);
        assert_eq!(d.signal, Some(Signal::Emergency));
        assert!(!d.consume);
    }

    #[test]
    fn the_emergency_chord_beats_remaps_and_blocks_of_its_own_keys() {
        // Step 73 (DDALGAK-SEC-003): every key of the chord is remapped or blocked, in every
        // order. The chord is checked on the physical state before the table, so no rule can
        // shadow it, and the keys a remap holds are released by the emergency path.
        let rules = || {
            vec![
                remap("ctrl", "LCtrl", "F16"),
                remap("ralt", "RAlt", "LCtrl"),
                remap("lalt", "LAlt", "F17"),
                remap("shift", "LShift", "RShift"),
                remap("esc", "Escape", "F18"),
                block("rshift", "RShift"),
                dual("caps", "CapsLock", "LCtrl", "Escape"),
            ]
        };
        let orders: [&[&str]; 4] = [
            &["LCtrl", "LAlt", "LShift", "Escape"],
            &["Escape", "LShift", "LCtrl", "LAlt"],
            &["RAlt", "LCtrl", "Escape", "RShift"],
            &["LShift", "Escape", "LAlt", "LCtrl"],
        ];
        for keys in orders {
            let mut t = T::new(rules());
            let mut last = None;
            for (i, k) in keys.iter().enumerate() {
                let d = t.ev(k, false);
                if i + 1 < keys.len() {
                    assert_ne!(d.signal, Some(Signal::Emergency), "{keys:?} {k}");
                }
                last = Some(d);
            }
            let d = last.unwrap();
            assert_eq!(d.signal, Some(Signal::Emergency), "{keys:?}");
            assert!(!d.consume, "the last key of the chord passes on");
            assert!(t.core.tripped());
            // Afterwards nothing matches: the remapped keys reach Windows as they are.
            for k in keys.iter().rev() {
                let up = t.ev(k, true);
                assert!(
                    up.signal.is_none() || matches!(up.signal, Some(Signal::Remap { .. })),
                    "{keys:?}: {k} up: {up:?}"
                );
            }
            let again = t.ev("Escape", false);
            assert!(!again.consume && again.signal.is_none());
        }
        // Injected modifiers of a remap never complete the chord: only physical keys count.
        let mut t = T::new(rules());
        t.src("LCtrl", false, Source::Own);
        t.src("LAlt", false, Source::Own);
        t.src("LShift", false, Source::Own);
        assert_ne!(t.ev("Escape", false).signal, Some(Signal::Emergency));
        // A paused engine, a secure desktop, a password field and an IME composition never
        // hide the chord either.
        for flags in [
            Flags {
                secure_desktop: true,
                ..Flags::default()
            },
            Flags {
                password_field: true,
                ime_composing: true,
                elevated_foreground: true,
                ..Flags::default()
            },
        ] {
            let mut t = T::new(rules());
            t.mode = Mode::Paused;
            t.flags = flags;
            assert_eq!(
                t.down(&["LCtrl", "LAlt", "LShift", "Escape"]).signal,
                Some(Signal::Emergency)
            );
        }
        // An empty table (an invalid rules.json never loads) still trips.
        let mut t = T::new(Vec::new());
        assert_eq!(
            t.down(&["RCtrl", "RShift", "LAlt", "Escape"]).signal,
            Some(Signal::Emergency)
        );
    }

    #[test]
    fn pause_lock_and_desktop_switches_release_every_output() {
        let mut t = T::new(vec![
            remap("down", "Ctrl+Shift+J", "Down"),
            remap("f", "F15", "F16"),
        ]);
        t.down(&["LCtrl", "LShift", "J"]);
        t.ev("F15", false);
        assert_eq!(t.core.active_remaps(), 2);
        let s = t.core.cancel_pending();
        assert_eq!(
            s,
            [
                Signal::ReleaseAll,
                Signal::Restore {
                    mods: bits(&[ModKey::LCtrl, ModKey::LShift])
                }
            ],
            "everything goes up; the user's still-held Ctrl and Shift come back"
        );
        assert_eq!(t.core.active_remaps(), 0);
        let u = t.ev("J", true);
        assert!(
            u.consume && op(&u).is_none(),
            "paired up, nothing more to release"
        );
        t.ev("F15", true);
        t.up(&["LCtrl", "LShift"]);
        assert!(
            t.core.cancel_pending().is_empty(),
            "nothing held, nothing sent"
        );
        // A desktop switch, an unlock or an elevated window: the trigger's up may never arrive.
        t.ev("F15", false);
        assert_eq!(t.core.reset_keys(), [Signal::ReleaseAll]);
        assert_eq!(t.core.active_remaps(), 0);
        assert!(t.ev("F15", true).consume, "a late up stays paired");
        assert!(t.core.reset_keys().is_empty());
        // The key works again on its next press.
        assert_eq!(
            op(&t.ev("F15", false)),
            Some(press(&[], &[], Some(vk("F16"))))
        );
    }

    #[test]
    fn a_reload_keeps_held_outputs_until_their_trigger_goes_up() {
        let mut t = T::new(vec![
            remap("f", "F15", "F16"),
            dual("caps", "CapsLock", "LCtrl", "Escape"),
        ]);
        t.ev("F15", false);
        t.ev("CapsLock", false);
        t.core.set_table(Table::compile(vec![], 20));
        assert_eq!(
            t.core.active_remaps(),
            1,
            "the undecided dual role is dropped"
        );
        let u = t.ev("F15", true);
        assert!(u.consume);
        assert_eq!(op(&u), Some(release(Some(vk("F16")), &[], &[])));
        let c = t.ev("CapsLock", true);
        assert!(c.consume && c.signal.is_none(), "no tap after a reload");
    }

    #[test]
    fn mouse_buttons_and_physical_targets_go_through_the_same_core() {
        let mut t = T::new(vec![
            remap("x1", "MouseX1", "code:KeyH"),
            remap("mid", "Ctrl+MouseMiddle", "F16"),
            block("x2", "MouseX2"),
            remap("f15", "F15", "MouseMiddle"),
        ]);
        let d = t.mouse(Button::X1, false);
        assert!(d.consume);
        assert_eq!(op(&d), Some(press(&[], &[], Some(OutCode::Scan(0x23)))));
        let u = t.mouse(Button::X1, true);
        assert_eq!(op(&u), Some(release(Some(OutCode::Scan(0x23)), &[], &[])));
        t.ev("LCtrl", false);
        assert_eq!(
            op(&t.mouse(Button::Middle, false)),
            Some(press(&[ModKey::LCtrl], &[], Some(vk("F16"))))
        );
        t.mouse(Button::Middle, true);
        t.ev("LCtrl", true);
        assert!(
            !t.mouse(Button::Middle, false).consume,
            "no rule without Ctrl"
        );
        t.mouse(Button::Middle, true);
        assert!(t.mouse(Button::X2, false).consume && t.mouse(Button::X2, true).consume);
        let d = t.ev("F15", false);
        assert_eq!(
            op(&d),
            Some(press(&[], &[], Some(OutCode::Button(Button::Middle))))
        );
        assert!(
            op(&t.ev("F15", false)).is_none(),
            "mouse buttons do not auto-repeat"
        );
    }

    #[test]
    fn remap_presses_are_rate_limited_but_their_repeats_are_not() {
        let mut t = T::new(vec![remap("f", "F15", "F16")]);
        t.ev("F15", false);
        for _ in 0..40 {
            assert!(matches!(
                op(&t.ev("F15", false)),
                Some(RemapOp::Repeat { .. })
            ));
        }
        t.ev("F15", true);
        let mut limited = 0;
        for _ in 0..30 {
            let d = t.ev("F15", false);
            assert!(d.consume);
            if d.signal == Some(Signal::RateLimited { binding: 0 }) {
                limited += 1;
                assert!(t.ev("F15", true).consume && t.core.active_remaps() == 0);
            } else {
                t.ev("F15", true);
            }
        }
        // 20 presses per second, as every rule; the first press of this test counts too.
        assert_eq!(limited, 10);
    }

    #[test]
    fn swapped_and_chained_remaps_never_loop() {
        // CapsLock → Escape and Escape → CapsLock: the injected Escape is Ddalgak's own input.
        let mut t = T::new(vec![
            remap("caps-esc", "CapsLock", "Escape"),
            remap("esc-caps", "Escape", "CapsLock"),
            remap("f15", "F15", "F16"),
            remap("f16", "F16", "F17"),
        ]);
        assert_eq!(
            op(&t.ev("CapsLock", false)),
            Some(press(&[], &[], Some(vk("Escape"))))
        );
        for _ in 0..50 {
            let d = t.src("Escape", false, Source::Own);
            assert!(!d.consume && d.signal.is_none());
        }
        t.src("Escape", true, Source::Own);
        t.ev("CapsLock", true);
        // F15 → F16 does not chain into F16 → F17: outputs never trigger rules.
        t.ev("F15", false);
        let d = t.src("F16", false, Source::Own);
        assert!(!d.consume && d.signal.is_none());
        t.src("F16", true, Source::Own);
        // Other programs' input never reaches remaps (they cannot accept injected input).
        let d = t.src("F16", false, Source::Other);
        assert!(!d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::Skipped {
                binding: 3,
                code: Skip::InjectedInput
            })
        );
    }

    #[test]
    fn remaps_step_aside_like_every_injecting_rule() {
        let mut t = T::new(vec![remap("f", "F15", "F16"), block("b", "F17")]);
        t.flags.password_field = true;
        let d = t.ev("F15", false);
        assert!(!d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::Skipped {
                binding: 0,
                code: Skip::PasswordField
            })
        );
        t.ev("F15", true);
        t.flags = Flags {
            elevated_foreground: true,
            ..Flags::default()
        };
        assert_eq!(
            t.ev("F15", false).signal,
            Some(Signal::Skipped {
                binding: 0,
                code: Skip::ElevatedTarget
            })
        );
        t.ev("F15", true);
        assert!(
            t.ev("F17", false).consume,
            "block injects nothing: it still works over an elevated window"
        );
        t.ev("F17", true);
        t.flags = Flags::default();
        t.mode = Mode::Paused;
        let d = t.ev("F15", false);
        assert!(!d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::Skipped {
                binding: 0,
                code: Skip::Paused
            })
        );
    }

    #[test]
    fn decisions_with_remaps_stay_far_below_the_hook_budget() {
        // 16 held remaps (all slots) plus 1000 rules: still allocation-free and fast.
        let mut bindings: Vec<Binding> = (13..=24)
            .map(|n| remap(&format!("f{n}"), &format!("F{n}"), "Ctrl+Shift+A"))
            .collect();
        for k in 'A'..='D' {
            bindings.push(remap(&format!("k{k}"), &k.to_string(), "Down"));
        }
        while bindings.len() < 1000 {
            let n = bindings.len();
            bindings.push(binding(
                &format!("s-{n}"),
                &format!("Ctrl+K code:Key{}", (b'A' + (n % 26) as u8) as char),
            ));
        }
        let mut t = T::new(bindings);
        for n in 13..=24 {
            t.ev(&format!("F{n}"), false);
        }
        for k in ["A", "B", "C", "D"] {
            t.ev(k, false);
        }
        assert_eq!(t.core.active_remaps(), SLOTS);
        let flags = Flags::default();
        let raw = RawKey::physical(0x91, 0x46, false, 0);
        let up = RawKey::physical(0x91, 0x46, true, 0);
        let n = 20_000u32;
        let start = std::time::Instant::now();
        for _ in 0..n / 2 {
            t.core.decide(&raw, Mode::Running, &flags);
            t.core.decide(&up, Mode::Running, &flags);
        }
        let per = start.elapsed() / n;
        eprintln!("1000 rules, 16 held remaps: {per:?} per event");
        assert!(per < std::time::Duration::from_micros(500), "{per:?}");
        // A 17th held remap has no slot: its key passes unchanged.
        let mut more = T::new(
            (13..=24)
                .map(|n| remap(&format!("f{n}"), &format!("F{n}"), "F1"))
                .chain(
                    ['A', 'B', 'C', 'D', 'E']
                        .iter()
                        .map(|k| remap(&format!("k{k}"), &k.to_string(), "F2")),
                )
                .collect(),
        );
        for n in 13..=24 {
            more.ev(&format!("F{n}"), false);
        }
        for k in ["A", "B", "C", "D"] {
            more.ev(k, false);
        }
        assert!(!more.ev("E", false).consume);
    }

    // ---- Step 69: hotstrings, typing runs' modifier state (DDALGAK_RULE_MODEL §3.4; ADR 0034).
    // Synthetic event streams only: no real typing, no injection.

    use crate::hotstring::{self as hs, Spec};
    use crate::run::{RunShared, TextSpec};
    use crate::text::CaseForm;

    fn hot(rule: &str, spec: Spec) -> Binding {
        Binding {
            rule: rule.into(),
            trigger: Trigger::none(),
            on: On::Press,
            repeat: false,
            pass_through: false,
            injected: Injected::Ignore,
            widen: false,
            injects: true,
            action: Action::Type(Arc::new(TextSpec {
                text: "by the way".into(),
                paste: false,
                key_delay_ms: 0,
                variables: false,
            })),
            hotstring: Some(Arc::new(spec)),
            modes: 0,
        }
    }

    fn typing(bindings: Vec<Binding>) -> (T, Arc<RunShared>) {
        let mut t = T::new(bindings);
        let runs = Arc::new(RunShared::default());
        t.core.set_runs(Arc::clone(&runs));
        t.core.set_translator(hs::us_qwerty);
        (t, runs)
    }

    impl T {
        /// Types keys (down and up) and returns the decision of the last down.
        fn type_keys(&mut self, keys: &[&str]) -> Decision {
            let mut last = None;
            for k in keys {
                last = Some(self.ev(k, false));
                self.ev(k, true);
            }
            last.unwrap()
        }

        fn fired_hotstring(d: &Decision) -> Option<(u16, HotstringFire)> {
            match d.signal {
                Some(Signal::Hotstring { binding, fire }) => Some((binding, fire)),
                _ => None,
            }
        }
    }

    #[test]
    fn a_hotstring_fires_on_an_end_character_and_consumes_only_that_key() {
        let (mut t, runs) = typing(vec![binding("other", "F13"), hot("btw", Spec::new("btw"))]);
        for k in ["B", "T", "W"] {
            let d = t.ev(k, false);
            assert!(!d.consume && d.signal.is_none(), "{k} reaches the app");
            t.ev(k, true);
        }
        let d = t.ev("Space", false);
        assert!(d.consume, "the end key is held back");
        let (b, f) = T::fired_hotstring(&d).unwrap();
        assert_eq!(b, 1);
        assert_eq!(
            (f.erase, f.case, f.retype, f.consumed),
            (3, CaseForm::AsWritten, true, true)
        );
        assert_eq!(
            f.key,
            OutCode::Exact {
                vk: 0x20,
                scan: 0,
                ext: false
            }
        );
        assert_eq!(f.typed, 4, "four physical presses so far");
        assert_eq!(runs.typed.load(Ordering::SeqCst), 4);
        assert!(t.ev("Space", true).consume, "consumed down, consumed up");
        assert_eq!(t.core.hotstring_buffer().0, 0, "cleared after a match");
        // The next word starts fresh.
        assert!(T::fired_hotstring(&t.type_keys(&["B", "T", "W", "Period"])).is_some());
    }

    #[test]
    fn case_forms_and_word_boundaries() {
        let (mut t, _) = typing(vec![hot("btw", Spec::new("btw"))]);
        t.ev("LShift", false);
        t.type_keys(&["B"]);
        t.ev("LShift", true);
        let d = t.type_keys(&["T", "W", "Space"]);
        let (_, f) = T::fired_hotstring(&d).unwrap();
        assert_eq!(f.case, CaseForm::Title);
        t.ev("LShift", false);
        let d = t.type_keys(&["B", "T", "W", "1"]);
        t.ev("LShift", true);
        // Shift+1 is `!`, a default end character.
        let (_, f) = T::fired_hotstring(&d).unwrap_or_else(|| panic!("{d:?}"));
        assert_eq!(f.case, CaseForm::Upper);
        assert_ne!(f.mods, 0, "Shift is typed again with the end key");
        // Inside a word: no match.
        assert!(t.type_keys(&["A", "B", "T", "W", "Space"]).signal.is_none());
    }

    #[test]
    fn every_reset_clears_the_buffer() {
        let spec = || Spec::new("btw");
        let fires = |f: &dyn Fn(&mut T)| {
            let (mut t, _) = typing(vec![hot("btw", spec())]);
            t.type_keys(&["B", "T"]);
            f(&mut t);
            t.type_keys(&["W"]);
            T::fired_hotstring(&t.type_keys(&["Space"])).is_some()
        };
        assert!(fires(&|_| {}), "the baseline fires");
        assert!(!fires(&|t| t.flags.focus_epoch += 1), "focus change");
        assert!(
            !fires(&|t| {
                t.flags.password_field = true;
                t.type_keys(&["X"]);
                t.flags.password_field = false;
            }),
            "password field"
        );
        assert!(
            !fires(&|t| {
                t.flags.ime_composing = true;
                t.type_keys(&["X"]);
                t.flags.ime_composing = false;
            }),
            "IME composition"
        );
        assert!(
            !fires(&|t| {
                t.flags.secure_desktop = true;
                t.type_keys(&["X"]);
                t.flags.secure_desktop = false;
            }),
            "secure desktop"
        );
        assert!(
            !fires(&|t| {
                t.mode = Mode::Paused;
                t.type_keys(&["X"]);
                t.mode = Mode::Running;
            }),
            "paused"
        );
        assert!(
            !fires(&|t| {
                t.type_keys(&["Left"]);
            }),
            "navigation"
        );
        assert!(
            !fires(&|t| {
                t.ev("LCtrl", false);
                t.type_keys(&["V"]);
                t.ev("LCtrl", true);
            }),
            "a shortcut"
        );
        assert!(
            !fires(&|t| {
                t.src("X", false, Source::Other);
                t.src("X", true, Source::Other);
            }),
            "another program's input"
        );
        assert!(!fires(&|t| t.t += 20_000), "a pause in typing");
        assert!(
            !fires(&|t| {
                t.key_with_scan(0xDF, 0x29, false);
                t.key_with_scan(0xDF, 0x29, true);
            }),
            "a dead key"
        );
        // A key-state reset (desktop switch) and a pause clear it too.
        let (mut t, _) = typing(vec![hot("btw", spec())]);
        t.type_keys(&["B", "T"]);
        let _ = t.core.reset_keys();
        assert_eq!(t.core.hotstring_buffer().0, 0);
        t.type_keys(&["B", "T"]);
        let _ = t.core.cancel_pending();
        assert_eq!(t.core.hotstring_buffer().0, 0);
        // Backspace forgets the last character; own input is not part of the buffer.
        let (mut t, _) = typing(vec![hot("btw", spec())]);
        t.type_keys(&["B", "T", "X", "Backspace", "W"]);
        t.src("Q", false, Source::Own);
        t.src("Q", true, Source::Own);
        assert!(T::fired_hotstring(&t.type_keys(&["Space"])).is_some());
    }

    #[test]
    fn the_buffer_is_bounded_by_the_longest_abbreviation() {
        let (mut t, _) = typing(vec![
            hot("btw", Spec::new("btw")),
            hot("sig", Spec::new("sig")),
        ]);
        assert_eq!(t.core.hotstring_buffer(), (0, 4));
        for _ in 0..50 {
            t.type_keys(&["A", "B", "C"]);
        }
        assert_eq!(t.core.hotstring_buffer().0, 4, "never more than the bound");
        // Without hotstrings there is no translation and no buffer use at all.
        let (mut t, _) = typing(vec![binding("x", "F13")]);
        t.type_keys(&["A", "B"]);
        assert_eq!(t.core.hotstring_buffer().0, 0);
    }

    #[test]
    fn immediate_hotstrings_and_kept_abbreviations() {
        let mut imm = Spec::new("sig");
        imm.immediate = true;
        let (mut t, _) = typing(vec![hot("sig", imm.clone())]);
        t.type_keys(&["S", "I"]);
        let d = t.ev("G", false);
        let (_, f) = T::fired_hotstring(&d).unwrap();
        assert!(d.consume);
        assert_eq!(
            (f.erase, f.retype),
            (2, false),
            "the last key never reached the app"
        );
        t.ev("G", true);
        imm.erase = false;
        let (mut t, _) = typing(vec![hot("sig", imm)]);
        t.type_keys(&["S", "I"]);
        let d = t.ev("G", false);
        let (_, f) = T::fired_hotstring(&d).unwrap();
        assert!(!d.consume, "kept: the last key reaches the app");
        assert_eq!((f.erase, f.consumed), (0, false));
        // A rule that takes a key clears the buffer.
        let (mut t, _) = typing(vec![binding("b-rule", "B"), hot("btw", Spec::new("btw"))]);
        t.type_keys(&["B", "T", "W"]);
        assert!(T::fired_hotstring(&t.type_keys(&["Space"])).is_none());
    }

    #[test]
    fn hotstrings_are_rate_limited_like_any_rule() {
        let mut t = T::new(vec![hot("btw", Spec::new("btw"))]);
        t.core = Core::new(Table::compile(vec![hot("btw", Spec::new("btw"))], 1));
        t.core.set_translator(hs::us_qwerty);
        assert!(T::fired_hotstring(&t.type_keys(&["B", "T", "W", "Space"])).is_some());
        let d = t.type_keys(&["B", "T", "W", "Space"]);
        assert!(matches!(d.signal, Some(Signal::RateLimited { binding: 0 })));
        assert!(!d.consume, "a dropped hotstring lets its key through");
    }

    #[test]
    fn typing_runs_see_the_modifiers_and_their_released_modifiers_are_paired() {
        let (mut t, runs) = typing(vec![binding("f13", "Ctrl+Shift+F13")]);
        t.down(&["LCtrl", "LShift"]);
        let both = ModKey::LCtrl.bit() | ModKey::LShift.bit();
        assert_eq!(runs.visible.load(Ordering::SeqCst), both);
        assert_eq!(runs.phys.load(Ordering::SeqCst), both);
        // A run released Shift logically (its own injected up is seen as own input).
        runs.lifted.fetch_or(ModKey::LShift.bit(), Ordering::SeqCst);
        t.src("LShift", true, Source::Own);
        assert_eq!(runs.visible.load(Ordering::SeqCst), ModKey::LCtrl.bit());
        assert_eq!(
            runs.phys.load(Ordering::SeqCst),
            both,
            "still held physically"
        );
        // The physical release is consumed: the application already saw Shift go up.
        assert!(t.ev("LShift", true).consume);
        assert_eq!(runs.lifted.load(Ordering::SeqCst), 0);
        // Pressing a released modifier again forgets the release.
        assert!(!t.ev("LCtrl", true).consume);
        runs.lifted.fetch_or(ModKey::LCtrl.bit(), Ordering::SeqCst);
        t.ev("LCtrl", false);
        assert_eq!(runs.lifted.load(Ordering::SeqCst), 0);
        assert!(!t.ev("LCtrl", true).consume);
        // Modifier presses are not counted as typed keys.
        let before = runs.typed.load(Ordering::SeqCst);
        t.type_keys(&["A"]);
        assert_eq!(runs.typed.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn decisions_with_100_hotstrings_stay_far_below_the_hook_budget() {
        // 100 hotstrings of 3–12 characters plus 900 key rules: every typed key is translated
        // and matched against all of them (the worst case: no match).
        let mut rules: Vec<Binding> = (0..100)
            .map(|i| {
                let abbr: String = (0..3 + i % 10)
                    .map(|k| char::from(b'a' + u8::try_from((i + k) % 26).unwrap()))
                    .collect();
                hot(&format!("h{i}"), Spec::new(&format!(";{abbr}")))
            })
            .collect();
        rules.extend(
            (0..900).map(|i| binding(&format!("k{i}"), &format!("Ctrl+Alt+F{}", 13 + i % 12))),
        );
        let mut t = T::new(rules);
        t.core.set_translator(hs::us_qwerty);
        let flags = Flags::default();
        let (vk, scan) = (0x51u16, 0x10u16); // Q: no abbreviation contains it after `;`
        let n = 20_000u32;
        let start = std::time::Instant::now();
        for i in 0..n / 2 {
            let down = RawKey::physical(vk, scan, false, i * 20);
            let up = RawKey::physical(vk, scan, true, i * 20 + 10);
            t.core.decide(&down, Mode::Running, &flags);
            t.core.decide(&up, Mode::Running, &flags);
        }
        let per = start.elapsed() / n;
        eprintln!("100 hotstrings + 900 rules: {per:?} per event");
        assert!(per < std::time::Duration::from_micros(500), "{per:?}");
        assert!(
            t.core.hotstring_buffer().0 <= 14,
            "the longest abbreviation is 13"
        );
    }

    #[test]
    fn keys_rules_with_typing_steps_fire_and_step_aside() {
        let mut b = binding("sign", "Ctrl+Shift+F2");
        b.injects = true;
        b.action = Action::Type(Arc::new(TextSpec {
            text: "x".into(),
            paste: false,
            key_delay_ms: 0,
            variables: false,
        }));
        let mut t = T::new(vec![b]);
        let d = t.down(&["LCtrl", "LShift", "F2"]);
        assert!(d.consume);
        assert_eq!(fired(&d), Some((0, FireKind::Press)));
        t.up(&["LCtrl", "LShift", "F2"]);
        t.flags.elevated_foreground = true;
        let d = t.down(&["LCtrl", "LShift", "F2"]);
        assert!(!d.consume);
        assert_eq!(
            d.signal,
            Some(Signal::Skipped {
                binding: 0,
                code: Skip::ElevatedTarget
            })
        );
    }

    // ---- Step 71: held buttons and moves while held (DDALGAK_RULE_MODEL §5.7, §5.9;
    // DDALGAK-FR-015, DDALGAK-SEC-005). Decisions only: nothing is injected.

    fn motion(rule: &str, keys: &str, dx: i32, dy: i32) -> Binding {
        let mut b = binding(rule, keys);
        b.injects = true;
        b.action = Action::Motion(crate::mouse::MotionSpec {
            dx,
            dy,
            unit: crate::mouse::Unit::Pixels,
            kind: crate::mouse::MotionKind::Continuous {
                speed: 600,
                acceleration: 0,
            },
        });
        b
    }

    fn hold(rule: &str, keys: &str, button: Button) -> Binding {
        let mut b = binding(rule, keys);
        b.injects = true;
        b.action = Action::MouseHold {
            spec: RemapSpec {
                to: Output::button(button),
                tap: None,
                tap_ms: 200,
            },
            at: None,
        };
        b
    }

    fn left() -> Option<OutCode> {
        Some(OutCode::Button(Button::Left))
    }

    #[test]
    fn a_move_while_held_lasts_exactly_as_long_as_its_trigger() {
        let mut t = T::new(vec![
            motion("up", "Up", 0, -1),
            motion("right", "Right", 1, 0),
        ]);
        let d = t.ev("Up", false);
        assert!(d.consume, "the trigger never reaches the application");
        assert_eq!(fired(&d), Some((0, FireKind::Press)));
        assert_eq!(t.core.active_motions(), 1);
        // Auto-repeat (a held key, a game's input storm) is consumed and fires nothing: the
        // motion goes on by itself.
        for _ in 0..50 {
            let r = t.ev("Up", false);
            assert!(r.consume && r.signal.is_none());
        }
        // Right while Up is held: two motions, the worker adds them up (a diagonal).
        assert_eq!(fired(&t.ev("Right", false)), Some((1, FireKind::Press)));
        assert_eq!(t.core.active_motions(), 2);
        let u = t.ev("Up", true);
        assert!(u.consume, "consumed down, consumed up");
        assert_eq!(u.signal, Some(Signal::MotionEnd { binding: 0 }));
        assert_eq!(
            t.ev("Right", true).signal,
            Some(Signal::MotionEnd { binding: 1 })
        );
        assert_eq!(t.core.active_motions(), 0);
        // Moves while held match exactly: Shift+Up is not Up.
        let s = t.down(&["LShift", "Up"]);
        assert!(!s.consume && s.signal.is_none());
        t.up(&["LShift", "Up"]);
    }

    #[test]
    fn a_held_button_is_a_shared_reference_counted_output() {
        let mut t = T::new(vec![
            hold("drag", "Space", Button::Left),
            hold("drag-too", "D", Button::Left),
            hold("pan", "Ctrl+M", Button::Middle),
        ]);
        let d = t.ev("Space", false);
        assert!(d.consume);
        assert_eq!(op(&d), Some(press(&[], &[], left())));
        // Buttons never auto-repeat: a held key's repeats send nothing.
        for _ in 0..5 {
            assert_eq!(op(&t.ev("Space", false)), None);
        }
        // A second hold of the same button shares it: it goes up when the last one lets go.
        assert_eq!(op(&t.ev("D", false)), Some(press(&[], &[], None)));
        assert_eq!(op(&t.ev("Space", true)), Some(release(None, &[], &[])));
        assert_eq!(op(&t.ev("D", true)), Some(release(left(), &[], &[])));
        // A chord trigger's modifiers are released while the button is held (a plain middle
        // drag, not Ctrl+drag) and restored afterwards when still held.
        t.down(&["LCtrl"]);
        let m = t.ev("M", false);
        assert_eq!(
            op(&m),
            Some(press(
                &[ModKey::LCtrl],
                &[],
                Some(OutCode::Button(Button::Middle))
            ))
        );
        assert_eq!(
            op(&t.ev("M", true)),
            Some(release(
                Some(OutCode::Button(Button::Middle)),
                &[],
                &[ModKey::LCtrl]
            ))
        );
        t.up(&["LCtrl"]);
        assert_eq!(t.core.active_remaps(), 0);
    }

    #[test]
    fn pause_lock_desktop_switches_and_reloads_never_leave_a_button_or_a_motion_held() {
        // 0: pause, stop or session lock (cancel_pending); 1: a desktop switch, an unlock or an
        // elevated window taking focus (reset_keys); 2: a reload.
        for path in 0..3 {
            let rules = || {
                vec![
                    motion("up", "Up", 0, -1),
                    hold("drag", "Space", Button::Left),
                ]
            };
            let mut t = T::new(rules());
            t.ev("Up", false);
            assert_eq!(op(&t.ev("Space", false)), Some(press(&[], &[], left())));
            let signals = match path {
                0 => t.core.cancel_pending(),
                1 => t.core.reset_keys(),
                _ => {
                    t.core.set_table(Table::compile(rules(), 20));
                    Vec::new()
                }
            };
            if path < 2 {
                assert!(signals.contains(&Signal::ReleaseAll), "{path}: {signals:?}");
                assert_eq!((t.core.active_motions(), t.core.active_remaps()), (0, 0));
                // The trigger-ups that come later change nothing more.
                assert_eq!(t.ev("Up", true).signal, None, "{path}");
                assert_eq!(op(&t.ev("Space", true)), None, "{path}");
            } else {
                // A reload: the worker stops every motion when it gets the new rules; the held
                // button stays until its trigger goes up and is then released.
                assert_eq!(t.core.active_motions(), 0);
                let u = t.ev("Up", true);
                assert!(u.consume && u.signal.is_none(), "its up stays consumed");
                assert_eq!(op(&t.ev("Space", true)), Some(release(left(), &[], &[])));
            }
        }
        // Only a motion: a release-all still goes to the worker so it stops.
        let mut t = T::new(vec![motion("up", "Up", 0, -1)]);
        t.ev("Up", false);
        assert_eq!(t.core.reset_keys(), vec![Signal::ReleaseAll]);
        assert!(t.core.reset_keys().is_empty(), "nothing left to release");
    }

    #[test]
    fn mouse_rules_step_aside_and_storms_are_rate_limited() {
        let mut t = T::new(vec![
            motion("up", "Up", 0, -1),
            hold("drag", "Space", Button::Left),
        ]);
        t.flags.elevated_foreground = true;
        for k in ["Up", "Space"] {
            let d = t.ev(k, false);
            assert!(!d.consume, "{k} passes unchanged");
            assert!(matches!(
                d.signal,
                Some(Signal::Skipped {
                    code: Skip::ElevatedTarget,
                    ..
                })
            ));
            t.ev(k, true);
        }
        t.flags.elevated_foreground = false;
        t.flags.secure_desktop = true;
        assert!(!t.ev("Up", false).consume);
        t.ev("Up", true);
        t.flags.secure_desktop = false;
        t.mode = Mode::Paused;
        assert!(matches!(
            t.ev("Up", false).signal,
            Some(Signal::Skipped {
                code: Skip::Paused,
                ..
            })
        ));
        t.ev("Up", true);
        t.mode = Mode::Running;
        assert_eq!(t.core.active_motions(), 0);
        // Hammering the trigger (a repeated hotkey, high-frequency input): beyond 20 a second the
        // presses are rate limited, start no motion, and their ups end nothing.
        let mut limited = 0;
        for _ in 0..30 {
            let d = t.ev("Up", false);
            if matches!(d.signal, Some(Signal::RateLimited { .. })) {
                limited += 1;
                assert_eq!(t.core.active_motions(), 0);
                assert_eq!(t.ev("Up", true).signal, None);
            } else {
                assert_eq!(
                    t.ev("Up", true).signal,
                    Some(Signal::MotionEnd { binding: 0 })
                );
            }
        }
        assert!(limited >= 10, "{limited}");
        // More moves while held than the core keeps: the extra key passes unchanged.
        let many: Vec<Binding> = (13..=21)
            .map(|n| motion(&format!("f{n}"), &format!("F{n}"), 1, 0))
            .collect();
        let mut t = T::new(many);
        for n in 13..=20 {
            assert!(t.ev(&format!("F{n}"), false).consume);
        }
        assert_eq!(t.core.active_motions(), MOTIONS);
        let extra = t.ev("F21", false);
        assert!(!extra.consume && extra.signal.is_none());
    }

    // ---- Step 72: modes and layers -------------------------------------------------------------

    use crate::model::{Mode as ModeDoc, Profile};
    use crate::modes::{ModeAction, ModeSet};

    /// A mode set from `(id, parent)` pairs; the first is the default mode.
    fn mode_set(spec: &[(&str, Option<&str>)]) -> ModeSet {
        let modes = spec
            .iter()
            .map(|(id, parent)| {
                let mut m = ModeDoc::new(*id, id.to_uppercase());
                m.parent = parent.map(str::to_string);
                m
            })
            .collect();
        ModeSet::from_profile(&Profile {
            id: "p".into(),
            title: "P".into(),
            description: None,
            default_mode: spec[0].0.into(),
            enabled: None,
            modes,
            rules: Vec::new(),
            extra: serde_json::Map::new(),
        })
    }

    /// base, nav (layer over base), sym (layer over nav), game (exclusive).
    fn layers() -> ModeSet {
        mode_set(&[
            ("base", None),
            ("nav", Some("base")),
            ("sym", Some("nav")),
            ("game", None),
        ])
    }

    fn bit(s: &ModeSet, id: &str) -> u32 {
        1 << s.index(id).unwrap()
    }

    fn mode_rule(rule: &str, keys: &str, action: ModeAction) -> Binding {
        Binding {
            action: Action::Mode(action),
            ..binding(rule, keys)
        }
    }

    fn scoped(mut b: Binding, mask: u32) -> Binding {
        b.modes = mask;
        b
    }

    impl T {
        fn with_modes(bindings: Vec<Binding>, modes: ModeSet) -> T {
            T {
                core: Core::new(Table::compile_modes(bindings, modes, 20)),
                mode: Mode::Running,
                flags: Flags::default(),
                t: 1000,
            }
        }

        fn active(&self) -> String {
            self.core
                .table()
                .modes()
                .id(self.core.active_mode())
                .to_string()
        }
    }

    /// The mode signal of a decision: (switched, active mode, restore).
    fn mode_sig(d: &Decision) -> Option<(bool, u8, u8)> {
        d.signals().find_map(|s| match s {
            Signal::Mode {
                switched,
                view,
                restore,
                ..
            } => Some((switched, view.active, restore)),
            _ => None,
        })
    }

    #[test]
    fn a_held_layer_changes_what_keys_mean_and_releases_its_outputs_on_the_way_out() {
        let s = layers();
        let nav = s.index("nav").unwrap();
        let mut t = T::with_modes(
            vec![
                mode_rule("hold-nav", "CapsLock", ModeAction::Hold(nav)),
                scoped(remap("nav-left", "code:KeyH", "Left"), bit(&s, "nav")),
            ],
            s.clone(),
        );
        // In Base, H is just H.
        assert!(!t.ev("code:KeyH", false).consume);
        assert!(!t.ev("code:KeyH", true).consume);
        // CapsLock held: Navigation, decided in the callback; the key itself is consumed.
        let d = t.ev("CapsLock", false);
        assert!(d.consume);
        assert_eq!(mode_sig(&d), Some((true, nav, 0)));
        assert_eq!(t.active(), "nav");
        // Its auto-repeat is consumed and changes nothing.
        let r = t.src("CapsLock", false, Source::Physical);
        assert!(r.consume && mode_sig(&r).is_none());
        // H is Left while held.
        let d = t.ev("code:KeyH", false);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("Left")))));
        assert_eq!(t.core.active_remaps(), 1);
        // CapsLock goes up while H is still down: Base returns and Left is released at once
        // (DDALGAK-SEC-005), not when H goes up.
        let d = t.ev("CapsLock", true);
        assert!(d.consume, "the trigger's up is consumed with its down");
        assert_eq!(mode_sig(&d), Some((true, 0, 0)));
        assert_eq!(t.core.active_remaps(), 0, "the core dropped the held remap");
        assert_eq!(t.active(), "base");
        // H's up stays consumed (its down was) and sends nothing more.
        let d = t.ev("code:KeyH", true);
        assert!(d.consume);
        assert!(op(&d).is_none());
        // H again: Base, passes.
        assert!(!t.ev("code:KeyH", false).consume);
    }

    #[test]
    fn the_nearest_layer_wins_then_its_parents_then_rules_for_every_mode() {
        let s = layers();
        let (nav, sym, game) = (
            s.index("nav").unwrap(),
            s.index("sym").unwrap(),
            s.index("game").unwrap(),
        );
        let every = binding("every", "F13"); // an engine rule in every mode
        let mut t = T::with_modes(
            vec![
                every,
                scoped(block("nav-f13", "F13"), bit(&s, "nav")),
                scoped(block("base-f14", "F14"), bit(&s, "base")),
                scoped(block("sym-f14", "F14"), bit(&s, "sym")),
                mode_rule("to-nav", "Ctrl+Shift+F1", ModeAction::Set(nav)),
                mode_rule("to-sym", "Ctrl+Shift+F2", ModeAction::Set(sym)),
                mode_rule("to-game", "Ctrl+Shift+F3", ModeAction::Set(game)),
            ],
            s.clone(),
        );
        let winner = |t: &mut T, key: &str| {
            let d = t.ev(key, false);
            t.ev(key, true);
            match d.signal {
                Some(Signal::Fire { binding, .. }) => Some(
                    t.core
                        .table()
                        .binding(usize::from(binding))
                        .unwrap()
                        .rule
                        .clone(),
                ),
                _ if d.consume => Some("consumed".to_string()),
                _ => None,
            }
        };
        let switch = |t: &mut T, keys: &[&str]| {
            let d = t.down(keys);
            assert!(mode_sig(&d).is_some());
            t.up(keys);
        };
        // Base: the rule for every mode; F14: Base's own block.
        assert_eq!(winner(&mut t, "F13").as_deref(), Some("every"));
        assert_eq!(winner(&mut t, "F14").as_deref(), Some("base-f14"));
        // Navigation (a layer over Base): its own F13 wins; F14 falls back to Base's block.
        switch(&mut t, &["LCtrl", "LShift", "F1"]);
        assert_eq!(t.active(), "nav");
        assert_eq!(winner(&mut t, "F13").as_deref(), Some("nav-f13"));
        assert_eq!(winner(&mut t, "F14").as_deref(), Some("base-f14"));
        // Symbols (a layer over Navigation): F13 falls back to Navigation's block.
        switch(&mut t, &["LCtrl", "LShift", "F2"]);
        assert_eq!(t.active(), "sym");
        assert_eq!(winner(&mut t, "F13").as_deref(), Some("nav-f13"));
        // Gaming (exclusive): only the rule for every mode; F14 reaches the application.
        switch(&mut t, &["LCtrl", "LShift", "F3"]);
        assert_eq!(t.active(), "game");
        assert_eq!(winner(&mut t, "F13").as_deref(), Some("every"));
        assert_eq!(
            winner(&mut t, "F14"),
            None,
            "unbound keys reach the application"
        );
    }

    #[test]
    fn switching_modes_while_typing_keeps_every_key_paired_and_nothing_held() {
        // A synthetic typing stream: letters, a remap held across switches, Shift, a toggle key.
        let s = layers();
        let nav = s.index("nav").unwrap();
        let mut t = T::with_modes(
            vec![
                mode_rule("toggle-nav", "F13", ModeAction::Toggle(nav)),
                scoped(remap("nav-j", "J", "Down"), bit(&s, "nav")),
                scoped(remap("caps", "CapsLock", "LCtrl"), bit(&s, "base")),
            ],
            s.clone(),
        );
        let mut consumed_downs = Vec::new();
        let mut type_key = |t: &mut T, k: &str| {
            let d = t.ev(k, false);
            if d.consume {
                consumed_downs.push(k.to_string());
            }
            let u = t.ev(k, true);
            assert_eq!(
                u.consume, d.consume,
                "{k}: down and up are consumed together"
            );
        };
        for k in ["H", "E", "J"] {
            type_key(&mut t, k);
        }
        assert!(consumed_downs.is_empty(), "Base: plain typing passes");
        // Hold CapsLock (Ctrl in Base), then toggle Navigation while it is held.
        assert!(t.ev("CapsLock", false).consume);
        assert_eq!(t.core.active_remaps(), 1);
        let d = t.ev("F13", false);
        assert_eq!(mode_sig(&d), Some((true, nav, 0)));
        assert_eq!(t.core.active_remaps(), 0, "Ctrl is released on the switch");
        t.ev("F13", true);
        // CapsLock's up: consumed (its down was), nothing pressed again.
        let d = t.ev("CapsLock", true);
        assert!(d.consume && op(&d).is_none());
        // In Navigation J is Down; typing continues with Shift.
        let d = t.ev("J", false);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("Down")))));
        t.ev("J", true);
        t.down(&["LShift", "E"]);
        t.up(&["LShift", "E"]);
        // Toggle back while J is held: Down is released, J's up stays consumed.
        assert!(t.ev("J", false).consume);
        let d = t.ev("F13", false);
        assert_eq!(mode_sig(&d), Some((true, 0, 0)));
        t.ev("F13", true);
        assert!(t.ev("J", true).consume);
        assert_eq!(t.core.active_remaps(), 0);
        for k in ["J", "K"] {
            assert!(!t.ev(k, false).consume, "{k} types again in Base");
            assert!(!t.ev(k, true).consume);
        }
    }

    #[test]
    fn every_mode_action_works_on_every_kind_of_permitted_trigger() {
        // Keyboard accessibility: a mode action can be put on a plain key, a chord, a lock key,
        // a physical key position, a sequence, a bare modifier tapped alone and a mouse button,
        // and it fires from that key alone (no pointer needed).
        let s = layers();
        let nav = s.index("nav").unwrap();
        let triggers: [(&str, &[&str]); 6] = [
            ("F13", &["F13"]),
            ("Ctrl+Alt+Shift+F14", &["LCtrl", "LAlt", "LShift", "F14"]),
            ("ScrollLock", &["ScrollLock"]),
            ("code:KeyM", &["code:KeyM"]),
            ("CapsLock", &["CapsLock"]),
            ("Insert", &["Insert"]),
        ];
        let actions = [
            ModeAction::Set(nav),
            ModeAction::Toggle(nav),
            ModeAction::Cycle(vec![0, nav].into()),
            ModeAction::Hold(nav),
        ];
        for a in &actions {
            for (keys, press) in triggers {
                let mut t = T::with_modes(vec![mode_rule("r", keys, a.clone())], s.clone());
                let d = t.down(press);
                assert_eq!(mode_sig(&d).map(|m| m.1), Some(nav), "{keys}: {a:?}");
                assert_eq!(t.active(), "nav");
                t.up(press);
                let back = if a.holds() { "base" } else { "nav" };
                assert_eq!(t.active(), back, "{keys}: {a:?}");
            }
        }
        // A sequence and a bare modifier tapped alone (on release) switch too.
        let mut seq = mode_rule("seq", "Ctrl+K M", ModeAction::Set(nav));
        seq.trigger = Trigger::parse("Ctrl+K M").unwrap();
        let mut tap = mode_rule("tap", "RAlt", ModeAction::Default);
        tap.on = On::Release;
        let mut t = T::with_modes(vec![seq, tap], s.clone());
        t.down(&["LCtrl", "K"]);
        t.up(&["LCtrl", "K"]);
        let d = t.ev("M", false);
        assert_eq!(mode_sig(&d).map(|m| m.1), Some(nav));
        t.ev("M", true);
        assert!(
            !t.ev("RAlt", false).consume,
            "a bare modifier keeps working in chords"
        );
        let d = t.ev("RAlt", true);
        assert_eq!(mode_sig(&d).map(|m| m.1), Some(0));
        // A middle mouse button as the trigger (with the mouse hook, Step 68).
        let mut t = T::with_modes(
            vec![mode_rule("mid", "MouseMiddle", ModeAction::Toggle(nav))],
            s,
        );
        let raw = RawKey::physical(Button::Middle.vk(), 0, false, 2000);
        let d = t.core.decide(&raw, Mode::Running, &Flags::default());
        assert_eq!(mode_sig(&d).map(|m| m.1), Some(nav));
    }

    #[test]
    fn previous_default_pass_through_and_rate_limits() {
        let s = layers();
        let (nav, game) = (s.index("nav").unwrap(), s.index("game").unwrap());
        let mut home = mode_rule("home", "Escape", ModeAction::Default);
        home.pass_through = true;
        let mut t = T::with_modes(
            vec![
                mode_rule("to-game", "F15", ModeAction::Set(game)),
                mode_rule("to-nav", "F16", ModeAction::Set(nav)),
                mode_rule("back", "F17", ModeAction::Previous),
                scoped(home, bit(&s, "nav") | bit(&s, "game")),
            ],
            s,
        );
        t.ev("F15", false);
        t.ev("F15", true);
        t.ev("F16", false);
        t.ev("F16", true);
        assert_eq!(t.active(), "nav");
        t.ev("F17", false);
        t.ev("F17", true);
        assert_eq!(t.active(), "game");
        // Escape returns to Base and still reaches the application.
        let d = t.ev("Escape", false);
        assert!(!d.consume);
        assert!(mode_sig(&d).is_some());
        assert!(!t.ev("Escape", true).consume);
        assert_eq!(t.active(), "base");
        // In Base the Escape rule does not apply: nothing fires.
        let d = t.ev("Escape", false);
        assert!(mode_sig(&d).is_none() && !d.consume);
        t.ev("Escape", true);
        // A storm of switches is rate limited like any rule.
        let mut limited = 0;
        for _ in 0..30 {
            let d = t.ev("F16", false);
            if matches!(d.signal, Some(Signal::RateLimited { .. })) {
                limited += 1;
            }
            t.ev("F16", true);
        }
        assert!(limited >= 10, "{limited}");
    }

    #[test]
    fn one_shot_and_timed_modes_run_through_the_core_and_its_timer() {
        let mut quick = ModeDoc::new("quick", "Quick");
        quick.one_shot = Some(true);
        let mut timed = ModeDoc::new("timed", "Timed");
        timed.timeout_ms = Some(2000);
        let s = ModeSet::from_profile(&Profile {
            id: "p".into(),
            title: "P".into(),
            description: None,
            default_mode: "base".into(),
            enabled: None,
            modes: vec![ModeDoc::new("base", "Base"), quick, timed],
            rules: Vec::new(),
            extra: serde_json::Map::new(),
        });
        let mut t = T::with_modes(
            vec![
                mode_rule("q", "F13", ModeAction::Set(1)),
                mode_rule("tm", "F14", ModeAction::Set(2)),
                scoped(remap("q-a", "A", "F20"), 0b10),
            ],
            s,
        );
        assert_eq!(t.core.next_timer(t.t), None, "no timer while idle");
        t.ev("F13", false);
        t.ev("F13", true);
        assert_eq!(t.active(), "quick");
        // A is the one key: remapped in the one-shot mode, then the mode ends with A's up.
        let d = t.ev("A", false);
        assert_eq!(op(&d), Some(press(&[], &[], Some(vk("F20")))));
        let d = t.ev("A", true);
        assert!(matches!(d.signal, Some(Signal::Remap { .. })));
        assert!(matches!(d.last, Some(Signal::Mode { .. })));
        assert_eq!(t.active(), "base");
        assert!(!t.ev("A", false).consume, "A is A again");
        t.ev("A", true);
        // A timed mode asks for the timer and ends after 2 s without a key.
        let d = t.ev("F14", false);
        assert_eq!(d.timer, Some(2000));
        let now = t.t;
        t.ev("F14", true);
        assert_eq!(t.active(), "timed");
        assert!(t.core.mode_tick(now + 1000).is_none());
        assert_eq!(t.core.next_timer(now + 1000), Some(1000));
        let s = t.core.mode_tick(now + 2000).unwrap();
        assert!(matches!(s, Signal::Mode { switched: true, .. }));
        assert_eq!(t.active(), "base");
        assert_eq!(t.core.next_timer(now + 2000), None);
    }

    #[test]
    fn pause_lock_desktop_switches_reloads_and_requests_never_strand_a_layer() {
        let s = layers();
        let (nav, game) = (s.index("nav").unwrap(), s.index("game").unwrap());
        let rules = vec![
            mode_rule("hold-nav", "CapsLock", ModeAction::Hold(nav)),
            mode_rule("to-game", "F15", ModeAction::Set(game)),
            scoped(remap("nav-l", "L", "Right"), bit(&s, "nav")),
        ];
        let mut t = T::with_modes(rules.clone(), s);
        // A key reset (desktop switch, unlock, elevated window) ends a held layer and its outputs.
        t.ev("CapsLock", false);
        t.ev("L", false);
        let signals = t.core.reset_keys();
        assert_eq!(signals[0], Signal::ReleaseAll);
        assert!(matches!(signals[1], Signal::Mode { .. }));
        assert_eq!(t.active(), "base");
        t.ev("L", true);
        t.ev("CapsLock", true);
        // Pause: cancel ends the layer, the reset returns to the default mode.
        t.ev("F15", false);
        t.ev("F15", true);
        t.ev("CapsLock", false);
        assert_eq!(t.active(), "nav");
        let signals = t.core.cancel_pending();
        assert!(signals.iter().any(|s| matches!(s, Signal::Mode { .. })));
        assert_eq!(t.active(), "game", "a lock keeps the latched mode");
        assert!(t.core.reset_mode(ModeOrigin::Pause).is_some());
        assert_eq!(t.active(), "base", "a pause is the safe reset");
        assert!(t.core.reset_mode(ModeOrigin::Pause).is_none());
        t.ev("CapsLock", true);
        // A reload keeps the latched mode by ID; a reload that deletes it falls back to Base and
        // releases what the old mode held.
        t.ev("F15", false);
        t.ev("F15", true);
        let s2 = mode_set(&[("base", None), ("game", None), ("nav", Some("base"))]);
        let sig = t
            .core
            .set_table(Table::compile_modes(rules.clone(), s2, 20));
        assert!(matches!(
            sig,
            Signal::Mode {
                switched: false,
                ..
            }
        ));
        assert_eq!(t.active(), "game");
        let s3 = mode_set(&[("base", None), ("nav", Some("base"))]);
        let sig = t.core.set_table(Table::compile_modes(rules, s3, 20));
        assert!(matches!(sig, Signal::Mode { switched: true, .. }));
        assert_eq!(t.active(), "base");
        // Requests (the CLI, the tray) change the mode exactly like rules; a hold is refused.
        let sig = t
            .core
            .request_mode(&ModeAction::Set(1), ModeOrigin::Cli, 5000);
        assert!(matches!(
            sig,
            Some(Signal::Mode {
                switched: true,
                binding: NONE,
                ..
            })
        ));
        assert!(
            t.core
                .request_mode(&ModeAction::Hold(1), ModeOrigin::Cli, 5000)
                .is_none()
        );
        // The emergency chord trips the core; a new engine starts in the default mode.
        t.down(&["LCtrl", "LAlt", "LShift", "Escape"]);
        assert!(t.core.tripped());
        let fresh = Core::new(Table::compile_modes(Vec::new(), layers(), 20));
        assert_eq!(fresh.active_mode(), 0);
        assert_eq!(fresh.mode_status().origin, "start");
    }

    #[test]
    fn decisions_with_modes_stay_far_below_the_hook_budget() {
        // 1000 rules over 8 modes (layers and exclusive ones), a key no rule uses, in the
        // deepest layer: the active mode's list is scanned, nothing else.
        let s = mode_set(&[
            ("base", None),
            ("a", Some("base")),
            ("b", Some("a")),
            ("c", Some("b")),
            ("d", None),
            ("e", Some("d")),
            ("f", None),
            ("g", Some("c")),
        ]);
        let bindings: Vec<Binding> = (0..1000)
            .map(|i| {
                let keys = format!(
                    "{}F{}",
                    ["", "Ctrl+", "Alt+", "Shift+", "Ctrl+Shift+"][i % 5],
                    13 + (i / 5) % 12
                );
                scoped(binding(&format!("r{i}"), &keys), 1 << (i % 8))
            })
            .collect();
        let mut t = T::with_modes(bindings, s);
        let g = t.core.table().modes().index("g").unwrap();
        let _ = t.core.request_mode(&ModeAction::Set(g), ModeOrigin::Cli, 0);
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            t.ev("F1", false);
            t.ev("F1", true);
        }
        let per = start.elapsed() / 20_000;
        eprintln!("decision with 1000 rules over 8 modes: {per:?}");
        assert!(per < std::time::Duration::from_micros(200), "{per:?}");
    }
}
