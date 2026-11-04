//! The instance core (ADR 0023 §1): one state machine that owns the capture sessions, the pins
//! and the shortcuts, and answers every request on the instance's UI thread.
//!
//! Requests arrive from four separate sources, and none of them can reach the others' state
//! except through [`App`]: IPC from the CLI ([`App::request`]), global hotkeys and tray clicks
//! ([`App::action`], [`App::tray_command`]) and the pin windows' own events
//! ([`App::pin_event`]). Everything the core needs from the OS goes through [`Shell`] (pin
//! windows, the clipboard, the pointer, notices) and [`CaptureBackend`], so the whole lifecycle
//! runs headless in tests with fakes; `win::runtime` supplies the Win32 implementations.
//!
//! The core never captures on its own (NOEUL-SEC-001), reads the clipboard only for
//! `paste.float` (NOEUL-SEC-002), writes no picture to disk (NOEUL-SEC-005) and reports only IDs,
//! geometry and counts (NOEUL-SEC-008).
//!
//! **The capture overlay** (Step 50, ADR 0026): `capture.region` (`F1`, the tray's left click,
//! `noeul capture --interactive`) freezes every monitor into one frame, opens a capture session
//! with it and shows an [`OverlayWindow`] over the virtual desktop. The window reports
//! [`OverlayEvent`]s; the core feeds them to the [`Overlay`] model, redraws only what changed,
//! and on `capture.pin` hands the session's [`crate::selection::CaptureSelection`] to a new pin.
//! While an overlay is open, further capture requests do nothing (NOEUL-FR-013).
//!
//! **Outputs of the overlay** (Step 51, UX spec §4.2): the overlay model reports
//! `Output(Pin | Copy | Save | QuickSave, rect, via)` for a double-click or middle-click inside
//! the selection, a key, the toolbar or the selection's context menu. A pin goes through
//! [`App::pin_selection`] (no clipboard, no file; a pointer gesture guards the new pin for one
//! double-click time); a copy encodes PNG and `CF_DIBV5` in memory and hands them to
//! [`Shell::clipboard_write`]; a save asks the runtime for the save dialog ([`SaveRequest`],
//! shown with the core released) and writes only the file the user chose; a quick save writes a
//! new file in the quick-save folder. Every failure keeps the overlay and its selection with a
//! banner (NOEUL-FR-025); a clipboard that is busy or disabled never affects Pin.
//!
//! **The pin engine** (Step 53, ADR 0029): every pin, whatever made it, goes through
//! [`crate::pins::NewPin`]: the overlay's selection through [`App::pin_selection`] (the entry
//! point of Step 51's double-click, with no clipboard in between), a CLI region through
//! `capture`/`pin.create`, the clipboard through `paste.float`/`pin.create`. Pins are moved,
//! zoomed, hidden, shown, closed and destroyed independently from their keys, the mouse, their
//! context menu ([`PinMenu`], shown by the runtime with the core released), the tray and IPC
//! (`pin.list`, `pin.hide`, `pin.show`, `pin.close`, `pin.reopen`). A display change
//! ([`App::display_changed`]) keeps pins where they are and offers to bring the lost ones back.
//!
//! **Smart selection** (Step 52, ADR 0035): when a capture starts, right after the frozen picture
//! and before the overlay is shown, the core asks [`Shell::window_snapshot`] for the windows on
//! the desktop (rectangles and flags only, NOEUL-SEC-007) and hands them to the model with the
//! `[capture] detection` mode; with detection off nothing is listed until `E` turns it on. Element
//! lookups go to [`Shell::request_element`], which answers off the UI thread as an
//! [`OverlayEvent::Element`]; [`Shell::end_detection`] stops them when the overlay closes. Pointer
//! keys move the real cursor through [`OverlayWindow::warp_pointer`]. The rectangles of
//! successful captures stay in memory for `history.*` (never on disk, NOEUL-SEC-005).
//!
//! **Annotation** (Step 56, ADR 0038): the overlay model keeps the annotations as vector objects;
//! after every input the core re-renders the changed pixels of the annotated selection (a layer
//! over the frozen picture, [`crate::annotation::draw`]) and lets the window's typesetter lay out
//! edited labels. Copy, save and quick save output the selection flattened with its annotations
//! ([`crate::annotation::flatten`]); Pin hands the unchanged pixels and the objects to the new pin,
//! which keeps them editable. `pin.annotate` (`Space`, the pin menu) opens the same overlay model
//! over a pin's picture ([`overlay::Mode::PinEdit`]) in a layered window; Done or `Enter` commits the
//! annotations (and a crop) to the pin, Cancel or `Escape` leaves the pin as it was. One overlay
//! exists at a time: a capture or a pin edit.

use crate::annotation::{self, Ime, Object, TextKey, Typesetter};
use crate::capture::{CaptureBackend, DesktopState, check_region};
use crate::detect::{DetectMode, ElementAnswer, ElementRequest, Snapshot};
use crate::error::{Code, NoeulError, Result};
use crate::frame::{Frame, Ledger, Rect};
use crate::hotkeys::Manager;
use crate::ipc;
use crate::output;
use crate::overlay::{
    self, Action as OverlayAction, AnnotateAction, Cursor, GestureAction, Gestures,
    Input as OverlayInput, Measure, Nav, Outcome as OverlayOutcome, Output, Overlay, Scene,
    SystemValues, Via,
};
use crate::paths::Paths;
use crate::pins::{NewPin, PinError, PinId, PinWindows, Pins, Placement, Source};
use crate::selection::{
    CaptureSelection, Captures, Origin, Outcome as SessionOutcome, SessionError,
};
use crate::settings::{self, KeyboardHelp, UiState};
use crate::transform::{self, Edge, Orientation};
use crate::tray::{Click, MenuItem, menu};
use crate::util::rfc3339;
use crate::zoom;
use kim_hotkeys::keys::Chord;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// What the core needs from the desktop besides capturing.
/// What one clipboard read found: the image to pin and, when the clipboard also offered an
/// animated GIF, its frame count (the image is its first frame; NOEUL-FR-068).
#[derive(Debug)]
pub struct Pasted {
    pub frame: Frame,
    /// 1 for a still picture.
    pub frames: u32,
}

pub trait Shell {
    fn pin_windows(&mut self) -> &mut dyn PinWindows;
    /// Reads the clipboard once and returns its image (`nothing_to_pin`,
    /// `clipboard_unavailable` or `image_invalid` otherwise), and how many frames the GIF next
    /// to it has when there is one. Called only for `paste.float` and `pin.create`.
    fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Pasted>;
    /// Puts a picture on the clipboard (Copy, NOEUL-FR-030): `png` as the registered `PNG`
    /// format and `dib` as `CF_DIBV5`, the clipboard opened for the write only and retried
    /// briefly when another program holds it (`clipboard_unavailable`). Never called for a pin.
    fn clipboard_write(&mut self, png: &[u8], dib: &[u8]) -> Result<()>;
    /// The user's Pictures folder (quick save defaults to its `Noeul` subfolder); `None` under
    /// the debug-build test hook, so a test never writes there.
    fn pictures_dir(&self) -> Option<PathBuf>;
    /// The local time `[year, month, day, hour, minute, second]` (saved files' names).
    fn local_time(&self) -> [u16; 6];
    /// The pointer position in device pixels.
    fn pointer(&self) -> (i32, i32);
    /// A short non-modal notice (a tray balloon when the icon is shown; nothing otherwise).
    fn notice(&mut self, text: &str);
    /// Whether the notification-area icon is shown.
    fn tray_shown(&self) -> bool;
    /// Asks once before quitting with open pins (GUI only; the CLI uses `--yes`).
    fn confirm_quit(&mut self, pins: usize) -> bool;
    /// Creates the capture overlay's window over `picture` (the frozen picture's rectangle in
    /// virtual-desktop device pixels), not shown yet. `layered`: the pin editor's window, whose
    /// transparent pixels show what is behind (Step 56).
    fn open_overlay(
        &mut self,
        picture: Rect,
        layered: bool,
    ) -> std::result::Result<Box<dyn OverlayWindow>, String>;
    /// The user's pointer settings, read now (NOEUL-NFR-012).
    fn system_values(&self) -> SystemValues;
    /// Whether Noeul's windows may take keyboard focus and stay on top. `false` only in the
    /// debug-build test hook, whose windows are off-screen and must never move the user's focus.
    fn interactive(&self) -> bool;
    /// The top-level windows of the desktop now, topmost first, over `picture`: rectangles and
    /// flags only, never a title or other text (NOEUL-FR-040, NOEUL-SEC-007). Called when a
    /// capture starts (before its overlay is shown) or when detection is turned on during it;
    /// the overlay window itself is never listed.
    fn window_snapshot(&mut self, picture: Rect) -> Snapshot;
    /// Asks UI Automation for the element under `req.at` inside `req.token`'s window, off the UI
    /// thread; the answer arrives as [`OverlayEvent::Element`]. Never blocks.
    fn request_element(&mut self, req: ElementRequest);
    /// The overlay closed: no more element lookups (a lookup still running is abandoned).
    fn end_detection(&mut self);
}

/// The capture overlay's window (`win::overlay`; a fake in tests). It owns a back buffer of
/// the picture's size, measures the labels' text and typesets annotation labels.
pub trait OverlayWindow: Measure + Typesetter {
    /// Composes `dirty` of `scene` over `frozen` (and the annotated selection `layer`) into the
    /// back buffer and repaints those parts.
    fn present(&mut self, frozen: &Frame, layer: Option<&Frame>, scene: &Scene, dirty: &[Rect]);
    /// Switches the input method on for the window, with its candidate window at `caret`, while
    /// an annotation label is edited, and off again with `None` (NOEUL-NFR-014).
    fn set_ime(&mut self, caret: Option<Rect>);
    /// Shows the window, on top and taking keyboard focus when `activate`, and paints it
    /// completely before returning; `false` when it could not be shown.
    fn show(&mut self, activate: bool) -> bool;
    fn set_cursor(&mut self, cursor: Cursor);
    /// Bytes of its back buffer (reported by `noeul status`).
    fn buffer_bytes(&self) -> u64;
    /// Moves the real pointer to `at` after a pointer key (`SetCursorPos`), only while the
    /// overlay is the foreground window and never under the test placement (NOEUL-SEC-012).
    fn warp_pointer(&mut self, at: (i32, i32));
    /// Destroys the window and frees its back buffer now.
    fn destroy(&mut self);
}

/// Something the overlay window reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEvent {
    /// A pointer or focus input, in virtual-desktop device pixels.
    Input(OverlayInput),
    /// A key press in the overlay (matched against the `capture-overlay` context): its logical
    /// chord, its physical chord (`code:…`, from the scan code) when it has one, and `true` when
    /// it is an auto-repeat.
    Key(Chord, Option<Chord>, bool),
    /// UI Automation's answer to an element request (Step 52).
    Element(ElementRequest, ElementAnswer),
    /// A typed character (`WM_CHAR`), `true` when auto-repeated: text for a label being edited,
    /// else matched against `char:` bindings (Step 56).
    Char(char, bool),
    /// An input-method event of the window (`WM_IME_*`, Step 56).
    Ime(Ime),
    /// The mouse wheel turned over the overlay (Step 56).
    Wheel(i32),
}

/// One first-frame measurement (NOEUL-NFR-003): from the request (the hotkey press reaching
/// the UI thread, a tray click, an IPC request) to the overlay painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencySample {
    pub total_us: u64,
    /// Until the frozen picture was taken.
    pub capture_us: u64,
    /// From there until the first frame was painted (window, back buffer, composition).
    pub render_us: u64,
    pub width: u32,
    pub height: u32,
}

/// The last first-frame measurements of this instance (a local metric, ADR 0012).
#[derive(Debug, Default)]
pub struct Latency {
    totals: VecDeque<u64>,
    last: Option<LatencySample>,
    count: u64,
}

impl Latency {
    /// How many samples the percentiles cover.
    pub const KEEP: usize = 100;

    pub fn push(&mut self, s: LatencySample) {
        if self.totals.len() == Latency::KEEP {
            self.totals.pop_front();
        }
        self.totals.push_back(s.total_us);
        self.last = Some(s);
        self.count += 1;
    }

    /// The nearest-rank percentile `p` (1–100) of the kept totals.
    pub fn percentile(&self, p: u32) -> Option<u64> {
        if self.totals.is_empty() {
            return None;
        }
        let mut v: Vec<u64> = self.totals.iter().copied().collect();
        v.sort_unstable();
        let rank = (u64::from(p.clamp(1, 100)) * v.len() as u64).div_ceil(100);
        v.get(usize::try_from(rank).unwrap_or(1).saturating_sub(1))
            .copied()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "count": self.count,
            "last": self.last.map(|s| json!({
                "total_us": s.total_us, "capture_us": s.capture_us, "render_us": s.render_us,
                "width": s.width, "height": s.height,
            })),
            "p50_us": self.percentile(50),
            "p95_us": self.percentile(95),
        })
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// The open overlay: its capture session, model, window and the scene on screen.
/// What an open overlay is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Capture,
    /// Editing this pin's annotations (Step 56).
    PinEdit(PinId),
}

struct OverlaySession {
    /// The capture session (0 for a pin edit).
    id: u64,
    kind: SessionKind,
    model: Overlay,
    window: Box<dyn OverlayWindow>,
    scene: Scene,
    /// The selection's pixels with the annotations drawn on them, while there are annotations
    /// (what the overlay shows inside the selection; Step 56).
    layer: Option<Frame>,
    /// A pin edit's picture: the pin's pixels on a transparent canvas.
    base: Option<Frame>,
    /// Where the input method's candidate window was put last (`None`: it is off).
    ime: Option<Rect>,
}

/// The annotation part of the overlay's status: the tool, counts and IDs, never text.
fn annotation_json(m: &Overlay) -> Value {
    let e = m.editor();
    let mut kinds = serde_json::Map::new();
    for t in annotation::Tool::ALL {
        let n = e.objects().iter().filter(|o| o.shape.kind() == t).count();
        kinds.insert(t.as_str().into(), json!(n));
    }
    json!({
        "tool": e.tool().map(annotation::Tool::as_str),
        "objects": e.objects().len(),
        "kinds": kinds,
        "selected": e.selected(),
        "undo": e.doc().undo_steps(),
        "redo": e.doc().redo_steps(),
        "editing_text": e.editing_text(),
        "composing": e.composing(),
        "color": e.color().0,
        "width": annotation::WIDTHS[usize::from(e.level()).min(annotation::WIDTHS.len() - 1)],
    })
}

impl OverlaySession {
    fn to_json(&self) -> Value {
        json!({
            "session": self.id,
            "mode": match self.kind {
                SessionKind::Capture => "capture",
                SessionKind::PinEdit(_) => "pin_edit",
            },
            "pin": match self.kind {
                SessionKind::PinEdit(id) => Value::String(id.to_string()),
                SessionKind::Capture => Value::Null,
            },
            "annotation": annotation_json(&self.model),
            "phase": self.model.phase().as_str(),
            "selection": self.model.selection().map(|r| r.to_json()),
            "picture": self.model.picture().to_json(),
            "monitors": self.model.monitors().len(),
            "buffer_bytes": self.window.buffer_bytes(),
            "help": self.model.help_shown(),
            "focus": self.model.toolbar_focus().map_or("canvas", overlay::ToolbarItem::as_str),
            "pointer": {"x": self.model.pointer().0, "y": self.model.pointer().1},
            "detection": {
                "mode": self.model.detect_mode().as_str(),
                "windows": self.model.detect_windows(),
                "target": self.model.detected().filter(|d| !d.protected).map(|d| json!({
                    "kind": d.kind.as_str(), "rect": d.rect.to_json(),
                })),
                "protected": self.model.detected().is_some_and(|d| d.protected),
                "fallback": self.model.detect_fallback().map(crate::detect::Fallback::as_str),
                "suspended": self.model.detect_suspended(),
            },
            "magnifier": {
                "enabled": self.model.magnifier_enabled(),
                "shown": self.scene.magnifier.is_some(),
            },
            "history": {
                "count": self.model.history().0,
                "position": self.model.history().1,
            },
        })
    }

    fn is_capture(&self) -> bool {
        self.kind == SessionKind::Capture
    }
}

/// The overlay's keyboard help, from the effective bindings and gestures (so a changed key or
/// gesture shows as such).
pub fn overlay_help(m: &Manager, gestures: Gestures) -> Vec<String> {
    let k = |a: &str| m.keys_for(a).map(crate::hotkeys::key_label);
    let mut lines = vec![
        "Drag to select a region, or click a highlighted window. Esc or right-click cancels."
            .to_string(),
    ];
    let mut outputs = Vec::new();
    match gestures.double_click {
        GestureAction::Pin => outputs.push("Double-click inside pins it".to_string()),
        GestureAction::Copy => outputs.push("Double-click inside copies it".to_string()),
        GestureAction::Save => outputs.push("Double-click inside saves it".to_string()),
        GestureAction::None => {}
    }
    for (action, verb) in [
        ("capture.copy", "copies"),
        ("capture.save", "saves"),
        ("capture.quick-save", "quick-saves"),
    ] {
        if let Some(x) = k(action) {
            outputs.push(format!("{x} {verb}"));
        }
    }
    if !outputs.is_empty() {
        lines.push(outputs.join(" \u{b7} "));
    }
    let group = |kind: &str| -> Option<String> {
        let mut keys = Vec::new();
        for d in ["up", "down", "left", "right"] {
            keys.push(k(&format!("selection.{kind}-{d}"))?);
        }
        let arrows = ["Up", "Down", "Left", "Right"];
        let prefix = keys[0].strip_suffix("Up").map(str::to_string);
        Some(match prefix {
            Some(p)
                if keys
                    .iter()
                    .zip(arrows)
                    .all(|(k, a)| k.strip_suffix(a) == Some(p.as_str())) =>
            {
                if p.is_empty() {
                    "Arrows".to_string()
                } else {
                    format!("{p}arrows")
                }
            }
            _ => keys.join("/"),
        })
    };
    let mut second = Vec::new();
    for (kind, verb) in [("move", "move"), ("grow", "grow"), ("shrink", "shrink")] {
        if let Some(g) = group(kind) {
            second.push(format!("{g} {verb}"));
        }
    }
    if !second.is_empty() {
        lines.push(second.join(" \u{b7} "));
    }
    let mut third = Vec::new();
    if let Some(x) = k("selection.full") {
        third.push(format!("{x} whole monitor"));
    }
    if let Some(x) = k("selection.last") {
        third.push(format!("{x} last area"));
    }
    if let Some(x) = k("capture.pin") {
        third.push(format!("{x} pins the selection"));
    }
    if !third.is_empty() {
        lines.push(third.join(" \u{b7} "));
    }
    // The precision keys (Step 52).
    let mut fourth = Vec::new();
    let pointer: Option<Vec<String>> = ["up", "left", "down", "right"]
        .iter()
        .map(|d| k(&format!("pointer.move-{d}")))
        .collect();
    if let Some(p) = pointer {
        fourth.push(format!("{} move the pointer", p.join(" ")));
    }
    if let Some(x) = k("selection.mark") {
        fourth.push(format!("{x} marks a corner"));
    }
    if let Some(x) = k("detect.cycle") {
        fourth.push(format!("{x} windows, elements, off"));
    }
    if let Some(x) = k("magnifier.toggle") {
        fourth.push(format!("{x} magnifier"));
    }
    if let (Some(p), Some(n)) = (k("history.previous"), k("history.next")) {
        fourth.push(format!("{p}/{n} earlier areas"));
    }
    if !fourth.is_empty() {
        lines.push(fourth.join(" \u{b7} "));
    }
    lines
}

/// Something a pin window reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinEvent {
    /// A key press in a focused pin (matched against the `pin` context); `true` when it is an
    /// auto-repeat.
    Key(PinId, Chord, bool),
    /// The user dragged the pin to a new position.
    Moved(PinId, i32, i32),
    /// The window was closed by the system (e.g. Alt+F4 or end of session).
    Closed(PinId),
    /// The context menu was asked for: a right-click at this screen point, or the keyboard
    /// (`Shift+F10`, the menu key) with `None`.
    Menu(PinId, Option<(i32, i32)>),
    /// The mouse wheel turned over the pin: the delta (120 per notch, positive away from the
    /// user), the pointer, and whether `Ctrl` was held.
    Wheel(PinId, i32, (i32, i32), bool),
    /// A middle-click on the pin.
    MiddleClick(PinId),
    /// The pin's border was dragged (Step 54): which side, the picture's rectangle when the drag
    /// began, and how far the pointer moved since (device pixels).
    Resize(PinId, Edge, Rect, (i32, i32)),
    /// The user dragged the pin to a new position and released it with `Ctrl` held: it joins the
    /// group of the pin it was dropped on, if any (Step 54).
    Dropped(PinId, i32, i32),
    /// `Shift`+double-click: thumbnail mode on or off (Step 54).
    ShiftDoubleClick(PinId),
}

/// One entry of a pin's context menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMenuItem {
    /// The pin action it runs (`None` for a separator).
    pub action: Option<&'static str>,
    /// The text, with its key after a tab (`Close\tEscape`).
    pub label: String,
    pub enabled: bool,
    /// Shown with a check mark (a mode that is on: thumbnail, click-through).
    pub checked: bool,
}

/// A pin's context menu to show (NOEUL-FR-061: it lists every pin action of this build). The
/// runtime shows it modally with the core released, then reports the choice through
/// [`App::pin_menu_choice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMenu {
    pub pin: PinId,
    /// Where it opens: the pointer of a right-click; `None` from the keyboard (the runtime then
    /// opens it at the pin's top-left corner).
    pub at: Option<(i32, i32)>,
    pub items: Vec<PinMenuItem>,
}

/// The selection's context menu to show (UX spec §3.2): the runtime shows it modally with the
/// core released, owned by the overlay window, then reports the choice through
/// [`App::overlay_menu_choice`]. Never shown under the debug-build test hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayMenu {
    /// Where it opens (the pointer of a right-click, or the selection's corner).
    pub at: (i32, i32),
    pub items: Vec<PinMenuItem>,
}

/// A save the user asked for (`capture.save`, the Save button): the runtime shows the save
/// dialog with the core released and answers with [`App::save_chosen`]. Under the debug-build
/// test hook no dialog is shown: the answer is a new file in the explicitly configured
/// quick-save folder, or "cancelled" when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    /// The capture session it belongs to (a later answer for another session is ignored); 0 for
    /// a pin's picture.
    pub session: u64,
    pub rect: Rect,
    /// `pin.save`: the pin whose picture is saved (the dialog is owned by its window).
    pub pin: Option<PinId>,
    /// The suggested file name, `noeul-YYYYMMDD-HHMMSS.png` (NOEUL-FR-031).
    pub name: String,
    /// The folder the dialog starts in (the quick-save folder), if known.
    pub folder: Option<PathBuf>,
    /// Whether that folder was set by the user (`[capture] quick_save_folder`).
    pub explicit: bool,
}

/// The selection's context menu: this build's outputs and keys (NOEUL-FR-114).
const OVERLAY_MENU: &[Option<(&str, &str)>] = &[
    Some(("capture.pin", "Pin")),
    Some(("capture.copy", "Copy")),
    Some(("capture.save", "Save\u{2026}")),
    Some(("capture.quick-save", "Quick save")),
    None,
    Some(("selection.full", "Select monitor")),
    Some(("selection.last", "Last rectangle")),
    None,
    Some(("capture.cancel", "Cancel")),
];

/// How a pin is made from a capture selection (Step 51's gestures pass their own).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinOptions {
    /// Take keyboard focus: the user's own gesture (never under the debug-build test hook).
    pub focus: bool,
    /// Ignore presses on the new pin for this long: after a double-click, the system
    /// double-click time, so the rest of the gesture cannot touch the pin (UX spec §4.2 rule 10).
    pub guard: Option<Duration>,
}

/// The menu's entries: implemented pin actions only (NOEUL-FR-114).
const MENU: &[Option<(&str, &str)>] = &[
    Some(("pin.copy", "Copy")),
    Some(("pin.save", "Save\u{2026}")),
    Some(("pin.quick-save", "Quick save")),
    None,
    Some(("pin.annotate", "Annotate")),
    None,
    Some(("pin.zoom-in", "Zoom in")),
    Some(("pin.zoom-out", "Zoom out")),
    Some(("pin.zoom-reset", "Zoom 100 %, opaque")),
    Some(("pin.rotate-cw", "Rotate right")),
    Some(("pin.rotate-ccw", "Rotate left")),
    Some(("pin.flip-horizontal", "Flip horizontally")),
    Some(("pin.flip-vertical", "Flip vertically")),
    Some(("pin.opacity-up", "More opaque")),
    Some(("pin.opacity-down", "More transparent")),
    None,
    Some(("pin.thumbnail", "Thumbnail")),
    Some(("pin.click-through", "Click-through")),
    Some(("pin.group-new", "Move to a new group")),
    None,
    Some(("pin.hide", "Hide")),
    Some(("pin.close", "Close")),
    Some(("pin.destroy", "Destroy")),
];

/// Global actions without default keys (Step 54): the keyboard's way back from click-through
/// and hidden pins when no pin has focus (HOTKEY_CONTRACT §7.5: only `F1` and `F3` are global
/// defaults; the user binds these).
pub const PINS_CLICKABLE: &str = "pins.clickable";
pub const PINS_TOGGLE_HIDDEN: &str = "pins.toggle-hidden";

/// One wheel notch.
const WHEEL_DELTA: i32 = 120;

/// Static facts about the running instance.
#[derive(Debug, Clone)]
pub struct Identity {
    pub pid: u32,
    pub endpoint: String,
    pub session: u32,
}

pub struct App<S: Shell> {
    shell: S,
    capture: Box<dyn CaptureBackend>,
    ledger: Arc<Ledger>,
    captures: Captures,
    pins: Pins,
    hotkeys: Manager,
    settings: settings::Loaded,
    paths: Paths,
    id: Identity,
    started_at: SystemTime,
    started: Instant,
    quit: bool,
    last_action: Option<(String, &'static str)>,
    overlay: Option<OverlaySession>,
    latency: Latency,
    ui_state: UiState,
    menu: Option<PinMenu>,
    /// The selection's context menu the runtime should show.
    overlay_menu: Option<OverlayMenu>,
    /// The save dialog the runtime should show.
    save_request: Option<SaveRequest>,
    /// Wheel deltas not yet worth a zoom step, per pin (high-resolution wheels send small ones).
    wheel: BTreeMap<PinId, i32>,
    /// Pins left off-screen by the last display change (a notice says so when it changes).
    lost: usize,
}

impl<S: Shell> std::fmt::Debug for App<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("pins", &self.pins.len())
            .field("overlay", &self.overlay.as_ref().map(|o| o.model.phase()))
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

fn session_error(e: SessionError) -> NoeulError {
    match e {
        SessionError::Busy { .. } => NoeulError::new(Code::Busy, "another capture is in progress")
            .hint("finish or cancel it first"),
        other => NoeulError::new(Code::Internal, format!("capture session: {other:?}")),
    }
}

fn pin_not_found(id: &str) -> NoeulError {
    NoeulError::new(Code::PinNotFound, format!("no pin `{id}`"))
        .hint("run `noeul pin list` to see the open pins")
}

impl<S: Shell> App<S> {
    pub fn new(
        shell: S,
        capture: Box<dyn CaptureBackend>,
        hotkeys: Manager,
        settings: settings::Loaded,
        paths: Paths,
        id: Identity,
    ) -> App<S> {
        let mut pins = Pins::new();
        pins.set_look(settings.settings.pin_look);
        App {
            shell,
            capture,
            ledger: Ledger::new(),
            captures: Captures::new(),
            pins,
            hotkeys,
            settings,
            ui_state: UiState::load(&paths.ui_state_file()),
            paths,
            id,
            started_at: SystemTime::now(),
            started: Instant::now(),
            quit: false,
            last_action: None,
            overlay: None,
            latency: Latency::default(),
            menu: None,
            overlay_menu: None,
            save_request: None,
            wheel: BTreeMap::new(),
            lost: 0,
        }
    }

    /// Whether the capture overlay is open.
    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// The first-frame measurements so far.
    pub fn latency(&self) -> &Latency {
        &self.latency
    }

    pub fn shell(&mut self) -> &mut S {
        &mut self.shell
    }

    pub fn hotkeys(&mut self) -> &mut Manager {
        &mut self.hotkeys
    }

    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    pub fn pins(&self) -> &Pins {
        &self.pins
    }

    pub fn started_at(&self) -> String {
        rfc3339(self.started_at)
    }

    /// Set once a quit was accepted; the runtime then tears down.
    pub fn quitting(&self) -> bool {
        self.quit
    }

