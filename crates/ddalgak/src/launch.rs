//! Program and website launches: `open_program` and `open_url` (docs/DDALGAK_RULE_MODEL.md §5.5,
//! §5.6, §6.2; ADR 0037; `DDALGAK-FR-013`, `DDALGAK-FR-014`, `DDALGAK-SEC-006`,
//! `DDALGAK-SEC-007`, `DDALGAK-SEC-008`, `DDALGAK-SEC-011`, `DDALGAK-SEC-013`, `DDALGAK-SEC-014`).
//!
//! **What runs.** A rule whose steps are all `open_program` and `open_url` steps compiles into a
//! [`Plan`]. When it fires, the worker hands the plan to a short-lived launch thread (so a slow
//! shell hand-off never delays remaps or typing), which carries out the targets in order:
//!
//! | Target | How (Windows, `win::launch`) | Outcomes |
//! | --- | --- | --- |
//! | a program by path | the path is expanded (known-folder tokens only), checked again (exists, a file, `.exe`, not a shell host even behind a link) and started with `CreateProcessW`: the resolved path as the application name (no search, no shell), a command line Ddalgak quotes per argument ([`quote_arg`]), the working folder (default: the program's folder), the window state, the current user's token, never elevated | `program_not_found`, `program_requires_elevation` (`ERROR_ELEVATION_REQUIRED`: Ddalgak never asks for elevation), `launch_failed` |
//! | a packaged app (`app_id`) | the shell's activation of `shell:AppsFolder\<AppUserModelID>` (`ShellExecuteExW`, verb `open`, no error UI) | `program_not_found` (not installed), `launch_failed` |
//! | a website | `ShellExecuteExW` with the verb `open` on the checked `http`/`https` URL: the user's default browser through its standard association; Ddalgak makes no connection itself | `browser_unavailable` (no association), `launch_failed` |
//!
//! `if_running: activate` first looks for a visible top-level window of a process started from
//! the same file and brings it to the front instead (Windows' foreground lock may only flash its
//! taskbar button). Activity entries carry the rule ID and the outcome code only.
//!
//! **Safety.** Nothing here runs a shell string: arguments are a list, quoted by Ddalgak; command
//! interpreters and script hosts are refused (§5.5); URLs are `http`/`https` only, without
//! credentials, and every other scheme (`file:`, `javascript:`, `ms-settings:`, custom protocols)
//! is refused, by the schema and again here. A target is never searched for: a missing or moved
//! program ends the run with `program_not_found`. Environment overrides exist only as an explicit
//! list in the step (`env`), covered by the launch fingerprint like every other input. Paths,
//! arguments, URLs and environment values never appear in activity, status, logs or validation
//! messages; [`redact_url`] and [`program_name`] give what may be shown. Loading, checking or
//! importing rules never launches anything: only a fired trigger reaches [`execute`].
//!
//! **Repeated triggers.** One run per rule at a time (`busy`) and at most one start per rule per
//! [`DEBOUNCE`] (`rate_limited`), so a double press or a held key never opens a program twice
//! ([`Gate`]).
//!
//! The editor's data models for Step 74 are here too: the program picker ([`ProgramDraft`]: path
//! normalization, known-folder tokens, quoting preview, file checks, an optional icon
//! ([`IconPreview`], extracted on Windows by `win::launch::icon_preview`)) and the website field
//! ([`UrlDraft`]: normalization, the http/https allowlist, the preview).

use crate::remap::Field;
use crate::text::FieldProblem;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Command interpreters and script hosts: never programs for `open_program` (§5.5; Step 79's
/// separately consented command action owns them). The schema refuses them by name; the runtime
/// refuses them again after following links.
pub const SHELL_HOSTS: [&str; 12] = [
    "cmd",
    "powershell",
    "powershell_ise",
    "pwsh",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "wsl",
    "bash",
    "conhost",
];

/// The known-folder tokens a path may start with, as written canonically, and the environment
/// variable each one reads (§5.5). No other variables are expanded.
pub const KNOWN_FOLDERS: [(&str, &str); 6] = [
    ("ProgramFiles", "ProgramFiles"),
    ("ProgramFiles(x86)", "ProgramFiles(x86)"),
    ("LocalAppData", "LOCALAPPDATA"),
    ("AppData", "APPDATA"),
    ("UserProfile", "USERPROFILE"),
    ("SystemRoot", "SystemRoot"),
];

/// Longest path (characters), as the schema.
pub const MAX_PATH: usize = 260;
/// Most arguments and the longest argument.
pub const MAX_ARGS: usize = 32;
pub const MAX_ARG: usize = 1024;
/// Longest URL.
pub const MAX_URL: usize = 2048;
/// Most environment overrides and the longest value.
pub const MAX_ENV: usize = 32;
pub const MAX_ENV_VALUE: usize = 4096;
/// `CreateProcessW` takes at most 32 767 UTF-16 units including the terminating NUL.
pub const MAX_COMMAND_LINE: usize = 32_766;
/// A launch rule starts at most once per this time; a trigger within it is `rate_limited`.
pub const DEBOUNCE: Duration = Duration::from_millis(1000);

/// How a program's first window appears (`window`, §5.5). `Hidden` is not a rule value: only the
/// debug test host uses it, for its own test program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Show {
    Normal,
    Maximized,
    Minimized,
    Hidden,
}

impl Show {
    /// A rule's `window` value (`normal` when absent).
    pub fn parse(s: Option<&str>) -> Option<Show> {
        match s {
            None | Some("normal") => Some(Show::Normal),
            Some("maximized") => Some(Show::Maximized),
            Some("minimized") => Some(Show::Minimized),
            Some(_) => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Show::Normal => "normal",
            Show::Maximized => "maximized",
            Show::Minimized => "minimized",
            Show::Hidden => "hidden",
        }
    }
}

/// A compiled `open_program` step with a path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProgramSpec {
    /// As written: a drive path or a known-folder token path.
    pub path: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub show: Show,
    /// `if_running: activate`.
    pub activate: bool,
    /// Explicit environment overrides (`env`), in the order written.
    pub env: Vec<(String, String)>,
}

/// One target of a launch rule.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Program(ProgramSpec),
    /// A packaged app by AppUserModelID.
    App {
        app_id: String,
        show: Show,
    },
    /// A checked `http`/`https` URL.
    Url(String),
}

/// A launch rule's targets in order (`on_error: stop` ends the run at the first failure).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Plan {
    pub targets: Vec<Target>,
    pub stop_on_error: bool,
}

// ---------------------------------------------------------------------------------------------
// Paths

/// What is wrong with a program or folder path (the path itself is never quoted).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathProblem {
    Empty,
    TooLong,
    /// Relative, a network path, or another variable than the known-folder tokens.
    NotAbsolute,
    /// A character Windows paths cannot hold (or a control character), at this position.
    BadCharacter {
        at: usize,
    },
    /// A `:` after the drive (an alternate data stream or a device name), at this position.
    Colon {
        at: usize,
    },
    NotExe,
    ShellHost,
}

impl PathProblem {
    pub fn message(self) -> String {
        match self {
            PathProblem::Empty => "choose a program: an absolute path to an .exe file".into(),
            PathProblem::TooLong => format!("the path is longer than {MAX_PATH} characters"),
            PathProblem::NotAbsolute => "use an absolute path on a drive (C:\\…) or under %ProgramFiles%, %ProgramFiles(x86)%, %LocalAppData%, %AppData%, %UserProfile% or %SystemRoot%; relative paths, network paths and other variables are never searched".into(),
            PathProblem::BadCharacter { at } => format!(
                "character {at}: < > \" | ? * and control characters cannot be part of a path"
            ),
            PathProblem::Colon { at } => {
                format!("character {at}: `:` is allowed only after the drive letter")
            }
            PathProblem::NotExe => {
                "only programs (.exe) can be opened; documents, shortcuts and scripts cannot".into()
            }
            PathProblem::ShellHost => "command interpreters and script hosts (cmd, PowerShell, wscript, mshta, …) are not programs for this action; a separately consented command action is planned (Step 79)".into(),
        }
    }
}

/// The known-folder token at the start of `raw` (`%LocalAppData%`), case-insensitive: its
/// environment variable and the length of the token in bytes.
fn token_prefix(raw: &str) -> Option<(&'static str, usize)> {
    let rest = raw.strip_prefix('%')?;
    let end = rest.find('%')?;
    let name = &rest[..end];
    KNOWN_FOLDERS
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(name))
        .map(|(_, var)| (*var, end + 2))
}

/// The file name of a path as written (`notepad.exe`): what previews and titles may show.
pub fn program_name(raw: &str) -> &str {
    raw.rsplit(['\\', '/']).next().unwrap_or(raw)
}

/// Whether a file name (any case, with or without `.exe`) is a command interpreter or script host.
pub fn is_shell_host(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    SHELL_HOSTS.contains(&stem)
}

/// Checks a path's form (§5.5): a drive path or a known-folder token path, no characters Windows
/// paths cannot hold, no `:` after the drive; for a program also `.exe` and not a shell host.
/// The same rules as the schema's `program_path`/`folder_path`, plus the `:` rule (the schema
/// accepts it; the runtime does not).
pub fn path_problem(raw: &str, program: bool) -> Option<PathProblem> {
    if raw.is_empty() {
        return Some(PathProblem::Empty);
    }
    if raw.chars().count() > MAX_PATH {
        return Some(PathProblem::TooLong);
    }
    let b = raw.as_bytes();
    let start = if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
        3
    } else if let Some((_, len)) = token_prefix(raw) {
        match raw[len..].chars().next() {
            Some('\\') => len + 1,
            None if !program => len,
            _ => return Some(PathProblem::NotAbsolute),
        }
    } else {
        return Some(PathProblem::NotAbsolute);
    };
    let skip = raw[..start].chars().count();
    for (i, c) in raw.chars().enumerate().skip(skip) {
        if matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || c.is_control() {
            return Some(PathProblem::BadCharacter { at: i });
        }
        if c == ':' {
            return Some(PathProblem::Colon { at: i });
        }
    }
    if program {
        let name = program_name(raw);
        let lower = name.to_ascii_lowercase();
        if lower.len() <= 4 || !lower.ends_with(".exe") || raw.len() <= start {
            return Some(PathProblem::NotExe);
        }
        if is_shell_host(name) {
            return Some(PathProblem::ShellHost);
        }
    }
    None
}

/// Expands a known-folder token (§5.5) with `var` (the process environment in the app). `None`
/// when the token is unknown or its variable is not set. Drive paths are returned unchanged.
pub fn expand(raw: &str, var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    if !raw.starts_with('%') {
        return Some(PathBuf::from(raw));
    }
    let (name, len) = token_prefix(raw)?;
    let base = var(name).filter(|v| !v.is_empty())?;
    let mut p = PathBuf::from(base);
    let tail = raw[len..].trim_start_matches('\\');
    if !tail.is_empty() {
        p.push(tail);
    }
    Some(p)
}

