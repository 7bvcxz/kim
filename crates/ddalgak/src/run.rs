//! Typing runs: `type_text`, `send_keys` and hotstring replacements (docs/DDALGAK_RULE_MODEL.md
//! §5.3, §5.4, §3.4; `DDALGAK-FR-011`, `DDALGAK-FR-012`, `DDALGAK-FR-070`, `DDALGAK-SEC-002`,
//! `DDALGAK-SEC-005`, `DDALGAK-SEC-007`, `DDALGAK-SEC-011`; ADR 0034).
//!
//! A run is a small plan of [`Piece`]s that the worker carries out one at a time, between its
//! other messages, so remaps keep working while text is typed and a pause, a stop, the emergency
//! chord or a reload ends the run at the next piece (`cancelled`). Before the first piece the
//! worker asks the platform about the focused target ([`Probe`]); before **every** call that
//! sends keys it asks again and stops when
//!
//! | The target | Outcome |
//! | --- | --- |
//! | the session is locked or the secure desktop is shown | `skipped_secure_desktop` |
//! | is a password field (`ES_PASSWORD`) | `skipped_password_field` |
//! | belongs to a process with a higher integrity level (UIPI would drop the input silently) | `skipped_elevated_target` |
//! | has an IME composition (or, for hotstrings, the IME is in native mode such as Hangul) | `skipped_ime_composition` |
//! | no control has keyboard focus, or it is disabled or read-only | `skipped_disabled_field` |
//! | another window or control took focus since the run started | `focus_changed`: nothing more is typed, never into the new window |
//! | `SendInput` inserted fewer events than asked | `injection_failed` |
//!
//! **Modifiers.** Before typing, the modifiers the application sees held (the trigger's
//! `Ctrl+Shift`, the Shift of an end character `!`) are released logically, with a masking key
//! before an Alt is released so no menu bar opens; afterwards Ctrl and Shift are pressed again if
//! they are still physically held (or held by a remap). Alt and Win are not pressed again: a lone
//! Alt release would open the menu bar; the core consumes their physical release instead
//! ([`RunShared::lifted`]).
//!
//! **Paste** (`method: paste`): the clipboard is opened (retried for up to 200 ms; then
//! `clipboard_busy`), every format on it is copied ([`Clipboard::snapshot`]; a format that cannot
//! be copied faithfully refuses the paste with `clipboard_not_restorable` and leaves the clipboard
//! untouched), the text is put on it (excluded from the clipboard history and cloud clipboard),
//! Ctrl+V is pressed, and after [`PASTE_SETTLE_MS`] the copy is put back, unless another program
//! changed the clipboard meanwhile (its newer content wins). A restore that cannot open the
//! clipboard within 500 ms ends with `clipboard_restore_failed` (the snippet stays on it). Every
//! way out of a run (cancel, emergency, focus change, failure) restores the clipboard first.
//!
//! **Hotstrings.** The key that completed the abbreviation was consumed by the core; the run
//! erases the abbreviation with Backspace, types the replacement and types the end character
//! again (the same key with the same modifiers), unless the rule omits it. When nothing is typed
//! (busy, a skip, a reload), the consumed key is given back unchanged. When the user typed
//! another key between the trigger and the first Backspace, the run is `cancelled` without
//! erasing anything, so it never deletes what the user typed meanwhile.
//!
//! Runs record only the rule ID and the outcome; the text never leaves the plan.

use crate::inject::{Backend, Injector, OutCode, OutEvent};
use crate::remap::Output;
use crate::text::{self, CaseForm};
use crate::time::LocalTime;
use crate::vk::ModKey;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// How long after Ctrl+V the previous clipboard content is put back. The target application reads
/// the clipboard while it handles Ctrl+V; restoring earlier would paste the old content.
pub const PASTE_SETTLE_MS: u32 = 500;
/// The longest run (DDALGAK_RULE_MODEL §5.12): a longer one ends with `timeout`.
pub const MAX_RUN_MS: u64 = 60_000;
/// The masking key sent before a logically released Alt (unassigned `0xE8`, as AutoHotkey's
/// default menu mask key), so the application does not see a lone Alt press and release.
pub const MASK_VK: u16 = 0xE8;
const CLIP_OPEN_TRIES: u32 = 10;
const CLIP_RESTORE_TRIES: u32 = 25;
const CLIP_RETRY_MS: u32 = 20;

/// State shared between the engine core (hook thread) and the worker's runs. Numbers only:
/// modifier bits and a count of key presses, never which keys (`DDALGAK-SEC-001`).
#[derive(Debug, Default)]
pub struct RunShared {
    /// The modifier keys the application sees held ([`ModKey::bit`] bits), as of the last event.
    pub visible: AtomicU8,
    /// The modifier keys held physically.
    pub phys: AtomicU8,
    /// Modifier keys a run released logically: the core consumes their physical release (the
    /// application already saw it) and forgets the bit when the key is pressed again.
    pub lifted: AtomicU8,
    /// Physical key presses so far (a hotstring is cancelled when the user typed on).
    pub typed: AtomicU32,
}

/// What the platform reports about the focused target right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Target {
    /// The foreground window and the focused control, as an opaque token.
    pub focus: u128,
    pub secure_desktop: bool,
    pub password_field: bool,
    pub elevated: bool,
    pub ime_composing: bool,
    /// The IME is open in a native-language mode (Hangul); asked only for hotstrings.
    pub ime_native: bool,
    /// A control has keyboard focus and is enabled and not read-only.
    pub editable: bool,
}

