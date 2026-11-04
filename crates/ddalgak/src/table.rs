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

use crate::hotstring;
use crate::modes::ModeSet;
use crate::remap::Output;
use crate::run::{KeysSpec, TextSpec};
use crate::trigger::{TKey, Trigger};
use crate::vk::{self, ModKey};
use kim_hotkeys::keys::Mods;
use std::sync::Arc;

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
/// blocks, Step 69 typed text and sent keys, Step 70 program and website launches, Step 71 mouse
/// moves, wheel, clicks and holds, Step 72 mode switches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Switches, toggles, cycles or holds a mode (§5.10, Step 72). The core runs it in the hook
    /// callback, so the very next key is decided in the new mode; nothing is injected.
    Mode(crate::modes::ModeAction),
    Engine(Vec<EngineOp>),
    /// The trigger acts as another key, chord or button while held (§5.1). The core decides the
    /// output events; the worker sends them.
    Remap(RemapSpec),
    /// The trigger is consumed and nothing happens (§5.2).
    Block,
    /// Types a text (§5.4, Step 69): a run on the worker.
    Type(Arc<TextSpec>),
    /// Taps chords in order (§5.3, Step 69): a run on the worker.
    Keys(Arc<KeysSpec>),
    /// Opens programs and websites in order (§5.5, §5.6, Step 70): a launch thread per run.
    Launch(Arc<crate::launch::Plan>),
    /// Mouse moves, clicks and wheel notches in order (§5.7–§5.9, Step 71): one short run on the
    /// worker.
    Mouse(Arc<crate::mouse::MousePlan>),
    /// A mouse button held exactly as long as the trigger (`mouse_click` with `op: hold`, Step
    /// 71): a remap whose target is the button (the core's reference-counted slots, released on
    /// every path), moved to `at` first when the rule names a point.
    MouseHold {
        spec: RemapSpec,
        at: Option<crate::mouse::PointSpec>,
    },
    /// A `mouse_move` with `while_held` (Step 71): the pointer moves while the trigger is held.
    Motion(crate::mouse::MotionSpec),
}

impl Action {
    /// A typing run (`type_text`, `send_keys`).
    pub fn is_run(&self) -> bool {
        matches!(self, Action::Type(_) | Action::Keys(_))
    }

    /// The outputs the core holds while the trigger is down (a remap, a held mouse button).
    pub fn remap_spec(&self) -> Option<RemapSpec> {
        match self {
            Action::Remap(r) | Action::MouseHold { spec: r, .. } => Some(*r),
            _ => None,
        }
    }
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
    /// A hotstring trigger (Step 69): `trigger` is then [`Trigger::none`] and the core matches
    /// typed characters.
    pub hotstring: Option<Arc<hotstring::Spec>>,
    /// The modes the rule is limited to, as bits of the profile's mode indexes (Step 72); `0`:
    /// every mode.
    pub modes: u32,
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
    /// A held motion (`mouse_move` with `while_held`, Step 71): the core tells the worker when
    /// its trigger goes up.
    pub motion: bool,
    /// A mode step (Step 72): the core runs it itself ([`Table::mode_action`]).
    pub mode: bool,
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
    /// The hotstring bindings in precedence order, with their binding index (every mode).
    pub(crate) hotstrings: Vec<(u16, Arc<hotstring::Spec>)>,
    /// The longest active abbreviation (the bound of the core's buffer).
    pub(crate) longest_hotstring: usize,
    /// Per-rule rate limit (`settings.max_runs_per_second`, 1–100).
    pub max_runs_per_second: u32,
    /// The active profile's modes (Step 72).
    pub(crate) modes: ModeSet,
    /// Per mode: the bindings that work in it, in precedence order (DDALGAK_RULE_MODEL §7.1:
    /// rules of the mode itself, then of its parent, …, then rules without modes; within each,
    /// higher priority, then file order). The core scans only the active mode's list.
    pub(crate) order: Vec<Vec<u16>>,
    /// Per mode: the hotstrings that work in it, in the same precedence.
    pub(crate) mode_hotstrings: Vec<Vec<(u16, Arc<hotstring::Spec>)>>,
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
    /// Compiles bindings given in precedence order for a profile with one mode (tests, a missing
    /// rules file). At most [`MAX_BINDINGS`]; the rest are dropped and reported by the caller.
    pub fn compile(bindings: Vec<Binding>, max_runs_per_second: u32) -> Table {
        Table::compile_modes(bindings, ModeSet::single(), max_runs_per_second)
    }