/// The process environment (what the app passes to [`expand`]).
pub fn process_var(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Rewrites an absolute path under a known folder with its token, the most specific folder first
/// (`C:\Users\kim\AppData\Local\Programs\x.exe` → `%LocalAppData%\Programs\x.exe`): rules stay
/// valid on other computers and do not carry the user name. `None` when no folder contains it.
pub fn tokenize(path: &str, var: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for (token, name) in KNOWN_FOLDERS {
        let Some(base) = var(name).filter(|v| !v.is_empty()) else {
            continue;
        };
        let base = base.to_string_lossy().trim_end_matches('\\').to_string();
        let Some(head) = path.get(..base.len()) else {
            continue;
        };
        let rest = &path[base.len()..];
        if head.eq_ignore_ascii_case(&base)
            && rest.starts_with('\\')
            && best.as_ref().is_none_or(|(l, _)| base.len() > *l)
        {
            best = Some((base.len(), format!("%{token}%{rest}")));
        }
    }
    best.map(|(_, p)| p)
}

/// What a typed or pasted path becomes before it is checked: surrounding spaces and one pair of
/// quotes removed (Explorer's "Copy as path" adds them), `/` written as `\`. Nothing is resolved.
pub fn normalize_path(input: &str) -> String {
    let t = input.trim();
    let t = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(t);
    t.trim().replace('/', "\\")
}

/// `\\?\C:\…` (what `canonicalize` returns on Windows) as `C:\…`.
fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

/// A program resolved and checked just before it starts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolved {
    /// The expanded path (what the user consented to); `CreateProcessW` gets exactly this.
    pub program: PathBuf,
    /// The file it really is (links followed), for the shell-host check and window matching.
    pub real: PathBuf,
    pub working_dir: PathBuf,
}

/// Expands and checks a program right before it starts: the form again (defense in depth), the
/// file exists and is a file (`program_not_found` otherwise: it is never searched for), the real
/// file behind links is an `.exe` and not a shell host (`launch_failed`), and the working folder
/// exists (`program_not_found`).
pub fn resolve(
    spec: &ProgramSpec,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Resolved, &'static str> {
    if path_problem(&spec.path, true).is_some() {
        return Err("launch_failed");
    }
    let program = expand(&spec.path, var).ok_or("program_not_found")?;
    let meta = std::fs::metadata(&program).map_err(|_| "program_not_found")?;
    if !meta.is_file() {
        return Err("program_not_found");
    }
    let real = std::fs::canonicalize(&program)
        .map(|p| plain(&p))
        .map_err(|_| "program_not_found")?;
    let real_name = real
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if !real_name.to_ascii_lowercase().ends_with(".exe") || is_shell_host(&real_name) {
        return Err("launch_failed");
    }
    let working_dir = match &spec.working_dir {
        Some(w) => {
            if path_problem(w, false).is_some() {
                return Err("launch_failed");
            }
            expand(w, var).ok_or("program_not_found")?
        }
        None => program
            .parent()
            .map(Path::to_path_buf)
            .ok_or("program_not_found")?,
    };
    if !working_dir.is_dir() {
        return Err("program_not_found");
    }
    Ok(Resolved {
        program,
        real,
        working_dir,
    })
}

// ---------------------------------------------------------------------------------------------
// Command lines

/// Appends one argument quoted by the Windows rules (`CommandLineToArgvW` and the C runtime): an
/// argument without spaces, tabs or quotes is written as is; otherwise it is put in quotes, a
/// quote becomes `\"` and the backslashes before a quote or the closing quote are doubled.
pub fn quote_arg(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// The command line `CreateProcessW` gets: the program in quotes (a path never holds a quote),
/// then each argument quoted on its own. The program itself is also passed as the application
/// name, so the line is never searched or split at a space of the path.
pub fn command_line(program: &Path, args: &[String]) -> String {
    let mut s = format!("\"{}\"", program.display());
    for a in args {
        s.push(' ');
        quote_arg(a, &mut s);
    }
    s
}

/// What is wrong with an argument list (the arguments are never quoted).
pub fn args_problem(args: &[String]) -> Option<String> {
    if args.len() > MAX_ARGS {
        return Some(format!("at most {MAX_ARGS} arguments"));
    }
    for (i, a) in args.iter().enumerate() {
        if a.chars().count() > MAX_ARG {
            return Some(format!("argument {i} is longer than {MAX_ARG} characters"));
        }
        if let Some(at) = a.chars().position(char::is_control) {
            return Some(format!(
                "argument {i}, character {at}: control characters (new lines, tabs) cannot be passed"
            ));
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Environment overrides

/// An environment variable name for `env`: a letter or `_`, then letters, digits and `_` (at
/// most 64).
pub fn env_name_ok(n: &str) -> bool {
    let mut c = n.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && n.len() <= 64
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

/// What is wrong with the overrides (names may be shown; values are never quoted).
pub fn env_problem(env: &[(String, String)]) -> Option<String> {
    if env.len() > MAX_ENV {
        return Some(format!("at most {MAX_ENV} environment overrides"));
    }
    let mut seen = HashSet::new();
    for (i, (n, v)) in env.iter().enumerate() {
        if !env_name_ok(n) {
            return Some(format!(
                "override {i}: a name is a letter or `_` followed by letters, digits and `_` (at most 64)"
            ));
        }
        if !seen.insert(n.to_ascii_uppercase()) {
            return Some(format!("override {i}: `{n}` is given twice"));
        }
        if v.chars().count() > MAX_ENV_VALUE {
            return Some(format!(
                "override {i} (`{n}`): the value is longer than {MAX_ENV_VALUE} characters"
            ));
        }
        if let Some(at) = v.chars().position(char::is_control) {
            return Some(format!(
                "override {i} (`{n}`), character {at}: control characters are not allowed"
            ));
        }
    }
    None
}

/// The environment a program with overrides gets: Ddalgak's own environment with each override
/// replacing a variable of the same name (names compare case-insensitively, as on Windows), sorted
/// by name as Windows expects an environment block.
pub fn merge_env(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    overrides: &[(String, String)],
) -> Vec<(OsString, OsString)> {
    let mut out: Vec<(OsString, OsString)> = base
        .into_iter()
        .filter(|(n, _)| {
            let n = n.to_string_lossy();
            !overrides.iter().any(|(o, _)| o.eq_ignore_ascii_case(&n))
        })
        .collect();
    out.extend(
        overrides
            .iter()
            .map(|(n, v)| (OsString::from(n), OsString::from(v))),
    );
    out.sort_by_key(|(n, _)| n.to_string_lossy().to_uppercase());
    out
}

// ---------------------------------------------------------------------------------------------
// URLs

/// What is wrong with a URL. The URL itself is never quoted; a scheme name may be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum UrlProblem {
    Empty,
    TooLong,
    /// A control character at this position.
    Control {
        at: usize,
    },
    /// Whitespace at this position (stored URLs only; the editor encodes spaces).
    Space {
        at: usize,
    },
    /// `"`, `<`, `>` or `\` at this position (stored URLs only; the editor encodes them).
    Character {
        at: usize,
    },
    /// A scheme that can run code, open local files or change settings (`file`, `javascript`,
    /// `ms-settings`, …).
    Dangerous(String),
    /// Any other scheme than `http` and `https` (custom protocols such as `mailto` or `zoommtg`).
    Scheme(String),
    /// A user name or password in the URL.
    Credentials,
    Host,
    Port,
    /// Not `http://…` or `https://…` in the form the rules file stores.
    Malformed,
}

impl UrlProblem {
    pub fn message(&self) -> String {
        match self {
            UrlProblem::Empty => "enter a website address".into(),
            UrlProblem::TooLong => format!("the address is longer than {MAX_URL} characters"),
            UrlProblem::Control { at } => {
                format!("character {at}: control characters cannot be part of an address")
            }
            UrlProblem::Space { at } => format!("character {at}: spaces must be written as %20"),
            UrlProblem::Character { at } => {
                format!("character {at}: \" < > and \\ must be percent-encoded")
            }
            UrlProblem::Dangerous(s) => format!(
                "`{s}:` links can run code, open local files or change system settings; only http and https websites can be opened"
            ),
            UrlProblem::Scheme(s) => format!(
                "`{s}:` links open other programs; only http and https websites can be opened (other protocols are refused in rules v1)"
            ),
            UrlProblem::Credentials => "a user name or password in the address is not allowed; sign in in the browser instead".into(),
            UrlProblem::Host => "the address needs a host name such as example.com (letters, digits, dots and hyphens; IP version 6 addresses are not supported)".into(),
            UrlProblem::Port => "the port must be a number from 1 to 65535".into(),
            UrlProblem::Malformed => {
                "write the address as http://host/… or https://host/…".into()
            }
        }
    }
}

/// Schemes that are refused with the stronger message ([`UrlProblem::Dangerous`]); every other
/// scheme but `http` and `https` is refused too.
pub fn is_dangerous_scheme(scheme: &str) -> bool {
    let s = scheme.to_ascii_lowercase();
    s.len() == 1 // a drive letter: `C:\…` is a local path
        || s.starts_with("ms-")
        || [
            "file",
            "javascript",
            "vbscript",
            "jscript",
            "data",
            "about",
            "blob",
            "shell",
            "search-ms",
            "search",
            "res",
            "mk",
            "its",
            "hcp",
            "help",
            "jar",
            "mhtml",
            "view-source",
            "microsoft-edge",
            "cdl",
            "vbefile",
        ]
        .contains(&s.as_str())
}

/// The scheme at the start of `s` (`https` of `https://…`, `javascript` of `javascript:…`) and
/// what follows the colon, if `s` starts with a scheme.
fn scheme_of(s: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = s.split_once(':')?;
    let mut c = scheme.chars();
    (matches!(c.next(), Some(f) if f.is_ascii_alphabetic())
        && c.all(|x| x.is_ascii_alphanumeric() || matches!(x, '+' | '-' | '.')))
    .then_some((scheme, rest))
}

fn scheme_problem(scheme: &str) -> UrlProblem {
    let s = scheme.to_ascii_lowercase();
    if is_dangerous_scheme(&s) {
        UrlProblem::Dangerous(if s.len() == 1 { "file".into() } else { s })
    } else {
        UrlProblem::Scheme(s)
    }
}

/// A checked URL's parts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UrlParts<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    pub port: Option<u16>,
    /// Path, query and fragment (may be empty).
    pub rest: &'a str,
}

fn host_ok(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && host.chars().any(|c| c.is_ascii_alphanumeric())
        && !host.starts_with(['.', '-'])
        && host
            .trim_end_matches('.')
            .split('.')
            .all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'))
}

fn port_ok(p: &str) -> Option<u16> {
    if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    p.parse::<u16>().ok().filter(|n| *n > 0)
}

/// Checks a URL as the rules file stores it (§5.6): `http://` or `https://` in lower case, a host
/// of letters, digits, dots and hyphens, an optional port 1–65535, then nothing or a path, query
/// or fragment; no user name or password, no whitespace or control characters, no `"`, `<`, `>`
/// or `\` (which could break a browser's command line), at most 2048 characters. Stricter than
/// the schema's pattern; the editor's [`normalize_url`] always produces a URL that passes.
pub fn check_url(url: &str) -> Result<UrlParts<'_>, UrlProblem> {
    if url.is_empty() {
        return Err(UrlProblem::Empty);
    }
    if url.chars().count() > MAX_URL {
        return Err(UrlProblem::TooLong);
    }
    for (i, c) in url.chars().enumerate() {
        if c.is_control() {
            return Err(UrlProblem::Control { at: i });
        }
        if c.is_whitespace() {
            return Err(UrlProblem::Space { at: i });
        }
        if matches!(c, '"' | '<' | '>' | '\\') {
            return Err(UrlProblem::Character { at: i });
        }
    }
    let Some((scheme, after)) = scheme_of(url) else {
        return Err(UrlProblem::Malformed);
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(scheme_problem(scheme));
    }
    let Some(body) = after.strip_prefix("//") else {
        return Err(UrlProblem::Malformed);
    };
    if scheme != "http" && scheme != "https" {
        return Err(UrlProblem::Malformed);
    }
    let end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let (authority, rest) = body.split_at(end);
    if authority.contains('@') {
        return Err(UrlProblem::Credentials);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(port_ok(p).ok_or(UrlProblem::Port)?)),
        None => (authority, None),
    };
    if !host_ok(host) {
        return Err(UrlProblem::Host);
    }
    Ok(UrlParts {
        scheme,
        host,
        port,
        rest,
    })
}

/// What a typed or pasted address becomes before it is stored (GUI_JOURNEYS J4; the CLI does the
/// same): trimmed; `https://` added when no scheme is given (`github.com`, `localhost:3000`); the
/// scheme and host in lower case; a non-ASCII host label converted to its `xn--` form (Punycode;
/// full IDNA mapping is not applied); `/` added when there is no path; spaces, quotes, `<`, `>`,
/// `\`, `^`, `` ` ``, `{`, `}`, `|`, non-ASCII characters and a `%` that starts no escape
/// percent-encoded in the path, query and fragment. Every scheme other than `http` and `https`
/// is refused, and so is a user name or password. The result passes [`check_url`].
pub fn normalize_url(input: &str) -> Result<String, UrlProblem> {
    let s = input.trim();
    if s.is_empty() {
        return Err(UrlProblem::Empty);
    }
    let (scheme, body) = match scheme_of(s) {
        Some((sc, rest)) if rest.starts_with("//") => {
            if !sc.eq_ignore_ascii_case("http") && !sc.eq_ignore_ascii_case("https") {
                return Err(scheme_problem(sc));
            }
            (sc.to_ascii_lowercase(), &rest[2..])
        }
        // `localhost:3000/x`, `example.com:8080`: a host and a port, not a scheme.
        Some((_, rest)) if rest.starts_with(|c: char| c.is_ascii_digit()) => ("https".into(), s),
        Some((sc, _)) => {
            if sc.eq_ignore_ascii_case("http") || sc.eq_ignore_ascii_case("https") {
                return Err(UrlProblem::Malformed);
            }
            return Err(scheme_problem(sc));
        }
        None => ("https".into(), s.strip_prefix("//").unwrap_or(s)),
    };
    let end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let (authority, rest) = body.split_at(end);
    if authority.contains('@') {
        return Err(UrlProblem::Credentials);
    }
    if authority.starts_with('[') {
        return Err(UrlProblem::Host);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(port_ok(p).ok_or(UrlProblem::Port)?)),
        None => (authority, None),
    };
    let host = host.replace(['\u{3002}', '\u{ff0e}', '\u{ff61}'], ".");
    let mut labels = Vec::new();
    for label in host.split('.') {
        let lower = label.to_lowercase();
        if lower.is_ascii() {
            labels.push(lower);
        } else {
            if lower.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(UrlProblem::Host);
            }
            labels.push(format!("xn--{}", punycode(&lower).ok_or(UrlProblem::Host)?));
        }
    }
    let host = labels.join(".");
    if !host_ok(&host) {
        return Err(UrlProblem::Host);
    }
    let mut out = format!("{scheme}://{host}");
    if let Some(p) = port {
        out.push_str(&format!(":{p}"));
    }
    if !rest.starts_with('/') {
        out.push('/');
    }
    // `rest` is a suffix of the input: positions are reported in the (trimmed) input.
    let offset = s.chars().count() - rest.chars().count();
    let chars: Vec<char> = rest.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let hex = |k: usize| chars.get(k).is_some_and(char::is_ascii_hexdigit);
        match c {
            '%' if hex(i + 1) && hex(i + 2) => out.push('%'),
            ' ' | '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '}' | '|' | '%' => {
                out.push_str(&format!("%{:02X}", u32::from(c)));
            }
            c if c.is_control() => {
                return Err(UrlProblem::Control { at: offset + i });
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    check_url(&out)?;
    Ok(out)
}

