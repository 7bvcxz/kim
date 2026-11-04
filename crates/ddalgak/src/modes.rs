//! User-defined modes and layers at run time (Step 72; docs/DDALGAK_RULE_MODEL.md §5.10, §7.3;
//! ADR 0043; `DDALGAK-FR-016`, `DDALGAK-FR-020`, `DDALGAK-SEC-005`).
//!
//! - **The mode set** ([`ModeSet`]) is compiled from the active profile: up to 32 modes, the
//!   default (base) mode and each mode's optional **parent**. A mode with a parent is a **layer**:
//!   while it is active, a key without a rule of the layer itself falls back to the rules of its
//!   parent, then the parent's parent (its [`ModeSet::chain`]), then to the rules without
//!   `modes`, then to the application. A mode without a parent is **exclusive**: only its own
//!   rules and the rules without `modes` apply in it. A parent that is missing or lies on a cycle
//!   is dropped (never broader); validation reports it (`unknown_mode`, `mode_cycle`). The
//!   default mode never has a parent, a one-shot exit or a timeout: it is where every exit ends.
//! - **The mode state** ([`ModeState`]) is runtime state of the hook thread's core: the
//!   *latched* mode (`set`, `toggle`, `cycle`, `previous`, `default`), up to [`MAX_HOLDS`]
//!   *temporary layers* held by their trigger keys (`hold`), the previous latched mode, a pending
//!   one-shot exit and the idle time of a mode with a timeout. The **active** mode is the newest
//!   held layer, or else the latched mode. Every change that changes the active mode makes the
//!   core release every key and button Ddalgak holds (`DDALGAK-SEC-005`).
//! - The active mode is **never stored and never restored**: an engine start, a pause, a profile
//!   switch and an emergency off begin in the default mode (ADR 0043). [`ModeState::reset`] is
//!   the safe reset the CLI (`ddalgak mode reset`) and the tray offer.
//!
//! Everything here is allocation-free on the paths the hook callback uses (`apply`, `release`,
//! `key_down`, `key_up`); [`ModeStatus`] and the indicator are built outside the callback.

use crate::lifecycle::State;
use crate::model::{ModeIndicator, Profile};
use serde_json::{Value, json};
use std::sync::Arc;

/// Most modes of one profile (schema `profile.modes`).
pub const MAX_MODES: usize = 32;
/// Most temporary layers held at the same time (one per held trigger key).
pub const MAX_HOLDS: usize = 4;
/// The binding index of a change no rule made.
pub const NO_RULE: u16 = u16::MAX;

/// One mode as the engine sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeDef {
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    /// The mode this one is a layer over (index), after dropping missing and cyclic parents.
    pub parent: Option<u8>,
    pub one_shot: bool,
    pub timeout_ms: Option<u32>,
    /// Step 73: `false` when the user disabled the mode (`enabled: false`): rules limited to it
    /// alone are inactive and nothing switches to it. The default mode is always enabled.
    pub enabled: bool,
}

impl ModeDef {
    fn plain(id: &str, title: &str) -> ModeDef {
        ModeDef {
            id: id.into(),
            title: title.into(),
            icon: None,
            color: None,
            parent: None,
            one_shot: false,
            timeout_ms: None,
            enabled: true,
        }
    }

    /// `exclusive` (no parent) or `layer`.
    pub fn policy(&self) -> &'static str {
        if self.parent.is_some() {
            "layer"
        } else {
            "exclusive"
        }
    }
}

/// The modes of the active profile, with their precedence chains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeSet {
    modes: Vec<ModeDef>,
    default: u8,
    chains: Vec<Vec<u8>>,
}

impl Default for ModeSet {
    fn default() -> ModeSet {
        ModeSet::single()
    }
}

fn idx(i: usize) -> u8 {
    u8::try_from(i).unwrap_or(u8::MAX)
}

