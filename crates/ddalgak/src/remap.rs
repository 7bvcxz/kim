//! Remap outputs, previews and the editor's form (docs/DDALGAK_RULE_MODEL.md §5.1, §5.2;
//! `DDALGAK-FR-010`; ADR 0031).
//!
//! A remap's `to` (and its dual-role `tap`) is one output chord: modifiers to hold plus one key,
//! one mouse button, or nothing more (a bare modifier such as `LCtrl` or `RWin`). [`Output`] is the
//! compiled form the engine core keeps (plain numbers, `Copy`, no allocation in the hook):
//!
//! - logical keys (`Down`, `F16`, `HangulMode`) are sent by virtual key, so the active layout
//!   decides what they type, like a key with that label;
//! - physical keys (`code:KeyH`) are sent by scan code: the key at that position on any layout;
//! - `NumpadEnter` keeps its extended flag;
//! - `MouseLeft`/`MouseRight` are the logical primary and secondary buttons (they follow the
//!   "swap buttons" setting), `MouseMiddle`, `MouseX1`, `MouseX2`;
//! - `AltGr` in an output is sent as Right Alt (on AltGr layouts Windows adds its fake Left Ctrl).
//!
//! The same module gives the editor (Step 74) and the playground (Step 76) what they show: the
//! form's fields ([`FIELDS`]), a one-line summary ([`summary`]) and the preview of §5.1/§5.2
//! ([`preview`]). Previews name keys and modes only; they never contain typed text or targets
//! (`DDALGAK-SEC-011`).

use crate::inject::{Button, OutCode};
use crate::trigger::{TKey, Trigger};
use crate::vk::{self, ModKey};
use kim_hotkeys::keys::{Chord, Key, Mods, Side};
use serde_json::{Value, json};

/// One compiled output chord.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Output {
    /// Modifiers to hold, by family and side (`Any` = Left when Ddalgak presses it).
    pub mods: Mods,
    /// The key or button; `None` for a bare modifier (it is in `mods`).
    pub key: Option<OutCode>,
}

impl Output {
    /// A mouse button without modifiers (the target of a held `mouse_click`, Step 71).
    pub fn button(b: Button) -> Output {
        Output {
            mods: Mods::default(),
            key: Some(OutCode::Button(b)),
        }
    }

    /// Parses canonical output text (`to`/`tap`): one chord, a bare modifier including
    /// `LWin`/`RWin`, a lock key, a `code:` key or a mouse button, with modifiers.
    pub fn parse(text: &str) -> Result<Output, String> {
        let bad = |why: &str| Err(format!("`{text}`: {why}"));
        if text.contains(' ') {
            return bad("a remap sends one chord");
        }
        if let Some(m) = ModKey::from_name(text) {
            return Ok(Output {
                mods: bare(m),
                key: None,
            });
        }
        let special = ["CapsLock", "NumLock"]
            .into_iter()
            .map(|n| (n, OutCode::Vk(vk::named_vk(n).unwrap_or(0))))
            .chain(
                Button::ALL
                    .into_iter()
                    .map(|b| (b.name(), OutCode::Button(b))),
            )
            .find(|(n, _)| text == *n || text.ends_with(&format!("+{n}")));
        let (mods, key) = if let Some((name, code)) = special {
            let prefix = &text[..text.len() - name.len()];
            let chord = Chord::parse(&format!("{prefix}F1")).map_err(|e| e.message)?;
            (chord.mods, code)
        } else {
            let chord = Chord::parse(text).map_err(|e| format!("`{text}`: {}", e.message))?;
            let code = match chord.key {
                Key::Named("NumpadEnter") => OutCode::VkExtended(vk::VK_RETURN),
                Key::Named(n) => match vk::named_vk(n) {
                    Some(v) => OutCode::Vk(v),
                    None => return bad("unknown key"),
                },
                Key::Code(c) => match vk::code_scan(c) {
                    Some((s, _)) => OutCode::Scan(s),
                    None => return bad("unknown key position"),
                },
                Key::Char(_) => return bad("character keys are not outputs; use type_text"),
            };
            (chord.mods, code)
        };
        let mut mods = mods;
        if mods.altgr {
            mods.altgr = false;
            mods.alt = Side::Right;
        }
        Ok(Output {
            mods,
            key: Some(key),
        })
    }