/// The platform's view of the focused target (Windows: `GetForegroundWindow`,
/// `GetGUIThreadInfo`, window styles, process integrity and the IME window). Read-only.
pub trait Probe: Send {
    fn sample(&mut self, ime_mode: bool) -> Target;
}

/// A copied clipboard format.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ClipData {
    /// A memory block (`HGLOBAL`): text, HTML, RTF, DIB images, file lists, registered formats.
    Bytes(Vec<u8>),
    /// An enhanced metafile's bits (`CF_ENHMETAFILE`).
    Metafile(Vec<u8>),
}

/// A copy of everything on the clipboard (empty when the clipboard was empty).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    pub formats: Vec<(u32, ClipData)>,
}

/// Why the clipboard could not be copied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipError {
    /// A format that cannot be copied faithfully (a GDI handle other than a metafile, an
    /// owner-drawn or private format) or more than the size limit.
    NotRestorable,
}

/// The system clipboard (Windows: `OpenClipboard` with the app's window). Tests use
/// [`FakeClipboard`]; the debug test host uses [`NoClipboard`].
pub trait Clipboard: Send {
    /// Opens the clipboard; `false` when another program holds it open.
    fn open(&mut self) -> bool;
    fn close(&mut self);
    fn snapshot(&mut self) -> Result<Snapshot, ClipError>;
    /// Replaces the contents with `text`, marked so clipboard history and cloud sync skip it.
    fn set_text(&mut self, text: &str) -> bool;
    /// Puts a copy back (also marked so clipboard history does not record it a second time).
    fn restore(&mut self, s: &Snapshot) -> bool;
    /// `GetClipboardSequenceNumber`: changes with every change of the clipboard.
    fn sequence(&mut self) -> u32;
}

/// A clipboard that is always busy: the debug test host never touches the real clipboard.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoClipboard;

impl Clipboard for NoClipboard {
    fn open(&mut self) -> bool {
        false
    }
    fn close(&mut self) {}
    fn snapshot(&mut self) -> Result<Snapshot, ClipError> {
        Ok(Snapshot::default())
    }
    fn set_text(&mut self, _text: &str) -> bool {
        false
    }
    fn restore(&mut self, _s: &Snapshot) -> bool {
        false
    }
    fn sequence(&mut self) -> u32 {
        0
    }
}

/// An in-memory clipboard for tests (shared with the test through an `Arc`).
#[derive(Clone, Debug, Default)]
pub struct FakeClipboard(pub std::sync::Arc<std::sync::Mutex<FakeState>>);

/// The fake clipboard's state.
#[derive(Clone, Debug, Default)]
pub struct FakeState {
    pub formats: Vec<(u32, ClipData)>,
    pub seq: u32,
    pub open: bool,
    /// The next `busy` opens fail (another program holds the clipboard).
    pub busy: u32,
    /// The contents cannot be copied faithfully.
    pub not_restorable: bool,
    /// Every operation, for the test to check the order.
    pub log: Vec<String>,
    /// Whether the last `set_text` was marked as excluded from history.
    pub excluded: bool,
}

/// `CF_UNICODETEXT`.
pub const CF_UNICODETEXT: u32 = 13;

impl FakeClipboard {
    pub fn with_text(text: &str) -> FakeClipboard {
        let c = FakeClipboard::default();
        if let Ok(mut s) = c.0.lock() {
            s.formats = vec![(CF_UNICODETEXT, ClipData::Bytes(text.as_bytes().to_vec()))];
            s.seq = 1;
        }
        c
    }

    pub fn state(&self) -> FakeState {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn with<R>(&self, f: impl FnOnce(&mut FakeState) -> R) -> R {
        let mut g = self.0.lock().expect("fake clipboard");
        f(&mut g)
    }
}

impl Clipboard for FakeClipboard {
    fn open(&mut self) -> bool {
        self.with(|s| {
            if s.busy > 0 {
                s.busy -= 1;
                s.log.push("open busy".into());
                return false;
            }
            assert!(!s.open, "opened twice");
            s.open = true;
            s.log.push("open".into());
            true
        })
    }
    fn close(&mut self) {
        self.with(|s| {
            s.open = false;
            s.log.push("close".into());
        });
    }
    fn snapshot(&mut self) -> Result<Snapshot, ClipError> {
        self.with(|s| {
            assert!(s.open);
            s.log.push("snapshot".into());
            if s.not_restorable {
                return Err(ClipError::NotRestorable);
            }
            Ok(Snapshot {
                formats: s.formats.clone(),
            })
        })
    }
    fn set_text(&mut self, text: &str) -> bool {
        self.with(|s| {
            assert!(s.open);
            s.formats = vec![(CF_UNICODETEXT, ClipData::Bytes(text.as_bytes().to_vec()))];
            s.seq += 1;
            s.excluded = true;
            s.log.push("set".into());
            true
        })
    }
    fn restore(&mut self, snap: &Snapshot) -> bool {
        self.with(|s| {
            assert!(s.open);
            s.formats = snap.formats.clone();
            s.seq += 1;
            s.log.push("restore".into());
            true
        })
    }
    fn sequence(&mut self) -> u32 {
        self.with(|s| s.seq)
    }
}

/// A probe with a fixed (test-controlled) answer.
#[derive(Clone, Debug, Default)]
pub struct FakeProbe(pub std::sync::Arc<std::sync::Mutex<Target>>);

impl FakeProbe {
    /// A normal, editable target.
    pub fn editable() -> FakeProbe {
        FakeProbe(std::sync::Arc::new(std::sync::Mutex::new(Target {
            focus: 1,
            editable: true,
            ..Target::default()
        })))
    }

