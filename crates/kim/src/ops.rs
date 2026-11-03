//! CLI handlers for explicit, state-changing operations: install, update, uninstall, settings.
//! Each supports `--dry-run` (plan only, no writes, no downloads) and none of them prompts when
//! Kim cannot ask (docs/CLI_CONTRACT.md §4).

use crate::{ChannelOpt, Ctx, Outcome, diag, load, trusted_keys};
use clap::{Args, Subcommand};
use kim::catalog::Channel;
use kim::catalog::store;
use kim::error::{ErrorCode, KimError};
use kim::fetch::DefaultFetcher;
use kim::install::{self, Event, Phase, Plan, PlanOptions};
use kim::paths::Paths;
use kim::platform::Platform;
use kim::settings::Settings;
use serde_json::json;
use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Tool id, e.g. `subak`
    tool: String,
    /// Install this exact version instead of the newest compatible one
    #[arg(long)]
    version: Option<String>,
    /// Release channel to pick from: stable, or preview (stable + preview)
    #[arg(long, value_enum, default_value_t = ChannelOpt::Stable)]
    channel: ChannelOpt,
    /// Show what would happen; download and change nothing
    #[arg(long)]
    dry_run: bool,
    /// Reinstall the same version (repair)
    #[arg(long)]
    reinstall: bool,
    /// Accept a catalog past its expiry date
    #[arg(long)]
    allow_stale: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// `check` to list available updates, or a tool id to update it
    target: String,
    /// With `check`: refresh the catalog first (explicit network access)
    #[arg(long)]
    refresh: bool,
    /// With `check --refresh`: catalog source URL (https://, http://localhost or file://)
    #[arg(long, default_value = store::DEFAULT_SOURCE, requires = "refresh")]
    source: String,
    /// Release channel: stable, or preview (stable + preview)
    #[arg(long, value_enum, default_value_t = ChannelOpt::Stable)]
    channel: ChannelOpt,
    /// Show what would happen; download and change nothing (with `check --refresh`: fetch and
    /// verify the catalog, but do not update the cache)
    #[arg(long)]
    dry_run: bool,
    /// Accept a catalog past its expiry date
    #[arg(long)]
    allow_stale: bool,
}

impl UpdateArgs {
    pub fn is_check(&self) -> bool {
        self.target == "check"
    }
}

#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// Tool id, e.g. `subak`
    tool: String,
    /// Also delete the tool's user data (configuration, state). Needs separate confirmation.
    #[arg(long)]
    purge_data: bool,
    /// Confirm without prompting (required when Kim cannot ask: --json, --no-input, no terminal)
    #[arg(long, short)]
    yes: bool,
    /// Show what would be removed; change nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Show all settings and where they are stored
    Show,
    /// Change a setting: check_updates_on_launch | offline_mode  (true/false)
    Set(SetArgs),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Setting name: check_updates_on_launch or offline_mode
    key: String,
    /// true or false
    value: String,
    /// Show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

pub fn load_settings() -> (Settings, Option<String>) {
    match Settings::path() {
        Ok(p) => Settings::load_from(&p),
        Err(e) => (Settings::default(), Some(e.message)),
    }
}

/// Process-wide cancellation flag, set by Ctrl+C.
fn cancel_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let _ = ctrlc::set_handler(move || f.store(true, Ordering::Relaxed));
    flag
}