    /// The modifier keys this output asks for when nothing is held (generic = left side).
    pub fn mod_keys(&self) -> Vec<ModKey> {
        let mut out = Vec::new();
        for (side, l, r) in families(&self.mods) {
            match side {
                Side::None => {}
                Side::Any | Side::Left => out.push(l),
                Side::Right => out.push(r),
            }
        }
        out
    }
}

/// The modifier families of `m`: (wanted side, left key, right key).
pub fn families(m: &Mods) -> [(Side, ModKey, ModKey); 4] {
    [
        (m.ctrl, ModKey::LCtrl, ModKey::RCtrl),
        (m.alt, ModKey::LAlt, ModKey::RAlt),
        (m.shift, ModKey::LShift, ModKey::RShift),
        (m.win, ModKey::LWin, ModKey::RWin),
    ]
}

fn bare(m: ModKey) -> Mods {
    let mut mods = Mods::default();
    let side = match m {
        ModKey::LCtrl | ModKey::LAlt | ModKey::LShift | ModKey::LWin => Side::Left,
        _ => Side::Right,
    };
    match m {
        ModKey::LCtrl | ModKey::RCtrl => mods.ctrl = side,
        ModKey::LAlt | ModKey::RAlt => mods.alt = side,
        ModKey::LShift | ModKey::RShift => mods.shift = side,
        ModKey::LWin | ModKey::RWin => mods.win = side,
    }
    mods
}

/// The friendly name of an output or trigger key for summaries ("Left Ctrl", "Escape").
fn friendly(text: &str) -> String {
    let names = [
        ("LCtrl", "Left Ctrl"),
        ("RCtrl", "Right Ctrl"),
        ("LAlt", "Left Alt"),
        ("RAlt", "Right Alt"),
        ("LShift", "Left Shift"),
        ("RShift", "Right Shift"),
        ("LWin", "Left Windows key"),
        ("RWin", "Right Windows key"),
        ("HangulMode", "한/영 (HangulMode)"),
        ("MouseLeft", "the primary mouse button"),
        ("MouseRight", "the secondary mouse button"),
        ("MouseMiddle", "the middle mouse button"),
        ("MouseX1", "mouse button X1 (back)"),
        ("MouseX2", "mouse button X2 (forward)"),
    ];
    names
        .iter()
        .find(|(k, _)| *k == text)
        .map_or_else(|| text.to_string(), |(_, v)| (*v).to_string())
}

/// What a trigger's held modifiers do while a remap holds its target (§5.1): for a trigger with
/// modifiers they are released while the target is held (unless the original key passes
/// through); a trigger without modifiers combines with whatever is held.
pub fn released_modifiers(trigger: &Trigger, pass_through: bool) -> Vec<&'static str> {
    let Some(c) = trigger.single() else {
        return Vec::new();
    };
    if pass_through {
        return Vec::new();
    }
    let m = &c.mods;
    let mut out = Vec::new();
    for (side, name) in [
        (m.ctrl, "Ctrl"),
        (m.alt, "Alt"),
        (if m.altgr { Side::Any } else { Side::None }, "AltGr"),
        (m.shift, "Shift"),
    ] {
        match side {
            Side::None => {}
            Side::Any => out.push(name),
            Side::Left => out.push(match name {
                "Ctrl" => "LCtrl",
                "Alt" => "LAlt",
                _ => "LShift",
            }),
            Side::Right => out.push(match name {
                "Ctrl" => "RCtrl",
                "Alt" => "RAlt",
                _ => "RShift",
            }),
        }
    }
    out
}

/// The playground preview of a remap step (§5.1 outputs, §11), exactly the `preview_remap` shape
/// of `ddalgak-simulation-v1`: `holds`, `tap` when there is one, and the trigger modifiers that
/// are released while the target is held, when there are any.
pub fn preview(trigger: &str, to: &str, tap: Option<&str>, pass_through: bool) -> Value {
    let mut v = json!({ "holds": to });
    if let Some(t) = tap {
        v["tap"] = json!(t);
    }
    let released = Trigger::parse(trigger)
        .map(|t| released_modifiers(&t, pass_through))
        .unwrap_or_default();
    if !released.is_empty() {
        v["released_modifiers"] = json!(released);
    }
    v
}