/// The parents (by index) of modes given as `(id, parent id)`, with missing parents, parents on
/// a cycle and the default mode's parent dropped: what the engine runs (never broader).
pub fn effective_parents(modes: &[(&str, Option<&str>)], default: usize) -> Vec<Option<usize>> {
    let index = |id: &str| modes.iter().position(|(m, _)| *m == id);
    let mut parent: Vec<Option<usize>> = modes
        .iter()
        .enumerate()
        .map(|(i, (_, p))| {
            if i == default {
                None
            } else {
                p.and_then(index)
            }
        })
        .collect();
    // A mode is on a cycle when following its parents comes back to it.
    let on_cycle: Vec<bool> = (0..modes.len())
        .map(|start| {
            let mut at = parent[start];
            for _ in 0..=modes.len() {
                match at {
                    Some(p) if p == start => return true,
                    Some(p) => at = parent[p],
                    None => return false,
                }
            }
            false
        })
        .collect();
    for (i, cyc) in on_cycle.into_iter().enumerate() {
        if cyc {
            parent[i] = None;
        }
    }
    parent
}

impl ModeSet {
    /// One mode `base` (a missing rules file, tests).
    pub fn single() -> ModeSet {
        ModeSet::build(vec![ModeDef::plain("base", "Base")], 0)
    }

    fn build(modes: Vec<ModeDef>, default: u8) -> ModeSet {
        let chains = (0..modes.len())
            .map(|m| {
                let mut chain = vec![idx(m)];
                let mut at = modes[m].parent;
                while let Some(p) = at {
                    if chain.contains(&p) || chain.len() >= modes.len() {
                        break;
                    }
                    chain.push(p);
                    at = modes.get(usize::from(p)).and_then(|d| d.parent);
                }
                chain
            })
            .collect();
        ModeSet {
            modes,
            default,
            chains,
        }
    }

