//! Typed `ddalgak.rules` v1 documents (docs/DDALGAK_RULE_MODEL.md §2–§6, schema
//! `ddalgak-rules-v1`, `DDALGAK-FR-005`, `DDALGAK-NFR-005`, `DDALGAK-NFR-008`).
//!
//! - **Versioned.** [`Document`] carries `schema`, `schema_version` and `hotkey_contract`; the
//!   store ([`crate::store`]) refuses newer versions and migrates older ones.
//! - **Every object keeps the fields it does not know** in an `extra` map, so a rewrite never
//!   drops what a newer Ddalgak wrote (§9). Unknown fields inside a rule make it inactive
//!   (`rule_unsupported`, [`crate::validate`]); at the document, settings, profile and mode level
//!   they are only kept.
//! - **One rule that does not fit the types stays as written.** A rule that a newer version (or a
//!   hand edit) shaped differently is kept as [`RuleEntry::Raw`]: it is preserved byte for byte
//!   in meaning, listed with its problem and never run.
//! - **Stable IDs.** New rules get `r-<UUID v4>` ([`new_rule_id`]); an ID never changes after
//!   creation.

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// `schema` of a rules document.
pub const SCHEMA: &str = "ddalgak.rules";
/// The `schema_version` this Ddalgak reads and writes.
pub const SCHEMA_VERSION: u64 = 1;
/// The key-syntax version triggers use (HOTKEY_CONTRACT §16).
pub const HOTKEY_CONTRACT: u64 = 1;

fn none<T>(v: &Option<T>) -> bool {
    v.is_none()
}

/// A whole rules file (`%APPDATA%\mouv\ddalgak\rules.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub schema: String,
    pub schema_version: u64,
    pub hotkey_contract: u64,
    #[serde(default, skip_serializing_if = "none")]
    pub revision: Option<u64>,
    pub active_profile: String,
    #[serde(default, skip_serializing_if = "none")]
    pub settings: Option<Settings>,
    #[serde(default, skip_serializing_if = "none")]
    pub exported: Option<Value>,
    pub profiles: Vec<Profile>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `settings` (§2).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "none")]
    pub engine_on_start: Option<OnOff>,
    #[serde(default, skip_serializing_if = "none")]
    pub activity: Option<Value>,
    #[serde(default, skip_serializing_if = "none")]
    pub max_runs_per_second: Option<u32>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnOff {
    Off,
    On,
}

/// A switchable set of modes and rules (§10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "none")]
    pub description: Option<String>,
    pub default_mode: String,
    pub modes: Vec<Mode>,
    pub rules: Vec<RuleEntry>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A user-defined named state of a profile (§7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "none")]
    pub color: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One rule of a profile: typed when it fits the v1 types and the schema, otherwise kept exactly
/// as written (and never run).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuleEntry {
    Typed(Box<Rule>),
    Raw(Value),
}

impl RuleEntry {
    pub fn id(&self) -> Option<&str> {
        match self {
            RuleEntry::Typed(r) => Some(&r.id),
            RuleEntry::Raw(v) => v.get("id").and_then(Value::as_str),
        }
    }

