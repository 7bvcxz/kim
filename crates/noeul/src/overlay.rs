//! The region-selection overlay's model: its state machine, geometry, DPI math and pixels
//! (NOEUL-FR-010…014, NOEUL-FR-042, NOEUL-FR-090, NOEUL-NFR-012; NOEUL_UX_SPEC §4; ADR 0026).
//!
//! Everything here is platform-independent and runs headless: [`Overlay`] is fed [`Input`]s in
//! virtual-desktop device pixels and answers with an [`Outcome`]; [`Overlay::scene`] says what
//! the overlay shows, [`dirty`] which pixels changed between two scenes, and [`compose`] writes
//! those pixels of a [`Scene`] over the frozen picture into a back buffer. The Win32 window
//! (`win::overlay`) only turns messages into inputs, draws the labels' text and copies the back
//! buffer to the screen; tests drive the same code with synthetic monitor layouts.
//!
//! States (UX spec §4.1; Step 50 the selection, Step 51 the outputs):
//!
//! ```text
//!   Hover ──left press──► Pressed ──moved beyond the drag threshold──► Dragging ──release──► Selected
//!     ▲                     │ (released in place: a click; nothing yet)                     │
//!     │                     ▼                                                               │
//!     └── right-click outside the selection ◄── Selected ──press inside/on a handle──► Pressed
//!                                                  ▲         ──beyond threshold──► Moving / Resizing
//!                                                  └──────────────── release ─────────────────┘
//!   Esc (any state), right-click without a selection, another program activated → Cancel
//!   With a confirmed selection: double-click or middle-click inside, Enter, the toolbar or the
//!   context menu → Output(Pin | Copy | Save | QuickSave, rect, via)
//! ```
//!
//! A press that waits (the pointer pauses with the button held) stays `Pressed` for as long as
//! it likes: the drag starts from the press point as soon as the pointer moves beyond the
//! user's drag threshold, whatever the pause. No timer exists (NOEUL-NFR-001).
//!
//! **The double-click gesture** (Step 51, UX spec §4.2 rules 1–5 and 13, NOEUL-FR-020/021/024).
//! Noeul counts presses itself from the raw button messages with the user's system values (the
//! window class has no `CS_DBLCLKS`): a left press P1 in the [interior](Zone::Interior) of the
//! confirmed selection, with no modifier, released before the pointer left the drag rectangle,
//! followed by a second plain left press P2 in the interior, at most the double-click time after
//! P1 and inside the double-click rectangle centered on P1. The output fires on P2's *press* (so
//! the pin appears at once) and does what the `[gestures] double_click` setting says (pin by
//! default). A single click inside a selection does nothing, so a click never has to wait to be
//! told apart from a double-click. A press on a handle, the border band, the toolbar or the size
//! chip, a press that became a drag, move or resize, a press outside, a press with `Ctrl`,
//! `Shift`, `Alt` or `Win`, and the release that ends a drag never count. The owner refreshes
//! the system values before every press ([`Overlay::set_system_values`]), so a change of the
//! Windows double-click speed applies from the next press.
//!
//! **The toolbar** (UX spec §3.1, §5): Pin (first, primary), Copy, Save, Cancel next to the
//! confirmed selection, below it, above it when there is no room, inside its bottom edge when it
//! fills the monitor, never over the size chip. A button runs on release over itself
//! (`Shift`+Save quick-saves). `Tab`/`Shift+Tab` move the keyboard focus between the canvas and
//! the buttons, the arrows between the buttons; while a button has focus the navigation keys go
//! to it first ([`Nav`]), so `Enter` on Copy copies (ADR 0021 §5); `Escape` there cancels, as
//! everywhere.
//!
//! **Smart selection and pixel precision** (Step 52, UX spec §4.8, ADR 0035, NOEUL-FR-040…043,
//! NOEUL-FR-050):
//!
//! - *Detection.* While nothing is selected and no button is held, the window under the pointer
//!   (or, after `detect.cycle` (`E`), the UI element; again, nothing) is shown undimmed with the
//!   selection's border, and a click without a drag — or `selection.mark` (`Space`) — selects it.
//!   The windows come from a [`Snapshot`] taken with the frozen picture; elements are asked for
//!   with [`Overlay::take_element_request`] and answered with [`Input::Element`]. A window that is
//!   protected from capture is never highlighted (a note says Windows shows it black); anything
//!   that cannot be detected leaves the plain region selection.
//! - *Keyboard pointer.* `pointer.move-*` (`W A S D`, physical keys) move the pointer one device
//!   pixel, also during a drag or a resize; the owner moves the real cursor along
//!   ([`Overlay::take_warp`]). While the pointer was last moved by the keyboard, detection and
//!   snapping pause, so a corner lands exactly where the keys put it. `selection.mark` sets the
//!   first corner at the pointer, then the opposite one (`Marking`).
//! - *Acceleration.* Held keys ([`Input::Repeat`]) move 1, then 2, 5 and 10 pixels per repeat
//!   ([`accel`]).
//! - *Snapping.* While dragging or resizing with the mouse, a moving edge within 6 logical pixels
//!   of a monitor edge or a detected window's edge snaps to it; `Alt` turns it off for that move.
//! - *Magnifier.* Near the pointer: [`MAG_CELLS`]² pixels of the frozen picture enlarged, a centre
//!   crosshair, the device-pixel position (and the logical one on a scaled monitor) and the
//!   pixel's colour as HEX and RGB ([`Scene::sample`]). `magnifier.toggle` (`M`) shows or hides it.
//! - *History.* `history.previous` / `history.next` (`Comma` / `Period`) step through the
//!   rectangles of earlier successful captures over the current picture (memory only).
//!
//! **Annotation** (Step 56, UX spec §4.9, ADR 0038, NOEUL-FR-080…083): the toolbar gains the
//! tools (rectangle, ellipse, line, arrow, pen, marker, text), the colour and width buttons, undo
//! and redo. While a tool is active (phase `annotating`) presses inside the selection draw, pick
//! or edit objects ([`crate::annotation::Editor`]) and never pin (a double-click on a label
//! selects a word); `Escape` leaves the tool and keeps the drawing; `Enter` and Pin still pin, with
//! the annotations. The overlay shows the annotated pixels its owner renders
//! ([`crate::annotation::draw`] over the frozen picture, the `layer` of [`compose`]) and asks for
//! the label's input method while one is edited. [`Mode::PinEdit`] runs the same model over a
//! pin's picture: the selection is the pin, resizing it crops, `Enter` or Done commits, `Escape`
//! cancels, and losing focus keeps the work.
//!
//! Lengths of Noeul's own drawing are *logical* pixels scaled by the DPI of the monitor they are
//! drawn on (handles 7, handle zones 9, border band 3, small-selection limit 27, toolbar 32
//! high, magnifier cells 8, snapping 6); every selection, readout and pin rectangle is in
//! *device* pixels (NOEUL-FR-090).

use crate::annotation::{self, Editor, Ime, Object, Order, TextKey, Tool, Typesetter};
use crate::capture::{Monitor, scale_px};
use crate::detect::{
    DetectMode, DetectWindow, ElementAnswer, ElementRequest, Fallback, Snapshot, check_element,
};
use crate::dpi;
use crate::frame::{Frame, Rect};
use std::cell::Cell;

/// A pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Modifier keys held during a pointer event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    /// Either Windows key.
    pub win: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
        win: false,
    };
    pub const SHIFT: Mods = Mods {
        ctrl: false,
        shift: true,
        alt: false,
        win: false,
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        shift: false,
        alt: false,
        win: false,
    };
    pub const ALT: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: true,
        win: false,
    };
    pub const WIN: Mods = Mods {
        ctrl: false,
        shift: false,
        alt: false,
        win: true,
    };

    /// Whether any modifier is held (a press with one never counts as a click, rule 2).
    pub fn any(self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }
}

/// The user's Windows pointer settings, read before every press, never Noeul's own constants
/// (NOEUL-NFR-012, UX spec §4.2 rule 13). Defaults are Windows' defaults (*documented*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemValues {
    /// `SM_CXDRAG` × `SM_CYDRAG`: the drag rectangle centered on a press.
    pub drag: (u32, u32),
    /// `GetDoubleClickTime` in ms: the most time between the two presses of a double-click.
    pub double_click_ms: u32,
    /// `SM_CXDOUBLECLK` × `SM_CYDOUBLECLK`: the rectangle centered on the first press that the
    /// second must land in.
    pub double_click: (u32, u32),
}

/// What a pointer gesture inside a confirmed selection does: the `[gestures]` settings
/// (NOEUL-FR-024; the middle-click offers `pin`, `copy` and `none`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureAction {
    Pin,
    Copy,
    /// The save dialog, as `capture.save` (`Ctrl+S`).
    Save,
    /// Nothing: the press is an ordinary press.
    None,
}

impl GestureAction {
    pub fn as_str(self) -> &'static str {
        match self {
            GestureAction::Pin => "pin",
            GestureAction::Copy => "copy",
            GestureAction::Save => "save",
            GestureAction::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<GestureAction> {
        [
            GestureAction::Pin,
            GestureAction::Copy,
            GestureAction::Save,
            GestureAction::None,
        ]
        .into_iter()
        .find(|g| g.as_str() == s)
    }

    fn output(self) -> Option<Output> {
        match self {
            GestureAction::Pin => Some(Output::Pin),
            GestureAction::Copy => Some(Output::Copy),
            GestureAction::Save => Some(Output::Save),
            GestureAction::None => None,
        }
    }
}

/// The gestures of a confirmed selection (`[gestures]`, NOEUL-FR-024). The default pins on
/// both: the signature flow of NOEUL-FR-020.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gestures {
    pub double_click: GestureAction,
    pub middle_click: GestureAction,
}

impl Default for Gestures {
    fn default() -> Gestures {
        Gestures {
            double_click: GestureAction::Pin,
            middle_click: GestureAction::Pin,
        }
    }
}

impl Default for SystemValues {
    fn default() -> SystemValues {
        SystemValues {
            drag: (4, 4),
            double_click_ms: 500,
            double_click: (4, 4),
        }
    }
}

/// A direction of a keyboard step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// The overlay actions this build implements (context `capture-overlay`, UX spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `capture.pin` (`Enter`): pin the confirmed selection.
    Pin,
    /// `capture.copy` (`Ctrl+C`): copy it and close (Step 51).
    Copy,
    /// `capture.save` (`Ctrl+S`): the save dialog (Step 51).
    Save,
    /// `capture.quick-save` (`Ctrl+Shift+S`): save to the quick-save folder (Step 51).
    QuickSave,
    /// `capture.cancel` (`Escape`).
    Cancel,
    /// `selection.full` (`Ctrl+A`): the monitor under the pointer, again all monitors.
    Full,
    /// `selection.last` (`R`): the rectangle of the last successful capture.
    Last,
    /// `selection.move-*` (arrows): one device pixel.
    Move(Dir),
    /// `selection.grow-*` (`Ctrl`+arrows): that edge outwards by one device pixel.
    Grow(Dir),
    /// `selection.shrink-*` (`Shift`+arrows): from the opposite edge by one device pixel.
    Shrink(Dir),
    /// `selection.mark` (`Space`): accept the detected window or element, else mark the first
    /// corner at the pointer, then the opposite corner (Step 52, NOEUL-FR-043).
    Mark,
    /// `pointer.move-*` (`W A S D`): the pointer by one device pixel (Step 52, NOEUL-FR-042).
    Pointer(Dir),
    /// `detect.cycle` (`E`): windows, elements, nothing (Step 52, NOEUL-FR-040).
    DetectCycle,
    /// `magnifier.toggle` (`M`) (Step 52, NOEUL-FR-041).
    Magnifier,
    /// `history.previous` (`Comma`): the rectangle of an earlier capture (Step 52, NOEUL-FR-050).
    HistoryPrevious,
    /// `history.next` (`Period`).
    HistoryNext,
}

/// The action IDs of [`Action`], in manifest order.
pub const ACTION_IDS: &[&str] = &[
    "capture.pin",
    "capture.copy",
    "capture.save",
    "capture.quick-save",
    "capture.cancel",
    "selection.mark",
    "selection.full",
    "selection.last",
    "selection.move-up",
    "selection.move-down",
    "selection.move-left",
    "selection.move-right",
    "selection.grow-up",
    "selection.grow-down",
    "selection.grow-left",
    "selection.grow-right",
    "selection.shrink-up",
    "selection.shrink-down",
    "selection.shrink-left",
    "selection.shrink-right",
    "pointer.move-up",
    "pointer.move-down",
    "pointer.move-left",
    "pointer.move-right",
    "detect.cycle",
    "magnifier.toggle",
    "history.previous",
    "history.next",
];

impl Action {
    pub fn from_id(id: &str) -> Option<Action> {
        let dir = |s: &str| match s {
            "up" => Some(Dir::Up),
            "down" => Some(Dir::Down),
            "left" => Some(Dir::Left),
            "right" => Some(Dir::Right),
            _ => None,
        };
        Some(match id {
            "capture.pin" => Action::Pin,
            "capture.copy" => Action::Copy,
            "capture.save" => Action::Save,
            "capture.quick-save" => Action::QuickSave,
            "capture.cancel" => Action::Cancel,
            "selection.mark" => Action::Mark,
            "selection.full" => Action::Full,
            "selection.last" => Action::Last,
            "detect.cycle" => Action::DetectCycle,
            "magnifier.toggle" => Action::Magnifier,
            "history.previous" => Action::HistoryPrevious,
            "history.next" => Action::HistoryNext,
            _ if id.starts_with("pointer.move-") => {
                Action::Pointer(dir(id.strip_prefix("pointer.move-")?)?)
            }
            _ => {
                let rest = id.strip_prefix("selection.")?;
                let (kind, d) = rest.split_once('-')?;
                let d = dir(d)?;
                match kind {
                    "move" => Action::Move(d),
                    "grow" => Action::Grow(d),
                    "shrink" => Action::Shrink(d),
                    _ => return None,
                }
            }
        })
    }
}

/// One input, in virtual-desktop device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Move {
        x: i32,
        y: i32,
        mods: Mods,
    },
    Press {
        button: Button,
        x: i32,
        y: i32,
        mods: Mods,
        /// The message time in ms (wraps); the double-click gesture compares two of them.
        time_ms: u32,
    },
    Release {
        button: Button,
        x: i32,
        y: i32,
    },
    /// A key press matched to an overlay action (the `capture-overlay` bindings).
    Action(Action),
    /// An auto-repeat of a held key bound with `repeat`: the steps accelerate ([`accel`]).
    Repeat(Action),
    /// UI Automation's answer to an [`ElementRequest`] (Step 52).
    Element {
        request: ElementRequest,
        answer: ElementAnswer,
    },
    /// A navigation key for the toolbar (the owner sends it while a button has focus, and for
    /// `Tab`/`Shift+Tab` from the canvas when no binding uses them).
    Nav(Nav),
    /// `Shift+F10` or the menu key: the selection's context menu.
    MenuKey,
    /// The reserved navigation key `Escape` (`nav.cancel`), `Alt+F4` or a close request: always
    /// leaves the overlay, also when its shortcuts are off (no trapped UI, NOEUL-SEC-013).
    Leave,
    /// Another program's window became active (NOEUL-FR-013).
    FocusLost,
    /// The pointer capture was taken away during a drag.
    CaptureLost,
    /// The mouse wheel (120 per notch, positive away from the user): one width level per notch
    /// while a tool is active or an annotation is selected (Step 56).
    Wheel(i32),
    /// A character typed into the text label being edited (`WM_CHAR`, Step 56).
    Char(char),
    /// An editing key of the text label being edited (Step 56).
    TextKey(TextKey),
    /// An action of the `annotate` context (Step 56).
    Annotate(AnnotateAction),
}

/// The actions of the `annotate` context (UX spec §6; Step 56). Its bindings are matched while
/// the overlay has a confirmed selection (or a pin is edited); `annotate.done` only while there
/// is something to end, so `Escape` otherwise still cancels the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotateAction {
    /// `annotate.done` (`Escape`): end the label, abort the shape being drawn, or leave the tool.
    Done,
    Undo,
    Redo,
    WidthDown,
    WidthUp,
    /// `annotate.rectangle` … `annotate.redact` (no default keys): choose that tool (again: leave
    /// it).
    Tool(Tool),
    /// `annotate.clear-all`: remove every annotation, as one undoable step.
    ClearAll,
    /// `annotate.delete` (`Delete`): the selected annotation.
    Delete,
    /// `annotate.select-next` / `-previous` (`N` / `Shift+N`): select annotations by keyboard.
    SelectNext,
    SelectPrevious,
    /// `annotate.bring-forward` and the like (`PageUp`, `PageDown`, `Shift+PageUp`,
    /// `Shift+PageDown`).
    Order(Order),
}

/// The `annotate` actions this build implements, in manifest order.
pub const ANNOTATE_IDS: &[&str] = &[
    "annotate.done",
    "annotate.undo",
    "annotate.redo",
    "annotate.width-down",
    "annotate.width-up",
    "annotate.rectangle",
    "annotate.ellipse",
    "annotate.line",
    "annotate.arrow",
    "annotate.pen",
    "annotate.marker",
    "annotate.text",
    "annotate.mosaic",
    "annotate.blur",
    "annotate.eraser",
    "annotate.redact",
    "annotate.clear-all",
    "annotate.delete",
    "annotate.select-next",
    "annotate.select-previous",
    "annotate.bring-forward",
    "annotate.send-backward",
    "annotate.bring-to-front",
    "annotate.send-to-back",
];

impl AnnotateAction {
    pub fn from_id(id: &str) -> Option<AnnotateAction> {
        Some(match id {
            "annotate.done" => AnnotateAction::Done,
            "annotate.undo" => AnnotateAction::Undo,
            "annotate.redo" => AnnotateAction::Redo,
            "annotate.width-down" => AnnotateAction::WidthDown,
            "annotate.width-up" => AnnotateAction::WidthUp,
            "annotate.clear-all" => AnnotateAction::ClearAll,
            "annotate.delete" => AnnotateAction::Delete,
            "annotate.select-next" => AnnotateAction::SelectNext,
            "annotate.select-previous" => AnnotateAction::SelectPrevious,
            "annotate.bring-forward" => AnnotateAction::Order(Order::Forward),
            "annotate.send-backward" => AnnotateAction::Order(Order::Backward),
            "annotate.bring-to-front" => AnnotateAction::Order(Order::Front),
            "annotate.send-to-back" => AnnotateAction::Order(Order::Back),
            other => AnnotateAction::Tool(Tool::from_action(other)?),
        })
    }
}

/// What an overlay is for (Step 56).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The capture overlay over the frozen desktop.
    Capture,
    /// Editing a pin's annotations: the canvas holds the pin's picture at 100 % (and room for the
    /// toolbar), the selection is the part the pin keeps (resizing it crops), `Enter` and Done
    /// commit, `Escape` and Cancel return the pin unchanged.
    PinEdit,
}

/// A navigation key while the toolbar may take it (UX spec §5, ADR 0021 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// `Tab`: the next button (from the canvas: the first; after the last: the canvas).
    Next,
    /// `Shift+Tab`.
    Prev,
    /// `Left`/`Up`: the previous button.
    Left,
    /// `Right`/`Down`: the next button.
    Right,
    /// `Enter` or `Space` on a focused button.
    Activate,
    /// `Escape` on a focused button: cancels the capture, as everywhere in the overlay.
    Escape,
}

/// An output of a confirmed selection (NOEUL-FR-020, NOEUL-FR-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Pin,
    Copy,
    /// The save dialog.
    Save,
    /// The quick-save folder, no dialog.
    QuickSave,
}

impl Output {
    pub fn action_id(self) -> &'static str {
        match self {
            Output::Pin => "capture.pin",
            Output::Copy => "capture.copy",
            Output::Save => "capture.save",
            Output::QuickSave => "capture.quick-save",
        }
    }
}

/// What asked for an output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A key binding, or `Enter`/`Space` on a focused toolbar button.
    Key,
    /// The second press of a double-click inside the selection.
    DoubleClick,
    /// A middle-click inside the selection.
    MiddleClick,
    /// A click on a toolbar button.
    Toolbar,
    /// The selection's context menu.
    Menu,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Key => "key",
            Via::DoubleClick => "double_click",
            Via::MiddleClick => "middle_click",
            Via::Toolbar => "toolbar",
            Via::Menu => "menu",
        }
    }

    /// Whether a mouse gesture made it (its remaining presses may reach a new pin, which then
    /// ignores them for one double-click time, UX spec §4.2 rule 10).
    pub fn pointer(self) -> bool {
        matches!(self, Via::DoubleClick | Via::MiddleClick | Via::Toolbar)
    }
}

/// What the owner of the overlay must do after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing beyond redrawing what changed.
    Stay,
    /// End the capture without pinning, copying or saving anything (NOEUL-FR-012).
    Cancel,
    /// Hand this confirmed selection to an output. The overlay stays `Selected` until the owner
    /// closes it, so a failed output keeps the work (NOEUL-FR-025).
    Output(Output, Rect, Via),
    /// Open the selection's context menu at this point (`None`: from the keyboard, at the
    /// selection's corner). Only with a confirmed selection.
    Menu(Option<(i32, i32)>),
}

/// A button of the selection's toolbar, in tab order (UX spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarItem {
    Pin,
    Copy,
    Save,
    /// A drawing tool (Step 56).
    Tool(Tool),
    /// The colour: the next colour of the palette (`Shift`: the previous one).
    Color,
    /// The width: one level thicker, from the thickest back to the thinnest (`Shift`: thinner).
    Width,
    Undo,
    Redo,
    /// Commits a pin's edit (pin editing only; primary).
    Done,
    Cancel,
}

/// The capture overlay's toolbar (UX spec §3.1): Pin (primary), Copy, Save, the tools (the
/// privacy tools mosaic, blur, eraser and redaction since Step 57), colour, width, Undo, Redo,
/// Cancel.
pub const TOOLBAR: [ToolbarItem; 19] = [
    ToolbarItem::Pin,
    ToolbarItem::Copy,
    ToolbarItem::Save,
    ToolbarItem::Tool(Tool::Rectangle),
    ToolbarItem::Tool(Tool::Ellipse),
    ToolbarItem::Tool(Tool::Line),
    ToolbarItem::Tool(Tool::Arrow),
    ToolbarItem::Tool(Tool::Pen),
    ToolbarItem::Tool(Tool::Marker),
    ToolbarItem::Tool(Tool::Text),
    ToolbarItem::Tool(Tool::Mosaic),
    ToolbarItem::Tool(Tool::Blur),
    ToolbarItem::Tool(Tool::Eraser),
    ToolbarItem::Tool(Tool::Redact),
    ToolbarItem::Color,
    ToolbarItem::Width,
    ToolbarItem::Undo,
    ToolbarItem::Redo,
    ToolbarItem::Cancel,
];

/// The pin editor's toolbar: Done (primary), the tools, colour, width, Undo, Redo, Cancel.
pub const EDIT_TOOLBAR: [ToolbarItem; 17] = [
    ToolbarItem::Done,
    ToolbarItem::Tool(Tool::Rectangle),
    ToolbarItem::Tool(Tool::Ellipse),
    ToolbarItem::Tool(Tool::Line),
    ToolbarItem::Tool(Tool::Arrow),
    ToolbarItem::Tool(Tool::Pen),
    ToolbarItem::Tool(Tool::Marker),
    ToolbarItem::Tool(Tool::Text),
    ToolbarItem::Tool(Tool::Mosaic),
    ToolbarItem::Tool(Tool::Blur),
    ToolbarItem::Tool(Tool::Eraser),
    ToolbarItem::Tool(Tool::Redact),
    ToolbarItem::Color,
    ToolbarItem::Width,
    ToolbarItem::Undo,
    ToolbarItem::Redo,
    ToolbarItem::Cancel,
];

impl ToolbarItem {
    /// The text of a text button; icon buttons have none (their icon is drawn by [`compose`]).
    pub fn label(self) -> &'static str {
        match self {
            ToolbarItem::Pin => "Pin",
            ToolbarItem::Copy => "Copy",
            ToolbarItem::Save => "Save",
            ToolbarItem::Done => "Done",
            ToolbarItem::Cancel => "Cancel",
            _ => "",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ToolbarItem::Pin => "pin",
            ToolbarItem::Copy => "copy",
            ToolbarItem::Save => "save",
            ToolbarItem::Tool(t) => t.as_str(),
            ToolbarItem::Color => "color",
            ToolbarItem::Width => "width",
            ToolbarItem::Undo => "undo",
            ToolbarItem::Redo => "redo",
            ToolbarItem::Done => "done",
            ToolbarItem::Cancel => "cancel",
        }
    }

    /// Its name for people (tooltips, the docs and UI Automation in N5).
    pub fn title(self) -> &'static str {
        match self {
            ToolbarItem::Pin => "Pin",
            ToolbarItem::Copy => "Copy",
            ToolbarItem::Save => "Save",
            ToolbarItem::Tool(t) => t.title(),
            ToolbarItem::Color => "Colour",
            ToolbarItem::Width => "Line width",
            ToolbarItem::Undo => "Undo",
            ToolbarItem::Redo => "Redo",
            ToolbarItem::Done => "Done",
            ToolbarItem::Cancel => "Cancel",
        }
    }

    /// Width in logical pixels (fixed, so hit-testing needs no text measurement).
    fn width(self) -> u32 {
        match self {
            ToolbarItem::Cancel => 64,
            ToolbarItem::Pin | ToolbarItem::Copy | ToolbarItem::Save | ToolbarItem::Done => 52,
            _ => 28,
        }
    }

    /// Buttons of one group sit 4 logical pixels apart, groups 10.
    fn group(self) -> u8 {
        match self {
            ToolbarItem::Pin | ToolbarItem::Copy | ToolbarItem::Save | ToolbarItem::Done => 0,
            ToolbarItem::Tool(_) => 1,
            ToolbarItem::Color | ToolbarItem::Width => 2,
            ToolbarItem::Undo | ToolbarItem::Redo => 3,
            ToolbarItem::Cancel => 4,
        }
    }
}

/// Which edges a handle or the border band moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

/// Where a point lies relative to a confirmed selection (UX spec §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Outside,
    Interior,
    /// A handle zone or the border band.
    Edge(Edges),
    /// The toolbar: on this button (index into the mode's toolbar), or on the bar between
    /// buttons.
    Toolbar(Option<usize>),
    /// The size chip where it covers the selection (no room above it).
    Chip,
}

/// The overlay's state, as reported by `noeul status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Hover,
    Pressed,
    Dragging,
    Selected,
    Moving,
    Resizing,
    /// The first corner is marked by `selection.mark`; the next mark (or a click) sets the
    /// opposite one (Step 52).
    Marking,
    /// An annotation tool is active, a label is edited or a stroke is drawn (Step 56).
    Annotating,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Hover => "hover",
            Phase::Pressed => "pressed",
            Phase::Dragging => "dragging",
            Phase::Selected => "selected",
            Phase::Moving => "moving",
            Phase::Resizing => "resizing",
            Phase::Marking => "marking",
            Phase::Annotating => "annotating",
        }
    }
}

/// What detection found under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectKind {
    Window,
    Element,
}

impl DetectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DetectKind::Window => "window",
            DetectKind::Element => "element",
        }
    }
}

/// The detected target under the pointer (geometry only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub rect: Rect,
    pub kind: DetectKind,
    /// Protected from capture: shown black, never highlighted or selected.
    pub protected: bool,
}