    /// One IPC request. Errors carry registered codes.
    pub fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        let dry_run = ipc::flag(params, "dry_run")?;
        match method {
            "status" => Ok(self.status()),
            "activate" => Ok(self.activate()),
            "capture" => self.capture(params, dry_run),
            "capture.region" => self.capture_region(Origin::Cli, Instant::now(), dry_run),
            "capture.cancel" => Ok(self.capture_cancel(dry_run)),
            "pin.list" => Ok(self.pins.list(&self.capture.desktop())),
            "pin.create" => self.pin_create(params, dry_run),
            "pin.hide" => self.pin_hide(params, dry_run),
            "pin.show" => self.pin_show(params, dry_run),
            "pin.close" => self.pin_close(params, dry_run),
            "pin.reopen" => self.pin_reopen(dry_run),
            "pin.set" => self.pin_set(params, dry_run),
            "group.list" => Ok(self.group_list()),
            "group.switch" => self.group_switch(params, dry_run),
            "hotkeys.reload" => {
                let problem = self.hotkeys.reload();
                Ok(json!({"reloaded": problem.is_none(), "problem": problem,
                          "hotkeys": self.hotkeys.status_json()}))
            }
            "settings.reload" => Ok(json!({"reloaded": true, "settings": self.reload_settings()})),
            "quit" => self.quit_request(ipc::flag(params, "force")?, dry_run),
            other => Err(NoeulError::new(
                Code::Usage,
                format!("unknown method `{other}`"),
            )),
        }
    }

    /// Re-reads `config.toml` (the settings watcher, IPC `settings.reload`). The gestures, the
    /// keyboard help and the quick-save folder apply from the next capture; the tray icon
    /// setting at the next start.
    pub fn reload_settings(&mut self) -> Value {
        self.settings = settings::load(&self.paths.config_file());
        self.pins.set_look(self.settings.settings.pin_look);
        self.settings.to_json()
    }

    pub fn settings(&self) -> &settings::Loaded {
        &self.settings
    }

    pub fn status(&self) -> Value {
        let (started, pinned, cancelled) = self.captures.counts();
        let (copied, saved) = self.captures.output_counts();
        json!({
            "running": true,
            "instance": {
                "pid": self.id.pid,
                "version": crate::VERSION,
                "started_at": rfc3339(self.started_at),
                "uptime_s": self.started.elapsed().as_secs(),
                "endpoint": self.id.endpoint,
                "session": self.id.session,
            },
            "desktop": self.capture.desktop().to_json(),
            "capture_backend": self.capture.name(),
            "pins": {"open": self.pins.len(), "reopenable": self.pins.reopenable().map(|p| p.to_string()),
                     "hidden": self.pins.hidden(), "off_screen": self.pins.off_screen(&self.capture.desktop()),
                     "click_through": self.pins.click_through(), "thumbnails": self.pins.thumbnails(),
                     "group": self.pins.current_group(), "groups": self.pins.groups().len(),
                     "editor": self.overlay.as_ref().filter(|o| !o.is_capture()).map(OverlaySession::to_json)},
            "captures": {"started": started, "pinned": pinned, "copied": copied, "saved": saved,
                         "cancelled": cancelled, "open": self.captures.is_open(),
                         "ignored": self.captures.ignored(),
                         "history": self.captures.history().len(),
                         "overlay": self.overlay.as_ref().filter(|o| o.is_capture()).map(OverlaySession::to_json),
                         "first_frame": self.latency.to_json()},
            "memory": self.ledger.to_json(),
            "hotkeys": self.hotkeys.status_json(),
            "tray": {"enabled": self.settings.settings.tray_icon, "shown": self.shell.tray_shown()},
            "settings": self.settings.to_json(),
            "last_action": self.last_action.as_ref().map(|(a, r)| json!({"action": a, "result": r})),
        })
    }

    /// A second start handed over (NOEUL-FR-003). The settings window it should open is N5;
    /// until then a second start is also a way back from click-through: every pin that lets
    /// clicks through becomes clickable again (NOEUL-FR-063, NOEUL-SEC-013).
    fn activate(&mut self) -> Value {
        let clickable = self.make_all_clickable();
        self.shell.notice(&if clickable.is_empty() {
            "Noeul is already running.".to_string()
        } else {
            format!(
                "Noeul is already running. {} pin(s) are clickable again.",
                clickable.len()
            )
        });
        json!({"activated": true, "pid": self.id.pid, "settings_window": "planned",
               "clickable": clickable})
    }

    /// Makes every pin that lets clicks through clickable again (the tray's "Make all pins
    /// clickable", `pins.clickable`, a second start); returns their IDs.
    fn make_all_clickable(&mut self) -> Vec<String> {
        let mut done = Vec::new();
        for id in self.pins.ids() {
            if self.pins.set_click_through(id, false) == Ok(true) {
                done.push(id.to_string());
            }
        }
        done
    }

    /// Whether a pin may start letting clicks through from its key or menu: only while the user
    /// has a way back that needs neither the pin nor a terminal — the tray's "Make all pins
    /// clickable" or a registered `pins.clickable` key (NOEUL-FR-063). The CLI may always turn it
    /// on (its caller holds the CLI way back), and a second start always turns it off.
    pub fn click_through_recoverable(&self) -> bool {
        self.shell.tray_shown() || self.hotkeys.is_registered(PINS_CLICKABLE)
    }

    /// `group.list`: the groups with their pin counts and the current group.
    fn group_list(&self) -> Value {
        json!({
            "current": self.pins.current_group(),
            "groups": self.pins.groups().into_iter().map(|g| json!({
                "name": g.name, "pins": g.pins, "hidden": g.hidden,
            })).collect::<Vec<_>>(),
        })
    }

    /// `group.switch`: switches to a group (created when new): its pins are shown, every other
    /// pin is hidden, and new pins go to it (NOEUL-FR-064). Nothing takes focus.
    fn group_switch(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let name = params["group"]
            .as_str()
            .and_then(crate::pins::group_name)
            .ok_or_else(group_usage)?;
        let (shown, hidden) = if dry_run {
            let mut s = Vec::new();
            let mut h = Vec::new();
            for id in self.pins.ids() {
                let visible = self.pins.info(id).is_some_and(|i| i.visible);
                match (self.pins.group(id) == Some(name.as_str()), visible) {
                    (true, false) => s.push(id),
                    (false, true) => h.push(id),
                    _ => {}
                }
            }
            (s, h)
        } else {
            self.pins.switch_group(&name)
        };
        if !dry_run {
            self.last_action = Some(("group.switch".into(), "switched"));
        }
        let ids = |v: Vec<PinId>| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        Ok(json!({"dry_run": dry_run, "group": name, "shown": ids(shown), "hidden": ids(hidden)}))
    }

    /// `pin.set` (`noeul pin set`): changes pins precisely (Step 54): `at` places one pin,
    /// `zoom`, `opacity`, `rotation`/`mirrored` (absolute), `rotate`/`flip` (relative),
    /// `thumbnail`, `click_through` and `move_to_group`. Every value is checked before anything
    /// changes; a dry run changes nothing.
    fn pin_set(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let c = SetChange::parse(params)?;
        if c.at.is_some() && ids.len() != 1 {
            return Err(NoeulError::new(
                Code::Usage,
                "`at` places one pin: name it with `id`",
            ));
        }
        let mut changed = Vec::new();
        if !dry_run {
            for id in &ids {
                if self.apply_set(*id, &c) {
                    changed.push(id.to_string());
                }
            }
            if !changed.is_empty() {
                self.last_action = Some(("pin.set".into(), "changed"));
            }
        }
        let desktop = self.capture.desktop();
        Ok(json!({
            "dry_run": dry_run,
            "targets": ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "changed": changed,
            "pins": ids.iter().filter_map(|id| self.pins.entry_of(&desktop, *id)).collect::<Vec<_>>(),
        }))
    }

    /// Applies one `pin.set` to a pin; whether anything changed.
    fn apply_set(&mut self, id: PinId, c: &SetChange) -> bool {
        let Some(info) = self.pins.info(id) else {
            return false;
        };
        let ok = |r: std::result::Result<bool, PinError>| r == Ok(true);
        let mut any = false;
        if let Some(g) = &c.group {
            any |= ok(self.pins.set_group(id, g));
        }
        if c.thumbnail == Some(false) {
            any |= ok(self.pins.set_thumbnail(id, false));
        }
        let o = info.transforms.orientation;
        let mut target = Orientation::new(
            c.rotation.unwrap_or(o.rotation()),
            c.mirrored.unwrap_or(o.mirrored()),
        )
        .unwrap_or(o);
        if let Some(cw) = c.rotate {
            target = if cw {
                target.rotate_cw()
            } else {
                target.rotate_ccw()
            };
        }
        if let Some(h) = c.flip {
            target = if h {
                target.flip_horizontal()
            } else {
                target.flip_vertical()
            };
        }
        any |= ok(self.pins.orient(id, |_| target));
        if let Some(z) = c.zoom {
            let anchor = self.pins.rect(id).map_or((0, 0), |r| (r.x, r.y));
            any |= ok(self.pins.set_zoom(id, z, anchor));
        }
        if let Some(op) = c.opacity {
            any |= ok(self.pins.set_opacity(id, op));
        }
        if let Some((x, y)) = c.at {
            let moved = self.pins.rect(id).is_some_and(|r| (r.x, r.y) != (x, y));
            if moved && self.pins.move_to(id, x, y) {
                any = true;
                self.update_dpi(id);
            }
        }
        if let Some(on) = c.click_through {
            any |= ok(self.pins.set_click_through(id, on));
        }
        if c.thumbnail == Some(true) {
            any |= ok(self.pins.set_thumbnail(id, true));
        }
        any
    }

    /// Draws a pin's outline for the DPI of the monitor it is on now (after a move or a display
    /// change); its picture keeps its pixel size.
    fn update_dpi(&mut self, id: PinId) {
        if let Some(r) = self.pins.rect(id) {
            let dpi = crate::pins::dpi_at(&self.capture.desktop(), r);
            let _ = self.pins.set_dpi(id, dpi);
        }
    }

    fn capture(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let region = params
            .get("region")
            .and_then(Rect::from_json)
            .ok_or_else(|| {
                NoeulError::new(
                    Code::Usage,
                    "`region` must be {x, y, width, height} in device pixels",
                )
            })?;
        if params.get("pin") != Some(&Value::Bool(true)) {
            return Err(NoeulError::new(
                Code::Usage,
                "the instance only pins captures in this build (`pin: true`)",
            )
            .hint("capture to a file or the clipboard is planned (NOEUL-FR-112)"));
        }
        let desktop = self.capture.desktop();
        check_region(&desktop, region).map_err(NoeulError::from)?;
        if dry_run {
            return Ok(
                json!({"dry_run": true, "region": region.to_json(), "pin": null,
                             "desktop": desktop.state.as_str()}),
            );
        }
        let frame = self
            .capture
            .capture(&self.ledger, region)
            .map_err(NoeulError::from)?;
        let sid = self
            .captures
            .begin(Origin::Cli, frame, Vec::new())
            .map_err(session_error)?;
        self.captures.select(sid, region).map_err(session_error)?;
        let frame = self.captures.finish_pin(sid).map_err(session_error)?;
        let id = self.create_pin(&desktop, frame, Source::Capture, Placement::Exact)?;
        self.last_action = Some(("capture".into(), "pinned"));
        Ok(
            json!({"dry_run": false, "region": region.to_json(), "pin": self.pin_ref(id),
                   "desktop": desktop.state.as_str()}),
        )
    }

    /// A pin requested over IPC: never with focus, so it never takes the keyboard from the
    /// program the user is in (NOEUL-FR-060); announced like every CLI capture or clipboard read
    /// (the default notice of NOEUL-SEC-010).
    fn create_pin(
        &mut self,
        desktop: &crate::capture::Desktop,
        frame: Frame,
        source: Source,
        placement: Placement,
    ) -> Result<PinId> {
        let new = NewPin {
            frame,
            source,
            placement,
            focus: false,
            guard: None,
            annotations: Vec::new(),
        };
        match self.pins.create(self.shell.pin_windows(), desktop, new) {
            Ok(id) => {
                self.shell.notice(match source {
                    Source::Capture => "Noeul captured a region (command line).",
                    Source::Clipboard => "Noeul pinned the clipboard (command line).",
                });
                Ok(id)
            }
            Err((frame, why)) => {
                drop(frame);
                Err(NoeulError::new(
                    Code::Internal,
                    format!("the pin window could not be created: {why}"),
                )
                .hint("nothing was pinned, copied or saved"))
            }
        }
    }

    /// `{id, rect, source, zoom}` of an open pin.
    fn pin_ref(&self, id: PinId) -> Value {
        self.pins.info(id).map_or(Value::Null, |i| {
            json!({"id": id.to_string(), "rect": i.rect.to_json(), "source": i.source.as_str(),
                   "zoom": i.zoom})
        })
    }

    /// The pins a request names: `all: true`, the pins of a `group`, or one `id` that must be
    /// open.
    fn pin_targets(&self, params: &Value) -> Result<Vec<PinId>> {
        if ipc::flag(params, "all")? {
            return Ok(self.pins.ids());
        }
        if let Some(g) = params.get("group").filter(|g| !g.is_null()) {
            let name = g
                .as_str()
                .and_then(crate::pins::group_name)
                .ok_or_else(group_usage)?;
            return Ok(self.pins.in_group(&name));
        }
        let text = params["id"]
            .as_str()
            .ok_or_else(|| NoeulError::new(Code::Usage, "give a pin id (`id`) or `all: true`"))?;
        let id = PinId::parse(text).ok_or_else(|| pin_not_found(text))?;
        if !self.pins.contains(id) {
            return Err(pin_not_found(text));
        }
        Ok(vec![id])
    }

