//! `noeul config set|reset`: Noeul's settings from the command line (Step 51; NOEUL-FR-024,
//! NOEUL-FR-032, NOEUL-FR-102). `config show` lives in `instance` with the other reads.
//!
//! Both edit `config.toml` in place ([`noeul::settings::set`], [`noeul::settings::reset`]: only
//! the changed keys' lines, everything else kept), support `--dry-run`, and tell a running Noeul
//! to read the file again (it also notices the change by itself). The gesture preset is the one
//! setting that also changes shortcuts: `gestures.preset snipaste` writes the preset's gesture
//! values and its overlay bindings in `hotkeys.json` (`Enter` copies, `Ctrl+T` pins);
//! `gestures.preset noeul` and `config reset gestures` undo both. These commands never open a
//! window, never start the instance and never register a hotkey.

use crate::{Ctx, Outcome, confirm};
use clap::Args;
use noeul::error::{Code, NoeulError};
use noeul::paths::Paths;
use noeul::settings::{self, Preset};
use serde_json::{Value, json};

#[derive(Debug, Args)]
pub struct SetArgs {
    /// The setting: general.tray_icon, capture.keyboard_help, capture.quick_save_folder,
    /// capture.detection (window, element, off), capture.magnifier (on, off),
    /// gestures.double_click (pin, copy, save, none), gestures.middle_click (pin, copy, none) or
    /// gestures.preset (noeul, snipaste)
    pub key: String,
    /// The new value
    #[arg(allow_hyphen_values = true)]
    pub value: String,
    /// Show what would change; write nothing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Reset only this section: general, capture or gestures (default: every setting)
    pub section: Option<String>,
    /// Confirm without asking
    #[arg(long, short)]
    pub yes: bool,
    /// Show what would change; write nothing
    #[arg(long)]
    pub dry_run: bool,
}

/// Writes (unless a dry run), tells the instance, and renders the result.
fn finish(
    ctx: &Ctx,
    paths: &Paths,
    edited: &settings::Edited,
    hotkeys: Option<(Vec<Value>, bool)>,
    dry_run: bool,
) -> Result<Outcome, NoeulError> {
    let path = paths.config_file();
    let changed = edited.changes.iter().any(|c| c.before != c.after);
    let written = changed && !dry_run;
    if written {
        settings::write(&path, &edited.text)?;
    }
    let hotkeys_written = hotkeys.as_ref().is_some_and(|(_, w)| *w);
    let mut warnings = Vec::new();
    let instance = if written || hotkeys_written {
        if hotkeys_written && let Some(problem) = crate::instance::reload_hotkeys(ctx) {
            warnings.push(problem);
        }
        match crate::instance::reload_settings(ctx) {
            Ok(true) => "reloaded",
            Ok(false) => "not_running",
            Err(problem) => {
                warnings.push(problem);
                "failed"
            }
        }
    } else {
        "unchanged"
    };
    let mut human = String::new();
    for c in edited.changes.iter().filter(|c| c.before != c.after) {
        human.push_str(&format!(
            "{}: {} \u{2192} {}\n",
            c.key,
            c.before.as_deref().unwrap_or("(default)"),
            c.after.as_deref().unwrap_or("(default)")
        ));
    }
    if let Some((changes, _)) = &hotkeys {
        for c in changes {
            let side = |s: &Value| {
                let keys: Vec<&str> = s["bindings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                format!(
                    "{} ({})",
                    keys.join(", "),
                    s["source"].as_str().unwrap_or("")
                )
            };
            human.push_str(&format!(
                "shortcut {}: {} \u{2192} {}\n",
                c["action"].as_str().unwrap_or(""),
                side(&c["before"]),
                side(&c["after"])
            ));
        }
    }
    if human.is_empty() {
        human.push_str("Nothing to change.\n");
    }
    if dry_run {
        human.push_str("Dry run: nothing was written.\n");
    } else if written || hotkeys_written {
        human.push_str(&format!(
            "Saved to {}.{}\n",
            path.display(),
            if instance == "reloaded" {
                " The running Noeul uses it from the next capture."
            } else {
                ""
            }
        ));
    }
    Ok(Outcome {
        data: json!({
            "path": path.display().to_string(),
            "dry_run": dry_run,
            "written": written,
            "changes": edited.changes.iter().map(settings::Change::to_json).collect::<Vec<_>>(),
            "hotkeys": hotkeys.map(|(changes, written)| json!({
                "path": paths.hotkeys_file().display().to_string(),
                "written": written,
                "changes": changes,
            })),
            "instance": instance,
        }),
        human,
        warnings,
    })
}

/// `noeul config set <key> <value>`.
pub fn set(ctx: &Ctx, a: &SetArgs) -> Result<Outcome, NoeulError> {
    let paths = Paths::from_env()?;
    let path = paths.config_file();
    let (section, name) = settings::split_key(&a.key)?;
    if (section, name) == ("gestures", "preset") {
        let preset = Preset::parse(&a.value).ok_or_else(|| {
            NoeulError::new(
                Code::Usage,
                "`gestures.preset` must be \"noeul\" or \"snipaste\"",
            )
        })?;
        let g = preset.gestures();
        let edited = settings::set(
            &path,
            &[
                ("gestures.preset", preset.as_str()),
                ("gestures.double_click", g.double_click.as_str()),
                ("gestures.middle_click", g.middle_click.as_str()),
            ],
        )?;
        // The shortcuts first: a conflict refuses the whole preset before anything is written.
        let hotkeys = crate::hotkeys_cmd::apply_preset(&paths, preset, a.dry_run)?;
        return finish(ctx, &paths, &edited, Some(hotkeys), a.dry_run);
    }
    let edited = settings::set(&path, &[(&a.key, &a.value)])?;
    finish(ctx, &paths, &edited, None, a.dry_run)
}

/// `noeul config reset [<section>]`.
pub fn reset(ctx: &Ctx, a: &ResetArgs) -> Result<Outcome, NoeulError> {
    let paths = Paths::from_env()?;
    let path = paths.config_file();
    let edited = settings::reset(&path, a.section.as_deref())?;
    let changed = edited.changes.iter().any(|c| c.before != c.after);
    let gestures = a.section.as_deref().is_none_or(|s| s == "gestures");
    if changed && !a.dry_run && !a.yes {
        let what = a.section.as_deref().map_or_else(
            || "every setting".to_string(),
            |s| format!("the {s} settings"),
        );
        match confirm(ctx, &format!("Reset {what} to the defaults? [y/N]"), "y") {
            Some(true) => {}
            Some(false) => {
                return Err(NoeulError::new(
                    Code::Cancelled,
                    "reset cancelled; nothing was changed",
                ));
            }
            None => {
                return Err(NoeulError::new(
                    Code::ConfirmationRequired,
                    format!("resetting {what} needs confirmation"),
                )
                .hint("re-run with --yes (or --dry-run to preview); nothing was changed"));
            }
        }
    }
    // Resetting the gestures also undoes the preset's shortcuts (only while they are unchanged).
    let hotkeys = if gestures {
        Some(crate::hotkeys_cmd::apply_preset(
            &paths,
            Preset::Noeul,
            a.dry_run,
        )?)
    } else {
        None
    };
    finish(ctx, &paths, &edited, hotkeys, a.dry_run)
}