    pub fn enabled(&self) -> Option<bool> {
        match self {
            RuleEntry::Typed(r) => Some(r.enabled),
            RuleEntry::Raw(v) => v.get("enabled").and_then(Value::as_bool),
        }
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            RuleEntry::Typed(r) => Some(&r.title),
            RuleEntry::Raw(v) => v.get("title").and_then(Value::as_str),
        }
    }

    pub fn rule(&self) -> Option<&Rule> {
        match self {
            RuleEntry::Typed(r) => Some(r),
            RuleEntry::Raw(_) => None,
        }
    }

    pub fn rule_mut(&mut self) -> Option<&mut Rule> {
        match self {
            RuleEntry::Typed(r) => Some(r),
            RuleEntry::Raw(_) => None,
        }
    }

    /// Sets `enabled` (both shapes; a raw rule keeps everything else as written).
    pub fn set_enabled(&mut self, on: bool) {
        match self {
            RuleEntry::Typed(r) => r.enabled = on,
            RuleEntry::Raw(Value::Object(m)) => {
                m.insert("enabled".into(), Value::Bool(on));
            }
            RuleEntry::Raw(_) => {}
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// *When* the trigger happens *do* the steps (§4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "none")]
    pub description: Option<String>,
    pub enabled: bool,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "none")]
    pub modes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "none")]
    pub when: Option<Condition>,
    #[serde(default, skip_serializing_if = "none")]
    pub priority: Option<i64>,
    pub steps: Vec<Action>,
    #[serde(default, skip_serializing_if = "none")]
    pub on_error: Option<OnError>,
    #[serde(default, skip_serializing_if = "none")]
    pub consent: Option<Consent>,
    #[serde(default, skip_serializing_if = "none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Rule {
    /// A new enabled rule with one step and a fresh ID.
    pub fn new(id: String, title: String, trigger: Trigger, steps: Vec<Action>) -> Rule {
        Rule {
            id,
            title,
            description: None,
            enabled: true,
            trigger,
            modes: None,
            when: None,
            priority: None,
            steps,
            on_error: None,
            consent: None,
            created_at: None,
            updated_at: None,
            extra: Map::new(),
        }
    }

    pub fn priority(&self) -> i64 {
        self.priority.unwrap_or(0)
    }

    /// The permissions the rule's steps need (§6.1), in a fixed order.
    pub fn permissions_needed(&self) -> Vec<Permission> {
        let mut out = Vec::new();
        for p in Permission::ALL {
            if self.steps.iter().any(|s| s.needs(p)) {
                out.push(p);
            }
        }
        out
    }

    /// The consent recorded for `p`, if any.
    pub fn grant(&self, p: Permission) -> Option<&Grant> {
        let c = self.consent.as_ref()?;
        match p {
            Permission::InputInjection => c.input_injection.as_ref(),
            Permission::LaunchPrograms => c.launch_programs.as_ref(),
            Permission::Clipboard => c.clipboard.as_ref(),
        }
    }

    /// Records (or replaces) the consent for `p`.
    pub fn set_grant(&mut self, p: Permission, g: Option<Grant>) {
        let c = self.consent.get_or_insert_with(Consent::default);
        match p {
            Permission::InputInjection => c.input_injection = g,
            Permission::LaunchPrograms => c.launch_programs = g,
            Permission::Clipboard => c.clipboard = g,
        }
        if c.input_injection.is_none()
            && c.launch_programs.is_none()
            && c.clipboard.is_none()
            && c.extra.is_empty()
        {
            self.consent = None;
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `trigger` (§3): keys, or (Step 69) a typed abbreviation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    /// The keys (§3.1); absent for a hotstring trigger.
    #[serde(default, skip_serializing_if = "none")]
    pub keys: Option<String>,
    /// A typed abbreviation (§3.4, Step 69); absent for a keys trigger.
    #[serde(default, skip_serializing_if = "none")]
    pub hotstring: Option<Hotstring>,
    #[serde(default, skip_serializing_if = "none")]
    pub on: Option<TriggerOn>,
    #[serde(default, skip_serializing_if = "none")]
    pub hold_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "none")]
    pub repeat: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub pass_through: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub injected: Option<InjectedPolicy>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Trigger {
    pub fn keys(keys: impl Into<String>) -> Trigger {
        Trigger {
            keys: Some(keys.into()),
            hotstring: None,
            on: None,
            hold_ms: None,
            repeat: None,
            pass_through: None,
            injected: None,
            extra: Map::new(),
        }
    }

    /// A hotstring trigger with the default options.
    pub fn hotstring(abbreviation: impl Into<String>) -> Trigger {
        Trigger {
            hotstring: Some(Hotstring::new(abbreviation)),
            keys: None,
            ..Trigger::keys("")
        }
    }

    /// The keys as text; empty for a hotstring trigger.
    pub fn keys_text(&self) -> &str {
        self.keys.as_deref().unwrap_or_default()
    }

    /// What the rule list and messages show: the keys, or `"btw" (hotstring)`.
    pub fn display(&self) -> String {
        match (&self.keys, &self.hotstring) {
            (Some(k), _) => k.clone(),
            (None, Some(h)) => format!("\"{}\" (hotstring)", h.abbreviation),
            (None, None) => String::new(),
        }
    }

    pub fn on(&self) -> TriggerOn {
        self.on.unwrap_or(TriggerOn::Press)
    }

    /// What makes two triggers claim the same input: keys, `on` and the hold time.
    pub fn claim(&self) -> (String, TriggerOn, u32) {
        (
            self.display(),
            self.on(),
            if self.on() == TriggerOn::Hold {
                self.hold_ms.unwrap_or(500)
            } else {
                500
            },
        )
    }
}