    /// The modes of `profile` (the first 32; a duplicate ID resolves to its first mode).
    pub fn from_profile(profile: &Profile) -> ModeSet {
        let list: Vec<&crate::model::Mode> = profile.modes.iter().take(MAX_MODES).collect();
        let default = list
            .iter()
            .position(|m| m.id == profile.default_mode)
            .unwrap_or(0);
        let pairs: Vec<(&str, Option<&str>)> = list
            .iter()
            .map(|m| (m.id.as_str(), m.parent.as_deref()))
            .collect();
        let parents = effective_parents(&pairs, default);
        let modes = list
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let base = i == default;
                ModeDef {
                    id: m.id.clone(),
                    title: m.title.clone(),
                    icon: m.icon.clone(),
                    color: m.color.clone(),
                    parent: parents[i].map(idx),
                    one_shot: !base && m.one_shot == Some(true),
                    timeout_ms: if base {
                        None
                    } else {
                        m.timeout_ms.map(|t| t.clamp(1000, 3_600_000))
                    },
                    // The default mode is where every exit ends: never disabled (Step 73).
                    enabled: base || m.enabled != Some(false),
                }
            })
            .collect();
        ModeSet::build(modes, idx(default))
    }

    pub fn len(&self) -> usize {
        self.modes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.modes.is_empty()
    }

    /// The default (base) mode.
    pub fn default_mode(&self) -> u8 {
        self.default
    }

    pub fn get(&self, i: u8) -> Option<&ModeDef> {
        self.modes.get(usize::from(i))
    }

    pub fn modes(&self) -> &[ModeDef] {
        &self.modes
    }

    /// The ID of mode `i` (`?` for an index this set does not have).
    pub fn id(&self, i: u8) -> &str {
        self.get(i).map_or("?", |m| m.id.as_str())
    }

    pub fn title(&self, i: u8) -> &str {
        self.get(i).map_or("?", |m| m.title.as_str())
    }

    pub fn index(&self, id: &str) -> Option<u8> {
        self.modes.iter().position(|m| m.id == id).map(idx)
    }

    /// Whether mode `i` is enabled (Step 73); an index this set does not have is not.
    pub fn is_enabled(&self, i: u8) -> bool {
        self.get(i).is_some_and(|m| m.enabled)
    }

    /// `mask` without the disabled modes (Step 73): a rule limited to disabled modes only
    /// gets 0 here although its `mask` is not 0.
    pub fn enabled_mask(&self, mask: u32) -> u32 {
        self.modes
            .iter()
            .enumerate()
            .take(32)
            .filter(|(_, m)| !m.enabled)
            .fold(mask, |acc, (i, _)| acc & !(1 << i))
    }

    /// The mode and its parents, nearest first: the precedence of mode-scoped rules in it.
    pub fn chain(&self, i: u8) -> &[u8] {
        self.chains.get(usize::from(i)).map_or(&[], Vec::as_slice)
    }

    /// The bit set of `ids` (`None` when one of them is not a mode of this set).
    pub fn mask(&self, ids: &[String]) -> Option<u32> {
        ids.iter().try_fold(0u32, |m, id| {
            self.index(id)
                .filter(|i| usize::from(*i) < 32)
                .map(|i| m | (1 << i))
        })
    }

    /// How near a rule limited to `mask` is to mode `m`: 0 for a rule of `m` itself, 1 for one of
    /// its parent, …; `None` when the rule does not work in `m`. A rule without modes (`mask` 0)
    /// works in every mode after all mode-scoped rules (`Some(MAX_MODES)`).
    pub fn rank(&self, m: u8, mask: u32) -> Option<usize> {
        if mask == 0 {
            return Some(MAX_MODES);
        }
        self.chain(m)
            .iter()
            .position(|c| u32::from(*c) < 32 && mask & (1 << c) != 0)
    }

    fn one_shot(&self, m: u8) -> bool {
        self.get(m).is_some_and(|d| d.one_shot)
    }

    fn timeout(&self, m: u8) -> Option<u32> {
        self.get(m).and_then(|d| d.timeout_ms)
    }

    /// What `status`, `mode status` and the indicator show for a state.
    pub fn status(&self, v: &ModeView, rule: &dyn Fn(u16) -> Option<String>) -> ModeStatus {
        let ids = |list: &[u8]| list.iter().map(|m| self.id(*m).to_string()).collect();
        let a = self.get(v.active);
        ModeStatus {
            active: self.id(v.active).into(),
            title: self.title(v.active).into(),
            icon: a.and_then(|d| d.icon.clone()),
            color: a.and_then(|d| d.color.clone()),
            latched: self.id(v.latched).into(),
            held: ids(&v.held[..usize::from(v.held_len).min(MAX_HOLDS)]),
            previous: self.id(v.previous).into(),
            default: self.id(self.default).into(),
            chain: ids(self.chain(v.active)),
            origin: v.origin.describe(rule),
            changes: v.changes,
        }
    }

    /// The status before anything changed (an engine that is off starts in the default mode).
    pub fn initial(&self) -> ModeStatus {
        self.status(&ModeState::new(self).view(), &|_| None)
    }
}

/// A compiled `mode` step (§5.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeAction {
    Set(u8),
    Toggle(u8),
    Cycle(Arc<[u8]>),
    Hold(u8),
    Previous,
    Default,
}

impl ModeAction {
    /// Compiles a `mode` step against the profile's modes.
    pub fn compile(step: &Value, set: &ModeSet) -> Result<ModeAction, String> {
        let one = || -> Result<u8, String> {
            let id = step
                .get("mode")
                .and_then(Value::as_str)
                .ok_or("this mode operation needs `mode`")?;
            set.index(id)
                .ok_or_else(|| format!("no mode `{id}` in the profile"))
        };
        Ok(match step.get("op").and_then(Value::as_str) {
            Some("set") => ModeAction::Set(one()?),
            Some("toggle") => ModeAction::Toggle(one()?),
            Some("hold") => ModeAction::Hold(one()?),
            Some("cycle") => {
                let list = step
                    .get("modes")
                    .and_then(Value::as_array)
                    .ok_or("`cycle` needs `modes`")?;
                let modes: Vec<u8> = list
                    .iter()
                    .map(|m| {
                        let id = m.as_str().unwrap_or_default();
                        set.index(id)
                            .ok_or_else(|| format!("no mode `{id}` in the profile"))
                    })
                    .collect::<Result<_, _>>()?;
                if modes.len() < 2 {
                    return Err("`cycle` needs at least two modes".into());
                }
                ModeAction::Cycle(modes.into())
            }
            Some("previous") => ModeAction::Previous,
            Some("default") => ModeAction::Default,
            _ => return Err("unknown mode `op`".into()),
        })
    }