    /// Compiles bindings given in priority and file order for the profile's `modes`: each mode
    /// gets its own precedence list over its chain (Step 72).
    pub fn compile_modes(
        bindings: Vec<Binding>,
        modes: ModeSet,
        max_runs_per_second: u32,
    ) -> Table {
        let mut t = Table::compile_flat(bindings, max_runs_per_second);
        t.order = (0..modes.len())
            .map(|m| {
                let m = u8::try_from(m).unwrap_or(u8::MAX);
                let mut ranked: Vec<(usize, u16)> = t
                    .bindings
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| {
                        let rank = modes.rank(m, b.modes)?;
                        Some((rank, u16::try_from(i).ok()?))
                    })
                    .collect();
                // A stable sort keeps priority and file order inside each rank.
                ranked.sort_by_key(|(rank, _)| *rank);
                ranked.into_iter().map(|(_, i)| i).collect()
            })
            .collect();
        t.mode_hotstrings = t
            .order
            .iter()
            .map(|list| {
                list.iter()
                    .filter_map(|i| {
                        let h = t.bindings[usize::from(*i)].hotstring.clone()?;
                        Some((*i, h))
                    })
                    .collect()
            })
            .collect();
        t.modes = modes;
        t
    }

    fn compile_flat(bindings: Vec<Binding>, max_runs_per_second: u32) -> Table {
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
                    is_remap: b.action.remap_spec().is_some(),
                    motion: matches!(b.action, Action::Motion(_)),
                    mode: matches!(b.action, Action::Mode(_)),
                }
            })
            .collect();
        let remaps = bindings.iter().map(|b| b.action.remap_spec()).collect();
        let hotstrings: Vec<(u16, Arc<hotstring::Spec>)> = bindings
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let h = b.hotstring.clone()?;
                Some((u16::try_from(i).ok()?, h))
            })
            .collect();
        let longest_hotstring = hotstrings
            .iter()
            .map(|(_, h)| h.abbreviation.len())
            .max()
            .unwrap_or(0);
        let all: Vec<u16> = (0..bindings.len())
            .filter_map(|i| u16::try_from(i).ok())
            .collect();
        Table {
            compiled,
            bindings,
            remaps,
            mode_hotstrings: vec![hotstrings.clone()],
            hotstrings,
            longest_hotstring,
            max_runs_per_second: max_runs_per_second.clamp(1, 100),
            modes: ModeSet::single(),
            order: vec![all],
        }
    }

    pub fn len(&self) -> usize {
        self.compiled.len()
    }

    /// The profile's modes.
    pub fn modes(&self) -> &ModeSet {
        &self.modes
    }

    /// The bindings that work in mode `m`, in precedence order.
    pub fn order(&self, m: u8) -> &[u16] {
        self.order.get(usize::from(m)).map_or(&[], Vec::as_slice)
    }

    /// The hotstrings that work in mode `m`, in precedence order.
    pub fn hotstrings_in(&self, m: u8) -> &[(u16, Arc<hotstring::Spec>)] {
        self.mode_hotstrings
            .get(usize::from(m))
            .map_or(&[], Vec::as_slice)
    }

    /// The mode step of binding `index`, if it is a mode rule.
    pub fn mode_action(&self, index: usize) -> Option<&crate::modes::ModeAction> {
        match self.bindings.get(index).map(|b| &b.action) {
            Some(Action::Mode(a)) => Some(a),
            _ => None,
        }
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

    /// Whether a hotstring rule is active in any mode.
    pub fn has_hotstrings(&self) -> bool {
        !self.hotstrings.is_empty()
    }

    /// Whether a hotstring rule works in mode `m` (only then does the core translate keys and
    /// keep its bounded buffer).
    pub fn has_hotstrings_in(&self, m: u8) -> bool {
        !self.hotstrings_in(m).is_empty()
    }
}