    pub fn set(&self, f: impl FnOnce(&mut Target)) {
        if let Ok(mut t) = self.0.lock() {
            f(&mut t);
        }
    }
}

impl Probe for FakeProbe {
    fn sample(&mut self, ime_mode: bool) -> Target {
        let mut t = self.0.lock().map(|t| *t).unwrap_or_default();
        if !ime_mode {
            t.ime_native = false;
        }
        t
    }
}

/// What a run needs from the platform.
pub struct Env {
    pub probe: Box<dyn Probe>,
    pub clipboard: Box<dyn Clipboard>,
    /// The local time for `{date}` and `{time}`.
    pub clock: fn() -> LocalTime,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("run::Env")
    }
}

impl Env {
    /// Tests and the debug test host: a fixed editable target, no clipboard, the UTC clock.
    pub fn inert() -> Env {
        Env {
            probe: Box::new(FakeProbe::editable()),
            clipboard: Box::new(NoClipboard),
            clock: || LocalTime::utc(crate::time::now_secs()),
        }
    }
}

/// A compiled `type_text` step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextSpec {
    pub text: String,
    pub paste: bool,
    pub key_delay_ms: u32,
    pub variables: bool,
}

/// A compiled `send_keys` step.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KeysSpec {
    pub chords: Vec<Output>,
    pub step_delay_ms: u32,
}

/// What a hotstring adds to a run (from the core's signal).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HotstringFire {
    /// Backspaces that erase what the application shows of the abbreviation.
    pub erase: u8,
    pub case: CaseForm,
    /// The key that completed the abbreviation, exactly as the hook saw it.
    pub key: OutCode,
    /// The modifiers held with it (for typing the end character again).
    pub mods: u8,
    /// Type the end character again after the replacement.
    pub retype: bool,
    /// The key was consumed: give it back when nothing is typed.
    pub consumed: bool,
    /// The core's count of physical key presses when it fired.
    pub typed: u32,
}

/// One piece of a run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Piece {
    /// Release the modifiers the application sees held.
    Lift,
    /// One `SendInput` call whose downs and ups pair up.
    Keys(Vec<OutEvent>),
    /// Wait this many milliseconds.
    Wait(u32),
    /// Copy the clipboard and put this text on it.
    ClipSet(String),
    /// Put the copy back.
    ClipRestore,
    /// Press again the released Ctrl and Shift that are still held.
    Restore,
}

fn mods_events(bits: u8, up: bool, out: &mut Vec<OutEvent>) {
    let mut keys: Vec<ModKey> = ModKey::ALL
        .into_iter()
        .filter(|m| bits & m.bit() != 0)
        .collect();
    if up {
        keys.reverse();
    }
    for m in keys {
        out.push(OutEvent {
            code: OutCode::Vk(m.vk()),
            up,
        });
    }
}

/// The end character typed again: the same key with the same modifiers, in one call.
fn retype_events(h: &HotstringFire) -> Vec<OutEvent> {
    let mut ev = Vec::new();
    mods_events(h.mods, false, &mut ev);
    ev.push(OutEvent::down(h.key));
    ev.push(OutEvent::up(h.key));
    mods_events(h.mods, true, &mut ev);
    ev
}

fn erase(h: Option<&HotstringFire>, out: &mut Vec<Piece>) {
    if let Some(h) = h
        && h.erase > 0
    {
        out.push(Piece::Keys(text::taps(text::VK_BACK, usize::from(h.erase))));
    }
}

/// The plan of a `type_text` run (`now` fills in the variables).
pub fn plan_text(spec: &TextSpec, hs: Option<&HotstringFire>, now: &LocalTime) -> Vec<Piece> {
    let r = text::render(&spec.text, spec.variables, now).unwrap_or_else(|_| text::Rendered {
        text: spec.text.clone(),
        ..text::Rendered::default()
    });
    let typed = match hs {
        Some(h) => text::apply_case(&r.text, h.case),
        None => r.text.clone(),
    };
    let mut out = Vec::new();
    if spec.paste {
        out.push(Piece::ClipSet(typed.clone()));
    }
    out.push(Piece::Lift);
    erase(hs, &mut out);
    if spec.paste {
        out.push(Piece::Keys(text::paste_keys()));
        out.push(Piece::Wait(PASTE_SETTLE_MS));
        out.push(Piece::ClipRestore);
    } else {
        for (i, c) in text::chunks(&typed, spec.key_delay_ms)
            .into_iter()
            .enumerate()
        {
            if i > 0 && spec.key_delay_ms > 0 {
                out.push(Piece::Wait(spec.key_delay_ms));
            }
            out.push(Piece::Keys(c));
        }
    }
    let retype = hs.filter(|h| h.retype);
    if let Some(h) = retype {
        out.push(Piece::Keys(retype_events(h)));
    }
    let back = r.cursor_back + usize::from(r.cursor && retype.is_some());
    if back > 0 {
        out.push(Piece::Keys(text::taps(text::VK_LEFT, back)));
    }
    out.push(Piece::Restore);
    out
}

/// The events of one tapped output chord (modifiers around the key, or a bare modifier).
pub fn chord_events(o: &Output) -> Vec<OutEvent> {
    let bits = o.mod_keys().iter().fold(0u8, |a, m| a | m.bit());
    let mut ev = Vec::new();
    mods_events(bits, false, &mut ev);
    if let Some(k) = o.key {
        ev.push(OutEvent::down(k));
        ev.push(OutEvent::up(k));
    }
    mods_events(bits, true, &mut ev);
    ev
}

