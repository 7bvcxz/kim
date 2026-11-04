//! `subak config …` and `subak profile …` (Step 20, docs/SUBAK_CLI.md §3.13, ADR 0051): Subak's
//! settings file `config.toml` from the command line — the same settings the window's menu
//! changes (`KIM-FR-013`), with `--json`, `--dry-run` and the CLI contract's exit codes.
//!
//! Headless by construction (`KIM-FR-012`): these commands read and write
//! `%APPDATA%\mouv\subak\config.toml` (and its `backups\`) only. They never contact or start a
//! Host, never open a window, and never change a running Terminal: the window applies its
//! appearance when it next gets the focus, and a profile is used only when a Terminal is
//! created. Secret-looking profile values are never printed (`***`) and never written.

use crate::cli::{Args, Cmd};
use crate::config::{
    self, Change, EditError, Edited, ImportError, Loaded, Problem, Reset, Settings, State,
};
use crate::output::{CliError, Code};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What a command produced: the `--json` data, the human text and warnings.
#[derive(Debug)]
pub struct Outcome {
    pub data: Value,
    pub human: String,
    pub warnings: Vec<String>,
}

fn dir() -> Result<PathBuf, CliError> {
    config::config_dir().ok_or_else(|| {
        CliError::new(
            Code::Io,
            if cfg!(windows) {
                "%APPDATA% is not set, so Subak's settings file has no place"
            } else {
                "$XDG_CONFIG_HOME is not set, so Subak's settings file has no place"
            },
        )
    })
}

/// Runs a `config` or `profile` command.
pub fn run(cmd: Cmd, a: &Args) -> Result<Outcome, CliError> {
    let dir = dir()?;
    match cmd {
        Cmd::ConfigShow => show(&dir),
        Cmd::ConfigCheck => check(&dir),
        Cmd::ConfigSet => set(&dir, a),
        Cmd::ConfigReset => reset(&dir, a),
        Cmd::ConfigExport => export(&dir),
        Cmd::ConfigImport => import(&dir, a),
        Cmd::ProfileList => profiles(&dir),
        Cmd::ProfileShow => profile(&dir, a),
        _ => Err(CliError::new(Code::Internal, "not a config command")),
    }
}

fn problems_json(p: &[Problem]) -> Value {
    Value::Array(p.iter().map(Problem::to_json).collect())
}

/// Warnings for the problems of a file (each names the key and its line).
fn problem_warnings(l: &Loaded) -> Vec<String> {
    l.parsed.problems.iter().map(Problem::text).collect()
}

fn path_text(p: &Path) -> String {
    p.display().to_string()
}

/// The error for a file an edit must not rewrite.
fn not_editable(l: &Loaded, e: &EditError) -> CliError {
    let details: Vec<String> = l.parsed.problems.iter().map(Problem::text).collect();
    let err = match l.state() {
        State::Newer => CliError::new(Code::ConfigFromNewerVersion, e.message())
            .hint("use the newer Subak to change settings, or import a file this one reads"),
        _ => CliError::new(Code::ConfigInvalid, e.message()).hint(
            "fix the file by hand (`subak config check` names every problem), or `subak config reset --all --yes` (keeps a backup)",
        ),
    };
    details.into_iter().fold(err, CliError::detail)
}

fn edit_error(l: &Loaded, e: &EditError) -> CliError {
    match e {
        EditError::UnknownKey(_) => CliError::new(Code::Usage, e.message())
            .hint(format!("settings: {}", config::key_ids().join(", "))),
        EditError::BadValue(..) => CliError::new(Code::Usage, e.message()),
        EditError::Layout(_) => CliError::new(Code::ConfigInvalid, e.message()).hint(format!(
            "edit {} by hand: keys go under [{}]",
            config::FILE,
            config::SECTIONS.join("] and [")
        )),
        EditError::NotEditable(..) => not_editable(l, e),
    }
}

fn changes_json(c: &[Change]) -> Value {
    Value::Array(c.iter().map(Change::to_json).collect())
}

fn changes_text(c: &[Change]) -> String {
    let mut out = String::new();
    for ch in c {
        out.push_str(&format!(
            "  {}: {} → {}\n",
            ch.key,
            ch.before.as_deref().unwrap_or("(default)"),
            ch.after.as_deref().unwrap_or("(default)")
        ));
    }
    out
}