    pub fn op(&self) -> &'static str {
        match self {
            ModeAction::Set(_) => "set",
            ModeAction::Toggle(_) => "toggle",
            ModeAction::Cycle(_) => "cycle",
            ModeAction::Hold(_) => "hold",
            ModeAction::Previous => "previous",
            ModeAction::Default => "default",
        }
    }

    /// A temporary layer that lasts while the trigger is held.
    pub fn holds(&self) -> bool {
        matches!(self, ModeAction::Hold(_))
    }

    /// Step 73: the step can only switch to disabled modes (the rule is then `mode_disabled`).
    pub fn targets_disabled(&self, set: &ModeSet) -> bool {
        match self {
            ModeAction::Set(m) | ModeAction::Toggle(m) | ModeAction::Hold(m) => !set.is_enabled(*m),
            ModeAction::Cycle(list) => !list.iter().any(|m| set.is_enabled(*m)),
            ModeAction::Previous | ModeAction::Default => false,
        }
    }
}

/// Why the mode changed (status and the indicator name it; never keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeOrigin {
    /// The engine started (or a new core was built).
    Start,
    /// A mode rule fired (binding index).
    Rule(u16),
    /// The trigger of a held layer went up (binding index).
    Released(u16),
    Cli,
    Tray,
    /// A mode with `timeout_ms` saw no key press for that long.
    Timeout,
    /// A one-shot mode's key went up.
    OneShot,
    /// The engine was paused (the safe reset).
    Pause,
    /// New rules were loaded.
    Reload,
    /// Input went to another desktop, the session was locked or an elevated window took focus:
    /// held layers end because their key-ups may never arrive.
    KeyReset,
}

impl ModeOrigin {
    /// `start`, `rule:<id>`, `released:<id>`, `cli`, `tray`, `timeout`, `one_shot`, `pause`,
    /// `reload`, `key_reset`.
    pub fn describe(self, rule: &dyn Fn(u16) -> Option<String>) -> String {
        match self {
            ModeOrigin::Start => "start".into(),
            ModeOrigin::Rule(b) => rule(b).map_or_else(|| "rule".into(), |r| format!("rule:{r}")),
            ModeOrigin::Released(b) => {
                rule(b).map_or_else(|| "released".into(), |r| format!("released:{r}"))
            }
            ModeOrigin::Cli => "cli".into(),
            ModeOrigin::Tray => "tray".into(),
            ModeOrigin::Timeout => "timeout".into(),
            ModeOrigin::OneShot => "one_shot".into(),
            ModeOrigin::Pause => "pause".into(),
            ModeOrigin::Reload => "reload".into(),
            ModeOrigin::KeyReset => "key_reset".into(),
        }
    }
}

/// A snapshot of the mode state by index: small and `Copy`, it travels in a worker signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeView {
    pub active: u8,
    pub latched: u8,
    pub previous: u8,
    /// The held layers, oldest first.
    pub held: [u8; MAX_HOLDS],
    pub held_len: u8,
    pub origin: ModeOrigin,
    /// Counts every change of this state (a newer view has a higher count).
    pub changes: u32,
}

/// The mode state by ID, as `status` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeStatus {
    pub active: String,
    pub title: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub latched: String,
    pub held: Vec<String>,
    pub previous: String,
    pub default: String,
    /// The active mode's precedence chain (the mode, its parent, …).
    pub chain: Vec<String>,
    pub origin: String,
    pub changes: u32,
}

impl ModeStatus {
    /// The `mode` block of `status` and `mode status`. `running`: the engine is on, so this is
    /// the live state; otherwise it is the mode the engine starts in.
    pub fn to_json(&self, running: bool, since: Option<&str>) -> Value {
        json!({
            "active": self.active,
            "title": self.title,
            "icon": self.icon,
            "color": self.color,
            "latched": self.latched,
            "held": self.held,
            "previous": self.previous,
            "default": self.default,
            "chain": self.chain,
            "origin": self.origin,
            "since": since,
            "live": running,
        })
    }