/// The plan of a `send_keys` run.
pub fn plan_keys(spec: &KeysSpec, hs: Option<&HotstringFire>) -> Vec<Piece> {
    let mut out = vec![Piece::Lift];
    erase(hs, &mut out);
    for (i, c) in spec.chords.iter().enumerate() {
        if i > 0 && spec.step_delay_ms > 0 {
            out.push(Piece::Wait(spec.step_delay_ms));
        }
        out.push(Piece::Keys(chord_events(c)));
    }
    if let Some(h) = hs.filter(|h| h.retype) {
        out.push(Piece::Keys(retype_events(h)));
    }
    out.push(Piece::Restore);
    out
}

/// What the worker does next with a run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next {
    /// Call [`Run::step`] again at this time.
    At(Instant),
    /// The run ended with this outcome.
    Done(&'static str),
}

/// One run in progress. See the module documentation.
#[derive(Debug)]
pub struct Run {
    pub rule: String,
    pub profile: String,
    pub mode: String,
    pieces: VecDeque<Piece>,
    hotstring: Option<HotstringFire>,
    focus: Option<u128>,
    started: Instant,
    lifted: u8,
    clip: Option<(Snapshot, u32)>,
    tries: u32,
    /// Something reached the target (the consumed key is then no longer given back).
    sent: bool,
    /// An outcome that replaces `ok` at the end (`clipboard_restore_failed`).
    late: Option<&'static str>,
}

fn guard(t: &Target, hotstring: bool) -> Option<&'static str> {
    if t.secure_desktop {
        Some("skipped_secure_desktop")
    } else if t.password_field {
        Some("skipped_password_field")
    } else if t.elevated {
        Some("skipped_elevated_target")
    } else if t.ime_composing || (hotstring && t.ime_native) {
        Some("skipped_ime_composition")
    } else if !t.editable {
        Some("skipped_disabled_field")
    } else {
        None
    }
}

const ALT: u8 = 0x04 | 0x08; // LAlt | RAlt (ModKey::bit order: LCtrl, RCtrl, LAlt, RAlt, …)
const WIN: u8 = 0x40 | 0x80;

type Inj<'a, B> = &'a mut Injector<B>;

impl Run {
    pub fn new(
        rule: String,
        profile: String,
        mode: String,
        pieces: Vec<Piece>,
        hotstring: Option<HotstringFire>,
        now: Instant,
    ) -> Run {
        Run {
            rule,
            profile,
            mode,
            pieces: pieces.into(),
            hotstring,
            focus: None,
            started: now,
            lifted: 0,
            clip: None,
            tries: 0,
            sent: false,
            late: None,
        }
    }

    /// The pieces still to do (tests).
    pub fn pieces(&self) -> &VecDeque<Piece> {
        &self.pieces
    }

    /// Carries out pieces until one sends keys, must wait, or the run ends.
    pub fn step<B: Backend>(
        &mut self,
        now: Instant,
        inj: Inj<'_, B>,
        env: &mut Env,
        shared: &RunShared,
    ) -> Next {
        if self.focus.is_none() {
            let t = env.probe.sample(self.hotstring.is_some());
            if let Some(code) = guard(&t, self.hotstring.is_some()) {
                return self.finish(code, true, inj, env, shared);
            }
            self.focus = Some(t.focus);
        }
        loop {
            if now.duration_since(self.started) > Duration::from_millis(MAX_RUN_MS) {
                return self.finish("timeout", false, inj, env, shared);
            }
            let Some(piece) = self.pieces.pop_front() else {
                let code = self.late.unwrap_or("ok");
                return self.finish(code, false, inj, env, shared);
            };
            match piece {
                Piece::Lift => self.lift(inj, shared),
                Piece::Restore => self.restore_mods(inj, shared),
                Piece::Wait(ms) => {
                    return Next::At(now + Duration::from_millis(u64::from(ms)));
                }
                Piece::Keys(ev) => {
                    let t = env.probe.sample(false);
                    if Some(t.focus) != self.focus {
                        return self.finish("focus_changed", false, inj, env, shared);
                    }
                    if let Some(code) = guard(&t, false) {
                        return self.finish(code, true, inj, env, shared);
                    }
                    if !self.sent
                        && let Some(h) = &self.hotstring
                        && shared.typed.load(Ordering::SeqCst) != h.typed
                    {
                        // The user typed on: erasing now would delete what they typed.
                        return self.finish("cancelled", false, inj, env, shared);
                    }
                    let ok = inj.send_balanced(&ev);
                    self.sent = true;
                    if !ok {
                        return self.finish("injection_failed", false, inj, env, shared);
                    }
                    // Yield after every call: the worker handles its other messages (remaps, a
                    // pause, the emergency chord) before the next chunk.
                    return Next::At(now);
                }
                Piece::ClipSet(t) => {
                    if !env.clipboard.open() {
                        self.tries += 1;
                        if self.tries >= CLIP_OPEN_TRIES {
                            return self.finish("clipboard_busy", true, inj, env, shared);
                        }
                        self.pieces.push_front(Piece::ClipSet(t));
                        return Next::At(now + Duration::from_millis(u64::from(CLIP_RETRY_MS)));
                    }
                    self.tries = 0;
                    let snap = match env.clipboard.snapshot() {
                        Ok(s) => s,
                        Err(ClipError::NotRestorable) => {
                            env.clipboard.close();
                            return self.finish("clipboard_not_restorable", true, inj, env, shared);
                        }
                    };
                    if !env.clipboard.set_text(&t) {
                        env.clipboard.restore(&snap);
                        env.clipboard.close();
                        return self.finish("clipboard_busy", true, inj, env, shared);
                    }
                    let seq = env.clipboard.sequence();
                    env.clipboard.close();
                    self.clip = Some((snap, seq));
                }
                Piece::ClipRestore => {
                    if self.clip.is_none() {
                        continue;
                    }
                    if !env.clipboard.open() {
                        self.tries += 1;
                        if self.tries >= CLIP_RESTORE_TRIES {
                            self.clip = None;
                            self.late = Some("clipboard_restore_failed");
                            continue;
                        }
                        self.pieces.push_front(Piece::ClipRestore);
                        return Next::At(now + Duration::from_millis(u64::from(CLIP_RETRY_MS)));
                    }
                    self.tries = 0;
                    if let Some(code) = self.put_back(env) {
                        self.late = Some(code);
                    }
                    env.clipboard.close();
                }
            }
        }
    }