/// Writes an edit (unless `--dry-run`), and reports it.
fn finish(
    l: &Loaded,
    edited: Edited,
    a: &Args,
    backup: Option<PathBuf>,
    verb: &str,
) -> Result<Outcome, CliError> {
    let write = !a.dry_run && !edited.changes.is_empty();
    if write {
        config::write_atomic(&l.path, &edited.text).map_err(|e| {
            CliError::new(
                Code::Io,
                format!("cannot write {}: {e}", path_text(&l.path)),
            )
        })?;
    }
    let after = config::parse(&edited.text);
    let mut warnings: Vec<String> = after.problems.iter().map(Problem::text).collect();
    if write {
        warnings.push(
            "a running Subak window applies the change when it next gets the focus; running Terminals are not changed"
                .into(),
        );
    }
    let mut human = if edited.changes.is_empty() {
        format!("Nothing to change in {}.\n", path_text(&l.path))
    } else if a.dry_run {
        format!(
            "Would {verb} in {} (dry run; nothing written):\n",
            path_text(&l.path)
        )
    } else {
        format!("{} {}:\n", capitalized(verb), path_text(&l.path))
    };
    human.push_str(&changes_text(&edited.changes));
    if let Some(b) = &backup {
        human.push_str(&format!("Backup: {}\n", path_text(b)));
    }
    Ok(Outcome {
        data: json!({
            "path": path_text(&l.path),
            "changes": changes_json(&edited.changes),
            "dry_run": a.dry_run,
            "written": write,
            "backup": backup.as_deref().map(path_text),
        }),
        human,
        warnings,
    })
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// The source of a key's value: `file` when the file sets it validly, else `default`.
fn source(l: &Loaded, id: &str) -> &'static str {
    let bad = l
        .parsed
        .problems
        .iter()
        .any(|p| p.key == id && p.severity == config::Severity::Error);
    if l.parsed.set_keys.iter().any(|k| k == id) && !bad {
        "file"
    } else {
        "default"
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::String(s) if s.is_empty() => "\"\"".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `subak config show`: every setting with its value, default and source, and the profiles.
fn show(dir: &Path) -> Result<Outcome, CliError> {
    let l = config::load(dir);
    let s = l.settings();
    let defaults = Settings::default();
    let mut rows = Vec::new();
    let mut human = format!(
        "{} ({})\n",
        path_text(&l.path),
        match l.state() {
            State::Missing => "no file: every setting is its default",
            State::User => "read",
            State::Invalid => "INVALID: the defaults apply and the file is left as it is",
            State::Newer => "written by a newer Subak: the defaults apply, read-only",
            State::Unreadable => "unreadable: the defaults apply",
        }
    );
    for k in config::KEYS {
        let value = s.value(k);
        let src = source(&l, &k.id());
        human.push_str(&format!(
            "  {:<28} {}{}\n",
            k.id(),
            shown(&value),
            if src == "default" { "  (default)" } else { "" }
        ));
        rows.push(json!({
            "key": k.id(),
            "value": value,
            "default": defaults.value(k),
            "source": src,
            "takes": k.takes,
        }));
    }
    human.push_str(&profiles_text(s));
    if !l.parsed.unknown.is_empty() {
        human.push_str(&format!(
            "Unknown keys (kept): {}\n",
            l.parsed.unknown.join(", ")
        ));
    }
    Ok(Outcome {
        data: json!({
            "path": path_text(&l.path),
            "state": l.state().as_str(),
            "config_version": config::VERSION,
            "settings": rows,
            "profiles": s.profiles.iter().map(|p| p.to_json(s.default_profile().is_some_and(|d| d.name == p.name))).collect::<Vec<_>>(),
            "default_profile": s.default_profile().map(|p| p.name.clone()),
            "problems": problems_json(&l.parsed.problems),
            "unknown": l.parsed.unknown,
            "backups": path_text(&dir.join(config::BACKUP_DIR)),
        }),
        warnings: problem_warnings(&l),
        human,
    })
}

fn profiles_text(s: &Settings) -> String {
    if s.profiles.is_empty() {
        return "Profiles: none (new Terminals run the Host's default shell)\n".into();
    }
    let mut out = String::from("Profiles:\n");
    for p in &s.profiles {
        let j = p.to_json(false);
        let default = s.default_profile().is_some_and(|d| d.name == p.name);
        let args: Vec<String> = j["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "  {}{}: {}{}{}\n",
            p.name,
            if default { " (default)" } else { "" },
            j["program"].as_str().unwrap_or(""),
            if args.is_empty() {
                String::new()
            } else {
                format!(" {}", args.join(" "))
            },
            p.cwd
                .as_deref()
                .map(|c| format!("  in {c}"))
                .unwrap_or_default()
        ));
        if let Some(env) = j["env"].as_object() {
            for (n, v) in env {
                out.push_str(&format!("      env {n}={}\n", v.as_str().unwrap_or("")));
            }
        }
        for n in &p.unset_env {
            out.push_str(&format!("      unset {n}\n"));
        }
    }
    out
}

/// `subak config check`: the file's state and every problem with its key and line. Exits 0;
/// `valid` says whether every value was used.
fn check(dir: &Path) -> Result<Outcome, CliError> {
    let l = config::load(dir);
    let errors = l
        .parsed
        .problems
        .iter()
        .filter(|p| p.severity == config::Severity::Error)
        .count();
    let valid = matches!(l.state(), State::Missing | State::User) && errors == 0;
    let mut human = format!(
        "{}: {}\n",
        path_text(&l.path),
        match (l.state(), valid) {
            (State::Missing, _) => "no file (every setting is its default)".to_string(),
            (State::User, true) if l.parsed.problems.is_empty() => "valid".to_string(),
            (State::User, true) => format!("valid, {} warning(s)", l.parsed.problems.len()),
            (State::User, false) =>
                format!("{errors} invalid value(s) fell back to their defaults"),
            (s, _) => format!("{} (the defaults apply)", s.as_str()),
        }
    );
    for p in &l.parsed.problems {
        human.push_str(&format!("  {}\n", p.text()));
    }
    if !l.parsed.unknown.is_empty() {
        human.push_str(&format!(
            "  unknown keys (kept as they are): {}\n",
            l.parsed.unknown.join(", ")
        ));
    }
    Ok(Outcome {
        data: json!({
            "path": path_text(&l.path),
            "state": l.state().as_str(),
            "valid": valid,
            "problems": problems_json(&l.parsed.problems),
            "unknown": l.parsed.unknown,
            "profiles": l.settings().profiles.len(),
        }),
        human,
        warnings: Vec::new(),
    })
}

/// `subak config set <key> <value>` (or `<key>=<value>`).
fn set(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let (key, value) = match a.positional.as_slice() {
        [kv] => kv.split_once('=').ok_or_else(|| {
            CliError::usage("`config set` takes <key> <value> or <key>=<value>")
                .hint("e.g. subak config set appearance.theme kim-light")
        })?,
        [k, v] => (k.as_str(), v.as_str()),
        _ => return Err(CliError::usage("`config set` takes <key> <value>")),
    };
    let l = config::load(dir);
    let edited = config::set(&l, &[(key, value)]).map_err(|e| edit_error(&l, &e))?;
    finish(&l, edited, a, None, "set")
}

/// `subak config reset (<key>|<section>|--all)`: the keys go back to their defaults (their
/// lines are removed); profiles, unknown keys and comments stay. `--all` asks (or needs
/// `--yes`) and backs the file up first; it also replaces an invalid file (kept as a backup).
fn reset(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let what = match (a.positional.first(), a.all) {
        (Some(_), true) | (None, false) => {
            return Err(
                CliError::usage("give exactly one of <key>, <section> or --all").hint(format!(
                    "sections: {}; keys: subak config show",
                    config::SECTIONS.join(", ")
                )),
            );
        }
        (w, _) => w.map(String::as_str),
    };
    let l = config::load(dir);
    let what = Reset::parse(what).map_err(|e| edit_error(&l, &e))?;
    let edited = config::reset(&l, what).map_err(|e| edit_error(&l, &e))?;
    let mut backup = None;
    if what == Reset::All && !a.dry_run && !edited.changes.is_empty() {
        crate::cli::confirm(
            a,
            &format!("Reset every setting in {}?", path_text(&l.path)),
            "`config reset --all` resets every setting",
            "add --yes to confirm (a backup is kept), or --dry-run to see the changes",
        )?;
        let tag = if l.state().editable() { "" } else { "invalid" };
        backup = config::backup(dir, config::FILE, tag).map_err(|e| {
            CliError::new(
                Code::Io,
                format!(
                    "cannot back up {}: {e}; nothing was changed",
                    path_text(&l.path)
                ),
            )
        })?;
    }
    finish(&l, edited, a, backup, "reset")
}

/// `subak config export`: the file (or, without one, the defaults' document) with every
/// secret-looking value replaced by `***`, on stdout.
fn export(dir: &Path) -> Result<Outcome, CliError> {
    let l = config::load(dir);
    let (text, source) = match (&l.text, l.state()) {
        (Some(t), _) => (t.clone(), "file"),
        (None, State::Unreadable) => {
            return Err(
                CliError::new(Code::Io, format!("cannot read {}", path_text(&l.path)))
                    .detail(l.notice().unwrap_or_default()),
            );
        }
        (None, _) => (config::new_file_text(), "defaults"),
    };
    let (text, redacted) = config::redacted_text(&text, l.settings());
    let mut warnings = Vec::new();
    if redacted > 0 {
        warnings.push(format!(
            "{redacted} secret-looking value(s) were replaced by {}; secrets do not belong in {}",
            crate::redact::MASK,
            config::FILE
        ));
    }
    Ok(Outcome {
        data: json!({
            "path": path_text(&l.path),
            "source": source,
            "text": text,
            "redacted": redacted,
        }),
        human: text,
        warnings,
    })
}

/// `subak config import <file>`: replaces `config.toml` with a checked document — it must be
/// valid, of this version or older, and hold no secret-looking value. The current file is
/// backed up first (an invalid one too); a newer one is never replaced.
fn import(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let from = PathBuf::from(a.positional.first().map_or("", String::as_str));
    let meta = std::fs::metadata(&from)
        .map_err(|e| CliError::new(Code::Io, format!("cannot read {}: {e}", path_text(&from))))?;
    if meta.len() > config::MAX_BYTES || meta.is_dir() {
        return Err(CliError::new(
            Code::ConfigInvalid,
            format!(
                "{} is not a settings file (a folder, or larger than {} bytes); nothing was changed",
                path_text(&from),
                config::MAX_BYTES
            ),
        ));
    }
    let text = std::fs::read_to_string(&from).map_err(|e| {
        CliError::new(
            Code::ConfigInvalid,
            format!(
                "cannot read {} as UTF-8 text: {e}; nothing was changed",
                path_text(&from)
            ),
        )
    })?;
    let incoming = match config::check_import(&text) {
        Ok(p) => p,
        Err(ImportError::Newer(p)) => {
            return Err(p.iter().fold(
                CliError::new(
                    Code::ConfigFromNewerVersion,
                    format!(
                        "{} was written by a newer Subak; nothing was changed",
                        path_text(&from)
                    ),
                ),
                |e, p| e.detail(p.text()),
            ));
        }
        Err(ImportError::Invalid(p)) => {
            return Err(p.iter().fold(
                CliError::new(
                    Code::ConfigInvalid,
                    format!("{} is not a valid Subak settings file; nothing was changed", path_text(&from)),
                )
                .hint("`subak config check` explains each problem once the file is in place; fix them first"),
                |e, p| e.detail(p.text()),
            ));
        }
        Err(ImportError::Secrets(names)) => {
            return Err(names.iter().fold(
                CliError::new(
                    Code::SecretInConfig,
                    format!(
                        "{} holds secret-looking profile values; Subak never writes secrets into {}, so nothing was changed",
                        path_text(&from),
                        config::FILE
                    ),
                )
                .hint("remove them (programs can read secrets from your own environment); `***` values from an export count too"),
                |e, n| e.detail(format!("profile value `{n}`")),
            ));
        }
    };
    let l = config::load(dir);
    if l.state() == State::Newer {
        return Err(not_editable(
            &l,
            &EditError::NotEditable(State::Newer, Vec::new()),
        ));
    }
    let changes = config::differences(l.settings(), &incoming.settings);
    let edited = Edited {
        text: if text.ends_with('\n') {
            text.clone()
        } else {
            format!("{text}\n")
        },
        changes: if changes.is_empty() && l.text.as_deref() != Some(text.as_str()) {
            vec![Change {
                key: "*".into(),
                before: Some("(the same settings)".into()),
                after: Some("(the imported text)".into()),
            }]
        } else {
            changes
        },
    };
    let mut backup = None;
    if !a.dry_run && !edited.changes.is_empty() {
        let tag = if l.state().editable() { "" } else { "invalid" };
        backup = config::backup(dir, config::FILE, tag).map_err(|e| {
            CliError::new(
                Code::Io,
                format!(
                    "cannot back up {}: {e}; nothing was changed",
                    path_text(&l.path)
                ),
            )
        })?;
    }
    let mut out = finish(&l, edited, a, backup, "import")?;
    out.data["from"] = json!(path_text(&from));
    Ok(out)
}

/// `subak profile list`: every profile, values redacted where they look secret.
fn profiles(dir: &Path) -> Result<Outcome, CliError> {
    let l = config::load(dir);
    let s = l.settings();
    Ok(Outcome {
        data: json!({
            "path": path_text(&l.path),
            "state": l.state().as_str(),
            "default": s.default_profile().map(|p| p.name.clone()),
            "profiles": s.profiles.iter().map(|p| p.to_json(s.default_profile().is_some_and(|d| d.name == p.name))).collect::<Vec<_>>(),
        }),
        human: profiles_text(s),
        warnings: problem_warnings(&l),
    })
}

/// `subak profile show <name>`.
fn profile(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let l = config::load(dir);
    let s = l.settings();
    let name = a.positional.first().map_or("", String::as_str);
    let p = find_profile(&l, name)?;
    let default = s.default_profile().is_some_and(|d| d.name == p.name);
    let one = Settings {
        profiles: vec![p.clone()],
        terminal: config::TerminalPrefs {
            default_profile: if default {
                p.name.clone()
            } else {
                String::new()
            },
            ..s.terminal.clone()
        },
        appearance: s.appearance.clone(),
    };
    Ok(Outcome {
        data: json!({"path": path_text(&l.path), "profile": p.to_json(default)}),
        human: profiles_text(&one),
        warnings: problem_warnings(&l),
    })
}

/// The profile `name` of a usable file (`profile_not_found`, `config_invalid`, …).
pub fn find_profile<'a>(l: &'a Loaded, name: &str) -> Result<&'a config::Profile, CliError> {
    match l.state() {
        State::Missing | State::User => {}
        State::Newer => {
            return Err(CliError::new(
                Code::ConfigFromNewerVersion,
                format!(
                    "{} was written by a newer Subak; its profiles are not used",
                    path_text(&l.path)
                ),
            ));
        }
        _ => {
            return Err(problem_details(
                CliError::new(
                    Code::ConfigInvalid,
                    format!(
                        "{} is invalid; its profiles are not used",
                        path_text(&l.path)
                    ),
                )
                .hint("`subak config check` names the problem"),
                l,
            ));
        }
    }
    l.settings().profile(name).ok_or_else(|| {
        let known: Vec<&str> = l
            .settings()
            .profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        let err = CliError::new(
            Code::ProfileNotFound,
            format!("no profile is named `{name}`"),
        )
        .hint(if known.is_empty() {
            format!(
                "{} defines no profiles; add a [[profiles]] table (docs/SUBAK_GUI.md §13)",
                path_text(&l.path)
            )
        } else {
            format!("profiles: {}", known.join(", "))
        });
        problem_details(err, l)
    })
}