/// RFC 3492 Punycode of one lower-case label (without the `xn--` prefix).
pub fn punycode(label: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    fn adapt(delta: u32, points: u32, first: bool) -> u32 {
        let mut delta = if first { delta / 700 } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + 38)
    }
    fn digit(d: u32) -> char {
        char::from(u8::try_from(if d < 26 { d + 97 } else { d - 26 + 48 }).unwrap_or(b'?'))
    }
    let input: Vec<u32> = label.chars().map(u32::from).collect();
    let mut out: String = label.chars().filter(char::is_ascii).collect();
    let basic = u32::try_from(out.len()).ok()?;
    let total = u32::try_from(input.len()).ok()?;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut h) = (128u32, 0u32, 72u32, basic);
    while h < total {
        let m = input.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &input {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == basic);
                delta = 0;
                h += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n = n.checked_add(1)?;
    }
    Some(out)
}

/// A URL as it may be shown or logged: the scheme and host (and port); a user name or password is
/// dropped and any path, query or fragment becomes `/…` (they can carry tokens; §5.6). Anything
/// that is not `scheme://…` shows only its scheme.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = scheme_of(url.trim()) else {
        return "(not a web address)".into();
    };
    let scheme = scheme.to_ascii_lowercase();
    let Some(body) = rest.strip_prefix("//") else {
        return format!("{scheme}:…");
    };
    let end = body.find(['/', '?', '#']).unwrap_or(body.len());
    let authority = &body[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let tail = if end < body.len() { "/…" } else { "" };
    format!("{scheme}://{host}{tail}")
}

/// The playground preview of an `open_url` step (§5.6, `preview_open_url`): scheme and host only.
pub fn url_preview(url: &str) -> Option<Value> {
    check_url(url)
        .ok()
        .map(|p| json!({ "scheme": p.scheme, "host": p.host }))
}

/// Whether an AppUserModelID has the schema's form (`PackageFamilyName!AppId`).
pub fn app_id_ok(id: &str) -> bool {
    let Some((family, app)) = id.split_once('!') else {
        return false;
    };
    let ok = |s: &str, min: usize, max: usize| {
        (min..=max).contains(&s.len())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    ok(family, 3, 200) && ok(app, 1, 64)
}

// ---------------------------------------------------------------------------------------------
// Compiling steps

/// The value of a step's `env` (a list of `{name, value}`), in order.
fn env_of(s: &Value) -> Result<Vec<(String, String)>, String> {
    let Some(list) = s.get("env") else {
        return Ok(Vec::new());
    };
    let list = list
        .as_array()
        .ok_or("`env` must be a list of {name, value}")?;
    list.iter()
        .enumerate()
        .map(|(i, e)| {
            match (
                e.get("name").and_then(Value::as_str),
                e.get("value").and_then(Value::as_str),
            ) {
                (Some(n), Some(v)) => Ok((n.to_string(), v.to_string())),
                _ => Err(format!("env[{i}] needs a text `name` and `value`")),
            }
        })
        .collect()
}

/// Compiles one `open_program` or `open_url` step. The checks repeat the schema's and add the
/// runtime's (the `:` rule, unique override names, a command line Windows accepts, a URL a
/// browser's command line cannot misread); a message never quotes a path, argument, URL or value.
pub fn compile_step(s: &Value) -> Result<Target, String> {
    let show = Show::parse(s.get("window").and_then(Value::as_str))
        .ok_or("`window` is normal, maximized or minimized")?;
    match s.get("type").and_then(Value::as_str) {
        Some("open_url") => {
            let url = s
                .get("url")
                .and_then(Value::as_str)
                .ok_or("an `open_url` step needs `url`")?;
            check_url(url).map_err(|p| format!("url: {}", p.message()))?;
            Ok(Target::Url(url.to_string()))
        }
        Some("open_program") => {
            let args: Vec<String> = match s.get("args") {
                None => Vec::new(),
                Some(a) => a
                    .as_array()
                    .and_then(|l| {
                        l.iter()
                            .map(|x| x.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or("`args` must be a list of text arguments")?,
            };
            let env = env_of(s)?;
            if let Some(id) = s.get("app_id") {
                let id = id
                    .as_str()
                    .filter(|i| app_id_ok(i))
                    .ok_or("`app_id` is PackageFamilyName!AppId")?;
                if s.get("path").is_some()
                    || !args.is_empty()
                    || s.get("working_dir").is_some()
                    || !env.is_empty()
                {
                    return Err(
                        "a packaged app takes no path, arguments, working folder or environment"
                            .into(),
                    );
                }
                return Ok(Target::App {
                    app_id: id.to_string(),
                    show,
                });
            }
            let path = s
                .get("path")
                .and_then(Value::as_str)
                .ok_or("an `open_program` step needs `path` or `app_id`")?;
            if let Some(p) = path_problem(path, true) {
                return Err(format!("path: {}", p.message()));
            }
            let working_dir = match s.get("working_dir") {
                None => None,
                Some(w) => {
                    let w = w.as_str().ok_or("`working_dir` must be text")?;
                    if let Some(p) = path_problem(w, false) {
                        return Err(format!("working_dir: {}", p.message()));
                    }
                    Some(w.to_string())
                }
            };
            if let Some(p) = args_problem(&args) {
                return Err(format!("args: {p}"));
            }
            if let Some(p) = env_problem(&env) {
                return Err(format!("env: {p}"));
            }
            if command_line(Path::new(path), &args).encode_utf16().count() > MAX_COMMAND_LINE {
                return Err(format!(
                    "args: the quoted command line exceeds Windows' limit of {MAX_COMMAND_LINE} characters"
                ));
            }
            let activate = match s.get("if_running").and_then(Value::as_str) {
                None | Some("start_new") => false,
                Some("activate") => true,
                Some(_) => return Err("`if_running` is start_new or activate".into()),
            };
            Ok(Target::Program(ProgramSpec {
                path: path.to_string(),
                args,
                working_dir,
                show,
                activate,
                env,
            }))
        }
        _ => Err("not a launch step".into()),
    }
}

// ---------------------------------------------------------------------------------------------
// The platform

/// Why the platform could not start something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LaunchError {
    /// The file, folder or app does not exist (any more).
    NotFound,
    /// The program's manifest asks for administrator rights (`ERROR_ELEVATION_REQUIRED`).
    ElevationRequired,
    /// No program is registered for the target (no default browser, an app not installed).
    NoAssociation,
    AccessDenied,
    /// Not a program Windows can run.
    BadFormat,
    /// Refused before reaching the OS (the debug test host's limits).
    Refused,
    /// Another Windows error code.
    Os(u32),
}

impl LaunchError {
    /// Maps a Windows error code (`GetLastError`; for the shell also the `SE_ERR_*` values below
    /// 32) to a launch error.
    pub fn from_os(code: u32) -> LaunchError {
        match code {
            // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_DIRECTORY (bad working folder),
            // ERROR_INVALID_DRIVE; SE_ERR_FNF and SE_ERR_PNF are the same numbers.
            2 | 3 | 15 | 267 => LaunchError::NotFound,
            // ERROR_ELEVATION_REQUIRED.
            740 => LaunchError::ElevationRequired,
            // ERROR_NO_ASSOCIATION, SE_ERR_NOASSOC, SE_ERR_ASSOCINCOMPLETE.
            1155 | 31 | 27 => LaunchError::NoAssociation,
            // ERROR_ACCESS_DENIED, SE_ERR_ACCESSDENIED, SE_ERR_SHARE.
            5 | 26 => LaunchError::AccessDenied,
            // ERROR_BAD_FORMAT, ERROR_BAD_EXE_FORMAT, ERROR_EXE_MACHINE_TYPE_MISMATCH,
            // ERROR_INVALID_EXE_SIGNATURE, ERROR_EXE_MARKED_INVALID.
            11 | 193 | 216 | 191 | 192 => LaunchError::BadFormat,
            c => LaunchError::Os(c),
        }
    }

    /// The outcome of a program or packaged app that failed this way (§12).
    pub fn program_outcome(self) -> &'static str {
        match self {
            LaunchError::NotFound => "program_not_found",
            LaunchError::ElevationRequired => "program_requires_elevation",
            _ => "launch_failed",
        }
    }

    /// The outcome of a website that failed this way.
    pub fn url_outcome(self) -> &'static str {
        match self {
            LaunchError::NoAssociation | LaunchError::NotFound => "browser_unavailable",
            LaunchError::ElevationRequired => "program_requires_elevation",
            _ => "launch_failed",
        }
    }
}

/// A program start for `CreateProcessW`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpawnRequest {
    /// The application name: exactly this file runs, nothing is searched.
    pub program: PathBuf,
    pub command_line: String,
    pub working_dir: PathBuf,
    pub show: Show,
    /// Overrides on top of Ddalgak's environment (empty: the environment is inherited).
    pub env: Vec<(String, String)>,
}