/// `trigger.hotstring` (§3.4, Step 69).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hotstring {
    pub abbreviation: String,
    #[serde(default, skip_serializing_if = "none")]
    pub case: Option<HotstringCase>,
    #[serde(default, skip_serializing_if = "none")]
    pub inside_words: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub immediate: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub end_chars: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub omit_end: Option<bool>,
    #[serde(default, skip_serializing_if = "none")]
    pub erase: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Hotstring {
    pub fn new(abbreviation: impl Into<String>) -> Hotstring {
        Hotstring {
            abbreviation: abbreviation.into(),
            case: None,
            inside_words: None,
            immediate: None,
            end_chars: None,
            omit_end: None,
            erase: None,
            extra: Map::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerOn {
    Press,
    Release,
    Hold,
}

impl TriggerOn {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerOn::Press => "press",
            TriggerOn::Release => "release",
            TriggerOn::Hold => "hold",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectedPolicy {
    Ignore,
    Accept,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    Stop,
    Continue,
}

/// `when` (§7.2; evaluated from Step 77, until then the rule is inactive).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Condition {
    #[serde(default, skip_serializing_if = "none")]
    pub apps: Option<Vec<AppMatcher>>,
    #[serde(default, skip_serializing_if = "none")]
    pub except_apps: Option<Vec<AppMatcher>>,
    #[serde(default, skip_serializing_if = "none")]
    pub window_title: Option<TitleMatch>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Condition {
    /// Whether any object inside carries a field this version does not know.
    pub fn has_unknown_fields(&self) -> Option<String> {
        if let Some(k) = self.extra.keys().next() {
            return Some(format!("when.{k}"));
        }
        for (name, list) in [("apps", &self.apps), ("except_apps", &self.except_apps)] {
            for m in list.iter().flatten() {
                if let Some(k) = m.extra.keys().next() {
                    return Some(format!("when.{name}[].{k}"));
                }
            }
        }
        self.window_title
            .as_ref()
            .and_then(|t| t.extra.keys().next())
            .map(|k| format!("when.window_title.{k}"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppMatcher {
    #[serde(default, skip_serializing_if = "none")]
    pub exe: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub path: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleMatch {
    #[serde(default, skip_serializing_if = "none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "none")]
    pub equals: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A catalog permission a rule can need (§6.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    InputInjection,
    LaunchPrograms,
    Clipboard,
}

impl Permission {
    pub const ALL: [Permission; 3] = [
        Permission::InputInjection,
        Permission::LaunchPrograms,
        Permission::Clipboard,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Permission::InputInjection => "input-injection",
            Permission::LaunchPrograms => "launch-programs",
            Permission::Clipboard => "clipboard",
        }
    }

    pub fn parse(s: &str) -> Option<Permission> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

/// `consent` (§6): one grant per permission.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Consent {
    #[serde(rename = "input-injection", default, skip_serializing_if = "none")]
    pub input_injection: Option<Grant>,
    #[serde(rename = "launch-programs", default, skip_serializing_if = "none")]
    pub launch_programs: Option<Grant>,
    #[serde(default, skip_serializing_if = "none")]
    pub clipboard: Option<Grant>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One recorded grant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub granted_at: String,
    /// `gui` or `cli`; agents never grant consent.
    pub via: String,
    #[serde(default, skip_serializing_if = "none")]
    pub fingerprint: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Grant {
    /// A grant given now on the command line (`--allow`).
    pub fn cli_now(fingerprint: Option<String>) -> Grant {
        Grant {
            granted_at: crate::time::now_rfc3339(),
            via: "cli".into(),
            fingerprint,
            extra: Map::new(),
        }
    }
}

macro_rules! step_struct {
    ($(#[$m:meta])* $name:ident { $($(#[$fm:meta])* $f:ident : $t:ty),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $($(#[$fm])* pub $f: $t,)*
            #[serde(flatten)]
            pub extra: Map<String, Value>,
        }
    };
}

step_struct!(
    /// `remap` (§5.1).
    Remap {
        to: String,
        #[serde(default, skip_serializing_if = "none")]
        tap: Option<String>,
        #[serde(default, skip_serializing_if = "none")]
        tap_ms: Option<u32>,
    }
);
step_struct!(
    /// `block` (§5.2).
    Block {}
);
step_struct!(
    /// `send_keys` (§5.3).
    SendKeys {
        keys: String,
        #[serde(default, skip_serializing_if = "none")]
        step_delay_ms: Option<u32>,
    }
);
step_struct!(
    /// `type_text` (§5.4).
    TypeText {
        text: String,
        #[serde(default, skip_serializing_if = "none")]
        method: Option<TextMethod>,
        #[serde(default, skip_serializing_if = "none")]
        key_delay_ms: Option<u32>,
        /// `text` is a template with the date and time variables and `{cursor}` (Step 69).
        #[serde(default, skip_serializing_if = "none")]
        variables: Option<bool>,
    }
);
step_struct!(
    /// `open_program` (§5.5).
    OpenProgram {
        #[serde(default, skip_serializing_if = "none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "none")]
        app_id: Option<String>,
        #[serde(default, skip_serializing_if = "none")]
        args: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "none")]
        working_dir: Option<String>,
        /// Explicit environment overrides (Step 70).
        #[serde(default, skip_serializing_if = "none")]
        env: Option<Vec<EnvVar>>,
        #[serde(default, skip_serializing_if = "none")]
        window: Option<WindowState>,
        #[serde(default, skip_serializing_if = "none")]
        if_running: Option<IfRunning>,
    }
);

/// One environment override of `open_program` (`{ "name", "value" }`; the schema allows no
/// other fields).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
}
step_struct!(
    /// `open_url` (§5.6).
    OpenUrl { url: String }
);
step_struct!(
    /// `mouse_move` (§5.7).
    MouseMove {
        mode: MoveMode,
        #[serde(default, skip_serializing_if = "none")]
        dx: Option<i32>,
        #[serde(default, skip_serializing_if = "none")]
        dy: Option<i32>,
        #[serde(default, skip_serializing_if = "none")]
        to: Option<Point>,
        #[serde(default, skip_serializing_if = "none")]
        while_held: Option<WhileHeld>,
    }
);
step_struct!(
    /// A point for `mouse_move` and `mouse_click`.
    Point {
        x: i32,
        y: i32,
        #[serde(default, skip_serializing_if = "none")]
        relative_to: Option<RelativeTo>,
    }
);
step_struct!(
    /// Continuous movement while the trigger is held.
    WhileHeld {
        speed: u32,
        #[serde(default, skip_serializing_if = "none")]
        acceleration: Option<u32>,
    }
);
step_struct!(
    /// `mouse_scroll` (§5.8).
    MouseScroll {
        direction: Direction,
        #[serde(default, skip_serializing_if = "none")]
        notches: Option<u32>,
    }
);
step_struct!(
    /// `mouse_click` (§5.9).
    MouseClick {
        button: Button,
        #[serde(default, skip_serializing_if = "none")]
        op: Option<ClickOp>,
        #[serde(default, skip_serializing_if = "none")]
        at: Option<Point>,
    }
);
step_struct!(
    /// `mode` (§5.10).
    ModeStep {
        op: ModeOp,
        #[serde(default, skip_serializing_if = "none")]
        mode: Option<String>,
        #[serde(default, skip_serializing_if = "none")]
        modes: Option<Vec<String>>,
    }
);
step_struct!(
    /// `engine` (§5.11).
    EngineStep { op: EngineOp }
);
step_struct!(
    /// `delay` (§5.12).
    Delay { ms: u32 }
);

macro_rules! snake_enum {
    ($(#[$m:meta])* $name:ident { $($v:ident = $s:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $v,)*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$v => $s,)*
                }
            }

            pub fn parse(s: &str) -> Option<$name> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }
    };
}

snake_enum!(TextMethod { Unicode = "unicode", Paste = "paste" });
snake_enum!(HotstringCase {
    Conform = "conform",
    Insensitive = "insensitive",
    Sensitive = "sensitive",
});
snake_enum!(WindowState { Normal = "normal", Maximized = "maximized", Minimized = "minimized" });
snake_enum!(IfRunning { StartNew = "start_new", Activate = "activate" });
snake_enum!(MoveMode { Relative = "relative", Absolute = "absolute" });
snake_enum!(RelativeTo {
    VirtualScreen = "virtual_screen",
    PrimaryMonitor = "primary_monitor",
    ActiveWindow = "active_window",
});
snake_enum!(Direction { Up = "up", Down = "down", Left = "left", Right = "right" });
snake_enum!(Button { Left = "left", Right = "right", Middle = "middle", X1 = "x1", X2 = "x2" });
snake_enum!(ClickOp { Click = "click", DoubleClick = "double_click", Hold = "hold" });
snake_enum!(ModeOp {
    Set = "set",
    Toggle = "toggle",
    Cycle = "cycle",
    Hold = "hold",
    Previous = "previous",
    Default = "default",
});
snake_enum!(EngineOp {
    Pause = "pause",
    Resume = "resume",
    TogglePause = "toggle_pause",
    Disable = "disable",
});

/// One step of a rule (§5). Serialized with its `type` first.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Remap(Remap),
    Block(Block),
    SendKeys(SendKeys),
    TypeText(TypeText),
    OpenProgram(OpenProgram),
    OpenUrl(OpenUrl),
    MouseMove(MouseMove),
    MouseScroll(MouseScroll),
    MouseClick(MouseClick),
    Mode(ModeStep),
    Engine(EngineStep),
    Delay(Delay),
    /// A step type of a later version: kept as written; the rule is inactive
    /// (`step_unsupported`, §5.13).
    Future {
        kind: String,
        fields: Map<String, Value>,
    },
}

/// The step types of rules document v1 (schema `known_step_types`).
pub const STEP_TYPES: [&str; 12] = [
    "remap",
    "block",
    "send_keys",
    "type_text",
    "open_program",
    "open_url",
    "mouse_move",
    "mouse_scroll",
    "mouse_click",
    "mode",
    "engine",
    "delay",
];

impl Action {
    pub fn kind(&self) -> &str {
        match self {
            Action::Remap(_) => "remap",
            Action::Block(_) => "block",
            Action::SendKeys(_) => "send_keys",
            Action::TypeText(_) => "type_text",
            Action::OpenProgram(_) => "open_program",
            Action::OpenUrl(_) => "open_url",
            Action::MouseMove(_) => "mouse_move",
            Action::MouseScroll(_) => "mouse_scroll",
            Action::MouseClick(_) => "mouse_click",
            Action::Mode(_) => "mode",
            Action::Engine(_) => "engine",
            Action::Delay(_) => "delay",
            Action::Future { kind, .. } => kind,
        }
    }

    /// The first field of a known step type (or of a point inside it) that this version does not
    /// know, as `field` or `to.field`. `None` for future step types (they are unknown as a whole).
    pub fn unknown_field(&self) -> Option<String> {
        let first =
            |m: &Map<String, Value>, prefix: &str| m.keys().next().map(|k| format!("{prefix}{k}"));
        match self {
            Action::Remap(s) => first(&s.extra, ""),
            Action::Block(s) => first(&s.extra, ""),
            Action::SendKeys(s) => first(&s.extra, ""),
            Action::TypeText(s) => first(&s.extra, ""),
            Action::OpenProgram(s) => first(&s.extra, ""),
            Action::OpenUrl(s) => first(&s.extra, ""),
            Action::MouseMove(s) => first(&s.extra, "")
                .or_else(|| s.to.as_ref().and_then(|p| first(&p.extra, "to.")))
                .or_else(|| {
                    s.while_held
                        .as_ref()
                        .and_then(|w| first(&w.extra, "while_held."))
                }),
            Action::MouseScroll(s) => first(&s.extra, ""),
            Action::MouseClick(s) => {
                first(&s.extra, "").or_else(|| s.at.as_ref().and_then(|p| first(&p.extra, "at.")))
            }
            Action::Mode(s) => first(&s.extra, ""),
            Action::Engine(s) => first(&s.extra, ""),
            Action::Delay(s) => first(&s.extra, ""),
            Action::Future { .. } => None,
        }
    }

    /// Whether the step needs `p` (§6.1).
    pub fn needs(&self, p: Permission) -> bool {
        match p {
            Permission::InputInjection => matches!(
                self,
                Action::Remap(_)
                    | Action::SendKeys(_)
                    | Action::TypeText(_)
                    | Action::MouseMove(_)
                    | Action::MouseScroll(_)
                    | Action::MouseClick(_)
            ),
            Permission::LaunchPrograms => {
                matches!(self, Action::OpenProgram(_) | Action::OpenUrl(_))
            }
            Permission::Clipboard => {
                matches!(self, Action::TypeText(t) if t.method == Some(TextMethod::Paste))
            }
        }
    }

    /// Output chords or sequences this step sends (`remap` target and tap, `send_keys` keys).
    pub fn outputs(&self) -> Vec<&str> {
        match self {
            Action::Remap(r) => std::iter::once(r.to.as_str())
                .chain(r.tap.as_deref())
                .collect(),
            Action::SendKeys(s) => vec![s.keys.as_str()],
            _ => Vec::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

impl Serialize for Action {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let fields = match self {
            Action::Remap(x) => serde_json::to_value(x),
            Action::Block(x) => serde_json::to_value(x),
            Action::SendKeys(x) => serde_json::to_value(x),
            Action::TypeText(x) => serde_json::to_value(x),
            Action::OpenProgram(x) => serde_json::to_value(x),
            Action::OpenUrl(x) => serde_json::to_value(x),
            Action::MouseMove(x) => serde_json::to_value(x),
            Action::MouseScroll(x) => serde_json::to_value(x),
            Action::MouseClick(x) => serde_json::to_value(x),
            Action::Mode(x) => serde_json::to_value(x),
            Action::Engine(x) => serde_json::to_value(x),
            Action::Delay(x) => serde_json::to_value(x),
            Action::Future { fields, .. } => Ok(Value::Object(fields.clone())),
        }
        .map_err(serde::ser::Error::custom)?;
        let Value::Object(fields) = fields else {
            return Err(serde::ser::Error::custom("a step is a JSON object"));
        };
        let mut m = s.serialize_map(Some(fields.len() + 1))?;
        m.serialize_entry("type", self.kind())?;
        for (k, v) in &fields {
            if k != "type" {
                m.serialize_entry(k, v)?;
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = Map::<String, Value>::deserialize(d)?;
        let Some(Value::String(kind)) = m.remove("type") else {
            return Err(D::Error::custom("a step needs a string `type`"));
        };
        let rest = Value::Object(m.clone());
        fn de<T: serde::de::DeserializeOwned, E: serde::de::Error>(v: Value) -> Result<T, E> {
            serde_json::from_value(v).map_err(E::custom)
        }
        Ok(match kind.as_str() {
            "remap" => Action::Remap(de(rest)?),
            "block" => Action::Block(de(rest)?),
            "send_keys" => Action::SendKeys(de(rest)?),
            "type_text" => Action::TypeText(de(rest)?),
            "open_program" => Action::OpenProgram(de(rest)?),
            "open_url" => Action::OpenUrl(de(rest)?),
            "mouse_move" => Action::MouseMove(de(rest)?),
            "mouse_scroll" => Action::MouseScroll(de(rest)?),
            "mouse_click" => Action::MouseClick(de(rest)?),
            "mode" => Action::Mode(de(rest)?),
            "engine" => Action::Engine(de(rest)?),
            "delay" => Action::Delay(de(rest)?),
            _ => Action::Future { kind, fields: m },
        })
    }
}

impl Document {
    /// What a missing file means (§2): one empty profile `default` with mode `base`.
    pub fn empty() -> Document {
        Document {
            schema: SCHEMA.into(),
            schema_version: SCHEMA_VERSION,
            hotkey_contract: HOTKEY_CONTRACT,
            revision: Some(0),
            active_profile: "default".into(),
            settings: None,
            exported: None,
            profiles: vec![Profile {
                id: "default".into(),
                title: "Default".into(),
                description: None,
                default_mode: "base".into(),
                modes: vec![Mode {
                    id: "base".into(),
                    title: "Base".into(),
                    color: None,
                    extra: Map::new(),
                }],
                rules: Vec::new(),
                extra: Map::new(),
            }],
            extra: Map::new(),
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.unwrap_or(0)
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn profile_index(&self, id: &str) -> Option<usize> {
        self.profiles.iter().position(|p| p.id == id)
    }

    /// Where a rule is: (profile index, rule index). Rule IDs are unique in the document; with a
    /// (broken) duplicate the first one is found.
    pub fn find_rule(&self, id: &str) -> Option<(usize, usize)> {
        self.profiles.iter().enumerate().find_map(|(pi, p)| {
            p.rules
                .iter()
                .position(|r| r.id() == Some(id))
                .map(|ri| (pi, ri))
        })
    }

    /// Whether any rule of any profile has this ID.
    pub fn has_rule_id(&self, id: &str) -> bool {
        self.find_rule(id).is_some()
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The pretty, stable text written to disk.
    pub fn to_text(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// A random UUID (version 4, RFC 9562 layout) from the OS-seeded hasher keys of the standard
/// library, mixed with the time, the process ID and a counter. Unique enough for IDs that are
/// also checked against every ID in the document; never used for anything secret.
pub fn uuid_v4() -> String {
    use std::hash::BuildHasher;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut b = [0u8; 16];
    for half in 0..2u8 {
        let h = std::collections::hash_map::RandomState::new().hash_one((
            nanos,
            std::process::id(),
            n,
            half,
        ));
        let i = usize::from(half) * 8;
        b[i..i + 8].copy_from_slice(&h.to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A new rule ID `r-<uuid>` (§4) that `taken` does not know yet.
pub fn new_rule_id(mut taken: impl FnMut(&str) -> bool) -> String {
    loop {
        let id = format!("r-{}", uuid_v4());
        if !taken(&id) {
            return id;
        }
    }
}

/// Whether `s` is a valid rule ID (`^[a-z][a-z0-9-]{1,47}$`).
pub fn is_rule_id(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Whether `s` is a valid profile or mode ID (`^[a-z][a-z0-9-]{0,31}$`).
pub fn is_short_id(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn examples() -> Vec<(String, Value)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/ddalgak-rules-v1");
        let mut out = Vec::new();
        for sub in [dir.clone(), dir.join("semantic-invalid")] {
            for e in std::fs::read_dir(&sub).unwrap().flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
                    out.push((p.display().to_string(), v));
                }
            }
        }
        out
    }

    #[test]
    fn every_valid_example_round_trips_through_the_types_without_loss() {
        let all = examples();
        assert!(all.len() >= 20);
        for (name, v) in all {
            let doc: Document =
                serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
            for p in &doc.profiles {
                for r in &p.rules {
                    assert!(
                        matches!(r, RuleEntry::Typed(_)),
                        "{name}: rule {:?} did not fit the types",
                        r.id()
                    );
                }
            }
            assert_eq!(doc.to_value(), v, "{name}: lossless round trip");
        }
    }

    #[test]
    fn unknown_fields_and_future_steps_are_kept_at_every_level() {
        let v = json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "p", "from_the_future": { "a": 1 },
            "settings": { "engine_on_start": "off", "theme": "dark" },
            "profiles": [ { "id": "p", "title": "P", "default_mode": "base", "color": "#fff",
                "modes": [ { "id": "base", "title": "Base", "icon": "star" } ],
                "rules": [
                    { "id": "a", "title": "A", "enabled": true, "only_on_tuesdays": true,
                      "trigger": { "keys": "F13", "double_tap": true },
                      "steps": [ { "type": "teleport", "where": "moon" },
                                 { "type": "engine", "op": "pause", "softly": true } ] },
                    { "id": "b", "title": 7, "enabled": "maybe", "trigger": {}, "steps": [] }
                ] } ]
        });
        let doc: Document = serde_json::from_value(v.clone()).unwrap();
        let p = &doc.profiles[0];
        let a = p.rules[0].rule().unwrap();
        assert_eq!(a.extra["only_on_tuesdays"], true);
        assert_eq!(a.trigger.extra["double_tap"], true);
        assert!(matches!(&a.steps[0], Action::Future { kind, .. } if kind == "teleport"));
        assert_eq!(a.steps[1].unknown_field().as_deref(), Some("softly"));
        assert!(matches!(p.rules[1], RuleEntry::Raw(_)), "kept as written");
        assert_eq!(doc.to_value(), v);
        // The step's type is written first.
        let text = doc.to_text();
        assert!(text.contains("\"type\": \"engine\",\n"), "{text}");
    }

    #[test]
    fn uuids_are_version_4_and_distinct() {
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..2000 {
            let u = uuid_v4();
            assert_eq!(u.len(), 36);
            assert_eq!(&u[14..15], "4");
            assert!(matches!(&u[19..20], "8" | "9" | "a" | "b"), "{u}");
            assert!(seen.insert(u));
        }
        let id = new_rule_id(|_| false);
        assert!(is_rule_id(&id), "{id}");
        assert!(id.starts_with("r-"));
        let mut calls = 0;
        let id2 = new_rule_id(|_| {
            calls += 1;
            calls < 3
        });
        assert!(is_rule_id(&id2));
    }

    #[test]
    fn permissions_follow_the_step_types() {
        let doc: Document = serde_json::from_value(json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "p",
            "profiles": [ { "id": "p", "title": "P", "default_mode": "base",
                "modes": [ { "id": "base", "title": "Base" } ],
                "rules": [ { "id": "a", "title": "A", "enabled": false, "trigger": { "keys": "F13" },
                    "steps": [ { "type": "type_text", "text": "x", "method": "paste" },
                               { "type": "open_url", "url": "https://example.com/" } ] } ] } ]
        }))
        .unwrap();
        let mut r = doc.profiles[0].rules[0].rule().unwrap().clone();
        assert_eq!(
            r.permissions_needed(),
            [
                Permission::InputInjection,
                Permission::LaunchPrograms,
                Permission::Clipboard
            ]
        );
        r.set_grant(Permission::Clipboard, Some(Grant::cli_now(None)));
        assert!(r.grant(Permission::Clipboard).is_some());
        r.set_grant(Permission::Clipboard, None);
        assert!(r.consent.is_none(), "an empty consent object is dropped");
    }
}