    /// `pin.create`: the instance reads the source itself (pictures never cross the pipe).
    /// `source: "region"` captures that region of the unlocked desktop and pins it in place;
    /// `source: "clipboard"` reads the clipboard once and pins its image at `at` (or the
    /// pointer), inside that monitor's work area and zoomed out when larger (NOEUL-SEC-002,
    /// ADR 0029). A dry run captures and reads nothing.
    fn pin_create(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let source = params["source"].as_str().unwrap_or("");
        let desktop = self.capture.desktop();
        match source {
            "region" => {
                let region = params
                    .get("region")
                    .and_then(Rect::from_json)
                    .ok_or_else(|| {
                        NoeulError::new(
                            Code::Usage,
                            "`region` must be {x, y, width, height} in device pixels",
                        )
                    })?;
                check_region(&desktop, region).map_err(NoeulError::from)?;
                if dry_run {
                    return Ok(json!({"dry_run": true, "source": "region", "pin": null}));
                }
                let frame = self
                    .capture
                    .capture(&self.ledger, region)
                    .map_err(NoeulError::from)?;
                let sid = self
                    .captures
                    .begin(Origin::Cli, frame, Vec::new())
                    .map_err(session_error)?;
                self.captures.select(sid, region).map_err(session_error)?;
                let frame = self.captures.finish_pin(sid).map_err(session_error)?;
                let id = self.create_pin(&desktop, frame, Source::Capture, Placement::Exact)?;
                self.last_action = Some(("pin.create".into(), "pinned"));
                Ok(json!({"dry_run": false, "source": "region", "pin": self.pin_ref(id)}))
            }
            "clipboard" => {
                if let Some(e) = desktop.state.error() {
                    return Err(e);
                }
                let at = match params.get("at") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(point(v).ok_or_else(|| {
                        NoeulError::new(Code::Usage, "`at` must be {x, y} in device pixels")
                    })?),
                };
                if dry_run {
                    // A dry run never opens the clipboard.
                    return Ok(json!({"dry_run": true, "source": "clipboard", "pin": null}));
                }
                let pasted = self.shell.clipboard_image(&self.ledger)?;
                let (x, y) = at.unwrap_or_else(|| self.shell.pointer());
                let id = self.create_pin(
                    &desktop,
                    pasted.frame,
                    Source::Clipboard,
                    Placement::At(x, y),
                )?;
                self.first_frame_only(id, pasted.frames);
                self.last_action = Some(("pin.create".into(), "pinned"));
                Ok(json!({"dry_run": false, "source": "clipboard", "pin": self.pin_ref(id)}))
            }
            _ => Err(NoeulError::new(
                Code::Usage,
                "`source` must be \"region\" (with `region`) or \"clipboard\"",
            )
            .hint("pinning a file is planned (NOEUL-FR-073)")),
        }
    }

    /// `pin.hide`: hides pins without closing them. Nothing takes focus; a hidden pin that had
    /// it hands it back to the window the user was in before.
    fn pin_hide(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let mut hidden = Vec::new();
        for id in ids {
            let visible = self.pins.info(id).is_some_and(|i| i.visible);
            if visible && (dry_run || self.pins.hide(id).unwrap_or(false)) {
                hidden.push(id.to_string());
            }
        }
        if !dry_run && !hidden.is_empty() {
            self.last_action = Some(("pin.hide".into(), "hidden"));
        }
        Ok(json!({"dry_run": dry_run, "hidden": hidden}))
    }

    /// `pin.show`: shows hidden pins (above the others, without focus); with `on_screen`, also
    /// moves every named pin that no monitor shows into the nearest work area (NOEUL-FR-067).
    fn pin_show(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let bring = ipc::flag(params, "on_screen")?;
        let desktop = self.capture.desktop();
        let (mut shown, mut moved) = (Vec::new(), Vec::new());
        for id in ids {
            let Some(info) = self.pins.info(id) else {
                continue;
            };
            if !info.visible && (dry_run || self.pins.show(id).unwrap_or(false)) {
                shown.push(id.to_string());
            }
            let lost = bring
                && desktop.state == DesktopState::Available
                && !crate::pins::on_screen(&desktop, info.rect);
            if lost && (dry_run || self.pins.bring_on_screen(id, &desktop).unwrap_or(false)) {
                moved.push(id.to_string());
            }
        }
        if !dry_run && !(shown.is_empty() && moved.is_empty()) {
            self.last_action = Some(("pin.show".into(), "shown"));
        }
        Ok(json!({"dry_run": dry_run, "shown": shown, "moved": moved}))
    }

    fn pin_close(&mut self, params: &Value, dry_run: bool) -> Result<Value> {
        let ids = self.pin_targets(params)?;
        let names: Vec<String> = ids.iter().map(ToString::to_string).collect();
        if !dry_run {
            for id in &ids {
                let _ = self.pins.close(*id);
            }
        }
        Ok(json!({"dry_run": dry_run, "closed": names,
                  "reopenable": if dry_run { names.last().cloned() } else { self.pins.reopenable().map(|p| p.to_string()) }}))
    }

    fn pin_reopen(&mut self, dry_run: bool) -> Result<Value> {
        let Some(id) = self.pins.reopenable() else {
            return Err(
                NoeulError::new(Code::PinNotFound, "no closed pin to reopen")
                    .hint("Noeul remembers the last closed pin until it quits"),
            );
        };
        if dry_run {
            return Ok(json!({"dry_run": true, "pin": id.to_string()}));
        }
        match self.pins.reopen(self.shell.pin_windows(), false) {
            Ok(id) => Ok(json!({"dry_run": false, "pin": id.to_string()})),
            Err(PinError::Window(why)) => Err(NoeulError::new(
                Code::Internal,
                format!("the pin window could not be created: {why}"),
            )),
            Err(_) => Err(NoeulError::new(
                Code::PinNotFound,
                "no closed pin to reopen",
            )),
        }
    }

    fn quit_request(&mut self, force: bool, dry_run: bool) -> Result<Value> {
        let open = self.pins.len();
        if open > 0 && !force {
            return Err(NoeulError::new(
                Code::ConfirmationRequired,
                format!("{open} pin(s) are open; quitting closes them, and pins are not backed up"),
            )
            .hint("re-run with --yes, or --dry-run to preview"));
        }
        if dry_run {
            return Ok(json!({"dry_run": true, "quitting": false, "closed_pins": open}));
        }
        self.shutdown();
        Ok(json!({"dry_run": false, "quitting": true, "closed_pins": open}))
    }

    /// Ends everything the instance holds: the capture session, every pin and the global
    /// hotkeys (NOEUL-FR-002). Idempotent; the runtime calls it again on any exit path.
    pub fn shutdown(&mut self) {
        self.quit = true;
        if let Some(mut ov) = self.overlay.take() {
            ov.window.destroy();
            if let SessionKind::PinEdit(id) = ov.kind {
                let _ = self.pins.set_editing(id, false);
            }
        }
        self.overlay_menu = None;
        self.save_request = None;
        self.captures.cancel();
        self.pins.clear();
        self.hotkeys.release_all();
    }

    /// A Noeul action from a global hotkey, the tray, a pin's keys or its menu (by action ID).
    pub fn action(&mut self, action: &str, pin: Option<PinId>) {
        let found = |ok: bool, word: &'static str| if ok { word } else { "not_found" };
        if let (Some(id), "pin.close" | "pin.destroy") = (pin, action) {
            // A pin that goes away ends its edit (nothing is committed).
            self.cancel_pin_edit_of(id);
        }
        let result = match (action, pin) {
            ("capture.region", _) => self.capture_region_action(Origin::Hotkey, Instant::now()),
            ("paste.float", _) => self.paste_float(),
            ("pin.annotate", Some(id)) => self.edit_pin(id),
            ("pin.close", Some(id)) => found(self.pins.close(id).is_ok(), "closed"),
            ("pin.destroy", Some(id)) => found(self.pins.destroy(id).is_ok(), "destroyed"),
            ("pin.copy", Some(id)) => self.copy_pin(id),
            ("pin.save", Some(id)) => self.request_pin_save(id),
            ("pin.quick-save", Some(id)) => self.quick_save_pin(id),
            ("pin.hide", Some(id)) => match self.pins.hide(id) {
                Ok(_) => {
                    if self.pins.hidden() == 1 {
                        self.shell.notice(
                            "Pin hidden. Show it again from the tray (Show all pins) or with `noeul pin show --all`.",
                        );
                    }
                    "hidden"
                }
                Err(_) => "not_found",
            },
            ("pin.move-up", Some(id)) => self.nudge(id, 0, -1),
            ("pin.move-down", Some(id)) => self.nudge(id, 0, 1),
            ("pin.move-left", Some(id)) => self.nudge(id, -1, 0),
            ("pin.move-right", Some(id)) => self.nudge(id, 1, 0),
            ("pin.zoom-in" | "pin.zoom-out", Some(id)) => {
                let anchor = self.pins.zoom_anchor(id, self.shell.pointer());
                self.zoom_step(id, action, anchor)
            }
            ("pin.zoom-reset", Some(id)) => {
                let anchor = self.pins.zoom_anchor(id, self.shell.pointer());
                self.reset_view(id, anchor)
            }
            ("pin.rotate-cw", Some(id)) => self.orient(id, Orientation::rotate_cw),
            ("pin.rotate-ccw", Some(id)) => self.orient(id, Orientation::rotate_ccw),
            ("pin.flip-horizontal", Some(id)) => self.orient(id, Orientation::flip_horizontal),
            ("pin.flip-vertical", Some(id)) => self.orient(id, Orientation::flip_vertical),
            ("pin.opacity-up", Some(id)) => self.opacity_step(id, true),
            ("pin.opacity-down", Some(id)) => self.opacity_step(id, false),
            ("pin.thumbnail", Some(id)) => self.toggle_thumbnail(id),
            ("pin.click-through", Some(id)) => self.toggle_click_through(id),
            ("pin.group-new", Some(id)) => self.new_group(id),
            (PINS_CLICKABLE, _) => {
                if self.make_all_clickable().is_empty() {
                    "unchanged"
                } else {
                    "clickable"
                }
            }
            (PINS_TOGGLE_HIDDEN, _) => self.toggle_hidden(),
            _ => "ignored",
        };
        self.last_action = Some((action.to_string(), result));
    }

    /// `pin.zoom-reset` (`0`, the middle button): 100 %, fully opaque, and out of thumbnail mode
    /// (NOEUL-FR-061); the orientation stays.
    fn reset_view(&mut self, id: PinId, anchor: Option<(i32, i32)>) -> &'static str {
        if !self.pins.contains(id) {
            return "not_found";
        }
        let thumb = self.pins.set_thumbnail(id, false) == Ok(true);
        let opaque = self
            .pins
            .set_opacity(id, transform::OPACITY_FULL)
            .unwrap_or(false);
        match self.zoom_step(id, "pin.zoom-reset", anchor) {
            "unchanged" if thumb || opaque => "reset",
            r => r,
        }
    }

    /// Rotates or flips a pin (`pin.rotate-*`, `pin.flip-*`).
    fn orient(&mut self, id: PinId, f: fn(Orientation) -> Orientation) -> &'static str {
        match self.pins.orient(id, f) {
            Ok(true) => "transformed",
            Ok(false) => "unchanged",
            Err(PinError::NotFound(_)) => "not_found",
            Err(_) => "failed",
        }
    }

    /// `pin.opacity-up`/`-down` (`Ctrl+Plus`/`Ctrl+Minus`, `Ctrl`+wheel): 10 % steps, never below
    /// 10 % (NOEUL-FR-062).
    fn opacity_step(&mut self, id: PinId, up: bool) -> &'static str {
        let Some(info) = self.pins.info(id) else {
            return "not_found";
        };
        let o = info.transforms.opacity;
        let to = if up {
            transform::opacity_up(o)
        } else {
            transform::opacity_down(o)
        };
        match to.map(|o| self.pins.set_opacity(id, o)) {
            Some(Ok(true)) => "changed",
            _ => "unchanged",
        }
    }

    /// `pin.thumbnail` (`T`, `Shift`+double-click, the menu): thumbnail mode on or off.
    fn toggle_thumbnail(&mut self, id: PinId) -> &'static str {
        let Some(info) = self.pins.info(id) else {
            return "not_found";
        };
        let on = info.transforms.thumbnail.is_none();
        match self.pins.set_thumbnail(id, on) {
            Ok(true) if on => "thumbnail",
            Ok(true) => "full_size",
            Ok(false) => "unchanged",
            Err(_) => "failed",
        }
    }

    /// `pin.click-through` (`Ctrl+Shift+T`, the menu): the pin lets the mouse through, or not
    /// (NOEUL-FR-063). Turning it on needs a way back that works without the pin (the tray or a
    /// registered `pins.clickable` key); turning it off always works.
    fn toggle_click_through(&mut self, id: PinId) -> &'static str {
        let Some(info) = self.pins.info(id) else {
            return "not_found";
        };
        let on = !info.transforms.click_through;
        if on && !self.click_through_recoverable() {
            return "no_recovery_path";
        }
        match self.pins.set_click_through(id, on) {
            Ok(true) if on => {
                self.shell.notice(
                    "The pin lets clicks through. Make it clickable again with Ctrl+Shift+T while it has focus, the tray's Make all pins clickable, or by starting Noeul again.",
                );
                "on"
            }
            Ok(true) => "off",
            Ok(false) => "unchanged",
            Err(_) => "failed",
        }
    }

    /// `pin.group-new` (the menu): moves the pin into a new group of its own (`group-2`, …).
    fn new_group(&mut self, id: PinId) -> &'static str {
        let name = self.pins.new_group_name();
        match self.pins.set_group(id, &name) {
            Ok(_) => {
                self.shell.notice(&format!(
                    "{id} is now in {name}. Drop other pins on it with Ctrl held to add them."
                ));
                "grouped"
            }
            Err(_) => "not_found",
        }
    }

    /// `pins.toggle-hidden` (a global action without default keys): shows every pin when one is
    /// hidden, otherwise hides them all.
    fn toggle_hidden(&mut self) -> &'static str {
        let ids = self.pins.ids();
        if self.pins.hidden() > 0 {
            for id in ids {
                let _ = self.pins.show(id);
            }
            "shown"
        } else if ids.is_empty() {
            "unchanged"
        } else {
            for id in ids {
                let _ = self.pins.hide(id);
            }
            "hidden"
        }
    }

    /// One zoom step of a pin around `anchor` (`pin.zoom-in`, `-out`, `-reset`).
    fn zoom_step(&mut self, id: PinId, action: &str, anchor: Option<(i32, i32)>) -> &'static str {
        let (Some(info), Some(anchor)) = (self.pins.info(id), anchor) else {
            return "not_found";
        };
        let (w, h) = info.image;
        let to = match action {
            "pin.zoom-in" => zoom::zoom_in(w, h, info.zoom),
            "pin.zoom-out" => zoom::zoom_out(info.zoom),
            _ => Some(zoom::FULL),
        };
        match to.map(|z| self.pins.set_zoom(id, z, anchor)) {
            Some(Ok(true)) => "zoomed",
            Some(Ok(false)) | None => "unchanged",
            Some(Err(_)) => "failed",
        }
    }

    fn nudge(&mut self, id: PinId, dx: i32, dy: i32) -> &'static str {
        if self.pins.nudge(id, dx, dy) {
            "moved"
        } else {
            "not_found"
        }
    }

    /// `paste.float`: read the clipboard once and pin its image at the pointer, at 100 % unless
    /// it is larger than the work area of the monitor there (then zoomed out to fit, ADR 0029;
    /// NOEUL-FR-070, image formats only in this build). Nothing to pin → a notice, no change
    /// (NOEUL-FR-074). While the capture overlay is open it does nothing: a new pin would cover
    /// the overlay or take its focus.
    fn paste_float(&mut self) -> &'static str {
        if self.capture.desktop().state != DesktopState::Available {
            return "desktop_unavailable";
        }
        if self.overlay.is_some() {
            return "overlay_open";
        }
        let pasted = match self.shell.clipboard_image(&self.ledger) {
            Ok(p) => p,
            Err(e) => {
                let text = match e.code {
                    Code::NothingToPin => "Nothing to pin: the clipboard holds no image.",
                    Code::ClipboardUnavailable => {
                        "The clipboard is in use by another program; try again."
                    }
                    Code::ImageInvalid => "The clipboard image could not be read.",
                    _ => "The clipboard could not be read.",
                };
                self.shell.notice(text);
                return e.code.as_str();
            }
        };
        let (px, py) = self.shell.pointer();
        let desktop = self.capture.desktop();
        let new = NewPin {
            frame: pasted.frame,
            source: Source::Clipboard,
            placement: Placement::At(px, py),
            focus: self.shell.interactive(),
            guard: None,
            annotations: Vec::new(),
        };
        match self.pins.create(self.shell.pin_windows(), &desktop, new) {
            Ok(id) => {
                self.first_frame_only(id, pasted.frames);
                "pinned"
            }
            Err((_, _)) => {
                self.shell.notice("The pin window could not be created.");
                "failed"
            }
        }
    }

    /// A pin made from an animated GIF shows its first frame only: it is marked, and a notice
    /// says so (NOEUL-FR-068: never flattened silently; animated pins are not in this build).
    fn first_frame_only(&mut self, id: PinId, frames: u32) {
        if frames > 1 && self.pins.mark_animation(id, frames) {
            self.shell.notice(&format!(
                "This GIF has {frames} frames; Noeul shows only the first. Animated pins are not supported yet."
            ));
        }
    }

    /// `capture.region`: freezes every monitor and opens the selection overlay over the whole
    /// virtual desktop (NOEUL-FR-010). While an overlay is open it does nothing and counts the
    /// request as ignored, so rapid repeated presses never open a second one and never capture
    /// again (NOEUL-FR-013). On a locked, secure or missing desktop nothing is captured
    /// (NOEUL-SEC-003). `trigger` is when the request arrived; the first-frame latency is
    /// measured from it (NOEUL-NFR-003).
    pub fn capture_region(
        &mut self,
        origin: Origin,
        trigger: Instant,
        dry_run: bool,
    ) -> Result<Value> {
        if self.overlay.as_ref().is_some_and(|o| !o.is_capture()) {
            // A pin is being edited: one overlay at a time (NOEUL-FR-013).
            if !dry_run {
                self.captures.note_ignored();
            }
            return Ok(
                json!({"dry_run": dry_run, "opened": false, "reason": "already_open",
                "session": null, "picture": self.overlay.as_ref().map(|o| o.model.picture().to_json()),
                "monitors": self.overlay.as_ref().map_or(0, |o| o.model.monitors().len()),
                "first_frame_us": null}),
            );
        }
        if let Some(open) = self.captures.open_id() {
            if !dry_run {
                self.captures.note_ignored();
            }
            return Ok(
                json!({"dry_run": dry_run, "opened": false, "reason": "already_open",
                "session": open, "picture": self.overlay.as_ref().map(|o| o.model.picture().to_json()),
                "monitors": self.overlay.as_ref().map_or(0, |o| o.model.monitors().len()),
                "first_frame_us": null}),
            );
        }
        let desktop = self.capture.desktop();
        if let Some(e) = desktop.state.error() {
            return Err(e);
        }
        let Some(picture) = desktop.virtual_rect else {
            return Err(DesktopState::NoDisplay
                .error()
                .unwrap_or_else(|| NoeulError::new(Code::Internal, "no display")));
        };
        if dry_run {
            return Ok(
                json!({"dry_run": true, "opened": false, "reason": null, "session": null,
                "picture": picture.to_json(), "monitors": desktop.monitors.len(), "first_frame_us": null}),
            );
        }
        let frame = self
            .capture
            .capture(&self.ledger, picture)
            .map_err(NoeulError::from)?;
        let captured = Instant::now();
        let id = self
            .captures
            .begin(origin, frame, desktop.monitors.clone())
            .map_err(session_error)?;
        let mut window = match self.shell.open_overlay(picture, false) {
            Ok(w) => w,
            Err(why) => {
                self.captures.cancel();
                return Err(NoeulError::new(
                    Code::Internal,
                    format!("the capture overlay could not be created: {why}"),
                )
                .hint("the frozen picture was discarded; nothing was pinned, copied or saved"));
            }
        };
        let help = self.help_lines();
        let monitors = desktop.monitors.len();
        let mode = self.settings.settings.detection;
        // The windows as the frozen picture shows them, listed before the overlay is shown
        // (rectangles only); nothing is listed while detection is off (Step 52).
        let snapshot = (mode != DetectMode::Off).then(|| self.shell.window_snapshot(picture));
        let model = Overlay::new(
            picture,
            desktop.monitors,
            self.shell.system_values(),
            self.shell.pointer(),
            self.captures.last(),
            help,
        )
        .with_gestures(self.settings.settings.gestures)
        .with_detection(mode, snapshot)
        .with_magnifier(self.settings.settings.magnifier)
        .with_history(&self.captures.history());
        let mut scene = model.scene(&*window);
        if let Some(frozen) = self.captures.frozen() {
            scene.sample(frozen);
            window.present(frozen, None, &scene, &[picture]);
        }
        window.set_cursor(model.cursor());
        if !window.show(self.shell.interactive()) {
            window.destroy();
            self.captures.cancel();
            return Err(NoeulError::new(
                Code::Internal,
                "the capture overlay could not be shown",
            ));
        }
        let done = Instant::now();
        let sample = LatencySample {
            total_us: micros(done.saturating_duration_since(trigger)),
            capture_us: micros(captured.saturating_duration_since(trigger)),
            render_us: micros(done.saturating_duration_since(captured)),
            width: picture.width,
            height: picture.height,
        };
        self.latency.push(sample);
        self.overlay = Some(OverlaySession {
            id,
            kind: SessionKind::Capture,
            model,
            window,
            scene,
            layer: None,
            base: None,
            ime: None,
        });
        self.last_action = Some(("capture.region".into(), "opened"));
        Ok(
            json!({"dry_run": false, "opened": true, "reason": null, "session": id,
            "picture": picture.to_json(), "monitors": monitors, "first_frame_us": sample.total_us}),
        )
    }

    /// `capture.region` from a hotkey or the tray: the result word for `last_action`; a failure
    /// that is not "busy" or "locked" is shown as a notice.
    fn capture_region_action(&mut self, origin: Origin, trigger: Instant) -> &'static str {
        match self.capture_region(origin, trigger, false) {
            Ok(v) if v["opened"] == true => "opened",
            Ok(_) => "ignored",
            Err(e) => {
                if e.code != Code::DesktopUnavailable {
                    self.shell.notice("Noeul could not capture the screen.");
                }
                e.code.as_str()
            }
        }
    }

    /// IPC `capture.cancel`: closes an open overlay without output.
    fn capture_cancel(&mut self, dry_run: bool) -> Value {
        let open = self.captures.open_id();
        if !dry_run && !self.cancel_overlay() {
            self.captures.cancel();
        }
        json!({"dry_run": dry_run, "cancelled": open.is_some() && !dry_run, "session": open})
    }

    /// Whether the overlay shows its keyboard help now (`[capture] keyboard_help`).
    fn help_lines(&self) -> Option<Vec<String>> {
        let show = match self.settings.settings.keyboard_help {
            KeyboardHelp::FirstRun => !self.ui_state.overlay_help_seen,
            KeyboardHelp::Always => true,
            KeyboardHelp::Never => false,
        };
        show.then(|| overlay_help(&self.hotkeys, self.settings.settings.gestures))
    }

    /// The first capture finished from the overlay: the first-run help is not shown again.
    fn help_done(&mut self) {
        if self.settings.settings.keyboard_help == KeyboardHelp::FirstRun
            && !self.ui_state.overlay_help_seen
        {
            self.ui_state.overlay_help_seen = true;
            // A hint only: if the file cannot be written, the help shows again next time.
            let _ = self.ui_state.save(&self.paths.ui_state_file());
        }
    }

    /// Routes an overlay window event.
    pub fn overlay_event(&mut self, e: OverlayEvent) {
        if self.overlay.is_none() {
            return;
        }
        let input = match e {
            OverlayEvent::Ime(ime) => {
                let outcome = match self.overlay.as_mut() {
                    Some(ov) => ov.model.ime(ime),
                    None => return,
                };
                self.after_overlay_input();
                return self.overlay_outcome(outcome);
            }
            OverlayEvent::Char(c, repeat) => match self.overlay_char(c, repeat) {
                Some(i) => i,
                None => return,
            },
            OverlayEvent::Wheel(delta) => OverlayInput::Wheel(delta),
            OverlayEvent::Input(i) => {
                if matches!(i, OverlayInput::Press { .. }) {
                    // The user's double-click time, rectangle and drag threshold as they are now,
                    // so a change in Windows applies from the next press (UX spec §4.2 rule 13).
                    let sys = self.shell.system_values();
                    if let Some(ov) = self.overlay.as_mut() {
                        ov.model.set_system_values(sys);
                    }
                }
                i
            }
            OverlayEvent::Key(chord, code, repeat) => {
                match self.overlay_key(&chord, code.as_ref(), repeat) {
                    Some(i) => i,
                    None => return,
                }
            }
            OverlayEvent::Element(request, answer) => OverlayInput::Element { request, answer },
        };
        let outcome = match self.overlay.as_mut() {
            Some(ov) => ov.model.handle(input),
            None => return,
        };
        self.after_overlay_input();
        self.overlay_outcome(outcome);
    }

    /// What the model asks of the platform after an input (Step 52): the real pointer follows a
    /// pointer key, a UI Automation request goes out, and a window list is taken when detection
    /// was turned on during the capture.
    fn after_overlay_input(&mut self) {
        let Some(ov) = self.overlay.as_mut() else {
            return;
        };
        if let Some(at) = ov.model.take_warp() {
            ov.window.warp_pointer(at);
        }
        let request = ov.model.take_element_request();
        let picture = ov.model.picture();
        if ov.model.needs_snapshot() {
            let s = self.shell.window_snapshot(picture);
            if let Some(ov) = self.overlay.as_mut() {
                ov.model.set_snapshot(s);
            }
        }
        if let Some(req) = request {
            self.shell.request_element(req);
        }
    }

    fn overlay_outcome(&mut self, outcome: OverlayOutcome) {
        if let Some(SessionKind::PinEdit(id)) = self.overlay.as_ref().map(|o| o.kind) {
            return match outcome {
                OverlayOutcome::Output(Output::Pin, rect, _) => self.commit_pin_edit(id, rect),
                OverlayOutcome::Cancel => {
                    self.cancel_overlay();
                }
                _ => self.redraw_overlay(),
            };
        }
        match outcome {
            OverlayOutcome::Stay => self.redraw_overlay(),
            OverlayOutcome::Cancel => {
                self.cancel_overlay();
            }
            OverlayOutcome::Output(out, rect, via) => match out {
                Output::Pin => self.pin_from_overlay(rect, via),
                Output::Copy => self.copy_from_overlay(rect),
                Output::Save => self.request_save(rect),
                Output::QuickSave => self.quick_save(rect),
            },
            OverlayOutcome::Menu(at) => self.open_overlay_menu(at),
        }
    }

    /// A key press in the overlay: the navigation keys go to a focused toolbar button first
    /// (ADR 0021 §5), then the `capture-overlay` bindings, then the reserved `Escape`; `Tab`
    /// and `Shift+Tab` move the focus into the toolbar when no binding uses them.
    fn overlay_key(
        &self,
        chord: &Chord,
        code: Option<&Chord>,
        repeat: bool,
    ) -> Option<OverlayInput> {
        let model = &self.overlay.as_ref()?.model;
        if model.editing_text() {
            // A label is edited: only its editing keys and Escape (which ends it) work; every
            // other shortcut is suspended (UX spec §6, NOEUL-NFR-014).
            return text_key_of(chord).map(OverlayInput::TextKey).or_else(|| {
                (!repeat && is_escape(chord))
                    .then_some(OverlayInput::Annotate(AnnotateAction::Done))
            });
        }
        let focused = model.toolbar_focus().is_some();
        let nav = nav_of(chord);
        if focused
            && let Some(n) = nav
            && !(repeat && matches!(n, Nav::Activate | Nav::Escape))
        {
            return Some(OverlayInput::Nav(n));
        }
        // The annotate context is inside the overlay's: its keys win while a selection is
        // confirmed; `annotate.done` only while there is something to end (Step 56).
        if model.annotate_context()
            && let Some(a) = self
                .hotkeys
                .annotate_action(chord, code, repeat)
                .as_deref()
                .and_then(AnnotateAction::from_id)
            && (a != AnnotateAction::Done || model.annotation_active())
        {
            return Some(OverlayInput::Annotate(a));
        }
        if let Some(a) = self
            .hotkeys
            .overlay_action(chord, code, repeat)
            .as_deref()
            .and_then(OverlayAction::from_id)
        {
            return Some(if repeat {
                OverlayInput::Repeat(a)
            } else {
                OverlayInput::Action(a)
            });
        }
        // The reserved navigation key always leaves (nav.cancel, NOEUL-SEC-013).
        if !repeat && is_escape(chord) {
            return Some(OverlayInput::Leave);
        }
        match nav {
            Some(n @ (Nav::Next | Nav::Prev)) => Some(OverlayInput::Nav(n)),
            _ => None,
        }
    }

    /// A typed character in the overlay: text for a label being edited, else a `char:` binding
    /// of the annotate context (`[`, `]`; Step 56).
    fn overlay_char(&self, c: char, repeat: bool) -> Option<OverlayInput> {
        let model = &self.overlay.as_ref()?.model;
        if model.editing_text() {
            return Some(OverlayInput::Char(c));
        }
        // Letters and digits are matched from their key press (a named key); their character
        // would fire the same binding twice.
        if !model.annotate_context() || c.is_ascii_alphanumeric() {
            return None;
        }
        let chord = crate::hotkeys::char_chord(c)?;
        self.hotkeys
            .annotate_action(&chord, None, repeat)
            .as_deref()
            .and_then(AnnotateAction::from_id)
            .filter(|a| *a != AnnotateAction::Done)
            .map(OverlayInput::Annotate)
    }

    /// Redraws what changed since the last frame (nothing when nothing changed): typesets the
    /// labels, re-renders the annotated pixels that changed, and follows the label's input method.
    fn redraw_overlay(&mut self) {
        let Some(ov) = self.overlay.as_mut() else {
            return;
        };
        ov.model.typeset(&*ov.window);
        let frozen = match &ov.base {
            Some(b) => Some(b),
            None => self.captures.frozen(),
        };
        let changed = ov.model.take_annotation_dirty();
        let mut extra = update_layer(&mut ov.layer, frozen, &ov.model, changed);
        let mut scene = ov.model.scene(&*ov.window);
        if let Some(f) = frozen {
            scene.sample(f);
        }
        let mut dirty = overlay::dirty(&ov.scene, &scene);
        for r in extra.drain(..) {
            if !dirty.contains(&r) {
                dirty.push(r);
            }
        }
        if !dirty.is_empty()
            && let Some(frozen) = frozen
        {
            ov.window.present(frozen, ov.layer.as_ref(), &scene, &dirty);
        }
        ov.window.set_cursor(ov.model.cursor());
        let caret = ov.model.ime_caret();
        if caret != ov.ime {
            ov.window.set_ime(caret);
            ov.ime = caret;
        }
        ov.scene = scene;
    }

    /// Closes the overlay without any output: the frozen picture is freed (NOEUL-FR-012).
    fn cancel_overlay(&mut self) -> bool {
        let Some(mut ov) = self.overlay.take() else {
            return false;
        };
        ov.window.destroy();
        if let SessionKind::PinEdit(id) = ov.kind {
            // The pin comes back exactly as it was (Step 56).
            self.end_pin_edit(id);
            self.last_action = Some(("pin.annotate".into(), "cancelled"));
            return true;
        }
        self.shell.end_detection();
        self.captures.cancel();
        self.overlay_menu = None;
        self.save_request = None;
        self.last_action = Some(("capture.cancel".into(), "cancelled"));
        true
    }

    /// The annotations of the open capture overlay inside `rect`, in picture pixels.
    fn overlay_annotations(&self, rect: Rect) -> Vec<Object> {
        self.overlay
            .as_ref()
            .map(|o| annotation::inside(o.model.annotations(), rect))
            .unwrap_or_default()
    }

    /// The open session's selection set to `rect`, as a [`CaptureSelection`].
    fn overlay_selection(&mut self, rect: Rect) -> Option<(u64, CaptureSelection)> {
        let id = self.overlay.as_ref().filter(|o| o.is_capture())?.id;
        self.captures
            .select(id, rect)
            .and_then(|()| self.captures.selection(id))
            .ok()
            .map(|s| (id, s))
    }

    /// An output succeeded: the session ends (its frozen picture is freed), the overlay closes.
    fn finish_overlay(
        &mut self,
        id: u64,
        how: SessionOutcome,
        action: &'static str,
        result: &'static str,
    ) {
        let _ = self.captures.complete(id, how);
        if let Some(mut ov) = self.overlay.take() {
            ov.window.destroy();
            self.shell.end_detection();
        }
        self.overlay_menu = None;
        self.save_request = None;
        self.help_done();
        self.last_action = Some((action.into(), result));
    }

    /// An output failed: the overlay stays with its selection and says why (NOEUL-FR-025).
    fn fail_overlay(&mut self, action: &'static str, result: &'static str, banner: &str) {
        if let Some(ov) = self.overlay.as_mut() {
            ov.model.set_banner(Some(banner.to_string()));
        }
        self.redraw_overlay();
        self.last_action = Some((action.into(), result));
    }

    /// `capture.pin`, the double-click, the middle-click, the Pin button or the menu: the
    /// session's selection becomes a pin at the same place and size, with keyboard focus; the
    /// overlay closes. Nothing touches the clipboard or the disk (NOEUL-FR-020, NOEUL-SEC-005).
    /// After a mouse gesture the new pin ignores presses for one double-click time, so the rest
    /// of a triple or quadruple click cannot move, zoom or close it (UX spec §4.2 rule 10). If
    /// the pin cannot be created, the overlay stays with its selection and says so
    /// (NOEUL-FR-025).
    fn pin_from_overlay(&mut self, rect: Rect, via: Via) {
        let Some(ov) = self.overlay.as_ref() else {
            return;
        };
        let guard = via
            .pointer()
            .then(|| Duration::from_millis(u64::from(ov.model.system_values().double_click_ms)));
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.pin",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let opts = PinOptions { focus: true, guard };
        // The pin keeps the unchanged pixels and the annotations as objects (Step 56).
        let objects = annotation::translate_all(&self.overlay_annotations(rect), -rect.x, -rect.y);
        match self.pin_selection_with(sel, opts, objects) {
            Ok(_) => self.finish_overlay(id, SessionOutcome::Pinned, "capture.pin", "pinned"),
            Err(_) => self.fail_overlay(
                "capture.pin",
                "failed",
                "The pin could not be created. Your selection is kept: try again, copy, save, or press Esc.",
            ),
        }
    }

    /// `capture.copy`, the Copy button, the menu (or the double-click set to `copy`): the
    /// selection goes to the clipboard as PNG and `CF_DIBV5`, then the overlay closes
    /// (NOEUL-FR-023, NOEUL-FR-030). Nothing is pinned or written to disk. A busy or failing
    /// clipboard keeps the overlay with a banner, so the user can retry, pin or save.
    fn copy_from_overlay(&mut self, rect: Rect) {
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.copy",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let annotated = annotation::flatten(sel.frame(), &self.overlay_annotations(rect), rect);
        let out = annotated.as_ref().unwrap_or(sel.frame());
        let encoded = output::png(out).map(|png| (png, output::dib_v5(out)));
        drop(annotated);
        drop(sel);
        let written = match encoded {
            Ok((png, dib)) => self.shell.clipboard_write(&png, &dib),
            Err(why) => Err(NoeulError::new(
                Code::Internal,
                format!("PNG encoding failed: {why}"),
            )),
        };
        match written {
            Ok(()) => self.finish_overlay(id, SessionOutcome::Copied, "capture.copy", "copied"),
            Err(e) if e.code == Code::ClipboardUnavailable => self.fail_overlay(
                "capture.copy",
                "clipboard_unavailable",
                "The clipboard is in use by another program; nothing was copied. Try again, pin, save, or press Esc.",
            ),
            Err(_) => self.fail_overlay(
                "capture.copy",
                "failed",
                "The picture could not be copied. Try again, pin, save, or press Esc.",
            ),
        }
    }

    /// The quick-save folder: `[capture] quick_save_folder`, else the Pictures folder + `Noeul`
    /// (never under the debug-build test hook), and whether the user set it.
    fn quick_save_folder(&self) -> (Option<PathBuf>, bool) {
        match &self.settings.settings.quick_save_folder {
            Some(f) => (Some(f.clone()), true),
            None => (self.shell.pictures_dir().map(|p| p.join("Noeul")), false),
        }
    }

    /// `capture.save`, the Save button, the menu (or the double-click set to `save`): asks the
    /// runtime for the save dialog. The overlay stays open behind it; nothing is written until
    /// the user chose a file ([`App::save_chosen`]).
    fn request_save(&mut self, rect: Rect) {
        let Some(id) = self
            .overlay
            .as_ref()
            .filter(|o| o.is_capture())
            .map(|o| o.id)
        else {
            return;
        };
        if self.captures.select(id, rect).is_err() {
            return self.fail_overlay(
                "capture.save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        }
        let (folder, explicit) = self.quick_save_folder();
        self.save_request = Some(SaveRequest {
            session: id,
            rect,
            pin: None,
            name: output::file_name(self.shell.local_time()),
            folder,
            explicit,
        });
        self.last_action = Some(("capture.save".into(), "asked"));
        self.redraw_overlay();
    }

    /// The save dialog the runtime should show now, if one was asked for.
    pub fn take_save_request(&mut self) -> Option<SaveRequest> {
        self.save_request.take()
    }

    /// The user's answer to the save dialog: `Some(path)` writes the selection there as PNG
    /// (the dialog already asked before replacing a file) and closes the overlay; `None`
    /// (cancelled) leaves the overlay as it was. An answer for a session that has ended since is
    /// ignored.
    pub fn save_chosen(&mut self, req: &SaveRequest, path: Option<PathBuf>) {
        if let Some(pin) = req.pin {
            return self.pin_saved(pin, path);
        }
        if self
            .overlay
            .as_ref()
            .filter(|o| o.is_capture())
            .map(|o| o.id)
            != Some(req.session)
        {
            return;
        }
        let Some(path) = path else {
            self.last_action = Some(("capture.save".into(), "cancelled"));
            return self.redraw_overlay();
        };
        let Some((id, sel)) = self.overlay_selection(req.rect) else {
            return self.fail_overlay(
                "capture.save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let annotated =
            annotation::flatten(sel.frame(), &self.overlay_annotations(req.rect), req.rect);
        let written = output::png(annotated.as_ref().unwrap_or(sel.frame()))
            .map_err(std::io::Error::other)
            .and_then(|bytes| output::write_file(&path, &bytes, true));
        drop(annotated);
        drop(sel);
        match written {
            Ok(()) => {
                self.shell.notice(&format!("Saved to {}", path.display()));
                self.finish_overlay(id, SessionOutcome::Saved, "capture.save", "saved");
            }
            Err(_) => self.fail_overlay(
                "capture.save",
                "failed",
                "The picture could not be saved there. Try another folder, copy, pin, or press Esc.",
            ),
        }
    }

    /// `capture.quick-save` (or `Shift`+Save): a new PNG in the quick-save folder, never
    /// overwriting a file, then the overlay closes (NOEUL-FR-032). Without a folder (none set,
    /// none known) the overlay stays and says how to set one.
    fn quick_save(&mut self, rect: Rect) {
        let (Some(folder), _) = self.quick_save_folder() else {
            return self.fail_overlay(
                "capture.quick-save",
                "no_folder",
                "No quick-save folder: set one with `noeul config set capture.quick_save_folder <folder>`, or press Esc.",
            );
        };
        let Some((id, sel)) = self.overlay_selection(rect) else {
            return self.fail_overlay(
                "capture.quick-save",
                "failed",
                "The selection could not be taken. Try again, or press Esc.",
            );
        };
        let name = output::file_name(self.shell.local_time());
        let annotated = annotation::flatten(sel.frame(), &self.overlay_annotations(rect), rect);
        let written = output::png(annotated.as_ref().unwrap_or(sel.frame()))
            .map_err(std::io::Error::other)
            .and_then(|bytes| output::quick_save(&folder, &name, &bytes));
        drop(annotated);
        drop(sel);
        match written {
            Ok(path) => {
                self.shell.notice(&format!("Saved to {}", path.display()));
                self.finish_overlay(
                    id,
                    SessionOutcome::Saved,
                    "capture.quick-save",
                    "saved",
                );
            }
            Err(_) => self.fail_overlay(
                "capture.quick-save",
                "failed",
                "The picture could not be saved in the quick-save folder. Copy, pin, save elsewhere, or press Esc.",
            ),
        }
    }

    /// `pin.copy` (`Ctrl+C`, the pin's menu): the pin's picture at 100 % on the clipboard as PNG
    /// and `CF_DIBV5`; a busy clipboard is reported in a notice (NOEUL-FR-030).
    fn copy_pin(&mut self, id: PinId) -> &'static str {
        let rendered = self.pins.rendered(id);
        let Some(frame) = rendered.as_ref().or(self.pins.picture(id)) else {
            return "not_found";
        };
        let encoded = output::png(frame).map(|png| (png, output::dib_v5(frame)));
        drop(rendered);
        let written = match encoded {
            Ok((png, dib)) => self.shell.clipboard_write(&png, &dib),
            Err(why) => Err(NoeulError::new(Code::Internal, why)),
        };
        match written {
            Ok(()) => "copied",
            Err(e) => {
                self.shell.notice(if e.code == Code::ClipboardUnavailable {
                    "The clipboard is in use by another program; nothing was copied."
                } else {
                    "The picture could not be copied."
                });
                if e.code == Code::ClipboardUnavailable {
                    "clipboard_unavailable"
                } else {
                    "failed"
                }
            }
        }
    }

    /// `pin.save` (`Ctrl+S`, the pin's menu): asks the runtime for the save dialog, owned by the
    /// pin; [`App::save_chosen`] writes the pin's picture at 100 % to the chosen file.
    fn request_pin_save(&mut self, id: PinId) -> &'static str {
        let Some(frame) = self.pins.picture(id) else {
            return "not_found";
        };
        let rect = frame.rect();
        let (folder, explicit) = self.quick_save_folder();
        self.save_request = Some(SaveRequest {
            session: 0,
            rect,
            pin: Some(id),
            name: output::file_name(self.shell.local_time()),
            folder,
            explicit,
        });
        "asked"
    }

    /// The answer to a pin's save dialog (the pin may be gone by then).
    fn pin_saved(&mut self, id: PinId, path: Option<PathBuf>) {
        let rendered = self.pins.rendered(id);
        let result = match (path, rendered.as_ref().or(self.pins.picture(id))) {
            (None, _) => "cancelled",
            (Some(_), None) => "not_found",
            (Some(path), Some(frame)) => {
                let written = output::png(frame)
                    .map_err(std::io::Error::other)
                    .and_then(|bytes| output::write_file(&path, &bytes, true));
                match written {
                    Ok(()) => {
                        self.shell.notice(&format!("Saved to {}", path.display()));
                        "saved"
                    }
                    Err(_) => {
                        self.shell
                            .notice("The picture could not be saved there. Try another folder.");
                        "failed"
                    }
                }
            }
        };
        self.last_action = Some(("pin.save".into(), result));
    }

    /// `pin.quick-save` (`Ctrl+Shift+S`): a new PNG of the pin's picture in the quick-save
    /// folder, never overwriting (NOEUL-FR-032).
    fn quick_save_pin(&mut self, id: PinId) -> &'static str {
        let (Some(folder), _) = self.quick_save_folder() else {
            self.shell.notice(
                "No quick-save folder: set one with `noeul config set capture.quick_save_folder <folder>`.",
            );
            return "no_folder";
        };
        let rendered = self.pins.rendered(id);
        let Some(frame) = rendered.as_ref().or(self.pins.picture(id)) else {
            return "not_found";
        };
        let name = output::file_name(self.shell.local_time());
        let written = output::png(frame)
            .map_err(std::io::Error::other)
            .and_then(|bytes| output::quick_save(&folder, &name, &bytes));
        drop(rendered);
        match written {
            Ok(path) => {
                self.shell.notice(&format!("Saved to {}", path.display()));
                "saved"
            }
            Err(_) => {
                self.shell
                    .notice("The picture could not be saved in the quick-save folder.");
                "failed"
            }
        }
    }

    /// The selection's context menu (right-click inside, `Shift+F10`, the menu key): at the
    /// pointer, or at the selection's corner from the keyboard. Under the debug-build test hook
    /// it is recorded, never shown (a popup menu always lands on a real monitor).
    fn open_overlay_menu(&mut self, at: Option<(i32, i32)>) {
        let Some(sel) = self.overlay.as_ref().and_then(|o| o.model.selection()) else {
            return;
        };
        if !self.shell.interactive() {
            self.last_action = Some(("capture.menu".into(), "not_shown"));
            return;
        }
        let last = self.captures.last().is_some();
        let items = OVERLAY_MENU
            .iter()
            .map(|entry| match entry {
                None => PinMenuItem {
                    action: None,
                    label: String::new(),
                    enabled: false,
                    checked: false,
                },
                Some((action, text)) => PinMenuItem {
                    action: Some(action),
                    checked: false,
                    label: self
                        .hotkeys
                        .keys_for(action)
                        .or(match *action {
                            "capture.cancel" => Some("Escape"),
                            _ => None,
                        })
                        .map_or_else(|| (*text).to_string(), |k| format!("{text}\t{k}")),
                    enabled: *action != "selection.last" || last,
                },
            })
            .collect();
        self.overlay_menu = Some(OverlayMenu {
            at: at.unwrap_or((sel.x.saturating_add(8), sel.y.saturating_add(8))),
            items,
        });
        self.last_action = Some(("capture.menu".into(), "shown"));
    }

    /// The selection's context menu the runtime should show now, if one was asked for.
    pub fn take_overlay_menu(&mut self) -> Option<OverlayMenu> {
        self.overlay_menu.take()
    }

    /// The user chose `action` in the selection's context menu (the overlay may be gone).
    pub fn overlay_menu_choice(&mut self, action: &str) {
        if !OVERLAY_MENU.iter().flatten().any(|(a, _)| *a == action) {
            return;
        }
        let Some(a) = OverlayAction::from_id(action) else {
            return;
        };
        let outcome = match self.overlay.as_mut() {
            Some(ov) => ov.model.handle(OverlayInput::Action(a)),
            None => return,
        };
        self.overlay_outcome(match outcome {
            OverlayOutcome::Output(o, r, _) => OverlayOutcome::Output(o, r, Via::Menu),
            other => other,
        });
    }

    /// The pin engine's entry point for a confirmed capture selection: a pin of exactly that
    /// rectangle at the same place and pixel size, at 100 %, with no clipboard or file in between
    /// (NOEUL-FR-020, NOEUL-FR-025, NOEUL-SEC-005). `Enter` (`capture.pin`) uses it today; Step
    /// 51's double-click calls it with `guard` set to the system double-click time. Focus is
    /// taken only when `opts.focus` asks and the instance is interactive (never under the test
    /// hook). On failure the selection's copy is dropped and the reason returned; the capture
    /// session and its frozen picture are untouched, so the overlay can stay open.
    pub fn pin_selection(
        &mut self,
        sel: CaptureSelection,
        opts: PinOptions,
    ) -> std::result::Result<PinId, String> {
        self.pin_selection_with(sel, opts, Vec::new())
    }

    /// [`App::pin_selection`] with annotations in image pixels, which the pin keeps editable
    /// (Step 56).
    pub fn pin_selection_with(
        &mut self,
        sel: CaptureSelection,
        opts: PinOptions,
        annotations: Vec<Object>,
    ) -> std::result::Result<PinId, String> {
        let desktop = self.capture.desktop();
        let new = NewPin {
            frame: sel.into_frame(),
            source: Source::Capture,
            placement: Placement::Exact,
            focus: opts.focus && self.shell.interactive(),
            guard: opts.guard,
            annotations,
        };
        self.pins
            .create(self.shell.pin_windows(), &desktop, new)
            .map_err(|(frame, why)| {
                drop(frame);
                why
            })
    }

    /// `pin.annotate` (`Space`, the pin menu): opens the annotation editor over the pin's
    /// picture at 100 % (Step 56, NOEUL-FR-080): a layered window as large as the pin and its
    /// toolbar, the pin's annotations ready to edit; the pin's window hides meanwhile. While a
    /// capture overlay or another edit is open nothing happens (one overlay at a time).
    fn edit_pin(&mut self, id: PinId) -> &'static str {
        if self.overlay.is_some() {
            return "busy";
        }
        let desktop = self.capture.desktop();
        if desktop.state != DesktopState::Available {
            return "desktop_unavailable";
        }
        let Some(frame) = self.pins.picture(id) else {
            return "not_found";
        };
        let pin = frame.rect();
        let center = (
            i32::try_from(i64::from(pin.x) + i64::from(pin.width / 2)).unwrap_or(pin.x),
            i32::try_from(i64::from(pin.y) + i64::from(pin.height / 2)).unwrap_or(pin.y),
        );
        let m = desktop
            .monitor_near(center.0, center.1)
            .copied()
            .unwrap_or(crate::capture::Monitor::new(pin, true));
        let canvas = editor_canvas(pin, &m);
        // The picture on a transparent canvas: the window shows what is behind the rest.
        let mut pixels = vec![0u8; canvas.bgra_bytes() as usize];
        let (cw, src) = (canvas.width as usize * 4, frame.pixels());
        let (dx, dy) = (
            (i64::from(pin.x) - i64::from(canvas.x)) as usize,
            (i64::from(pin.y) - i64::from(canvas.y)) as usize,
        );
        let row = pin.width as usize * 4;
        for r in 0..pin.height as usize {
            let at = (dy + r) * cw + dx * 4;
            pixels[at..at + row].copy_from_slice(&src[r * row..(r + 1) * row]);
        }
        let Ok(base) = Frame::new(&self.ledger, canvas, pixels) else {
            return "failed";
        };
        let objects =
            annotation::translate_all(self.pins.annotations(id).unwrap_or_default(), pin.x, pin.y);
        let model = Overlay::for_pin(
            canvas,
            desktop.monitors,
            self.shell.system_values(),
            pin,
            objects,
        );
        let mut window = match self.shell.open_overlay(canvas, true) {
            Ok(w) => w,
            Err(_) => {
                self.shell
                    .notice("The pin could not be opened for annotation.");
                return "failed";
            }
        };
        let scene = model.scene(&*window);
        window.present(&base, None, &scene, &[canvas]);
        window.set_cursor(model.cursor());
        if !window.show(self.shell.interactive()) {
            window.destroy();
            return "failed";
        }
        let _ = self.pins.set_editing(id, true);
        self.overlay = Some(OverlaySession {
            id: 0,
            kind: SessionKind::PinEdit(id),
            model,
            window,
            scene,
            layer: None,
            base: Some(base),
            ime: None,
        });
        // The pin's annotations are drawn into the layer right away.
        self.redraw_overlay();
        "editing"
    }

    /// Done or `Enter` in the pin editor: the annotations (and the crop to `keep`) go to the pin,
    /// which shows them; the editor closes. A failure keeps the editor open with a banner.
    fn commit_pin_edit(&mut self, id: PinId, keep: Rect) {
        let objects = match self.overlay.as_ref() {
            Some(ov) => ov.model.annotations().to_vec(),
            None => return,
        };
        match self.pins.commit_edit(id, &objects, keep) {
            Ok(()) => {
                if let Some(mut ov) = self.overlay.take() {
                    ov.window.destroy();
                }
                self.end_pin_edit(id);
                self.last_action = Some(("pin.annotate".into(), "committed"));
            }
            Err(PinError::NotFound(_)) => {
                self.cancel_overlay();
            }
            Err(_) => {
                if let Some(ov) = self.overlay.as_mut() {
                    ov.model.set_banner(Some(
                        "The pin could not be updated. Try again, or press Esc.".to_string(),
                    ));
                }
                self.redraw_overlay();
                self.last_action = Some(("pin.annotate".into(), "failed"));
            }
        }
    }

    /// The pin edited before shows again and takes the keyboard back (the user's gesture).
    fn end_pin_edit(&mut self, id: PinId) {
        let _ = self.pins.set_editing(id, false);
        if self.shell.interactive() {
            self.pins.focus(id);
        }
    }

    /// Cancels the edit of pin `id`, if it is being edited (it is closed or destroyed).
    fn cancel_pin_edit_of(&mut self, id: PinId) {
        if self
            .overlay
            .as_ref()
            .is_some_and(|o| o.kind == SessionKind::PinEdit(id))
        {
            self.cancel_overlay();
        }
    }

    /// Routes a pin window event.
    pub fn pin_event(&mut self, e: PinEvent) {
        match e {
            PinEvent::Key(id, chord, repeat) => {
                if let Some(a) = self.hotkeys.pin_action(&chord, repeat) {
                    self.action(&a, Some(id));
                } else if !repeat && is_escape(&chord) {
                    // The reserved navigation key always closes the focused pin when no pin
                    // binding uses it, even with shortcuts off: no trapped UI (NOEUL-SEC-013).
                    self.action("pin.close", Some(id));
                }
            }
            PinEvent::Moved(id, x, y) => {
                self.pins.moved(id, x, y);
                self.update_dpi(id);
            }
            PinEvent::Dropped(id, x, y) => {
                // Released with Ctrl over another pin: join its group (Step 54).
                self.pins.moved(id, x, y);
                self.update_dpi(id);
                let target = self.pins.rect(id).and_then(|r| {
                    let c = (
                        i32::try_from(i64::from(r.x) + i64::from(r.width / 2)).unwrap_or(r.x),
                        i32::try_from(i64::from(r.y) + i64::from(r.height / 2)).unwrap_or(r.y),
                    );
                    self.pins.pin_under(id, c)
                });
                let group = target.and_then(|t| self.pins.group(t).map(str::to_string));
                let result = match group {
                    Some(g) if self.pins.set_group(id, &g) == Ok(true) => {
                        self.shell.notice(&format!("{id} joined {g}."));
                        "joined"
                    }
                    Some(_) => "unchanged",
                    None => "moved",
                };
                self.last_action = Some(("pin.drop".into(), result));
            }
            PinEvent::Resize(id, edge, start, delta) => {
                let r = match self.pins.resize(id, edge, start, delta) {
                    Ok(true) => "resized",
                    Ok(false) => "unchanged",
                    Err(PinError::NotFound(_)) => "not_found",
                    Err(_) => "failed",
                };
                self.last_action = Some(("pin.resize".into(), r));
            }
            PinEvent::ShiftDoubleClick(id) => self.action("pin.thumbnail", Some(id)),
            PinEvent::Closed(id) => {
                self.cancel_pin_edit_of(id);
                let _ = self.pins.close(id);
            }
            PinEvent::Menu(id, at) => {
                if !self.pins.contains(id) {
                    return;
                }
                if self.shell.interactive() {
                    self.menu = Some(PinMenu {
                        pin: id,
                        at,
                        items: self.pin_menu_items(id),
                    });
                } else {
                    // Under the debug-build test hook a popup menu would open on the user's
                    // screen (Windows keeps menus on a monitor): it is recorded, not shown.
                    self.last_action = Some(("pin.menu".into(), "not_shown"));
                }
            }
            PinEvent::Wheel(id, delta, pointer, ctrl) => {
                // Away from the user zooms in (with Ctrl: more opaque); towards the user zooms
                // out (with Ctrl: more transparent). High-resolution wheels add up to notches.
                if !self.pins.contains(id) {
                    return;
                }
                let acc = self.wheel.entry(id).or_insert(0);
                *acc += delta;
                let steps = *acc / WHEEL_DELTA;
                *acc %= WHEEL_DELTA;
                let action = match (steps > 0, ctrl) {
                    (true, false) => "pin.zoom-in",
                    (false, false) => "pin.zoom-out",
                    (true, true) => "pin.opacity-up",
                    (false, true) => "pin.opacity-down",
                };
                for _ in 0..steps.unsigned_abs() {
                    let r = if ctrl {
                        self.opacity_step(id, steps > 0)
                    } else {
                        // Around the pointer when it is over the pin (it is, for a real wheel).
                        let anchor = self.pins.zoom_anchor(id, pointer);
                        self.zoom_step(id, action, anchor)
                    };
                    self.last_action = Some((action.to_string(), r));
                }
            }
            PinEvent::MiddleClick(id) => {
                let anchor = self.pins.zoom_anchor(id, self.shell.pointer());
                let r = self.reset_view(id, anchor);
                self.last_action = Some(("pin.zoom-reset".into(), r));
            }
        }
        self.wheel.retain(|id, _| self.pins.contains(*id));
    }

    /// The context menu of one pin, from the effective keys (a changed key reads as such).
    pub fn pin_menu_items(&self, id: PinId) -> Vec<PinMenuItem> {
        let info = self.pins.info(id);
        let zoom = info.map_or(zoom::FULL, |i| i.zoom);
        let t = info.map(|i| i.transforms).unwrap_or_default();
        let thumb = t.thumbnail.is_some();
        let (w, h) = info.map_or((1, 1), |i| t.orientation.size(i.image.0, i.image.1));
        let mut items: Vec<PinMenuItem> = MENU
            .iter()
            .map(|entry| match entry {
                None => PinMenuItem {
                    action: None,
                    label: String::new(),
                    enabled: false,
                    checked: false,
                },
                Some((action, text)) => {
                    let keys = self.hotkeys.keys_for(action).or(match *action {
                        // Escape closes a pin even without a binding (nav.cancel).
                        "pin.close" => Some("Escape"),
                        _ => None,
                    });
                    let enabled = match *action {
                        "pin.zoom-in" => !thumb && zoom::zoom_in(w, h, zoom).is_some(),
                        "pin.zoom-out" => !thumb && zoom::zoom_out(zoom).is_some(),
                        "pin.zoom-reset" => {
                            zoom != zoom::FULL || t.opacity != transform::OPACITY_FULL || thumb
                        }
                        "pin.opacity-up" => t.opacity < transform::OPACITY_FULL,
                        "pin.opacity-down" => t.opacity > transform::OPACITY_MIN,
                        // Turning it on needs a way back without the pin (NOEUL-FR-063).
                        "pin.click-through" => t.click_through || self.click_through_recoverable(),
                        _ => true,
                    };
                    let text = match *action {
                        "pin.group-new" => format!(
                            "{text} (now in {})",
                            self.pins.group(id).unwrap_or(crate::pins::DEFAULT_GROUP)
                        ),
                        "pin.opacity-down" if t.opacity != transform::OPACITY_FULL => {
                            format!("{text} ({} %)", t.opacity)
                        }
                        _ => (*text).to_string(),
                    };
                    PinMenuItem {
                        action: Some(action),
                        label: keys.map_or_else(|| text.clone(), |k| format!("{text}\t{k}")),
                        enabled,
                        checked: match *action {
                            "pin.thumbnail" => thumb,
                            "pin.click-through" => t.click_through,
                            _ => false,
                        },
                    }
                }
            })
            .collect();
        if let Some(frames) = info.and_then(|i| i.animation) {
            // Never flattened silently (NOEUL-FR-068): the menu says what the pin shows.
            items.push(PinMenuItem {
                action: None,
                label: String::new(),
                enabled: false,
                checked: false,
            });
            items.push(PinMenuItem {
                action: Some("pin.animation"),
                label: format!(
                    "Animated GIF: first of {frames} frames only (playback not supported yet)"
                ),
                enabled: false,
                checked: false,
            });
        }
        items
    }

    /// The pin menu the runtime should show now, if one was asked for.
    pub fn take_pin_menu(&mut self) -> Option<PinMenu> {
        self.menu.take()
    }

    /// The user chose `action` in the menu of `pin` (the pin may be gone by then).
    pub fn pin_menu_choice(&mut self, pin: PinId, action: &str) {
        if MENU.iter().flatten().any(|(a, _)| *a == action) {
            self.action(action, Some(pin));
        }
    }

    /// Monitors were attached, removed or rearranged, or a work area changed
    /// (`WM_DISPLAYCHANGE`, `WM_SETTINGCHANGE`). Pins keep their place, even off-screen
    /// (NOEUL-FR-067): the model only follows where the system put each window. When a pin is
    /// left where no monitor shows it, a notice names the way back (NOEUL-SEC-013).
    pub fn display_changed(&mut self) {
        self.pins.sync();
        for id in self.pins.ids() {
            self.update_dpi(id);
        }
        let desktop = self.capture.desktop();
        if desktop.state != DesktopState::Available {
            return;
        }
        let off = self.pins.off_screen(&desktop);
        let before = std::mem::replace(&mut self.lost, off);
        if off > 0 && off != before {
            self.shell.notice(&format!(
                "{off} pin(s) are off-screen after the display change. Use Bring pins on screen in the tray, or `noeul pin show --all --on-screen`."
            ));
        }
        self.last_action = Some((
            "display.change".into(),
            if off > 0 { "off_screen" } else { "ok" },
        ));
    }

    /// The tray menu for the current state (items appear only once implemented, NOEUL-FR-004).
    pub fn tray_menu(&self) -> Vec<MenuItem> {
        menu(&crate::tray::MenuState {
            reopenable: self.pins.reopenable().is_some(),
            shortcuts_on: self.hotkeys.status_json()["enabled"] == true,
            shortcuts_switchable: !hotkeys_forced_off(&self.hotkeys),
            open: self.pins.len(),
            hidden: self.pins.hidden(),
            off_screen: self.pins.off_screen(&self.capture.desktop()),
            click_through: self.pins.click_through(),
            groups: self.pins.groups().len(),
        })
    }

    /// The tray menu as shown: each item with its text (groups by name) and check mark.
    pub fn tray_entries(&self) -> Vec<crate::tray::Entry> {
        let groups = self.pins.groups();
        let current = self.pins.current_group();
        self.tray_menu()
            .into_iter()
            .map(|item| match item {
                MenuItem::Group(i) => {
                    let name = groups.get(usize::from(i)).map_or("", |g| g.name.as_str());
                    crate::tray::Entry {
                        item,
                        label: format!("Group: {name}"),
                        checked: name == current,
                    }
                }
                _ => crate::tray::Entry {
                    item,
                    label: item.label().to_string(),
                    checked: false,
                },
            })
            .collect()
    }

    /// Hides or shows every pin, or brings the lost ones on screen (tray items).
    fn all_pins(&mut self, item: MenuItem) {
        let desktop = self.capture.desktop();
        for id in self.pins.ids() {
            match item {
                MenuItem::HideAllPins => {
                    let _ = self.pins.hide(id);
                }
                MenuItem::ShowAllPins => {
                    let _ = self.pins.show(id);
                }
                MenuItem::BringPinsOnScreen => {
                    let _ = self.pins.show(id);
                    let _ = self.pins.bring_on_screen(id, &desktop);
                }
                _ => {}
            }
        }
    }

    pub fn tray_click(&mut self, click: Click) {
        match click {
            Click::Middle => self.action("paste.float", None),
            // Left click starts a capture (NOEUL-FR-004).
            Click::Left => {
                let r = self.capture_region_action(Origin::Tray, Instant::now());
                self.last_action = Some(("capture.region".into(), r));
            }
            // The menu opens (the runtime shows it).
            Click::Right => {}
        }
    }

    pub fn tray_command(&mut self, item: MenuItem) {
        match item {
            MenuItem::CaptureRegion => {
                let r = self.capture_region_action(Origin::Tray, Instant::now());
                self.last_action = Some(("capture.region".into(), r));
            }
            MenuItem::PinClipboard => self.action("paste.float", None),
            MenuItem::HideAllPins | MenuItem::ShowAllPins | MenuItem::BringPinsOnScreen => {
                self.all_pins(item);
            }
            MenuItem::ReopenClosedPin => {
                let focus = self.shell.interactive();
                let _ = self.pins.reopen(self.shell.pin_windows(), focus);
            }
            MenuItem::MakeAllPinsClickable => self.action(PINS_CLICKABLE, None),
            MenuItem::Group(i) => {
                if let Some(g) = self.pins.groups().get(usize::from(i)) {
                    let name = g.name.clone();
                    self.pins.switch_group(&name);
                    self.last_action = Some(("group.switch".into(), "switched"));
                }
            }
            MenuItem::Shortcuts { on } => {
                if let Err(e) = self.switch_shortcuts(!on) {
                    self.shell.notice(&e.message);
                }
            }
            MenuItem::Quit => {
                let open = self.pins.len();
                if open == 0 || self.shell.confirm_quit(open) {
                    self.shutdown();
                }
            }
        }
    }

    /// The tray's "Keyboard shortcuts on/off": the same switch as `noeul hotkeys disable --all`
    /// (§12.4), written to `hotkeys.json` and applied at once.
    fn switch_shortcuts(&mut self, on: bool) -> Result<()> {
        use kim_hotkeys::edit::{self, Switch};
        let (state, loaded) = crate::hotkeys::load(&self.paths);
        if loaded.state.safe_mode() {
            return Err(NoeulError::new(
                Code::HotkeysInvalid,
                "hotkeys.json is invalid or newer; fix or reset it first",
            ));
        }
        let switch = if on {
            Switch::EnableAll
        } else {
            Switch::DisableAll
        };
        let applied = edit::apply_switch(&[state], &[], 0, &switch);
        if applied.changed {
            let layer = applied
                .layer
                .unwrap_or_else(|| kim_hotkeys::UserLayer::new(crate::hotkeys::TOOL));
            kim_hotkeys::store::save(&loaded.path, &layer, false)
                .map_err(|e| NoeulError::io(loaded.path.display(), &e))?;
        }
        self.hotkeys.reload();
        Ok(())
    }

    /// A pressed global hotkey (from the registration thread's waker).
    pub fn hotkey_pressed(&mut self, id: kim_hotkeys::register::HotkeyId) {
        // The first-frame latency of the overlay counts from here (NOEUL-NFR-003).
        let trigger = Instant::now();
        // NOEUL-SEC-003: no action while the user's desktop is not the input desktop.
        if self.capture.desktop().state != DesktopState::Available {
            self.last_action = Some(("hotkey".into(), "desktop_unavailable"));
            return;
        }
        match self.hotkeys.action_for(id) {
            Some(a) if a == "capture.region" => {
                let r = self.capture_region_action(Origin::Hotkey, trigger);
                self.last_action = Some((a, r));
            }
            Some(a) => self.action(&a, None),
            None => {}
        }
    }
}