/// A shell hand-off (`ShellExecuteExW`): always the verb `open`, a URL or a
/// `shell:AppsFolder\…` item, no parameters, no error UI.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShellRequest {
    pub verb: &'static str,
    pub target: String,
    pub show: Show,
}

/// What `if_running: activate` found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activation {
    /// A window of the program was restored and asked to come to the front; `foreground` is
    /// whether Windows let it (the foreground lock may only flash its taskbar button).
    Activated { foreground: bool },
    /// No visible window of the program: it is started.
    NotRunning,
}

/// The platform that starts programs and hands URLs to the shell (`win::launch::WinLauncher`;
/// [`FakeLauncher`] in tests; [`TestHostLauncher`] in the debug test host). Called on the launch
/// thread only.
pub trait Launcher: Send + Sync {
    /// Starts a program; returns its process ID.
    fn spawn(&self, r: &SpawnRequest) -> Result<u32, LaunchError>;
    fn shell_open(&self, r: &ShellRequest) -> Result<(), LaunchError>;
    /// Brings a visible window of a process started from `program` to the front.
    fn activate(&self, program: &Path, real: &Path) -> Activation;
}

/// Refuses everything (tests and platforms without launches).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLauncher;

impl Launcher for NoLauncher {
    fn spawn(&self, _: &SpawnRequest) -> Result<u32, LaunchError> {
        Err(LaunchError::Refused)
    }
    fn shell_open(&self, _: &ShellRequest) -> Result<(), LaunchError> {
        Err(LaunchError::Refused)
    }
    fn activate(&self, _: &Path, _: &Path) -> Activation {
        Activation::NotRunning
    }
}

/// One call a [`FakeLauncher`] received.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Call {
    Spawn(SpawnRequest),
    Shell(ShellRequest),
    Activate(PathBuf),
}

/// Records every call and answers as configured: the tests' platform (never starts anything).
#[derive(Debug, Default)]
pub struct FakeLauncher {
    pub calls: Mutex<Vec<Call>>,
    pub spawn_error: Mutex<Option<LaunchError>>,
    pub shell_error: Mutex<Option<LaunchError>>,
    /// What `activate` answers.
    pub running: Mutex<Option<Activation>>,
    /// How long each call takes (a slow shell hand-off).
    pub delay: Mutex<Duration>,
}

impl FakeLauncher {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }

    fn record(&self, c: Call) {
        let d = self.delay.lock().map(|d| *d).unwrap_or_default();
        if !d.is_zero() {
            std::thread::sleep(d);
        }
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(c);
        }
    }
}

impl Launcher for FakeLauncher {
    fn spawn(&self, r: &SpawnRequest) -> Result<u32, LaunchError> {
        self.record(Call::Spawn(r.clone()));
        match self.spawn_error.lock().ok().and_then(|e| *e) {
            Some(e) => Err(e),
            None => Ok(4242),
        }
    }
    fn shell_open(&self, r: &ShellRequest) -> Result<(), LaunchError> {
        self.record(Call::Shell(r.clone()));
        match self.shell_error.lock().ok().and_then(|e| *e) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    fn activate(&self, program: &Path, _: &Path) -> Activation {
        self.record(Call::Activate(program.to_path_buf()));
        self.running
            .lock()
            .ok()
            .and_then(|r| *r)
            .unwrap_or(Activation::NotRunning)
    }
}

/// The debug test host's platform (never in a release build): programs start only from inside
/// the test's own folder (`--test-launch-root`) and always hidden (`CREATE_NO_WINDOW`,
/// `SW_HIDE`); URLs and packaged apps are **never** handed to the shell, only recorded as one
/// JSON line each in `<root>\shell-open.jsonl` (the verb, the target, the window state); no
/// window of another program is ever activated. Without a root everything is refused.
#[derive(Clone)]
pub struct TestHostLauncher {
    inner: std::sync::Arc<dyn Launcher>,
    root: Option<PathBuf>,
}

impl std::fmt::Debug for TestHostLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHostLauncher")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl TestHostLauncher {
    pub fn new(inner: std::sync::Arc<dyn Launcher>, root: Option<PathBuf>) -> TestHostLauncher {
        TestHostLauncher { inner, root }
    }

    /// The record file of shell hand-offs.
    pub fn record_file(root: &Path) -> PathBuf {
        root.join("shell-open.jsonl")
    }
}

impl Launcher for TestHostLauncher {
    fn spawn(&self, r: &SpawnRequest) -> Result<u32, LaunchError> {
        let root = self
            .root
            .as_ref()
            .and_then(|r| std::fs::canonicalize(r).ok())
            .ok_or(LaunchError::Refused)?;
        let program = std::fs::canonicalize(&r.program).map_err(|_| LaunchError::NotFound)?;
        if !program.starts_with(&root) {
            return Err(LaunchError::Refused);
        }
        let mut hidden = r.clone();
        hidden.show = Show::Hidden;
        self.inner.spawn(&hidden)
    }

    fn shell_open(&self, r: &ShellRequest) -> Result<(), LaunchError> {
        use std::io::Write;
        let root = self.root.as_ref().ok_or(LaunchError::Refused)?;
        let line = json!({ "verb": r.verb, "target": r.target, "show": r.show.as_str() });
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::record_file(root))
            .and_then(|mut f| writeln!(f, "{line}"))
            .map_err(|_| LaunchError::Refused)
    }

    fn activate(&self, _: &Path, _: &Path) -> Activation {
        Activation::NotRunning
    }
}

// ---------------------------------------------------------------------------------------------
// Running a plan

/// What [`execute`] works with.
pub struct Exec<'a> {
    pub launcher: &'a dyn Launcher,
    /// Environment lookup for known-folder tokens ([`process_var`] in the app).
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
    /// Whether a pause, stop, emergency or reload happened since the trigger fired.
    pub cancelled: &'a dyn Fn() -> bool,
}

impl std::fmt::Debug for Exec<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("launch::Exec")
    }
}

fn run_target(t: &Target, x: &Exec<'_>) -> Result<(), &'static str> {
    match t {
        Target::Program(spec) => {
            let r = resolve(spec, x.var)?;
            if spec.activate
                && let Activation::Activated { .. } = x.launcher.activate(&r.program, &r.real)
            {
                return Ok(());
            }
            let command_line = command_line(&r.program, &spec.args);
            if command_line.encode_utf16().count() > MAX_COMMAND_LINE {
                return Err("launch_failed");
            }
            x.launcher
                .spawn(&SpawnRequest {
                    program: r.program,
                    command_line,
                    working_dir: r.working_dir,
                    show: spec.show,
                    env: spec.env.clone(),
                })
                .map(|_| ())
                .map_err(LaunchError::program_outcome)
        }
        Target::App { app_id, show } => {
            if !app_id_ok(app_id) {
                return Err("launch_failed");
            }
            x.launcher
                .shell_open(&ShellRequest {
                    verb: "open",
                    target: format!("shell:AppsFolder\\{app_id}"),
                    show: *show,
                })
                .map_err(|e| match e {
                    LaunchError::NoAssociation => "program_not_found",
                    e => e.program_outcome(),
                })
        }
        Target::Url(url) => {
            check_url(url).map_err(|_| "launch_failed")?;
            x.launcher
                .shell_open(&ShellRequest {
                    verb: "open",
                    target: url.clone(),
                    show: Show::Normal,
                })
                .map_err(LaunchError::url_outcome)
        }
    }
}

/// Carries out a plan's targets in order; returns the outcome code (§12). A cancellation between
/// targets ends the run with `cancelled` (a started program is never stopped). With
/// `on_error: continue` later targets still run and the first failure is the outcome.
pub fn execute(plan: &Plan, x: &Exec<'_>) -> &'static str {
    let mut first_error = None;
    for t in &plan.targets {
        if (x.cancelled)() {
            return "cancelled";
        }
        if let Err(code) = run_target(t, x) {
            if plan.stop_on_error {
                return code;
            }
            first_error.get_or_insert(code);
        }
    }
    first_error.unwrap_or("ok")
}

/// One run per launch rule at a time, and at most one start per rule per [`DEBOUNCE`].
#[derive(Debug, Default)]
pub struct Gate {
    running: HashSet<String>,
    last: HashMap<String, Instant>,
}

impl Gate {
    /// Admits a fired launch rule, or says why not (`busy`, `rate_limited`).
    pub fn admit(&mut self, rule: &str, now: Instant) -> Result<(), &'static str> {
        if self.running.contains(rule) {
            return Err("busy");
        }
        self.last
            .retain(|_, t| now.saturating_duration_since(*t) < DEBOUNCE);
        if self.last.contains_key(rule) {
            return Err("rate_limited");
        }
        self.running.insert(rule.to_string());
        self.last.insert(rule.to_string(), now);
        Ok(())
    }

    /// The rule's run ended.
    pub fn done(&mut self, rule: &str) {
        self.running.remove(rule);
    }
}

// ---------------------------------------------------------------------------------------------
// Previews, summaries and the editor's data model (Step 74 draws it)

/// The editor's icon preview of a program: 32-bit RGBA pixels, row by row from the top
/// (`win::launch::icon_preview` extracts it from the file without running it).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct IconPreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for IconPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IconPreview({}x{})", self.width, self.height)
    }
}

