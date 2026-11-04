//! Compiled trigger tables (DDALGAK_SAFETY §7: "decides synchronously only whether the event is
//! consumed, using precomputed trigger tables").
//!
//! A [`Binding`] is what the engine needs from one active rule: its trigger with `on`, `repeat`,
//! `pass_through` and the injected-input policy, whether it is an engine-control rule (which keeps
//! working while paused), whether it injects input (skipped over elevated windows) and, for a
//! remap (Step 68), its compiled outputs. Bindings are given in precedence order
//! (DDALGAK_RULE_MODEL §7.1; the loader sorts them). [`Table::compile`] turns them into
//! fixed-size records with virtual keys and scan codes, so the hook callback compares numbers and
//! never allocates.

use crate::remap::Output;
use crate::trigger::{TKey, Trigger};
use crate::vk::{self, ModKey};
use kim_hotkeys::keys::Mods;

/// When a trigger fires (DDALGAK_RULE_MODEL §3.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum On {
    Press,
    /// The chord was released with no other key in between (a tap).
    Release,
    /// The chord stayed down for this many milliseconds (200–5000).
    Hold(u32),
}

/// Whether other programs' injected input may trigger a rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Injected {
    Ignore,
    Accept,
}

/// An engine-control step (DDALGAK_RULE_MODEL §5.11). There is no "turn on": no rule can.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineOp {
    Pause,
    Resume,
    TogglePause,
    Disable,
}

impl EngineOp {
    pub fn parse(s: &str) -> Option<EngineOp> {
        Some(match s {
            "pause" => EngineOp::Pause,
            "resume" => EngineOp::Resume,
            "toggle_pause" => EngineOp::TogglePause,
            "disable" => EngineOp::Disable,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EngineOp::Pause => "pause",
            EngineOp::Resume => "resume",
            EngineOp::TogglePause => "toggle_pause",
            EngineOp::Disable => "disable",
        }
    }
}

/// A compiled remap (DDALGAK_RULE_MODEL §5.1): the target, the optional dual-role tap output and
/// its time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RemapSpec {
    pub to: Output,
    pub tap: Option<Output>,
    /// 100–1000 ms (default 200); only with `tap`.
    pub tap_ms: u32,
}

/// What a binding does when it fires. Step 66 runs engine-control rules, Step 68 remaps and
/// blocks; the other action types arrive in Steps 69–72 and are inactive until then (never
/// broader, §9).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Engine(Vec<EngineOp>),
    /// The trigger acts as another key, chord or button while held (§5.1). The core decides the
    /// output events; the worker sends them.
    Remap(RemapSpec),
    /// The trigger is consumed and nothing happens (§5.2).
    Block,
}

/// One active rule as the engine sees it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binding {
    /// The rule ID (activity entries name it; keys are never recorded).
    pub rule: String,
    pub trigger: Trigger,
    pub on: On,
    pub repeat: bool,
    pub pass_through: bool,
    pub injected: Injected,
    /// `remap`/`block` semantics: a trigger without modifiers matches with any modifiers held,
    /// except where that would form a reserved chord (§3.2); an exact chord of another rule wins
    /// over the widened match.
    pub widen: bool,
    /// The rule injects input (skipped while an elevated window has focus).
    pub injects: bool,
    pub action: Action,
}

impl Binding {
    /// An engine-control rule: every step is an `engine` step.
    pub fn is_engine_control(&self) -> bool {
        matches!(self.action, Action::Engine(_))
    }
}

/// How one chord's key is recognized.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyMatch {
    /// Virtual key; `Some(ext)` also requires the extended flag (NumpadEnter versus Enter).
    Vk(u16, Option<bool>),
    /// Scan code (low byte) of a physical key.
    Scan(u16),
    /// A bare modifier key.
    Modifier(ModKey),
}

/// One compiled chord.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CChord {
    pub key: KeyMatch,
    pub mods: Mods,
}

/// One compiled binding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Compiled {
    pub steps: [CChord; 4],
    pub len: u8,
    pub on: On,
    pub repeat: bool,
    pub pass_through: bool,
    pub injected: Injected,
    pub widen: bool,
    pub injects: bool,
    pub engine_control: bool,
    /// A remap: its outputs are in [`Table::remap`] (kept out of this record so the scan over
    /// every binding stays small and cache-friendly).
    pub is_remap: bool,
}

/// Most bindings a table holds (one profile's rule limit, DDALGAK_RULE_MODEL §2).
pub const MAX_BINDINGS: usize = 1000;

/// A compiled table plus the per-rule data the worker needs.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub(crate) compiled: Vec<Compiled>,
    pub(crate) bindings: Vec<Binding>,
    /// The compiled outputs of each remap binding (`None` for other bindings).
    pub(crate) remaps: Vec<Option<RemapSpec>>,
    /// Per-rule rate limit (`settings.max_runs_per_second`, 1–100).
    pub max_runs_per_second: u32,
}

fn compile_chord(c: &crate::trigger::TChord) -> CChord {
    let key = match c.key {
        TKey::Named(n) | TKey::Lock(n) => {
            KeyMatch::Vk(vk::named_vk(n).unwrap_or(0), vk::named_needs_extended(n))
        }
        TKey::Code(code) => KeyMatch::Scan(vk::code_scan(code).map_or(0, |(s, _)| s)),
        TKey::Modifier(m) => KeyMatch::Modifier(m),
        TKey::Mouse(b) => KeyMatch::Vk(b.vk(), None),
    };
    CChord { key, mods: c.mods }
}

impl Table {
    /// Compiles bindings given in precedence order. At most [`MAX_BINDINGS`]; the rest are
    /// dropped and reported by the caller.
    pub fn compile(bindings: Vec<Binding>, max_runs_per_second: u32) -> Table {
        let mut bindings = bindings;
        bindings.truncate(MAX_BINDINGS);
        let _ = vk::vk_name(0, false); // build the name table outside any hook callback
        let compiled = bindings
            .iter()
            .map(|b| {
                let mut steps = [CChord {
                    key: KeyMatch::Vk(0, None),
                    mods: Mods::default(),
                }; 4];
                for (i, s) in b.trigger.steps().iter().take(4).enumerate() {
                    steps[i] = compile_chord(s);
                }
                Compiled {
                    steps,
                    len: u8::try_from(b.trigger.steps().len().min(4)).unwrap_or(4),
                    on: b.on,
                    repeat: b.repeat,
                    pass_through: b.pass_through,
                    injected: b.injected,
                    widen: b.widen,
                    injects: b.injects,
                    engine_control: b.is_engine_control(),
                    is_remap: matches!(b.action, Action::Remap(_)),
                }
            })
            .collect();
        let remaps = bindings
            .iter()
            .map(|b| match &b.action {
                Action::Remap(r) => Some(*r),
                _ => None,
            })
            .collect();
        Table {
            compiled,
            bindings,
            remaps,
            max_runs_per_second: max_runs_per_second.clamp(1, 100),
        }
    }

    pub fn len(&self) -> usize {
        self.compiled.len()
    }

    /// The compiled outputs of binding `index`, if it is a remap.
    pub fn remap(&self, index: usize) -> Option<RemapSpec> {
        self.remaps.get(index).copied().flatten()
    }

    pub fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    pub fn binding(&self, index: usize) -> Option<&Binding> {
        self.bindings.get(index)
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Whether a binding has a mouse-button trigger: only then is the low-level mouse hook
    /// installed (Step 68, ADR 0031; disclosed as the `mouse-hook` permission).
    pub fn has_mouse(&self) -> bool {
        self.bindings.iter().any(|b| b.trigger.mouse().is_some())
    }
}