/// Whether a plain trigger combines with held modifiers (§3.2): the editor says so next to the
/// summary ("Shift+CapsLock sends Shift+Escape").
pub fn widened(trigger: &str) -> bool {
    Trigger::parse(trigger)
        .ok()
        .and_then(|t| t.single().copied())
        .is_some_and(|c| c.mods.is_empty())
}

/// The editor's live summary sentence (GUI_JOURNEYS J4): "CapsLock acts as Left Ctrl; a quick
/// tap is Escape".
pub fn summary(trigger: &str, to: &str, tap: Option<&str>, pass_through: bool) -> String {
    let who = Trigger::parse(trigger)
        .ok()
        .and_then(|t| t.single().copied())
        .map_or_else(
            || trigger.to_string(),
            |c| match c.key {
                TKey::Modifier(m) => friendly(m.name()),
                TKey::Mouse(b) => {
                    let mut s = friendly(b.name());
                    if let Some(first) = s.get(..1) {
                        s = first.to_uppercase() + &s[1..];
                    }
                    if c.mods.is_empty() {
                        s
                    } else {
                        format!("{trigger} ({s})")
                    }
                }
                _ => trigger.to_string(),
            },
        );
    let mut s = format!("{who} acts as {}", friendly(to));
    if let Some(t) = tap {
        s.push_str(&format!("; a quick tap is {}", friendly(t)));
    }
    if pass_through {
        s.push_str("; the original key still reaches the app");
    }
    s
}

/// The `block` summary (§5.2).
pub fn block_summary(trigger: &str) -> String {
    format!("{trigger} does nothing (blocked)")
}

/// One field of the remap form (GUI_JOURNEYS J4, Step 74): the rule-document field it edits,
/// how it is entered, whether it is required, its default and the help line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The JSON path it edits inside the rule (`steps[0].to`, `trigger.pass_through`).
    pub path: &'static str,
    /// `capture` (key capture with "type instead"), `toggle`, `number`.
    pub input: &'static str,
    pub required: bool,
    pub default: Option<&'static str>,
    pub label: &'static str,
    pub help: &'static str,
}

/// The "Act as another key" form, in display order (required first).
pub const FIELDS: &[Field] = &[
    Field {
        path: "steps[0].to",
        input: "capture",
        required: true,
        default: None,
        label: "Acts as",
        help: "One key, chord, modifier or mouse button. Logical keys follow the layout; \"this key position\" stores code:Key… instead.",
    },
    Field {
        path: "steps[0].tap",
        input: "capture",
        required: false,
        default: None,
        label: "A quick tap alone sends",
        help: "Dual role: a press and release with no other key within the tap time sends this instead.",
    },
    Field {
        path: "steps[0].tap_ms",
        input: "number",
        required: false,
        default: Some("200"),
        label: "Tap time (ms)",
        help: "100–1000. Held longer, or another key pressed meanwhile: the key acts as the target.",
    },
    Field {
        path: "trigger.pass_through",
        input: "toggle",
        required: false,
        default: Some("false"),
        label: "Also let the original key through",
        help: "The original key reaches the app as well; not with a tap alternative.",
    },
];