/// What the editor and the playground show about a program (§5.5 preview): its file name or app,
/// whether it is there, the number of arguments — never the full path or the arguments.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProgramPreview {
    pub program: String,
    /// The file exists. For a packaged app only that the ID is well formed: this version cannot
    /// list installed apps (that needs the shell's COM enumeration), and a run of an app that is
    /// not installed ends with `program_not_found`.
    pub found: bool,
    pub arguments: usize,
    pub if_running: &'static str,
    /// The file starts like a Windows program (`MZ`), so it is not a renamed document.
    pub is_program: bool,
    /// The working folder exists (or none is set).
    pub folder_found: bool,
}

impl ProgramPreview {
    /// The `preview_open_program` shape of `ddalgak-simulation-v1`.
    pub fn to_json(&self) -> Value {
        json!({ "program": self.program, "found": self.found, "arguments": self.arguments,
                "if_running": self.if_running })
    }
}

/// Whether a file starts with the `MZ` signature of Windows programs.
pub fn looks_like_program(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && head == *b"MZ"
}

/// The one-line summary of a launch step (titles, the editor): "Opens notepad.exe with 2
/// arguments", "Opens github.com in your browser". No path, argument or URL path.
pub fn summary(step: &Value) -> String {
    match step.get("type").and_then(Value::as_str) {
        Some("open_url") => {
            let url = step.get("url").and_then(Value::as_str).unwrap_or_default();
            match check_url(url) {
                Ok(p) => format!("Opens {} in your browser", p.host),
                Err(_) => "Opens a website in your browser".into(),
            }
        }
        Some("open_program") => {
            let mut s = match (
                step.get("path").and_then(Value::as_str),
                step.get("app_id").and_then(Value::as_str),
            ) {
                (Some(p), _) => format!("Opens {}", program_name(p)),
                (None, Some(id)) => format!(
                    "Opens the app {}",
                    id.split(['_', '!']).next().unwrap_or(id)
                ),
                _ => "Opens a program".into(),
            };
            let n = step
                .get("args")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            match n {
                0 => {}
                1 => s.push_str(" with 1 argument"),
                n => s.push_str(&format!(" with {n} arguments")),
            }
            match step.get("window").and_then(Value::as_str) {
                Some("maximized") => s.push_str(", maximized"),
                Some("minimized") => s.push_str(", minimized"),
                _ => {}
            }
            if step.get("if_running").and_then(Value::as_str) == Some("activate") {
                s.push_str(", or brings it to the front");
            }
            s
        }
        _ => String::new(),
    }
}

/// The program picker's draft (GUI_JOURNEYS J4 "Open a program"; Step 74 draws it). Nothing is
/// started by the editor: **Save** writes through the rule store only when the user presses it
/// and the draft is valid, after the consent dialog that names the exact target; **Test** opens
/// it only after its own confirmation (DDALGAK_RULE_MODEL §11).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ProgramDraft {
    /// The rule's `title` (1–80 characters).
    pub title: String,
    /// The program as typed, pasted or picked; empty when an app is chosen.
    pub path: String,
    /// A packaged app's AppUserModelID instead of a path.
    pub app_id: String,
    pub args: Vec<String>,
    /// Empty: the program's folder.
    pub working_dir: String,
    /// `normal`, `maximized` or `minimized`.
    pub window: String,
    /// Bring it to the front if it already runs (`if_running: activate`).
    pub activate: bool,
    /// Explicit environment overrides (an advanced option; values stay private).
    pub env: Vec<(String, String)>,
    /// The draft as last saved (`None` for a new rule).
    pub saved: Option<Box<ProgramDraft>>,
}

impl ProgramDraft {
    /// A draft for a file chosen in the file picker: the path normalized and, when it lies in a
    /// known folder, written with its token; the title from the file name.
    pub fn picked(path: &str, var: &dyn Fn(&str) -> Option<OsString>) -> ProgramDraft {
        let p = normalize_path(path);
        let p = tokenize(&p, var).unwrap_or(p);
        ProgramDraft {
            title: format!("Open {}", program_name(&p))
                .chars()
                .take(80)
                .collect(),
            path: p,
            window: "normal".into(),
            ..ProgramDraft::default()
        }
    }

    fn path_value(&self) -> String {
        normalize_path(&self.path)
    }

    /// The field problems of the draft (the schema's rules and the runtime's, with positions; no
    /// path, argument or value is quoted).
    pub fn problems(&self) -> Vec<FieldProblem> {
        let mut out = Vec::new();
        let mut push = |path: &'static str, message: String| {
            out.push(FieldProblem { path, message });
        };
        if !(1..=80).contains(&self.title.chars().count()) {
            push("title", "the rule name needs 1–80 characters".into());
        }
        let path = self.path_value();
        let app = self.app_id.trim();
        match (path.is_empty(), app.is_empty()) {
            (true, true) => push(
                "steps[0].path",
                "choose a program, or an app instead".into(),
            ),
            (false, false) => push(
                "steps[0].app_id",
                "choose a program or an app, not both".into(),
            ),
            (false, true) => {
                if let Some(p) = path_problem(&path, true) {
                    push("steps[0].path", p.message());
                }
            }
            (true, false) => {
                if !app_id_ok(app) {
                    push(
                        "steps[0].app_id",
                        "an app ID looks like Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
                    );
                }
                if !self.args.is_empty()
                    || !self.working_dir.trim().is_empty()
                    || !self.env.is_empty()
                {
                    push(
                        "steps[0].app_id",
                        "an app takes no arguments, start folder or environment".into(),
                    );
                }
            }
        }
        if let Some(p) = args_problem(&self.args) {
            push("steps[0].args", p);
        }
        let wd = normalize_path(&self.working_dir);
        if !wd.is_empty()
            && let Some(p) = path_problem(&wd, false)
        {
            push("steps[0].working_dir", p.message());
        }
        if Show::parse(Some(self.window.as_str()).filter(|w| !w.is_empty())).is_none() {
            push("steps[0].window", "normal, maximized or minimized".into());
        }
        if let Some(p) = env_problem(&self.env) {
            push("steps[0].env", p);
        }
        if !path.is_empty()
            && command_line(Path::new(&path), &self.args)
                .encode_utf16()
                .count()
                > MAX_COMMAND_LINE
        {
            push(
                "steps[0].args",
                format!(
                    "the quoted command line exceeds Windows' limit of {MAX_COMMAND_LINE} characters"
                ),
            );
        }
        out
    }

    /// What the editor shows under the program field, from the file system (never runs it).
    pub fn preview(&self, var: &dyn Fn(&str) -> Option<OsString>) -> ProgramPreview {
        let if_running = if self.activate {
            "activate"
        } else {
            "start_new"
        };
        let app = self.app_id.trim();
        if !app.is_empty() {
            return ProgramPreview {
                program: app.split(['_', '!']).next().unwrap_or(app).to_string(),
                found: app_id_ok(app),
                arguments: 0,
                if_running,
                is_program: app_id_ok(app),
                folder_found: true,
            };
        }
        let path = self.path_value();
        let file = (path_problem(&path, true).is_none())
            .then(|| expand(&path, var))
            .flatten();
        let found = file.as_ref().is_some_and(|f| f.is_file());
        let wd = normalize_path(&self.working_dir);
        ProgramPreview {
            program: program_name(&path).to_string(),
            found,
            arguments: self.args.len(),
            if_running,
            is_program: found && file.as_deref().is_some_and(looks_like_program),
            folder_found: wd.is_empty()
                || (path_problem(&wd, false).is_none()
                    && expand(&wd, var).is_some_and(|d| d.is_dir())),
        }
    }

    /// The command line Ddalgak would pass (shown in the editor's details only, as the user's own
    /// data); `None` while the path is not valid.
    pub fn command_line(&self, var: &dyn Fn(&str) -> Option<OsString>) -> Option<String> {
        let path = self.path_value();
        if path_problem(&path, true).is_some() {
            return None;
        }
        let p = expand(&path, var)?;
        Some(command_line(&p, &self.args))
    }

    /// The step the draft saves as (canonical: defaults left out).
    pub fn step(&self) -> Value {
        let mut s = json!({ "type": "open_program" });
        let app = self.app_id.trim();
        if app.is_empty() {
            s["path"] = json!(self.path_value());
            if !self.args.is_empty() {
                s["args"] = json!(self.args);
            }
            let wd = normalize_path(&self.working_dir);
            if !wd.is_empty() {
                s["working_dir"] = json!(wd);
            }
            if !self.env.is_empty() {
                s["env"] = Value::Array(
                    self.env
                        .iter()
                        .map(|(n, v)| json!({ "name": n, "value": v }))
                        .collect(),
                );
            }
        } else {
            s["app_id"] = json!(app);
        }
        if matches!(self.window.as_str(), "maximized" | "minimized") {
            s["window"] = json!(self.window);
        }
        if self.activate {
            s["if_running"] = json!("activate");
        }
        s
    }

    /// The live summary sentence ("Opens notepad.exe with 2 arguments").
    pub fn summary(&self) -> String {
        summary(&self.step())
    }

    pub fn dirty(&self) -> bool {
        match &self.saved {
            None => true,
            Some(s) => {
                let mut a = self.clone();
                a.saved = None;
                let mut b = (**s).clone();
                b.saved = None;
                a != b
            }
        }
    }

    /// Save is enabled for a changed, valid draft; otherwise the problems say why not.
    pub fn can_save(&self) -> Result<(), Vec<FieldProblem>> {
        let p = self.problems();
        if !p.is_empty() {
            return Err(p);
        }
        if !self.dirty() {
            return Err(vec![FieldProblem {
                path: "",
                message: "nothing changed since the last save".into(),
            }]);
        }
        Ok(())
    }

    /// Test is enabled for a valid draft whose program exists; it still asks before opening.
    pub fn can_test(&self, var: &dyn Fn(&str) -> Option<OsString>) -> bool {
        self.problems().is_empty() && self.preview(var).found
    }
}

/// The website field's draft (GUI_JOURNEYS J4 "Open a website").
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct UrlDraft {
    pub title: String,
    /// The address as typed or pasted.
    pub input: String,
    pub saved: Option<Box<UrlDraft>>,
}

impl UrlDraft {
    /// The address as it would be stored ([`normalize_url`]).
    pub fn normalized(&self) -> Result<String, UrlProblem> {
        normalize_url(&self.input)
    }

    pub fn problems(&self) -> Vec<FieldProblem> {
        let mut out = Vec::new();
        if !(1..=80).contains(&self.title.chars().count()) {
            out.push(FieldProblem {
                path: "title",
                message: "the rule name needs 1–80 characters".into(),
            });
        }
        if let Err(e) = self.normalized() {
            out.push(FieldProblem {
                path: "steps[0].url",
                message: e.message(),
            });
        }
        out
    }

    /// `{scheme, host}` of the normalized address.
    pub fn preview(&self) -> Option<Value> {
        self.normalized().ok().and_then(|u| url_preview(&u))
    }

    pub fn summary(&self) -> String {
        summary(&json!({ "type": "open_url", "url": self.normalized().unwrap_or_default() }))
    }

    /// The step the draft saves as; `None` while the address is not valid.
    pub fn step(&self) -> Option<Value> {
        self.normalized()
            .ok()
            .map(|u| json!({ "type": "open_url", "url": u }))
    }

    pub fn dirty(&self) -> bool {
        self.saved
            .as_ref()
            .is_none_or(|s| s.title != self.title || s.normalized() != self.normalized())
    }