/// Source pixels per side of the magnifier (odd, so one sits in the centre).
pub const MAG_CELLS: u32 = 15;
/// Logical pixels per enlarged source pixel.
pub const MAG_CELL: u32 = 8;
/// Logical pixels between the pointer and the magnifier.
const MAG_GAP: u32 = 20;
/// Logical pixels within which a moving edge snaps.
pub const SNAP: u32 = 6;
/// How many earlier capture rectangles the overlay steps through.
pub const HISTORY: usize = 20;

/// The step of a held key after `repeats` auto-repeats: 1 device pixel for the first press and
/// nine repeats, then 2, 5 and 10 (NOEUL-FR-042, "with acceleration").
pub fn accel(repeats: u32) -> i32 {
    match repeats {
        0..=9 => 1,
        10..=19 => 2,
        20..=29 => 5,
        _ => 10,
    }
}

/// Hover detection's state for one capture (Step 52).
#[derive(Debug, Clone, Default)]
struct Detection {
    mode: DetectMode,
    snapshot: Snapshot,
    /// The pointer was last moved by the keyboard: no highlight and no snapping until the mouse
    /// moves it again.
    suspended: bool,
    /// The last accepted element: its window's token and rectangle.
    element: Option<(u64, Rect)>,
    /// The last request issued (its window and point are not asked again).
    asked: Option<ElementRequest>,
    seq: u64,
    /// A request for the owner to send.
    pending: Option<ElementRequest>,
    /// Windows that refused or did not answer: not asked again during this capture.
    refused: Vec<u64>,
    /// Why the last lookup fell back to the window rectangle.
    fallback: Option<Fallback>,
}

/// The pointer shape the overlay asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Cross,
    /// Over the toolbar and the size chip.
    Arrow,
    Move,
    /// Left or right edge.
    SizeWe,
    /// Top or bottom edge.
    SizeNs,
    /// Top-left or bottom-right corner.
    SizeNwse,
    /// Top-right or bottom-left corner.
    SizeNesw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hover,
    /// The left button is down and has not moved beyond the drag threshold yet. `click` is the
    /// press time when it may become the first press of a double-click (a plain press in the
    /// interior of a confirmed selection).
    Pressed {
        at: (i32, i32),
        zone: Zone,
        click: Option<u32>,
    },
    Dragging {
        anchor: (i32, i32),
        to: (i32, i32),
        square: bool,
        before: Option<Rect>,
    },
    Selected,
    Moving {
        grab: (i32, i32),
        start: Rect,
    },
    Resizing {
        edges: Edges,
        grab: (i32, i32),
        start: Rect,
    },
    /// `selection.mark` set the first corner; `before` is the selection it replaces.
    Marking {
        anchor: (i32, i32),
        before: Option<Rect>,
    },
    /// The left button draws, moves or resizes an annotation (Step 56).
    Drawing,
}

/// The overlay of one capture session. See the module documentation.
#[derive(Debug, Clone)]
pub struct Overlay {
    picture: Rect,
    monitors: Vec<Monitor>,
    sys: SystemValues,
    gestures: Gestures,
    state: State,
    selection: Option<Rect>,
    pointer: (i32, i32),
    shift: bool,
    /// `Alt` held during the last pointer event: no snapping.
    alt: bool,
    last: Option<Rect>,
    detect: Detection,
    magnifier: bool,
    /// Earlier capture rectangles inside the picture, oldest first, and where `history.*` is.
    history: Vec<Rect>,
    history_at: Option<usize>,
    /// The action of the last key and how many auto-repeats of it came in a row.
    streak: Option<(Action, u32)>,
    /// Where the owner should move the real pointer (a keyboard pointer move).
    warp: Option<(i32, i32)>,
    /// Whether the owner provided a window list (it may be empty or unavailable).
    snapshot_tried: bool,
    help: Option<Vec<String>>,
    help_monitor: usize,
    banner: Option<String>,
    /// The first press of a possible double-click: where and when (released without a drag).
    click: Option<((i32, i32), u32)>,
    /// The toolbar button with keyboard focus (`None`: the canvas).
    bar_focus: Option<usize>,
    /// The button under the pointer.
    bar_hover: Option<usize>,
    /// The button pressed with the left button (and whether `Shift` was held), run on release.
    bar_pressed: Option<(usize, bool)>,
    /// Where the size chip was drawn last (hit-testing follows what the user sees; the scene is
    /// recomputed after every input).
    chip_at: Cell<Option<Rect>>,
    /// What this overlay is for (Step 56).
    mode: Mode,
    /// Where the selection may lie: the picture, or the pin's image in [`Mode::PinEdit`].
    bound: Rect,
    /// The annotations and their editor (Step 56).
    ann: Editor,
    /// The last plain press on the label being edited (a second one selects a word).
    text_click: Option<((i32, i32), u32)>,
    /// Wheel deltas not yet worth a width step.
    wheel: i32,
}

impl Overlay {
    /// An overlay over `picture` (the frozen picture's rectangle, the virtual desktop) with its
    /// monitors. `pointer` is where the pointer is when it opens (it may never move before the
    /// first press); `last` is the rectangle of the last successful capture (`selection.last`);
    /// `help` the first-run keyboard help lines, if shown.
    pub fn new(
        picture: Rect,
        monitors: Vec<Monitor>,
        sys: SystemValues,
        pointer: (i32, i32),
        last: Option<Rect>,
        help: Option<Vec<String>>,
    ) -> Overlay {
        let mut o = Overlay {
            picture,
            monitors,
            sys,
            gestures: Gestures::default(),
            state: State::Hover,
            selection: None,
            pointer: (picture.x, picture.y),
            shift: false,
            alt: false,
            last,
            detect: Detection::default(),
            magnifier: true,
            history: Vec::new(),
            history_at: None,
            streak: None,
            warp: None,
            snapshot_tried: false,
            help,
            help_monitor: 0,
            banner: None,
            click: None,
            bar_focus: None,
            bar_hover: None,
            bar_pressed: None,
            chip_at: Cell::new(None),
            mode: Mode::Capture,
            bound: picture,
            ann: Editor::default(),
            text_click: None,
            wheel: 0,
        };
        o.pointer = o.clamp(pointer);
        o.help_monitor = o.monitor_index(o.pointer);
        o
    }

    /// The same overlay with these gestures (`[gestures]`, NOEUL-FR-024).
    #[must_use]
    pub fn with_gestures(mut self, gestures: Gestures) -> Overlay {
        self.gestures = gestures;
        self
    }

    pub fn gestures(&self) -> Gestures {
        self.gestures
    }

    /// The pin editor (Step 56): an overlay over `canvas` (the pin's picture at 100 % at `pin`, and
    /// room for the toolbar), the selection fixed to `pin` (resizing it crops), with the pin's
    /// `objects` (in canvas pixels) ready to edit. No detection, magnifier, help or history.
    pub fn for_pin(
        canvas: Rect,
        monitors: Vec<Monitor>,
        sys: SystemValues,
        pin: Rect,
        objects: Vec<Object>,
    ) -> Overlay {
        let mut o = Overlay::new(canvas, monitors, sys, (pin.x, pin.y), None, None);
        o.mode = Mode::PinEdit;
        o.bound = intersection(pin, canvas).unwrap_or(canvas);
        o.magnifier = false;
        o.ann = Editor::new(annotation::Doc::with(objects));
        o.set_selection(o.bound);
        o.ann.set_clip(o.bound);
        o
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The buttons of this overlay's toolbar, in tab order.
    pub fn bar(&self) -> &'static [ToolbarItem] {
        match self.mode {
            Mode::Capture => &TOOLBAR,
            Mode::PinEdit => &EDIT_TOOLBAR,
        }
    }

    /// The annotation editor (its document, tool, style and selection; Step 56).
    pub fn editor(&self) -> &Editor {
        &self.ann
    }

    /// The annotations as they are now, in picture pixels (an output takes these).
    pub fn annotations(&self) -> &[Object] {
        self.ann.objects()
    }

    /// The annotation pixels that changed since the last call (the owner re-renders them).
    pub fn take_annotation_dirty(&mut self) -> Option<Rect> {
        self.ann.take_dirty()
    }

    /// Lays out the labels whose text or size changed (after every input, with the platform's
    /// typesetter).
    pub fn typeset(&mut self, ts: &dyn Typesetter) {
        self.ann.typeset(ts);
    }

    /// An input-method event for the label being edited (Step 56, NOEUL-NFR-014).
    pub fn ime(&mut self, e: Ime) -> Outcome {
        self.ann.ime(e);
        Outcome::Stay
    }

    /// Where the caret of the label being edited is, while one is: the owner switches the input
    /// method on for the window and puts its candidate window there (NOEUL-NFR-014).
    pub fn ime_caret(&self) -> Option<Rect> {
        self.ann.text_marks().map(|m| m.caret).or_else(|| {
            // Until the label is typeset: its corner.
            self.ann
                .editing_text()
                .then(|| {
                    self.ann
                        .selected_bounds()
                        .and_then(annotation::Bounds::to_rect)
                })
                .flatten()
        })
    }

    /// Whether the `annotate` context applies to a key: the overlay has a confirmed selection
    /// and no button is held.
    pub fn annotate_context(&self) -> bool {
        self.selection().is_some() && !self.dragging()
    }

    /// Whether `annotate.done` has something to end (a label, a shape being drawn, a tool or a
    /// selected annotation); otherwise `Escape` belongs to the outer context.
    pub fn annotation_active(&self) -> bool {
        self.ann.editing_text()
            || self.ann.busy()
            || self.ann.tool().is_some()
            || self.ann.selected().is_some()
    }

    /// Whether a label is being edited: every key but the editing keys and `Escape` is suspended
    /// (UX spec §6).
    pub fn editing_text(&self) -> bool {
        self.ann.editing_text()
    }

    /// The same overlay with hover detection in `mode` over `snapshot` (`None`: no window list
    /// yet; the owner provides one when [`Overlay::needs_snapshot`] says so) (Step 52).
    #[must_use]
    pub fn with_detection(mut self, mode: DetectMode, snapshot: Option<Snapshot>) -> Overlay {
        self.detect.mode = mode;
        if let Some(s) = snapshot {
            self.set_snapshot(s);
        }
        self
    }