    /// The same state after new rules: the latched mode by ID when it still exists (else the
    /// default), held layers ended (what the hook thread does on a reload, [`ModeState::carry`]).
    pub fn carried(&self, set: &ModeSet) -> ModeStatus {
        let latched = set
            .index(&self.latched)
            .filter(|i| set.is_enabled(*i))
            .unwrap_or_else(|| set.default_mode());
        let previous = set
            .index(&self.previous)
            .filter(|i| set.is_enabled(*i))
            .unwrap_or_else(|| set.default_mode());
        let mut st = ModeState::new(set);
        st.latched = latched;
        st.previous = previous;
        st.origin = ModeOrigin::Reload;
        st.changes = self.changes.wrapping_add(1);
        set.status(&st.view(), &|_| None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    vk: u16,
    mode: u8,
    binding: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OneShot {
    Idle,
    /// Waiting for the next key; then back to this mode.
    Armed(u8),
    /// That key went down; its up ends the mode.
    Key(u16, u8),
}

/// What a change did: the state changed, and whether the **active** mode changed (then every
/// output Ddalgak holds is released).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Change {
    pub state: bool,
    pub active: bool,
}

/// The runtime mode state (see the module documentation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModeState {
    latched: u8,
    previous: u8,
    holds: [Option<Held>; MAX_HOLDS],
    one_shot: OneShot,
    /// The last key press while a mode with a timeout is latched (event clock, ms).
    idle_since: Option<u32>,
    origin: ModeOrigin,
    changes: u32,
}

type Shape = (u8, u8, u8, [Option<Held>; MAX_HOLDS], OneShot);

impl ModeState {
    pub fn new(set: &ModeSet) -> ModeState {
        let d = set.default_mode();
        ModeState {
            latched: d,
            previous: d,
            holds: [None; MAX_HOLDS],
            one_shot: OneShot::Idle,
            idle_since: None,
            origin: ModeOrigin::Start,
            changes: 0,
        }
    }

    /// The active mode: the newest held layer, or the latched mode.
    pub fn active(&self) -> u8 {
        self.holds
            .iter()
            .rev()
            .flatten()
            .next()
            .map_or(self.latched, |h| h.mode)
    }

    pub fn latched(&self) -> u8 {
        self.latched
    }

    pub fn previous(&self) -> u8 {
        self.previous
    }

    /// How many layers are held now.
    pub fn held(&self) -> usize {
        self.holds.iter().flatten().count()
    }

    pub fn view(&self) -> ModeView {
        let mut held = [0u8; MAX_HOLDS];
        let mut n = 0;
        for h in self.holds.iter().flatten() {
            held[n] = h.mode;
            n += 1;
        }
        ModeView {
            active: self.active(),
            latched: self.latched,
            previous: self.previous,
            held,
            held_len: idx(n),
            origin: self.origin,
            changes: self.changes,
        }
    }

    fn shape(&self) -> Shape {
        (
            self.active(),
            self.latched,
            self.previous,
            self.holds,
            self.one_shot,
        )
    }

    fn finish(&mut self, before: Shape, origin: ModeOrigin) -> Change {
        let after = self.shape();
        let c = Change {
            state: before.0 != after.0
                || before.1 != after.1
                || before.2 != after.2
                || before.3 != after.3,
            active: before.0 != after.0,
        };
        if c.state {
            self.origin = origin;
            self.changes = self.changes.wrapping_add(1);
        }
        c
    }

    fn clear_holds(&mut self) {
        self.holds = [None; MAX_HOLDS];
    }

    /// Latches `to`: held layers end, the old latched mode becomes the previous one, and a
    /// one-shot or timed mode starts waiting (unless this is an exit, `arm == false`).
    fn latch(&mut self, set: &ModeSet, to: u8, now: u32, arm: bool) {
        self.clear_holds();
        if to != self.latched {
            self.previous = self.latched;
        }
        let back = self.latched;
        self.latched = to;
        self.one_shot = if arm && to != back && set.one_shot(to) {
            OneShot::Armed(back)
        } else {
            OneShot::Idle
        };
        self.idle_since = set.timeout(to).map(|_| now);
    }