    pub fn can_save(&self) -> Result<(), Vec<FieldProblem>> {
        let p = self.problems();
        if !p.is_empty() {
            return Err(p);
        }
        if !self.dirty() {
            return Err(vec![FieldProblem {
                path: "",
                message: "nothing changed since the last save".into(),
            }]);
        }
        Ok(())
    }
}

/// The "Open a program" form, in display order (GUI_JOURNEYS J4, Step 74). A program or an app
/// is required (one of the first two fields).
pub const OPEN_PROGRAM_FIELDS: &[Field] = &[
    Field {
        path: "steps[0].path",
        input: "program_picker",
        required: false,
        default: None,
        label: "Program",
        help: "Choose… opens a file picker limited to .exe files; a path you type or paste is checked as you type (quotes from \"Copy as path\" are removed, known folders become %LocalAppData%-style tokens). Command interpreters and script hosts cannot be chosen.",
    },
    Field {
        path: "steps[0].app_id",
        input: "app_id",
        required: false,
        default: None,
        label: "Or an app",
        help: "A packaged app's ID (PackageFamilyName!AppId). Apps take no arguments or start folder.",
    },
    Field {
        path: "steps[0].args",
        input: "list",
        required: false,
        default: None,
        label: "Arguments",
        help: "One argument per line, up to 32; Ddalgak quotes each one. Never a shell command.",
    },
    Field {
        path: "steps[0].working_dir",
        input: "folder_picker",
        required: false,
        default: None,
        label: "Start in",
        help: "Empty: the program's folder.",
    },
    Field {
        path: "steps[0].window",
        input: "choice",
        required: false,
        default: Some("normal"),
        label: "Window",
        help: "Normal, maximized or minimized. Programs never start hidden.",
    },
    Field {
        path: "steps[0].if_running",
        input: "toggle",
        required: false,
        default: Some("start_new"),
        label: "Bring it to the front if it is already running",
        help: "Windows may only flash its taskbar button when another program is in front.",
    },
    Field {
        path: "steps[0].env",
        input: "pairs",
        required: false,
        default: None,
        label: "Environment (advanced)",
        help: "Variables set for this program only, on top of yours. Shown when you approve the rule; values are never shown in lists or logs.",
    },
];

/// The "Open a website" form (§5.6).
pub const OPEN_URL_FIELDS: &[Field] = &[Field {
    path: "steps[0].url",
    input: "url",
    required: true,
    default: None,
    label: "Website",
    help: "https:// is added when missing; only http and https addresses are accepted, without a user name or password.",
}];

#[cfg(test)]
mod tests {
    use super::*;

    fn no_var(_: &str) -> Option<OsString> {
        None
    }

    fn vars(name: &str) -> Option<OsString> {
        match name {
            "LOCALAPPDATA" => Some(r"C:\Users\kim\AppData\Local".into()),
            "APPDATA" => Some(r"C:\Users\kim\AppData\Roaming".into()),
            "USERPROFILE" => Some(r"C:\Users\kim".into()),
            "ProgramFiles" => Some(r"C:\Program Files".into()),
            "SystemRoot" => Some(r"C:\Windows".into()),
            _ => None,
        }
    }