    /// The window list, taken by the owner (with the frozen picture, or when detection was
    /// turned on during the capture).
    pub fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.detect.snapshot = snapshot;
        self.snapshot_tried = true;
    }

    /// Whether detection is on and no window list was taken yet.
    pub fn needs_snapshot(&self) -> bool {
        self.detect.mode != DetectMode::Off && !self.snapshot_tried
    }

    /// The same overlay with the magnifier on or off (`[capture] magnifier`).
    #[must_use]
    pub fn with_magnifier(mut self, on: bool) -> Overlay {
        self.magnifier = on;
        self
    }

    /// The same overlay with the rectangles of earlier captures, oldest first (`history.*`); the
    /// ones outside the picture and repeated ones are left out, at most [`HISTORY`] are kept.
    #[must_use]
    pub fn with_history(mut self, history: &[Rect]) -> Overlay {
        let mut v: Vec<Rect> = Vec::new();
        for r in history
            .iter()
            .filter_map(|r| intersection(*r, self.picture))
        {
            if v.last() != Some(&r) {
                v.push(r);
            }
        }
        let extra = v.len().saturating_sub(HISTORY);
        v.drain(..extra);
        self.history = v;
        self
    }

    pub fn detect_mode(&self) -> DetectMode {
        self.detect.mode
    }

    /// How many windows detection offers in this capture (`None`: no list).
    pub fn detect_windows(&self) -> Option<usize> {
        self.detect
            .snapshot
            .taken
            .then(|| self.detect.snapshot.len())
    }

    /// Why the last element lookup fell back to the window rectangle.
    pub fn detect_fallback(&self) -> Option<Fallback> {
        self.detect.fallback
    }

    /// Whether detection and snapping pause because the keyboard moved the pointer last.
    pub fn detect_suspended(&self) -> bool {
        self.detect.suspended
    }

    pub fn magnifier_enabled(&self) -> bool {
        self.magnifier
    }

    /// `(retained rectangles, position)` of `history.*`.
    pub fn history(&self) -> (usize, Option<usize>) {
        (self.history.len(), self.history_at)
    }

    /// Where the real pointer should go after a keyboard pointer move (taken once).
    pub fn take_warp(&mut self) -> Option<(i32, i32)> {
        self.warp.take()
    }

    /// A UI Automation request for the owner to send (taken once).
    pub fn take_element_request(&mut self) -> Option<ElementRequest> {
        self.detect.pending.take()
    }

    /// The window under the pointer that detection may use now: only while nothing is selected
    /// and no button is held (a press from `Hover` keeps it until its release), detection on,
    /// and the mouse moved the pointer last.
    fn hover_window(&self) -> Option<&DetectWindow> {
        if self.detect.mode == DetectMode::Off
            || self.detect.suspended
            || self.selection.is_some()
            || !matches!(self.state, State::Hover | State::Pressed { .. })
        {
            return None;
        }
        self.detect.snapshot.window_at(self.pointer)
    }

    /// What detection shows under the pointer now.
    pub fn detected(&self) -> Option<Detected> {
        let w = self.hover_window()?;
        if w.protected {
            return Some(Detected {
                rect: w.rect,
                kind: DetectKind::Window,
                protected: true,
            });
        }
        let at = Rect::new(self.pointer.0, self.pointer.1, 1, 1);
        if self.detect.mode == DetectMode::Element
            && let Some((token, r)) = self.detect.element
            && token == w.token
            && r.contains(&at)
        {
            return Some(Detected {
                rect: r,
                kind: DetectKind::Element,
                protected: false,
            });
        }
        Some(Detected {
            rect: w.rect,
            kind: DetectKind::Window,
            protected: false,
        })
    }

    /// The detected target a click or `selection.mark` selects (never a protected window).
    fn selectable(&self) -> Option<Rect> {
        self.detected().filter(|d| !d.protected).map(|d| d.rect)
    }

    /// Issues a UI Automation request when the element under the pointer may have changed.
    fn update_element_request(&mut self) {
        if self.detect.mode != DetectMode::Element {
            return;
        }
        let Some(w) = self.hover_window().copied() else {
            return;
        };
        if w.protected || self.detect.refused.contains(&w.token) {
            return;
        }
        if self
            .detect
            .asked
            .is_some_and(|a| a.token == w.token && a.at == self.pointer)
        {
            return;
        }
        self.detect.seq += 1;
        let req = ElementRequest {
            seq: self.detect.seq,
            token: w.token,
            window: w.rect,
            at: self.pointer,
        };
        self.detect.asked = Some(req);
        self.detect.pending = Some(req);
    }

    /// Takes UI Automation's answer: the current request's, or an older one whose element still
    /// holds the pointer inside the same window.
    fn element_answer(&mut self, request: ElementRequest, answer: ElementAnswer) {
        let current = self.detect.asked.is_some_and(|a| a.seq == request.seq);
        let window = self.hover_window().map(|w| w.token);
        match answer {
            ElementAnswer::Found(r) => match check_element(r, &request, &self.monitors) {
                Some(r) => {
                    let at = Rect::new(self.pointer.0, self.pointer.1, 1, 1);
                    if current || (window == Some(request.token) && r.contains(&at)) {
                        self.detect.element = Some((request.token, r));
                        self.detect.fallback = None;
                    }
                }
                None if current => {
                    self.detect.element = None;
                    self.detect.fallback = Some(Fallback::InvalidRect);
                }
                None => {}
            },
            ElementAnswer::WindowOnly if current => {
                self.detect.element = None;
                self.detect.fallback = None;
            }
            ElementAnswer::WindowOnly => {}
            ElementAnswer::Failed(f) => {
                if f.refuses_window() && !self.detect.refused.contains(&request.token) {
                    self.detect.refused.push(request.token);
                }
                if current {
                    self.detect.element = None;
                    self.detect.fallback = Some(f);
                }
            }
        }
    }

    /// The user's pointer settings as they are now; the owner calls this before every press, so
    /// a changed Windows double-click speed or drag threshold applies from the next press
    /// (UX spec §4.2 rule 13).
    pub fn set_system_values(&mut self, sys: SystemValues) {
        self.sys = sys;
    }

    pub fn system_values(&self) -> SystemValues {
        self.sys
    }

    /// The toolbar button with keyboard focus (`None`: the canvas has it).
    pub fn toolbar_focus(&self) -> Option<ToolbarItem> {
        self.bar_focus.and_then(|i| self.bar().get(i).copied())
    }

    pub fn picture(&self) -> Rect {
        self.picture
    }

    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }

    /// The displays changed while the overlay is open (Step 58): the handles, the border band,
    /// the toolbar, the magnifier and the labels are measured with the new scales from now on;
    /// the selection, the pointer and the annotations (device and picture pixels) stay. The help
    /// stays on the monitor it was on when it still exists.
    pub fn set_monitors(&mut self, monitors: Vec<Monitor>) {
        if self.help_monitor >= monitors.len() {
            self.help_monitor = 0;
        }
        self.monitors = monitors;
    }

    pub fn phase(&self) -> Phase {
        match self.state {
            State::Hover => Phase::Hover,
            State::Pressed { .. } => Phase::Pressed,
            State::Dragging { .. } => Phase::Dragging,
            State::Drawing => Phase::Annotating,
            State::Selected if self.ann.tool().is_some() || self.ann.editing_text() => {
                Phase::Annotating
            }
            State::Selected => Phase::Selected,
            State::Moving { .. } => Phase::Moving,
            State::Resizing { .. } => Phase::Resizing,
            State::Marking { .. } => Phase::Marking,
        }
    }

    /// The confirmed selection (released after a drag, or set by a key).
    pub fn selection(&self) -> Option<Rect> {
        match self.state {
            State::Dragging { .. } | State::Marking { .. } => None,
            _ => self.selection,
        }
    }

    /// The rectangle shown undimmed now: the rubber band while dragging or marking, else the
    /// selection.
    pub fn live(&self) -> Option<Rect> {
        match self.state {
            State::Dragging {
                anchor, to, square, ..
            } => Some(self.band(anchor, to, square)),
            State::Marking { anchor, .. } => Some(self.band(anchor, self.pointer, self.shift)),
            _ => self.selection,
        }
    }

    pub fn pointer(&self) -> (i32, i32) {
        self.pointer
    }

    /// Hides the first-run help (after the first successful capture, ADR 0026).
    pub fn hide_help(&mut self) {
        self.help = None;
    }

    pub fn help_shown(&self) -> bool {
        self.help.is_some()
    }

    /// A one-line message at the top of the overlay (a failed pin, NOEUL-FR-025); `None` clears.
    pub fn set_banner(&mut self, text: Option<String>) {
        self.banner = text;
    }

    fn clamp(&self, (x, y): (i32, i32)) -> (i32, i32) {
        let p = self.picture;
        let max_x = i32::try_from(p.right() - 1).unwrap_or(i32::MAX);
        let max_y = i32::try_from(p.bottom() - 1).unwrap_or(i32::MAX);
        (x.clamp(p.x, max_x.max(p.x)), y.clamp(p.y, max_y.max(p.y)))
    }

    /// The monitor that contains `pt`, or the nearest one.
    fn monitor_index(&self, (x, y): (i32, i32)) -> usize {
        let dist = |m: &Monitor| {
            let r = m.rect;
            let dx = (i64::from(r.x) - i64::from(x))
                .max(i64::from(x) - (r.right() - 1))
                .max(0);
            let dy = (i64::from(r.y) - i64::from(y))
                .max(i64::from(y) - (r.bottom() - 1))
                .max(0);
            dx * dx + dy * dy
        };
        self.monitors
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| dist(m))
            .map_or(0, |(i, _)| i)
    }

    /// The monitor at `pt` (a 100 % stand-in when the layout is empty).
    pub fn monitor_at(&self, pt: (i32, i32)) -> Monitor {
        self.monitors
            .get(self.monitor_index(pt))
            .copied()
            .unwrap_or(Monitor::new(self.picture, true))
    }

    fn beyond_drag(&self, from: (i32, i32), to: (i32, i32)) -> bool {
        let (cx, cy) = self.sys.drag;
        let dx = (i64::from(to.0) - i64::from(from.0)).unsigned_abs();
        let dy = (i64::from(to.1) - i64::from(from.1)).unsigned_abs();
        dx > u64::from(cx / 2) || dy > u64::from(cy / 2)
    }

    /// The rubber band from `anchor` to `to` (both inclusive pixels); `square` keeps the sides
    /// equal (Shift, NOEUL-FR-011) within the picture.
    fn band(&self, anchor: (i32, i32), to: (i32, i32), square: bool) -> Rect {
        let (ax, ay) = (i64::from(anchor.0), i64::from(anchor.1));
        let (mut tx, mut ty) = (i64::from(to.0), i64::from(to.1));
        if square {
            let sx = if tx < ax { -1 } else { 1 };
            let sy = if ty < ay { -1 } else { 1 };
            let d = (tx - ax).abs().max((ty - ay).abs());
            let (cx, cy) = self.clamp((
                i32::try_from(ax + sx * d).unwrap_or(i32::MAX),
                i32::try_from(ay + sy * d).unwrap_or(i32::MAX),
            ));
            let d = (i64::from(cx) - ax).abs().min((i64::from(cy) - ay).abs());
            tx = ax + sx * d;
            ty = ay + sy * d;
        }
        let (mut l, mut t) = (ax.min(tx), ay.min(ty));
        let (mut r, mut b) = (ax.max(tx) + 1, ay.max(ty) + 1);
        if !square && self.snapping() {
            // The edges on the pointer's side move; they snap (Step 52).
            let p = self.picture;
            let (pl, pr, pt, pb) = (i64::from(p.x), p.right(), i64::from(p.y), p.bottom());
            if tx >= ax {
                let s = self.snap(r, true, (t, b));
                if s > l {
                    r = s.min(pr);
                }
            } else {
                let s = self.snap(l, true, (t, b));
                if s < r {
                    l = s.max(pl);
                }
            }
            if ty >= ay {
                let s = self.snap(b, false, (l, r));
                if s > t {
                    b = s.min(pb);
                }
            } else {
                let s = self.snap(t, false, (l, r));
                if s < b {
                    t = s.max(pt);
                }
            }
        }
        rect_from(l, t, r, b)
    }

    /// Lengths of the selection's handles and band, from the DPI of the monitor at its center.
    fn metrics(&self, s: Rect) -> SelMetrics {
        let c = (
            i32::try_from(i64::from(s.x) + i64::from(s.width / 2)).unwrap_or(s.x),
            i32::try_from(i64::from(s.y) + i64::from(s.height / 2)).unwrap_or(s.y),
        );
        let m = self.monitor_at(c);
        SelMetrics {
            half: m.px(9) / 2,
            band: m.px(3),
            handle: m.px(7),
            small: s.width < m.px(27) || s.height < m.px(27),
        }
    }

    /// The zone of a point relative to the confirmed selection (UX spec §1): the toolbar, a
    /// handle zone (9×9 logical pixels), the border band (3 logical pixels on each side of an
    /// edge), the size chip where it covers the selection, the interior, or outside. A
    /// selection smaller than 27 logical pixels in either direction has its handles and band
    /// outside it, so its whole area is interior (minus the toolbar and chip, if they cover it).
    pub fn zone(&self, (x, y): (i32, i32)) -> Zone {
        let at = Rect::new(x, y, 1, 1);
        if let Some(bar) = self.toolbar(self.chip_at.get())
            && bar.rect.contains(&at)
        {
            return Zone::Toolbar(bar.buttons.iter().position(|b| b.contains(&at)));
        }
        let z = self.base_zone((x, y));
        // With a tool the chip is only a readout: presses under it draw (Step 56).
        if z == Zone::Interior
            && self.ann.tool().is_none()
            && self.chip_at.get().is_some_and(|c| c.contains(&at))
        {
            return Zone::Chip;
        }
        z
    }

    fn base_zone(&self, (x, y): (i32, i32)) -> Zone {
        let Some(s) = self.selection else {
            return Zone::Outside;
        };
        if self.ann.tool().is_some() {
            // A tool hides the handles: the selection is a canvas, inside or not (Step 56).
            return if s.contains(&Rect::new(x, y, 1, 1)) {
                Zone::Interior
            } else {
                Zone::Outside
            };
        }
        let m = self.metrics(s);
        let (px, py) = (i64::from(x), i64::from(y));
        let half = i64::from(m.half);
        for (edges, (cx, cy)) in handle_centers(s, &m) {
            if (px - cx).abs() <= half && (py - cy).abs() <= half {
                return Zone::Edge(edges);
            }
        }
        let (l, r, t, b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
        let band = i64::from(m.band);
        let inside = px >= l && px < r && py >= t && py < b;
        if m.small && inside {
            return Zone::Interior;
        }
        let in_y = py >= t - band && py < b + band;
        let in_x = px >= l - band && px < r + band;
        let edges = if m.small {
            Edges {
                left: in_y && px >= l - band && px < l,
                right: in_y && px >= r && px < r + band,
                top: in_x && py >= t - band && py < t,
                bottom: in_x && py >= b && py < b + band,
            }
        } else {
            Edges {
                left: in_y && (px - l).abs() <= band,
                right: in_y && (px - r).abs() <= band,
                top: in_x && (py - t).abs() <= band,
                bottom: in_x && (py - b).abs() <= band,
            }
        };
        if edges != Edges::default() {
            Zone::Edge(edges)
        } else if inside {
            Zone::Interior
        } else {
            Zone::Outside
        }
    }

    /// The pointer shape for the current state and position.
    pub fn cursor(&self) -> Cursor {
        match self.state {
            State::Moving { .. } => Cursor::Move,
            State::Resizing { edges, .. } => edge_cursor(edges),
            State::Pressed { zone, .. } => zone_cursor(zone),
            State::Drawing => Cursor::Cross,
            State::Selected if self.ann.tool().is_some() => match self.zone(self.pointer) {
                Zone::Toolbar(_) | Zone::Chip => Cursor::Arrow,
                _ => Cursor::Cross,
            },
            State::Selected => zone_cursor(self.zone(self.pointer)),
            State::Hover | State::Dragging { .. } | State::Marking { .. } => Cursor::Cross,
        }
    }

    /// Feeds one input. See the module documentation for the rules.
    pub fn handle(&mut self, input: Input) -> Outcome {
        let out = self.input(input);
        self.update_element_request();
        if let Some(s) = self.selection {
            self.ann.set_clip(s);
        }
        out
    }

    /// The mouse put the pointer at `p`: a real move resumes detection and snapping after the
    /// keyboard moved the pointer (the real cursor warped by a pointer key reports the same
    /// position again, which changes nothing).
    fn mouse_at(&mut self, p: (i32, i32)) {
        if p != self.pointer {
            self.detect.suspended = false;
        }
        self.pointer = p;
    }

    fn input(&mut self, input: Input) -> Outcome {
        match input {
            // The pin editor keeps the work when another window becomes active.
            Input::FocusLost if self.mode == Mode::PinEdit => Outcome::Stay,
            Input::Leave | Input::FocusLost | Input::Action(Action::Cancel) => Outcome::Cancel,
            Input::CaptureLost => {
                self.bar_pressed = None;
                if self.state == State::Drawing {
                    self.ann.abort();
                }
                self.abort_drag();
                Outcome::Stay
            }
            Input::Wheel(delta) => {
                if self.ann.tool().is_some() || self.ann.selected().is_some() {
                    self.wheel += delta;
                    while self.wheel.abs() >= 120 {
                        let up = self.wheel > 0;
                        self.ann.step_width(up);
                        self.wheel -= if up { 120 } else { -120 };
                    }
                }
                Outcome::Stay
            }
            Input::Char(c) => {
                self.ann.type_char(c);
                Outcome::Stay
            }
            Input::TextKey(k) => {
                self.ann.text_key(k);
                Outcome::Stay
            }
            Input::Annotate(a) => {
                self.click = None;
                self.annotate(a)
            }
            Input::Move { x, y, mods } => {
                self.mouse_at(self.clamp((x, y)));
                self.shift = mods.shift;
                self.alt = mods.alt;
                self.pointer_moved();
                Outcome::Stay
            }
            Input::Press {
                button,
                x,
                y,
                mods,
                time_ms,
            } => {
                self.mouse_at(self.clamp((x, y)));
                self.shift = mods.shift;
                self.alt = mods.alt;
                self.press(button, mods, time_ms)
            }
            Input::Release { button, x, y } => {
                if button == Button::Left {
                    self.mouse_at(self.clamp((x, y)));
                    self.pointer_moved();
                    return self.release();
                }
                Outcome::Stay
            }
            Input::Action(a) => {
                self.click = None;
                self.streak = Some((a, 0));
                self.action(a)
            }
            Input::Repeat(a) => {
                self.click = None;
                self.streak = Some(match self.streak {
                    Some((last, n)) if last == a => (a, n.saturating_add(1)),
                    _ => (a, 1),
                });
                self.action(a)
            }
            Input::Element { request, answer } => {
                self.element_answer(request, answer);
                Outcome::Stay
            }
            Input::Nav(n) => {
                self.click = None;
                self.nav(n)
            }
            Input::MenuKey => {
                self.click = None;
                match (self.state, self.selection) {
                    (State::Selected, Some(_)) => Outcome::Menu(None),
                    _ => Outcome::Stay,
                }
            }
        }
    }

    /// Whether a plain left press at `time` in the interior completes a double-click with the
    /// remembered first press (UX spec §4.2 rule 2; the system values as they are now).
    fn completes_double_click(&self, first: Option<((i32, i32), u32)>, time: u32) -> bool {
        let Some(((x, y), t)) = first else {
            return false;
        };
        let (cx, cy) = self.sys.double_click;
        let dx = (i64::from(self.pointer.0) - i64::from(x)).unsigned_abs();
        let dy = (i64::from(self.pointer.1) - i64::from(y)).unsigned_abs();
        time.wrapping_sub(t) <= self.sys.double_click_ms
            && dx <= u64::from(cx / 2)
            && dy <= u64::from(cy / 2)
    }

    fn press(&mut self, button: Button, mods: Mods, time: u32) -> Outcome {
        // Any press ends the wait for a second click; a plain left press in the interior may
        // complete it first.
        let first = self.click.take();
        if self.state == State::Drawing {
            // Another button while drawing: a right-click finishes the shape (ANN-10).
            if button == Button::Right && self.ann.right_click() {
                self.state = State::Selected;
            }
            return Outcome::Stay;
        }
        if self.state == State::Selected
            && let Some(out) = self.annotation_press(button, mods, time)
        {
            return out;
        }
        if let State::Marking { anchor, .. } = self.state {
            match button {
                // A click sets the opposite corner of a marked selection.
                Button::Left => {
                    let r = self.band(anchor, self.pointer, self.shift);
                    self.set_selection(r);
                    self.history_at = None;
                }
                // A right-click undoes the mark.
                Button::Right => self.abort_drag(),
                Button::Middle => {}
            }
            return Outcome::Stay;
        }
        match button {
            Button::Left => match self.state {
                State::Hover => {
                    self.state = State::Pressed {
                        at: self.pointer,
                        zone: Zone::Outside,
                        click: None,
                    };
                    Outcome::Stay
                }
                State::Selected => {
                    let zone = self.zone(self.pointer);
                    match zone {
                        // A toolbar button runs on release over itself.
                        Zone::Toolbar(i) => {
                            self.bar_pressed = i.map(|i| (i, mods.shift));
                            return Outcome::Stay;
                        }
                        // The size chip where it covers the selection: nothing.
                        Zone::Chip => return Outcome::Stay,
                        // The pin editor never starts a new selection outside the pin.
                        Zone::Outside if self.mode == Mode::PinEdit => return Outcome::Stay,
                        _ => {}
                    }
                    self.bar_focus = None; // a press in the canvas gives it the keyboard focus
                    let plain = zone == Zone::Interior && !mods.any() && self.mode == Mode::Capture;
                    if plain
                        && self.completes_double_click(first, time)
                        && let (Some(out), Some(s)) =
                            (self.gestures.double_click.output(), self.selection)
                    {
                        // Fires on the second press, so the pin appears at once (rule 3).
                        return Outcome::Output(out, s, Via::DoubleClick);
                    }
                    self.state = State::Pressed {
                        at: self.pointer,
                        zone,
                        click: plain.then_some(time),
                    };
                    Outcome::Stay
                }
                _ => Outcome::Stay,
            },
            Button::Right => match self.state {
                State::Hover => Outcome::Cancel,
                // The pin editor has no selection menu and never loses its selection.
                State::Selected if self.mode == Mode::PinEdit => Outcome::Stay,
                State::Selected => match self.zone(self.pointer) {
                    Zone::Outside => {
                        // Right-click outside a confirmed selection clears it (NOEUL-FR-012).
                        self.selection = None;
                        self.bar_focus = None;
                        self.bar_hover = None;
                        self.state = State::Hover;
                        Outcome::Stay
                    }
                    // Inside: the selection's context menu (UX spec §3.2).
                    Zone::Interior | Zone::Edge(_) => Outcome::Menu(Some(self.pointer)),
                    Zone::Toolbar(_) | Zone::Chip => Outcome::Stay,
                },
                _ => {
                    // During a drag: undo that drag.
                    self.abort_drag();
                    Outcome::Stay
                }
            },
            // A middle-click inside the selection: the `[gestures] middle_click` output.
            Button::Middle => match (self.state, self.selection) {
                (State::Selected, Some(s))
                    if self.zone(self.pointer) == Zone::Interior && self.mode == Mode::Capture =>
                {
                    self.gestures
                        .middle_click
                        .output()
                        .map_or(Outcome::Stay, |out| {
                            Outcome::Output(out, s, Via::MiddleClick)
                        })
                }
                _ => Outcome::Stay,
            },
        }
    }

    /// A press while annotating (UX spec §4.2 rule 6, §4.9): with a tool, a label being edited or
    /// an annotation selected, presses go to the editor first. `None`: the press belongs to the
    /// selection as before.
    fn annotation_press(&mut self, button: Button, mods: Mods, time: u32) -> Option<Outcome> {
        let tool = self.ann.tool().is_some();
        if !(tool || self.ann.editing_text() || self.ann.selected().is_some()) {
            return None;
        }
        let at = self.pointer;
        if matches!(self.zone(at), Zone::Toolbar(_) | Zone::Chip) {
            return None;
        }
        match button {
            Button::Left => {
                let inside = self
                    .selection
                    .is_some_and(|s| s.contains(&Rect::new(at.0, at.1, 1, 1)));
                let dpi = self.monitor_at(at).dpi;
                let first = self.text_click.take();
                if self.ann.editing_text()
                    && inside
                    && !mods.any()
                    && self.completes_double_click(first, time)
                {
                    // FLOW-14: a double-click on a word of the label selects it; no pin.
                    self.ann.select_word_at(at);
                    return Some(Outcome::Stay);
                }
                // `Ctrl` gives a privacy tool its other form (Step 57).
                if (inside || self.ann.handle_at(at, dpi).is_some())
                    && self.ann.press_with(at, dpi, mods.ctrl)
                {
                    self.bar_focus = None;
                    self.text_click = (!mods.any()).then_some((at, time));
                    if self.ann.busy() {
                        self.state = State::Drawing;
                    }
                    return Some(Outcome::Stay);
                }
                // With a tool, a press elsewhere does nothing (it never pins or reselects).
                tool.then_some(Outcome::Stay)
            }
            Button::Right => {
                // A right-click ends a label, finishes a shape, else leaves the tool (ANN-10).
                if self.ann.editing_text() {
                    self.ann.end_text();
                } else if !self.ann.right_click() {
                    self.ann.set_tool(None);
                }
                Some(Outcome::Stay)
            }
            Button::Middle => tool.then_some(Outcome::Stay),
        }
    }

    /// An `annotate` action (Step 56).
    fn annotate(&mut self, a: AnnotateAction) -> Outcome {
        if !self.annotate_context() {
            return Outcome::Stay;
        }
        match a {
            AnnotateAction::Done => {
                self.ann.done();
            }
            AnnotateAction::Undo => {
                self.ann.undo();
            }
            AnnotateAction::Redo => {
                self.ann.redo();
            }
            AnnotateAction::WidthDown | AnnotateAction::WidthUp => {
                self.ann.step_width(a == AnnotateAction::WidthUp);
            }
            AnnotateAction::Tool(t) => self.choose_tool(t),
            AnnotateAction::ClearAll => {
                self.ann.clear_all();
            }
            AnnotateAction::Delete => {
                self.ann.delete_selected();
            }
            AnnotateAction::SelectNext | AnnotateAction::SelectPrevious => {
                self.ann.select_step(a == AnnotateAction::SelectNext);
            }
            AnnotateAction::Order(o) => {
                self.ann.reorder(o);
            }
        }
        Outcome::Stay
    }

    /// Chooses a tool; choosing the active one again leaves it (its button toggles).
    fn choose_tool(&mut self, t: Tool) {
        let next = (self.ann.tool() != Some(t)).then_some(t);
        self.ann.set_tool(next);
        self.text_click = None;
    }

    fn pointer_moved(&mut self) {
        let p = self.pointer;
        match self.state {
            State::Pressed { at, zone, .. } if self.beyond_drag(at, p) => {
                self.state = match zone {
                    Zone::Outside | Zone::Toolbar(_) | Zone::Chip => State::Dragging {
                        anchor: at,
                        to: p,
                        square: self.shift,
                        before: self.selection,
                    },
                    Zone::Interior => State::Moving {
                        grab: at,
                        start: self.selection.unwrap_or(self.picture),
                    },
                    Zone::Edge(edges) => State::Resizing {
                        edges,
                        grab: at,
                        start: self.selection.unwrap_or(self.picture),
                    },
                };
                self.pointer_moved();
            }
            State::Dragging { anchor, before, .. } => {
                self.state = State::Dragging {
                    anchor,
                    to: p,
                    square: self.shift,
                    before,
                };
            }
            State::Moving { grab, start } => {
                self.selection = Some(self.moved(start, p.0 - grab.0, p.1 - grab.1));
            }
            State::Resizing { edges, grab, start } => {
                self.selection = Some(self.resized(start, edges, p.0 - grab.0, p.1 - grab.1));
            }
            State::Drawing => self.ann.drag(p, self.shift),
            State::Selected => {
                self.bar_hover = match self.zone(p) {
                    Zone::Toolbar(i) => i,
                    _ => None,
                };
                if self.ann.marking() {
                    // A keyboard-drawn shape follows the pointer to its end point.
                    self.ann.drag(p, self.shift);
                }
            }
            _ => {}
        }
    }

    fn release(&mut self) -> Outcome {
        match self.state {
            State::Pressed { at, click, .. } => {
                // A click without a drag. Inside a confirmed selection it may be the first
                // press of a double-click; with nothing selected it selects the detected window
                // or element (Step 52, J2), if any.
                self.click = click.map(|t| (at, t));
                if let Some(r) = self.selectable() {
                    self.set_selection(r);
                    self.history_at = None;
                } else {
                    self.state = self.rest();
                }
            }
            State::Dragging {
                anchor, to, square, ..
            } => {
                // The release that ends a drag is not a click (rule 4).
                self.selection = Some(self.band(anchor, to, square));
                self.state = State::Selected;
                self.history_at = None;
            }
            State::Moving { .. } | State::Resizing { .. } => self.state = State::Selected,
            State::Drawing => {
                self.ann.release(self.pointer, self.shift);
                self.state = State::Selected;
            }
            State::Selected => {
                if let Some((i, shift)) = self.bar_pressed.take()
                    && self.zone(self.pointer) == Zone::Toolbar(Some(i))
                {
                    return self.activate(i, shift, Via::Toolbar);
                }
            }
            State::Hover | State::Marking { .. } => {}
        }
        Outcome::Stay
    }

    /// Runs toolbar button `i` (`Shift`+Save quick-saves).
    fn activate(&mut self, i: usize, shift: bool, via: Via) -> Outcome {
        let Some(s) = self.selection else {
            return Outcome::Stay;
        };
        let item = self.bar().get(i).copied();
        if matches!(
            item,
            Some(ToolbarItem::Pin | ToolbarItem::Copy | ToolbarItem::Save | ToolbarItem::Done)
        ) {
            // An output keeps a shape still being drawn with the keyboard and the label typed.
            self.ann.finish();
        }
        match item {
            Some(ToolbarItem::Pin | ToolbarItem::Done) => Outcome::Output(Output::Pin, s, via),
            Some(ToolbarItem::Copy) => Outcome::Output(Output::Copy, s, via),
            Some(ToolbarItem::Save) if shift => Outcome::Output(Output::QuickSave, s, via),
            Some(ToolbarItem::Save) => Outcome::Output(Output::Save, s, via),
            Some(ToolbarItem::Cancel) => Outcome::Cancel,
            Some(ToolbarItem::Tool(t)) => {
                self.choose_tool(t);
                // The canvas takes the keyboard, so the pointer keys and Space draw at once.
                self.bar_focus = None;
                Outcome::Stay
            }
            Some(ToolbarItem::Color) => {
                self.ann.step_color(!shift);
                Outcome::Stay
            }
            Some(ToolbarItem::Width) => {
                let max = (annotation::WIDTHS.len() - 1) as u8;
                if shift {
                    self.ann.step_width(false);
                } else if self.ann.level() >= max {
                    while self.ann.step_width(false) && self.ann.level() > 0 {}
                } else {
                    self.ann.step_width(true);
                }
                Outcome::Stay
            }
            Some(ToolbarItem::Undo) => {
                self.ann.undo();
                Outcome::Stay
            }
            Some(ToolbarItem::Redo) => {
                self.ann.redo();
                Outcome::Stay
            }
            None => Outcome::Stay,
        }
    }

    /// A navigation key for the toolbar (UX spec §5): it exists only next to a confirmed
    /// selection while no button is held.
    fn nav(&mut self, n: Nav) -> Outcome {
        if n == Nav::Escape {
            return Outcome::Cancel;
        }
        if self.state != State::Selected || self.selection.is_none() {
            return Outcome::Stay;
        }
        let count = self.bar().len();
        self.bar_focus = match (n, self.bar_focus) {
            (Nav::Next, None) => Some(0),
            (Nav::Next, Some(i)) => (i + 1 < count).then_some(i + 1),
            (Nav::Prev, None) => Some(count - 1),
            (Nav::Prev, Some(i)) => i.checked_sub(1),
            (Nav::Left, Some(i)) => Some((i + count - 1) % count),
            (Nav::Right, Some(i)) => Some((i + 1) % count),
            (Nav::Activate, Some(i)) => return self.activate(i, false, Via::Key),
            (_, focus) => focus,
        };
        Outcome::Stay
    }

    fn rest(&self) -> State {
        if self.selection.is_some() {
            State::Selected
        } else {
            State::Hover
        }
    }

    fn abort_drag(&mut self) {
        match self.state {
            State::Dragging { before, .. } | State::Marking { before, .. } => {
                self.selection = before;
            }
            State::Moving { start, .. } | State::Resizing { start, .. } => {
                self.selection = Some(start);
            }
            _ => {}
        }
        self.state = self.rest();
    }

    fn dragging(&self) -> bool {
        matches!(
            self.state,
            State::Pressed { .. }
                | State::Dragging { .. }
                | State::Moving { .. }
                | State::Resizing { .. }
                | State::Drawing
        )
    }

    /// The step of the current key: its auto-repeats in a row accelerate it.
    fn key_step(&self) -> i32 {
        accel(self.streak.map_or(0, |(_, n)| n))
    }

    fn action(&mut self, a: Action) -> Outcome {
        match a {
            // Settings of the session work in every state.
            Action::Cancel => return Outcome::Cancel,
            // The pin editor has no detection, magnifier, history or other outputs.
            Action::DetectCycle
            | Action::Magnifier
            | Action::Full
            | Action::Last
            | Action::HistoryPrevious
            | Action::HistoryNext
            | Action::Copy
            | Action::Save
            | Action::QuickSave
                if self.mode == Mode::PinEdit =>
            {
                return Outcome::Stay;
            }
            Action::DetectCycle => {
                self.detect.mode = self.detect.mode.next();
                self.detect.element = None;
                self.detect.asked = None;
                self.detect.fallback = None;
                self.detect.suspended = false;
                return Outcome::Stay;
            }
            Action::Magnifier => {
                self.magnifier = !self.magnifier;
                return Outcome::Stay;
            }
            // The keyboard pointer works during a drag, a move and a resize too: the precise
            // end of a drag is what it is for.
            Action::Pointer(d) => {
                let n = self.key_step();
                let (dx, dy) = step(d);
                let p = self.clamp((
                    self.pointer.0.saturating_add(dx * n),
                    self.pointer.1.saturating_add(dy * n),
                ));
                self.pointer = p;
                self.detect.suspended = true;
                self.warp = Some(p);
                self.pointer_moved();
                return Outcome::Stay;
            }
            _ => {}
        }
        if self.dragging() {
            // Other keys wait until the button is released (Esc still cancels, above).
            return Outcome::Stay;
        }
        let marking = matches!(self.state, State::Marking { .. });
        if matches!(
            a,
            Action::Pin | Action::Copy | Action::Save | Action::QuickSave
        ) && !marking
        {
            // An output keeps a shape still being drawn with the keyboard.
            self.ann.finish();
        }
        // With a tool, `selection.mark` draws: the start of a shape, then its end (Step 56).
        if a == Action::Mark
            && self.state == State::Selected
            && let Some(s) = self.selection
            && self.ann.tool().is_some()
        {
            let p = clamp_point(self.pointer, s);
            let dpi = self.monitor_at(p).dpi;
            self.ann.mark(p, dpi, self.shift);
            return Outcome::Stay;
        }
        if a == Action::Mark && self.mode == Mode::PinEdit {
            return Outcome::Stay;
        }
        // While an annotation is selected, the selection keys act on it.
        if let (Action::Move(d) | Action::Grow(d) | Action::Shrink(d), Some(_)) =
            (a, self.ann.selected())
            && !self.ann.editing_text()
        {
            let n = self.key_step();
            let (dx, dy) = step(d);
            match a {
                Action::Move(_) => self.ann.nudge(dx * n, dy * n),
                Action::Grow(_) => self.ann.resize_by((dx, dy), n, true),
                _ => self.ann.resize_by((dx, dy), n, false),
            };
            return Outcome::Stay;
        }
        let output = |o: Output| move |s: Rect| Outcome::Output(o, s, Via::Key);
        match a {
            // Nothing is confirmed while a corner is marked.
            Action::Pin
            | Action::Copy
            | Action::Save
            | Action::QuickSave
            | Action::Move(_)
            | Action::Grow(_)
            | Action::Shrink(_)
                if marking =>
            {
                Outcome::Stay
            }
            Action::Pin => self.selection.map_or(Outcome::Stay, output(Output::Pin)),
            Action::Copy => self.selection.map_or(Outcome::Stay, output(Output::Copy)),
            Action::Save => self.selection.map_or(Outcome::Stay, output(Output::Save)),
            Action::QuickSave => self
                .selection
                .map_or(Outcome::Stay, output(Output::QuickSave)),
            Action::Mark => {
                match self.state {
                    State::Marking { anchor, .. } => {
                        let r = self.band(anchor, self.pointer, self.shift);
                        self.set_selection(r);
                        self.history_at = None;
                    }
                    State::Hover if self.selectable().is_some() => {
                        if let Some(r) = self.selectable() {
                            self.set_selection(r);
                            self.history_at = None;
                        }
                    }
                    _ => {
                        self.bar_focus = None;
                        self.state = State::Marking {
                            anchor: self.pointer,
                            before: self.selection,
                        };
                    }
                }
                Outcome::Stay
            }
            Action::Full => {
                let m = self.monitor_at(self.pointer).rect;
                let next = if self.selection() == Some(m) {
                    self.picture
                } else {
                    m
                };
                self.set_selection(next);
                self.history_at = None;
                Outcome::Stay
            }
            Action::Last => {
                if let Some(r) = self.last.and_then(|l| intersection(l, self.picture)) {
                    self.set_selection(r);
                    self.history_at = None;
                }
                Outcome::Stay
            }
            Action::HistoryPrevious | Action::HistoryNext => {
                let len = self.history.len();
                if len == 0 {
                    return Outcome::Stay;
                }
                let at = match (a, self.history_at) {
                    (Action::HistoryPrevious, None) => Some(len - 1),
                    (Action::HistoryPrevious, Some(i)) => Some(i.saturating_sub(1)),
                    (_, Some(i)) => Some((i + 1).min(len - 1)),
                    (_, None) => None,
                };
                if let Some(i) = at {
                    self.history_at = Some(i);
                    self.set_selection(self.history[i]);
                }
                Outcome::Stay
            }
            Action::Move(d) | Action::Grow(d) | Action::Shrink(d) => {
                let n = self.key_step();
                if let Some(s) = self.selection {
                    let next = match a {
                        Action::Move(_) => {
                            let (dx, dy) = step(d);
                            self.moved(s, dx * n, dy * n)
                        }
                        Action::Grow(_) => self.grown(s, d, n),
                        _ => shrunk(s, d, n),
                    };
                    self.selection = Some(next);
                }
                Outcome::Stay
            }
            Action::Cancel | Action::DetectCycle | Action::Magnifier | Action::Pointer(_) => {
                Outcome::Stay
            }
        }
    }

    fn set_selection(&mut self, r: Rect) {
        self.selection = Some(r);
        self.state = State::Selected;
    }

    /// `start` moved by (dx, dy), kept inside the picture (the pin's image when editing one).
    fn moved(&self, start: Rect, dx: i32, dy: i32) -> Rect {
        let p = self.bound;
        let max_x = p.right() - i64::from(start.width);
        let max_y = p.bottom() - i64::from(start.height);
        let x =
            (i64::from(start.x) + i64::from(dx)).clamp(i64::from(p.x), max_x.max(i64::from(p.x)));
        let y =
            (i64::from(start.y) + i64::from(dy)).clamp(i64::from(p.y), max_y.max(i64::from(p.y)));
        Rect::new(
            i32::try_from(x).unwrap_or(start.x),
            i32::try_from(y).unwrap_or(start.y),
            start.width,
            start.height,
        )
    }

    /// `start` with the edges in `edges` moved by (dx, dy), at least one pixel, inside the picture;
    /// an edge dragged across the opposite one flips the rectangle.
    fn resized(&self, start: Rect, edges: Edges, dx: i32, dy: i32) -> Rect {
        let p = self.bound;
        let (pl, pr, pt, pb) = (i64::from(p.x), p.right(), i64::from(p.y), p.bottom());
        let mut l = i64::from(start.x);
        let mut r = start.right();
        let mut t = i64::from(start.y);
        let mut b = start.bottom();
        if edges.left {
            l = (l + i64::from(dx)).clamp(pl, pr);
        }
        if edges.right {
            r = (r + i64::from(dx)).clamp(pl, pr);
        }
        if edges.top {
            t = (t + i64::from(dy)).clamp(pt, pb);
        }
        if edges.bottom {
            b = (b + i64::from(dy)).clamp(pt, pb);
        }
        if self.snapping() {
            // The dragged edges snap (Step 52); the extent across is the rectangle's.
            let (ys, xs) = ((t.min(b), t.max(b)), (l.min(r), l.max(r)));
            if edges.left {
                l = self.snap(l, true, ys).clamp(pl, pr);
            }
            if edges.right {
                r = self.snap(r, true, ys).clamp(pl, pr);
            }
            if edges.top {
                t = self.snap(t, false, xs).clamp(pt, pb);
            }
            if edges.bottom {
                b = self.snap(b, false, xs).clamp(pt, pb);
            }
        }
        let (l, r) = span(l, r, pl, pr);
        let (t, b) = span(t, b, pt, pb);
        rect_from(l, t, r, b)
    }

    /// `s` with the edge towards `d` moved out by `n` device pixels, inside the picture.
    fn grown(&self, s: Rect, d: Dir, n: i32) -> Rect {
        let p = self.bound;
        let n = i64::from(n.max(1));
        let (mut l, mut r, mut t, mut b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
        match d {
            Dir::Up => t = (t - n).max(i64::from(p.y)),
            Dir::Down => b = (b + n).min(p.bottom()),
            Dir::Left => l = (l - n).max(i64::from(p.x)),
            Dir::Right => r = (r + n).min(p.right()),
        }
        rect_from(l, t, r, b)
    }

    /// Whether moving edges snap now: detection on (its windows are the targets), the mouse moved
    /// the pointer last, and `Alt` is not held.
    fn snapping(&self) -> bool {
        self.detect.mode != DetectMode::Off && !self.detect.suspended && !self.alt
    }

    /// `edge` (a vertical edge's x when `vertical`, else a horizontal edge's y) snapped to the
    /// nearest monitor or detected window edge within [`SNAP`] logical pixels whose extent
    /// overlaps `span` (the moving edge's extent across).
    fn snap(&self, edge: i64, vertical: bool, span: (i64, i64)) -> i64 {
        let reach = i64::from(self.monitor_at(self.pointer).px(SNAP));
        let overlaps = |r: Rect| {
            let (lo, hi) = if vertical {
                (i64::from(r.y), r.bottom())
            } else {
                (i64::from(r.x), r.right())
            };
            lo < span.1 && span.0 < hi
        };
        let edges = |r: Rect| {
            if vertical {
                [i64::from(r.x), r.right()]
            } else {
                [i64::from(r.y), r.bottom()]
            }
        };
        let rects = self
            .monitors
            .iter()
            .map(|m| m.rect)
            .chain(self.detect.snapshot.windows().iter().map(|w| w.rect));
        let mut best: Option<(i64, i64)> = None;
        for r in rects.filter(|r| overlaps(*r)) {
            for c in edges(r) {
                let d = (c - edge).abs();
                if d <= reach && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, c));
                }
            }
        }
        best.map_or(edge, |(_, c)| c)
    }

    /// The scene for the current state (what [`compose`] draws). `measure` sizes the labels'
    /// text in device pixels.
    pub fn scene(&self, measure: &dyn Measure) -> Scene {
        let live = self.live();
        // With nothing selected, the detected window or element is shown as a selection would
        // be, without handles; a protected window is not, and a note says why (Step 52).
        let detected = if live.is_none() {
            self.detected()
        } else {
            None
        };
        let protected = detected.is_some_and(|d| d.protected);
        let shown = live.or(detected.filter(|d| !d.protected).map(|d| d.rect));
        let confirmed =
            live.is_some() && !matches!(self.state, State::Dragging { .. } | State::Marking { .. });
        let mut scene = Scene {
            picture: self.picture,
            selection: shown,
            accent: None,
            halo: None,
            handles: Vec::new(),
            chip: None,
            help: None,
            toolbar: None,
            magnifier: None,
            annotation: None,
            layered: self.mode == Mode::PinEdit,
        };
        if let Some(s) = shown {
            // The border sits just outside the selection; on a picture edge it moves inside so
            // it stays visible (UX spec §10).
            scene.accent = Some(clamp_into(grow(s, 1), self.picture));
            scene.halo = Some(clamp_into(grow(s, 2), self.picture));
            // A tool hides the handles: presses inside draw (Step 56).
            if confirmed && self.ann.tool().is_none() {
                let m = self.metrics(s);
                scene.handles = handle_centers(s, &m)
                    .iter()
                    .filter_map(|(_, (cx, cy))| {
                        let h = i64::from(m.handle);
                        intersection(
                            rect_from(cx - h / 2, cy - h / 2, cx - h / 2 + h, cy - h / 2 + h),
                            self.picture,
                        )
                    })
                    .collect();
            }
        }
        // The size readout of the annotation drawn or selected (NOEUL-FR-083), and what a
        // privacy tool's protection is worth (NOEUL-FR-082).
        let note = protected
            .then(|| PROTECTED_NOTE.to_string())
            .or_else(|| self.ann.note());
        let chip = self.chip(shown, note.as_deref(), measure);
        // Hit-testing follows what is drawn (the toolbar avoids the chip; a press on the chip
        // where it covers the selection is not a click).
        self.chip_at.set(Some(chip.rect));
        scene.magnifier = self.magnifier_scene(measure);
        let bar = self.toolbar(Some(chip.rect));
        scene.chip = Some(chip);
        if let Some(bar) = &bar {
            let doc = self.ann.doc();
            scene.toolbar = Some(ToolbarScene {
                rect: bar.rect,
                dpi: bar.dpi,
                buttons: self
                    .bar()
                    .iter()
                    .zip(bar.buttons.iter().copied())
                    .enumerate()
                    .map(|(i, (item, rect))| ButtonScene {
                        rect,
                        label: item.label(),
                        item: *item,
                        primary: matches!(item, ToolbarItem::Pin | ToolbarItem::Done),
                        hover: self.bar_hover == Some(i),
                        pressed: self.bar_pressed.is_some_and(|(p, _)| p == i),
                        focus: self.bar_focus == Some(i),
                        active: matches!(item, ToolbarItem::Tool(t) if self.ann.tool() == Some(*t)),
                        disabled: match item {
                            ToolbarItem::Undo => doc.undo_steps() == 0 && !self.ann.busy(),
                            ToolbarItem::Redo => doc.redo_steps() == 0,
                            _ => false,
                        },
                        color: self.ann.color().1,
                        level: self.ann.level(),
                    })
                    .collect(),
            });
        }
        scene.annotation = self.annotation_scene();
        if !self.dragging() {
            let lines = match &self.banner {
                Some(b) => Some(vec![b.clone()]),
                None => self.help.clone(),
            };
            if let Some(lines) = lines {
                scene.help = Some(self.help_label(lines, shown, bar.map(|b| b.rect), measure));
            }
        }
        scene
    }

    /// Whether the magnifier is wanted now: while selecting (hovering, dragging, resizing,
    /// marking) and next to a confirmed selection where a press would start something new or
    /// resize; not while moving it, nor over its inside, the toolbar or the size chip.
    fn magnifier_wanted(&self) -> bool {
        if self.mode == Mode::PinEdit {
            return false;
        }
        match self.state {
            State::Hover
            | State::Dragging { .. }
            | State::Resizing { .. }
            | State::Marking { .. } => true,
            State::Pressed { zone, .. } => matches!(zone, Zone::Outside | Zone::Edge(_)),
            State::Selected => matches!(self.zone(self.pointer), Zone::Outside | Zone::Edge(_)),
            State::Moving { .. } | State::Drawing => false,
        }
    }

    /// The magnifier near the pointer, on the pointer's monitor and at its DPI: right of and below
    /// the pointer, or left of and above it where the monitor ends (NOEUL-FR-041).
    fn magnifier_scene(&self, measure: &dyn Measure) -> Option<MagnifierScene> {
        if !self.magnifier || !self.magnifier_wanted() {
            return None;
        }
        let m = self.monitor_at(self.pointer);
        let (x, y) = self.pointer;
        let mut lines = vec![format!("{x}, {y}")];
        if m.dpi != crate::capture::BASE_DPI
            && let Some(lp) = dpi::point_to_logical(&self.monitors, self.pointer)
        {
            // On a scaled monitor, also the logical position on that monitor.
            lines.push(format!(
                "{}, {} at {} %",
                lp.x,
                lp.y,
                dpi::scale_percent(m.dpi)
            ));
        }
        let color = lines.len();
        lines.extend(COLOR_TEMPLATE.iter().map(|s| (*s).to_string()));
        let mut text = Label::new(lines, Font::Mono, m.dpi, measure);
        let cell = m.px(MAG_CELL);
        let side = i64::from(cell * MAG_CELLS);
        let w = side.max(i64::from(text.rect.width)) + 2;
        let h = side + i64::from(text.rect.height) + 2;
        let gap = i64::from(m.px(MAG_GAP));
        let mr = m.rect;
        let (ml, mt, mrr, mb) = (i64::from(mr.x), i64::from(mr.y), mr.right(), mr.bottom());
        let (px, py) = (i64::from(x), i64::from(y));
        let bx = if px + gap + w <= mrr {
            px + gap
        } else {
            px - gap - w
        };
        let by = if py + gap + h <= mb {
            py + gap
        } else {
            py - gap - h
        };
        let (bx, by) = (bx.min(mrr - w).max(ml), by.min(mb - h).max(mt));
        let rect = rect_from(bx, by, bx + w, by + h);
        let gx = bx + 1 + (w - 2 - side) / 2;
        let grid = rect_from(gx, by + 1, gx + side, by + 1 + side);
        text.rect = rect_from(bx + 1, by + 1 + side, bx + w - 1, by + h - 1);
        Some(MagnifierScene {
            rect,
            grid,
            cell,
            at: self.pointer,
            text,
            color,
        })
    }

    /// The toolbar's place and buttons (UX spec §3.1): next to a confirmed selection while no
    /// button is held (and while an annotation is drawn), sized for the monitor under the
    /// selection's bottom-right corner ([`toolbar_layout`]).
    fn toolbar(&self, chip: Option<Rect>) -> Option<ToolbarLayout> {
        if !matches!(
            self.state,
            State::Selected | State::Pressed { .. } | State::Drawing
        ) {
            return None;
        }
        let s = self.selection?;
        let corner = (
            i32::try_from(s.right() - 1).unwrap_or(s.x),
            i32::try_from(s.bottom() - 1).unwrap_or(s.y),
        );
        let m = self.monitor_at(corner);
        Some(toolbar_layout(self.bar(), s, &m, chip))
    }

    /// The selected annotation's box and handles, and the caret, selected text and composition
    /// of the label being edited, inside the picture (Step 56).
    fn annotation_scene(&self) -> Option<AnnotationScene> {
        let pic = self.picture;
        let outline = self
            .ann
            .selected_bounds()
            .and_then(annotation::Bounds::to_rect)
            .and_then(|r| intersection(r, pic));
        let handles: Vec<Rect> = self
            .ann
            .selected_handles()
            .iter()
            .filter_map(|(_, c)| {
                let m = self.monitor_at((c.x as i32, c.y as i32));
                let h = i64::from(m.px(7));
                let (cx, cy) = (c.x.round() as i64, c.y.round() as i64);
                intersection(
                    rect_from(cx - h / 2, cy - h / 2, cx - h / 2 + h, cy - h / 2 + h),
                    pic,
                )
            })
            .collect();
        let marks = self.ann.text_marks();
        if outline.is_none() && handles.is_empty() && marks.is_none() {
            return None;
        }
        let clip = |v: Vec<Rect>| -> Vec<Rect> {
            v.into_iter().filter_map(|r| intersection(r, pic)).collect()
        };
        Some(AnnotationScene {
            outline,
            handles,
            caret: marks.as_ref().and_then(|m| intersection(m.caret, pic)),
            text_selection: marks
                .as_ref()
                .map(|m| clip(m.selection.clone()))
                .unwrap_or_default(),
            composition: marks
                .as_ref()
                .map(|m| clip(m.composition.clone()))
                .unwrap_or_default(),
        })
    }

    /// The size and position readout (device pixels): next to the selection (or the detected
    /// target), or next to the pointer while nothing is shown, with an optional second line.
    fn chip(&self, live: Option<Rect>, note: Option<&str>, measure: &dyn Measure) -> Label {
        let (text, anchor) = match live {
            Some(s) => (
                format!("{} \u{d7} {}  at {}, {}", s.width, s.height, s.x, s.y),
                (s.x, s.y),
            ),
            None => (
                format!("{}, {}", self.pointer.0, self.pointer.1),
                self.pointer,
            ),
        };
        let m = self.monitor_at(anchor);
        let mut lines = vec![text];
        lines.extend(note.map(str::to_string));
        let mut label = Label::new(lines, Font::Mono, m.dpi, measure);
        let (w, h) = (i64::from(label.rect.width), i64::from(label.rect.height));
        let mr = m.rect;
        let (ml, mt, mrr, mb) = (i64::from(mr.x), i64::from(mr.y), mr.right(), mr.bottom());
        let gap = i64::from(m.px(6));
        let (mut x, mut y) = match live {
            Some(s) => {
                let above = i64::from(s.y) - 2 - gap - h;
                if above >= mt {
                    (i64::from(s.x), above)
                } else {
                    (i64::from(s.x) + 2 + gap, i64::from(s.y) + 2 + gap)
                }
            }
            None => {
                let off = i64::from(m.px(16));
                let (px, py) = (i64::from(self.pointer.0), i64::from(self.pointer.1));
                let x = if px + off + w <= mrr {
                    px + off
                } else {
                    px - off - w
                };
                let y = if py + off + h <= mb {
                    py + off
                } else {
                    py - off - h
                };
                (x, y)
            }
        };
        x = x.min(mrr - w).max(ml);
        y = y.min(mb - h).max(mt);
        label.rect.x = i32::try_from(x).unwrap_or(mr.x);
        label.rect.y = i32::try_from(y).unwrap_or(mr.y);
        label
    }

    /// The first-run help (or a banner): top center of the monitor the pointer was on when the
    /// overlay opened, or its bottom when the selection or the toolbar is in the way.
    fn help_label(
        &self,
        lines: Vec<String>,
        live: Option<Rect>,
        bar: Option<Rect>,
        measure: &dyn Measure,
    ) -> Label {
        let m = self
            .monitors
            .get(self.help_monitor)
            .copied()
            .unwrap_or(Monitor::new(self.picture, true));
        let mut label = Label::new(lines, Font::Ui, m.dpi, measure);
        let mr = m.rect;
        let w = i64::from(label.rect.width);
        let h = i64::from(label.rect.height);
        let margin = i64::from(m.px(24));
        let x = (i64::from(mr.x) + (i64::from(mr.width) - w) / 2).max(i64::from(mr.x));
        let top = i64::from(mr.y) + margin;
        let bottom = (mr.bottom() - margin - h).max(i64::from(mr.y));
        label.rect.x = i32::try_from(x).unwrap_or(mr.x);
        label.rect.y = i32::try_from(top).unwrap_or(mr.y);
        let margin = i32::try_from(m.px(12)).unwrap_or(12);
        let in_the_way = |r: &Rect| {
            live.is_some_and(|s| r.intersects(&grow(s, margin)))
                || bar.is_some_and(|b| r.intersects(&grow(b, margin)))
        };
        if in_the_way(&label.rect) {
            let mut at_bottom = label.rect;
            at_bottom.y = i32::try_from(bottom).unwrap_or(mr.y);
            // The bottom unless the toolbar sits there too; then the top stays.
            if !bar.is_some_and(|b| at_bottom.intersects(&grow(b, margin))) {
                label.rect = at_bottom;
            }
        }
        label
    }
}