    /// Ends the run from outside (pause, stop, emergency, reload): restores the clipboard and the
    /// modifiers, gives nothing back.
    pub fn cancel<B: Backend>(
        &mut self,
        inj: Inj<'_, B>,
        env: &mut Env,
        shared: &RunShared,
    ) -> &'static str {
        match self.finish("cancelled", false, inj, env, shared) {
            Next::Done(c) => c,
            Next::At(_) => "cancelled",
        }
    }

    /// Puts the clipboard copy back (the clipboard is open); `Some(code)` when that failed.
    fn put_back(&mut self, env: &mut Env) -> Option<&'static str> {
        let (snap, seq) = self.clip.take()?;
        if env.clipboard.sequence() != seq {
            return None; // another program wrote the clipboard meanwhile: its content wins
        }
        (!env.clipboard.restore(&snap)).then_some("clipboard_restore_failed")
    }

    fn lift<B: Backend>(&mut self, inj: Inj<'_, B>, shared: &RunShared) {
        let vis = shared.visible.load(Ordering::SeqCst) & !WIN;
        if vis == 0 {
            return;
        }
        // Marked before the releases are sent: a physical release that arrives meanwhile is
        // consumed by the core and replaced by ours.
        shared.lifted.fetch_or(vis, Ordering::SeqCst);
        self.lifted |= vis;
        let mut ev = Vec::new();
        if vis & ALT != 0 {
            ev.push(OutEvent::down(OutCode::Vk(MASK_VK)));
            ev.push(OutEvent::up(OutCode::Vk(MASK_VK)));
        }
        mods_events(vis, true, &mut ev);
        inj.adjust(&ev);
    }

    fn restore_mods<B: Backend>(&mut self, inj: Inj<'_, B>, shared: &RunShared) {
        if self.lifted == 0 {
            return;
        }
        let held = |m: ModKey, phys: u8, inj: &Injector<B>| {
            phys & m.bit() != 0 || inj.is_held(OutCode::Vk(m.vk()))
        };
        // Alt and Win stay released (a lone Alt would open the menu bar); the core consumes
        // their physical release.
        let want = self.lifted & !(ALT | WIN);
        let still = shared.lifted.load(Ordering::SeqCst) & want;
        let phys = shared.phys.load(Ordering::SeqCst);
        let restore: u8 = ModKey::ALL
            .into_iter()
            .filter(|m| still & m.bit() != 0 && held(*m, phys, inj))
            .fold(0, |a, m| a | m.bit());
        shared.lifted.fetch_and(!want, Ordering::SeqCst);
        self.lifted &= ALT | WIN;
        if restore == 0 {
            return;
        }
        let mut ev = Vec::new();
        mods_events(restore, false, &mut ev);
        inj.adjust(&ev);
        // The user may have let go between the check and the press: release again.
        let phys = shared.phys.load(Ordering::SeqCst);
        let gone: u8 = ModKey::ALL
            .into_iter()
            .filter(|m| restore & m.bit() != 0 && !held(*m, phys, inj))
            .fold(0, |a, m| a | m.bit());
        if gone != 0 {
            let mut ev = Vec::new();
            mods_events(gone, true, &mut ev);
            inj.adjust(&ev);
        }
    }

    fn finish<B: Backend>(
        &mut self,
        code: &'static str,
        give_back: bool,
        inj: Inj<'_, B>,
        env: &mut Env,
        shared: &RunShared,
    ) -> Next {
        self.pieces.clear();
        let mut code = code;
        if self.clip.is_some() {
            let mut restored = false;
            for i in 0..5 {
                if env.clipboard.open() {
                    let failed = self.put_back(env);
                    env.clipboard.close();
                    restored = failed.is_none();
                    break;
                }
                if i < 4 {
                    std::thread::sleep(Duration::from_millis(u64::from(CLIP_RETRY_MS)));
                }
            }
            self.clip = None;
            if !restored && code == "ok" {
                code = "clipboard_restore_failed";
            }
        }
        self.restore_mods(inj, shared);
        if give_back
            && !self.sent
            && let Some(h) = self.hotstring.filter(|h| h.consumed)
        {
            inj.send_balanced(&[OutEvent::down(h.key), OutEvent::up(h.key)]);
        }
        Next::Done(code)
    }
}