    #[test]
    fn paths_must_be_absolute_exe_files_and_never_shell_hosts() {
        for ok in [
            r"C:\Windows\System32\notepad.exe",
            r"%SystemRoot%\System32\notepad.exe",
            r"%localappdata%\Programs\Microsoft VS Code\Code.exe",
            r"D:\도구\한글 프로그램.EXE",
            r"%ProgramFiles(x86)%\App\app.exe",
        ] {
            assert_eq!(path_problem(ok, true), None, "{ok}");
        }
        let cases = [
            ("", PathProblem::Empty),
            (r"notepad.exe", PathProblem::NotAbsolute),
            (r"..\tool.exe", PathProblem::NotAbsolute),
            (r"\\server\share\tool.exe", PathProblem::NotAbsolute),
            (r"%TEMP%\tool.exe", PathProblem::NotAbsolute),
            (r"%SystemRoot%notepad.exe", PathProblem::NotAbsolute),
            (r"C:/Windows/notepad.exe", PathProblem::NotAbsolute),
            (r"C:\a|b.exe", PathProblem::BadCharacter { at: 4 }),
            (r#"C:\"x".exe"#, PathProblem::BadCharacter { at: 3 }),
            ("C:\\a\u{7}.exe", PathProblem::BadCharacter { at: 4 }),
            (r"C:\notes.txt:evil.exe", PathProblem::Colon { at: 12 }),
            (r"C:\tools\run.bat", PathProblem::NotExe),
            (r"C:\tools\link.lnk", PathProblem::NotExe),
            (r"C:\tools\.exe", PathProblem::NotExe),
            (r"C:\Windows\System32\cmd.exe", PathProblem::ShellHost),
            (
                r"C:\Windows\System32\WindowsPowerShell\v1.0\PowerShell.EXE",
                PathProblem::ShellHost,
            ),
            (r"%SystemRoot%\System32\mshta.exe", PathProblem::ShellHost),
            (
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                PathProblem::ShellHost,
            ),
        ];
        for (p, want) in cases {
            assert_eq!(path_problem(p, true), Some(want), "{p}");
        }
        let long = format!(r"C:\{}.exe", "a".repeat(260));
        assert_eq!(path_problem(&long, true), Some(PathProblem::TooLong));
        // Folders: a bare token is a folder; a file name is not required.
        assert_eq!(path_problem("%UserProfile%", false), None);
        assert_eq!(path_problem(r"C:\", false), None);
        assert_eq!(
            path_problem("%UserProfile%", true),
            Some(PathProblem::NotAbsolute)
        );
        // Messages never quote the path.
        for (p, _) in cases {
            if let Some(m) = path_problem(p, true) {
                let msg = m.message();
                assert!(p.is_empty() || !msg.contains(p), "{msg}");
            }
        }
    }

    #[test]
    fn known_folder_tokens_expand_and_picked_paths_get_them() {
        assert_eq!(
            expand(r"%LocalAppData%\Programs\x.exe", &vars),
            Some(PathBuf::from(r"C:\Users\kim\AppData\Local\Programs\x.exe"))
        );
        assert_eq!(
            expand("%UserProfile%", &vars),
            Some(PathBuf::from(r"C:\Users\kim"))
        );
        assert_eq!(expand(r"%TEMP%\x.exe", &vars), None);
        assert_eq!(
            expand(r"%SystemRoot%\x.exe", &no_var),
            None,
            "unset variable"
        );
        assert_eq!(
            expand(r"C:\x.exe", &no_var),
            Some(PathBuf::from(r"C:\x.exe"))
        );
        // The most specific folder wins and the user name disappears.
        assert_eq!(
            tokenize(r"C:\Users\kim\AppData\Local\Programs\Code\Code.exe", &vars).as_deref(),
            Some(r"%LocalAppData%\Programs\Code\Code.exe")
        );
        assert_eq!(
            tokenize(r"c:\users\KIM\tools\t.exe", &vars).as_deref(),
            Some(r"%UserProfile%\tools\t.exe")
        );
        assert_eq!(tokenize(r"D:\x.exe", &vars), None);
        assert_eq!(
            tokenize(r"C:\Users\kimchi\x.exe", &vars),
            None,
            "not a prefix by folder"
        );
        assert_eq!(
            normalize_path(r#"  "C:/Program Files/App/app.exe"  "#),
            r"C:\Program Files\App\app.exe"
        );
        let d = ProgramDraft::picked(
            r#""C:\Users\kim\AppData\Local\Programs\Microsoft VS Code\Code.exe""#,
            &vars,
        );
        assert_eq!(
            d.path,
            r"%LocalAppData%\Programs\Microsoft VS Code\Code.exe"
        );
        assert_eq!(d.title, "Open Code.exe");
    }

    /// The reference splitter: `CommandLineToArgvW`'s rules for arguments after the program.
    fn split(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut in_quotes = false;
        let mut any = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let mut n = 0;
                while i < chars.len() && chars[i] == '\\' {
                    n += 1;
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    cur.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        i += 1;
                    }
                } else {
                    cur.extend(std::iter::repeat_n('\\', n));
                }
                any = true;
                continue;
            }
            if c == '"' {
                in_quotes = !in_quotes;
                any = true;
            } else if (c == ' ' || c == '\t') && !in_quotes {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            } else {
                cur.push(c);
                any = true;
            }
            i += 1;
        }
        if any {
            out.push(cur);
        }
        out
    }

    #[test]
    fn arguments_are_quoted_so_windows_splits_them_back_exactly() {
        let args: Vec<String> = [
            "plain",
            "",
            "with space",
            "한글 인자",
            "tab\there",
            r#"say "hi""#,
            r"C:\path with space\",
            r"C:\no-space\",
            r"back\\slash",
            r#"\"quoted\""#,
            r"trailing\\",
            "--flag=a b",
            "emoji 👍",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        let line = command_line(Path::new(r"C:\프로그램 폴더\app.exe"), &args);
        assert!(
            line.starts_with("\"C:\\프로그램 폴더\\app.exe\" "),
            "{line}"
        );
        let rest = &line[line.find("\" ").unwrap() + 2..];
        assert_eq!(split(rest), args);
        let mut s = String::new();
        quote_arg("simple", &mut s);
        assert_eq!(s, "simple");
        s.clear();
        quote_arg(r#"a"b"#, &mut s);
        assert_eq!(s, r#""a\"b""#);
        s.clear();
        quote_arg(r"C:\dir\", &mut s);
        assert_eq!(s, r"C:\dir\", "no space: written as is");
        s.clear();
        quote_arg(r"C:\my dir\", &mut s);
        assert_eq!(s, r#""C:\my dir\\""#);
        assert!(
            args_problem(&["a\nb".into()])
                .unwrap()
                .contains("argument 0")
        );
        assert!(args_problem(&vec!["x".to_string(); 33]).is_some());
    }

    #[test]
    fn environment_overrides_are_explicit_checked_and_merged() {
        let env = vec![
            ("MY_TOKEN".to_string(), "s3cret-value".to_string()),
            ("Path".to_string(), r"C:\bin".to_string()),
        ];
        assert_eq!(env_problem(&env), None);
        let merged = merge_env(
            vec![
                (OsString::from("PATH"), OsString::from(r"C:\Windows")),
                (OsString::from("HOME"), OsString::from(r"C:\Users\kim")),
            ],
            &env,
        );
        let names: Vec<String> = merged
            .iter()
            .map(|(n, _)| n.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names,
            ["HOME", "MY_TOKEN", "Path"],
            "replaced case-insensitively, sorted"
        );
        assert_eq!(merged[2].1, OsString::from(r"C:\bin"));
        for (bad, why) in [
            (vec![("1X".into(), "v".into())], "name"),
            (vec![("A=B".into(), "v".into())], "name"),
            (
                vec![("A".into(), "v".into()), ("a".into(), "w".into())],
                "twice",
            ),
            (vec![("A".into(), "line\nbreak".into())], "control"),
        ] {
            let p = env_problem(&bad).unwrap();
            assert!(p.contains(why), "{p}");
            assert!(!p.contains("line\nbreak"), "values are never quoted: {p}");
        }
    }

    #[test]
    fn urls_are_http_or_https_only_and_normalized_for_the_editor() {
        let ok = [
            ("github.com", "https://github.com/"),
            (
                "  https://GitHub.com/hellomouv/kim  ",
                "https://github.com/hellomouv/kim",
            ),
            ("HTTP://Example.COM", "http://example.com/"),
            (
                "localhost:3000/status?view=compact",
                "https://localhost:3000/status?view=compact",
            ),
            ("example.com?q=1#top", "https://example.com/?q=1#top"),
            (
                "https://ko.wikipedia.org/wiki/김치",
                "https://ko.wikipedia.org/wiki/%EA%B9%80%EC%B9%98",
            ),
            (
                "https://example.com/a b\"c<d>",
                "https://example.com/a%20b%22c%3Cd%3E",
            ),
            (
                "https://example.com/100%25/50%",
                "https://example.com/100%25/50%25",
            ),
            ("https://bücher.example/", "https://xn--bcher-kva.example/"),
            ("한국.kr", "https://xn--3e0b707e.kr/"),
            ("https://例え。jp", "https://xn--r8jz45g.jp/"),
        ];
        for (input, want) in ok {
            assert_eq!(normalize_url(input).as_deref(), Ok(want), "{input}");
            assert!(check_url(want).is_ok(), "{want}");
        }
        let bad = [
            ("", UrlProblem::Empty),
            (
                "file:///C:/Windows/win.ini",
                UrlProblem::Dangerous("file".into()),
            ),
            (
                "javascript:alert(1)",
                UrlProblem::Dangerous("javascript".into()),
            ),
            (
                "JavaScript:alert(1)",
                UrlProblem::Dangerous("javascript".into()),
            ),
            (
                "ms-settings:privacy",
                UrlProblem::Dangerous("ms-settings".into()),
            ),
            ("ms-msdt:/id x", UrlProblem::Dangerous("ms-msdt".into())),
            (
                "search-ms:query=x",
                UrlProblem::Dangerous("search-ms".into()),
            ),
            ("vbscript:msgbox", UrlProblem::Dangerous("vbscript".into())),
            ("data:text/html,<b>", UrlProblem::Dangerous("data".into())),
            (
                r"C:\Windows\notepad.exe",
                UrlProblem::Dangerous("file".into()),
            ),
            (
                "mailto:kim@example.com",
                UrlProblem::Scheme("mailto".into()),
            ),
            (
                "zoommtg://zoom.us/join",
                UrlProblem::Scheme("zoommtg".into()),
            ),
            ("ftp://example.com/", UrlProblem::Scheme("ftp".into())),
            ("https://user:pass@example.com/", UrlProblem::Credentials),
            ("https://token@example.com/", UrlProblem::Credentials),
            ("https://[::1]/", UrlProblem::Host),
            ("https:///path", UrlProblem::Host),
            ("https://exa mple.com/", UrlProblem::Host),
            ("https://example.com:0/", UrlProblem::Port),
            ("https://example.com:99999/", UrlProblem::Port),
            ("https:example.com", UrlProblem::Malformed),
            ("https://example.com/a\tb", UrlProblem::Control { at: 21 }),
        ];
        for (input, want) in bad {
            assert_eq!(normalize_url(input), Err(want.clone()), "{input}");
        }
        // The stored form: exactly lower-case http(s), nothing to encode.
        for (url, want) in [
            ("HTTPS://example.com/", UrlProblem::Malformed),
            ("https://example.com/a b", UrlProblem::Space { at: 21 }),
            ("https://example.com/\"", UrlProblem::Character { at: 20 }),
            ("https://example.com/\u{1}", UrlProblem::Control { at: 20 }),
            ("file:///etc/passwd", UrlProblem::Dangerous("file".into())),
            ("custom://open", UrlProblem::Scheme("custom".into())),
            ("https://-x.com/", UrlProblem::Host),
        ] {
            assert_eq!(check_url(url), Err(want), "{url}");
        }
        let p = check_url("https://example.com:8443/x?y#z").unwrap();
        assert_eq!(
            (p.scheme, p.host, p.port, p.rest),
            ("https", "example.com", Some(8443), "/x?y#z")
        );
        assert_eq!(punycode("münchen").as_deref(), Some("mnchen-3ya"));
    }

    #[test]
    fn urls_are_redacted_to_scheme_and_host() {
        for (url, want) in [
            (
                "https://example.com/reset?token=abc123",
                "https://example.com/…",
            ),
            (
                "https://user:pw@example.com:8080/x",
                "https://example.com:8080/…",
            ),
            ("http://localhost:3000", "http://localhost:3000"),
            ("javascript:alert(document.cookie)", "javascript:…"),
            ("not a url", "(not a web address)"),
        ] {
            let r = redact_url(url);
            assert_eq!(r, want, "{url}");
            assert!(!r.contains("abc123") && !r.contains("pw") && !r.contains("cookie"));
        }
        assert_eq!(
            url_preview("https://github.com/hellomouv/kim?tab=readme"),
            Some(json!({ "scheme": "https", "host": "github.com" }))
        );
        // Every message is free of the URL's path and query.
        for input in [
            "https://user:secret@x.com/?k=v",
            "ms-settings:secret",
            "https://x.com:0/secret",
        ] {
            let m = normalize_url(input).unwrap_err().message();
            assert!(!m.contains("secret"), "{m}");
        }
    }

    #[test]
    fn steps_compile_with_the_runtime_checks_and_messages_never_quote_targets() {
        let t = compile_step(&json!({ "type": "open_program", "path": r"%SystemRoot%\System32\notepad.exe",
            "args": ["a b"], "working_dir": "%UserProfile%", "window": "maximized", "if_running": "activate",
            "env": [{ "name": "K", "value": "v" }] }))
        .unwrap();
        assert_eq!(
            t,
            Target::Program(ProgramSpec {
                path: r"%SystemRoot%\System32\notepad.exe".into(),
                args: vec!["a b".into()],
                working_dir: Some("%UserProfile%".into()),
                show: Show::Maximized,
                activate: true,
                env: vec![("K".into(), "v".into())],
            })
        );
        assert_eq!(
            compile_step(
                &json!({ "type": "open_program", "app_id": "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App" })
            ),
            Ok(Target::App {
                app_id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
                show: Show::Normal
            })
        );
        assert_eq!(
            compile_step(&json!({ "type": "open_url", "url": "https://github.com/" })),
            Ok(Target::Url("https://github.com/".into()))
        );
        for bad in [
            json!({ "type": "open_program", "path": r"C:\x.txt:secret.exe" }),
            json!({ "type": "open_program", "path": r"C:\Windows\System32\cmd.exe" }),
            json!({ "type": "open_program", "path": r"C:\x.exe", "env": [{ "name": "A", "value": "secret" }, { "name": "a", "value": "secret" }] }),
            json!({ "type": "open_program", "app_id": "A.B_c!App", "env": [{ "name": "A", "value": "secret" }] }),
            json!({ "type": "open_program", "path": r"C:\x.exe", "args": ["secret\u{1}"] }),
            json!({ "type": "open_url", "url": "https://example.com/\"secret" }),
            json!({ "type": "open_url", "url": "file:///secret" }),
            json!({ "type": "open_program", "path": r"C:\x.exe", "window": "hidden" }),
        ] {
            let e = compile_step(&bad).unwrap_err();
            assert!(!e.contains("secret"), "{e}");
        }
        let huge: Vec<Value> = (0..32)
            .map(|_| json!(format!("{}\"", "\\".repeat(1020))))
            .collect();
        let e = compile_step(&json!({ "type": "open_program", "path": r"C:\x.exe", "args": huge }))
            .unwrap_err();
        assert!(e.contains("limit"), "{e}");
    }

    #[test]
    fn os_errors_map_to_the_outcomes_of_the_rule_model() {
        assert_eq!(LaunchError::from_os(740), LaunchError::ElevationRequired);
        assert_eq!(
            LaunchError::from_os(740).program_outcome(),
            "program_requires_elevation"
        );
        assert_eq!(
            LaunchError::from_os(2).program_outcome(),
            "program_not_found"
        );
        assert_eq!(
            LaunchError::from_os(3).program_outcome(),
            "program_not_found"
        );
        assert_eq!(
            LaunchError::from_os(267).program_outcome(),
            "program_not_found"
        );
        assert_eq!(LaunchError::from_os(193).program_outcome(), "launch_failed");
        assert_eq!(LaunchError::from_os(5).program_outcome(), "launch_failed");
        assert_eq!(
            LaunchError::from_os(1155).url_outcome(),
            "browser_unavailable"
        );
        assert_eq!(
            LaunchError::from_os(31).url_outcome(),
            "browser_unavailable"
        );
        assert_eq!(LaunchError::from_os(1234).url_outcome(), "launch_failed");
    }

    fn plan(targets: Vec<Target>) -> Plan {
        Plan {
            targets,
            stop_on_error: true,
        }
    }

    fn exec<'a>(l: &'a dyn Launcher, cancelled: &'a dyn Fn() -> bool) -> Exec<'a> {
        Exec {
            launcher: l,
            var: &process_var,
            cancelled,
        }
    }

    #[test]
    fn websites_and_apps_reach_the_shell_with_the_open_verb_only() {
        let f = FakeLauncher::default();
        let never = || false;
        let p = plan(vec![
            Target::Url("https://github.com/hellomouv/kim?tab=readme".into()),
            Target::App {
                app_id: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
                show: Show::Minimized,
            },
        ]);
        assert_eq!(execute(&p, &exec(&f, &never)), "ok");
        assert_eq!(
            f.calls(),
            [
                Call::Shell(ShellRequest {
                    verb: "open",
                    target: "https://github.com/hellomouv/kim?tab=readme".into(),
                    show: Show::Normal
                }),
                Call::Shell(ShellRequest {
                    verb: "open",
                    target: r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
                        .into(),
                    show: Show::Minimized
                }),
            ]
        );
        // No browser: the outcome says so; a URL that is not http(s) never reaches the shell.
        *f.shell_error.lock().unwrap() = Some(LaunchError::NoAssociation);
        let url = plan(vec![Target::Url("https://example.com/".into())]);
        assert_eq!(execute(&url, &exec(&f, &never)), "browser_unavailable");
        let app = plan(vec![Target::App {
            app_id: "Not.Installed_x!App".into(),
            show: Show::Normal,
        }]);
        assert_eq!(execute(&app, &exec(&f, &never)), "program_not_found");
        let before = f.calls().len();
        for evil in [
            "javascript:alert(1)",
            "file:///C:/x",
            "ms-settings:",
            "https://u:p@x.com/",
        ] {
            assert_eq!(
                execute(&plan(vec![Target::Url(evil.into())]), &exec(&f, &never)),
                "launch_failed"
            );
        }
        assert_eq!(f.calls().len(), before, "refused before the shell");
    }

    #[cfg(windows)]
    struct Dir(PathBuf);

    #[cfg(windows)]
    impl Dir {
        fn new(tag: &str) -> Dir {
            let d = std::env::temp_dir().join(format!(
                "ddalgak-launch-{tag}-{}-{}",
                std::process::id(),
                crate::time::now_secs()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Dir(d)
        }
    }

    #[cfg(windows)]
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    fn program_in(dir: &Path, folder: &str, name: &str) -> PathBuf {
        let d = dir.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, b"MZ not really a program").unwrap();
        p
    }

    #[cfg(windows)]
    fn spec(path: &Path) -> ProgramSpec {
        ProgramSpec {
            path: path.to_string_lossy().to_string(),
            args: vec!["첫 번째".into(), "two".into()],
            working_dir: None,
            show: Show::Normal,
            activate: false,
            env: Vec::new(),
        }
    }

    #[cfg(windows)]
    #[test]
    fn programs_are_resolved_checked_and_started_with_a_quoted_argument_list() {
        let d = Dir::new("spawn");
        let exe = program_in(&d.0, "테스트 폴더 with spaces", "도구 app.exe");
        let f = FakeLauncher::default();
        let never = || false;
        assert_eq!(
            execute(&plan(vec![Target::Program(spec(&exe))]), &exec(&f, &never)),
            "ok"
        );
        let calls = f.calls();
        let Call::Spawn(s) = &calls[0] else {
            panic!("{calls:?}")
        };
        assert_eq!(s.program, exe);
        assert_eq!(
            s.working_dir,
            exe.parent().unwrap(),
            "default: the program's folder"
        );
        assert_eq!(
            s.command_line,
            format!("\"{}\" \"첫 번째\" two", exe.display())
        );
        assert_eq!(s.show, Show::Normal);
        // Elevation required (a fake answer): the outcome says so, nothing is retried elevated.
        *f.spawn_error.lock().unwrap() = Some(LaunchError::ElevationRequired);
        assert_eq!(
            execute(&plan(vec![Target::Program(spec(&exe))]), &exec(&f, &never)),
            "program_requires_elevation"
        );
        assert_eq!(f.calls().len(), 2, "one attempt, no elevated retry");
        *f.spawn_error.lock().unwrap() = None;
        // `if_running: activate`: a running window wins, nothing starts.
        let mut act = spec(&exe);
        act.activate = true;
        *f.running.lock().unwrap() = Some(Activation::Activated { foreground: false });
        assert_eq!(
            execute(&plan(vec![Target::Program(act.clone())]), &exec(&f, &never)),
            "ok"
        );
        assert!(matches!(f.calls().last(), Some(Call::Activate(_))));
        *f.running.lock().unwrap() = Some(Activation::NotRunning);
        assert_eq!(
            execute(&plan(vec![Target::Program(act)]), &exec(&f, &never)),
            "ok"
        );
        assert!(matches!(f.calls().last(), Some(Call::Spawn(_))));
    }

    #[cfg(windows)]
    #[test]
    fn missing_moved_and_disguised_programs_never_start() {
        let d = Dir::new("missing");
        let exe = program_in(&d.0, "app", "app.exe");
        let f = FakeLauncher::default();
        let never = || false;
        let p = plan(vec![Target::Program(spec(&exe))]);
        // Moved away after the rule was written: never searched for.
        let moved = d.0.join("elsewhere");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::rename(&exe, moved.join("app.exe")).unwrap();
        assert_eq!(execute(&p, &exec(&f, &never)), "program_not_found");
        // A folder named like a program.
        std::fs::create_dir_all(d.0.join("app").join("app.exe")).unwrap();
        assert_eq!(execute(&p, &exec(&f, &never)), "program_not_found");
        // A missing working folder.
        let exe2 = program_in(&d.0, "two", "two.exe");
        let mut s = spec(&exe2);
        s.working_dir = Some(d.0.join("gone").to_string_lossy().to_string());
        assert_eq!(
            execute(&plan(vec![Target::Program(s)]), &exec(&f, &never)),
            "program_not_found"
        );
        // An unknown token or an unset variable.
        let mut s = spec(&exe2);
        s.path = r"%NoSuchFolder%\two.exe".into();
        assert_eq!(
            execute(&plan(vec![Target::Program(s)]), &exec(&f, &never)),
            "launch_failed"
        );
        assert!(
            f.calls().is_empty(),
            "nothing reached the platform: {:?}",
            f.calls()
        );
        // The OS says the file vanished between the check and the start.
        *f.spawn_error.lock().unwrap() = Some(LaunchError::NotFound);
        assert_eq!(
            execute(&plan(vec![Target::Program(spec(&exe2))]), &exec(&f, &never)),
            "program_not_found"
        );
    }

    #[cfg(windows)]
    #[test]
    fn cancellation_and_on_error_decide_between_targets() {
        let d = Dir::new("order");
        let exe = program_in(&d.0, "a", "a.exe");
        let f = FakeLauncher::default();
        let gone = Target::Program(spec(&d.0.join("missing.exe")));
        let url = Target::Url("https://example.com/".into());
        let never = || false;
        let mut p = plan(vec![gone, url.clone()]);
        assert_eq!(execute(&p, &exec(&f, &never)), "program_not_found");
        assert!(f.calls().is_empty(), "stop: the URL did not open");
        p.stop_on_error = false;
        assert_eq!(execute(&p, &exec(&f, &never)), "program_not_found");
        assert_eq!(
            f.calls().len(),
            1,
            "continue: the URL opened, the first failure is reported"
        );
        let always = || true;
        let p = plan(vec![Target::Program(spec(&exe)), url]);
        assert_eq!(execute(&p, &exec(&f, &always)), "cancelled");
        assert_eq!(f.calls().len(), 1, "nothing more after a cancellation");
    }

    #[cfg(windows)]
    #[test]
    fn the_test_host_starts_only_its_own_programs_hidden_and_never_opens_urls() {
        let d = Dir::new("testhost");
        let inside = program_in(&d.0, "sandbox 한글", "marker.exe");
        let outside_dir = Dir::new("outside");
        let outside = program_in(&outside_dir.0, "x", "x.exe");
        let inner = std::sync::Arc::new(FakeLauncher::default());
        let host = TestHostLauncher::new(inner.clone(), Some(d.0.join("sandbox 한글")));
        let never = || false;
        assert_eq!(
            execute(
                &plan(vec![Target::Program(spec(&inside))]),
                &exec(&host, &never)
            ),
            "ok"
        );
        assert_eq!(
            execute(
                &plan(vec![Target::Program(spec(&outside))]),
                &exec(&host, &never)
            ),
            "launch_failed"
        );
        let calls = inner.calls();
        assert_eq!(calls.len(), 1);
        let Call::Spawn(s) = &calls[0] else { panic!() };
        assert_eq!(s.show, Show::Hidden, "always hidden");
        assert_eq!(
            execute(
                &plan(vec![Target::Url("https://example.com/x?y=1".into())]),
                &exec(&host, &never)
            ),
            "ok"
        );
        assert_eq!(inner.calls().len(), 1, "the shell is never called");
        let rec = std::fs::read_to_string(TestHostLauncher::record_file(&d.0.join("sandbox 한글")))
            .unwrap();
        let line: Value = serde_json::from_str(rec.trim()).unwrap();
        assert_eq!(
            line,
            json!({ "verb": "open", "target": "https://example.com/x?y=1", "show": "normal" })
        );
        // Without a root everything is refused.
        let none = TestHostLauncher::new(inner, None);
        assert_eq!(
            execute(
                &plan(vec![Target::Program(spec(&inside))]),
                &exec(&none, &never)
            ),
            "launch_failed"
        );
        assert_eq!(
            execute(
                &plan(vec![Target::Url("https://example.com/".into())]),
                &exec(&none, &never)
            ),
            "launch_failed"
        );
    }

    #[test]
    fn repeated_triggers_are_busy_then_rate_limited() {
        let mut g = Gate::default();
        let t0 = Instant::now();
        assert_eq!(g.admit("open-x", t0), Ok(()));
        assert_eq!(
            g.admit("open-x", t0 + Duration::from_millis(10)),
            Err("busy")
        );
        assert_eq!(g.admit("open-y", t0), Ok(()), "per rule");
        g.done("open-x");
        assert_eq!(
            g.admit("open-x", t0 + Duration::from_millis(300)),
            Err("rate_limited")
        );
        assert_eq!(
            g.admit("open-x", t0 + DEBOUNCE + Duration::from_millis(1)),
            Ok(())
        );
    }

    #[test]
    fn the_program_editor_checks_previews_and_saves_canonical_steps() {
        let mut d = ProgramDraft {
            title: "Open the tool".into(),
            path: r#""C:/Tools/My Tool/tool.exe""#.into(),
            args: vec!["--open".into(), "C:\\My Files\\".into()],
            window: "maximized".into(),
            activate: true,
            ..ProgramDraft::default()
        };
        assert!(d.problems().is_empty(), "{:?}", d.problems());
        assert_eq!(
            d.step(),
            json!({ "type": "open_program", "path": r"C:\Tools\My Tool\tool.exe",
                    "args": ["--open", "C:\\My Files\\"], "window": "maximized", "if_running": "activate" })
        );
        assert_eq!(
            d.command_line(&no_var).as_deref(),
            Some(r#""C:\Tools\My Tool\tool.exe" --open "C:\My Files\\""#)
        );
        assert_eq!(
            d.summary(),
            "Opens tool.exe with 2 arguments, maximized, or brings it to the front"
        );
        let pv = d.preview(&no_var);
        assert_eq!(pv.program, "tool.exe");
        assert_eq!(pv.to_json()["arguments"], 2);
        assert!(
            !pv.to_json().to_string().contains("Tools"),
            "never the path"
        );
        assert!(d.can_save().is_ok());
        d.saved = Some(Box::new(d.clone()));
        assert!(d.can_save().is_err(), "nothing changed");
        d.path = r"C:\Windows\System32\cmd.exe".into();
        assert_eq!(d.problems()[0].path, "steps[0].path");
        d.path = String::new();
        d.app_id = "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into();
        let p: Vec<&str> = d.problems().iter().map(|p| p.path).collect();
        assert_eq!(p, ["steps[0].app_id"], "an app takes no arguments");
        d.args.clear();
        assert!(d.problems().is_empty());
        assert_eq!(
            d.summary(),
            "Opens the app Microsoft.WindowsCalculator, maximized, or brings it to the front"
        );
        assert_eq!(
            d.step()["app_id"],
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        );
        d.env = vec![("K".into(), "v".into())];
        assert!(!d.problems().is_empty());
    }

    #[test]
    fn the_website_editor_normalizes_and_refuses_other_schemes() {
        let mut d = UrlDraft {
            title: "Kim".into(),
            input: "GitHub.com/hellomouv/kim".into(),
            saved: None,
        };
        assert_eq!(
            d.step(),
            Some(json!({ "type": "open_url", "url": "https://github.com/hellomouv/kim" }))
        );
        assert_eq!(d.summary(), "Opens github.com in your browser");
        assert_eq!(
            d.preview(),
            Some(json!({ "scheme": "https", "host": "github.com" }))
        );
        assert!(d.can_save().is_ok());
        d.input = "ms-settings:privacy".into();
        assert_eq!(d.problems()[0].path, "steps[0].url");
        assert!(d.step().is_none());
        assert!(d.can_save().is_err());
    }

    #[test]
    fn the_form_fields_are_fields_of_the_published_schema() {
        let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
        let defs = &schema["$defs"];
        for (fields, step) in [
            (OPEN_PROGRAM_FIELDS, "step_open_program"),
            (OPEN_URL_FIELDS, "step_open_url"),
        ] {
            for f in fields {
                let name = f.path.strip_prefix("steps[0].").unwrap();
                assert!(
                    defs[step]["properties"].get(name).is_some(),
                    "{} is not a schema field",
                    f.path
                );
            }
        }
    }
}