/// Where the toolbar and its buttons are (device pixels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarLayout {
    pub rect: Rect,
    /// One per button, in tab order.
    pub buttons: Vec<Rect>,
    pub dpi: u32,
}

/// Where a toolbar of `items` goes next to the selection `s` on monitor `m` (UX spec §3.1):
/// right-aligned with the selection, 6 logical pixels below it; above it when there is no room
/// below on that monitor (and above the size chip `chip` if they would meet); inside its bottom
/// edge when there is room neither below nor above (a selection filling the monitor). Buttons
/// wrap into more rows when the bar is wider than the monitor (Step 56). The pin editor sizes
/// its canvas with the same function.
pub fn toolbar_layout(
    items: &[ToolbarItem],
    s: Rect,
    m: &Monitor,
    chip: Option<Rect>,
) -> ToolbarLayout {
    let px = |l: u32| i64::from(m.px(l));
    let (pad, gap, group, bh) = (px(4), px(4), px(10), px(24));
    let mr = m.rect;
    let (ml, mt, mrr, mb) = (i64::from(mr.x), i64::from(mr.y), mr.right(), mr.bottom());
    let max_w = (mrr - ml).max(1);
    // Rows of (x offset in the bar, width).
    let mut rows: Vec<Vec<(i64, i64)>> = vec![Vec::new()];
    let mut end = pad;
    let mut widest = 0;
    for (i, item) in items.iter().enumerate() {
        let bw = px(item.width());
        let sep = match i.checked_sub(1).map(|j| items[j]) {
            Some(prev) if prev.group() != item.group() => group,
            Some(_) => gap,
            None => 0,
        };
        let row_empty = rows.last().is_none_or(Vec::is_empty);
        let x = if row_empty { pad } else { end + sep };
        if !row_empty && x + bw + pad > max_w {
            widest = widest.max(end + pad);
            rows.push(vec![(pad, bw)]);
            end = pad + bw;
        } else {
            if let Some(r) = rows.last_mut() {
                r.push((x, bw));
            }
            end = x + bw;
        }
    }
    widest = widest.max(end + pad);
    let n = rows.len() as i64;
    let (w, h) = (widest, 2 * pad + n * bh + (n - 1) * gap);
    let off = px(6) + 2; // beyond the border and its halo
    let fits = |y: i64| y >= mt && y + h <= mb;
    let clamp_x = |x: i64| x.min(mrr - w).max(ml);
    let mut x = clamp_x(s.right() - w);
    let below = s.bottom() + off;
    let above = i64::from(s.y) - off - h;
    let meets_chip =
        |x: i64, y: i64| chip.is_some_and(|c| rect_from(x, y, x + w, y + h).intersects(&c));
    let y = if fits(below) {
        below
    } else if fits(above) && !meets_chip(x, above) {
        above
    } else if let Some(c) = chip.filter(|_| fits(above))
        && fits(i64::from(c.y) - gap - h)
    {
        i64::from(c.y) - gap - h
    } else {
        // Inside the bottom edge, right-aligned (FLOW-38).
        x = clamp_x(s.right() - px(6) - w).max(i64::from(s.x).min(mrr - w).max(ml));
        (s.bottom() - px(6) - h).max(i64::from(s.y)).max(mt)
    };
    let rect = rect_from(x, y, x + w, y + h);
    let mut buttons = Vec::with_capacity(items.len());
    for (r, row) in rows.iter().enumerate() {
        let by = y + pad + r as i64 * (bh + gap);
        for (bx, bw) in row {
            buttons.push(rect_from(x + bx, by, x + bx + bw, by + bh));
        }
    }
    ToolbarLayout {
        rect,
        buttons,
        dpi: m.dpi,
    }
}

/// `p` moved inside `r`.
fn clamp_point(p: (i32, i32), r: Rect) -> (i32, i32) {
    let max_x = i32::try_from(r.right() - 1).unwrap_or(i32::MAX).max(r.x);
    let max_y = i32::try_from(r.bottom() - 1).unwrap_or(i32::MAX).max(r.y);
    (p.0.clamp(r.x, max_x), p.1.clamp(r.y, max_y))
}

#[derive(Debug, Clone, Copy)]
struct SelMetrics {
    half: u32,
    band: u32,
    handle: u32,
    small: bool,
}

/// The centers of the eight handles (corners and edge midpoints, on the edge lines), moved
/// outside a small selection so its whole area stays interior (UX spec §1).
fn handle_centers(s: Rect, m: &SelMetrics) -> [(Edges, (i64, i64)); 8] {
    let (l, r, t, b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
    let off = if m.small { i64::from(m.half) + 1 } else { 0 };
    let (xl, xr, yt, yb) = (l - off, r + off, t - off, b + off);
    let (xm, ym) = ((l + r) / 2, (t + b) / 2);
    let e = |left, right, top, bottom| Edges {
        left,
        right,
        top,
        bottom,
    };
    [
        (e(true, false, true, false), (xl, yt)),
        (e(false, true, true, false), (xr, yt)),
        (e(true, false, false, true), (xl, yb)),
        (e(false, true, false, true), (xr, yb)),
        (e(false, false, true, false), (xm, yt)),
        (e(false, false, false, true), (xm, yb)),
        (e(true, false, false, false), (xl, ym)),
        (e(false, true, false, false), (xr, ym)),
    ]
}

fn edge_cursor(e: Edges) -> Cursor {
    match (e.left || e.right, e.top || e.bottom) {
        (true, true) if (e.left && e.top) || (e.right && e.bottom) => Cursor::SizeNwse,
        (true, true) => Cursor::SizeNesw,
        (true, false) => Cursor::SizeWe,
        _ => Cursor::SizeNs,
    }
}

fn zone_cursor(z: Zone) -> Cursor {
    match z {
        Zone::Outside => Cursor::Cross,
        Zone::Interior => Cursor::Move,
        Zone::Edge(e) => edge_cursor(e),
        Zone::Toolbar(_) | Zone::Chip => Cursor::Arrow,
    }
}

fn step(d: Dir) -> (i32, i32) {
    match d {
        Dir::Up => (0, -1),
        Dir::Down => (0, 1),
        Dir::Left => (-1, 0),
        Dir::Right => (1, 0),
    }
}

/// `Shift`+arrow: shrink from the edge opposite to the arrow (the manifest's titles: `Shift+Up`
/// shrinks from the bottom edge) by `n` pixels, never below one pixel.
fn shrunk(s: Rect, d: Dir, n: i32) -> Rect {
    let n = i64::from(n.max(1));
    let (mut l, mut r, mut t, mut b) = (i64::from(s.x), s.right(), i64::from(s.y), s.bottom());
    match d {
        Dir::Up => b = (b - n).max(t + 1),
        Dir::Down => t = (t + n).min(b - 1),
        Dir::Left => r = (r - n).max(l + 1),
        Dir::Right => l = (l + n).min(r - 1),
    }
    rect_from(l, t, r, b)
}

/// Orders two edges and keeps at least one pixel between them, inside `lo..hi`.
fn span(a: i64, b: i64, lo: i64, hi: i64) -> (i64, i64) {
    let (mut a, mut b) = if a <= b { (a, b) } else { (b, a) };
    if a == b {
        if b < hi {
            b += 1;
        } else {
            a = (a - 1).max(lo);
        }
    }
    (a, b)
}

fn rect_from(l: i64, t: i64, r: i64, b: i64) -> Rect {
    Rect::new(
        i32::try_from(l).unwrap_or(i32::MAX),
        i32::try_from(t).unwrap_or(i32::MAX),
        u32::try_from((r - l).max(0)).unwrap_or(0),
        u32::try_from((b - t).max(0)).unwrap_or(0),
    )
}

/// `r` grown by `by` pixels on every side (may stick out of the picture).
pub fn grow(r: Rect, by: i32) -> Rect {
    let by64 = i64::from(by);
    rect_from(
        i64::from(r.x) - by64,
        i64::from(r.y) - by64,
        r.right() + by64,
        r.bottom() + by64,
    )
}

/// The overlap of two rectangles, if any.
pub fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let l = i64::from(a.x).max(i64::from(b.x));
    let t = i64::from(a.y).max(i64::from(b.y));
    let r = a.right().min(b.right());
    let bt = a.bottom().min(b.bottom());
    (l < r && t < bt).then(|| rect_from(l, t, r, bt))
}

/// `r` shrunk where it sticks out of `into` (an outline then runs along `into`'s edge).
fn clamp_into(r: Rect, into: Rect) -> Rect {
    intersection(r, into).unwrap_or(into)
}

/// `a` minus `b`, as up to four rectangles.
pub fn subtract(a: Rect, b: Rect) -> Vec<Rect> {
    let Some(i) = intersection(a, b) else {
        return if a.is_empty() { Vec::new() } else { vec![a] };
    };
    let (al, at, ar, ab) = (i64::from(a.x), i64::from(a.y), a.right(), a.bottom());
    let (il, it, ir, ib) = (i64::from(i.x), i64::from(i.y), i.right(), i.bottom());
    [
        rect_from(al, at, ar, it),
        rect_from(al, ib, ar, ab),
        rect_from(al, it, il, ib),
        rect_from(ir, it, ar, ib),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// A text font of Noeul's overlay (DESIGN_SYSTEM §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Font {
    /// The mono family at caption size (11): the size and position readout.
    Mono,
    /// The UI family at body size (12): the help and banners.
    Ui,
}

/// Measures one line of text in device pixels (GDI on Windows; [`Approx`] in tests).
pub trait Measure {
    /// `(width, height)` of `text` in `font` at `dpi`.
    fn text(&self, font: Font, dpi: u32, text: &str) -> (u32, u32);
}

/// A stand-in measure: 7 logical pixels per character, 15 per line.
#[derive(Debug, Clone, Copy, Default)]
pub struct Approx;

impl Measure for Approx {
    fn text(&self, _: Font, dpi: u32, text: &str) -> (u32, u32) {
        let n = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        (n.saturating_mul(scale_px(7, dpi)), scale_px(15, dpi))
    }
}

/// A box with lines of text: `bg.overlay` with a `border.subtle` outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub rect: Rect,
    pub lines: Vec<String>,
    pub font: Font,
    pub dpi: u32,
    /// Padding inside the box and the height of one line (device pixels).
    pub pad: u32,
    pub line_height: u32,
}

impl Label {
    fn new(lines: Vec<String>, font: Font, dpi: u32, measure: &dyn Measure) -> Label {
        let pad = scale_px(if font == Font::Mono { 4 } else { 8 }, dpi);
        let mut w = 0;
        let mut lh = 1;
        for l in &lines {
            let (tw, th) = measure.text(font, dpi, l);
            w = w.max(tw);
            lh = lh.max(th);
        }
        let n = u32::try_from(lines.len()).unwrap_or(1).max(1);
        Label {
            rect: Rect::new(0, 0, w + 2 * pad, lh * n + 2 * pad),
            lines,
            font,
            dpi,
            pad,
            line_height: lh,
        }
    }

    /// Where line `i` starts.
    pub fn line_origin(&self, i: usize) -> (i32, i32) {
        let pad = i32::try_from(self.pad).unwrap_or(0);
        let lh = i32::try_from(self.line_height).unwrap_or(0);
        (
            self.rect.x + pad,
            self.rect.y + pad + lh * i32::try_from(i).unwrap_or(0),
        )
    }
}

/// What the overlay shows. Every rectangle is in device pixels of the virtual desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    /// The frozen picture's rectangle (the whole overlay).
    pub picture: Rect,
    /// Shown undimmed.
    pub selection: Option<Rect>,
    /// One-pixel outlines: the accent border and its dark halo (UX spec §10).
    pub accent: Option<Rect>,
    pub halo: Option<Rect>,
    /// Resize handles (accent squares with a halo outline), only on a confirmed selection.
    pub handles: Vec<Rect>,
    /// The size and position readout.
    pub chip: Option<Label>,
    /// The first-run keyboard help, or a banner.
    pub help: Option<Label>,
    /// The toolbar of a confirmed selection.
    pub toolbar: Option<ToolbarScene>,
    /// The magnifier near the pointer (Step 52).
    pub magnifier: Option<MagnifierScene>,
    /// The selected annotation's box and handles, the caret, selected text and composition of a
    /// label being edited (Step 56).
    pub annotation: Option<AnnotationScene>,
    /// Pin editing: a layered window, so pixels are premultiplied and the canvas outside the
    /// pin's picture stays transparent (Step 56).
    pub layered: bool,
}

/// What the overlay draws for annotation editing, over the annotated pixels (Step 56).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnnotationScene {
    /// The box around the selected annotation (or the label being edited).
    pub outline: Option<Rect>,
    pub handles: Vec<Rect>,
    pub caret: Option<Rect>,
    pub text_selection: Vec<Rect>,
    /// The underline of an input-method composition.
    pub composition: Vec<Rect>,
}

/// The note of the size chip over a window protected from capture (NOEUL-SEC-004).
pub const PROTECTED_NOTE: &str = "Protected window: Windows captures it as black";

/// The widest colour lines (HEX, then RGB), for sizing the magnifier's readout before it is
/// sampled.
const COLOR_TEMPLATE: [&str; 2] = ["#000000", "255, 255, 255"];

/// The magnifier as drawn: a `bg.overlay` box with a `border.subtle` outline holding
/// [`MAG_CELLS`]² enlarged pixels of the frozen picture around `at` (each `cell` device pixels
/// wide, pixels outside the picture dark), a crosshair through the centre pixel, and the readout:
/// the device-pixel position, on a scaled monitor the logical one, then the colour as HEX and
/// RGB (filled in by [`Scene::sample`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagnifierScene {
    pub rect: Rect,
    pub grid: Rect,
    pub cell: u32,
    /// The picture pixel in the centre (the pointer).
    pub at: (i32, i32),
    pub text: Label,
    /// The index of the HEX line in `text` (RGB follows it).
    pub color: usize,
}

impl MagnifierScene {
    /// The centre cell (the pointer's pixel).
    pub fn centre(&self) -> Rect {
        let half = i32::try_from((MAG_CELLS / 2) * self.cell).unwrap_or(0);
        Rect::new(self.grid.x + half, self.grid.y + half, self.cell, self.cell)
    }
}

/// One pixel of the frozen picture (BGRA), if `(x, y)` lies in it.
pub fn pixel_at(frozen: &Frame, (x, y): (i32, i32)) -> Option<[u8; 4]> {
    let r = frozen.rect();
    if !r.contains(&Rect::new(x, y, 1, 1)) {
        return None;
    }
    let i = ((i64::from(y) - i64::from(r.y)) as usize * r.width as usize
        + (i64::from(x) - i64::from(r.x)) as usize)
        * 4;
    frozen
        .pixels()
        .get(i..i + 4)
        .map(|p| [p[0], p[1], p[2], p[3]])
}

/// The magnifier's colour lines for the frozen picture's pixel at `at`: HEX (`#3A6EA5`) and RGB
/// (`58, 110, 165`) of the captured sRGB values (NOEUL-FR-041); dashes outside the picture.
pub fn color_lines(frozen: &Frame, at: (i32, i32)) -> [String; 2] {
    match pixel_at(frozen, at) {
        Some([b, g, r, _]) => [format!("#{r:02X}{g:02X}{b:02X}"), format!("{r}, {g}, {b}")],
        None => ["\u{2014}".to_string(), "\u{2014}".to_string()],
    }
}

impl Scene {
    /// Fills in what depends on the frozen picture's pixels: the magnifier's colour lines.
    pub fn sample(&mut self, frozen: &Frame) {
        if let Some(m) = &mut self.magnifier {
            let [hex, rgb] = color_lines(frozen, m.at);
            if let Some(l) = m.text.lines.get_mut(m.color) {
                *l = hex;
            }
            if let Some(l) = m.text.lines.get_mut(m.color + 1) {
                *l = rgb;
            }
        }
    }
}

/// The toolbar as drawn: a `bg.overlay` bar with a `border.subtle` outline, the primary Pin
/// button in `accent`, the others flat (`bg.hover` under the pointer), a 2-pixel `focus.ring`
/// around the button with keyboard focus. The platform draws the labels' text, centered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolbarScene {
    pub rect: Rect,
    pub dpi: u32,
    pub buttons: Vec<ButtonScene>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ButtonScene {
    pub rect: Rect,
    /// The text of a text button (`""` for an icon button).
    pub label: &'static str,
    pub item: ToolbarItem,
    /// The accent-filled primary button (Pin, or Done when editing a pin).
    pub primary: bool,
    pub hover: bool,
    pub pressed: bool,
    pub focus: bool,
    /// The active tool (Step 56).
    pub active: bool,
    /// Undo or Redo with nothing to undo or redo.
    pub disabled: bool,
    /// The current annotation colour and width level (the colour, width and marker icons).
    pub color: annotation::Rgba,
    pub level: u8,
}