/// Keeps the annotated layer of an overlay in step with its model (Step 56): when there are
/// annotations, `layer` holds the selection's pixels of `frozen` with the annotations drawn on
/// them, re-rendered where `changed` says (or entirely when the selection moved). Returns the
/// rectangles to repaint besides the scene's.
fn update_layer(
    layer: &mut Option<Frame>,
    frozen: Option<&Frame>,
    model: &Overlay,
    changed: Option<Rect>,
) -> Vec<Rect> {
    let mut extra: Vec<Rect> = changed.into_iter().collect();
    let objects = model.annotations();
    match (model.selection(), frozen) {
        (Some(sel), Some(f)) if !objects.is_empty() => {
            if layer.as_ref().map(Frame::rect) != Some(sel) {
                if let Some(old) = layer.take() {
                    extra.push(old.rect());
                }
                *layer = annotation::flatten(f, objects, sel);
                extra.push(sel);
            } else if let (Some(l), Some(c)) = (layer.as_mut(), changed)
                && let Some(area) = overlay::intersection(c, sel)
                && let Some(done) = annotation::refresh(l, f, objects, area)
                && done != area
            {
                // A mosaic or blur the edit touched was re-rendered whole (Step 57).
                extra.push(done);
            }
        }
        _ => {
            if let Some(old) = layer.take() {
                extra.push(old.rect());
            }
        }
    }
    extra
}

/// The pin editor's canvas for a pin at `pin` on monitor `m`: the pin, its toolbar and room for
/// the size readout above it, inside the monitor (the pin itself may stick out of it).
fn editor_canvas(pin: Rect, m: &crate::capture::Monitor) -> Rect {
    let bar = overlay::toolbar_layout(&overlay::EDIT_TOOLBAR, pin, m, None).rect;
    let band = i64::from(m.px(36));
    let chip = Rect::new(
        pin.x,
        i32::try_from(i64::from(pin.y) - band).unwrap_or(pin.y),
        pin.width.max(m.px(360)),
        u32::try_from(band).unwrap_or(0),
    );
    let chip = overlay::intersection(chip, m.rect).unwrap_or(pin);
    let (l, t) = (
        i64::from(pin.x)
            .min(i64::from(bar.x))
            .min(i64::from(chip.x)),
        i64::from(pin.y)
            .min(i64::from(bar.y))
            .min(i64::from(chip.y)),
    );
    let (r, b) = (
        pin.right().max(bar.right()).max(chip.right()),
        pin.bottom().max(bar.bottom()).max(chip.bottom()),
    );
    Rect::new(
        i32::try_from(l).unwrap_or(pin.x),
        i32::try_from(t).unwrap_or(pin.y),
        u32::try_from(r - l).unwrap_or(pin.width),
        u32::try_from(b - t).unwrap_or(pin.height),
    )
}

/// The editing key of a label a chord means (`Backspace`, `Delete`, arrows, `Home`, `End`,
/// `Enter` for a new line), without modifiers.
fn text_key_of(c: &Chord) -> Option<TextKey> {
    let is =
        |name: &str| crate::hotkeys::chord_of(name, false, false, false).is_some_and(|k| k == *c);
    [
        ("Backspace", TextKey::Backspace),
        ("Delete", TextKey::Delete),
        ("Left", TextKey::Left),
        ("Right", TextKey::Right),
        ("Home", TextKey::Home),
        ("End", TextKey::End),
        ("Enter", TextKey::Newline),
    ]
    .into_iter()
    .find(|(name, _)| is(name))
    .map(|(_, k)| k)
}

fn is_escape(c: &Chord) -> bool {
    crate::hotkeys::chord_of("Escape", false, false, false).is_some_and(|e| e == *c)
}

/// The toolbar navigation a key means (UX spec §5): `Tab`, `Shift+Tab`, the arrows, `Enter`,
/// `Space` and `Escape`, without other modifiers.
fn nav_of(c: &Chord) -> Option<Nav> {
    let is = |name: &str, shift: bool| {
        crate::hotkeys::chord_of(name, false, false, shift).is_some_and(|k| k == *c)
    };
    [
        ("Tab", false, Nav::Next),
        ("Tab", true, Nav::Prev),
        ("Left", false, Nav::Left),
        ("Up", false, Nav::Left),
        ("Right", false, Nav::Right),
        ("Down", false, Nav::Right),
        ("Enter", false, Nav::Activate),
        ("Space", false, Nav::Activate),
        ("Escape", false, Nav::Escape),
    ]
    .into_iter()
    .find(|(name, shift, _)| is(name, *shift))
    .map(|(_, _, n)| n)
}

fn hotkeys_forced_off(m: &Manager) -> bool {
    let s = m.status_json();
    matches!(
        s["off_reason"].as_str(),
        Some("--no-hotkeys" | "KIM_NO_HOTKEYS" | "safe_mode")
    )
}

fn group_usage() -> NoeulError {
    NoeulError::new(
        Code::Usage,
        "a group name is 1–32 letters, digits, spaces, `-`, `_` or `.`, starting with a letter or digit",
    )
    .hint("run `noeul pin group list` to see the groups")
}

/// What one `pin.set` changes (every value checked before anything changes).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct SetChange {
    at: Option<(i32, i32)>,
    zoom: Option<u32>,
    opacity: Option<u32>,
    /// Absolute: degrees clockwise.
    rotation: Option<u32>,
    mirrored: Option<bool>,
    /// Relative: `true` clockwise.
    rotate: Option<bool>,
    /// Relative: `true` horizontally.
    flip: Option<bool>,
    thumbnail: Option<bool>,
    click_through: Option<bool>,
    group: Option<String>,
}

impl SetChange {
    fn parse(p: &Value) -> Result<SetChange> {
        let bad = |what: &str| NoeulError::new(Code::Usage, what.to_string());
        let num = |k: &str, lo: u64, hi: u64, what: &str| -> Result<Option<u32>> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v
                    .as_u64()
                    .filter(|n| (lo..=hi).contains(n))
                    .and_then(|n| u32::try_from(n).ok())
                    .map(Some)
                    .ok_or_else(|| bad(what)),
            }
        };
        let flag = |k: &str| -> Result<Option<bool>> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::Bool(b)) => Ok(Some(*b)),
                Some(_) => Err(bad(&format!("`{k}` must be true or false"))),
            }
        };
        let word = |k: &str, yes: &str, no: &str| -> Result<Option<bool>> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) if v == yes => Ok(Some(true)),
                Some(v) if v == no => Ok(Some(false)),
                Some(_) => Err(bad(&format!("`{k}` must be \"{yes}\" or \"{no}\""))),
            }
        };
        let rotation = num("rotation", 0, 270, "`rotation` must be 0, 90, 180 or 270")?;
        if rotation.is_some_and(|r| r % 90 != 0) {
            return Err(bad("`rotation` must be 0, 90, 180 or 270"));
        }
        let at = match p.get("at") {
            None | Some(Value::Null) => None,
            Some(v) => Some(point(v).ok_or_else(|| bad("`at` must be {x, y} in device pixels"))?),
        };
        let group = match p.get("move_to_group") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_str()
                    .and_then(crate::pins::group_name)
                    .ok_or_else(group_usage)?,
            ),
        };
        let c = SetChange {
            at,
            zoom: num(
                "zoom",
                u64::from(zoom::MIN),
                u64::from(zoom::MAX),
                "`zoom` must be a percentage from 10 to 800",
            )?,
            opacity: num(
                "opacity",
                u64::from(transform::OPACITY_MIN),
                u64::from(transform::OPACITY_FULL),
                "`opacity` must be a percentage from 10 to 100",
            )?,
            rotation,
            mirrored: flag("mirrored")?,
            rotate: word("rotate", "cw", "ccw")?,
            flip: word("flip", "horizontal", "vertical")?,
            thumbnail: flag("thumbnail")?,
            click_through: flag("click_through")?,
            group,
        };
        if c == SetChange::default() {
            return Err(bad(
                "nothing to change: give at, zoom, opacity, rotation, mirrored, rotate, flip, thumbnail, click_through or move_to_group",
            ));
        }
        Ok(c)
    }
}