    fn push_hold(&mut self, h: Held) -> bool {
        // A key that is already holding a layer (a stale pairing) gives it up first.
        for s in &mut self.holds {
            if s.is_some_and(|x| x.vk == h.vk) {
                *s = None;
            }
        }
        self.compact();
        match self.holds.iter_mut().find(|s| s.is_none()) {
            Some(s) => {
                *s = Some(h);
                true
            }
            None => false,
        }
    }

    fn compact(&mut self) {
        let mut out = [None; MAX_HOLDS];
        for (i, h) in self.holds.iter().flatten().enumerate() {
            out[i] = Some(*h);
        }
        self.holds = out;
    }

    /// Runs a mode step of the rule `binding`, fired by the key `vk` (the hook callback).
    /// Returns `None` when a hold finds no free layer slot (the key then passes unchanged).
    pub fn apply(
        &mut self,
        set: &ModeSet,
        action: &ModeAction,
        vk: u16,
        binding: u16,
        now: u32,
    ) -> Option<Change> {
        let before = self.shape();
        let active = self.active();
        // Nothing switches to a disabled mode (Step 73); switching away from one still works.
        match action {
            ModeAction::Set(m) | ModeAction::Hold(m) if !set.is_enabled(*m) => return None,
            ModeAction::Toggle(m) if active != *m && !set.is_enabled(*m) => return None,
            ModeAction::Cycle(list) if !list.iter().any(|m| set.is_enabled(*m)) => return None,
            _ => {}
        }
        match action {
            ModeAction::Set(m) => self.latch(set, *m, now, true),
            ModeAction::Toggle(m) => {
                if active == *m {
                    if self.held() > 0 && self.latched != *m {
                        // The mode is a held layer: toggling it off ends the layers.
                        self.clear_holds();
                    } else {
                        let back = if self.previous == *m {
                            set.default_mode()
                        } else {
                            self.previous
                        };
                        self.latch(set, back, now, false);
                    }
                } else {
                    self.latch(set, *m, now, true);
                }
            }
            ModeAction::Cycle(list) => {
                // The next enabled mode after the active one (disabled modes are skipped).
                let start = list
                    .iter()
                    .position(|m| *m == active)
                    .unwrap_or(list.len() - 1);
                let next = (1..=list.len())
                    .map(|k| list[(start + k) % list.len()])
                    .find(|m| set.is_enabled(*m))
                    .unwrap_or(set.default_mode());
                self.latch(set, next, now, true);
            }
            ModeAction::Hold(m) => {
                if !self.push_hold(Held {
                    vk,
                    mode: *m,
                    binding,
                }) {
                    return None;
                }
            }
            ModeAction::Previous => {
                if self.held() > 0 {
                    self.clear_holds();
                } else {
                    let back = self.previous;
                    self.latch(set, back, now, false);
                }
            }
            ModeAction::Default => self.latch(set, set.default_mode(), now, false),
        }
        Some(self.finish(before, ModeOrigin::Rule(binding)))
    }

    /// A change asked for outside the keyboard (the CLI, the tray, a rule the worker timed).
    /// A `hold` needs a held key and is refused (`None`).
    pub fn request(
        &mut self,
        set: &ModeSet,
        action: &ModeAction,
        origin: ModeOrigin,
        now: u32,
    ) -> Option<Change> {
        if action.holds() {
            return None;
        }
        let c = self.apply(set, action, 0, NO_RULE, now)?;
        if c.state {
            self.origin = origin;
        }
        Some(c)
    }

    /// The key `vk` went up: a layer it held ends.
    pub fn release(&mut self, vk: u16) -> Change {
        let before = self.shape();
        let mut binding = NO_RULE;
        for s in &mut self.holds {
            if let Some(h) = s
                && h.vk == vk
            {
                binding = h.binding;
                *s = None;
            }
        }
        self.compact();
        self.finish(before, ModeOrigin::Released(binding))
    }

    /// Whether the key `vk` holds a layer.
    pub fn holds_key(&self, vk: u16) -> bool {
        self.holds.iter().flatten().any(|h| h.vk == vk)
    }