/// The "Block this key" form has no inputs (§5.2).
pub const BLOCK_FIELDS: &[Field] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    fn out(t: &str) -> Output {
        Output::parse(t).unwrap_or_else(|e| panic!("{t}: {e}"))
    }

    #[test]
    fn outputs_compile_to_keys_scan_codes_buttons_and_bare_modifiers() {
        assert_eq!(out("F16").key, Some(OutCode::Vk(0x7F)));
        assert!(out("F16").mods.is_empty());
        let c = out("Ctrl+Shift+C");
        assert_eq!(c.key, Some(OutCode::Vk(u16::from(b'C'))));
        assert_eq!(c.mod_keys(), [ModKey::LCtrl, ModKey::LShift]);
        assert_eq!(out("RCtrl+Up").mod_keys(), [ModKey::RCtrl]);
        assert_eq!(out("NumpadEnter").key, Some(OutCode::VkExtended(0x0D)));
        assert_eq!(out("code:KeyH").key, Some(OutCode::Scan(0x23)));
        assert_eq!(out("CapsLock").key, Some(OutCode::Vk(0x14)));
        assert_eq!(out("Shift+CapsLock").mod_keys(), [ModKey::LShift]);
        assert_eq!(out("HangulMode").key, Some(OutCode::Vk(0x15)));
        assert_eq!(out("MediaPlayPause").key, Some(OutCode::Vk(0xB3)));
        assert_eq!(out("F24").key, Some(OutCode::Vk(0x87)));
        let l = out("LCtrl");
        assert_eq!((l.key, l.mod_keys()), (None, vec![ModKey::LCtrl]));
        assert_eq!(out("RWin").mod_keys(), [ModKey::RWin]);
        assert_eq!(out("Win+D").mod_keys(), [ModKey::LWin]);
        assert_eq!(out("MouseX1").key, Some(OutCode::Button(Button::X1)));
        let click = out("Ctrl+MouseLeft");
        assert_eq!(click.key, Some(OutCode::Button(Button::Left)));
        assert_eq!(click.mod_keys(), [ModKey::LCtrl]);
        assert_eq!(
            out("AltGr+Q").mod_keys(),
            [ModKey::RAlt],
            "AltGr is sent as RAlt"
        );
        for bad in ["", "F13 F14", "char:a", "ctrl+c", "Shift+Ctrl+C", "Mouse"] {
            assert!(Output::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn previews_and_summaries_name_keys_only() {
        let p = preview("CapsLock", "LCtrl", Some("Escape"), false);
        assert_eq!(p, json!({ "holds": "LCtrl", "tap": "Escape" }));
        assert!(widened("CapsLock") && !widened("Ctrl+Shift+J"));
        let p = preview("Ctrl+Shift+J", "Down", None, false);
        assert_eq!(
            p,
            json!({ "holds": "Down", "released_modifiers": ["Ctrl", "Shift"] })
        );
        assert_eq!(
            preview("RCtrl+F13", "F16", None, true),
            json!({ "holds": "F16" }),
            "pass-through keeps the trigger's modifiers"
        );
        // Every preview fits the playground contract (ddalgak-simulation-v1 `preview_remap`).
        let sim: Value = serde_json::from_str(include_str!(
            "../../../docs/contracts/schemas/ddalgak-simulation-v1.schema.json"
        ))
        .unwrap();
        let mut shape = sim["$defs"]["preview_remap"].clone();
        shape["$defs"] = sim["$defs"].clone();
        let v = crate::jsonschema::Validator::new(&shape);
        for p in [
            preview("CapsLock", "LCtrl", Some("Escape"), false),
            preview("Ctrl+Shift+J", "Down", None, false),
            preview("LCtrl+RAlt+F13", "MouseX1", Some("F16"), false),
        ] {
            assert!(v.validate(&p).unwrap().is_empty(), "{p}");
        }
        assert_eq!(
            summary("CapsLock", "LCtrl", Some("Escape"), false),
            "CapsLock acts as Left Ctrl; a quick tap is Escape"
        );
        assert_eq!(
            summary("RAlt", "HangulMode", None, false),
            "Right Alt acts as 한/영 (HangulMode)"
        );
        assert_eq!(
            summary("MouseX1", "Ctrl+C", None, false),
            "Mouse button X1 (back) acts as Ctrl+C"
        );
        assert_eq!(
            summary("F15", "F16", None, true),
            "F15 acts as F16; the original key still reaches the app"
        );
        assert_eq!(block_summary("Insert"), "Insert does nothing (blocked)");
    }

    #[test]
    fn the_form_fields_are_fields_of_the_published_schema() {
        let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
        let remap = &schema["$defs"]["step_remap"]["properties"];
        let trigger = &schema["$defs"]["trigger"]["properties"];
        for f in FIELDS {
            let known = match f.path.split_once('.') {
                Some(("steps[0]", name)) => remap.get(name).is_some(),
                Some(("trigger", name)) => trigger.get(name).is_some(),
                _ => false,
            };
            assert!(known, "{} is not a schema field", f.path);
        }
        assert!(FIELDS[0].required && FIELDS.iter().skip(1).all(|f| !f.required));
        assert!(BLOCK_FIELDS.is_empty());
    }
}