/// Gives back a hotstring's consumed key when no run starts (busy, a reload in between).
pub fn give_back<B: Backend>(inj: &mut Injector<B>, h: &HotstringFire) {
    if h.consumed {
        inj.send_balanced(&[OutEvent::down(h.key), OutEvent::up(h.key)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::Recorder;

    fn now() -> LocalTime {
        LocalTime {
            year: 2026,
            month: 11,
            day: 4,
            hour: 9,
            minute: 5,
            second: 0,
        }
    }

    struct H {
        inj: Injector<Recorder>,
        env: Env,
        probe: FakeProbe,
        clip: FakeClipboard,
        shared: RunShared,
        t: Instant,
    }

    impl H {
        fn new() -> H {
            let probe = FakeProbe::editable();
            let clip = FakeClipboard::with_text("previous");
            H {
                inj: Injector::new(Recorder::default()),
                env: Env {
                    probe: Box::new(probe.clone()),
                    clipboard: Box::new(clip.clone()),
                    clock: now,
                },
                probe,
                clip,
                shared: RunShared::default(),
                t: Instant::now(),
            }
        }

        /// Runs to the end, jumping over waits; calls `between` before each step.
        fn run(&mut self, run: &mut Run, mut between: impl FnMut(&mut H, usize)) -> &'static str {
            for i in 0..10_000 {
                between(self, i);
                match run.step(self.t, &mut self.inj, &mut self.env, &self.shared) {
                    Next::Done(c) => return c,
                    Next::At(t) => self.t = t.max(self.t),
                }
            }
            panic!("the run never ended");
        }

        fn sent(&self) -> &[OutEvent] {
            &self.inj.backend().sent
        }
    }

    fn text(t: &str) -> TextSpec {
        TextSpec {
            text: t.into(),
            paste: false,
            key_delay_ms: 0,
            variables: false,
        }
    }

    fn run_of(pieces: Vec<Piece>, hs: Option<HotstringFire>) -> Run {
        Run::new(
            "r".into(),
            "p".into(),
            "m".into(),
            pieces,
            hs,
            Instant::now(),
        )
    }

    const LCTRL: u8 = 0x01;
    const LSHIFT: u8 = 0x10;

    #[test]
    fn text_is_typed_exactly_and_trigger_modifiers_are_released_and_restored() {
        let mut h = H::new();
        h.shared.visible.store(LCTRL | LSHIFT, Ordering::SeqCst);
        h.shared.phys.store(LCTRL | LSHIFT, Ordering::SeqCst);
        let mut r = run_of(plan_text(&text("감사👍"), None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        let sent = h.sent();
        let up = |c| OutEvent::up(OutCode::Vk(c));
        let down = |c| OutEvent::down(OutCode::Vk(c));
        assert_eq!(
            sent[..2],
            [up(0xA0), up(0xA2)],
            "Shift and Ctrl released first"
        );
        assert_eq!(sent[2..10], text::events("감사👍")[..]);
        assert_eq!(sent[10..], [down(0xA2), down(0xA0)], "and pressed again");
        assert!(h.inj.held().is_empty());
        assert_eq!(h.shared.lifted.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn released_alt_is_masked_and_never_pressed_again() {
        let mut h = H::new();
        let lalt = 0x04;
        h.shared.visible.store(lalt, Ordering::SeqCst);
        h.shared.phys.store(lalt, Ordering::SeqCst);
        let mut r = run_of(plan_text(&text("a"), None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        let sent = h.sent();
        assert_eq!(sent[0], OutEvent::down(OutCode::Vk(MASK_VK)));
        assert_eq!(sent[2], OutEvent::up(OutCode::Vk(0xA4)));
        assert_eq!(sent.len(), 5, "no Alt down at the end: {sent:?}");
        assert_eq!(
            h.shared.lifted.load(Ordering::SeqCst),
            lalt,
            "the core consumes Alt's physical release"
        );
    }

    #[test]
    fn a_released_modifier_the_user_let_go_is_not_pressed_again() {
        let mut h = H::new();
        h.shared.visible.store(LSHIFT, Ordering::SeqCst);
        h.shared.phys.store(LSHIFT, Ordering::SeqCst);
        let mut r = run_of(plan_text(&text("abc"), None, &now()), None);
        let code = h.run(&mut r, |h, i| {
            if i == 1 {
                // The core consumed the physical release and cleared the bit.
                h.shared.lifted.fetch_and(!LSHIFT, Ordering::SeqCst);
                h.shared.phys.store(0, Ordering::SeqCst);
            }
        });
        assert_eq!(code, "ok");
        assert!(
            !h.sent().contains(&OutEvent::down(OutCode::Vk(0xA0))),
            "{:?}",
            h.sent()
        );
    }

    #[test]
    fn chunks_delays_and_the_insertion_point() {
        let mut h = H::new();
        let spec = TextSpec {
            text: "ab{cursor}cd".into(),
            variables: true,
            key_delay_ms: 15,
            paste: false,
        };
        let plan = plan_text(&spec, None, &now());
        let waits = plan.iter().filter(|p| matches!(p, Piece::Wait(15))).count();
        assert_eq!(waits, 3, "between four characters");
        let start = h.t;
        let mut r = run_of(plan, None);
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        assert!(h.t >= start + Duration::from_millis(45));
        let lefts = h
            .sent()
            .iter()
            .filter(|e| e.code == OutCode::Vk(text::VK_LEFT) && !e.up)
            .count();
        assert_eq!(lefts, 2, "back over `cd`");
        // Variables are filled in at run time.
        let p = plan_text(
            &TextSpec {
                text: "{date}".into(),
                variables: true,
                ..text("")
            },
            None,
            &now(),
        );
        assert_eq!(p[1], Piece::Keys(text::events("2026-11-04")));
    }

    #[test]
    fn a_focus_change_stops_the_run_and_never_types_into_the_new_window() {
        let mut h = H::new();
        let long: String = "가".repeat(100);
        let mut r = run_of(plan_text(&text(&long), None, &now()), None);
        let code = h.run(&mut r, |h, i| {
            if i == 1 {
                h.probe.set(|t| t.focus = 2);
            }
        });
        assert_eq!(code, "focus_changed");
        let typed = h.sent().iter().filter(|e| !e.up).count();
        assert_eq!(typed, text::CHUNK, "exactly the first chunk, then nothing");
        assert!(h.inj.held().is_empty());
    }

    #[test]
    fn the_target_is_checked_before_every_call() {
        for (set, code) in [
            (
                (|t: &mut Target| t.password_field = true) as fn(&mut Target),
                "skipped_password_field",
            ),
            (|t| t.elevated = true, "skipped_elevated_target"),
            (|t| t.secure_desktop = true, "skipped_secure_desktop"),
            (|t| t.ime_composing = true, "skipped_ime_composition"),
            (|t| t.editable = false, "skipped_disabled_field"),
        ] {
            // Before the run: nothing at all is sent.
            let mut h = H::new();
            h.probe.set(set);
            let mut r = run_of(plan_text(&text("hello"), None, &now()), None);
            assert_eq!(h.run(&mut r, |_, _| {}), code);
            assert!(h.sent().is_empty(), "{code}: {:?}", h.sent());
            // During the run: the rest is not typed.
            let mut h = H::new();
            let mut r = run_of(plan_text(&text(&"x".repeat(70)), None, &now()), None);
            let probe = h.probe.clone();
            assert_eq!(
                h.run(&mut r, |_, i| {
                    if i == 1 {
                        probe.set(set);
                    }
                }),
                code
            );
            assert_eq!(h.sent().len(), 2 * text::CHUNK, "{code}");
        }
        // The IME's native mode stops hotstrings only.
        let mut h = H::new();
        h.probe.set(|t| t.ime_native = true);
        let mut r = run_of(plan_text(&text("x"), None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
    }

    #[test]
    fn a_refused_injection_is_reported_and_leaves_nothing_pressed() {
        let mut h = H::new();
        h.inj = Injector::new(Recorder {
            limit: Some(3),
            ..Recorder::default()
        });
        let mut r = run_of(plan_text(&text("ab"), None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "injection_failed");
        assert!(h.inj.held().is_empty());
    }

    #[test]
    fn cancellation_restores_the_clipboard_and_the_modifiers() {
        let mut h = H::new();
        h.shared.visible.store(LCTRL, Ordering::SeqCst);
        h.shared.phys.store(LCTRL, Ordering::SeqCst);
        let spec = TextSpec {
            paste: true,
            ..text("snippet")
        };
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        // Up to the wait after Ctrl+V.
        let next = r.step(h.t, &mut h.inj, &mut h.env, &h.shared);
        assert!(
            matches!(next, Next::At(_)),
            "waiting for the paste to settle"
        );
        assert_eq!(
            h.clip.state().formats[0].1,
            ClipData::Bytes(b"snippet".to_vec())
        );
        // The emergency chord, a pause or a reload arrives now.
        assert_eq!(r.cancel(&mut h.inj, &mut h.env, &h.shared), "cancelled");
        let st = h.clip.state();
        assert_eq!(st.formats[0].1, ClipData::Bytes(b"previous".to_vec()));
        assert!(!st.open);
        assert_eq!(
            h.sent().last(),
            Some(&OutEvent::down(OutCode::Vk(0xA2))),
            "Ctrl pressed again"
        );
    }

    #[test]
    fn paste_saves_sets_pastes_waits_and_restores() {
        let mut h = H::new();
        let spec = TextSpec {
            paste: true,
            ..text("주소\n서울")
        };
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        let start = h.t;
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        assert!(h.t >= start + Duration::from_millis(u64::from(PASTE_SETTLE_MS)));
        let st = h.clip.state();
        assert_eq!(
            st.log,
            [
                "open", "snapshot", "set", "close", "open", "restore", "close"
            ]
        );
        assert!(st.excluded, "kept out of the clipboard history");
        assert_eq!(st.formats[0].1, ClipData::Bytes(b"previous".to_vec()));
        assert_eq!(h.sent(), text::paste_keys());
    }

    #[test]
    fn paste_retries_a_busy_clipboard_then_gives_up_without_typing() {
        let mut h = H::new();
        h.clip.0.lock().unwrap().busy = 3;
        let spec = TextSpec {
            paste: true,
            ..text("x")
        };
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        assert_eq!(
            h.run(&mut r, |_, _| {}),
            "ok",
            "three busy opens, then it works"
        );
        let mut h = H::new();
        h.clip.0.lock().unwrap().busy = 100;
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "clipboard_busy");
        assert!(h.sent().is_empty());
        assert_eq!(
            h.clip.state().formats[0].1,
            ClipData::Bytes(b"previous".to_vec())
        );
    }

    #[test]
    fn content_that_cannot_be_copied_is_never_replaced() {
        let mut h = H::new();
        h.clip.0.lock().unwrap().not_restorable = true;
        let spec = TextSpec {
            paste: true,
            ..text("x")
        };
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        assert_eq!(h.run(&mut r, |_, _| {}), "clipboard_not_restorable");
        let st = h.clip.state();
        assert_eq!(st.log, ["open", "snapshot", "close"]);
        assert_eq!(st.formats[0].1, ClipData::Bytes(b"previous".to_vec()));
        assert!(h.sent().is_empty());
    }

    #[test]
    fn a_newer_clipboard_wins_and_a_stuck_restore_is_reported() {
        let mut h = H::new();
        let spec = TextSpec {
            paste: true,
            ..text("x")
        };
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        let clip = h.clip.clone();
        let code = h.run(&mut r, |_, i| {
            if i == 1 {
                // The user copies something else while Ddalgak waits.
                let mut s = clip.0.lock().unwrap();
                s.formats = vec![(CF_UNICODETEXT, ClipData::Bytes(b"newer".to_vec()))];
                s.seq += 1;
            }
        });
        assert_eq!(code, "ok");
        assert_eq!(
            h.clip.state().formats[0].1,
            ClipData::Bytes(b"newer".to_vec())
        );
        let mut h = H::new();
        let mut r = run_of(plan_text(&spec, None, &now()), None);
        let clip = h.clip.clone();
        let code = h.run(&mut r, |_, i| {
            if i == 1 {
                clip.0.lock().unwrap().busy = 1000;
            }
        });
        assert_eq!(code, "clipboard_restore_failed");
    }

    fn hs(erase: u8, retype: bool, typed: u32) -> HotstringFire {
        HotstringFire {
            erase,
            case: CaseForm::Title,
            key: OutCode::Exact {
                vk: 0x31,
                scan: 0x02,
                ext: false,
            },
            mods: LSHIFT,
            retype,
            consumed: true,
            typed,
        }
    }

    #[test]
    fn a_hotstring_erases_types_in_the_typed_case_and_types_the_end_key_again() {
        let mut h = H::new();
        h.shared.visible.store(LSHIFT, Ordering::SeqCst);
        h.shared.phys.store(LSHIFT, Ordering::SeqCst);
        h.shared.typed.store(4, Ordering::SeqCst);
        let fire = hs(3, true, 4);
        let mut r = run_of(
            plan_text(&text("by the way"), Some(&fire), &now()),
            Some(fire),
        );
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        let sent = h.sent();
        let bs = OutEvent::down(OutCode::Vk(text::VK_BACK));
        assert_eq!(sent.iter().filter(|e| **e == bs).count(), 3);
        let typed: Vec<OutEvent> = text::events("By the way");
        let pos = sent.windows(typed.len()).position(|w| w == typed).unwrap();
        assert!(pos > 0);
        // Shift+1 again after the replacement, then Shift restored (still held).
        let tail = &sent[pos + typed.len()..];
        assert_eq!(tail[0], OutEvent::down(OutCode::Vk(0xA0)));
        assert_eq!(tail[1], OutEvent::down(fire.key));
        assert_eq!(tail.last(), Some(&OutEvent::down(OutCode::Vk(0xA0))));
    }

    #[test]
    fn a_hotstring_gives_its_key_back_when_skipped_and_never_erases_after_typing_on() {
        let mut h = H::new();
        h.probe.set(|t| t.ime_native = true);
        let fire = hs(3, true, 0);
        let mut r = run_of(plan_text(&text("x"), Some(&fire), &now()), Some(fire));
        assert_eq!(h.run(&mut r, |_, _| {}), "skipped_ime_composition");
        assert_eq!(
            h.sent(),
            [OutEvent::down(fire.key), OutEvent::up(fire.key)],
            "the end key, given back unchanged"
        );
        // The user typed another key before the first Backspace.
        let mut h = H::new();
        h.shared.typed.store(5, Ordering::SeqCst);
        let fire = hs(3, true, 4);
        let mut r = run_of(plan_text(&text("x"), Some(&fire), &now()), Some(fire));
        assert_eq!(h.run(&mut r, |_, _| {}), "cancelled");
        assert!(h.sent().is_empty(), "nothing erased, nothing given back");
        // Without retyping (omit_end) the end key is not typed again.
        let mut h = H::new();
        let fire = hs(3, false, 0);
        let mut r = run_of(plan_text(&text("x"), Some(&fire), &now()), Some(fire));
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        assert!(!h.sent().contains(&OutEvent::down(fire.key)));
    }

    #[test]
    fn send_keys_taps_each_chord_with_its_modifiers() {
        let spec = KeysSpec {
            chords: vec![
                Output::parse("Ctrl+K").unwrap(),
                Output::parse("Ctrl+V").unwrap(),
                Output::parse("LWin").unwrap(),
            ],
            step_delay_ms: 50,
        };
        let plan = plan_keys(&spec, None);
        assert_eq!(
            plan.iter().filter(|p| matches!(p, Piece::Wait(50))).count(),
            2
        );
        let mut h = H::new();
        let mut r = run_of(plan, None);
        assert_eq!(h.run(&mut r, |_, _| {}), "ok");
        let d = |c| OutEvent::down(OutCode::Vk(c));
        let u = |c| OutEvent::up(OutCode::Vk(c));
        assert_eq!(
            h.sent(),
            [
                d(0xA2),
                d(0x4B),
                u(0x4B),
                u(0xA2),
                d(0xA2),
                d(0x56),
                u(0x56),
                u(0xA2),
                d(0x5B),
                u(0x5B)
            ]
        );
        assert!(h.inj.held().is_empty());
    }

    #[test]
    fn a_run_longer_than_a_minute_times_out() {
        let mut h = H::new();
        let spec = TextSpec {
            key_delay_ms: 200,
            ..text(&"x".repeat(400))
        };
        let mut r = Run::new(
            "r".into(),
            "p".into(),
            "m".into(),
            plan_text(&spec, None, &now()),
            None,
            h.t,
        );
        assert_eq!(h.run(&mut r, |_, _| {}), "timeout");
        let typed = h.sent().iter().filter(|e| !e.up).count();
        assert!((290..=310).contains(&typed), "{typed}");
    }
}