    /// A key went down (before it is decided): the idle time of a timed mode starts again, and a
    /// waiting one-shot mode takes this key (not a modifier) as its one key.
    pub fn key_down(&mut self, vk: u16, modifier: bool, now: u32) {
        if self.idle_since.is_some() {
            self.idle_since = Some(now);
        }
        if let OneShot::Armed(back) = self.one_shot
            && !modifier
            && self.held() == 0
        {
            self.one_shot = OneShot::Key(vk, back);
        }
    }

    /// A key went up (after it was decided): the up of a one-shot mode's key ends the mode.
    pub fn key_up(&mut self, set: &ModeSet, vk: u16, now: u32) -> Change {
        let OneShot::Key(k, back) = self.one_shot else {
            return Change::default();
        };
        if k != vk {
            return Change::default();
        }
        let before = self.shape();
        self.latch(set, back, now, false);
        self.finish(before, ModeOrigin::OneShot)
    }

    /// Milliseconds until a timed mode ends, if one is latched.
    pub fn due_in(&self, set: &ModeSet, now: u32) -> Option<u32> {
        let since = self.idle_since?;
        let t = set.timeout(self.latched)?;
        Some(t.saturating_sub(now.wrapping_sub(since)))
    }

    /// The timer fired: a timed mode idle for its time returns to the default mode. While a
    /// layer is held the user is still at it, so the time starts again.
    pub fn tick(&mut self, set: &ModeSet, now: u32) -> Change {
        if self.due_in(set, now) != Some(0) {
            return Change::default();
        }
        if self.held() > 0 {
            self.idle_since = Some(now);
            return Change::default();
        }
        let before = self.shape();
        self.latch(set, set.default_mode(), now, false);
        self.finish(before, ModeOrigin::Timeout)
    }

    /// Held layers end (their key-ups may never arrive) and a one-shot mode stops waiting for a
    /// key-up it may never see; the latched mode stays.
    pub fn drop_holds(&mut self, origin: ModeOrigin) -> Change {
        let before = self.shape();
        self.clear_holds();
        if let OneShot::Key(_, back) = self.one_shot {
            self.one_shot = OneShot::Armed(back);
        }
        self.finish(before, origin)
    }

    /// The safe reset: the default mode, nothing held, nothing remembered.
    pub fn reset(&mut self, set: &ModeSet, origin: ModeOrigin) -> Change {
        let before = self.shape();
        let d = set.default_mode();
        self.clear_holds();
        self.latched = d;
        self.previous = d;
        self.one_shot = OneShot::Idle;
        self.idle_since = None;
        self.finish(before, origin)
    }

    /// The state for new rules (a reload): the latched and previous modes by ID when they still
    /// exist (else the default), held layers and a pending one-shot exit ended.
    pub fn carry(&self, old: &ModeSet, new: &ModeSet, now: u32) -> ModeState {
        // A mode that is gone or was disabled (Step 73) gives way to the default mode.
        let map = |m: u8| {
            new.index(old.id(m))
                .filter(|i| new.is_enabled(*i))
                .unwrap_or_else(|| new.default_mode())
        };
        let latched = map(self.latched);
        let mut st = ModeState::new(new);
        st.latched = latched;
        st.previous = map(self.previous);
        st.idle_since = new.timeout(latched).map(|_| now);
        st.changes = self.changes;
        st.origin = self.origin;
        let moved = old.id(self.active()) != new.id(st.active())
            || old.id(self.latched) != new.id(latched)
            || self.held() > 0;
        if moved {
            st.origin = ModeOrigin::Reload;
            st.changes = self.changes.wrapping_add(1);
        }
        st
    }
}

/// The playground preview of a mode step (`ddalgak-simulation-v1` `preview_mode`): the active
/// mode before and after, and `until_release` for a held layer.
pub fn preview(set: &ModeSet, state: &ModeState, action: &ModeAction) -> Value {
    let mut after = state.clone();
    let _ = after.apply(set, action, 0, NO_RULE, 0);
    let mut v = json!({ "from": set.id(state.active()), "to": set.id(after.active()) });
    if action.holds() {
        v["until_release"] = json!(true);
    }
    v
}

/// The editor's summary sentence of a mode step (GUI_JOURNEYS J4): mode titles, never IDs.
pub fn summary(set: &ModeSet, action: &ModeAction, trigger: &str) -> String {
    let t = |m: u8| set.title(m).to_string();
    match action {
        ModeAction::Set(m) => format!("{trigger} switches to {}", t(*m)),
        ModeAction::Toggle(m) => format!(
            "{trigger} switches to {} and, pressed again, back to the previous mode",
            t(*m)
        ),
        ModeAction::Cycle(list) => format!(
            "{trigger} cycles through {}",
            list.iter().map(|m| t(*m)).collect::<Vec<_>>().join(", ")
        ),
        ModeAction::Hold(m) => format!(
            "{} is active while {trigger} is held, then the previous mode returns",
            t(*m)
        ),
        ModeAction::Previous => format!("{trigger} returns to the previous mode"),
        ModeAction::Default => format!("{trigger} returns to {}", t(set.default_mode())),
    }
}

/// What the tray and a compact indicator show (`settings.mode_indicator`, ADR 0043). The title
/// is always there: the icon and the color are never the only cue (`DDALGAK-NFR-007`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Indicator {
    /// Show the mode at all (off by default where it is not needed: a profile with one mode).
    pub visible: bool,
    /// The active mode is not the default mode (the tray icon then has its own shape).
    pub away: bool,
    pub title: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    /// `N Navigation`, or the title alone.
    pub label: String,
}