/// Reads `{"x", "y"}` (device pixels).
fn point(v: &Value) -> Option<(i32, i32)> {
    let o = v.as_object()?;
    if o.len() != 2 {
        return None;
    }
    let c = |k: &str| {
        o.get(k)?
            .as_i64()
            .filter(|n| n.unsigned_abs() <= 1_000_000)
            .and_then(|n| i32::try_from(n).ok())
    };
    Some((c("x")?, c("y")?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capture::{CaptureError, Desktop, Monitor, Synthetic};
    use crate::overlay::{Approx, Button, Mods, color};
    use crate::pins::fake::FakeWindows;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What the fake overlay windows saw, shared with the test.
    #[derive(Debug, Default)]
    pub struct OverlayLog {
        pub opened: Vec<Rect>,
        pub presents: usize,
        pub dirty_rects: usize,
        pub shown: Vec<bool>,
        pub cursor: Option<Cursor>,
        pub alive: usize,
        pub destroyed: usize,
        /// The back buffer of the last window, composed exactly as the Win32 window does.
        pub buffer: Vec<u8>,
        pub scene: Option<Scene>,
        /// Where pointer keys asked the real pointer to go (Step 52).
        pub warps: Vec<(i32, i32)>,
        /// The input method switched on (at a caret) and off (Step 56).
        pub ime: Vec<Option<Rect>>,
        /// Whether each window was layered (the pin editor, Step 56).
        pub layered: Vec<bool>,
    }

    #[derive(Debug, Default, Clone)]
    pub struct FakeOverlays {
        pub log: Rc<RefCell<OverlayLog>>,
        pub fail_open: bool,
        pub fail_show: bool,
    }

    struct FakeOverlay {
        log: Rc<RefCell<OverlayLog>>,
        picture: Rect,
        fail_show: bool,
        destroyed: bool,
    }

    impl Measure for FakeOverlay {
        fn text(&self, font: overlay::Font, dpi: u32, text: &str) -> (u32, u32) {
            Approx.text(font, dpi, text)
        }
    }

    impl Typesetter for FakeOverlay {
        fn typeset(&self, text: &str, size: u32) -> annotation::Glyphs {
            annotation::BlockType.typeset(text, size)
        }
    }

    impl OverlayWindow for FakeOverlay {
        fn present(
            &mut self,
            frozen: &Frame,
            layer: Option<&Frame>,
            scene: &Scene,
            dirty: &[Rect],
        ) {
            let mut l = self.log.borrow_mut();
            if l.buffer.len() != frozen.pixels().len() {
                l.buffer = vec![0; frozen.pixels().len()];
            }
            for r in dirty {
                overlay::compose(frozen, layer, scene, *r, &mut l.buffer);
            }
            l.presents += 1;
            l.dirty_rects += dirty.len();
            l.scene = Some(scene.clone());
        }
        fn show(&mut self, activate: bool) -> bool {
            if self.fail_show {
                return false;
            }
            self.log.borrow_mut().shown.push(activate);
            true
        }
        fn set_cursor(&mut self, cursor: Cursor) {
            self.log.borrow_mut().cursor = Some(cursor);
        }
        fn buffer_bytes(&self) -> u64 {
            self.picture.bgra_bytes()
        }
        fn warp_pointer(&mut self, at: (i32, i32)) {
            self.log.borrow_mut().warps.push(at);
        }
        fn set_ime(&mut self, caret: Option<Rect>) {
            self.log.borrow_mut().ime.push(caret);
        }
        fn destroy(&mut self) {
            if !self.destroyed {
                self.destroyed = true;
                let mut l = self.log.borrow_mut();
                l.alive -= 1;
                l.destroyed += 1;
                l.buffer = Vec::new();
            }
        }
    }

    impl Drop for FakeOverlay {
        fn drop(&mut self) {
            self.destroy();
        }
    }

    /// A desktop of one 100×100 monitor that captures solid frames.
    pub struct FakeCapture {
        pub state: DesktopState,
    }

    impl CaptureBackend for FakeCapture {
        fn name(&self) -> &'static str {
            "gdi" // stands in for the real backend (the schema knows only real names)
        }
        fn desktop(&self) -> Desktop {
            Desktop {
                state: self.state,
                virtual_rect: Some(Rect::new(0, 0, 100, 100)),
                monitors: vec![Monitor::new(Rect::new(0, 0, 100, 100), true)],
            }
        }
        fn capture(
            &self,
            ledger: &Arc<Ledger>,
            r: Rect,
        ) -> std::result::Result<Frame, CaptureError> {
            check_region(&self.desktop(), r)?;
            Ok(Frame::solid(ledger, r, [10, 20, 30, 255])?)
        }
    }

    #[derive(Default)]
    pub struct FakeShell {
        pub windows: FakeWindows,
        pub clipboard: Option<(u32, u32)>,
        /// Frames of the GIF next to the clipboard image (0 or 1: none; Step 54).
        pub clipboard_frames: u32,
        /// The tray icon is shown (a way back from click-through, Step 54).
        pub tray: bool,
        pub clipboard_reads: usize,
        pub notices: Vec<String>,
        pub confirm: bool,
        pub overlays: FakeOverlays,
        pub pointer_at: Option<(i32, i32)>,
        /// `Some(false)` stands for the debug-build test hook.
        pub interactive: Option<bool>,
        /// What Copy put on the (fake) clipboard: `(png, dib)` per write.
        pub copies: Vec<(Vec<u8>, Vec<u8>)>,
        /// Every clipboard write attempt, also the refused ones.
        pub clipboard_writes: usize,
        /// The fake clipboard is held by another program (or disabled).
        pub clipboard_busy: bool,
        /// The Pictures folder (quick save's default parent).
        pub pictures: Option<PathBuf>,
        /// The user's pointer settings (Windows defaults when `None`).
        pub sys: Option<SystemValues>,
        /// The windows the fake desktop lists, topmost first (Step 52).
        pub facts: Vec<crate::detect::WindowFacts>,
        /// How many window lists were taken.
        pub snapshots: usize,
        /// The UI Automation requests sent.
        pub element_requests: Vec<ElementRequest>,
        /// How many times element lookups were stopped (an overlay closed).
        pub detection_ended: usize,
    }

    impl Shell for FakeShell {
        fn pin_windows(&mut self) -> &mut dyn PinWindows {
            &mut self.windows
        }
        fn clipboard_image(&mut self, ledger: &Arc<Ledger>) -> Result<Pasted> {
            self.clipboard_reads += 1;
            match self.clipboard {
                Some((w, h)) => Ok(Pasted {
                    frame: Frame::solid(ledger, Rect::new(0, 0, w, h), [1, 1, 1, 255]).unwrap(),
                    frames: self.clipboard_frames.max(1),
                }),
                None => Err(NoeulError::new(Code::NothingToPin, "no image")),
            }
        }
        fn clipboard_write(&mut self, png: &[u8], dib: &[u8]) -> Result<()> {
            self.clipboard_writes += 1;
            if self.clipboard_busy {
                return Err(NoeulError::new(Code::ClipboardUnavailable, "held (test)"));
            }
            self.copies.push((png.to_vec(), dib.to_vec()));
            Ok(())
        }
        fn pictures_dir(&self) -> Option<PathBuf> {
            self.pictures.clone()
        }
        fn local_time(&self) -> [u16; 6] {
            [2026, 11, 4, 9, 5, 7]
        }
        fn pointer(&self) -> (i32, i32) {
            self.pointer_at.unwrap_or((90, 95))
        }
        fn notice(&mut self, text: &str) {
            self.notices.push(text.to_string());
        }
        fn tray_shown(&self) -> bool {
            self.tray
        }
        fn confirm_quit(&mut self, _: usize) -> bool {
            self.confirm
        }
        fn open_overlay(
            &mut self,
            picture: Rect,
            layered: bool,
        ) -> std::result::Result<Box<dyn OverlayWindow>, String> {
            if self.overlays.fail_open {
                return Err("window creation failed (test)".into());
            }
            let mut l = self.overlays.log.borrow_mut();
            l.opened.push(picture);
            l.layered.push(layered);
            l.alive += 1;
            Ok(Box::new(FakeOverlay {
                log: Rc::clone(&self.overlays.log),
                picture,
                fail_show: self.overlays.fail_show,
                destroyed: false,
            }))
        }
        fn system_values(&self) -> SystemValues {
            self.sys.unwrap_or_default()
        }
        fn interactive(&self) -> bool {
            self.interactive.unwrap_or(true)
        }
        fn window_snapshot(&mut self, picture: Rect) -> Snapshot {
            self.snapshots += 1;
            Snapshot::from_facts(self.facts.iter().copied(), picture)
        }
        fn request_element(&mut self, req: ElementRequest) {
            self.element_requests.push(req);
        }
        fn end_detection(&mut self) {
            self.detection_ended += 1;
        }
    }

    pub fn app(dir: &std::path::Path, state: DesktopState) -> App<FakeShell> {
        let paths = Paths::under(dir);
        let hotkeys = Manager::start(&paths, Some("--no-hotkeys"), || unreachable!());
        App::new(
            FakeShell::default(),
            Box::new(FakeCapture { state }),
            hotkeys,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        )
    }

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noeul-app-{tag}-{}", std::process::id()))
    }

    /// Two monitors: 640×360 at 100 % left of the origin, and the primary 960×540 at 150 %.
    pub fn layout() -> Vec<Monitor> {
        vec![
            Monitor::new(Rect::new(-640, 0, 640, 360), false),
            Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(144),
        ]
    }

    /// An instance on the synthetic two-monitor desktop with the default keys (no registrar).
    fn overlay_app(tag: &str) -> (App<FakeShell>, Arc<Synthetic>, PathBuf) {
        let dir = tmp(tag);
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::under(&dir);
        let syn = Arc::new(Synthetic::new(layout()));
        let hotkeys = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        let shell = FakeShell {
            pointer_at: Some((500, 300)),
            ..FakeShell::default()
        };
        let a = App::new(
            shell,
            Box::new(Arc::clone(&syn)),
            hotkeys,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        );
        (a, syn, dir)
    }

    fn ev(a: &mut App<FakeShell>, i: OverlayInput) {
        a.overlay_event(OverlayEvent::Input(i));
    }

    fn key(a: &mut App<FakeShell>, name: &str, ctrl: bool, shift: bool) {
        let c = crate::hotkeys::chord_of(name, ctrl, false, shift).unwrap();
        a.overlay_event(OverlayEvent::Key(c, None, false));
    }

    fn drag(a: &mut App<FakeShell>, from: (i32, i32), to: (i32, i32)) {
        ev(
            a,
            OverlayInput::Press {
                button: Button::Left,
                x: from.0,
                y: from.1,
                mods: Mods::NONE,
                time_ms: 0,
            },
        );
        for (x, y) in [((from.0 + to.0) / 2, (from.1 + to.1) / 2), to] {
            ev(
                a,
                OverlayInput::Move {
                    x,
                    y,
                    mods: Mods::NONE,
                },
            );
        }
        ev(
            a,
            OverlayInput::Release {
                button: Button::Left,
                x: to.0,
                y: to.1,
            },
        );
    }

    fn files_under(dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    out.extend(files_under(&e.path()));
                } else {
                    out.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        out
    }

    #[test]
    fn f1_opens_one_overlay_over_every_monitor_and_esc_cancels_cleanly() {
        let (mut a, syn, _dir) = overlay_app("ov-open");
        a.action("capture.region", None);
        let picture = Rect::new(-640, 0, 1600, 540);
        {
            let l = a.shell.overlays.log.borrow();
            assert_eq!(l.opened, [picture], "one overlay over the virtual desktop");
            assert_eq!(l.shown, [true], "on top, with keyboard focus");
            assert_eq!(l.cursor, Some(Cursor::Cross));
            // The first frame is the whole frozen picture, dimmed (nothing selected yet).
            let i = (10 * 1600 + 10) * 4;
            assert_eq!(
                &l.buffer[i..i + 4],
                &overlay::dim_pixel(Synthetic::pixel(-630, 10))
            );
        }
        assert_eq!(syn.captures(), 1);
        assert_eq!(a.ledger.live(), (1, picture.bgra_bytes()));
        let st = a.status();
        assert_eq!(st["captures"]["open"], true);
        assert_eq!(st["captures"]["overlay"]["phase"], "hover");
        assert_eq!(st["captures"]["overlay"]["monitors"], 2);
        assert_eq!(st["captures"]["first_frame"]["count"], 1);
        assert_eq!(st["last_action"]["result"], "opened");
        // Rapid repeated presses: ignored, no second overlay, no second capture.
        for _ in 0..5 {
            a.action("capture.region", None);
        }
        assert_eq!(a.shell.overlays.log.borrow().opened.len(), 1);
        assert_eq!(syn.captures(), 1, "the screen is not captured again");
        assert_eq!(a.status()["captures"]["ignored"], 5);
        assert_eq!(a.status()["last_action"]["result"], "ignored");
        // The selection tracks the pointer across the monitor seam.
        drag(&mut a, (-100, 50), (199, 249));
        let st = a.status();
        assert_eq!(st["captures"]["overlay"]["phase"], "selected");
        assert_eq!(
            st["captures"]["overlay"]["selection"],
            json!({"x": -100, "y": 50, "width": 300, "height": 200})
        );
        {
            let l = a.shell.overlays.log.borrow();
            let at = |x: i32, y: i32| {
                let i = ((y as usize) * 1600 + (x + 640) as usize) * 4;
                [
                    l.buffer[i],
                    l.buffer[i + 1],
                    l.buffer[i + 2],
                    l.buffer[i + 3],
                ]
            };
            // (Below the first-run help, which stays at the top of the small 150 % monitor now that
            // the toolbar takes two rows at its bottom, Step 57.)
            assert_eq!(at(0, 200), Synthetic::pixel(0, 200), "plain inside");
            assert_eq!(at(-101, 150), color::ACCENT, "border outside the left edge");
            assert_eq!(
                at(-600, 300),
                overlay::dim_pixel(Synthetic::pixel(-600, 300))
            );
            assert!(l.dirty_rects > 0 && l.presents >= 4);
        }
        // Escape cancels: the window and the frozen picture are gone, nothing was pinned.
        key(&mut a, "Escape", false, false);
        assert!(!a.overlay_open());
        let l = a.shell.overlays.log.borrow();
        assert_eq!((l.alive, l.destroyed), (0, 1));
        drop(l);
        assert_eq!(a.ledger.live(), (0, 0), "nothing is kept");
        assert!(a.pins.is_empty());
        assert_eq!(a.status()["captures"]["cancelled"], 1);
        assert_eq!(a.shell.clipboard_reads, 0, "the clipboard is never touched");
    }

    #[test]
    fn enter_pins_exactly_the_selection_and_nothing_is_copied_or_written() {
        let (mut a, syn, dir) = overlay_app("ov-pin");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (499, 399));
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open(), "the overlay closed");
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(100, 100, 400, 300), true)],
            "same place and size, with focus"
        );
        assert_eq!(
            a.ledger.live(),
            (1, 400 * 300 * 4),
            "only the pin's pixels are kept"
        );
        assert_eq!(syn.captures(), 1, "the pin shows the frozen picture");
        assert_eq!(a.shell.clipboard_reads, 0);
        let st = a.status();
        assert_eq!(st["captures"]["pinned"], 1);
        assert_eq!(st["last_action"]["action"], "capture.pin");
        // Nothing reached the disk except the "help seen" flag.
        assert_eq!(files_under(&dir), ["ui-state.json"]);
        // R brings the last rectangle back in the next capture.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 100, "y": 100, "width": 400, "height": 300})
        );
        assert!(
            a.shell
                .overlays
                .log
                .borrow()
                .scene
                .as_ref()
                .unwrap()
                .help
                .is_none(),
            "the first-run help is gone after the first capture"
        );
        a.shutdown();
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(a.shell.overlays.log.borrow().alive, 0);
    }

    #[test]
    fn keyboard_only_capture_with_ctrl_a_arrows_and_enter() {
        let (mut a, _syn, _dir) = overlay_app("ov-keys");
        a.action("capture.region", None);
        key(&mut a, "Enter", false, false);
        assert!(a.overlay_open(), "Enter without a selection pins nothing");
        key(&mut a, "A", true, false); // the monitor under the pointer (500,300): the primary
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 0, "y": 0, "width": 960, "height": 540})
        );
        key(&mut a, "Right", false, true); // shrink from the left edge
        key(&mut a, "Up", false, true); // shrink from the bottom edge
        key(&mut a, "Left", false, false); // move left (stays on the picture)
        assert_eq!(
            a.status()["captures"]["overlay"]["selection"],
            json!({"x": 0, "y": 0, "width": 959, "height": 539})
        );
        key(&mut a, "Left", true, false); // grow left across the seam
        key(&mut a, "Enter", false, false);
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(created, [(PinId(1), Rect::new(-1, 0, 960, 539), true)]);
    }

    #[test]
    fn a_failed_pin_keeps_the_overlay_and_its_selection() {
        let (mut a, _syn, _dir) = overlay_app("ov-fail");
        a.action("capture.region", None);
        drag(&mut a, (10, 10), (109, 59));
        *a.shell.windows.fail.borrow_mut() = true;
        key(&mut a, "Enter", false, false);
        assert!(a.overlay_open(), "the overlay stays (NOEUL-FR-025)");
        let st = a.status();
        assert_eq!(st["captures"]["overlay"]["phase"], "selected");
        assert_eq!(st["captures"]["overlay"]["selection"]["width"], 100);
        assert_eq!(st["last_action"]["result"], "failed");
        let banner = a.shell.overlays.log.borrow().scene.clone().unwrap().help;
        assert!(banner.unwrap().lines[0].starts_with("The pin could not be created"));
        assert_eq!(
            a.ledger.live().0,
            1,
            "only the frozen picture: the failed pin's copy was freed"
        );
        *a.shell.windows.fail.borrow_mut() = false;
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.pins.len(), 1);
    }

    #[test]
    fn ipc_and_tray_open_the_overlay_and_a_dry_run_captures_nothing() {
        let (mut a, syn, _dir) = overlay_app("ov-ipc");
        let dry = a
            .request("capture.region", &json!({"dry_run": true}))
            .unwrap();
        assert_eq!(
            (dry["opened"].as_bool(), dry["dry_run"].as_bool()),
            (Some(false), Some(true))
        );
        assert_eq!(dry["picture"]["width"], 1600);
        assert_eq!(syn.captures(), 0, "a dry run captures nothing");
        assert!(a.shell.overlays.log.borrow().opened.is_empty());
        let r = a.request("capture.region", &json!({})).unwrap();
        assert_eq!(r["opened"], true);
        assert!(r["first_frame_us"].as_u64().is_some());
        let again = a.request("capture.region", &json!({})).unwrap();
        assert_eq!(
            (again["opened"].as_bool(), again["reason"].as_str()),
            (Some(false), Some("already_open"))
        );
        let c = a
            .request("capture.cancel", &json!({"dry_run": true}))
            .unwrap();
        assert_eq!(c["cancelled"], false);
        assert!(a.overlay_open(), "a dry run cancels nothing");
        let c = a.request("capture.cancel", &json!({})).unwrap();
        assert_eq!(c["cancelled"], true);
        assert!(!a.overlay_open());
        let c = a.request("capture.cancel", &json!({})).unwrap();
        assert_eq!(
            (c["cancelled"].as_bool(), c["session"].clone()),
            (Some(false), Value::Null)
        );
        // The tray's left click and its menu item start a capture too.
        a.tray_click(Click::Left);
        assert!(a.overlay_open());
        assert!(a.tray_menu().contains(&MenuItem::CaptureRegion));
        a.request("capture.cancel", &json!({})).unwrap();
        a.tray_command(MenuItem::CaptureRegion);
        assert!(a.overlay_open());
        // Focus moving to another program cancels.
        ev(&mut a, OverlayInput::FocusLost);
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(syn.captures(), 3);
        assert_eq!(a.latency().to_json()["count"], 3);
    }

    #[test]
    fn locked_desktops_and_failing_windows_open_no_overlay() {
        let (mut a, syn, _dir) = overlay_app("ov-locked");
        syn.set_state(DesktopState::Locked);
        a.action("capture.region", None);
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "desktop_unavailable");
        let e = a.request("capture.region", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::DesktopUnavailable);
        assert_eq!(syn.captures(), 0, "a locked desktop is never captured");
        assert!(
            a.shell.notices.is_empty(),
            "a locked desktop is not an error to show"
        );
        syn.set_state(DesktopState::Available);
        a.shell.overlays.fail_open = true;
        let e = a.request("capture.region", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::Internal);
        assert_eq!(a.ledger.live(), (0, 0), "the frozen picture was discarded");
        a.shell.overlays.fail_open = false;
        a.shell.overlays.fail_show = true;
        a.action("capture.region", None);
        assert!(!a.overlay_open());
        assert_eq!(a.ledger.live(), (0, 0));
        assert_eq!(a.shell.notices, ["Noeul could not capture the screen."]);
        assert_eq!(a.status()["captures"]["open"], false);
    }

    #[test]
    fn the_test_hook_never_takes_focus_and_the_help_follows_the_setting() {
        let (mut a, _syn, dir) = overlay_app("ov-hook");
        a.shell.interactive = Some(false);
        a.action("capture.region", None);
        assert_eq!(
            a.shell.overlays.log.borrow().shown,
            [false],
            "never activated"
        );
        let help = a
            .shell
            .overlays
            .log
            .borrow()
            .scene
            .clone()
            .unwrap()
            .help
            .unwrap();
        assert_eq!(
            help.lines,
            [
                "Drag to select a region, or click a highlighted window. Esc or right-click cancels.",
                "Double-click inside pins it \u{b7} Ctrl+C copies \u{b7} Ctrl+S saves \u{b7} Ctrl+Shift+S quick-saves",
                "Arrows move \u{b7} Ctrl+arrows grow \u{b7} Shift+arrows shrink",
                "Ctrl+A whole monitor \u{b7} R last area \u{b7} Enter pins the selection",
                "W A S D move the pointer \u{b7} Space marks a corner \u{b7} E windows, elements, off \u{b7} M magnifier \u{b7} Comma/Period earlier areas",
            ]
        );
        assert_eq!(help.dpi, 144, "drawn at the primary monitor's 150 %");
        drag(&mut a, (10, 10), (19, 19));
        key(&mut a, "Enter", false, false);
        let created = a.shell.windows.log.borrow().created.clone();
        assert!(!created[0].2, "a pin from the hook never takes focus");
        // `always` shows the help again; `never` never.
        std::fs::create_dir_all(dir.join("Roaming").join("mouv").join("noeul")).unwrap();
        for (value, shown) in [("always", true), ("never", false), ("first-run", false)] {
            std::fs::write(
                a.paths.config_file(),
                format!("[capture]\nkeyboard_help = \"{value}\"\n"),
            )
            .unwrap();
            a.settings = settings::load(&a.paths.config_file());
            a.action("capture.region", None);
            let has = a
                .shell
                .overlays
                .log
                .borrow()
                .scene
                .clone()
                .unwrap()
                .help
                .is_some();
            assert_eq!(has, shown, "{value}");
            a.request("capture.cancel", &json!({})).unwrap();
        }
    }

    #[test]
    fn the_help_names_changed_keys_and_disappears_without_them() {
        let dir = tmp("ov-help");
        let paths = Paths::under(&dir);
        let off = Manager::start(&paths, Some("--no-hotkeys"), || unreachable!());
        assert_eq!(
            overlay_help(&off, Gestures::default()),
            [
                "Drag to select a region, or click a highlighted window. Esc or right-click cancels.",
                "Double-click inside pins it"
            ],
            "without shortcuts only the mouse and Escape remain"
        );
        let copy = Gestures {
            double_click: GestureAction::Copy,
            ..Gestures::default()
        };
        assert_eq!(
            overlay_help(&off, copy)[1],
            "Double-click inside copies it",
            "the help names the gesture setting"
        );
        let none = Gestures {
            double_click: GestureAction::None,
            ..Gestures::default()
        };
        assert_eq!(overlay_help(&off, none).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.hotkeys_file(),
            serde_json::to_vec(
                &json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
                "overrides": [
                    {"action": "capture.pin", "bindings": ["Ctrl+Enter"]},
                    {"action": "selection.move-up", "bindings": ["code:KeyW"]},
                    {"action": "selection.last", "disabled": true}
                ]}),
            )
            .unwrap(),
        )
        .unwrap();
        let m = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        let lines = overlay_help(&m, Gestures::default());
        assert_eq!(
            lines[2], "W/Down/Left/Right move \u{b7} Ctrl+arrows grow \u{b7} Shift+arrows shrink",
            "a physical key reads as its letter"
        );
        assert_eq!(
            lines[3],
            "Ctrl+A whole monitor \u{b7} Ctrl+Enter pins the selection"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real trigger path on an obscure chord: `kim-hotkeys` registers `capture.region` with
    /// `RegisterHotKey` on `Ctrl+Alt+Shift+F21`, and that registration's id (what a press
    /// delivers to the instance) opens the overlay. No key is pressed or synthesized, and F1 is
    /// never registered.
    #[cfg(windows)]
    #[test]
    fn the_registered_capture_chord_opens_the_overlay() {
        use kim_hotkeys::register::{HotkeyId, Registrar, Status};
        let dir = tmp("ov-hotkey");
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths::under(&dir);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.hotkeys_file(),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.region", "bindings": ["Ctrl+Alt+Shift+F21"]},
                    {"action": "paste.float", "disabled": true}]}))
            .unwrap(),
        )
        .unwrap();
        let mgr = Manager::start(&paths, None, Registrar::start);
        let regs = mgr.registrations().to_vec();
        assert_eq!(regs.len(), 1, "{regs:?}");
        assert_eq!(
            (
                regs[0].action.as_str(),
                regs[0].keys.as_str(),
                regs[0].status
            ),
            ("capture.region", "Ctrl+Alt+Shift+F21", "registered"),
            "{:?}",
            regs[0].message
        );
        let id = (1..=16)
            .map(HotkeyId)
            .find(|id| mgr.action_for(*id).as_deref() == Some("capture.region"))
            .expect("the registration's id");
        let syn = Arc::new(Synthetic::new(layout()));
        let shell = FakeShell {
            pointer_at: Some((10, 10)),
            ..FakeShell::default()
        };
        let mut a = App::new(
            shell,
            Box::new(Arc::clone(&syn)),
            mgr,
            settings::load(&paths.config_file()),
            paths,
            Identity {
                pid: 42,
                endpoint: r"\\.\pipe\noeul-test".into(),
                session: 1,
            },
        );
        a.hotkey_pressed(id);
        assert!(a.overlay_open(), "the press opened the overlay");
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "capture.region", "result": "opened"})
        );
        assert_eq!(a.latency().to_json()["count"], 1);
        a.hotkey_pressed(id);
        assert_eq!(syn.captures(), 1, "a second press while open does nothing");
        assert_eq!(a.status()["captures"]["ignored"], 1);
        a.shutdown();
        let probe = Registrar::start().unwrap();
        let (pid, st) = probe
            .register(&Chord::parse("Ctrl+Alt+Shift+F21").unwrap())
            .unwrap();
        assert_eq!(st, Status::Registered, "shutdown released the chord");
        probe.unregister(pid.unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latency_percentiles_use_the_nearest_rank() {
        let mut l = Latency::default();
        assert_eq!(l.to_json()["p95_us"], Value::Null);
        for t in 1..=20u64 {
            l.push(LatencySample {
                total_us: t * 1000,
                capture_us: t * 600,
                render_us: t * 400,
                width: 1,
                height: 1,
            });
        }
        assert_eq!(l.percentile(50), Some(10_000));
        assert_eq!(l.percentile(95), Some(19_000));
        for _ in 0..200 {
            l.push(LatencySample {
                total_us: 5,
                capture_us: 1,
                render_us: 4,
                width: 1,
                height: 1,
            });
        }
        assert_eq!(l.percentile(95), Some(5), "only the last 100 count");
        assert_eq!(l.to_json()["count"], 220);
    }

    #[test]
    fn cli_capture_pins_and_every_picture_is_released() {
        let mut a = app(&tmp("capture"), DesktopState::Available);
        let region = json!({"x": 10, "y": 10, "width": 20, "height": 5});
        let dry = a
            .request(
                "capture",
                &json!({"region": region, "pin": true, "dry_run": true}),
            )
            .unwrap();
        assert_eq!(dry["dry_run"], true);
        assert!(
            a.shell.windows.log.borrow().created.is_empty(),
            "a dry run creates nothing"
        );
        assert_eq!(a.ledger.live(), (0, 0), "and captures nothing");

        let r = a
            .request("capture", &json!({"region": region, "pin": true}))
            .unwrap();
        assert_eq!(r["pin"]["id"], "pin-1");
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(10, 10, 20, 5), false)],
            "no focus stealing"
        );
        assert_eq!(a.ledger.live(), (1, 400), "only the pin's picture is alive");
        assert_eq!(a.status()["captures"]["pinned"], 1);

        let outside = a.request(
            "capture",
            &json!({"region": {"x": 90, "y": 0, "width": 20, "height": 5}, "pin": true}),
        );
        assert_eq!(outside.unwrap_err().code, Code::Usage);
        assert!(
            a.request("capture", &json!({"region": region})).is_err(),
            "pin is required"
        );

        let listed = a.request("pin.list", &json!({})).unwrap();
        assert_eq!(listed["pins"][0]["rect"]["width"], 20);
        assert_eq!(
            a.request("pin.close", &json!({"id": "pin-9"}))
                .unwrap_err()
                .code,
            Code::PinNotFound
        );
        let closed = a.request("pin.close", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(closed["closed"][0], "pin-1");
        assert_eq!(a.request("pin.reopen", &json!({})).unwrap()["pin"], "pin-1");

        let q = a.request("quit", &json!({}));
        assert_eq!(
            q.unwrap_err().code,
            Code::ConfirmationRequired,
            "open pins need --yes"
        );
        assert!(!a.quitting());
        let q = a
            .request("quit", &json!({"force": true, "dry_run": true}))
            .unwrap();
        assert_eq!(q["closed_pins"], 1);
        assert!(!a.quitting(), "a dry run does not quit");
        a.request("quit", &json!({"force": true})).unwrap();
        assert!(a.quitting());
        assert!(a.shell.windows.log.borrow().alive.is_empty());
        assert_eq!(a.ledger.live(), (0, 0), "quit releases every picture");
    }

    #[test]
    fn locked_desktops_capture_nothing() {
        for s in [
            DesktopState::Locked,
            DesktopState::NonInteractive,
            DesktopState::NoDisplay,
        ] {
            let mut a = app(&tmp("locked"), s);
            let e = a
                .request(
                    "capture",
                    &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
                )
                .unwrap_err();
            assert_eq!(e.code, Code::DesktopUnavailable, "{s:?}");
            a.shell.clipboard = Some((2, 2));
            a.action("paste.float", None);
            assert!(
                a.shell.windows.log.borrow().created.is_empty(),
                "{s:?}: no pin"
            );
            assert_eq!(a.ledger.live(), (0, 0));
        }
        let mut a = app(&tmp("unsupported"), DesktopState::Unsupported);
        let e = a
            .request(
                "capture",
                &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
            )
            .unwrap_err();
        assert_eq!(e.code, Code::UnsupportedPlatform);
    }

    #[test]
    fn paste_float_pins_at_the_pointer_or_says_nothing_to_pin() {
        let mut a = app(&tmp("paste"), DesktopState::Available);
        a.action("paste.float", None);
        assert_eq!(a.shell.notices.len(), 1);
        assert!(a.shell.notices[0].starts_with("Nothing to pin"));
        assert!(a.pins.is_empty());
        a.shell.clipboard = Some((30, 20));
        a.action("paste.float", None);
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created,
            [(PinId(1), Rect::new(70, 80, 30, 20), true)],
            "kept on the desktop, focused"
        );
        // Pin keys: Escape closes the focused pin, arrows move it.
        let key = |n: &str| crate::hotkeys::chord_of(n, false, false, false).unwrap();
        let mut b = app(&tmp("paste-keys"), DesktopState::Available);
        let paths = Paths::under(tmp("paste-keys"));
        b.hotkeys = Manager::start(&paths, None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        b.shell.clipboard = Some((2, 2));
        b.action("paste.float", None);
        b.pin_event(PinEvent::Key(PinId(1), key("Left"), true));
        assert_eq!(b.pins.rect(PinId(1)).unwrap().x, 89);
        b.pin_event(PinEvent::Moved(PinId(1), 5, 6));
        assert_eq!(b.pins.rect(PinId(1)).unwrap().y, 6);
        b.pin_event(PinEvent::Key(PinId(1), key("Escape"), true));
        assert_eq!(
            b.pins.len(),
            1,
            "a held Escape does not close a new pin by repeat"
        );
        b.pin_event(PinEvent::Key(PinId(1), key("Escape"), false));
        assert!(b.pins.is_empty());
        assert_eq!(b.pins.reopenable(), Some(PinId(1)));
        let s = b.status();
        assert_eq!(
            s["hotkeys"]["registrations"][0]["status"], "failed",
            "a registrar failure is reported"
        );
    }

    #[test]
    fn tray_items_follow_the_state_and_quit_asks_once() {
        let mut a = app(&tmp("tray"), DesktopState::Available);
        let items = a.tray_menu();
        assert!(items.contains(&MenuItem::PinClipboard));
        assert!(!items.contains(&MenuItem::ReopenClosedPin));
        assert!(items.contains(&MenuItem::Quit));
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, MenuItem::Shortcuts { .. })),
            "--no-hotkeys cannot be switched from the tray"
        );
        a.shell.clipboard = Some((1, 1));
        a.tray_click(Click::Middle);
        assert_eq!(a.pins.len(), 1, "middle click pins the clipboard");
        a.action("pin.close", Some(PinId(1)));
        assert!(a.tray_menu().contains(&MenuItem::ReopenClosedPin));
        a.tray_command(MenuItem::ReopenClosedPin);
        assert_eq!(a.pins.len(), 1);
        a.tray_command(MenuItem::Quit);
        assert!(!a.quitting(), "declined");
        a.shell.confirm = true;
        a.tray_command(MenuItem::Quit);
        assert!(a.quitting());
        assert_eq!(a.ledger.live(), (0, 0));
    }

    #[test]
    fn every_ipc_answer_matches_the_schema() {
        let mut a = app(&tmp("schema"), DesktopState::Available);
        a.shell.clipboard = Some((20, 10));
        let check = |v: Value| {
            let errors = crate::schema_errors("noeul-ipc-v1", &v);
            assert!(errors.is_empty(), "{errors:?}\n{v:#}");
        };
        let region = json!({"x": 1, "y": 2, "width": 3, "height": 4});
        for (method, params) in [
            ("status", json!({})),
            ("activate", json!({"reason": "start"})),
            (
                "capture",
                json!({"region": region, "pin": true, "dry_run": true}),
            ),
            ("capture", json!({"region": region, "pin": true})),
            ("pin.list", json!({})),
            (
                "pin.create",
                json!({"source": "region", "region": region, "dry_run": true}),
            ),
            ("pin.create", json!({"source": "region", "region": region})),
            (
                "pin.create",
                json!({"source": "clipboard", "dry_run": true}),
            ),
            (
                "pin.create",
                json!({"source": "clipboard", "at": {"x": 5, "y": 5}}),
            ),
            ("pin.hide", json!({"id": "pin-2", "dry_run": true})),
            ("pin.hide", json!({"all": true})),
            ("pin.list", json!({})),
            (
                "pin.show",
                json!({"all": true, "on_screen": true, "dry_run": true}),
            ),
            ("pin.show", json!({"id": "pin-3", "on_screen": true})),
            ("status", json!({})),
            ("pin.close", json!({"all": true, "dry_run": true})),
            ("pin.close", json!({"id": "pin-1"})),
            ("pin.reopen", json!({"dry_run": true})),
            ("pin.reopen", json!({})),
            ("hotkeys.reload", json!({})),
            ("settings.reload", json!({})),
            ("status", json!({})),
            ("capture.region", json!({"dry_run": true})),
            ("capture.region", json!({})),
            ("status", json!({})),
            ("capture.region", json!({})),
            ("capture.cancel", json!({"dry_run": true})),
            ("capture.cancel", json!({})),
            ("quit", json!({"force": true, "dry_run": true})),
        ] {
            check(ipc::request(7, method, params.clone()));
            let r = a.request(method, &params).unwrap();
            check(ipc::ok(7, r));
        }
        for (method, params) in [
            ("pin.close", json!({"id": "pin-9"})),
            ("pin.hide", json!({})),
            ("pin.show", json!({"id": "pin-77"})),
            ("pin.create", json!({"source": "file"})),
            ("pin.create", json!({"source": "clipboard", "at": {"x": 1}})),
            ("quit", json!({})),
            (
                "capture",
                json!({"region": {"x": 99, "y": 99, "width": 9, "height": 9}, "pin": true}),
            ),
        ] {
            let e = a.request(method, &params).unwrap_err();
            check(ipc::err(8, &e));
            check(ipc::connection_error(&e));
        }
        check(ipc::hello("noeul-cli"));
        check(ipc::welcome(1, 42, &a.started_at()));
        let mut locked = app(&tmp("schema-locked"), DesktopState::Locked);
        let e = locked
            .request("capture", &json!({"region": region, "pin": true}))
            .unwrap_err();
        check(ipc::err(2, &e));
        check(ipc::ok(1, locked.status()));
    }

    #[test]
    fn status_reports_ids_and_geometry_only() {
        let mut a = app(&tmp("status"), DesktopState::Available);
        a.request(
            "capture",
            &json!({"region": {"x": 0, "y": 0, "width": 3, "height": 3}, "pin": true}),
        )
        .unwrap();
        let s = a.status();
        assert_eq!(s["pins"]["open"], 1);
        assert_eq!(s["memory"]["bytes"], 36);
        assert_eq!(s["capture_backend"], "gdi");
        assert_eq!(s["hotkeys"]["off_reason"], "--no-hotkeys");
        let text = s.to_string();
        assert!(!text.contains("10, 20, 30"), "no pixel values");
        let act = a.request("activate", &json!({})).unwrap();
        assert_eq!(act["settings_window"], "planned");
    }

    /// An instance with the default keys (no registrar) on the fake 100×100 desktop.
    fn keyed(tag: &str) -> App<FakeShell> {
        let dir = tmp(tag);
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app(&dir, DesktopState::Available);
        a.hotkeys = Manager::start(&Paths::under(&dir), None, || {
            Err(kim_hotkeys::register::RegisterError::Stopped)
        });
        a
    }

    fn chord(name: &str, ctrl: bool, shift: bool) -> Chord {
        crate::hotkeys::chord_of(name, ctrl, false, shift).unwrap()
    }

    fn press(a: &mut App<FakeShell>, id: PinId, name: &str, shift: bool) {
        a.pin_event(PinEvent::Key(id, chord(name, false, shift), false));
    }

    #[test]
    fn pin_keys_zoom_hide_destroy_and_escape_always_closes() {
        let mut a = keyed("pin-keys");
        a.shell.clipboard = Some((20, 10));
        a.shell.pointer_at = Some((10, 10));
        a.action("paste.float", None);
        let id = PinId(1);
        assert_eq!(a.pins.rect(id), Some(Rect::new(10, 10, 20, 10)));
        // Plus zooms in around the pointer (inside the pin), 0 returns to 100 %.
        a.shell.pointer_at = Some((20, 15));
        press(&mut a, id, "Plus", false);
        let info = a.pins.info(id).unwrap();
        assert_eq!((info.zoom, info.rect), (110, Rect::new(9, 10, 22, 11)));
        press(&mut a, id, "Minus", false);
        press(&mut a, id, "Minus", false);
        assert_eq!(a.pins.info(id).unwrap().zoom, 90);
        press(&mut a, id, "0", false);
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        assert_eq!(a.status()["last_action"]["action"], "pin.zoom-reset");
        // H hides it (with a hint how to get it back), IPC shows it again.
        press(&mut a, id, "H", false);
        assert!(!a.pins.info(id).unwrap().visible);
        assert!(a.shell.notices.last().unwrap().contains("noeul pin show"));
        assert_eq!(a.status()["pins"]["hidden"], 1);
        assert!(a.tray_menu().contains(&MenuItem::ShowAllPins));
        let r = a.request("pin.show", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(r["shown"], json!(["pin-1"]));
        // Shift+Escape destroys: nothing is kept for reopen.
        press(&mut a, id, "Escape", true);
        assert!(a.pins.is_empty());
        assert_eq!(a.pins.reopenable(), None);
        assert_eq!(a.ledger.live(), (0, 0));
        // With every shortcut off, Escape still closes the focused pin (NOEUL-SEC-013).
        let mut off = app(&tmp("pin-keys-off"), DesktopState::Available);
        off.shell.clipboard = Some((5, 5));
        off.action("paste.float", None);
        press(&mut off, PinId(1), "Plus", false);
        assert_eq!(
            off.pins.info(PinId(1)).unwrap().zoom,
            100,
            "no zoom key while off"
        );
        off.pin_event(PinEvent::Key(PinId(1), chord("Escape", false, false), true));
        assert_eq!(off.pins.len(), 1, "a held Escape does not close by repeat");
        press(&mut off, PinId(1), "Escape", false);
        assert!(off.pins.is_empty(), "the reserved Escape closes it");
        assert_eq!(off.pins.reopenable(), Some(PinId(1)));
    }

    #[test]
    fn the_pin_menu_lists_this_builds_actions_with_their_keys() {
        let mut a = keyed("pin-menu");
        a.shell.clipboard = Some((40, 30));
        a.action("paste.float", None);
        let id = PinId(1);
        a.pin_event(PinEvent::Menu(id, Some((50, 60))));
        let menu = a.take_pin_menu().expect("a menu request");
        assert_eq!((menu.pin, menu.at), (id, Some((50, 60))));
        let labels: Vec<(&str, bool)> = menu
            .items
            .iter()
            .map(|i| (i.label.as_str(), i.enabled))
            .collect();
        assert_eq!(
            labels,
            [
                ("Copy\tCtrl+C", true),
                ("Save\u{2026}\tCtrl+S", true),
                ("Quick save\tCtrl+Shift+S", true),
                ("", false),
                ("Annotate\tSpace", true),
                ("", false),
                ("Zoom in\tPlus", true),
                ("Zoom out\tMinus", true),
                ("Zoom 100 %, opaque\t0", false),
                ("Rotate right\t1", true),
                ("Rotate left\t2", true),
                ("Flip horizontally\t3", true),
                ("Flip vertically\t4", true),
                ("More opaque\tCtrl+Plus", false),
                ("More transparent\tCtrl+Minus", true),
                ("", false),
                ("Thumbnail\tT", true),
                // No tray and no `pins.clickable` key: no way back, so it cannot be turned on.
                ("Click-through\tCtrl+Shift+T", false),
                ("Move to a new group (now in default)", true),
                ("", false),
                ("Hide\tH", true),
                ("Close\tEscape", true),
                ("Destroy\tShift+Escape", true),
            ]
        );
        for action in menu.items.iter().filter_map(|i| i.action) {
            assert!(
                crate::hotkeys::IMPLEMENTED.contains(&action),
                "{action} is implemented (NOEUL-FR-114)"
            );
        }
        assert!(a.take_pin_menu().is_none(), "taken once");
        // The choice runs the action on that pin only.
        a.pin_menu_choice(id, "pin.zoom-in");
        assert_eq!(a.pins.info(id).unwrap().zoom, 110);
        a.pin_event(PinEvent::Menu(id, None));
        let items = a.take_pin_menu().unwrap().items;
        assert!(items[8].enabled, "100 % is offered once zoomed");
        a.pin_menu_choice(id, "capture.region");
        assert!(
            !a.overlay_open(),
            "only pin actions are accepted from the menu"
        );
        a.pin_menu_choice(id, "pin.close");
        assert!(a.pins.is_empty());
        // A menu for a pin that is gone is not shown.
        a.pin_event(PinEvent::Menu(id, None));
        assert!(a.take_pin_menu().is_none());
        // Under the test hook a menu is never shown (it would open on the user's screen).
        a.shell.interactive = Some(false);
        a.action("paste.float", None);
        a.pin_event(PinEvent::Menu(PinId(2), None));
        assert!(a.take_pin_menu().is_none());
        assert_eq!(a.status()["last_action"]["action"], "pin.menu");
        a.shell.interactive = None;
        a.pin_menu_choice(id, "pin.hide");
        assert_eq!(a.status()["last_action"]["result"], "not_found");
    }

    #[test]
    fn the_wheel_zooms_around_the_pointer_and_middle_click_resets() {
        let mut a = keyed("pin-wheel");
        a.shell.clipboard = Some((40, 20));
        a.shell.pointer_at = Some((0, 0));
        a.action("paste.float", None);
        let id = PinId(1);
        // A high-resolution wheel: two half notches make one step.
        a.pin_event(PinEvent::Wheel(id, 60, (20, 10), false));
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        a.pin_event(PinEvent::Wheel(id, 60, (20, 10), false));
        let info = a.pins.info(id).unwrap();
        assert_eq!(
            (info.zoom, info.rect.x, info.rect.y),
            (110, -2, -1),
            "around (20,10)"
        );
        a.pin_event(PinEvent::Wheel(id, 120, (20, 10), true));
        assert_eq!(
            a.pins.info(id).unwrap().zoom,
            110,
            "Ctrl+wheel (opacity) is N2"
        );
        a.pin_event(PinEvent::Wheel(id, -240, (20, 10), false));
        assert_eq!(a.pins.info(id).unwrap().zoom, 90);
        a.pin_event(PinEvent::MiddleClick(id));
        assert_eq!(a.pins.info(id).unwrap().zoom, 100);
        assert!(
            a.shell.windows.log.borrow().focused.len() <= 1,
            "zooming never focuses"
        );
        a.pin_event(PinEvent::Wheel(PinId(9), 120, (0, 0), false));
        assert!(
            !a.wheel.contains_key(&PinId(9)),
            "nothing is kept for pins that do not exist"
        );
    }

    #[test]
    fn a_monitor_disconnect_keeps_pins_and_offers_the_way_back() {
        let (mut a, syn, _dir) = overlay_app("pin-display");
        let left = a
            .request(
                "pin.create",
                &json!({"source": "region", "region": {"x": -600, "y": 50, "width": 200, "height": 100}}),
            )
            .unwrap();
        assert_eq!(left["pin"]["id"], "pin-1");
        a.request(
            "capture",
            &json!({"region": {"x": 10, "y": 10, "width": 30, "height": 30}, "pin": true}),
        )
        .unwrap();
        assert_eq!(a.status()["pins"]["off_screen"], 0);
        let notices = a.shell.notices.len();
        // The left monitor is unplugged; the runtime reports the display change.
        syn.set_monitors(vec![
            Monitor::new(Rect::new(0, 0, 960, 540), true).with_dpi(144),
        ]);
        a.display_changed();
        let st = a.status();
        assert_eq!(st["pins"]["off_screen"], 1);
        assert!(
            a.shell.notices[notices].contains("off-screen"),
            "{:?}",
            a.shell.notices
        );
        assert_eq!(
            a.pins.rect(PinId(1)),
            Some(Rect::new(-600, 50, 200, 100)),
            "it keeps its place (NOEUL-FR-067)"
        );
        let list = a.request("pin.list", &json!({})).unwrap();
        assert_eq!(list["pins"][0]["on_screen"], false);
        assert_eq!(list["pins"][1]["on_screen"], true);
        assert!(a.tray_menu().contains(&MenuItem::BringPinsOnScreen));
        // A dry run moves nothing; the real request brings only the lost pin back.
        let dry = a
            .request(
                "pin.show",
                &json!({"all": true, "on_screen": true, "dry_run": true}),
            )
            .unwrap();
        assert_eq!(dry["moved"], json!(["pin-1"]));
        assert_eq!(a.pins.rect(PinId(1)).unwrap().x, -600);
        let r = a
            .request("pin.show", &json!({"all": true, "on_screen": true}))
            .unwrap();
        assert_eq!(
            (r["shown"].clone(), r["moved"].clone()),
            (json!([]), json!(["pin-1"]))
        );
        assert_eq!(a.pins.rect(PinId(1)), Some(Rect::new(0, 50, 200, 100)));
        assert_eq!(a.status()["pins"]["off_screen"], 0);
        assert!(!a.tray_menu().contains(&MenuItem::BringPinsOnScreen));
        // The tray's item does the same for every pin, and shows hidden ones.
        syn.set_monitors(vec![Monitor::new(Rect::new(2000, 0, 300, 300), true)]);
        a.display_changed();
        a.request("pin.hide", &json!({"all": true})).unwrap();
        a.tray_command(MenuItem::BringPinsOnScreen);
        let st = a.status();
        assert_eq!(
            (
                st["pins"]["off_screen"].as_u64(),
                st["pins"]["hidden"].as_u64()
            ),
            (Some(0), Some(0))
        );
        // While the desktop is locked nothing is reported as lost.
        syn.set_state(DesktopState::Locked);
        a.display_changed();
        assert_eq!(
            a.request("pin.list", &json!({})).unwrap()["pins"][0]["on_screen"],
            Value::Null
        );
    }

    #[test]
    fn pin_selection_is_the_step_51_entry_point_without_the_clipboard() {
        let (mut a, _syn, _dir) = overlay_app("pin-selection");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        let sid = a.overlay.as_ref().unwrap().id;
        let rect = Rect::new(100, 100, 200, 100);
        a.captures.select(sid, rect).unwrap();
        let sel = a.captures.selection(sid).unwrap();
        let guard = Duration::from_millis(500);
        let id = a
            .pin_selection(
                sel,
                PinOptions {
                    focus: true,
                    guard: Some(guard),
                },
            )
            .unwrap();
        {
            let log = a.shell.windows.log.borrow();
            assert_eq!(
                log.created,
                [(id, rect, true)],
                "same place and pixel size, focused"
            );
            assert_eq!(
                log.guards,
                [Some(guard)],
                "the double-click tail is swallowed"
            );
        }
        assert_eq!(a.shell.clipboard_reads, 0, "no clipboard in between");
        assert_eq!(
            a.ledger.live(),
            (
                2,
                rect.bgra_bytes() + Rect::new(-640, 0, 1600, 540).bgra_bytes()
            ),
            "the pin's copy and the frozen picture, still owned by the open session"
        );
        // A failure hands nothing to the clipboard and keeps the session.
        *a.shell.windows.fail.borrow_mut() = true;
        let sel = a.captures.selection(sid).unwrap();
        assert!(
            a.pin_selection(
                sel,
                PinOptions {
                    focus: true,
                    guard: None
                }
            )
            .is_err()
        );
        assert!(a.overlay_open() && a.captures.is_open());
        assert_eq!(a.ledger.live().0, 2, "the failed copy was freed");
        // Under the test hook a pin never takes focus, whatever the caller asks.
        *a.shell.windows.fail.borrow_mut() = false;
        a.shell.interactive = Some(false);
        let sel = a.captures.selection(sid).unwrap();
        a.pin_selection(
            sel,
            PinOptions {
                focus: true,
                guard: None,
            },
        )
        .unwrap();
        assert!(!a.shell.windows.log.borrow().created[1].2);
    }

    #[test]
    fn paste_never_lands_over_an_open_overlay_and_a_huge_paste_fits() {
        let (mut a, _syn, _dir) = overlay_app("pin-paste-overlay");
        a.shell.clipboard = Some((10, 10));
        a.action("capture.region", None);
        a.action("paste.float", None);
        assert!(a.pins.is_empty());
        assert_eq!(a.shell.clipboard_reads, 0, "the clipboard is not even read");
        assert_eq!(a.status()["last_action"]["result"], "overlay_open");
        assert!(a.overlay_open(), "the overlay keeps its focus");
        a.request("capture.cancel", &json!({})).unwrap();
        // A picture larger than the monitor's work area is zoomed out to fit it.
        let mut b = app(&tmp("pin-paste-huge"), DesktopState::Available);
        b.shell.clipboard = Some((300, 200));
        b.action("paste.float", None);
        let info = b.pins.info(PinId(1)).unwrap();
        assert_eq!(info.image, (300, 200));
        assert_eq!(
            (info.zoom, info.rect),
            (33, Rect::new(1, 34, 99, 66)),
            "fits, at the pointer"
        );
        // pin.create from the clipboard: at a point, never focused, announced, read once.
        let r = b
            .request(
                "pin.create",
                &json!({"source": "clipboard", "dry_run": true}),
            )
            .unwrap();
        assert_eq!(r["pin"], Value::Null);
        assert_eq!(b.shell.clipboard_reads, 1, "a dry run reads nothing");
        b.shell.clipboard = Some((10, 10));
        let r = b
            .request(
                "pin.create",
                &json!({"source": "clipboard", "at": {"x": 95, "y": 2}}),
            )
            .unwrap();
        assert_eq!(
            r["pin"]["rect"],
            json!({"x": 90, "y": 2, "width": 10, "height": 10})
        );
        assert_eq!(b.shell.clipboard_reads, 2);
        assert!(!b.shell.windows.log.borrow().created[1].2, "never focused");
        assert_eq!(
            b.shell.notices.last().unwrap(),
            "Noeul pinned the clipboard (command line)."
        );
        b.shell.clipboard = None;
        let e = b
            .request("pin.create", &json!({"source": "clipboard"}))
            .unwrap_err();
        assert_eq!(e.code, Code::NothingToPin);
        let e = b
            .request(
                "pin.create",
                &json!({"source": "region", "region": {"x": 95, "y": 0, "width": 10, "height": 1}}),
            )
            .unwrap_err();
        assert_eq!(e.code, Code::Usage, "outside the desktop");
    }

    #[test]
    fn pins_live_in_memory_only_and_a_restart_restores_nothing() {
        let dir = tmp("pin-restart");
        let _ = std::fs::remove_dir_all(&dir);
        let mut a = app(&dir, DesktopState::Available);
        for x in [0, 20, 40] {
            a.request(
                "capture",
                &json!({"region": {"x": x, "y": 0, "width": 10, "height": 10}, "pin": true}),
            )
            .unwrap();
        }
        a.request("pin.close", &json!({"id": "pin-2"})).unwrap();
        a.request("pin.hide", &json!({"id": "pin-3"})).unwrap();
        assert_eq!(
            a.ledger.live(),
            (3, 3 * 400),
            "two open and one closed for reopen"
        );
        a.request("quit", &json!({"force": true})).unwrap();
        assert_eq!(a.ledger.live(), (0, 0), "quit frees every picture");
        assert!(
            files_under(&dir).is_empty(),
            "nothing about the pins reached the disk: {:?}",
            files_under(&dir)
        );
        // The next instance starts with no pins, no reopen memory and fresh IDs.
        let mut b = app(&dir, DesktopState::Available);
        let list = b.request("pin.list", &json!({})).unwrap();
        assert_eq!(
            list,
            json!({"pins": [], "reopenable": null, "group": "default"})
        );
        let e = b.request("pin.reopen", &json!({})).unwrap_err();
        assert_eq!(e.code, Code::PinNotFound);
        let r = b
            .request(
                "capture",
                &json!({"region": {"x": 0, "y": 0, "width": 1, "height": 1}, "pin": true}),
            )
            .unwrap();
        assert_eq!(r["pin"]["id"], "pin-1", "IDs belong to one instance");
    }

    #[test]
    fn hide_show_and_close_name_their_pins_and_leave_the_others() {
        let mut a = app(&tmp("pin-hide-show"), DesktopState::Available);
        for x in [0, 30, 60] {
            a.request(
                "capture",
                &json!({"region": {"x": x, "y": 0, "width": 10, "height": 10}, "pin": true}),
            )
            .unwrap();
        }
        let dry = a
            .request("pin.hide", &json!({"all": true, "dry_run": true}))
            .unwrap();
        assert_eq!(dry["hidden"], json!(["pin-1", "pin-2", "pin-3"]));
        assert_eq!(a.pins.hidden(), 0, "a dry run hides nothing");
        let r = a.request("pin.hide", &json!({"id": "pin-2"})).unwrap();
        assert_eq!(r["hidden"], json!(["pin-2"]));
        let r = a.request("pin.hide", &json!({"all": true})).unwrap();
        assert_eq!(
            r["hidden"],
            json!(["pin-1", "pin-3"]),
            "pin-2 was hidden already"
        );
        a.tray_command(MenuItem::ShowAllPins);
        assert_eq!(a.pins.hidden(), 0);
        a.tray_command(MenuItem::HideAllPins);
        assert_eq!(a.pins.hidden(), 3);
        let r = a.request("pin.show", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(r["shown"], json!(["pin-1"]));
        a.request("pin.close", &json!({"id": "pin-1"})).unwrap();
        assert_eq!(a.pins.ids(), [PinId(2), PinId(3)]);
        assert!(a.pins.info(PinId(2)).is_some_and(|i| !i.visible));
        let e = a.request("pin.show", &json!({"id": "pin-1"})).unwrap_err();
        assert_eq!(e.code, Code::PinNotFound);
        let e = a
            .request("pin.show", &json!({"id": "pin-2", "on_screen": "yes"}))
            .unwrap_err();
        assert_eq!(e.code, Code::Usage);
        assert!(
            a.shell.windows.log.borrow().focused.is_empty(),
            "no IPC request gave a pin focus"
        );
    }

    // ---- Step 51: the outputs of the overlay (UX spec §4.3, window-free) ----

    fn press_at(a: &mut App<FakeShell>, b: Button, x: i32, y: i32, mods: Mods, t: u32) {
        ev(
            a,
            OverlayInput::Press {
                button: b,
                x,
                y,
                mods,
                time_ms: t,
            },
        );
    }

    fn up(a: &mut App<FakeShell>, x: i32, y: i32) {
        ev(
            a,
            OverlayInput::Release {
                button: Button::Left,
                x,
                y,
            },
        );
    }

    /// Two plain clicks 120 ms apart at `(x, y)`.
    fn double_click(a: &mut App<FakeShell>, x: i32, y: i32, t: u32) {
        press_at(a, Button::Left, x, y, Mods::NONE, t);
        up(a, x, y);
        press_at(a, Button::Left, x, y, Mods::NONE, t + 120);
        up(a, x, y);
    }

    /// The synthetic pattern of `r` as a frame-ordered BGRA buffer.
    fn pattern(r: Rect) -> Vec<u8> {
        let mut v = Vec::new();
        for y in r.y..r.y + r.height as i32 {
            for x in r.x..r.x + r.width as i32 {
                v.extend_from_slice(&Synthetic::pixel(x, y));
            }
        }
        v
    }

    /// The pixels of a PNG as BGRA (opaque RGB PNGs, as Noeul writes them for captures).
    fn png_bgra(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes.to_vec()))
            .read_info()
            .unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgb, "opaque capture: RGB");
        let mut out = Vec::new();
        for p in buf[..info.buffer_size()].as_chunks::<3>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
        (info.width, info.height, out)
    }

    fn set_config(a: &mut App<FakeShell>, values: &[(&str, &str)]) {
        let path = a.paths.config_file();
        let edited = settings::set(&path, values).unwrap();
        settings::write(&path, &edited.text).unwrap();
        a.reload_settings();
        assert!(a.settings.problems.is_empty(), "{:?}", a.settings.problems);
    }

    #[test]
    fn the_double_click_pins_exactly_the_selection_with_no_clipboard_or_file() {
        // FLOW-01 and FLOW-28 on the 150 % primary monitor of the synthetic layout.
        let (mut a, syn, dir) = overlay_app("dbl-pin");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (499, 399));
        double_click(&mut a, 300, 250, 1_000);
        assert!(!a.overlay_open(), "the overlay closed on the second press");
        let log = a.shell.windows.log.borrow();
        assert_eq!(
            log.created,
            [(PinId(1), Rect::new(100, 100, 400, 300), true)],
            "same place and size, with focus"
        );
        assert_eq!(
            log.guards,
            [Some(Duration::from_millis(500))],
            "the new pin ignores presses for one double-click time (rule 10)"
        );
        drop(log);
        // The third and fourth presses of a quadruple click reach no overlay: one pin.
        press_at(&mut a, Button::Left, 300, 250, Mods::NONE, 1_240);
        up(&mut a, 300, 250);
        press_at(&mut a, Button::Left, 300, 250, Mods::NONE, 1_360);
        assert_eq!(a.pins.len(), 1);
        assert_eq!(
            a.shell.clipboard_writes + a.shell.clipboard_reads,
            0,
            "no clipboard"
        );
        assert_eq!(files_under(&dir), ["ui-state.json"], "no picture on disk");
        assert_eq!(syn.captures(), 1, "the pin shows the frozen picture");
        assert_eq!(a.ledger.live(), (1, 400 * 300 * 4));
        let st = a.status();
        assert_eq!(st["captures"]["pinned"], 1);
        assert_eq!(st["captures"]["copied"], 0);
        assert_eq!(
            st["last_action"],
            json!({"action": "capture.pin", "result": "pinned"})
        );
    }

    #[test]
    fn enter_the_toolbar_keys_and_copy_follow_the_focus_rule() {
        let (mut a, _syn, _dir) = overlay_app("keys-copy");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (499, 399));
        // FLOW-17: Tab → Pin, Tab → Copy, Enter copies (the focused button, not capture.pin).
        key(&mut a, "Tab", false, false);
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "pin");
        key(&mut a, "Tab", false, false);
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "copy");
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert!(a.pins.is_empty(), "copy never pins");
        // FLOW-19: PNG and CF_DIBV5 with exactly the frozen pixels.
        let (png, dib) = a.shell.copies[0].clone();
        let sel = Rect::new(100, 100, 400, 300);
        let (w, h, px) = png_bgra(&png);
        assert_eq!((w, h), (400, 300));
        assert!(px == pattern(sel), "the PNG holds the frozen pixels");
        let back = crate::dib::decode(&a.ledger, &dib).unwrap();
        assert!(back.pixels() == pattern(sel).as_slice(), "so does the DIB");
        drop(back);
        let st = a.status();
        assert_eq!(
            (
                st["captures"]["copied"].as_u64(),
                st["captures"]["pinned"].as_u64()
            ),
            (Some(1), Some(0))
        );
        assert_eq!(a.ledger.live(), (0, 0), "nothing is kept after a copy");
        // FLOW-16 with the toolbar: Shift+Tab reaches Cancel, Tab twice more reaches Pin.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "Tab", false, true);
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "cancel");
        key(&mut a, "Tab", false, false);
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "canvas");
        key(&mut a, "Tab", false, false);
        key(&mut a, "Space", false, false);
        let log = a.shell.windows.log.borrow();
        assert_eq!(
            log.created[0].1, sel,
            "the Pin button pinned the last rectangle"
        );
        assert_eq!(log.guards, [None], "a key needs no guard");
        drop(log);
        // Ctrl+C and Escape on a focused button: copy / cancel.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "C", true, false);
        assert_eq!(a.shell.copies.len(), 2);
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "Tab", false, false);
        key(&mut a, "Escape", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["action"], "capture.cancel");
    }

    #[test]
    fn a_busy_clipboard_never_affects_pin_and_a_failed_output_keeps_the_work() {
        // FLOW-27: another program keeps the clipboard open.
        let (mut a, _syn, _dir) = overlay_app("busy");
        a.shell.clipboard_busy = true;
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        key(&mut a, "C", true, false);
        assert!(a.overlay_open(), "the copy failed; the work is kept");
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "capture.copy", "result": "clipboard_unavailable"})
        );
        let banner = a
            .shell
            .overlays
            .log
            .borrow()
            .scene
            .clone()
            .unwrap()
            .help
            .unwrap();
        assert!(
            banner.lines[0].starts_with("The clipboard is in use"),
            "{banner:?}"
        );
        double_click(&mut a, 200, 150, 0);
        assert_eq!(a.pins.len(), 1, "the double-click pinned regardless");
        assert_eq!(
            a.shell.clipboard_writes, 1,
            "only the copy attempt touched it"
        );
        // FLOW-26: pin creation fails; the overlay stays; Ctrl+C still copies.
        a.shell.clipboard_busy = false;
        *a.shell.windows.fail.borrow_mut() = true;
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        double_click(&mut a, 200, 150, 5_000);
        assert!(a.overlay_open());
        let banner = a
            .shell
            .overlays
            .log
            .borrow()
            .scene
            .clone()
            .unwrap()
            .help
            .unwrap();
        assert!(banner.lines[0].starts_with("The pin could not be created"));
        key(&mut a, "C", true, false);
        assert!(!a.overlay_open());
        assert_eq!(a.shell.copies.len(), 1);
        assert_eq!(a.pins.len(), 1);
    }

    #[test]
    fn save_writes_only_where_the_user_chose_and_quick_save_never_overwrites() {
        let (mut a, _syn, dir) = overlay_app("save");
        let shots = dir.join("shots");
        let shots_text = shots.display().to_string();
        // No folder set and none known (the test hook): quick save explains, keeps the work.
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        key(&mut a, "S", true, true);
        assert!(a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "no_folder");
        // FLOW-20: Ctrl+Shift+S into the configured folder; a second one never overwrites.
        set_config(&mut a, &[("capture.quick_save_folder", &shots_text)]);
        key(&mut a, "S", true, true);
        assert!(!a.overlay_open());
        let first = shots.join("noeul-20261104-090507.png");
        let (w, h, px) = png_bgra(&std::fs::read(&first).unwrap());
        assert_eq!((w, h), (200, 100));
        assert!(px == pattern(Rect::new(100, 100, 200, 100)));
        assert_eq!(
            a.shell.notices.last().unwrap(),
            &format!("Saved to {}", first.display())
        );
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "S", true, true);
        assert!(shots.join("noeul-20261104-090507-2.png").exists());
        assert!(
            std::fs::read(&first).unwrap().len() > 8,
            "the first file stays"
        );
        // Ctrl+S: the save dialog is asked for; cancelling keeps the overlay; a chosen file is
        // written and closes it.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "S", true, false);
        let req = a
            .take_save_request()
            .expect("the runtime is asked for the dialog");
        assert_eq!(req.name, "noeul-20261104-090507.png");
        assert_eq!((req.folder.clone(), req.explicit), (Some(shots), true));
        assert_eq!(req.rect, Rect::new(100, 100, 200, 100));
        assert!(a.overlay_open(), "nothing is written before the user chose");
        a.save_chosen(&req, None);
        assert!(a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "cancelled");
        key(&mut a, "S", true, false);
        let req = a.take_save_request().unwrap();
        let chosen = dir.join("chosen.png");
        a.save_chosen(&req, Some(chosen.clone()));
        assert!(!a.overlay_open());
        assert!(png_bgra(&std::fs::read(&chosen).unwrap()).2 == pattern(req.rect));
        // An answer for a session that has ended is ignored.
        a.save_chosen(&req, Some(dir.join("late.png")));
        assert!(!dir.join("late.png").exists());
        let st = a.status();
        assert_eq!(st["captures"]["saved"], 3);
        assert_eq!(
            a.shell.clipboard_writes, 0,
            "saving never touches the clipboard"
        );
        // The default folder: the Pictures folder + Noeul.
        set_config(&mut a, &[]);
        a.settings.settings.quick_save_folder = None;
        a.shell.pictures = Some(dir.join("Pictures"));
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "S", true, true);
        assert!(
            dir.join("Pictures")
                .join("Noeul")
                .join("noeul-20261104-090507.png")
                .exists()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gesture_settings_and_the_snipaste_style_preset_reach_the_next_overlay() {
        let (mut a, _syn, _dir) = overlay_app("gestures");
        // FLOW-24: the double-click copies; Enter still pins.
        set_config(&mut a, &[("gestures.double_click", "copy")]);
        assert_eq!(a.status()["settings"]["gestures"]["double_click"], "copy");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        double_click(&mut a, 200, 150, 0);
        assert_eq!((a.shell.copies.len(), a.pins.len()), (1, 0));
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "Enter", false, false);
        assert_eq!(a.pins.len(), 1);
        // FLOW-25: `none` does nothing on a double-click.
        set_config(&mut a, &[("gestures.double_click", "none")]);
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        double_click(&mut a, 200, 150, 0);
        assert!(a.overlay_open());
        key(&mut a, "Escape", false, false);
        // The Snipaste-style preset: config values plus its keys in hotkeys.json.
        set_config(
            &mut a,
            &[
                ("gestures.preset", "snipaste"),
                ("gestures.double_click", "copy"),
                ("gestures.middle_click", "pin"),
            ],
        );
        std::fs::write(
            a.paths.hotkeys_file(),
            serde_json::to_vec(&json!({"schema": "kim.hotkeys", "schema_version": 1,
                "tool": "noeul", "overrides": [
                    {"action": "capture.pin", "bindings": ["Ctrl+T"]},
                    {"action": "capture.copy", "bindings": ["Ctrl+C", "Enter"]}]}))
            .unwrap(),
        )
        .unwrap();
        assert!(a.hotkeys().reload().is_none());
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "Enter", false, false);
        assert_eq!(a.shell.copies.len(), 2, "Enter copies with the preset");
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        key(&mut a, "T", true, false);
        assert_eq!(a.pins.len(), 2, "Ctrl+T pins with the preset");
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        press_at(&mut a, Button::Middle, 200, 150, Mods::NONE, 0);
        assert_eq!(a.pins.len(), 3, "the middle-click pins");
        assert_eq!(a.status()["settings"]["gestures"]["preset"], "snipaste");
    }

    #[test]
    fn rapid_repeated_captures_each_make_an_independent_pin() {
        // FLOW-29 and FLOW-30: ten captures as fast as possible, F1 pressed again each time.
        let (mut a, _syn, _dir) = overlay_app("rapid");
        for i in 0..10 {
            a.action("capture.region", None);
            a.action("capture.region", None); // ignored: one overlay at a time
            assert_eq!(a.shell.overlays.log.borrow().alive, 1);
            let x = -600 + i * 60;
            drag(&mut a, (x, 100), (x + 49, 139));
            double_click(&mut a, x + 25, 120, i as u32 * 1_000);
            assert!(!a.overlay_open(), "capture {i}");
        }
        assert_eq!(a.pins.len(), 10);
        let created = a.shell.windows.log.borrow().created.clone();
        let ids: std::collections::BTreeSet<_> = created.iter().map(|c| c.0).collect();
        assert_eq!(ids.len(), 10, "distinct IDs");
        for (i, c) in created.iter().enumerate() {
            assert_eq!(
                c.1,
                Rect::new(-600 + i as i32 * 60, 100, 50, 40),
                "no lost image"
            );
        }
        let st = a.status();
        assert_eq!(
            (
                st["captures"]["ignored"].as_u64(),
                st["captures"]["pinned"].as_u64()
            ),
            (Some(10), Some(10))
        );
        assert_eq!(a.shell.overlays.log.borrow().opened.len(), 10);
    }

    #[test]
    fn the_selection_menu_is_handed_to_the_runtime_or_recorded_under_the_hook() {
        let (mut a, _syn, _dir) = overlay_app("menu");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        press_at(&mut a, Button::Right, 150, 150, Mods::NONE, 0);
        let menu = a.take_overlay_menu().expect("a menu to show");
        assert_eq!(menu.at, (150, 150));
        let labels: Vec<&str> = menu.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Pin\tEnter",
                "Copy\tCtrl+C",
                "Save\u{2026}\tCtrl+S",
                "Quick save\tCtrl+Shift+S",
                "",
                "Select monitor\tCtrl+A",
                "Last rectangle\tR",
                "",
                "Cancel\tEscape",
            ]
        );
        assert!(!menu.items[6].enabled, "no last rectangle yet");
        a.overlay_menu_choice("capture.copy");
        assert!(!a.overlay_open());
        assert_eq!(a.shell.copies.len(), 1);
        // The keyboard opens it at the selection's corner; Pin from the menu needs no guard.
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        ev(&mut a, OverlayInput::MenuKey);
        let menu = a.take_overlay_menu().unwrap();
        assert_eq!(menu.at, (108, 108));
        assert!(menu.items[6].enabled);
        a.overlay_menu_choice("capture.pin");
        assert_eq!(a.shell.windows.log.borrow().guards, [None]);
        a.overlay_menu_choice("format.disk"); // not a menu entry: ignored
        // Under the test hook a menu is never shown.
        a.shell.interactive = Some(false);
        a.action("capture.region", None);
        key(&mut a, "R", false, false);
        press_at(&mut a, Button::Right, 150, 150, Mods::NONE, 0);
        assert!(a.take_overlay_menu().is_none());
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "capture.menu", "result": "not_shown"})
        );
    }

    #[test]
    fn a_pin_copies_and_saves_its_own_picture() {
        let (mut a, _syn, dir) = overlay_app("pin-out");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        double_click(&mut a, 200, 150, 0);
        let id = PinId(1);
        let sel = Rect::new(100, 100, 200, 100);
        let ctrl =
            |name: &str, shift: bool| crate::hotkeys::chord_of(name, true, false, shift).unwrap();
        // Ctrl+C on the focused pin: its picture at 100 %, even when zoomed.
        a.action("pin.zoom-in", Some(id));
        a.pin_event(PinEvent::Key(id, ctrl("C", false), false));
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "pin.copy", "result": "copied"})
        );
        let (png, _) = a.shell.copies[0].clone();
        let (w, h, px) = png_bgra(&png);
        assert_eq!((w, h), (200, 100));
        assert!(px == pattern(sel));
        // A held clipboard: a notice, the pin stays.
        a.shell.clipboard_busy = true;
        a.pin_event(PinEvent::Key(id, ctrl("C", false), false));
        assert_eq!(a.status()["last_action"]["result"], "clipboard_unavailable");
        assert!(a.pins.contains(id));
        // Ctrl+S asks for the dialog owned by the pin; the answer writes the picture.
        a.pin_event(PinEvent::Key(id, ctrl("S", false), false));
        let req = a.take_save_request().unwrap();
        assert_eq!((req.pin, req.rect), (Some(id), sel));
        let out = dir.join("pinned.png");
        a.save_chosen(&req, Some(out.clone()));
        assert!(png_bgra(&std::fs::read(&out).unwrap()).2 == pattern(sel));
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "pin.save", "result": "saved"})
        );
        a.save_chosen(&req, None);
        assert_eq!(a.status()["last_action"]["result"], "cancelled");
        // Ctrl+Shift+S: quick save, never overwriting; from the menu as well.
        set_config(
            &mut a,
            &[(
                "capture.quick_save_folder",
                &dir.join("q").display().to_string(),
            )],
        );
        a.pin_event(PinEvent::Key(id, ctrl("S", true), false));
        a.pin_menu_choice(id, "pin.quick-save");
        assert!(dir.join("q").join("noeul-20261104-090507.png").exists());
        assert!(dir.join("q").join("noeul-20261104-090507-2.png").exists());
        assert!(a.pins.contains(id), "outputs never close the pin");
        a.pins.close(id).unwrap();
        a.action("pin.copy", Some(id));
        assert_eq!(a.status()["last_action"]["result"], "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_users_double_click_speed_is_read_before_every_press() {
        // FLOW-36: Windows' double-click time changes to 900 ms while the overlay is open.
        let (mut a, _syn, _dir) = overlay_app("speed");
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (299, 199));
        a.shell.sys = Some(SystemValues {
            double_click_ms: 900,
            ..SystemValues::default()
        });
        press_at(&mut a, Button::Left, 200, 150, Mods::NONE, 0);
        up(&mut a, 200, 150);
        press_at(&mut a, Button::Left, 200, 150, Mods::NONE, 800);
        assert_eq!(a.pins.len(), 1);
        assert_eq!(
            a.shell.windows.log.borrow().guards,
            [Some(Duration::from_millis(900))]
        );
    }

    // ---- Step 52: smart selection reaches the platform, the status and the history ----

    fn overlay_status(a: &App<FakeShell>) -> Value {
        let st = a.status();
        let errors = crate::schema_errors("noeul-ipc-v1#status_result", &st);
        assert!(errors.is_empty(), "{errors:?}\n{st:#}");
        st["captures"]["overlay"].clone()
    }

    fn mouse(a: &mut App<FakeShell>, x: i32, y: i32) {
        ev(
            a,
            OverlayInput::Move {
                x,
                y,
                mods: Mods::NONE,
            },
        );
    }

    #[test]
    fn smart_selection_reaches_the_platform_and_the_status() {
        use crate::detect::{ElementAnswer, WindowFacts};
        let (mut a, _syn, _dir) = overlay_app("ov-smart");
        a.shell.facts = vec![
            WindowFacts::plain(11, Rect::new(100, 100, 300, 200)),
            WindowFacts::plain(12, Rect::new(-640, 0, 1600, 540)),
        ];
        a.action("capture.region", None);
        assert_eq!(a.shell.snapshots, 1, "listed once, with the frozen picture");
        mouse(&mut a, 150, 150);
        let ov = overlay_status(&a);
        assert_eq!(ov["detection"]["mode"], "window");
        assert_eq!(ov["detection"]["windows"], 2);
        assert_eq!(
            ov["detection"]["target"],
            json!({"kind": "window", "rect": {"x": 100, "y": 100, "width": 300, "height": 200}})
        );
        assert_eq!(ov["magnifier"], json!({"enabled": true, "shown": true}));
        assert_eq!(ov["pointer"], json!({"x": 150, "y": 150}));
        // E: element detection asks the platform, off the UI thread.
        key(&mut a, "E", false, false);
        mouse(&mut a, 160, 160);
        let req = *a.shell.element_requests.last().expect("an element request");
        assert_eq!((req.token, req.at), (11, (160, 160)));
        let button = Rect::new(150, 150, 40, 20);
        a.overlay_event(OverlayEvent::Element(req, ElementAnswer::Found(button)));
        let ov = overlay_status(&a);
        assert_eq!(ov["detection"]["mode"], "element");
        assert_eq!(ov["detection"]["target"]["kind"], "element");
        // A physical W moves the pointer up one pixel and the real cursor with it.
        let w = crate::hotkeys::chord_of("W", false, false, false).unwrap();
        let code = crate::hotkeys::physical_chord(0x11, false, false, false, false);
        a.overlay_event(OverlayEvent::Key(w, code, false));
        assert_eq!(a.shell.overlays.log.borrow().warps, [(160, 159)]);
        let ov = overlay_status(&a);
        assert_eq!(ov["pointer"], json!({"x": 160, "y": 159}));
        assert_eq!(ov["detection"]["suspended"], true);
        assert_eq!(ov["detection"]["target"], Value::Null);
        // The logical W alone (no position) does nothing.
        a.overlay_event(OverlayEvent::Key(w, None, false));
        assert_eq!(a.shell.overlays.log.borrow().warps.len(), 1);
        // The mouse again: the element is back; a click selects it, Enter pins it.
        mouse(&mut a, 170, 165);
        ev(
            &mut a,
            OverlayInput::Press {
                button: Button::Left,
                x: 170,
                y: 165,
                mods: Mods::NONE,
                time_ms: 0,
            },
        );
        ev(
            &mut a,
            OverlayInput::Release {
                button: Button::Left,
                x: 170,
                y: 165,
            },
        );
        assert_eq!(overlay_status(&a)["selection"], button.to_json());
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.shell.detection_ended, 1, "lookups stop with the overlay");
        assert_eq!(a.status()["captures"]["history"], 1);
        // The next capture steps back to it with Comma.
        a.action("capture.region", None);
        key(&mut a, "Comma", false, false);
        let ov = overlay_status(&a);
        assert_eq!(ov["selection"], button.to_json());
        assert_eq!(ov["history"], json!({"count": 1, "position": 0}));
        // M toggles the magnifier.
        key(&mut a, "M", false, false);
        assert_eq!(overlay_status(&a)["magnifier"]["enabled"], false);
        a.request("capture.cancel", &json!({})).unwrap();
        assert_eq!(a.shell.detection_ended, 2);
        // Nothing of a window's text exists anywhere in the status (there is none to report).
        assert!(!a.status().to_string().to_lowercase().contains("title"));
    }

    #[test]
    fn detection_off_lists_no_windows_until_e_turns_it_on_and_the_magnifier_setting_applies() {
        use crate::detect::WindowFacts;
        let (mut a, _syn, _dir) = overlay_app("ov-smart-off");
        a.shell.facts = vec![WindowFacts::plain(11, Rect::new(100, 100, 300, 200))];
        set_config(
            &mut a,
            &[("capture.detection", "off"), ("capture.magnifier", "off")],
        );
        a.action("capture.region", None);
        assert_eq!(a.shell.snapshots, 0, "off: no window is listed");
        mouse(&mut a, 150, 150);
        let ov = overlay_status(&a);
        assert_eq!(
            (
                ov["detection"]["mode"].as_str(),
                ov["detection"]["windows"].clone()
            ),
            (Some("off"), Value::Null)
        );
        assert_eq!(ov["magnifier"], json!({"enabled": false, "shown": false}));
        key(&mut a, "E", false, false);
        assert_eq!(a.shell.snapshots, 1, "listed when detection is turned on");
        assert_eq!(overlay_status(&a)["detection"]["windows"], 1);
        mouse(&mut a, 151, 150);
        assert_eq!(overlay_status(&a)["detection"]["target"]["kind"], "window");
        a.request("capture.cancel", &json!({})).unwrap();
    }

    /// FLOW-18 through the instance core: `W A S D` (physical keys) to one corner, `Space`,
    /// to the other, `Space`, `Enter`: a pin of exactly that rectangle.
    #[test]
    fn keyboard_only_marking_pins_the_marked_rectangle() {
        let (mut a, _syn, _dir) = overlay_app("ov-flow18");
        a.action("capture.region", None);
        mouse(&mut a, 95, 98);
        let press_code = |a: &mut App<FakeShell>, name: &str, sc: u32, n: usize| {
            let c = crate::hotkeys::chord_of(name, false, false, false).unwrap();
            for _ in 0..n {
                let code = crate::hotkeys::physical_chord(sc, false, false, false, false);
                a.overlay_event(OverlayEvent::Key(c, code, false));
            }
        };
        press_code(&mut a, "D", 0x20, 5);
        press_code(&mut a, "S", 0x1F, 2);
        assert_eq!(overlay_status(&a)["pointer"], json!({"x": 100, "y": 100}));
        key(&mut a, "Space", false, false);
        assert_eq!(overlay_status(&a)["phase"], "marking");
        mouse(&mut a, 497, 402);
        press_code(&mut a, "D", 0x20, 3);
        press_code(&mut a, "W", 0x11, 2);
        key(&mut a, "Space", false, false);
        let want = Rect::new(100, 100, 401, 301);
        assert_eq!(overlay_status(&a)["selection"], want.to_json());
        key(&mut a, "Enter", false, false);
        let created = a.shell.windows.log.borrow().created.clone();
        assert_eq!(
            created.last().unwrap().1,
            want,
            "the pin is the marked rectangle"
        );
    }

    // ----- Step 56: annotation through the instance core (NOEUL-FR-080…083, NOEUL-NFR-014) -----

    const RED: [u8; 4] = [0x4D, 0x48, 0xE5, 0xFF];

    /// The centre of a toolbar button of the open overlay (from the scene it shows).
    fn bar_button(a: &App<FakeShell>, item: overlay::ToolbarItem) -> (i32, i32) {
        let bar = a.overlay.as_ref().unwrap().scene.toolbar.clone().unwrap();
        let r = bar.buttons.iter().find(|b| b.item == item).unwrap().rect;
        (
            r.x + i32::try_from(r.width / 2).unwrap(),
            r.y + i32::try_from(r.height / 2).unwrap(),
        )
    }

    fn click_button(a: &mut App<FakeShell>, item: overlay::ToolbarItem) {
        let (x, y) = bar_button(a, item);
        press_at(a, Button::Left, x, y, Mods::NONE, 0);
        up(a, x, y);
    }

    fn choose(a: &mut App<FakeShell>, t: annotation::Tool) {
        click_button(a, overlay::ToolbarItem::Tool(t));
        assert_eq!(
            a.overlay.as_ref().unwrap().model.editor().tool(),
            Some(t),
            "{t:?} chosen from the toolbar"
        );
    }

    /// The overlay's (or editor's) status, validated against the schema.
    fn checked_status(a: &App<FakeShell>) -> Value {
        let st = a.status();
        let errors = crate::schema_errors("noeul-ipc-v1#status_result", &st);
        assert!(errors.is_empty(), "{errors:?}\n{st:#}");
        st
    }

    /// The pixel `(x, y)` of a BGRA buffer covering `r`.
    fn px_of(buf: &[u8], r: Rect, x: i32, y: i32) -> [u8; 4] {
        let i = (((y - r.y) as u32 * r.width + (x - r.x) as u32) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// How many pixels of column `x` (rows `y0..y1`) are exactly the annotation red.
    fn red_rows(buf: &[u8], r: Rect, x: i32, y0: i32, y1: i32) -> usize {
        (y0..y1).filter(|y| px_of(buf, r, x, *y) == RED).count()
    }

    #[test]
    fn what_the_overlay_shows_is_what_copy_exports_at_100_and_150_percent() {
        let (mut a, _syn, dir) = overlay_app("ann-copy");
        // On the 100 % monitor: a 3-pixel rectangle outline.
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        let sel = Rect::new(-600, 20, 400, 200);
        choose(&mut a, annotation::Tool::Rectangle);
        drag(&mut a, (-580, 40), (-500, 100));
        let pic = a.overlay.as_ref().unwrap().model.picture();
        // Near the top of the monitor the size chip sits inside the selection, over the top edge.
        let chip = a.overlay.as_ref().unwrap().scene.chip.clone().unwrap().rect;
        {
            let log = a.shell.overlays.log.borrow();
            assert_eq!(px_of(&log.buffer, pic, -580, 70), RED, "shown at once");
            assert_eq!(red_rows(&log.buffer, pic, -540, 60, 110), 3, "a 3-px edge");
        }
        let st = checked_status(&a);
        let ov = &st["captures"]["overlay"];
        assert_eq!(ov["phase"], "annotating");
        assert_eq!(ov["annotation"]["objects"], 1);
        assert_eq!(ov["annotation"]["kinds"]["rectangle"], 1);
        assert_eq!(ov["annotation"]["tool"], "rectangle");
        let shown = |x: i32, y: i32| !chip.contains(&Rect::new(x, y, 1, 1));
        let screen: Vec<[u8; 4]> = {
            let log = a.shell.overlays.log.borrow();
            (sel.y..sel.y + 200)
                .flat_map(|y| (sel.x..sel.x + 400).map(move |x| (x, y)))
                .filter(|(x, y)| shown(*x, *y))
                .map(|(x, y)| px_of(&log.buffer, pic, x, y))
                .collect()
        };
        key(&mut a, "C", true, false);
        let (png, dib) = a.shell.copies[0].clone();
        let (w, h, px) = png_bgra(&png);
        assert_eq!((w, h), (400, 200));
        let exported: Vec<[u8; 4]> = (sel.y..sel.y + 200)
            .flat_map(|y| (sel.x..sel.x + 400).map(move |x| (x, y)))
            .filter(|(x, y)| shown(*x, *y))
            .map(|(x, y)| px_of(&px, sel, x, y))
            .collect();
        assert!(
            exported == screen,
            "the copy holds exactly what the overlay showed (outside the readout)"
        );
        assert_eq!(red_rows(&px, sel, -540, 30, 110), 6, "both 3-px edges");
        assert_eq!(px_of(&px, sel, -580, 70), RED);
        assert_eq!(
            px_of(&px, sel, -576, 70),
            Synthetic::pixel(-576, 70),
            "inside the outline: the frozen pixels"
        );
        assert!(!dib.is_empty());
        // On the 150 % monitor the same tool draws 5 device pixels wide.
        a.action("capture.region", None);
        drag(&mut a, (100, 100), (399, 299));
        let sel = Rect::new(100, 100, 300, 200);
        choose(&mut a, annotation::Tool::Rectangle);
        drag(&mut a, (120, 120), (200, 200));
        key(&mut a, "C", true, false);
        let (w, h, px) = png_bgra(&a.shell.copies[1].0);
        assert_eq!((w, h), (300, 200));
        assert_eq!(
            red_rows(&px, sel, 160, 100, 300),
            10,
            "two 5-px edges at 150 %"
        );
        assert_eq!(px_of(&px, sel, 124, 160), RED);
        assert_eq!(px_of(&px, sel, 125, 160), Synthetic::pixel(125, 160));
        assert_eq!(a.ledger.live().0, 0, "every picture and layer is freed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn annotations_across_the_monitor_seam_keep_the_dpi_they_were_drawn_at() {
        let (mut a, _syn, dir) = overlay_app("ann-seam");
        set_config(
            &mut a,
            &[(
                "capture.quick_save_folder",
                &dir.join("q").display().to_string(),
            )],
        );
        a.action("capture.region", None);
        drag(&mut a, (-100, 100), (99, 249));
        let sel = Rect::new(-100, 100, 200, 150);
        choose(&mut a, annotation::Tool::Line);
        // One line pressed on the 100 % monitor, crossing into the 150 % one; one on the 150 %.
        drag(&mut a, (-60, 130), (60, 130));
        drag(&mut a, (60, 170), (-60, 170));
        key(&mut a, "S", true, true);
        assert_eq!(a.status()["last_action"]["result"], "saved");
        let file = dir.join("q").join("noeul-20261104-090507.png");
        let (w, h, px) = png_bgra(&std::fs::read(&file).unwrap());
        assert_eq!((w, h), (200, 150));
        for x in [-30, 30] {
            assert_eq!(red_rows(&px, sel, x, 120, 145), 3, "line 1 at x {x}");
            assert_eq!(red_rows(&px, sel, x, 160, 185), 5, "line 2 at x {x}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_redo_and_cancel_are_lossless_and_leave_nothing_behind() {
        let (mut a, _syn, dir) = overlay_app("ann-undo");
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        choose(&mut a, annotation::Tool::Ellipse);
        drag(&mut a, (-580, 40), (-500, 100));
        drag(&mut a, (-400, 40), (-300, 100));
        let objects =
            |a: &App<FakeShell>| a.status()["captures"]["overlay"]["annotation"]["objects"].clone();
        let all = a.overlay.as_ref().unwrap().model.annotations().to_vec();
        key(&mut a, "Z", true, false);
        assert_eq!(objects(&a), 1);
        key(&mut a, "Y", true, false);
        assert_eq!(objects(&a), 2);
        key(&mut a, "Z", true, false);
        key(&mut a, "Z", true, true);
        assert_eq!(
            a.overlay.as_ref().unwrap().model.annotations(),
            all.as_slice(),
            "Ctrl+Shift+Z redoes too, exactly"
        );
        let st = checked_status(&a);
        assert_eq!(st["captures"]["overlay"]["annotation"]["undo"], 2);
        // Escape leaves the tool (the drawing stays), the next Escape cancels the capture.
        key(&mut a, "Escape", false, false);
        assert!(a.overlay_open());
        assert_eq!(a.status()["captures"]["overlay"]["phase"], "selected");
        assert_eq!(objects(&a), 2);
        key(&mut a, "Escape", false, false);
        assert!(!a.overlay_open(), "cancelled");
        assert!(a.shell.copies.is_empty() && a.shell.clipboard_writes == 0);
        assert!(a.pins.is_empty());
        assert_eq!(
            a.ledger.live(),
            (0, 0),
            "the picture and the annotated layer are freed"
        );
        assert!(files_under(&dir).iter().all(|f| !f.ends_with(".png")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pin_keeps_its_annotations_editable_and_edits_commit_crop_or_cancel() {
        let (mut a, _syn, dir) = overlay_app("ann-pin");
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        let sel = Rect::new(-600, 20, 400, 200);
        choose(&mut a, annotation::Tool::Rectangle);
        drag(&mut a, (-580, 40), (-500, 100));
        key(&mut a, "Enter", false, false);
        let id = PinId(1);
        assert!(!a.overlay_open());
        // The pin keeps the unchanged pixels and the object (in image pixels).
        assert!(a.pins.picture(id).unwrap().pixels() == pattern(sel).as_slice());
        let ann = a.pins.annotations(id).unwrap().to_vec();
        assert_eq!(ann.len(), 1);
        assert_eq!(
            ann[0].bounds().l,
            20.0 - 1.0,
            "image pixels (its box grown by 1)"
        );
        let shown = a.pins.rendered(id).unwrap();
        assert_eq!(px_of(shown.pixels(), shown.rect(), -580, 70), RED);
        drop(shown);
        let list = a.request("pin.list", &json!({})).unwrap();
        assert_eq!(list["pins"][0]["annotations"], 1);
        assert_eq!(list["pins"][0]["editing"], false);
        // Copy outputs the pin with its annotations.
        a.action("pin.copy", Some(id));
        let (_, _, px) = png_bgra(&a.shell.copies[0].0);
        assert_eq!(px_of(&px, sel, -580, 70), RED);
        // pin.annotate: a layered editor over the pin; the pin's window hides meanwhile.
        let ctrl_less = crate::hotkeys::chord_of("Space", false, false, false).unwrap();
        a.pin_event(PinEvent::Key(id, ctrl_less, false));
        assert_eq!(a.status()["last_action"]["result"], "editing");
        assert!(a.overlay_open());
        assert_eq!(a.shell.overlays.log.borrow().layered.last(), Some(&true));
        let st = checked_status(&a);
        assert_eq!(st["pins"]["editor"]["mode"], "pin_edit");
        assert_eq!(st["pins"]["editor"]["pin"], "pin-1");
        assert_eq!(st["pins"]["editor"]["annotation"]["objects"], 1);
        assert!(st["captures"]["overlay"].is_null());
        assert_eq!(a.pins.info(id).map(|i| i.editing), Some(true));
        assert!(a.shell.windows.log.borrow().visible.contains(&(id, false)));
        // A capture while editing is ignored (one overlay at a time).
        a.action("capture.region", None);
        assert_eq!(a.status()["last_action"]["result"], "ignored");
        // Draw a line in the editor and commit with Enter.
        choose(&mut a, annotation::Tool::Line);
        drag(&mut a, (-450, 150), (-350, 150));
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "committed");
        assert_eq!(a.pins.annotations(id).unwrap().len(), 2);
        assert_eq!(a.pins.info(id).map(|i| i.editing), Some(false));
        assert_eq!(
            a.shell.windows.log.borrow().visible.last(),
            Some(&(id, true))
        );
        // Cancel is lossless: a drawing made and cancelled leaves the pin as it was.
        let before = a.pins.annotations(id).unwrap().to_vec();
        a.action("pin.annotate", Some(id));
        choose(&mut a, annotation::Tool::Pen);
        drag(&mut a, (-450, 180), (-350, 190));
        key(&mut a, "Escape", false, false); // leaves the tool
        key(&mut a, "Escape", false, false); // cancels the edit
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "cancelled");
        assert_eq!(a.pins.annotations(id).unwrap(), before.as_slice());
        assert!(a.pins.picture(id).unwrap().pixels() == pattern(sel).as_slice());
        // Crop: Shift+Left ten times keeps 390 pixels; Done commits.
        a.action("pin.annotate", Some(id));
        for _ in 0..10 {
            key(&mut a, "Left", false, true);
        }
        click_button(&mut a, overlay::ToolbarItem::Done);
        assert!(!a.overlay_open());
        let info = a.pins.info(id).unwrap();
        assert_eq!(info.image, (390, 200));
        assert_eq!(a.pins.annotations(id).unwrap().len(), 2, "both lie inside");
        assert_eq!(a.ledger.live().0, 1, "only the pin's picture is alive");
        // Closing a pin while it is edited ends the edit without committing.
        a.action("pin.annotate", Some(id));
        a.action("pin.close", Some(id));
        assert!(!a.overlay_open());
        assert!(!a.pins.contains(id));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_label_suspends_shortcuts_takes_characters_and_the_ime() {
        let (mut a, _syn, dir) = overlay_app("ann-text");
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        choose(&mut a, annotation::Tool::Text);
        press_at(&mut a, Button::Left, -550, 60, Mods::NONE, 0);
        up(&mut a, -550, 60);
        let st = checked_status(&a);
        assert_eq!(
            st["captures"]["overlay"]["annotation"]["editing_text"],
            true
        );
        assert!(
            a.shell
                .overlays
                .log
                .borrow()
                .ime
                .last()
                .is_some_and(Option::is_some),
            "the input method is switched on for the window at the caret"
        );
        // `E` (detect.cycle) does not fire while typing: it is a character of the label.
        let mode = st["captures"]["overlay"]["detection"]["mode"].clone();
        key(&mut a, "E", false, false);
        a.overlay_event(OverlayEvent::Char('e', false));
        a.overlay_event(OverlayEvent::Char('[', false));
        assert_eq!(a.status()["captures"]["overlay"]["detection"]["mode"], mode);
        // A Korean composition (what WM_IME_COMPOSITION reports).
        a.overlay_event(OverlayEvent::Ime(Ime::Start));
        a.overlay_event(OverlayEvent::Ime(Ime::Compose("\u{d55c}".into())));
        assert_eq!(
            a.status()["captures"]["overlay"]["annotation"]["composing"],
            true
        );
        a.overlay_event(OverlayEvent::Ime(Ime::Commit("\u{d55c}".into())));
        a.overlay_event(OverlayEvent::Ime(Ime::End));
        key(&mut a, "Escape", false, false);
        let st = checked_status(&a);
        assert_eq!(
            st["captures"]["overlay"]["annotation"]["editing_text"],
            false
        );
        assert_eq!(a.shell.overlays.log.borrow().ime.last(), Some(&None));
        let text = match &a.overlay.as_ref().unwrap().model.annotations()[0].shape {
            annotation::Shape::Text(t) => t.text.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(text, "e[\u{d55c}");
        assert!(
            !serde_json::to_string(&st).unwrap().contains('\u{d55c}'),
            "the status never carries the label's text"
        );
        // Not typing: a letter's character never fires its key's binding a second time
        // (`N` selects the next annotation on its key press only).
        a.overlay_event(OverlayEvent::Char('n', false));
        assert!(a.status()["captures"]["overlay"]["annotation"]["selected"].is_null());
        // `[` and `]` are the width keys (char: bindings).
        a.overlay_event(OverlayEvent::Char('[', false));
        assert_eq!(a.status()["captures"]["overlay"]["annotation"]["width"], 2);
        a.overlay_event(OverlayEvent::Char(']', false));
        a.overlay_event(OverlayEvent::Char(']', true));
        assert_eq!(a.status()["captures"]["overlay"]["annotation"]["width"], 4);
        // Ctrl+Z removes the whole label in one step.
        key(&mut a, "Z", true, false);
        assert_eq!(
            a.status()["captures"]["overlay"]["annotation"]["objects"],
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keyboard_only_annotation_reaches_a_pin_through_the_core() {
        // J5 with keys only: Tab to the rectangle tool, Enter, Space, D five times, S three
        // times, Space, Enter.
        let (mut a, _syn, dir) = overlay_app("ann-keys");
        a.action("capture.region", None);
        key(&mut a, "A", true, false); // the monitor under the pointer
        let sel = a.overlay.as_ref().unwrap().model.selection().unwrap();
        for _ in 0..4 {
            key(&mut a, "Tab", false, false);
        }
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "rectangle");
        key(&mut a, "Enter", false, false);
        assert_eq!(a.status()["captures"]["overlay"]["focus"], "canvas");
        key(&mut a, "Space", false, false);
        let code = |k: &str| crate::hotkeys::chord_of(k, false, false, false);
        for (name, n) in [("D", 5), ("S", 3)] {
            for _ in 0..n {
                a.overlay_event(OverlayEvent::Key(
                    code(name).unwrap(),
                    code(&format!("code:Key{name}")),
                    false,
                ));
            }
        }
        key(&mut a, "Space", false, false);
        let st = checked_status(&a);
        assert_eq!(st["captures"]["overlay"]["annotation"]["objects"], 1);
        key(&mut a, "Enter", false, false);
        assert_eq!(a.pins.len(), 1);
        let id = a.pins.ids()[0];
        assert_eq!(a.pins.annotations(id).unwrap().len(), 1);
        assert_eq!(a.pins.info(id).unwrap().image, (sel.width, sel.height));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ----- Step 57: the privacy tools through the instance core (NOEUL-FR-082) -----

    /// The solid redaction's default fill (the palette's black), as BGRA.
    const FILL: [u8; 4] = [0x11, 0x11, 0x11, 0xFF];

    /// The chunk types of a PNG, in order.
    fn png_chunks(bytes: &[u8]) -> Vec<String> {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let mut out = Vec::new();
        let mut i = 8;
        while i + 8 <= bytes.len() {
            let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            out.push(String::from_utf8_lossy(&bytes[i + 4..i + 8]).into_owned());
            i += 12 + len;
        }
        assert_eq!(i, bytes.len(), "nothing after IEND");
        out
    }

    /// Every pixel of `r` in an output covering `sel` is the fill: not one value of the picture
    /// under the redaction survives.
    fn all_filled(px: &[u8], sel: Rect, r: Rect) {
        for y in r.y..r.y + r.height as i32 {
            for x in r.x..r.x + r.width as i32 {
                assert_eq!(px_of(px, sel, x, y), FILL, "({x}, {y}) under the redaction");
            }
        }
    }

    /// The pixels of a packed `CF_DIBV5` (bottom-up BGRA rows after the 124-byte header).
    fn dib_bgra(dib: &[u8], w: u32, h: u32) -> Vec<u8> {
        let rows = &dib[124..];
        let stride = w as usize * 4;
        let mut out = Vec::with_capacity(rows.len());
        for row in (0..h as usize).rev() {
            out.extend_from_slice(&rows[row * stride..row * stride + stride]);
        }
        out
    }

    #[test]
    fn a_solid_redaction_is_shown_copied_and_saved_without_a_pixel_of_the_original() {
        let (mut a, _syn, dir) = overlay_app("priv-out");
        let q = dir.join("q");
        set_config(
            &mut a,
            &[("capture.quick_save_folder", &q.display().to_string())],
        );
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        let sel = Rect::new(-600, 20, 400, 200);
        choose(&mut a, annotation::Tool::Redact);
        drag(&mut a, (-580, 100), (-500, 140));
        let red = Rect::new(-580, 100, 81, 41);
        // Mosaic and blur next to it, the mosaic over its corner (they read the redacted picture).
        choose(&mut a, annotation::Tool::Mosaic);
        drag(&mut a, (-520, 120), (-430, 200));
        choose(&mut a, annotation::Tool::Blur);
        drag(&mut a, (-400, 60), (-300, 150));
        let st = checked_status(&a);
        let ann = &st["captures"]["overlay"]["annotation"];
        assert_eq!(
            (
                ann["kinds"]["redact"].as_u64(),
                ann["kinds"]["mosaic"].as_u64()
            ),
            (Some(1), Some(1))
        );
        assert_eq!(
            (ann["tool"].as_str(), ann["color"].as_str()),
            (Some("blur"), Some("red"))
        );
        // The chip says that blur is weak protection.
        let chip = a.overlay.as_ref().unwrap().scene.chip.clone().unwrap();
        assert!(
            chip.lines.iter().any(|l| l.contains("weak protection")),
            "{:?}",
            chip.lines
        );
        let pic = a.overlay.as_ref().unwrap().model.picture();
        let shown = |x: i32, y: i32| !chip.rect.contains(&Rect::new(x, y, 1, 1));
        let screen: Vec<[u8; 4]> = {
            let log = a.shell.overlays.log.borrow();
            // The mosaic covers the redaction's corner from (-520, 120) on; the rest is fill.
            all_filled(&log.buffer, pic, Rect::new(-580, 100, 60, 20));
            (sel.y..sel.y + 200)
                .flat_map(|y| (sel.x..sel.x + 400).map(move |x| (x, y)))
                .filter(|(x, y)| shown(*x, *y))
                .map(|(x, y)| px_of(&log.buffer, pic, x, y))
                .collect()
        };
        // Copy (the in-memory clipboard stand-in): PNG and CF_DIBV5 hold the fill and exactly
        // what the overlay showed; the PNG has no chunk beyond IHDR, IDAT and IEND.
        key(&mut a, "C", true, false);
        assert!(!a.overlay_open());
        let (png, dib) = a.shell.copies[0].clone();
        let (w, h, px) = png_bgra(&png);
        assert_eq!((w, h), (400, 200));
        let exported: Vec<[u8; 4]> = (sel.y..sel.y + 200)
            .flat_map(|y| (sel.x..sel.x + 400).map(move |x| (x, y)))
            .filter(|(x, y)| shown(*x, *y))
            .map(|(x, y)| px_of(&px, sel, x, y))
            .collect();
        assert!(exported == screen, "the copy is what the overlay showed");
        all_filled(&px, sel, Rect::new(-580, 100, 60, 20));
        // Where the mosaic lies over the redaction, nothing of the original either.
        for y in 120..141 {
            for x in -520..-499 {
                assert_ne!(px_of(&px, sel, x, y), Synthetic::pixel(x, y), "({x}, {y})");
            }
        }
        let mut chunks = png_chunks(&png);
        chunks.dedup();
        assert_eq!(chunks, ["IHDR", "IDAT", "IEND"], "no ancillary chunk");
        all_filled(&dib_bgra(&dib, 400, 200), sel, Rect::new(-580, 100, 60, 20));
        // Quick save: one file in the folder, no temporary file anywhere in the sandbox.
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        choose(&mut a, annotation::Tool::Redact);
        drag(&mut a, (-580, 100), (-500, 140));
        key(&mut a, "S", true, true);
        assert_eq!(a.status()["last_action"]["result"], "saved");
        let files = files_under(&dir);
        let pngs: Vec<&String> = files.iter().filter(|f| f.ends_with(".png")).collect();
        assert_eq!(pngs.len(), 1, "{files:?}");
        assert!(files.iter().all(|f| !f.ends_with(".tmp")), "{files:?}");
        let bytes = std::fs::read(q.join(pngs[0])).unwrap();
        let mut chunks = png_chunks(&bytes);
        chunks.dedup();
        assert_eq!(chunks, ["IHDR", "IDAT", "IEND"]);
        let (_, _, px) = png_bgra(&bytes);
        all_filled(&px, sel, red);
        assert_eq!(
            px_of(&px, sel, -581, 100),
            Synthetic::pixel(-581, 100),
            "next to it"
        );
        assert_eq!(
            a.ledger.live().0,
            0,
            "the pictures under the redactions are freed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_export_keeps_every_redaction_editable_and_leaves_no_file() {
        let (mut a, _syn, dir) = overlay_app("priv-fail");
        std::fs::create_dir_all(&dir).unwrap();
        // The quick-save "folder" is a file: saving fails.
        let blocked = dir.join("not-a-folder");
        std::fs::write(&blocked, b"x").unwrap();
        set_config(
            &mut a,
            &[("capture.quick_save_folder", &blocked.display().to_string())],
        );
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        let sel = Rect::new(-600, 20, 400, 200);
        choose(&mut a, annotation::Tool::Redact);
        drag(&mut a, (-580, 100), (-500, 140));
        let objects = a.overlay.as_ref().unwrap().model.annotations().to_vec();
        key(&mut a, "S", true, true);
        assert_eq!(a.status()["last_action"]["result"], "failed");
        assert!(a.overlay_open(), "the work is kept");
        assert_eq!(
            a.overlay.as_ref().unwrap().model.annotations(),
            objects.as_slice()
        );
        // A busy clipboard: nothing copied, the work kept too.
        a.shell.clipboard_busy = true;
        key(&mut a, "C", true, false);
        assert_eq!(a.status()["last_action"]["result"], "clipboard_unavailable");
        assert!(a.shell.copies.is_empty() && a.overlay_open());
        // Before an output only the user restores the original: undo, then redo.
        key(&mut a, "Z", true, false);
        assert_eq!(
            checked_status(&a)["captures"]["overlay"]["annotation"]["kinds"]["redact"],
            0
        );
        {
            let pic = a.overlay.as_ref().unwrap().model.picture();
            let log = a.shell.overlays.log.borrow();
            assert_eq!(
                px_of(&log.buffer, pic, -560, 120),
                Synthetic::pixel(-560, 120)
            );
        }
        key(&mut a, "Y", true, false);
        assert_eq!(
            a.overlay.as_ref().unwrap().model.annotations(),
            objects.as_slice()
        );
        // Nothing was left behind by the failures; the retry outputs the redacted picture.
        let files = files_under(&dir);
        assert!(
            files
                .iter()
                .all(|f| !f.ends_with(".png") && !f.ends_with(".tmp")),
            "{files:?}"
        );
        a.shell.clipboard_busy = false;
        key(&mut a, "C", true, false);
        assert!(!a.overlay_open());
        let (_, _, px) = png_bgra(&a.shell.copies[0].0);
        all_filled(&px, sel, Rect::new(-580, 100, 81, 41));
        assert_eq!(a.ledger.live().0, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pin_keeps_its_redactions_editable_and_every_output_of_it_is_redacted() {
        let (mut a, _syn, dir) = overlay_app("priv-pin");
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        let sel = Rect::new(-600, 20, 400, 200);
        choose(&mut a, annotation::Tool::Redact);
        drag(&mut a, (-580, 100), (-500, 140));
        let red = Rect::new(-580, 100, 81, 41);
        key(&mut a, "Enter", false, false);
        let id = PinId(1);
        // The pin's window and its copy show the fill; the picture under it is kept in memory,
        // unchanged, so the user can still take the redaction away before an output.
        let shown = a.pins.rendered(id).unwrap();
        all_filled(shown.pixels(), shown.rect(), red);
        drop(shown);
        assert!(a.pins.picture(id).unwrap().pixels() == pattern(sel).as_slice());
        a.action("pin.copy", Some(id));
        let (_, _, px) = png_bgra(&a.shell.copies[0].0);
        all_filled(&px, sel, red);
        // In the pin editor: N selects it, Delete removes it, Escape cancels: still redacted.
        a.action("pin.annotate", Some(id));
        key(&mut a, "N", false, false);
        key(&mut a, "Delete", false, false);
        assert_eq!(
            checked_status(&a)["pins"]["editor"]["annotation"]["kinds"]["redact"],
            0
        );
        key(&mut a, "Escape", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.pins.annotations(id).unwrap().len(), 1);
        let shown = a.pins.rendered(id).unwrap();
        all_filled(shown.pixels(), shown.rect(), red);
        drop(shown);
        // Deleted and committed (Enter): the user's own choice brings the picture back.
        a.action("pin.annotate", Some(id));
        key(&mut a, "N", false, false);
        key(&mut a, "Delete", false, false);
        key(&mut a, "Enter", false, false);
        assert!(a.pins.annotations(id).unwrap().is_empty());
        a.action("pin.copy", Some(id));
        let (_, _, px) = png_bgra(&a.shell.copies[1].0);
        assert_eq!(px_of(&px, sel, -560, 120), Synthetic::pixel(-560, 120));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn privacy_effects_are_processed_only_when_an_edit_asks_never_while_idle() {
        let (mut a, _syn, dir) = overlay_app("priv-idle");
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        choose(&mut a, annotation::Tool::Blur);
        let before = annotation::effect_passes();
        drag(&mut a, (-580, 40), (-400, 180));
        let drawn = annotation::effect_passes();
        assert!(drawn > before, "drawing renders the blur (the preview)");
        // Idle redraws, pointer moves over the canvas and status requests process nothing.
        for i in 0..5 {
            a.redraw_overlay();
            ev(
                &mut a,
                OverlayInput::Move {
                    x: -300 + i,
                    y: 100,
                    mods: Mods::NONE,
                },
            );
            let _ = a.status();
        }
        assert_eq!(
            annotation::effect_passes(),
            drawn,
            "no image processing while idle"
        );
        // A stronger blur of the selected object is one more render.
        key(&mut a, "N", false, false);
        a.overlay_event(OverlayEvent::Char(']', false));
        assert!(annotation::effect_passes() > drawn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_privacy_tool_works_on_floating_pins_in_the_pin_editor() {
        let (mut a, _syn, dir) = overlay_app("priv-pinedit");
        // A pin of the frozen pixels, then the four tools in its editor.
        a.action("capture.region", None);
        drag(&mut a, (-600, 20), (-201, 219));
        key(&mut a, "Enter", false, false);
        let id = PinId(1);
        let sel = Rect::new(-600, 20, 400, 200);
        a.action("pin.annotate", Some(id));
        assert!(a.overlay_open());
        choose(&mut a, annotation::Tool::Mosaic);
        drag(&mut a, (-580, 60), (-520, 120));
        choose(&mut a, annotation::Tool::Blur);
        drag(&mut a, (-500, 60), (-430, 120));
        choose(&mut a, annotation::Tool::Redact);
        assert_eq!(
            checked_status(&a)["pins"]["editor"]["annotation"]["color"],
            "black"
        );
        drag(&mut a, (-420, 60), (-360, 120));
        choose(&mut a, annotation::Tool::Eraser);
        drag(&mut a, (-560, 70), (-560, 110)); // through the mosaic
        drag(&mut a, (-390, 70), (-390, 110)); // through the redaction
        let st = checked_status(&a);
        let kinds = &st["pins"]["editor"]["annotation"]["kinds"];
        assert_eq!(
            (
                kinds["mosaic"].as_u64(),
                kinds["blur"].as_u64(),
                kinds["redact"].as_u64(),
                kinds["eraser"].as_u64()
            ),
            (Some(1), Some(1), Some(1), Some(2))
        );
        key(&mut a, "Enter", false, false);
        assert!(!a.overlay_open());
        assert_eq!(a.status()["last_action"]["result"], "committed");
        assert_eq!(a.pins.annotations(id).unwrap().len(), 5);
        // The pin's window and its copy show exactly the same pixels.
        let shown = a.pins.rendered(id).unwrap();
        let (r, px) = (shown.rect(), shown.pixels().to_vec());
        drop(shown);
        a.action("pin.copy", Some(id));
        let (_, _, copied) = png_bgra(&a.shell.copies[0].0);
        assert!(copied == px, "the copy is what the pin shows");
        assert_eq!(
            px_of(&px, r, -560, 90),
            Synthetic::pixel(-560, 90),
            "erased back"
        );
        assert_ne!(
            px_of(&px, r, -530, 90),
            Synthetic::pixel(-530, 90),
            "mosaic"
        );
        let blurred = (60..121)
            .flat_map(|y| (-500..-429).map(move |x| (x, y)))
            .filter(|(x, y)| px_of(&px, r, *x, *y) != Synthetic::pixel(*x, *y))
            .count();
        assert!(blurred > 100, "the blur changed its pixels: {blurred}");
        all_filled(&px, r, Rect::new(-420, 60, 61, 61));
        assert_eq!(r, sel, "the pin did not move");
        // A pin of the clipboard (a floating image) takes a redaction the same way.
        a.shell.clipboard = Some((40, 30));
        a.action("paste.float", None);
        let clip = a.pins.ids().into_iter().find(|p| *p != id).unwrap();
        let at = a.pins.info(clip).unwrap().rect;
        a.action("pin.annotate", Some(clip));
        choose(&mut a, annotation::Tool::Redact);
        drag(&mut a, (at.x + 5, at.y + 5), (at.x + 24, at.y + 14));
        key(&mut a, "Enter", false, false);
        let shown = a.pins.rendered(clip).unwrap();
        all_filled(
            shown.pixels(),
            shown.rect(),
            Rect::new(at.x + 5, at.y + 5, 20, 10),
        );
        assert_eq!(
            px_of(shown.pixels(), shown.rect(), at.x + 30, at.y + 20),
            [1, 1, 1, 255]
        );
        drop(shown);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