impl ButtonScene {
    /// Where the focus ring is drawn: 2 pixels wide, 1 pixel away from the button.
    pub fn ring(&self) -> Rect {
        grow(self.rect, 3)
    }
}

impl Scene {
    /// Rectangles drawn over the dimmed or plain picture.
    fn parts(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        if let (Some(a), Some(h)) = (self.accent, self.halo) {
            out.extend(outline_strips(h, 2));
            out.extend(outline_strips(a, 1));
        }
        out.extend(self.handles.iter().copied());
        out.extend(self.chip.iter().map(|l| l.rect));
        out.extend(self.help.iter().map(|l| l.rect));
        if let Some(t) = &self.toolbar {
            out.push(t.rect);
            out.extend(t.buttons.iter().map(ButtonScene::ring));
        }
        out.extend(self.magnifier.iter().map(|m| m.rect));
        if let Some(a) = &self.annotation {
            if let Some(o) = a.outline {
                out.extend(outline_strips(grow(o, 1), 1));
            }
            out.extend(a.handles.iter().copied());
            out.extend(a.caret);
            out.extend(a.text_selection.iter().copied());
            out.extend(a.composition.iter().copied());
        }
        out
    }
}

/// The four strips of an outline `width` pixels thick along the inside of `r`.
fn outline_strips(r: Rect, width: u32) -> Vec<Rect> {
    let w = i64::from(width);
    let (l, t, rr, b) = (i64::from(r.x), i64::from(r.y), r.right(), r.bottom());
    [
        rect_from(l, t, rr, (t + w).min(b)),
        rect_from(l, (b - w).max(t), rr, b),
        rect_from(l, t, (l + w).min(rr), b),
        rect_from((rr - w).max(l), t, rr, b),
    ]
    .into_iter()
    .filter(|r| !r.is_empty())
    .collect()
}

/// The pixels that differ between two scenes: every part of both, and where the undimmed area
/// changed. Clipped to the picture; may overlap.
pub fn dirty(old: &Scene, new: &Scene) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut push = |r: Rect| {
        if let Some(r) = intersection(r, new.picture)
            && !out.contains(&r)
        {
            out.push(r);
        }
    };
    if old.selection != new.selection {
        match (old.selection, new.selection) {
            (Some(a), Some(b)) => {
                subtract(a, b).into_iter().for_each(&mut push);
                subtract(b, a).into_iter().for_each(&mut push);
            }
            (Some(a), None) | (None, Some(a)) => push(a),
            (None, None) => {}
        }
    }
    if old != new {
        for r in old.parts().into_iter().chain(new.parts()) {
            push(r);
        }
    }
    out
}

/// BGRA colors of the overlay (DESIGN_SYSTEM §2: `accent`, `text.on-accent` as the halo,
/// `bg.overlay`, `border.subtle`).
pub mod color {
    pub const ACCENT: [u8; 4] = [0xF5, 0xA6, 0xB7, 0xFF];
    /// `accent.hover` and `accent.pressed` (the Pin button).
    pub const ACCENT_HOVER: [u8; 4] = [0xFA, 0xB9, 0xC7, 0xFF];
    pub const ACCENT_PRESSED: [u8; 4] = [0xF0, 0x86, 0x9B, 0xFF];
    pub const HALO: [u8; 4] = [0x12, 0x0E, 0x0E, 0xFF];
    pub const BG_OVERLAY: [u8; 4] = [0x24, 0x1B, 0x1C, 0xFF];
    /// `bg.hover` and `bg.selected` (the other toolbar buttons).
    pub const BG_HOVER: [u8; 4] = [0x2C, 0x21, 0x22, 0xFF];
    pub const BG_SELECTED: [u8; 4] = [0x38, 0x28, 0x2A, 0xFF];
    pub const BORDER_SUBTLE: [u8; 4] = [0x2F, 0x25, 0x26, 0xFF];
    /// `focus.ring`.
    pub const FOCUS_RING: [u8; 4] = [0xFF, 0xBC, 0xC9, 0xFF];
    /// `text.primary` and `text.secondary` as RGB.
    pub const TEXT_PRIMARY: [u8; 3] = [0xE8, 0xE6, 0xF0];
    pub const TEXT_SECONDARY: [u8; 3] = [0xA9, 0xA6, 0xB8];
    /// `text.on-accent` as RGB (the Pin button's label).
    pub const TEXT_ON_ACCENT: [u8; 3] = [0x0E, 0x0E, 0x12];
    /// `text.primary` as BGRA (annotation handles, the colour swatch's frame).
    pub const TEXT_PRIMARY_BGRA: [u8; 4] = [0xF0, 0xE6, 0xE8, 0xFF];
}

/// Brightness kept outside the selection: a black veil of about 45 % (UX spec §10), as n/256.
pub const DIM: u32 = 141;

/// One dimmed BGRA pixel (alpha opaque).
#[inline]
pub fn dim_pixel(p: [u8; 4]) -> [u8; 4] {
    let d = |c: u8| u8::try_from((u32::from(c) * DIM) >> 8).unwrap_or(255);
    [d(p[0]), d(p[1]), d(p[2]), 0xFF]
}

/// One premultiplied BGRA pixel (a layered window's format).
#[inline]
pub fn premultiply(p: [u8; 4]) -> [u8; 4] {
    let a = u32::from(p[3]);
    if a == 255 {
        return p;
    }
    let m = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
    [m(p[0]), m(p[1]), m(p[2]), p[3]]
}

/// Writes the pixels of `scene` inside `area` into `out`, a BGRA buffer of the frozen picture's
/// size and position (`frozen.rect()`): the picture dimmed outside the selection and plain inside
/// it — inside, the pixels of `layer` (the annotated selection, Step 56) where it has them — then
/// the outlines, handles, label boxes, toolbar icons and annotation marks. In a layered scene
/// (pin editing) pixels are premultiplied and transparency is kept. Label text is drawn by the
/// platform after this, clipped to `area`.
pub fn compose(frozen: &Frame, layer: Option<&Frame>, scene: &Scene, area: Rect, out: &mut [u8]) {
    let pic = frozen.rect();
    let Some(area) = intersection(area, pic) else {
        return;
    };
    let stride = pic.width as usize * 4;
    if out.len() < stride * pic.height as usize {
        return;
    }
    let src = frozen.pixels();
    let ox = (i64::from(area.x) - i64::from(pic.x)) as usize;
    let oy = (i64::from(area.y) - i64::from(pic.y)) as usize;
    let w = area.width as usize;
    let sel = scene.selection.and_then(|s| intersection(s, area));
    let layered = scene.layered;
    let shown = |p: [u8; 4]| if layered { premultiply(p) } else { p };
    for row in 0..area.height as usize {
        let y = oy + row;
        let start = y * stride + ox * 4;
        let line_src = &src[start..start + w * 4];
        let line_out = &mut out[start..start + w * 4];
        let ay = i64::from(area.y) + row as i64;
        let (k0, k1) = match sel {
            Some(s) if ay >= i64::from(s.y) && ay < s.bottom() => (
                (i64::from(s.x) - i64::from(area.x)) as usize,
                (s.right() - i64::from(area.x)) as usize,
            ),
            _ => (w, w),
        };
        for (i, (o, s)) in line_out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(line_src.as_chunks::<4>().0)
            .enumerate()
        {
            *o = if i >= k0 && i < k1 {
                shown(*s)
            } else if layered {
                // Dimmed but as transparent as it was (the canvas around a pin stays clear).
                let p = premultiply(*s);
                let d = dim_pixel(p);
                [d[0], d[1], d[2], p[3]]
            } else {
                dim_pixel(*s)
            };
        }
        // The annotated pixels inside the selection.
        if k0 < k1
            && let Some(l) = layer
        {
            let lr = l.rect();
            if ay >= i64::from(lr.y) && ay < lr.bottom() {
                let x0 = (i64::from(area.x) + k0 as i64).max(i64::from(lr.x));
                let x1 = (i64::from(area.x) + k1 as i64).min(lr.right());
                if x0 < x1 {
                    let lstride = lr.width as usize * 4;
                    let lrow = (ay - i64::from(lr.y)) as usize * lstride;
                    let lpx = l.pixels();
                    for x in x0..x1 {
                        let li = lrow + (x - i64::from(lr.x)) as usize * 4;
                        let oi = (x - i64::from(area.x)) as usize * 4;
                        let p = [lpx[li], lpx[li + 1], lpx[li + 2], lpx[li + 3]];
                        line_out[oi..oi + 4].copy_from_slice(&shown(p));
                    }
                }
            }
        }
    }
    let mut fill = |r: Rect, c: [u8; 4]| {
        let Some(r) = intersection(r, area) else {
            return;
        };
        let x0 = (i64::from(r.x) - i64::from(pic.x)) as usize;
        let y0 = (i64::from(r.y) - i64::from(pic.y)) as usize;
        for y in y0..y0 + r.height as usize {
            let s = y * stride + x0 * 4;
            for p in out[s..s + r.width as usize * 4].as_chunks_mut::<4>().0 {
                *p = c;
            }
        }
    };
    if let Some(h) = scene.halo {
        outline_strips(h, 1)
            .into_iter()
            .for_each(|r| fill(r, color::HALO));
    }
    if let Some(a) = scene.accent {
        outline_strips(a, 1)
            .into_iter()
            .for_each(|r| fill(r, color::ACCENT));
    }
    for h in &scene.handles {
        fill(*h, color::HALO);
        fill(grow(*h, -1), color::ACCENT);
    }
    for l in scene.chip.iter().chain(scene.help.iter()) {
        fill(l.rect, color::BORDER_SUBTLE);
        fill(grow(l.rect, -1), color::BG_OVERLAY);
    }
    if let Some(t) = &scene.toolbar {
        fill(t.rect, color::BORDER_SUBTLE);
        fill(grow(t.rect, -1), color::BG_OVERLAY);
        for b in &t.buttons {
            let c = match (b.primary, b.pressed, b.hover) {
                (true, true, _) => color::ACCENT_PRESSED,
                (true, false, true) => color::ACCENT_HOVER,
                (true, false, false) => color::ACCENT,
                (false, true, _) => color::BG_SELECTED,
                (false, false, _) if b.active => color::BG_SELECTED,
                (false, false, true) => color::BG_HOVER,
                (false, false, false) => color::BG_OVERLAY,
            };
            fill(b.rect, c);
            if b.active {
                // The active tool: an accent bar under its icon.
                let m = scale_px(2, t.dpi);
                fill(
                    Rect::new(
                        b.rect.x + i32::try_from(m * 2).unwrap_or(0),
                        b.rect.y + i32::try_from(b.rect.height.saturating_sub(m)).unwrap_or(0),
                        b.rect.width.saturating_sub(m * 4),
                        m,
                    ),
                    color::ACCENT,
                );
            }
            if b.item == ToolbarItem::Color {
                let side = scale_px(12, t.dpi);
                let sw = Rect::new(
                    b.rect.x + i32::try_from((b.rect.width.saturating_sub(side)) / 2).unwrap_or(0),
                    b.rect.y + i32::try_from((b.rect.height.saturating_sub(side)) / 2).unwrap_or(0),
                    side,
                    side,
                );
                fill(sw, color::TEXT_PRIMARY_BGRA);
                fill(grow(sw, -1), [b.color.b, b.color.g, b.color.r, 0xFF]);
            }
            if b.focus {
                for r in outline_strips(b.ring(), 2) {
                    fill(r, color::FOCUS_RING);
                }
            }
        }
    }
    if let Some(a) = &scene.annotation {
        if let Some(o) = a.outline {
            for r in outline_strips(grow(o, 1), 1) {
                fill(r, color::ACCENT);
            }
        }
        for h in &a.handles {
            fill(*h, color::HALO);
            fill(grow(*h, -1), color::TEXT_PRIMARY_BGRA);
        }
        for r in a.caret.iter().chain(&a.composition) {
            fill(*r, color::ACCENT);
        }
    }
    if let Some(m) = &scene.magnifier {
        fill(m.rect, color::BORDER_SUBTLE);
        fill(grow(m.rect, -1), color::BG_OVERLAY);
        // The enlarged pixels: undimmed, opaque; outside the picture a dark cell.
        let half = i64::from(MAG_CELLS / 2);
        let cell = i64::from(m.cell);
        for j in 0..i64::from(MAG_CELLS) {
            for i in 0..i64::from(MAG_CELLS) {
                let x0 = i64::from(m.grid.x) + i * cell;
                let y0 = i64::from(m.grid.y) + j * cell;
                let cr = rect_from(x0, y0, x0 + cell, y0 + cell);
                if !cr.intersects(&area) {
                    continue;
                }
                let src = (
                    i32::try_from(i64::from(m.at.0) - half + i).unwrap_or(i32::MAX),
                    i32::try_from(i64::from(m.at.1) - half + j).unwrap_or(i32::MAX),
                );
                let c = pixel_at(frozen, src).map_or(color::HALO, |p| [p[0], p[1], p[2], 0xFF]);
                fill(cr, c);
            }
        }
        // The crosshair: one device pixel through the middle of the centre row and column,
        // stopping at the centre cell, which gets an accent outline with a dark halo.
        let c = m.centre();
        let (cx, cy) = (i64::from(c.x) + cell / 2, i64::from(c.y) + cell / 2);
        let (gl, gt, gr, gb) = (
            i64::from(m.grid.x),
            i64::from(m.grid.y),
            m.grid.right(),
            m.grid.bottom(),
        );
        for r in [
            rect_from(cx, gt, cx + 1, i64::from(c.y) - 1),
            rect_from(cx, c.bottom() + 1, cx + 1, gb),
            rect_from(gl, cy, i64::from(c.x) - 1, cy + 1),
            rect_from(c.right() + 1, cy, gr, cy + 1),
        ] {
            fill(r, color::ACCENT);
        }
        for r in outline_strips(grow(c, 1), 1) {
            fill(r, color::HALO);
        }
        for r in outline_strips(c, 1) {
            fill(r, color::ACCENT);
        }
    }
    if let Some(a) = &scene.annotation {
        for r in &a.text_selection {
            blend(out, pic, area, *r, color::ACCENT, 90);
        }
    }
    if let Some(t) = &scene.toolbar {
        for b in &t.buttons {
            if let Some(clip) = intersection(b.rect, area) {
                let objects = icon(b, t.dpi);
                annotation::draw(&objects, out, pic, clip);
            }
        }
    }
}

/// `c` at `alpha`/255 over the pixels of `r` inside `area` (a selected range of text).
fn blend(out: &mut [u8], pic: Rect, area: Rect, r: Rect, c: [u8; 4], alpha: u32) {
    let Some(r) = intersection(r, area) else {
        return;
    };
    let stride = pic.width as usize * 4;
    let x0 = (i64::from(r.x) - i64::from(pic.x)) as usize;
    let y0 = (i64::from(r.y) - i64::from(pic.y)) as usize;
    for y in y0..y0 + r.height as usize {
        let s = y * stride + x0 * 4;
        for p in out[s..s + r.width as usize * 4].as_chunks_mut::<4>().0 {
            for k in 0..3 {
                p[k] =
                    ((u32::from(c[k]) * alpha + u32::from(p[k]) * (255 - alpha) + 127) / 255) as u8;
            }
        }
    }
}