/// The indicator for a mode status.
pub fn indicator(setting: Option<ModeIndicator>, set: &ModeSet, s: &ModeStatus) -> Indicator {
    let visible = match setting.unwrap_or(ModeIndicator::Auto) {
        ModeIndicator::Off => false,
        ModeIndicator::Tray => true,
        ModeIndicator::Auto => set.len() > 1,
    };
    let label = match &s.icon {
        Some(i) => format!("{i} {}", s.title),
        None => s.title.clone(),
    };
    Indicator {
        visible,
        away: s.active != s.default,
        title: s.title.clone(),
        icon: s.icon.clone(),
        color: s.color.clone(),
        label,
    }
}

impl Indicator {
    pub fn to_json(&self) -> Value {
        json!({
            "visible": self.visible,
            "away_from_default": self.away,
            "title": self.title,
            "icon": self.icon,
            "color": self.color,
            "label": self.label,
        })
    }
}

/// The tray tooltip (at most 127 characters, the notification area's limit): the engine state in
/// words and, when the indicator is visible, the active mode.
pub fn tooltip(engine: State, ind: Option<&Indicator>) -> String {
    let base = match engine {
        State::Off => "Ddalgak: engine off",
        State::Running => "Ddalgak: engine on",
        State::Paused => "Ddalgak: engine paused",
    };
    let mut s = match ind {
        Some(i) if i.visible && engine != State::Off => format!("{base} · mode {}", i.label),
        _ => base.to_string(),
    };
    if s.chars().count() > 127 {
        s = s.chars().take(126).collect::<String>() + "…";
    }
    s
}

/// One entry of the mode menu (the tray's `Mode ▸` submenu and the editor header's switcher).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEntry {
    pub id: String,
    /// `Navigation (layer over Base)`, `Base (default)`.
    pub label: String,
    pub checked: bool,
}

/// The mode menu: every enabled mode, the active one checked, in profile order (a disabled mode
/// is not offered, Step 73).
pub fn menu(set: &ModeSet, s: &ModeStatus) -> Vec<MenuEntry> {
    set.modes()
        .iter()
        .enumerate()
        .filter(|(_, m)| m.enabled)
        .map(|(i, m)| {
            let mut label = match &m.icon {
                Some(icon) => format!("{icon} {}", m.title),
                None => m.title.clone(),
            };
            if idx(i) == set.default_mode() {
                label.push_str(" (default)");
            } else if let Some(p) = m.parent {
                label.push_str(&format!(" (layer over {})", set.title(p)));
            }
            MenuEntry {
                id: m.id.clone(),
                label,
                checked: m.id == s.active,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