fn problem_details(e: CliError, l: &Loaded) -> CliError {
    l.parsed
        .problems
        .iter()
        .filter(|p| p.key.starts_with("profiles") || p.key.is_empty())
        .fold(e, |e, p| e.detail(p.text()))
}

/// The profile `subak new` uses: `--profile <name>`, else (without a program, `--command-line`
/// or `--no-profile`) the default profile. Returns it, or a warning when the default profile
/// cannot be used (the Host's default shell runs then).
pub fn profile_for_new(a: &Args) -> Result<(Option<config::Profile>, Vec<String>), CliError> {
    let Some(dir) = config::config_dir() else {
        return match &a.profile {
            Some(_) => Err(dir()
                .err()
                .unwrap_or_else(|| CliError::new(Code::Io, "no settings folder"))),
            None => Ok((None, Vec::new())),
        };
    };
    let l = config::load(&dir);
    if let Some(name) = &a.profile {
        return Ok((Some(find_profile(&l, name)?.clone()), Vec::new()));
    }
    if a.no_profile || !a.command.is_empty() || a.command_line.is_some() {
        return Ok((None, Vec::new()));
    }
    let d = l.settings().terminal.default_profile.trim().to_string();
    if d.is_empty() {
        return Ok((None, Vec::new()));
    }
    match l.settings().default_profile() {
        Some(p) => Ok((Some(p.clone()), Vec::new())),
        None => Ok((
            None,
            vec![format!(
                "the default profile `{d}` is not usable ({}); the Host's default shell runs",
                l.notice().unwrap_or_else(|| "no such profile".into())
            )],
        )),
    }
}