fn human_size(b: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mb = b as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

/// Renders progress on stderr (human mode only).
struct ProgressPrinter {
    tty: bool,
    quiet: bool,
    last_pct: u64,
}

impl ProgressPrinter {
    fn new(quiet: bool) -> Self {
        Self {
            tty: std::io::stderr().is_terminal(),
            quiet,
            last_pct: u64::MAX,
        }
    }

    fn event(&mut self, e: Event) {
        if self.quiet {
            return;
        }
        let mut err = std::io::stderr().lock();
        match e {
            Event::Phase { phase } => {
                let label = match phase {
                    Phase::Locking => return,
                    Phase::Downloading => "Downloading",
                    Phase::Verifying => "Verifying SHA-256 against the signed catalog",
                    Phase::Unpacking => "Unpacking",
                    Phase::SelfCheck => "Running self-check",
                    Phase::Activating => "Activating",
                    Phase::CleaningUp => "Cleaning up",
                    Phase::Done => "Done",
                };
                if self.tty && self.last_pct != u64::MAX {
                    let _ = writeln!(err);
                }
                self.last_pct = u64::MAX;
                let _ = writeln!(err, "• {label}");
            }
            Event::Progress { done, total } => {
                let pct = (done * 100).checked_div(total).unwrap_or(100);
                let step = if self.tty { 1 } else { 25 };
                if self.last_pct != u64::MAX && pct / step == self.last_pct / step {
                    return;
                }
                self.last_pct = pct;
                if self.tty {
                    let _ = write!(
                        err,
                        "\r  {} / {} ({pct}%)   ",
                        human_size(done),
                        human_size(total)
                    );
                } else {
                    let _ = writeln!(
                        err,
                        "  {} / {} ({pct}%)",
                        human_size(done),
                        human_size(total)
                    );
                }
            }
        }
    }
}

fn plan_text(plan: &Plan) -> String {
    let what = match plan.action {
        install::Action::Install => format!("install {} {}", plan.tool, plan.to),
        install::Action::Update => format!(
            "update {} {} → {}",
            plan.tool,
            plan.from.as_deref().unwrap_or("?"),
            plan.to
        ),
        install::Action::Downgrade => format!(
            "downgrade {} {} → {}",
            plan.tool,
            plan.from.as_deref().unwrap_or("?"),
            plan.to
        ),
        install::Action::Reinstall => format!("reinstall {} {}", plan.tool, plan.to),
    };
    let mut s = format!(
        "Plan: {what}\n  package  {} ({}, SHA-256 {}…)\n  into     {}\n",
        plan.asset.url,
        human_size(plan.asset.size),
        &plan.asset.sha256[..12],
        plan.tool_dir.display()
    );
    for n in &plan.notes {
        s.push_str(&format!("  note     {n}\n"));
    }
    s
}

fn run_plan(plan: &Plan, paths: &Paths, ctx: Ctx) -> Result<Outcome, KimError> {
    let cancel = cancel_flag();
    let mut printer = ProgressPrinter::new(ctx.json || ctx.quiet);
    if !ctx.json && !ctx.quiet {
        eprint!("{}", kim::redact::text(&plan_text(plan)));
    }
    let out = install::execute(
        plan,
        paths,
        &DefaultFetcher::default(),
        &mut |e| printer.event(e),
        &cancel,
    )?;
    let mut human = format!(
        "{} {} is installed at {}\n",
        plan.tool,
        out.to,
        out.entrypoint.display()
    );
    if let Some(from) = &out.from {
        human.push_str(&format!("Previous version {from} is kept for rollback.\n"));
    }
    if !out.running.is_empty() {
        human.push_str(&format!(
            "{} program(s) of the old version are still running and were not stopped; the new version is used the next time {} starts.\n",
            out.running.len(),
            plan.tool
        ));
    }
    if !out.kept_in_use.is_empty() {
        human.push_str(&format!(
            "Old version(s) {} are still in use and will be removed by a later operation.\n",
            out.kept_in_use.join(", ")
        ));
    }
    Ok(Outcome {
        data: json!({ "plan": plan, "dry_run": false, "result": out }),
        human,
        warnings: Vec::new(),
    })
}

fn options(
    version: Option<String>,
    channel: ChannelOpt,
    allow_stale: bool,
    reinstall: bool,
) -> PlanOptions {
    PlanOptions {
        version,
        channel: Some(Channel::from(channel)),
        allow_stale,
        reinstall,
    }
}

fn dry_run_outcome(plan: &Plan, warnings: Vec<String>) -> Outcome {
    Outcome {
        data: json!({ "plan": plan, "dry_run": true, "result": null }),
        human: format!(
            "{}Dry run: nothing was downloaded or changed.\n",
            plan_text(plan)
        ),
        warnings,
    }
}

pub fn install(a: &InstallArgs, ctx: Ctx) -> Result<Outcome, KimError> {
    let (settings, warn) = load_settings();
    let (paths, loaded) = load()?;
    let plan = install::plan_install(
        &loaded,
        &paths,
        &Platform::current(),
        &a.tool,
        &options(a.version.clone(), a.channel, a.allow_stale, a.reinstall),
    )?;
    let warnings: Vec<String> = loaded.warnings.iter().cloned().chain(warn).collect();
    if a.dry_run {
        return Ok(dry_run_outcome(&plan, warnings));
    }
    settings.ensure_online()?;
    let mut out = run_plan(&plan, &paths, ctx)?;
    out.warnings = warnings;
    Ok(out)
}

pub fn update(a: &UpdateArgs, ctx: Ctx) -> Result<Outcome, KimError> {
    let (settings, warn) = load_settings();
    if a.is_check() {
        return update_check(a, &settings, warn);
    }
    let (paths, loaded) = load()?;
    let plan = install::plan_update(
        &loaded,
        &paths,
        &Platform::current(),
        &a.target,
        &options(None, a.channel, a.allow_stale, false),
    )?;
    let warnings: Vec<String> = loaded.warnings.iter().cloned().chain(warn).collect();
    if a.dry_run {
        return Ok(dry_run_outcome(&plan, warnings));
    }
    settings.ensure_online()?;
    let mut out = run_plan(&plan, &paths, ctx)?;
    out.warnings = warnings;
    Ok(out)
}

/// `kim update check [--refresh] [--dry-run]`: shows updates; never downloads a package.
fn update_check(
    a: &UpdateArgs,
    settings: &Settings,
    warn: Option<String>,
) -> Result<Outcome, KimError> {
    let (paths, mut loaded) = load()?;
    let mut refresh = None;
    if a.refresh {
        settings.ensure_online()?;
        let now = kim::time::now_utc();
        diag(format!("fetching {} and {}.sig", a.source, a.source));
        let fetched = store::fetch_verified(
            &paths,
            &trusted_keys(),
            now,
            &a.source,
            &DefaultFetcher::default(),
        )?;
        if !a.dry_run {
            store::commit(&paths, &fetched)?;
        }
        loaded = fetched.effective(loaded, now);
        refresh = Some(fetched.outcome);
    }
    let ups = install::check_updates(
        &loaded,
        &paths,
        &Platform::current(),
        Channel::from(a.channel),
    );
    let mut human = crate::catalog_header(&loaded);
    if ups.is_empty() {
        human.push_str("All installed tools are up to date.\n");
    } else {
        for u in &ups {
            human.push_str(&format!(
                "{}: {} → {}   (run `kim update {}` to install)\n",
                u.tool, u.installed, u.available, u.tool
            ));
        }
    }
    if a.refresh && a.dry_run {
        human.push_str("Dry run: the refreshed catalog was verified but not saved.\n");
    }
    human.push_str("Checking never downloads or installs anything.\n");
    Ok(Outcome {
        data: json!({
            "updates": ups,
            "refreshed": a.refresh,
            "refresh": refresh,
            "dry_run": a.dry_run,
        }),
        human,
        warnings: loaded.warnings.into_iter().chain(warn).collect(),
    })
}

/// Asks a yes/no question on the terminal; `None` when Kim may not ask (see [`Ctx::can_prompt`]).
pub(crate) fn confirm(ctx: Ctx, question: &str, expected: &str) -> Option<bool> {
    if !ctx.can_prompt() {
        return None;
    }
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line.trim().eq_ignore_ascii_case(expected))
}