/// The vector icon of an icon button (original Noeul artwork, drawn with the annotation
/// rasterizer): 16 × 16 logical pixels in the middle of the button.
fn icon(b: &ButtonScene, dpi: u32) -> Vec<Object> {
    use annotation::{Area, Effect, P, Rgba, Shape};
    let k = dpi as f32 / 96.0;
    let (cx, cy) = (
        b.rect.x as f32 + b.rect.width as f32 / 2.0,
        b.rect.y as f32 + b.rect.height as f32 / 2.0,
    );
    let pt = |x: f32, y: f32| P::new(cx + (x - 8.0) * k, cy + (y - 8.0) * k);
    let [r, g, bl] = if b.active {
        [color::ACCENT[2], color::ACCENT[1], color::ACCENT[0]]
    } else if b.disabled {
        color::TEXT_SECONDARY
    } else {
        color::TEXT_PRIMARY
    };
    let ink = Rgba {
        r,
        g,
        b: bl,
        a: if b.disabled { 120 } else { 255 },
    };
    let obj = |shape: Shape, level: u8, c: Rgba| Object {
        id: 0,
        shape,
        color: c,
        level,
        dpi,
    };
    let line = |a: (f32, f32), z: (f32, f32)| obj(Shape::Line(pt(a.0, a.1), pt(z.0, z.1)), 0, ink);
    let boxed = |l: f32, t: f32, rr: f32, bb: f32| {
        annotation::Bounds::new(
            (cx + (l - 8.0) * k).round(),
            (cy + (t - 8.0) * k).round(),
            (cx + (rr - 8.0) * k).round(),
            (cy + (bb - 8.0) * k).round(),
        )
    };
    match b.item {
        ToolbarItem::Tool(Tool::Rectangle) => {
            vec![obj(Shape::Rectangle(boxed(2.0, 3.0, 14.0, 13.0)), 0, ink)]
        }
        ToolbarItem::Tool(Tool::Ellipse) => {
            vec![obj(Shape::Ellipse(boxed(1.0, 3.0, 15.0, 13.0)), 0, ink)]
        }
        ToolbarItem::Tool(Tool::Line) => vec![line((3.0, 13.0), (13.0, 3.0))],
        ToolbarItem::Tool(Tool::Arrow) => vec![
            line((3.0, 13.0), (13.0, 3.0)),
            line((13.0, 3.0), (7.5, 3.0)),
            line((13.0, 3.0), (13.0, 8.5)),
        ],
        ToolbarItem::Tool(Tool::Pen) => vec![obj(
            Shape::Pen(vec![
                pt(2.0, 12.0),
                pt(5.0, 6.0),
                pt(8.0, 11.0),
                pt(11.0, 5.0),
                pt(14.0, 10.0),
            ]),
            0,
            ink,
        )],
        ToolbarItem::Tool(Tool::Marker) => vec![obj(
            Shape::Marker(vec![pt(3.0, 8.0), pt(13.0, 8.0)]),
            0,
            b.color,
        )],
        ToolbarItem::Tool(Tool::Text) => {
            vec![line((3.0, 3.0), (13.0, 3.0)), line((8.0, 3.0), (8.0, 13.0))]
        }
        // Step 57: a checkerboard, a soft disc, an eraser block, a solid bar.
        ToolbarItem::Tool(Tool::Mosaic) => {
            let mut v = vec![obj(Shape::Rectangle(boxed(2.0, 2.0, 14.0, 14.0)), 0, ink)];
            for j in 0..4 {
                for i in 0..4 {
                    if (i + j) % 2 == 0 {
                        let (l, t) = (2.0 + 3.0 * i as f32, 2.0 + 3.0 * j as f32);
                        v.push(obj(
                            Shape::Effect(Effect::Redact, Area::Box(boxed(l, t, l + 3.0, t + 3.0))),
                            0,
                            ink,
                        ));
                    }
                }
            }
            v
        }
        ToolbarItem::Tool(Tool::Blur) => vec![
            obj(Shape::Ellipse(boxed(2.0, 2.0, 14.0, 14.0)), 0, ink),
            obj(Shape::Marker(vec![pt(8.0, 8.0)]), 0, ink),
        ],
        ToolbarItem::Tool(Tool::Eraser) => vec![
            obj(
                Shape::Pen(vec![
                    pt(2.0, 10.0),
                    pt(8.0, 4.0),
                    pt(13.0, 9.0),
                    pt(7.0, 15.0),
                    pt(2.0, 10.0),
                ]),
                0,
                ink,
            ),
            line((5.0, 7.0), (10.0, 12.0)),
            line((9.0, 15.0), (15.0, 15.0)),
        ],
        ToolbarItem::Tool(Tool::Redact) => vec![obj(
            Shape::Effect(Effect::Redact, Area::Box(boxed(2.0, 4.0, 14.0, 12.0))),
            0,
            ink,
        )],
        ToolbarItem::Width => vec![obj(
            Shape::Line(pt(3.0, 8.0), pt(13.0, 8.0)),
            b.level.min(4),
            ink,
        )],
        ToolbarItem::Undo | ToolbarItem::Redo => {
            let f = |x: f32| {
                if b.item == ToolbarItem::Undo {
                    x
                } else {
                    16.0 - x
                }
            };
            vec![
                obj(
                    Shape::Pen(vec![
                        pt(f(4.0), 6.0),
                        pt(f(10.0), 6.0),
                        pt(f(13.0), 9.0),
                        pt(f(10.0), 12.0),
                        pt(f(5.0), 12.0),
                    ]),
                    0,
                    ink,
                ),
                line((f(4.0), 6.0), (f(7.0), 3.0)),
                line((f(4.0), 6.0), (f(7.0), 9.0)),
            ]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Synthetic;
    use crate::frame::Ledger;

    fn one(w: u32, h: u32) -> Overlay {
        // The Step 50/51 tests look at the selection; the magnifier has its own tests.
        Overlay::new(
            Rect::new(0, 0, w, h),
            vec![Monitor::new(Rect::new(0, 0, w, h), true)],
            SystemValues::default(),
            (5, 5),
            None,
            None,
        )
        .with_magnifier(false)
    }

    fn mv(o: &mut Overlay, x: i32, y: i32) -> Outcome {
        o.handle(Input::Move {
            x,
            y,
            mods: Mods::NONE,
        })
    }

    fn press(o: &mut Overlay, b: Button, x: i32, y: i32) -> Outcome {
        o.handle(Input::Press {
            button: b,
            x,
            y,
            mods: Mods::NONE,
            time_ms: 0,
        })
    }

    fn release(o: &mut Overlay, x: i32, y: i32) -> Outcome {
        o.handle(Input::Release {
            button: Button::Left,
            x,
            y,
        })
    }

    fn drag(o: &mut Overlay, from: (i32, i32), to: (i32, i32)) {
        press(o, Button::Left, from.0, from.1);
        mv(o, (from.0 + to.0) / 2, (from.1 + to.1) / 2);
        mv(o, to.0, to.1);
        release(o, to.0, to.1);
    }

    #[test]
    fn press_drag_release_selects_inclusive_pixels_and_tracks_the_pointer() {
        let mut o = one(800, 600);
        assert_eq!(o.phase(), Phase::Hover);
        press(&mut o, Button::Left, 100, 100);
        assert_eq!(o.phase(), Phase::Pressed);
        mv(&mut o, 300, 250);
        assert_eq!(o.phase(), Phase::Dragging);
        assert_eq!(o.live(), Some(Rect::new(100, 100, 201, 151)), "live band");
        assert_eq!(o.selection(), None, "not confirmed while dragging");
        mv(&mut o, 499, 399);
        release(&mut o, 499, 399);
        assert_eq!(o.phase(), Phase::Selected);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 400, 300)));
        // Dragging up and left works the same way.
        drag(&mut o, (50, 60), (10, 20));
        assert_eq!(o.selection(), Some(Rect::new(10, 20, 41, 41)));
        // The pointer is clamped to the picture.
        drag(&mut o, (700, 500), (5000, -40));
        assert_eq!(o.selection(), Some(Rect::new(700, 0, 100, 501)));
    }

    #[test]
    fn the_drag_starts_only_beyond_the_users_threshold_whatever_the_pause() {
        let mut o = one(200, 200);
        let sys = SystemValues {
            drag: (10, 10),
            ..SystemValues::default()
        };
        o.sys = sys;
        press(&mut o, Button::Left, 50, 50);
        mv(&mut o, 55, 50); // within ±5
        assert_eq!(o.phase(), Phase::Pressed);
        // A long pause with the button held changes nothing (there is no timer); the next
        // movement beyond the threshold starts the drag from the press point.
        mv(&mut o, 56, 50);
        assert_eq!(o.phase(), Phase::Dragging);
        assert_eq!(o.live(), Some(Rect::new(50, 50, 7, 1)));
        release(&mut o, 56, 50);
        // A click in place (released within the threshold) selects nothing.
        let mut c = one(200, 200);
        press(&mut c, Button::Left, 30, 30);
        mv(&mut c, 31, 31);
        release(&mut c, 31, 31);
        assert_eq!((c.phase(), c.selection()), (Phase::Hover, None));
        // A press right after opening, before any move, starts where it happened.
        let mut d = one(200, 200);
        press(&mut d, Button::Left, 120, 80);
        mv(&mut d, 140, 90);
        release(&mut d, 140, 90);
        assert_eq!(d.selection(), Some(Rect::new(120, 80, 21, 11)));
    }

    #[test]
    fn shift_makes_a_square_inside_the_picture() {
        let mut o = one(300, 200);
        o.handle(Input::Press {
            button: Button::Left,
            x: 100,
            y: 100,
            mods: Mods::SHIFT,
            time_ms: 0,
        });
        o.handle(Input::Move {
            x: 160,
            y: 120,
            mods: Mods::SHIFT,
        });
        assert_eq!(o.live(), Some(Rect::new(100, 100, 61, 61)));
        // Towards the bottom edge the square shrinks to what fits.
        o.handle(Input::Move {
            x: 290,
            y: 150,
            mods: Mods::SHIFT,
        });
        assert_eq!(o.live(), Some(Rect::new(100, 100, 100, 100)));
        // Releasing Shift during the drag frees the shape again.
        mv(&mut o, 130, 110);
        assert_eq!(o.live(), Some(Rect::new(100, 100, 31, 11)));
    }

    #[test]
    fn escape_right_click_and_focus_loss_cancel() {
        let mut o = one(100, 100);
        assert_eq!(press(&mut o, Button::Right, 10, 10), Outcome::Cancel);
        let mut o = one(100, 100);
        drag(&mut o, (10, 10), (40, 40));
        assert_eq!(o.handle(Input::Leave), Outcome::Cancel);
        assert_eq!(o.handle(Input::FocusLost), Outcome::Cancel);
        assert_eq!(o.handle(Input::Action(Action::Cancel)), Outcome::Cancel);
        // Escape cancels even in the middle of a drag.
        let mut o = one(100, 100);
        press(&mut o, Button::Left, 10, 10);
        mv(&mut o, 50, 50);
        assert_eq!(o.handle(Input::Leave), Outcome::Cancel);
    }

    #[test]
    fn right_click_outside_clears_inside_keeps_and_during_a_drag_undoes_it() {
        let mut o = one(400, 400);
        drag(&mut o, (100, 100), (199, 199));
        let sel = o.selection();
        assert_eq!(
            press(&mut o, Button::Right, 150, 150),
            Outcome::Menu(Some((150, 150))),
            "inside: the selection's context menu"
        );
        assert_eq!(o.selection(), sel, "inside: kept");
        assert_eq!(press(&mut o, Button::Right, 350, 350), Outcome::Stay);
        assert_eq!((o.phase(), o.selection()), (Phase::Hover, None), "cleared");
        // A new drag over an old selection, undone with a right-click, restores it.
        drag(&mut o, (100, 100), (199, 199));
        press(&mut o, Button::Left, 300, 300);
        mv(&mut o, 380, 380);
        assert_eq!(o.live(), Some(Rect::new(300, 300, 81, 81)));
        press(&mut o, Button::Right, 380, 380);
        assert_eq!(o.selection(), sel, "the drag is undone");
        assert_eq!(o.phase(), Phase::Selected);
        // A lost pointer capture undoes the drag the same way.
        press(&mut o, Button::Left, 300, 300);
        mv(&mut o, 380, 380);
        o.handle(Input::CaptureLost);
        assert_eq!(o.selection(), sel);
    }

    #[test]
    fn a_drag_outside_replaces_inside_moves_and_handles_resize() {
        let mut o = one(800, 600);
        drag(&mut o, (100, 100), (499, 399)); // (100,100) 400×300
        // Move by dragging the interior.
        drag(&mut o, (300, 250), (320, 260));
        assert_eq!(o.selection(), Some(Rect::new(120, 110, 400, 300)));
        // The right handle (edge midpoint at x = 520, y = 260) resizes the right edge.
        assert_eq!(
            o.zone((520, 260)),
            Zone::Edge(Edges {
                right: true,
                ..Edges::default()
            })
        );
        drag(&mut o, (520, 260), (560, 300));
        assert_eq!(o.selection(), Some(Rect::new(120, 110, 440, 300)));
        // The top-left corner handle moves two edges.
        drag(&mut o, (120, 110), (100, 90));
        assert_eq!(o.selection(), Some(Rect::new(100, 90, 460, 320)));
        // The border band on the bottom edge (not at a handle).
        assert_eq!(
            o.zone((200, 411)),
            Zone::Edge(Edges {
                bottom: true,
                ..Edges::default()
            })
        );
        // Dragging an edge across the opposite one flips, never below one pixel.
        drag(&mut o, (330, 90), (330, 500));
        let s = o.selection().unwrap();
        assert_eq!((s.y, s.height), (410, 90));
        // A drag that starts outside starts a new selection that replaces the old one.
        drag(&mut o, (700, 50), (760, 80));
        assert_eq!(o.selection(), Some(Rect::new(700, 50, 61, 31)));
        // Moving stays inside the picture.
        drag(&mut o, (730, 60), (2000, 2000));
        assert_eq!(o.selection(), Some(Rect::new(739, 569, 61, 31)));
    }

    #[test]
    fn zones_and_cursors_follow_dpi_and_small_selections() {
        // 150 %: handle zones 14 px (half 7), band 5 px, small below 41 px.
        let mut o = Overlay::new(
            Rect::new(0, 0, 1000, 1000),
            vec![Monitor::new(Rect::new(0, 0, 1000, 1000), true).with_dpi(144)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        o.handle(Input::Action(Action::Full));
        o.selection = Some(Rect::new(100, 100, 400, 400));
        o.state = State::Selected;
        assert!(matches!(o.zone((93, 93)), Zone::Edge(e) if e.left && e.top));
        assert_eq!(o.zone((92, 93)), Zone::Outside, "outside the 150 % zone");
        assert!(
            matches!(o.zone((105, 250)), Zone::Edge(e) if e.left),
            "band"
        );
        assert_eq!(o.zone((106, 250)), Zone::Interior);
        assert!(
            matches!(o.zone((106, 300)), Zone::Edge(e) if e.left),
            "handle zone"
        );
        mv(&mut o, 300, 300);
        assert_eq!(o.cursor(), Cursor::Move);
        mv(&mut o, 500, 500);
        assert_eq!(o.cursor(), Cursor::SizeNwse);
        mv(&mut o, 500, 100);
        assert_eq!(o.cursor(), Cursor::SizeNesw);
        mv(&mut o, 300, 100);
        assert_eq!(o.cursor(), Cursor::SizeNs);
        mv(&mut o, 900, 900);
        assert_eq!(o.cursor(), Cursor::Cross);
        // A 10×10 selection is interior everywhere; its handles sit outside it.
        o.selection = Some(Rect::new(200, 200, 10, 10));
        for p in [(200, 200), (209, 209), (204, 200)] {
            assert_eq!(o.zone(p), Zone::Interior, "{p:?}");
        }
        assert!(matches!(o.zone((192, 192)), Zone::Edge(e) if e.left && e.top));
        assert!(matches!(o.zone((205, 212)), Zone::Edge(e) if e.bottom));
    }

    #[test]
    fn keyboard_moves_grows_shrinks_and_pins() {
        let mut o = one(100, 100);
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Stay,
            "nothing to pin without a selection"
        );
        o.handle(Input::Action(Action::Move(Dir::Left)));
        assert_eq!(o.selection(), None, "keys need a selection");
        drag(&mut o, (10, 10), (19, 19)); // 10×10 at (10,10)
        let key = |o: &mut Overlay, a| {
            o.handle(Input::Action(a));
            o.selection().unwrap()
        };
        assert_eq!(
            key(&mut o, Action::Move(Dir::Right)),
            Rect::new(11, 10, 10, 10)
        );
        assert_eq!(key(&mut o, Action::Move(Dir::Up)), Rect::new(11, 9, 10, 10));
        assert_eq!(key(&mut o, Action::Grow(Dir::Up)), Rect::new(11, 8, 10, 11));
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Right)),
            Rect::new(11, 8, 11, 11)
        );
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Left)),
            Rect::new(10, 8, 12, 11)
        );
        assert_eq!(
            key(&mut o, Action::Grow(Dir::Down)),
            Rect::new(10, 8, 12, 12)
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Up)),
            Rect::new(10, 8, 12, 11),
            "Shift+Up shrinks from the bottom edge"
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Down)),
            Rect::new(10, 9, 12, 10),
            "Shift+Down shrinks from the top edge"
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Left)),
            Rect::new(10, 9, 11, 10)
        );
        assert_eq!(
            key(&mut o, Action::Shrink(Dir::Right)),
            Rect::new(11, 9, 10, 10)
        );
        for _ in 0..50 {
            key(&mut o, Action::Shrink(Dir::Right));
        }
        assert_eq!(o.selection().unwrap().width, 1, "never below one pixel");
        for _ in 0..500 {
            key(&mut o, Action::Move(Dir::Left));
            key(&mut o, Action::Grow(Dir::Left));
        }
        assert_eq!(o.selection().unwrap().x, 0, "kept inside the picture");
        let s = o.selection().unwrap();
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        assert_eq!(
            o.phase(),
            Phase::Selected,
            "the owner decides when it closes"
        );
        // Keys wait while a button is held (a press outside the selection, the bar and the chip).
        assert_eq!(o.zone((95, 1)), Zone::Outside);
        press(&mut o, Button::Left, 95, 1);
        mv(&mut o, 98, 5);
        assert_eq!(o.handle(Input::Action(Action::Pin)), Outcome::Stay);
    }

    #[test]
    fn ctrl_a_selects_the_monitor_then_all_and_r_restores_the_last() {
        // Two monitors: 100 % on the left, 150 % on the right, the right one higher.
        let mons = vec![
            Monitor::new(Rect::new(0, 100, 1920, 1080), true),
            Monitor::new(Rect::new(1920, 0, 2560, 1440), false).with_dpi(144),
        ];
        let pic = Rect::new(0, 0, 4480, 1440);
        let mut o = Overlay::new(
            pic,
            mons,
            SystemValues::default(),
            (2000, 50),
            Some(Rect::new(10, 150, 20, 30)),
            None,
        );
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(1920, 0, 2560, 1440)));
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(pic), "again: every monitor");
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(1920, 0, 2560, 1440)));
        o.handle(Input::Action(Action::Last));
        assert_eq!(o.selection(), Some(Rect::new(10, 150, 20, 30)));
        // A pointer in the gap above the left monitor belongs to the nearest monitor.
        mv(&mut o, 500, 20);
        o.handle(Input::Action(Action::Full));
        assert_eq!(o.selection(), Some(Rect::new(0, 100, 1920, 1080)));
    }

    #[test]
    fn action_ids_round_trip() {
        for id in ACTION_IDS {
            assert!(Action::from_id(id).is_some(), "{id}");
        }
        assert_eq!(
            Action::from_id("selection.grow-left"),
            Some(Action::Grow(Dir::Left))
        );
        assert_eq!(Action::from_id("selection.mark"), Some(Action::Mark));
        assert_eq!(
            Action::from_id("pointer.move-left"),
            Some(Action::Pointer(Dir::Left))
        );
        assert_eq!(Action::from_id("pointer.move-sideways"), None);
        assert_eq!(Action::from_id("color.copy"), None, "not in this build");
        assert_eq!(Action::from_id("selection.move-sideways"), None);
    }

    #[test]
    fn the_scene_places_readout_help_and_handles_per_monitor_dpi() {
        let mons = vec![
            Monitor::new(Rect::new(0, 0, 1000, 800), true),
            Monitor::new(Rect::new(1000, 0, 1500, 1200), false).with_dpi(144),
        ];
        let help = vec!["Drag to select".to_string(), "Esc cancels".to_string()];
        let mut o = Overlay::new(
            Rect::new(0, 0, 2500, 1200),
            mons,
            SystemValues::default(),
            (1500, 600),
            None,
            Some(help.clone()),
        );
        let s = o.scene(&Approx);
        let chip = s.chip.clone().unwrap();
        assert_eq!(chip.lines, ["1500, 600"]);
        assert_eq!(chip.dpi, 144, "drawn at the monitor's scale");
        assert_eq!(
            (chip.rect.x, chip.rect.y),
            (1524, 624),
            "16 logical px from the pointer"
        );
        let h = s.help.unwrap();
        assert_eq!(h.lines, help);
        assert_eq!(h.rect.y, 36, "24 logical px from the top at 150 %");
        assert!(Rect::new(1000, 0, 1500, 1200).contains(&h.rect));
        // A selection under the help pushes it to the bottom; dragging hides it.
        drag(&mut o, (1100, 10), (2400, 300));
        let s = o.scene(&Approx);
        let h = s.help.unwrap();
        assert!(
            h.rect.bottom() <= 1200 - 36 && h.rect.y > 300,
            "{:?}",
            h.rect
        );
        assert_eq!(s.chip.unwrap().lines, ["1301 \u{d7} 291  at 1100, 10"]);
        assert_eq!(s.handles.len(), 8);
        assert!(
            s.handles.iter().all(|r| r.width == 11),
            "7 logical px at 150 %"
        );
        press(&mut o, Button::Left, 1500, 600);
        mv(&mut o, 1600, 700);
        let s = o.scene(&Approx);
        assert!(
            s.help.is_none() && s.handles.is_empty(),
            "nothing extra while dragging"
        );
        // A banner replaces the help.
        release(&mut o, 1600, 700);
        o.set_banner(Some("The pin could not be created.".into()));
        assert_eq!(
            o.scene(&Approx).help.unwrap().lines,
            ["The pin could not be created."]
        );
        o.set_banner(None);
        o.hide_help();
        assert!(o.scene(&Approx).help.is_none());
    }

    #[test]
    fn the_border_stays_visible_on_the_picture_edge() {
        let mut o = one(200, 100);
        o.handle(Input::Action(Action::Full));
        let s = o.scene(&Approx);
        assert_eq!(s.selection, Some(Rect::new(0, 0, 200, 100)));
        assert_eq!(s.accent, Some(Rect::new(0, 0, 200, 100)), "drawn inside");
        assert!(
            s.handles
                .iter()
                .all(|h| Rect::new(0, 0, 200, 100).contains(h))
        );
        // The chip moves inside when there is no room above.
        let c = s.chip.unwrap();
        assert!(c.rect.y > 0 && c.rect.x > 0);
    }

    #[test]
    fn dirty_covers_exactly_what_changed() {
        let mut o = one(400, 300);
        drag(&mut o, (100, 100), (199, 199));
        let a = o.scene(&Approx);
        assert!(dirty(&a, &a).is_empty(), "no change, no work");
        o.handle(Input::Action(Action::Move(Dir::Right)));
        let b = o.scene(&Approx);
        let d = dirty(&a, &b);
        // The column that became dimmed and the one that became plain are both covered.
        let covered = |x: i32, y: i32| d.iter().any(|r| r.contains(&Rect::new(x, y, 1, 1)));
        assert!(covered(100, 150) && covered(200, 150));
        assert!(!covered(150, 150), "the unchanged middle is not redrawn");
        assert!(covered(97, 97), "old halo");
        // A full repaint area: every pixel of the new scene composed into a buffer equals
        // composing only the dirty parts over the old picture.
        let ledger = Ledger::new();
        let syn = Synthetic::new(vec![Monitor::new(Rect::new(0, 0, 400, 300), true)]);
        let frozen =
            crate::capture::CaptureBackend::capture(&syn, &ledger, Rect::new(0, 0, 400, 300))
                .unwrap();
        let mut full = vec![0u8; 400 * 300 * 4];
        compose(&frozen, None, &b, b.picture, &mut full);
        let mut inc = vec![0u8; 400 * 300 * 4];
        compose(&frozen, None, &a, a.picture, &mut inc);
        for r in &d {
            compose(&frozen, None, &b, *r, &mut inc);
        }
        assert!(full == inc, "incremental composition equals a full one");
    }

    #[test]
    fn compose_dims_outside_keeps_inside_and_draws_the_border() {
        let ledger = Ledger::new();
        let pic = Rect::new(-50, -20, 300, 200);
        let syn = Synthetic::new(vec![Monitor::new(pic, true)]);
        let frozen = crate::capture::CaptureBackend::capture(&syn, &ledger, pic).unwrap();
        let mut o = Overlay::new(
            pic,
            vec![Monitor::new(pic, true)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        )
        .with_magnifier(false);
        drag(&mut o, (-40, -10), (59, 59)); // 100×70 at (-40,-10)
        let scene = o.scene(&Approx);
        let mut out = vec![0u8; 300 * 200 * 4];
        compose(&frozen, None, &scene, pic, &mut out);
        let at = |x: i32, y: i32| {
            let i = (((y - pic.y) * 300 + (x - pic.x)) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        assert_eq!(
            at(0, 40),
            Synthetic::pixel(0, 40),
            "inside: the frozen pixel"
        );
        assert_eq!(
            at(240, 30),
            dim_pixel(Synthetic::pixel(240, 30)),
            "outside: dimmed"
        );
        assert_eq!(at(0, -11), color::ACCENT, "border one pixel outside");
        assert_eq!(at(0, -12), color::HALO, "halo beyond it");
        assert_eq!(at(0, 60), color::ACCENT);
        assert_eq!(at(-40, 25), color::ACCENT, "inside a handle");
        let chip = scene.chip.as_ref().unwrap().rect;
        assert_eq!(at(chip.x, chip.y), color::BORDER_SUBTLE);
        assert_eq!(at(chip.x + 2, chip.y + 2), color::BG_OVERLAY);
        let d = dim_pixel([200, 100, 0, 255]);
        assert_eq!(d, [110, 55, 0, 255], "about 45 % darker");
        // The frozen picture itself is never changed.
        assert_eq!(&frozen.pixels()[..4], &Synthetic::pixel(-50, -20));
    }

    #[test]
    fn rect_helpers() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            intersection(a, Rect::new(5, 5, 10, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(intersection(a, Rect::new(10, 0, 5, 5)), None);
        let parts = subtract(a, Rect::new(2, 2, 3, 3));
        let area: u64 = parts
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum();
        assert_eq!(area, 100 - 9);
        assert_eq!(subtract(a, a), Vec::<Rect>::new());
        assert_eq!(subtract(a, Rect::new(50, 50, 1, 1)), vec![a]);
        assert_eq!(grow(a, 2), Rect::new(-2, -2, 14, 14));
        assert_eq!(grow(a, -1), Rect::new(1, 1, 8, 8));
    }

    // ---- Step 51: the double-click gesture, the toolbar and the outputs (UX spec §4.3) ----

    /// A press with modifiers at a message time.
    fn press_at(o: &mut Overlay, b: Button, x: i32, y: i32, mods: Mods, t: u32) -> Outcome {
        o.handle(Input::Press {
            button: b,
            x,
            y,
            mods,
            time_ms: t,
        })
    }

    /// A plain left click (press and release in place) at time `t`.
    fn click(o: &mut Overlay, x: i32, y: i32, t: u32) -> (Outcome, Outcome) {
        let p = press_at(o, Button::Left, x, y, Mods::NONE, t);
        let r = release(o, x, y);
        (p, r)
    }

    /// Two plain clicks `dt` ms apart, the second at `(x2, y2)`; the second press's outcome.
    fn double(o: &mut Overlay, at: (i32, i32), second: (i32, i32), t: u32, dt: u32) -> Outcome {
        click(o, at.0, at.1, t);
        let out = press_at(
            o,
            Button::Left,
            second.0,
            second.1,
            Mods::NONE,
            t.wrapping_add(dt),
        );
        release(o, second.0, second.1);
        out
    }

    /// FLOW-01's overlay: one 800×600 monitor at 100 %, selection (100,100)–(500,400) confirmed,
    /// the scene drawn once (hit-testing follows what is drawn).
    fn flow() -> (Overlay, Rect) {
        let mut o = one(800, 600);
        drag(&mut o, (100, 100), (499, 399));
        o.scene(&Approx);
        let s = o.selection().unwrap();
        assert_eq!(s, Rect::new(100, 100, 400, 300));
        (o, s)
    }

    fn pinned(s: Rect) -> Outcome {
        Outcome::Output(Output::Pin, s, Via::DoubleClick)
    }

    #[test]
    fn a_double_click_inside_fires_on_the_second_press_with_the_users_values() {
        // FLOW-01: two presses 120 ms apart, no movement: the pin fires on the second press.
        let (mut o, s) = flow();
        click(&mut o, 300, 250, 1_000);
        assert_eq!(o.phase(), Phase::Selected, "a single click does nothing");
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 1_120),
            pinned(s)
        );
        assert_eq!(o.selection(), Some(s), "the owner closes the overlay");
        // FLOW-02 / FLOW-03: T − 1 pins, T + 1 does not (T = 500 ms, Windows' default).
        let (mut o, s) = flow();
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 5_000, 499),
            pinned(s)
        );
        let (mut o, s) = flow();
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 5_000, 501),
            Outcome::Stay
        );
        assert_eq!(
            (o.phase(), o.selection()),
            (Phase::Selected, Some(s)),
            "slow: nothing changed"
        );
        // …but that slow second click may be the first of a new double-click.
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 5_501 + 200),
            pinned(s)
        );
        // FLOW-04: inside the 4×4 double-click rectangle (±2) pins, outside (3) does not.
        let (mut o, s) = flow();
        assert_eq!(double(&mut o, (300, 250), (302, 248), 0, 100), pinned(s));
        let (mut o, _) = flow();
        assert_eq!(
            double(&mut o, (300, 250), (303, 250), 0, 100),
            Outcome::Stay
        );
        // The message clock wraps around.
        let (mut o, s) = flow();
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), u32::MAX - 100, 201),
            pinned(s)
        );
        // FLOW-36: the user slows the double-click speed to 900 ms while the overlay is open;
        // the owner refreshes the values before each press.
        let (mut o, s) = flow();
        click(&mut o, 300, 250, 10_000);
        o.set_system_values(SystemValues {
            double_click_ms: 900,
            double_click: (8, 8),
            ..SystemValues::default()
        });
        assert_eq!(
            press_at(&mut o, Button::Left, 304, 254, Mods::NONE, 10_800),
            pinned(s),
            "800 ms apart and 4 px away pin with the user's new values"
        );
    }

    #[test]
    fn presses_that_do_something_else_never_count() {
        // FLOW-05: both outside; FLOW-06: one inside, one outside (both orders).
        for (a, b) in [
            ((600, 500), (600, 500)),
            ((300, 250), (600, 500)),
            ((600, 500), (300, 250)),
        ] {
            let (mut o, s) = flow();
            assert_eq!(double(&mut o, a, b, 0, 100), Outcome::Stay, "{a:?} {b:?}");
            assert_eq!(o.selection(), Some(s), "the selection stays");
        }
        // FLOW-07: on a corner handle, on the border band of the bottom edge.
        for p in [(100, 100), (500, 400), (300, 402), (300, 398)] {
            let (mut o, _) = flow();
            assert!(matches!(o.zone(p), Zone::Edge(_)), "{p:?}");
            assert_eq!(double(&mut o, p, p, 0, 100), Outcome::Stay, "{p:?}");
        }
        // FLOW-09: a move (press, 20 px, release) is not a click; the next two presses pin the
        // moved rectangle.
        let (mut o, _) = flow();
        press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 0);
        mv(&mut o, 320, 260);
        assert_eq!(o.phase(), Phase::Moving);
        release(&mut o, 320, 260);
        let moved = Rect::new(120, 110, 400, 300);
        assert_eq!(o.selection(), Some(moved));
        assert_eq!(
            press_at(&mut o, Button::Left, 320, 260, Mods::NONE, 100),
            Outcome::Stay,
            "the release of the move is not a first click"
        );
        release(&mut o, 320, 260);
        assert_eq!(
            press_at(&mut o, Button::Left, 320, 260, Mods::NONE, 200),
            pinned(moved)
        );
        // FLOW-10: a jitter within the drag threshold keeps the click and the selection.
        let (mut o, s) = flow();
        press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 0);
        mv(&mut o, 301, 251);
        release(&mut o, 301, 251);
        assert_eq!(o.selection(), Some(s));
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 150),
            pinned(s)
        );
        // FLOW-11: resize by the right handle, then a double-click pins the resized rectangle.
        let (mut o, _) = flow();
        drag(&mut o, (500, 250), (560, 250));
        let resized = Rect::new(100, 100, 460, 300);
        assert_eq!(o.selection(), Some(resized));
        o.scene(&Approx);
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 0, 100),
            pinned(resized)
        );
        // FLOW-12: the press that started a drag is not a first click.
        let mut o = one(800, 600);
        press_at(&mut o, Button::Left, 100, 100, Mods::NONE, 0);
        mv(&mut o, 300, 250);
        release(&mut o, 300, 250);
        o.scene(&Approx);
        assert_eq!(
            press_at(&mut o, Button::Left, 250, 200, Mods::NONE, 50),
            Outcome::Stay
        );
        // FLOW-22: a modifier at either press.
        for mods in [Mods::CTRL, Mods::SHIFT, Mods::ALT, Mods::WIN] {
            let (mut o, _) = flow();
            press_at(&mut o, Button::Left, 300, 250, mods, 0);
            release(&mut o, 300, 250);
            assert_eq!(
                press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 100),
                Outcome::Stay,
                "{mods:?} at P1"
            );
            let (mut o, _) = flow();
            click(&mut o, 300, 250, 0);
            assert_eq!(
                press_at(&mut o, Button::Left, 300, 250, mods, 100),
                Outcome::Stay,
                "{mods:?} at P2"
            );
        }
        // FLOW-23: a right-button double-click inside opens the menu and never pins.
        let (mut o, _) = flow();
        for t in [0, 100] {
            assert_eq!(
                press_at(&mut o, Button::Right, 300, 250, Mods::NONE, t),
                Outcome::Menu(Some((300, 250)))
            );
        }
        // A key between the clicks ends the wait for the second one.
        let (mut o, s) = flow();
        click(&mut o, 300, 250, 0);
        o.handle(Input::Action(Action::Move(Dir::Right)));
        o.handle(Input::Action(Action::Move(Dir::Left)));
        assert_eq!(o.selection(), Some(s));
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 100),
            Outcome::Stay
        );
        // FLOW-37: a 10×10 selection is interior everywhere.
        let mut o = one(800, 600);
        drag(&mut o, (200, 200), (209, 209));
        o.scene(&Approx);
        let s = o.selection().unwrap();
        assert_eq!(double(&mut o, (209, 200), (209, 200), 0, 100), pinned(s));
        // Rule 1: in Hover nothing pins.
        let mut o = one(800, 600);
        assert_eq!(double(&mut o, (50, 50), (50, 50), 0, 100), Outcome::Stay);
        assert_eq!(o.selection(), None);
    }

    #[test]
    fn the_gesture_settings_change_the_double_click_and_the_middle_click_only() {
        let with = |double_click, middle_click| {
            let (o, s) = flow();
            (
                o.with_gestures(Gestures {
                    double_click,
                    middle_click,
                }),
                s,
            )
        };
        // FLOW-21: a middle-click inside pins, outside does nothing.
        let (mut o, s) = flow();
        assert_eq!(
            press_at(&mut o, Button::Middle, 300, 250, Mods::NONE, 0),
            Outcome::Output(Output::Pin, s, Via::MiddleClick)
        );
        assert_eq!(
            press_at(&mut o, Button::Middle, 700, 500, Mods::NONE, 0),
            Outcome::Stay
        );
        // FLOW-24: `copy` copies on the double-click; Enter still pins.
        let (mut o, s) = with(GestureAction::Copy, GestureAction::Pin);
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 0, 100),
            Outcome::Output(Output::Copy, s, Via::DoubleClick)
        );
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        // `save` asks for the save dialog.
        let (mut o, s) = with(GestureAction::Save, GestureAction::Pin);
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 0, 100),
            Outcome::Output(Output::Save, s, Via::DoubleClick)
        );
        // FLOW-25: `none`: the second press is an ordinary press (it may start a move).
        let (mut o, s) = with(GestureAction::None, GestureAction::None);
        click(&mut o, 300, 250, 0);
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 100),
            Outcome::Stay
        );
        assert_eq!(o.phase(), Phase::Pressed);
        release(&mut o, 300, 250);
        assert_eq!(
            press_at(&mut o, Button::Middle, 300, 250, Mods::NONE, 0),
            Outcome::Stay
        );
        assert_eq!(o.selection(), Some(s));
        // The Snipaste-style preset: double-click copies, middle-click pins.
        let (mut o, s) = with(GestureAction::Copy, GestureAction::Pin);
        assert_eq!(
            press_at(&mut o, Button::Middle, 300, 250, Mods::NONE, 0),
            Outcome::Output(Output::Pin, s, Via::MiddleClick)
        );
        let (mut o, s) = with(GestureAction::Pin, GestureAction::Copy);
        assert_eq!(
            press_at(&mut o, Button::Middle, 300, 250, Mods::NONE, 0),
            Outcome::Output(Output::Copy, s, Via::MiddleClick)
        );
        for id in ["pin", "copy", "save", "none"] {
            assert_eq!(GestureAction::parse(id).unwrap().as_str(), id);
        }
        assert_eq!(GestureAction::parse("close"), None);
    }

    #[test]
    fn the_keys_copy_save_and_quick_save() {
        let (mut o, s) = flow();
        for (a, out) in [
            (Action::Copy, Output::Copy),
            (Action::Save, Output::Save),
            (Action::QuickSave, Output::QuickSave),
            (Action::Pin, Output::Pin),
        ] {
            assert_eq!(
                o.handle(Input::Action(a)),
                Outcome::Output(out, s, Via::Key)
            );
            assert_eq!(Action::from_id(out.action_id()), Some(a));
        }
        let mut empty = one(100, 100);
        assert_eq!(empty.handle(Input::Action(Action::Copy)), Outcome::Stay);
        assert_eq!(
            empty.handle(Input::MenuKey),
            Outcome::Stay,
            "no selection, no menu"
        );
        assert_eq!(o.handle(Input::MenuKey), Outcome::Menu(None));
    }

    #[test]
    fn the_toolbar_sits_next_to_the_selection_and_never_over_the_chip() {
        // Below the selection, 6 + 2 px away, right-aligned unless the monitor's left edge stops it
        // (616 logical pixels since Step 56, 744 with Step 57's privacy tools); 19 buttons in tab
        // order.
        let (o, s) = flow();
        let scene = o.scene(&Approx);
        let bar = scene.toolbar.clone().unwrap();
        assert_eq!(bar.rect, Rect::new(0, 408, 744, 32));
        assert_eq!(bar.rect.y, i32::try_from(s.bottom()).unwrap() + 8);
        let items: Vec<&str> = bar.buttons.iter().map(|b| b.item.as_str()).collect();
        assert_eq!(
            items,
            [
                "pin",
                "copy",
                "save",
                "rectangle",
                "ellipse",
                "line",
                "arrow",
                "pen",
                "marker",
                "text",
                "mosaic",
                "blur",
                "eraser",
                "redact",
                "color",
                "width",
                "undo",
                "redo",
                "cancel"
            ]
        );
        let labels: Vec<&str> = bar.buttons.iter().map(|b| b.label).collect();
        assert_eq!(&labels[..3], ["Pin", "Copy", "Save"]);
        assert_eq!(labels[18], "Cancel");
        assert!(labels[3..18].iter().all(|l| l.is_empty()), "icon buttons");
        assert!(
            bar.buttons[0].primary && !bar.buttons[1].primary,
            "Pin is primary"
        );
        assert_eq!(bar.buttons[1].rect, Rect::new(60, 412, 52, 24));
        assert_eq!(
            bar.buttons[3].rect,
            Rect::new(178, 412, 28, 24),
            "the tools"
        );
        assert_eq!(
            bar.buttons[10].rect,
            Rect::new(402, 412, 28, 24),
            "the mosaic tool after the text tool"
        );
        assert_eq!(bar.buttons[18].rect, Rect::new(676, 412, 64, 24));
        assert!(
            bar.buttons[16].disabled && bar.buttons[17].disabled,
            "nothing to undo or redo yet"
        );
        assert!(!bar.rect.intersects(&scene.chip.unwrap().rect));
        // No room below: above the selection, clear of the chip.
        let mut o = one(800, 600);
        drag(&mut o, (100, 300), (499, 589));
        let scene = o.scene(&Approx);
        let bar = scene.toolbar.unwrap();
        let chip = scene.chip.unwrap().rect;
        assert!(bar.rect.bottom() <= 300, "above: {:?}", bar.rect);
        assert!(!bar.rect.intersects(&chip), "{:?} {:?}", bar.rect, chip);
        // Neither (FLOW-38): inside the bottom edge of a selection filling the monitor.
        let mut o = one(800, 600);
        o.handle(Input::Action(Action::Full));
        let scene = o.scene(&Approx);
        let bar = scene.toolbar.clone().unwrap();
        assert_eq!(bar.rect, Rect::new(50, 562, 744, 32));
        assert!(Rect::new(0, 0, 800, 600).contains(&bar.rect));
        assert!(!bar.rect.intersects(&scene.chip.unwrap().rect));
        // The bar covers part of the selection: that part is not interior. A double-click on
        // the bar between buttons pins nothing; elsewhere inside it pins.
        let gap = (bar.buttons[0].rect.right() as i32 + 1, bar.rect.y + 2);
        assert_eq!(o.zone(gap), Zone::Toolbar(None));
        assert_eq!(double(&mut o, gap, gap, 0, 100), Outcome::Stay);
        o.scene(&Approx);
        let full = Rect::new(0, 0, 800, 600);
        assert_eq!(double(&mut o, (400, 300), (400, 300), 0, 100), pinned(full));
        // The chip covering the selection (no room above) is not interior either.
        let chip = o.scene(&Approx).chip.unwrap().rect;
        assert!(full.contains(&chip));
        assert_eq!(o.zone((chip.x + 3, chip.y + 3)), Zone::Chip);
        let at = (chip.x + 3, chip.y + 3);
        assert_eq!(double(&mut o, at, at, 0, 100), Outcome::Stay);
        // Hidden while a drag, move or resize is in progress.
        let (mut o, _) = flow();
        press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 0);
        mv(&mut o, 340, 270);
        assert!(o.scene(&Approx).toolbar.is_none());
        // 150 %: every length scales, the selection stays in device pixels.
        let mut o = Overlay::new(
            Rect::new(0, 0, 1500, 1000),
            vec![Monitor::new(Rect::new(0, 0, 1500, 1000), true).with_dpi(144)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        drag(&mut o, (100, 100), (699, 549));
        let bar = o.scene(&Approx).toolbar.unwrap();
        assert_eq!((bar.rect.width, bar.rect.height), (1116, 48));
        assert_eq!(bar.rect.y, 550 + 9 + 2);
        assert_eq!(bar.buttons[3].rect.width, 42, "a tool");
        assert_eq!(bar.buttons[18].rect.width, 96, "Cancel");
        // A monitor narrower than the bar: the buttons wrap into rows inside it (Step 56).
        let mut o = one(300, 400);
        drag(&mut o, (20, 20), (119, 99));
        let bar = o.scene(&Approx).toolbar.unwrap();
        assert!(
            Rect::new(0, 0, 300, 400).contains(&bar.rect),
            "{:?}",
            bar.rect
        );
        assert!(bar.rect.height > 32 * 2, "rows: {:?}", bar.rect);
        assert!(
            bar.buttons.iter().all(|b| bar.rect.contains(&b.rect)),
            "every button inside the bar"
        );
        for (i, a) in bar.buttons.iter().enumerate() {
            for b in &bar.buttons[i + 1..] {
                assert!(!a.rect.intersects(&b.rect), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn toolbar_buttons_run_on_release_and_the_keyboard_follows_adr_0021() {
        let (mut o, s) = flow();
        let copy = (86, 424);
        let save = (142, 424);
        let cancel = (708, 424);
        assert_eq!(o.zone(copy), Zone::Toolbar(Some(1)));
        mv(&mut o, 600, 500);
        assert_eq!(o.cursor(), Cursor::Cross, "outside the selection");
        mv(&mut o, copy.0, copy.1);
        assert_eq!(o.cursor(), Cursor::Arrow);
        assert!(o.scene(&Approx).toolbar.unwrap().buttons[1].hover);
        // FLOW-08: a click on Copy copies (on release), never pins.
        assert_eq!(
            press_at(&mut o, Button::Left, copy.0, copy.1, Mods::NONE, 0),
            Outcome::Stay
        );
        assert!(o.scene(&Approx).toolbar.unwrap().buttons[1].pressed);
        assert_eq!(
            release(&mut o, copy.0, copy.1),
            Outcome::Output(Output::Copy, s, Via::Toolbar)
        );
        // Released elsewhere: nothing.
        press_at(&mut o, Button::Left, copy.0, copy.1, Mods::NONE, 0);
        assert_eq!(release(&mut o, 600, 500), Outcome::Stay);
        assert_eq!(
            o.selection(),
            Some(s),
            "a press on the bar never moves or replaces"
        );
        // Save, Shift+Save, Cancel.
        assert_eq!(
            click(&mut o, save.0, save.1, 0).1,
            Outcome::Output(Output::Save, s, Via::Toolbar)
        );
        press_at(&mut o, Button::Left, save.0, save.1, Mods::SHIFT, 0);
        assert_eq!(
            release(&mut o, save.0, save.1),
            Outcome::Output(Output::QuickSave, s, Via::Toolbar)
        );
        assert_eq!(click(&mut o, cancel.0, cancel.1, 0).1, Outcome::Cancel);
        // A click on the bar is not a first click either.
        click(&mut o, 262, 410, 0);
        assert_eq!(
            press_at(&mut o, Button::Left, 300, 250, Mods::NONE, 50),
            Outcome::Stay
        );
        // FLOW-17: Tab moves to Pin, then Copy; Enter on Copy copies.
        let (mut o, s) = flow();
        assert_eq!(o.toolbar_focus(), None, "the canvas has the focus");
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Pin));
        assert!(o.scene(&Approx).toolbar.unwrap().buttons[0].focus);
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Copy));
        assert_eq!(
            o.handle(Input::Nav(Nav::Activate)),
            Outcome::Output(Output::Copy, s, Via::Key)
        );
        // Shift+Tab from the canvas reaches the last button; arrows wrap; Tab past the last
        // returns to the canvas; Escape on a button cancels.
        let (mut o, s) = flow();
        o.handle(Input::Nav(Nav::Prev));
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Cancel));
        o.handle(Input::Nav(Nav::Right));
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Pin));
        o.handle(Input::Nav(Nav::Left));
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Cancel));
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(o.toolbar_focus(), None);
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(
            o.handle(Input::Nav(Nav::Activate)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        assert_eq!(o.handle(Input::Nav(Nav::Escape)), Outcome::Cancel);
        // A press in the canvas gives the focus back to it.
        click(&mut o, 300, 250, 0);
        assert_eq!(o.toolbar_focus(), None);
        // Without a selection there is no toolbar.
        let mut o = one(100, 100);
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(o.toolbar_focus(), None);
        assert!(o.scene(&Approx).toolbar.is_none());
        // Right-click on the bar does nothing; outside clears the selection and the focus.
        let (mut o, _) = flow();
        o.handle(Input::Nav(Nav::Next));
        assert_eq!(press(&mut o, Button::Right, copy.0, copy.1), Outcome::Stay);
        press(&mut o, Button::Right, 700, 550);
        assert_eq!((o.selection(), o.toolbar_focus()), (None, None));
    }

    #[test]
    fn the_toolbar_is_composed_with_the_primary_pin_and_the_focus_ring() {
        let ledger = Ledger::new();
        let syn = Synthetic::new(vec![Monitor::new(Rect::new(0, 0, 800, 600), true)]);
        let frozen =
            crate::capture::CaptureBackend::capture(&syn, &ledger, Rect::new(0, 0, 800, 600))
                .unwrap();
        let (mut o, _) = flow();
        o.handle(Input::Nav(Nav::Next));
        o.handle(Input::Nav(Nav::Next)); // Copy has the focus
        mv(&mut o, 142, 424); // the pointer over Save
        let scene = o.scene(&Approx);
        let mut out = vec![0u8; 800 * 600 * 4];
        compose(&frozen, None, &scene, scene.picture, &mut out);
        let at = |x: i32, y: i32| {
            let i = ((y * 800 + x) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        assert_eq!(at(30, 424), color::ACCENT, "Pin: accent fill");
        assert_eq!(at(86, 424), color::BG_OVERLAY, "Copy: flat");
        assert_eq!(at(142, 424), color::BG_HOVER, "Save: under the pointer");
        assert_eq!(
            at(57, 410),
            color::FOCUS_RING,
            "Copy's ring, 1 px off its corner"
        );
        assert_eq!(at(0, 408), color::BORDER_SUBTLE, "the bar's outline");
        assert_eq!(at(1, 409), color::BG_OVERLAY);
        // The tools' icons are drawn (original vector artwork): the rectangle tool's outline.
        let rect_tool = Rect::new(178, 412, 28, 24);
        let ink = (rect_tool.y..rect_tool.y + 24)
            .flat_map(|y| (rect_tool.x..rect_tool.x + 28).map(move |x| (x, y)))
            .filter(|(x, y)| at(*x, *y) != color::BG_OVERLAY)
            .count();
        assert!(ink > 30, "the icon's pixels: {ink}");
        // The toolbar is part of what the dirty strips cover.
        let before = o.scene(&Approx);
        o.handle(Input::Nav(Nav::Next));
        let after = o.scene(&Approx);
        let d = dirty(&before, &after);
        assert!(
            d.iter().any(|r| r.contains(&Rect::new(114, 410, 1, 1))),
            "{d:?}"
        );
    }

    #[test]
    fn high_dpi_and_multi_monitor_double_clicks_pin_exact_device_pixels() {
        // FLOW-31: two monitors side by side at the same scale, a selection across the seam.
        let mons = vec![
            Monitor::new(Rect::new(0, 0, 640, 480), true),
            Monitor::new(Rect::new(640, 0, 640, 480), false),
        ];
        let mut o = Overlay::new(
            Rect::new(0, 0, 1280, 480),
            mons,
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        drag(&mut o, (500, 100), (799, 299));
        o.scene(&Approx);
        let s = Rect::new(500, 100, 300, 200);
        assert_eq!(double(&mut o, (640, 200), (640, 200), 0, 100), pinned(s));
        // FLOW-32: one monitor at 150 %: a 600×450 device-pixel selection pins 600×450; the
        // handle zones and the band are 14 and 5 device pixels.
        let mut o = Overlay::new(
            Rect::new(-1000, 0, 1500, 1000),
            vec![Monitor::new(Rect::new(-1000, 0, 1500, 1000), true).with_dpi(144)],
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        drag(&mut o, (-900, 100), (-301, 549));
        let scene = o.scene(&Approx);
        assert_eq!(scene.chip.unwrap().lines, ["600 \u{d7} 450  at -900, 100"]);
        let s = Rect::new(-900, 100, 600, 450);
        assert!(
            matches!(o.zone((-895, 300)), Zone::Edge(_)),
            "inside the 150 % band"
        );
        assert_eq!(
            double(&mut o, (-895, 300), (-895, 300), 0, 100),
            Outcome::Stay
        );
        assert_eq!(o.zone((-894, 200)), Zone::Interior);
        assert_eq!(double(&mut o, (-894, 200), (-894, 200), 0, 100), pinned(s));
    }

    // ---- Step 52: smart selection and pixel precision (UX spec §4.8, ADR 0035) ----

    use crate::detect::{Affinity, WindowFacts};

    /// One 800×600 monitor with detection over these windows (topmost first).
    fn detecting(facts: Vec<WindowFacts>) -> Overlay {
        let pic = Rect::new(0, 0, 800, 600);
        one(800, 600).with_detection(DetectMode::Window, Some(Snapshot::from_facts(facts, pic)))
    }

    /// A small window above a large one.
    fn windows() -> Vec<WindowFacts> {
        vec![
            WindowFacts::plain(1, Rect::new(100, 100, 300, 200)),
            WindowFacts::plain(2, Rect::new(0, 0, 800, 560)),
        ]
    }

    fn act(o: &mut Overlay, a: Action) -> Outcome {
        o.handle(Input::Action(a))
    }

    #[test]
    fn detection_highlights_the_window_under_the_pointer_and_a_click_selects_it() {
        // J2: hover the window (it is highlighted), click, double-click inside: a pin of it.
        let mut o = detecting(windows());
        mv(&mut o, 150, 150);
        let w = Rect::new(100, 100, 300, 200);
        assert_eq!(
            o.detected(),
            Some(Detected {
                rect: w,
                kind: DetectKind::Window,
                protected: false
            })
        );
        let s = o.scene(&Approx);
        assert_eq!(
            s.selection,
            Some(w),
            "shown undimmed, as a selection would be"
        );
        assert_eq!(s.accent, Some(grow(w, 1)));
        assert!(
            s.handles.is_empty() && s.toolbar.is_none(),
            "not confirmed yet"
        );
        assert!(s.chip.unwrap().lines[0].starts_with("300 \u{d7} 200"));
        assert_eq!(o.selection(), None);
        // Elsewhere the window below.
        mv(&mut o, 600, 500);
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(0, 0, 800, 560))
        );
        mv(&mut o, 600, 590);
        assert_eq!(o.detected(), None, "no window there");
        // A click (no drag) selects the highlighted window; that press does not start a
        // double-click.
        mv(&mut o, 150, 150);
        click(&mut o, 150, 150, 0);
        assert_eq!((o.phase(), o.selection()), (Phase::Selected, Some(w)));
        assert_eq!(o.detected(), None, "no detection next to a selection");
        assert_eq!(
            double(&mut o, (200, 200), (200, 200), 1_000, 100),
            pinned(w)
        );
        // A drag still makes a region, whatever lies below.
        let mut o = detecting(windows());
        drag(&mut o, (150, 150), (250, 220));
        assert_eq!(o.selection(), Some(Rect::new(150, 150, 101, 71)));
        // Right-click outside clears the selection: detection comes back.
        press(&mut o, Button::Right, 700, 500);
        assert_eq!(o.phase(), Phase::Hover);
        mv(&mut o, 120, 120);
        assert_eq!(o.detected().map(|d| d.rect), Some(w));
        // A pointer that never moved still detects at its opening position.
        let mut o = detecting(windows());
        o.pointer = (150, 150);
        assert_eq!(o.detected().map(|d| d.rect), Some(w));
    }

    #[test]
    fn protected_absent_and_undetectable_windows_fall_back_to_the_plain_region() {
        let mut protected = WindowFacts::plain(3, Rect::new(300, 300, 200, 100));
        protected.affinity = Affinity::Monitor;
        let mut absent = WindowFacts::plain(4, Rect::new(50, 400, 200, 100));
        absent.affinity = Affinity::Excluded;
        let mut through = WindowFacts::plain(5, Rect::new(600, 400, 100, 100));
        through.layered = true;
        through.transparent = true;
        let mut facts = vec![protected, absent, through];
        facts.extend(windows());
        let mut o = detecting(facts);
        // A protected window: never highlighted, a note says why, a click selects nothing.
        mv(&mut o, 350, 350);
        assert!(o.detected().is_some_and(|d| d.protected));
        let s = o.scene(&Approx);
        assert_eq!(s.selection, None, "nothing undimmed");
        let chip = s.chip.unwrap();
        assert_eq!(chip.lines[1], PROTECTED_NOTE);
        click(&mut o, 350, 350, 0);
        assert_eq!((o.phase(), o.selection()), (Phase::Hover, None));
        assert_eq!(act(&mut o, Action::Mark), Outcome::Stay);
        assert_eq!(o.phase(), Phase::Marking, "Space marks a corner instead");
        press(&mut o, Button::Right, 350, 350);
        assert_eq!(o.phase(), Phase::Hover);
        // The plain region still works over it (the picture holds black there).
        drag(&mut o, (310, 310), (400, 380));
        assert_eq!(o.selection(), Some(Rect::new(310, 310, 91, 71)));
        // Excluded from capture and click-through windows are not there: the one below is.
        let mut o = detecting({
            let mut f = vec![absent, through];
            f.extend(windows());
            f
        });
        mv(&mut o, 60, 410);
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(0, 0, 800, 560))
        );
        mv(&mut o, 650, 450);
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(0, 0, 800, 560))
        );
        // No window list (the platform could not read it), or detection turned off with E:
        // the plain region selection.
        let mut none =
            one(800, 600).with_detection(DetectMode::Window, Some(Snapshot::unavailable()));
        mv(&mut none, 150, 150);
        assert_eq!(none.detected(), None);
        assert_eq!(none.detect_windows(), None);
        click(&mut none, 150, 150, 0);
        assert_eq!(none.selection(), None);
        let mut o = detecting(windows());
        act(&mut o, Action::DetectCycle);
        assert_eq!(o.detect_mode(), DetectMode::Element);
        act(&mut o, Action::DetectCycle);
        assert_eq!(o.detect_mode(), DetectMode::Off);
        mv(&mut o, 150, 150);
        assert_eq!(o.detected(), None);
        click(&mut o, 150, 150, 0);
        assert_eq!(o.selection(), None, "off: a click selects nothing");
        act(&mut o, Action::DetectCycle);
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(100, 100, 300, 200))
        );
    }

    #[test]
    fn element_detection_asks_off_thread_and_falls_back_to_the_window() {
        let mut o = detecting(windows());
        act(&mut o, Action::DetectCycle);
        o.pointer = (5, 5);
        o.take_element_request();
        mv(&mut o, 150, 150);
        let req = o
            .take_element_request()
            .expect("a request for the window under the pointer");
        assert_eq!(
            (req.token, req.window, req.at),
            (1, Rect::new(100, 100, 300, 200), (150, 150))
        );
        mv(&mut o, 150, 150);
        assert_eq!(
            o.take_element_request(),
            None,
            "the same point is not asked again"
        );
        // Until the answer comes, the window rectangle.
        assert_eq!(o.detected().map(|d| d.kind), Some(DetectKind::Window));
        let button = Rect::new(120, 130, 80, 30);
        o.handle(Input::Element {
            request: req,
            answer: ElementAnswer::Found(button),
        });
        assert_eq!(
            o.detected(),
            Some(Detected {
                rect: button,
                kind: DetectKind::Element,
                protected: false
            })
        );
        click(&mut o, 150, 150, 0);
        assert_eq!(o.selection(), Some(button), "a click selects the element");
        // A newer request supersedes an old answer unless it still fits the pointer.
        let mut o = detecting(windows());
        act(&mut o, Action::DetectCycle);
        mv(&mut o, 150, 150);
        let first = o.take_element_request().unwrap();
        mv(&mut o, 300, 250);
        let second = o.take_element_request().unwrap();
        assert!(second.seq > first.seq);
        o.handle(Input::Element {
            request: first,
            answer: ElementAnswer::Found(button),
        });
        assert_eq!(
            o.detected().map(|d| d.kind),
            Some(DetectKind::Window),
            "the old element does not hold the pointer any more"
        );
        o.handle(Input::Element {
            request: second,
            answer: ElementAnswer::WindowOnly,
        });
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(100, 100, 300, 200))
        );
        assert_eq!(o.detect_fallback(), None);
        // An answer that does not fit the window: the window rectangle, reason reported.
        mv(&mut o, 310, 250);
        let third = o.take_element_request().unwrap();
        o.handle(Input::Element {
            request: third,
            answer: ElementAnswer::Found(Rect::new(700, 500, 10, 10)),
        });
        assert_eq!(o.detect_fallback(), Some(Fallback::InvalidRect));
        assert_eq!(o.detected().map(|d| d.kind), Some(DetectKind::Window));
        // An elevated or unresponsive window is not asked again during this capture.
        mv(&mut o, 320, 250);
        let fourth = o.take_element_request().unwrap();
        o.handle(Input::Element {
            request: fourth,
            answer: ElementAnswer::Failed(Fallback::Denied),
        });
        assert_eq!(o.detect_fallback(), Some(Fallback::Denied));
        mv(&mut o, 330, 260);
        assert_eq!(o.take_element_request(), None, "refused window");
        assert_eq!(
            o.detected().map(|d| d.rect),
            Some(Rect::new(100, 100, 300, 200))
        );
        mv(&mut o, 600, 500);
        assert!(
            o.take_element_request().is_some_and(|r| r.token == 2),
            "other windows are still asked"
        );
        // Protected windows are never asked.
        let mut p = WindowFacts::plain(9, Rect::new(0, 0, 50, 50));
        p.affinity = Affinity::Monitor;
        let mut o = detecting(vec![p]);
        act(&mut o, Action::DetectCycle);
        mv(&mut o, 10, 10);
        assert_eq!(o.take_element_request(), None);
    }

    #[test]
    fn pointer_keys_move_one_pixel_accelerate_and_pause_detection() {
        let mut o = detecting(windows());
        mv(&mut o, 150, 150);
        assert!(o.detected().is_some());
        act(&mut o, Action::Pointer(Dir::Right));
        assert_eq!(o.pointer(), (151, 150));
        assert_eq!(o.take_warp(), Some((151, 150)), "the real pointer follows");
        assert_eq!(o.take_warp(), None);
        assert!(o.detect_suspended());
        assert_eq!(o.detected(), None, "exact corners, not windows");
        // The warped cursor reports the same position: still paused. A real move resumes.
        mv(&mut o, 151, 150);
        assert!(o.detect_suspended());
        mv(&mut o, 152, 150);
        assert!(!o.detect_suspended());
        assert!(o.detected().is_some());
        // Held: 1 for the press and nine repeats, then 2, 5 and 10 per repeat.
        let mut o = one(800, 600);
        mv(&mut o, 0, 300);
        act(&mut o, Action::Pointer(Dir::Right));
        for _ in 0..30 {
            o.handle(Input::Repeat(Action::Pointer(Dir::Right)));
        }
        let want = 1 + 9 + 10 * 2 + 10 * 5 + 10;
        assert_eq!(o.pointer(), (want, 300));
        assert_eq!(
            [accel(0), accel(9), accel(10), accel(20), accel(99)],
            [1, 1, 2, 5, 10]
        );
        // Another key starts over at one pixel; the picture's edge stops the pointer.
        act(&mut o, Action::Pointer(Dir::Up));
        assert_eq!(o.pointer(), (want, 299));
        for _ in 0..100 {
            o.handle(Input::Repeat(Action::Pointer(Dir::Up)));
        }
        assert_eq!(o.pointer(), (want, 0), "clamped to the picture");
        // During a drag the pointer keys move its end, pixel by pixel.
        let mut o = one(800, 600);
        press(&mut o, Button::Left, 100, 100);
        mv(&mut o, 200, 150);
        act(&mut o, Action::Pointer(Dir::Right));
        act(&mut o, Action::Pointer(Dir::Down));
        assert_eq!(o.live(), Some(Rect::new(100, 100, 102, 52)));
        release(&mut o, 201, 151);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 102, 52)));
        // Arrow nudges and edge adjustments accelerate the same way.
        let before = o.selection().unwrap();
        act(&mut o, Action::Move(Dir::Right));
        for _ in 0..10 {
            o.handle(Input::Repeat(Action::Move(Dir::Right)));
        }
        assert_eq!(o.selection().unwrap().x, before.x + 1 + 9 + 2);
        act(&mut o, Action::Grow(Dir::Down));
        for _ in 0..12 {
            o.handle(Input::Repeat(Action::Grow(Dir::Down)));
        }
        assert_eq!(o.selection().unwrap().height, before.height + 1 + 9 + 3 * 2);
        act(&mut o, Action::Shrink(Dir::Up));
        for _ in 0..12 {
            o.handle(Input::Repeat(Action::Shrink(Dir::Up)));
        }
        assert_eq!(o.selection().unwrap().height, before.height);
    }

    #[test]
    fn space_marks_two_corners_or_accepts_the_detection() {
        // FLOW-18: W A S D to (100,100), Space, to (500,400), Space, Enter.
        let mut o = one(800, 600);
        mv(&mut o, 90, 95);
        for _ in 0..10 {
            act(&mut o, Action::Pointer(Dir::Right));
        }
        for _ in 0..5 {
            act(&mut o, Action::Pointer(Dir::Down));
        }
        assert_eq!(o.pointer(), (100, 100));
        act(&mut o, Action::Mark);
        assert_eq!((o.phase(), o.selection()), (Phase::Marking, None));
        mv(&mut o, 495, 397);
        for _ in 0..5 {
            act(&mut o, Action::Pointer(Dir::Right));
        }
        for _ in 0..3 {
            act(&mut o, Action::Pointer(Dir::Down));
        }
        assert_eq!(
            o.live(),
            Some(Rect::new(100, 100, 401, 301)),
            "the band follows"
        );
        assert_eq!(
            act(&mut o, Action::Pin),
            Outcome::Stay,
            "nothing confirmed yet"
        );
        act(&mut o, Action::Mark);
        let s = Rect::new(100, 100, 401, 301);
        assert_eq!((o.phase(), o.selection()), (Phase::Selected, Some(s)));
        assert_eq!(
            act(&mut o, Action::Pin),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        // From a selection, Space starts a new one; a right-click restores the old.
        act(&mut o, Action::Mark);
        assert_eq!(o.phase(), Phase::Marking);
        press(&mut o, Button::Right, 600, 500);
        assert_eq!((o.phase(), o.selection()), (Phase::Selected, Some(s)));
        // A click sets the second corner of a marked one.
        mv(&mut o, 10, 10);
        act(&mut o, Action::Mark);
        press(&mut o, Button::Left, 20, 30);
        release(&mut o, 20, 30);
        assert_eq!(o.selection(), Some(Rect::new(10, 10, 11, 21)));
        // J2 by keyboard: Space over a highlighted window selects it.
        let mut o = detecting(windows());
        mv(&mut o, 150, 150);
        act(&mut o, Action::Mark);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 300, 200)));
        // After the keyboard moved the pointer, Space marks a corner instead.
        let mut o = detecting(windows());
        mv(&mut o, 150, 150);
        act(&mut o, Action::Pointer(Dir::Left));
        act(&mut o, Action::Mark);
        assert_eq!(o.phase(), Phase::Marking);
    }

    #[test]
    fn moving_edges_snap_to_windows_and_monitors_unless_alt_or_the_keyboard() {
        let target = || vec![WindowFacts::plain(1, Rect::new(200, 200, 300, 200))];
        let mut o = detecting(target());
        // The drag's right/bottom edges end 3 pixels short of the window's: they snap.
        drag(&mut o, (210, 210), (496, 396));
        assert_eq!(o.selection(), Some(Rect::new(210, 210, 290, 190)));
        // Farther than 6 logical pixels: no snap (a right-click outside clears first).
        press(&mut o, Button::Right, 790, 590);
        drag(&mut o, (210, 210), (490, 390));
        assert_eq!(o.selection(), Some(Rect::new(210, 210, 281, 181)));
        // Dragging up and left, the left/top edges snap.
        press(&mut o, Button::Right, 790, 590);
        drag(&mut o, (450, 350), (203, 197));
        assert_eq!(o.selection(), Some(Rect::new(200, 200, 251, 151)));
        // With Alt held: exact.
        press(&mut o, Button::Right, 790, 590);
        press(&mut o, Button::Left, 100, 100);
        o.handle(Input::Move {
            x: 496,
            y: 396,
            mods: Mods::ALT,
        });
        release(&mut o, 496, 396);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 397, 297)));
        // Resizing by the right edge snaps to the monitor's edge (800).
        let mut o = detecting(Vec::new());
        drag(&mut o, (100, 100), (299, 299));
        press(&mut o, Button::Left, 300, 200);
        mv(&mut o, 500, 200);
        mv(&mut o, 796, 200);
        release(&mut o, 796, 200);
        assert_eq!(o.selection(), Some(Rect::new(100, 100, 700, 200)));
        // Pointer keys are exact: no snapping while the keyboard moved the pointer last.
        let mut o = detecting(target());
        mv(&mut o, 210, 210);
        act(&mut o, Action::Pointer(Dir::Right));
        act(&mut o, Action::Pointer(Dir::Left));
        act(&mut o, Action::Mark);
        mv(&mut o, 494, 396);
        act(&mut o, Action::Pointer(Dir::Right));
        act(&mut o, Action::Mark);
        assert_eq!(o.selection(), Some(Rect::new(210, 210, 286, 187)));
        // Detection off: no window edges.
        let mut o = detecting(target());
        act(&mut o, Action::DetectCycle);
        act(&mut o, Action::DetectCycle);
        drag(&mut o, (210, 210), (496, 396));
        assert_eq!(o.selection(), Some(Rect::new(210, 210, 287, 187)));
    }

    fn frame_of(pic: Rect) -> (std::sync::Arc<Ledger>, Frame) {
        let ledger = Ledger::new();
        let syn = Synthetic::new(vec![Monitor::new(pic, true)]);
        let f = crate::capture::CaptureBackend::capture(&syn, &ledger, pic).unwrap();
        (ledger, f)
    }

    #[test]
    fn the_magnifier_enlarges_exact_pixels_with_a_crosshair_and_reads_the_colour() {
        let pic = Rect::new(0, 0, 800, 600);
        let (_l, frozen) = frame_of(pic);
        let mut o = one(800, 600).with_magnifier(true);
        mv(&mut o, 50, 40);
        let mut s = o.scene(&Approx);
        s.sample(&frozen);
        let m = s.magnifier.clone().expect("shown while selecting");
        assert_eq!(m.cell, 8);
        assert_eq!(m.grid.width, 8 * MAG_CELLS);
        assert_eq!(
            (m.rect.x, m.rect.y),
            (70, 60),
            "right of and below the pointer"
        );
        let p = Synthetic::pixel(50, 40);
        assert_eq!(m.text.lines[0], "50, 40");
        assert_eq!(
            m.text.lines[1],
            format!("#{:02X}{:02X}{:02X}", p[2], p[1], p[0])
        );
        assert_eq!(m.text.lines[2], format!("{}, {}, {}", p[2], p[1], p[0]));
        assert_eq!(color_lines(&frozen, (5000, 5)), ["\u{2014}", "\u{2014}"]);
        // Composed: every cell is its source pixel, the centre cell is outlined, the crosshair
        // runs through the middle.
        let mut out = vec![0u8; 800 * 600 * 4];
        compose(&frozen, None, &s, pic, &mut out);
        let at = |out: &[u8], x: i32, y: i32| {
            let i = ((y * 800 + x) * 4) as usize;
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        let half = (MAG_CELLS / 2) as i32;
        for (i, j) in [(0, 0), (3, 11), (14, 14), (6, 5)] {
            let src = Synthetic::pixel(50 - half + i, 40 - half + j);
            let cx = m.grid.x + i * 8 + 2;
            let cy = m.grid.y + j * 8 + 2;
            assert_eq!(at(&out, cx, cy), src, "cell ({i}, {j})");
        }
        let c = m.centre();
        assert_eq!(
            at(&out, c.x, c.y),
            color::ACCENT,
            "the centre cell's outline"
        );
        assert_eq!(
            at(&out, c.x + 2, c.y + 2),
            p,
            "the pointer's pixel inside it"
        );
        assert_eq!(
            at(&out, c.x + 4, m.grid.y + 1),
            color::ACCENT,
            "the crosshair"
        );
        // Near the bottom-right corner it moves left of and above the pointer.
        mv(&mut o, 790, 590);
        let m = o.scene(&Approx).magnifier.unwrap();
        assert!(m.rect.right() <= 790 && m.rect.bottom() <= 590);
        // At the picture's corner, cells outside it are dark.
        mv(&mut o, 0, 0);
        let mut s = o.scene(&Approx);
        s.sample(&frozen);
        let m = s.magnifier.clone().unwrap();
        compose(&frozen, None, &s, pic, &mut out);
        assert_eq!(
            at(&out, m.grid.x + 2, m.grid.y + 2),
            color::HALO,
            "outside the picture"
        );
        // M hides and shows it; it hides while moving the selection and over its inside.
        act(&mut o, Action::Magnifier);
        assert!(o.scene(&Approx).magnifier.is_none());
        act(&mut o, Action::Magnifier);
        drag(&mut o, (100, 100), (399, 299));
        mv(&mut o, 250, 200);
        assert!(o.scene(&Approx).magnifier.is_none(), "inside the selection");
        mv(&mut o, 600, 500);
        assert!(o.scene(&Approx).magnifier.is_some(), "outside it");
        mv(&mut o, 399, 299);
        assert!(
            o.scene(&Approx).magnifier.is_some(),
            "on a handle: precise resizing"
        );
        // It is redrawn where it was and where it is now.
        mv(&mut o, 600, 500);
        let a = o.scene(&Approx);
        mv(&mut o, 650, 450);
        let b = o.scene(&Approx);
        let d = dirty(&a, &b);
        let covered = |r: Rect| d.iter().any(|x| x.contains(&r));
        assert!(covered(a.magnifier.unwrap().rect) && covered(b.magnifier.unwrap().rect));
    }

    #[test]
    fn the_magnifier_follows_the_monitor_scale_and_shows_logical_coordinates() {
        // Mixed DPI: a 100 % monitor and a 150 % one to its right.
        let mons = vec![
            Monitor::new(Rect::new(0, 0, 800, 600), true),
            Monitor::new(Rect::new(800, 0, 1200, 900), false).with_dpi(144),
        ];
        let mut o = Overlay::new(
            Rect::new(0, 0, 2000, 900),
            mons,
            SystemValues::default(),
            (0, 0),
            None,
            None,
        );
        mv(&mut o, 400, 300);
        let m = o.scene(&Approx).magnifier.unwrap();
        assert_eq!(
            (m.cell, m.text.lines.len()),
            (8, 3),
            "100 %: no logical line"
        );
        mv(&mut o, 800 + 300, 450);
        let m = o.scene(&Approx).magnifier.unwrap();
        assert_eq!(m.cell, 12, "8 logical pixels at 150 %");
        assert_eq!(m.text.lines[0], "1100, 450");
        assert_eq!(
            m.text.lines[1], "200, 300 at 150 %",
            "logical, on that monitor"
        );
        assert_eq!(m.color, 2);
        assert_eq!(m.text.dpi, 144);
    }

    #[test]
    fn history_steps_through_earlier_rectangles_over_the_current_picture() {
        let a = Rect::new(10, 10, 100, 100);
        let b = Rect::new(20, 20, 50, 50);
        let c = Rect::new(700, 500, 200, 200); // sticks out: clipped
        let gone = Rect::new(5000, 5000, 10, 10); // off this picture: left out
        let mut o = one(800, 600).with_history(&[a, b, b, gone, c]);
        assert_eq!(o.history(), (3, None), "repeats and absent ones left out");
        act(&mut o, Action::HistoryNext);
        assert_eq!(o.selection(), None, "nothing newer than now");
        act(&mut o, Action::HistoryPrevious);
        assert_eq!(o.selection(), Some(Rect::new(700, 500, 100, 100)));
        act(&mut o, Action::HistoryPrevious);
        assert_eq!(o.selection(), Some(b));
        act(&mut o, Action::HistoryPrevious);
        act(&mut o, Action::HistoryPrevious);
        assert_eq!(
            (o.selection(), o.history()),
            (Some(a), (3, Some(0))),
            "stops at the oldest"
        );
        act(&mut o, Action::HistoryNext);
        assert_eq!(o.selection(), Some(b));
        assert_eq!(o.phase(), Phase::Selected);
        // A new drag leaves the history; Previous starts again from the newest.
        drag(&mut o, (300, 300), (350, 350));
        assert_eq!(o.history().1, None);
        act(&mut o, Action::HistoryPrevious);
        assert_eq!(o.selection(), Some(Rect::new(700, 500, 100, 100)));
        // Bounded.
        let many: Vec<Rect> = (0..40).map(|i| Rect::new(i, 0, 5, 5)).collect();
        let o = one(800, 600).with_history(&many);
        assert_eq!(o.history().0, HISTORY);
        assert_eq!(one(800, 600).history(), (0, None));
    }

    // ----- Step 56: annotation (UX spec §4.9, NOEUL-FR-080…083, FLOW-13…15) -----

    use crate::annotation::{BlockType, Bounds, DEFAULT_LEVEL, PALETTE, Shape};

    /// The centre of a toolbar button.
    fn button(o: &Overlay, item: ToolbarItem) -> (i32, i32) {
        let bar = o.scene(&Approx).toolbar.unwrap();
        let b = bar.buttons.iter().find(|b| b.item == item).unwrap().rect;
        (
            b.x + i32::try_from(b.width / 2).unwrap(),
            b.y + i32::try_from(b.height / 2).unwrap(),
        )
    }

    fn tool(o: &mut Overlay, t: Tool) {
        o.handle(Input::Annotate(AnnotateAction::Tool(t)));
    }

    fn red_rect(b: Bounds) -> Object {
        Object {
            id: 1,
            shape: Shape::Rectangle(b),
            color: PALETTE[0].1,
            level: 0,
            dpi: 96,
        }
    }

    #[test]
    fn a_tool_draws_inside_never_pins_and_escape_leaves_it_with_the_drawing() {
        let (mut o, s) = flow();
        let pen = button(&o, ToolbarItem::Tool(Tool::Pen));
        click(&mut o, pen.0, pen.1, 0);
        assert_eq!(o.editor().tool(), Some(Tool::Pen));
        assert_eq!(o.phase(), Phase::Annotating);
        let scene = o.scene(&Approx);
        assert!(
            scene.handles.is_empty(),
            "a tool hides the selection's handles"
        );
        let bar = scene.toolbar.unwrap();
        assert!(
            bar.buttons
                .iter()
                .any(|b| b.active && b.item == ToolbarItem::Tool(Tool::Pen))
        );
        assert_eq!(o.zone((101, 101)), Zone::Interior, "no handle zones");
        mv(&mut o, 300, 250);
        assert_eq!(o.cursor(), Cursor::Cross);
        // FLOW-13 / ANN-17: a double-click inside draws (two dots) and pins nothing.
        assert_eq!(
            double(&mut o, (300, 250), (300, 250), 1000, 100),
            Outcome::Stay
        );
        assert_eq!(o.annotations().len(), 2);
        assert_eq!(o.selection(), Some(s), "the selection did not move");
        // A drag draws a stroke; it does not move the selection.
        press_at(&mut o, Button::Left, 150, 150, Mods::NONE, 2000);
        assert_eq!(o.phase(), Phase::Annotating);
        mv(&mut o, 200, 180);
        mv(&mut o, 250, 220);
        release(&mut o, 250, 220);
        assert_eq!(o.annotations().len(), 3);
        assert_eq!(o.selection(), Some(s));
        // A press outside the selection with a tool does nothing (no new selection, no pin).
        click(&mut o, 700, 550, 3000);
        assert_eq!((o.selection(), o.annotations().len()), (Some(s), 3));
        // Enter pins with the drawing (the owner takes the objects).
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        // FLOW-15: Escape leaves the tool and keeps the drawing; then a double-click pins.
        assert!(o.annotation_active());
        o.handle(Input::Annotate(AnnotateAction::Done));
        assert_eq!(o.editor().tool(), None);
        assert_eq!(o.phase(), Phase::Selected);
        assert_eq!(o.annotations().len(), 3);
        assert!(
            !o.annotation_active(),
            "Escape now belongs to the capture overlay"
        );
        assert!(!o.scene(&Approx).handles.is_empty(), "handles are back");
        assert_eq!(double(&mut o, (300, 250), (300, 250), 5000, 100), pinned(s));
        // The tool button toggles.
        let mut o2 = flow().0;
        tool(&mut o2, Tool::Line);
        tool(&mut o2, Tool::Line);
        assert_eq!(o2.editor().tool(), None);
    }

    #[test]
    fn keyboard_only_annotation_tabs_to_a_tool_marks_two_points_and_enter_pins() {
        // J5 keyboard path: Tab to the rectangle tool, Enter, mark, pointer keys, mark, Enter.
        let (mut o, s) = flow();
        for _ in 0..4 {
            o.handle(Input::Nav(Nav::Next));
        }
        assert_eq!(o.toolbar_focus(), Some(ToolbarItem::Tool(Tool::Rectangle)));
        o.handle(Input::Nav(Nav::Activate));
        assert_eq!(o.editor().tool(), Some(Tool::Rectangle));
        assert_eq!(
            o.toolbar_focus(),
            None,
            "the canvas takes the keys after a tool is chosen"
        );
        // The pointer is where the selection's drag ended (499, 399).
        o.handle(Input::Action(Action::Mark));
        assert!(o.editor().marking());
        for _ in 0..20 {
            o.handle(Input::Action(Action::Pointer(Dir::Left)));
        }
        for _ in 0..10 {
            o.handle(Input::Action(Action::Pointer(Dir::Up)));
        }
        let chip = o.scene(&Approx).chip.unwrap();
        assert_eq!(chip.lines[1], "21 \u{d7} 11", "the size readout");
        o.handle(Input::Action(Action::Mark));
        assert!(!o.editor().marking());
        assert_eq!(
            o.annotations()[0].shape,
            Shape::Rectangle(Bounds::new(479.0, 389.0, 500.0, 400.0))
        );
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        // Enter during a keyboard gesture keeps the shape drawn so far.
        let (mut o, s) = flow();
        tool(&mut o, Tool::Arrow);
        o.handle(Input::Action(Action::Mark));
        o.handle(Input::Action(Action::Pointer(Dir::Left)));
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
        assert_eq!(o.annotations().len(), 1);
        assert!(!o.editor().marking());
    }

    #[test]
    fn undo_redo_width_object_keys_order_delete_and_clear_all() {
        let (mut o, s) = flow();
        tool(&mut o, Tool::Rectangle);
        drag(&mut o, (150, 150), (200, 200));
        drag(&mut o, (300, 150), (350, 200));
        assert_eq!(o.annotations().len(), 2);
        o.handle(Input::Annotate(AnnotateAction::Undo));
        assert_eq!(o.annotations().len(), 1);
        o.handle(Input::Annotate(AnnotateAction::Redo));
        assert_eq!(o.annotations().len(), 2);
        // The wheel (whole notches) and the width keys.
        o.handle(Input::Wheel(120));
        assert_eq!(o.editor().level(), DEFAULT_LEVEL + 1);
        o.handle(Input::Wheel(60));
        o.handle(Input::Wheel(-60));
        assert_eq!(o.editor().level(), DEFAULT_LEVEL + 1, "half notches add up");
        o.handle(Input::Annotate(AnnotateAction::WidthDown));
        assert_eq!(o.editor().level(), DEFAULT_LEVEL);
        // N selects an object; the selection keys then act on it, not on the capture.
        o.handle(Input::Annotate(AnnotateAction::SelectNext));
        let id = o.editor().selected().unwrap();
        o.handle(Input::Action(Action::Move(Dir::Right)));
        o.handle(Input::Action(Action::Grow(Dir::Down)));
        assert_eq!(o.selection(), Some(s), "the capture selection stays");
        let shape = |o: &Overlay| {
            o.annotations()
                .iter()
                .find(|x| x.id == id)
                .unwrap()
                .shape
                .clone()
        };
        assert_eq!(
            shape(&o),
            Shape::Rectangle(Bounds::new(151.0, 150.0, 202.0, 202.0))
        );
        let a = o.scene(&Approx).annotation.unwrap();
        assert_eq!(a.handles.len(), 8);
        assert!(a.outline.is_some());
        // Order, delete, clear all (undoable).
        o.handle(Input::Annotate(AnnotateAction::Order(Order::Front)));
        assert_eq!(o.annotations().last().unwrap().id, id);
        o.handle(Input::Annotate(AnnotateAction::Delete));
        assert_eq!(o.annotations().len(), 1);
        o.handle(Input::Annotate(AnnotateAction::ClearAll));
        assert!(o.annotations().is_empty());
        // The toolbar's Undo button undoes; Redo redoes; the colour button recolours.
        let undo = button(&o, ToolbarItem::Undo);
        click(&mut o, undo.0, undo.1, 0);
        assert_eq!(o.annotations().len(), 1);
        let redo = button(&o, ToolbarItem::Redo);
        click(&mut o, redo.0, redo.1, 0);
        assert!(o.annotations().is_empty());
        let color = button(&o, ToolbarItem::Color);
        click(&mut o, color.0, color.1, 0);
        assert_eq!(o.editor().color().0, "orange");
        press_at(&mut o, Button::Left, color.0, color.1, Mods::SHIFT, 0);
        release(&mut o, color.0, color.1);
        assert_eq!(o.editor().color().0, "red", "Shift goes back");
        let width = button(&o, ToolbarItem::Width);
        for _ in 0..7 {
            click(&mut o, width.0, width.1, 0);
        }
        assert_eq!(o.editor().level(), 2, "it cycles back from the thickest");
    }

    #[test]
    fn text_labels_take_characters_and_the_ime_and_a_double_click_selects_a_word() {
        let (mut o, s) = flow();
        tool(&mut o, Tool::Text);
        click(&mut o, 150, 150, 0);
        assert!(o.editing_text());
        assert_eq!(o.phase(), Phase::Annotating);
        assert!(
            o.ime_caret().is_some(),
            "the window is asked for the input method at the caret"
        );
        for c in "one two ".chars() {
            o.handle(Input::Char(c));
        }
        // 한 composed by the IME: shown while composing, committed as text.
        o.ime(Ime::Start);
        o.ime(Ime::Compose("\u{314e}".into()));
        o.ime(Ime::Compose("\u{d55c}".into()));
        assert!(o.editor().composing());
        o.ime(Ime::Commit("\u{d55c}".into()));
        o.ime(Ime::End);
        o.typeset(&BlockType);
        let caret = o.ime_caret().unwrap();
        assert_eq!(caret.x, 150 + 9 * 11, "after 9 characters of 11 px");
        // FLOW-14: a double-click on "two" selects it; nothing is pinned.
        let x = 150 + 5 * 11;
        assert_eq!(double(&mut o, (x, 155), (x, 155), 1000, 100), Outcome::Stay);
        let a = o.scene(&Approx).annotation.unwrap();
        assert_eq!(a.text_selection, [Rect::new(194, 150, 33, 23)]);
        o.handle(Input::Char('2'));
        o.handle(Input::TextKey(TextKey::End));
        o.handle(Input::TextKey(TextKey::Backspace));
        // Escape ends the label (the tool stays); Enter pins with it.
        o.handle(Input::Annotate(AnnotateAction::Done));
        assert!(!o.editing_text());
        assert_eq!(o.ime_caret(), None, "the input method is off again");
        let Shape::Text(t) = &o.annotations()[0].shape else {
            panic!()
        };
        assert_eq!(t.text, "one 2 ");
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, s, Via::Key)
        );
    }

    #[test]
    fn right_click_finishes_a_shape_or_leaves_the_tool_and_a_lost_capture_aborts() {
        let (mut o, _) = flow();
        tool(&mut o, Tool::Line);
        press(&mut o, Button::Left, 150, 150);
        mv(&mut o, 200, 200);
        assert_eq!(press(&mut o, Button::Right, 200, 200), Outcome::Stay);
        assert_eq!(o.annotations().len(), 1, "finished (ANN-10)");
        mv(&mut o, 260, 260);
        release(&mut o, 260, 260);
        assert_eq!(
            o.annotations()[0].shape,
            Shape::Line(
                annotation::P::of_pixel((150, 150)),
                annotation::P::of_pixel((200, 200))
            ),
            "the rest of the gesture changes nothing"
        );
        assert_eq!(
            press(&mut o, Button::Right, 300, 300),
            Outcome::Stay,
            "no menu"
        );
        assert_eq!(o.editor().tool(), None, "a right-click leaves the tool");
        tool(&mut o, Tool::Arrow);
        press(&mut o, Button::Left, 150, 250);
        mv(&mut o, 250, 250);
        o.handle(Input::CaptureLost);
        assert_eq!(o.annotations().len(), 1, "the arrow was aborted");
        assert_eq!(o.phase(), Phase::Annotating);
    }

    #[test]
    fn the_pin_editor_keeps_its_selection_crops_inside_the_pin_and_commits_with_enter() {
        let canvas = Rect::new(0, 0, 700, 500);
        let pin = Rect::new(100, 50, 300, 200);
        let mut o = Overlay::for_pin(
            canvas,
            vec![Monitor::new(Rect::new(0, 0, 800, 600), true)],
            SystemValues::default(),
            pin,
            vec![red_rect(Bounds::new(110.0, 60.0, 150.0, 90.0))],
        );
        assert_eq!(o.mode(), Mode::PinEdit);
        assert_eq!(o.selection(), Some(pin));
        assert_eq!(o.annotations().len(), 1, "the pin's annotations, editable");
        let scene = o.scene(&Approx);
        assert!(scene.layered && scene.magnifier.is_none() && scene.help.is_none());
        let bar = scene.toolbar.unwrap();
        assert_eq!(bar.buttons[0].item, ToolbarItem::Done);
        assert!(bar.buttons[0].primary);
        assert_eq!(bar.buttons.len(), EDIT_TOOLBAR.len());
        assert!(canvas.contains(&bar.rect));
        // Presses outside the pin and right-clicks never lose the selection; losing focus keeps
        // the work.
        click(&mut o, 600, 20, 0);
        press(&mut o, Button::Right, 600, 20);
        press(&mut o, Button::Right, 200, 150);
        assert_eq!(o.selection(), Some(pin));
        assert_eq!(o.handle(Input::FocusLost), Outcome::Stay);
        // No double-click output, no copy or save, no detection or history.
        assert_eq!(
            double(&mut o, (200, 150), (200, 150), 0, 100),
            Outcome::Stay
        );
        for a in [
            Action::Copy,
            Action::Save,
            Action::QuickSave,
            Action::Full,
            Action::Mark,
        ] {
            assert_eq!(o.handle(Input::Action(a)), Outcome::Stay, "{a:?}");
        }
        assert_eq!(o.selection(), Some(pin));
        // Crop: the bottom-right handle inwards, then outwards: never beyond the picture.
        press(&mut o, Button::Left, 399, 249);
        mv(&mut o, 350, 200);
        release(&mut o, 350, 200);
        assert_eq!(o.selection(), Some(Rect::new(100, 50, 251, 151)));
        press(&mut o, Button::Left, 350, 200);
        mv(&mut o, 650, 480);
        release(&mut o, 650, 480);
        assert_eq!(o.selection(), Some(pin));
        o.handle(Input::Action(Action::Shrink(Dir::Left)));
        let keep = Rect::new(100, 50, 299, 200);
        assert_eq!(o.selection(), Some(keep));
        // Enter (Done) commits; Escape cancels.
        assert_eq!(
            o.handle(Input::Action(Action::Pin)),
            Outcome::Output(Output::Pin, keep, Via::Key)
        );
        let done = button(&o, ToolbarItem::Done);
        assert_eq!(
            click(&mut o, done.0, done.1, 0).1,
            Outcome::Output(Output::Pin, keep, Via::Toolbar)
        );
        assert_eq!(o.handle(Input::Leave), Outcome::Cancel);
        // Drawing works as in the overlay, inside the pin.
        tool(&mut o, Tool::Pen);
        drag(&mut o, (120, 120), (600, 450));
        let Shape::Pen(points) = &o.annotations()[1].shape else {
            panic!()
        };
        assert_eq!(
            points.last(),
            Some(&annotation::P::of_pixel((398, 249))),
            "kept inside the part the pin keeps"
        );
    }

    #[test]
    fn compose_shows_the_annotated_layer_inside_and_keeps_a_layered_canvas_transparent() {
        let ledger = Ledger::new();
        let canvas = Rect::new(0, 0, 700, 300);
        let mut px = vec![0u8; canvas.bgra_bytes() as usize];
        let at_px = |x: usize, y: usize| (y * 700 + x) * 4;
        for y in 10..30 {
            for x in 10..50 {
                let i = at_px(x, y);
                px[i..i + 4].copy_from_slice(&[10, 20, 30, 255]);
            }
        }
        let i = at_px(12, 28);
        px[i..i + 4].copy_from_slice(&[100, 100, 100, 128]);
        let base = Frame::new(&ledger, canvas, px).unwrap();
        let pin = Rect::new(10, 10, 40, 20);
        let obj = red_rect(Bounds::new(10.0, 10.0, 50.0, 30.0));
        let o = Overlay::for_pin(
            canvas,
            vec![Monitor::new(Rect::new(0, 0, 800, 600), true)],
            SystemValues::default(),
            pin,
            vec![obj.clone()],
        );
        let layer = annotation::flatten(&base, &[obj], pin).unwrap();
        let scene = o.scene(&Approx);
        let mut out = vec![0u8; canvas.bgra_bytes() as usize];
        compose(&base, Some(&layer), &scene, canvas, &mut out);
        let at = |out: &[u8], x: usize, y: usize| {
            let i = at_px(x, y);
            [out[i], out[i + 1], out[i + 2], out[i + 3]]
        };
        let red = PALETTE[0].1;
        assert_eq!(
            at(&out, 10, 25),
            [red.b, red.g, red.r, 255],
            "the annotation"
        );
        assert_eq!(at(&out, 11, 25), [10, 20, 30, 255], "the picture inside");
        assert_eq!(
            at(&out, 12, 28),
            [50, 50, 50, 128],
            "premultiplied for the layered window"
        );
        assert_eq!(
            at(&out, 650, 200),
            [0, 0, 0, 0],
            "the canvas around the pin is clear"
        );
        // The same scene without the layer shows the unannotated picture.
        compose(&base, None, &scene, canvas, &mut out);
        assert_eq!(at(&out, 10, 25), [10, 20, 30, 255]);
    }
}