pub fn uninstall(a: &UninstallArgs, ctx: Ctx) -> Result<Outcome, KimError> {
    let paths = Paths::from_env()?;
    let plan = install::plan_uninstall(&paths, &a.tool, a.purge_data)?;
    let mut text = format!(
        "Plan: uninstall {} {}\n  remove   {}\n",
        plan.tool,
        plan.version.as_deref().unwrap_or("(unknown version)"),
        plan.tool_dir.display()
    );
    for d in &plan.data_dirs {
        text.push_str(&format!(
            "  {}  {}\n",
            if plan.purge_data {
                "DELETE  "
            } else {
                "keep    "
            },
            d.display()
        ));
    }
    if !plan.running.is_empty() {
        let pids: Vec<String> = plan.running.iter().map(|p| p.pid.to_string()).collect();
        text.push_str(&format!(
            "  running  PID {} — uninstall will be refused; Kim never stops programs\n",
            pids.join(", ")
        ));
    }
    if a.dry_run {
        return Ok(Outcome {
            data: json!({ "plan": plan, "dry_run": true, "result": null }),
            human: format!("{text}Dry run: nothing was changed.\n"),
            warnings: Vec::new(),
        });
    }
    if !a.yes {
        if !ctx.can_prompt() {
            return Err(confirmation_needed(&a.tool));
        }
        eprint!("{text}");
        match confirm(ctx, &format!("Uninstall {}? [y/N]", a.tool), "y") {
            Some(true) => {}
            Some(false) => {
                return Err(KimError::new(
                    ErrorCode::Cancelled,
                    "uninstall cancelled; nothing was changed",
                ));
            }
            None => return Err(confirmation_needed(&a.tool)),
        }
        if a.purge_data && !plan.data_dirs.is_empty() {
            match confirm(
                ctx,
                &format!(
                    "Also DELETE {}'s user data permanently? Type `delete` to confirm:",
                    a.tool
                ),
                "delete",
            ) {
                Some(true) => {}
                _ => {
                    return Err(KimError::new(
                        ErrorCode::Cancelled,
                        "data deletion not confirmed; nothing was changed",
                    ));
                }
            }
        }
    }
    let out = install::execute_uninstall(&plan, &paths)?;
    let mut human = format!("{} was uninstalled.\n", out.tool);
    for d in &out.kept_data {
        human.push_str(&format!(
            "User data kept: {} (use --purge-data to delete)\n",
            d.display()
        ));
    }
    for d in &out.purged_data {
        human.push_str(&format!("User data deleted: {}\n", d.display()));
    }
    let warnings = out
        .leftovers
        .iter()
        .map(|l| format!("could not remove {l}"))
        .collect();
    Ok(Outcome {
        data: json!({ "plan": plan, "dry_run": false, "result": out }),
        human,
        warnings,
    })
}

fn confirmation_needed(tool: &str) -> KimError {
    KimError::new(
        ErrorCode::ConfirmationRequired,
        format!("uninstalling {tool} needs confirmation"),
    )
    .with_hint("re-run with --yes (or --dry-run to preview); nothing was changed")
}

pub fn settings(c: &SettingsCommand) -> Result<Outcome, KimError> {
    let path = Settings::path()?;
    let (current, warn) = Settings::load_from(&path);
    let describe = |s: &Settings| {
        format!(
            "check_updates_on_launch = {}   (Kim Manager may refresh the catalog at start to *show* updates)\noffline_mode            = {}   (refuse all network operations)\nfile: {}\n",
            s.check_updates_on_launch,
            s.offline_mode,
            path.display()
        )
    };
    match c {
        SettingsCommand::Show => Ok(Outcome {
            data: json!({ "settings": current, "path": path }),
            human: describe(&current),
            warnings: warn.into_iter().collect(),
        }),
        SettingsCommand::Set(a) => {
            // Never change a setting based on defaults that only stand in for a file Kim
            // cannot read: that could silently undo the user's choice (e.g. offline_mode).
            if let Err(e) = std::fs::File::open(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(KimError::io(&path, &e));
            }
            let previous = current.get(&a.key);
            let mut next = current.clone();
            next.set(&a.key, &a.value)?;
            let value = next.get(&a.key);
            let changed = next != current;
            if changed && !a.dry_run {
                next.save_to(&path)?;
            }
            let summary = match (changed, a.dry_run) {
                (false, _) => format!(
                    "{} is already {}; nothing was changed.\n",
                    a.key,
                    value.unwrap_or_default()
                ),
                (true, true) => format!(
                    "Dry run: {} would change {} → {}; nothing was written.\n",
                    a.key,
                    previous.unwrap_or_default(),
                    value.unwrap_or_default()
                ),
                (true, false) => format!(
                    "{}: {} → {}\n",
                    a.key,
                    previous.unwrap_or_default(),
                    value.unwrap_or_default()
                ),
            };
            let shown = if a.dry_run { &current } else { &next };
            Ok(Outcome {
                data: json!({
                    "settings": shown,
                    "path": path,
                    "key": a.key,
                    "previous": previous,
                    "value": value,
                    "changed": changed,
                    "dry_run": a.dry_run,
                }),
                human: format!("{summary}{}", describe(shown)),
                warnings: warn.into_iter().collect(),
            })
        }
    }
}
