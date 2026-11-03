//! `kim` — the Kim manager CLI (headless; never opens a window).
//!
//! Follows docs/CLI_CONTRACT.md: results on stdout, diagnostics on stderr, exactly one JSON
//! document with `--json`, documented exit codes and stable error codes, `--dry-run` for every
//! state-changing command, and no prompts when Kim cannot ask (`--json`, `--no-input`, no
//! terminal). Every state-changing command acts only on what the user named: nothing runs in the
//! background, nothing is installed or updated implicitly, and the network is used only by
//! `catalog refresh`, `update check --refresh`, `install` and `update`. No code path here starts
//! a GUI; the only child processes are a staged tool's `--version` self-check during an explicit
//! install and `tools describe --probe`.

mod discover;
mod hotkeys_cmd;
mod meta;
mod ops;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use kim::capabilities::JSON_SCHEMA_VERSION;
use kim::catalog::status::{self, ToolStatus, ToolView};
use kim::catalog::store::{self, Loaded};
use kim::catalog::{Channel, details, signature};
use kim::error::{ErrorCode, KimError};
use kim::fetch::DefaultFetcher;
use kim::paths::Paths;
use kim::platform::Platform;
use kim::redact;
use kim::time::{format_date, now_utc, parse_utc};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const AFTER_HELP: &str = "\
Exit codes: 0 ok · 1 failure (I/O, permission) · 2 usage · 3 not found
            4 integrity/validation · 5 incompatible · 6 unavailable (network, offline, timeout)
            7 conflict or confirmation needed · 8 cancelled
--json prints exactly one JSON document on stdout, also for errors. Agents: add --json and
--no-input; every state-changing command supports --dry-run. Contract: docs/CLI_CONTRACT.md";

#[derive(Debug, Parser)]
#[command(
    name = "kim",
    bin_name = "kim",
    version,
    about = "Kim manager — explicit, user-controlled catalog and installer for Kim tools",
    long_about = "Kim lists Kim tools, shows their status on this machine, and installs, updates \
        or removes a tool when you ask for exactly that.\n\n\
        It never installs, updates, removes or starts a tool by itself, never runs in the \
        background, never opens a window, and uses the network only for commands that need it \
        and that you run explicitly (catalog refresh, update check --refresh, install, update).",
    after_help = AFTER_HELP
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
struct Global {
    /// Print exactly one JSON document on stdout instead of human-readable text
    #[arg(long, global = true)]
    json: bool,
    /// Print only results and errors: no warnings or progress on stderr
    #[arg(long, short, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Print extra diagnostics on stderr (stdout is unchanged)
    #[arg(long, short, global = true)]
    verbose: bool,
    /// Never ask questions, even on a terminal (also KIM_NO_INPUT=1); confirmations need --yes
    #[arg(long, global = true)]
    no_input: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Query the tool catalog (read-only, works offline; `refresh` uses the network)
    #[command(subcommand)]
    Catalog(CatalogCommand),
    /// Install a tool (explicit; downloads, verifies, then activates)
    Install(ops::InstallArgs),
    /// `update check` shows available updates; `update <tool>` installs one (both explicit)
    Update(ops::UpdateArgs),
    /// Remove a tool's program files (user data is kept unless --purge-data)
    Uninstall(ops::UninstallArgs),
    /// Discover installed Kim tools and their capabilities (read-only; starts nothing)
    #[command(subcommand)]
    Tools(discover::ToolsCommand),
    /// Kim version, catalog freshness, settings, installed tools and updates (read-only)
    Status,
    /// Kim's version and the CLI contract and schema versions it speaks (read-only)
    Version,
    /// Kim's own capabilities document: its commands and what each may change (read-only)
    Capabilities,
    /// Show or change Kim settings
    #[command(subcommand)]
    Settings(ops::SettingsCommand),
    /// Kim Manager's keyboard shortcuts: list, change, disable, reset, check conflicts across
    /// installed tools (never registers a hotkey or opens a window)
    #[command(subcommand)]
    Hotkeys(hotkeys_cmd::HotkeysCommand),
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    /// List tools and their status on this machine
    List {
        #[command(flatten)]
        channel: ChannelArg,
        /// Only tools installed on this machine (including Kim itself)
        #[arg(long)]
        installed: bool,
    },
    /// Show one tool with its releases and packages
    Show {
        /// Tool id, e.g. `subak`
        tool: String,
        #[command(flatten)]
        channel: ChannelArg,
    },
    /// Show which catalog is in use (bundled or cached), its signer and freshness
    Status,
    /// Validate a catalog file and its signature (for publishers and agents)
    Verify {
        /// Path to a catalog.json
        file: std::path::PathBuf,
        /// Signature file (default: <file>.sig)
        #[arg(long)]
        sig: Option<std::path::PathBuf>,
    },
    /// Fetch the catalog from its source (explicit network access; installs nothing)
    Refresh {
        /// Source URL (https://, http://localhost or file://); `.sig` is appended for the signature
        #[arg(long, default_value = store::DEFAULT_SOURCE)]
        source: String,
        /// Fetch and verify, report what would change, but do not update the cache
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
struct ChannelArg {
    /// Release channel: stable, or preview (stable + preview)
    #[arg(long, value_enum, default_value_t = ChannelOpt::Stable)]
    channel: ChannelOpt,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ChannelOpt {
    Stable,
    Preview,
}

impl From<ChannelOpt> for Channel {
    fn from(c: ChannelOpt) -> Self {
        match c {
            ChannelOpt::Stable => Channel::Stable,
            ChannelOpt::Preview => Channel::Preview,
        }
    }
}

/// Output options shared by every command (docs/CLI_CONTRACT.md §3–§4).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ctx {
    pub json: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub no_input: bool,
}

impl Ctx {
    fn new(g: &Global) -> Self {
        let env_no_input = std::env::var_os("KIM_NO_INPUT")
            .is_some_and(|v| !v.is_empty() && v != "0" && v != "false");
        Self {
            json: g.json,
            quiet: g.quiet,
            verbose: g.verbose,
            no_input: g.no_input || env_no_input,
        }
    }

    /// Whether Kim may ask the user a question now: never with `--json` or `--no-input`, and
    /// only when both stdin and stderr are terminals.
    pub fn can_prompt(self) -> bool {
        !self.json
            && !self.no_input
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal()
    }
}

/// Result of a command: JSON data plus a human rendering.
pub(crate) struct Outcome {
    pub data: Value,
    pub human: String,
    pub warnings: Vec<String>,
}

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// A `--verbose` diagnostic line on stderr (redacted, never on stdout).
pub(crate) fn diag(msg: impl AsRef<str>) {
    if VERBOSE.load(Ordering::Relaxed) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "kim: {}",
            redact::text(msg.as_ref())
        );
    }
}

fn main() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let json_requested = argv.iter().skip(1).any(|a| a == "--json");
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(e) => return parse_failure(&e, &argv, json_requested),
    };
    let ctx = Ctx::new(&cli.global);
    VERBOSE.store(ctx.verbose, Ordering::Relaxed);
    let Some(command) = cli.command else {
        if ctx.json {
            let err = KimError::new(ErrorCode::Usage, "no command given")
                .with_hint("run `kim --help` to list the commands");
            print_json(error_doc("kim", &err));
            return ExitCode::from(ErrorCode::Usage.exit_code());
        }
        let _ = Cli::command().print_help();
        return ExitCode::SUCCESS;
    };
    let name = command_name(&command);
    diag(format!(
        "{name}: kim {} ({} build), CLI contract v{}",
        kim::KIM_VERSION,
        build_kind(),
        kim::capabilities::CLI_CONTRACT
    ));
    let started = Instant::now();
    let result = match command {
        Command::Catalog(c) => run_catalog(c),
        Command::Install(a) => ops::install(&a, ctx),
        Command::Update(a) => ops::update(&a, ctx),
        Command::Uninstall(a) => ops::uninstall(&a, ctx),
        Command::Tools(c) => discover::tools(&c),
        Command::Status => discover::status(),
        Command::Version => discover::version(),
        Command::Capabilities => Ok(discover::capabilities()),
        Command::Settings(c) => ops::settings(&c),
        Command::Hotkeys(c) => hotkeys_cmd::run(&c, ctx),
    };
    finish(name, result, ctx, started)
}

/// The dotted command path used as the envelope's `command` (docs/CLI_CONTRACT.md §7).
fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Catalog(CatalogCommand::List { .. }) => "catalog.list",
        Command::Catalog(CatalogCommand::Show { .. }) => "catalog.show",
        Command::Catalog(CatalogCommand::Status) => "catalog.status",
        Command::Catalog(CatalogCommand::Verify { .. }) => "catalog.verify",
        Command::Catalog(CatalogCommand::Refresh { .. }) => "catalog.refresh",
        Command::Install(_) => "install",
        Command::Update(a) if a.is_check() => "update.check",
        Command::Update(_) => "update",
        Command::Uninstall(_) => "uninstall",
        Command::Tools(discover::ToolsCommand::List) => "tools.list",
        Command::Tools(discover::ToolsCommand::Describe(_)) => "tools.describe",
        Command::Status => "status",
        Command::Version => "version",
        Command::Capabilities => "capabilities",
        Command::Settings(ops::SettingsCommand::Show) => "settings.show",
        Command::Settings(ops::SettingsCommand::Set(_)) => "settings.set",
        Command::Hotkeys(c) => hotkeys_cmd::command_name(c),
    }
}

/// Best-effort command path of arguments that failed to parse: the leading subcommand words.
fn parsed_path(argv: &[OsString]) -> Vec<String> {
    let mut cmd = Cli::command();
    let mut path = Vec::new();
    for arg in argv.iter().skip(1) {
        let Some(a) = arg.to_str() else { break };
        if a.starts_with('-') {
            continue;
        }
        if path == ["update"] && a == "check" {
            path.push("check".to_string());
            break;
        }
        let Some(sub) = cmd.find_subcommand(a).cloned() else {
            break;
        };
        path.push(sub.get_name().to_string());
        cmd = sub;
    }
    path
}

fn parse_failure(e: &clap::Error, argv: &[OsString], json: bool) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = e.print();
        return ExitCode::SUCCESS;
    }
    let path = parsed_path(argv);
    let name = if path.is_empty() {
        "kim".to_string()
    } else {
        path.join(".")
    };
    if json {
        let words: Vec<&str> = std::iter::once("kim")
            .chain(path.iter().map(String::as_str))
            .collect();
        let err = KimError::new(ErrorCode::Usage, first_line(&e.to_string()))
            .with_hint(format!("see `{} --help`", words.join(" ")));
        print_json(error_doc(&name, &err));
    } else {
        let _ = e.print();
    }
    ExitCode::from(ErrorCode::Usage.exit_code())
}

fn finish(name: &str, result: Result<Outcome, KimError>, ctx: Ctx, started: Instant) -> ExitCode {
    let code = match result {
        Ok(out) => {
            if ctx.json {
                print_json(json!({
                    "ok": true,
                    "command": name,
                    "schema_version": JSON_SCHEMA_VERSION,
                    "data": out.data,
                    "warnings": out.warnings,
                }));
            } else {
                if !ctx.quiet {
                    let mut err = std::io::stderr().lock();
                    for w in &out.warnings {
                        let _ = writeln!(err, "kim: warning: {}", redact::text(w));
                    }
                }
                let _ = write!(std::io::stdout().lock(), "{}", redact::text(&out.human));
            }
            0
        }
        Err(err) => {
            if ctx.json {
                print_json(error_doc(name, &err));
            } else {
                let mut out = std::io::stderr().lock();
                let _ = writeln!(out, "kim: error: {}", redact::text(&err.message));
                for d in &err.details {
                    let _ = writeln!(out, "  - {}", redact::text(d));
                }
                if let Some(h) = &err.hint {
                    let _ = writeln!(out, "hint: {}", redact::text(h));
                }
            }
            diag(format!(
                "error code `{}` (exit code {})",
                err.code.as_str(),
                err.code.exit_code()
            ));
            err.code.exit_code()
        }
    };
    diag(format!(
        "{name} finished in {} ms",
        started.elapsed().as_millis()
    ));
    ExitCode::from(code)
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or("")
        .trim_start_matches("error: ")
        .to_string()
}

fn error_doc(command: &str, err: &KimError) -> Value {
    json!({
        "ok": false,
        "command": command,
        "schema_version": JSON_SCHEMA_VERSION,
        "error": {
            "code": err.code,
            "exit_code": err.code.exit_code(),
            "message": err.message,
            "hint": err.hint,
            "details": err.details,
        },
    })
}

/// Prints one JSON document on stdout, redacted (KIM-SEC-013).
fn print_json(mut v: Value) {
    redact::json(&mut v);
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer_pretty(&mut out, &v);
    let _ = writeln!(out);
}

pub(crate) fn build_kind() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// Renders rows as aligned columns; the first row is the header, the last column is not padded.
pub(crate) fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.first().map_or(0, Vec::len);
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .map(|r| r.get(i).map_or(0, |c| c.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut s = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, cell) in r.iter().enumerate() {
            if i + 1 == cols {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

fn run_catalog(c: CatalogCommand) -> Result<Outcome, KimError> {
    match c {
        CatalogCommand::List { channel, installed } => {
            catalog_list(channel.channel.into(), installed)
        }
        CatalogCommand::Show { tool, channel } => catalog_show(&tool, channel.channel.into()),
        CatalogCommand::Status => catalog_status(),
        CatalogCommand::Verify { file, sig } => catalog_verify(&file, sig.as_deref()),
        CatalogCommand::Refresh { source, dry_run } => catalog_refresh(&source, dry_run),
    }
}

/// Keys trusted by this build. Debug builds additionally accept one development key from
/// `KIM_DEV_TRUSTED_KEY` (hex public key) so integration tests can use test-signed catalogs;
/// release builds compile this out entirely.
pub(crate) fn trusted_keys() -> Vec<signature::TrustedKey> {
    #[allow(unused_mut)]
    let mut keys = signature::TRUSTED_KEYS.to_vec();
    #[cfg(debug_assertions)]
    if let Ok(hex) = std::env::var("KIM_DEV_TRUSTED_KEY")
        && let Some(bytes) = signature::unhex::<32>(hex.trim())
    {
        let id: &'static str = Box::leak(signature::key_id(&bytes).into_boxed_str());
        let public: &'static str = Box::leak(hex.trim().to_string().into_boxed_str());
        keys.push(signature::TrustedKey {
            key_id: id,
            public_key_hex: public,
            label: "development key from KIM_DEV_TRUSTED_KEY (debug build only)",
        });
    }
    keys
}

pub(crate) fn load() -> Result<(Paths, Loaded), KimError> {
    let paths = Paths::from_env()?;
    let loaded = store::load(&paths, &trusted_keys(), now_utc())?;
    diag(format!(
        "catalog v{} from the {} copy, signed by {}{}",
        loaded.catalog.catalog_version,
        match loaded.source {
            store::Source::Bundled => "bundled",
            store::Source::Cache => "cached",
        },
        loaded.signer.key_id,
        if loaded.stale { " (stale)" } else { "" }
    ));
    Ok((paths, loaded))
}

pub(crate) fn catalog_info(l: &Loaded) -> Value {
    json!({
        "source": l.source,
        "catalog_version": l.catalog.catalog_version,
        "publisher": l.catalog.publisher,
        "generated_at": l.catalog.generated_at,
        "expires_at": l.catalog.expires_at,
        "stale": l.stale,
        "signed_by": l.signer,
    })
}

pub(crate) fn catalog_header(l: &Loaded) -> String {
    let date = |s: &str| parse_utc(s).map_or_else(|| s.to_string(), format_date);
    format!(
        "Catalog v{} ({}, signed by {}) · generated {} · expires {}{}\n",
        l.catalog.catalog_version,
        match l.source {
            store::Source::Bundled => "bundled",
            store::Source::Cache => "cached",
        },
        l.signer.key_id,
        date(&l.catalog.generated_at),
        date(&l.catalog.expires_at),
        if l.stale { " · STALE" } else { "" }
    )
}

pub(crate) fn status_text(v: &ToolView) -> String {
    match v.status {
        ToolStatus::Installed => format!(
            "installed {}",
            v.installed_version.as_deref().unwrap_or("?")
        ),
        ToolStatus::UpdateAvailable => format!(
            "update {} → {}",
            v.installed_version.as_deref().unwrap_or("?"),
            v.latest_compatible.as_deref().unwrap_or("?")
        ),
        ToolStatus::Damaged => "damaged install".into(),
        // available / available (preview) / incompatible / planned / preview only
        ToolStatus::Available | ToolStatus::Incompatible | ToolStatus::Unreleased => {
            v.availability_label().unwrap_or("not released").into()
        }
    }
}

/// The release stage in words (CATALOG_FORMAT §6).
pub(crate) fn availability_text(a: status::Availability) -> &'static str {
    match a {
        status::Availability::Planned => "planned (no release has been published yet)",
        status::Availability::Preview => "preview (only preview releases; use --channel preview)",
        status::Availability::Available => "available (stable releases are published)",
    }
}

/// Human rendering of the catalog's tool metadata and what Kim decides itself (§2.1).
fn details_text(d: &details::Details) -> String {
    let mut s = String::new();
    if let Some(r) = &d.runs {
        s.push_str(&format!(
            "Runs:     {}{}\n",
            r.id,
            r.label.map(|l| format!(" — {l}")).unwrap_or_default()
        ));
    }
    if let Some(i) = &d.install {
        s.push_str(&format!(
            "Install:  {} (per user, never as administrator)\n",
            i.program_dir.display()
        ));
        let data: Vec<String> = i
            .user_data_dirs
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        s.push_str(&format!(
            "Data:     {} (kept on uninstall unless you also choose --purge-data)\n",
            data.join(", ")
        ));
    }
    if let Some(m) = &d.manage {
        s.push_str(&format!(
            "Updates:  manual only, never automatic: `{}`, `{}`, `{}`; `{}` only shows updates\n",
            m.install, m.update, m.uninstall, m.check
        ));
    }
    if !d.features.is_empty() {
        s.push_str("\nFeatures:\n");
        for f in &d.features {
            s.push_str(&format!("  - {f}\n"));
        }
    }
    if let Some(sc) = &d.shortcuts {
        let source = match sc.source {
            "installed" => "installed manifest and your hotkeys.json",
            _ => "catalog defaults",
        };
        s.push_str(&format!(
            "\nShortcuts ({source}; {} backend; every one can be changed or turned off):\n",
            sc.backend
        ));
        let mut rows = vec![vec![
            "  KEYS".to_string(),
            "ACTION".into(),
            "SCOPE".into(),
            "STATE".into(),
        ]];
        for a in &sc.actions {
            rows.push(vec![
                format!(
                    "  {}",
                    if a.keys.is_empty() {
                        "-".to_string()
                    } else {
                        a.keys.join(", ")
                    }
                ),
                format!("{} ({})", a.title, a.action),
                a.scope.clone(),
                if a.bindable {
                    a.state.clone()
                } else {
                    format!("{}, fixed", a.state)
                },
            ]);
        }
        s.push_str(&table(&rows));
        if let Some(n) = &sc.note {
            s.push_str(&format!("  note: {n}\n"));
        }
    }
    if !d.permissions.is_empty() {
        s.push_str("\nPermissions (shown before every install):\n");
        for p in &d.permissions {
            s.push_str(&format!(
                "  - {}{}: {}\n",
                p.label.unwrap_or(&p.id),
                if p.opt_in {
                    " (only after you turn it on)"
                } else {
                    ""
                },
                p.detail
            ));
        }
    }
    if !d.entry_points.is_empty() {
        s.push_str("\nEntry points:\n");
        let rows: Vec<Vec<String>> = d
            .entry_points
            .iter()
            .map(|e| {
                vec![
                    format!("  {}", e.kind),
                    e.command.clone(),
                    match e.status {
                        kim::catalog::EntryStatus::Available => String::new(),
                        kim::catalog::EntryStatus::Planned => "[planned]".into(),
                    },
                ]
            })
            .collect();
        s.push_str(&table(&rows));
    }
    if !d.links.is_empty() {
        s.push_str("\nLinks:\n");
        for l in &d.links {
            s.push_str(&format!("  {}: {} <{}>\n", l.kind, l.title, l.url));
        }
    }
    s
}

fn catalog_list(channel: Channel, installed_only: bool) -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let platform = Platform::current();
    let views: Vec<ToolView> = status::evaluate(&loaded.catalog, &paths, &platform, channel)
        .into_iter()
        .filter(|v| {
            !installed_only
                || matches!(
                    v.status,
                    ToolStatus::Installed | ToolStatus::UpdateAvailable | ToolStatus::Damaged
                )
        })
        .collect();
    let mut human = catalog_header(&loaded);
    human.push('\n');
    let mut rows = vec![vec![
        "TOOL".to_string(),
        "LATEST".into(),
        "STATUS".into(),
        "SUMMARY".into(),
    ]];
    rows.extend(views.iter().map(|v| {
        vec![
            v.id.clone(),
            v.latest_compatible
                .clone()
                .or_else(|| v.latest.clone())
                .unwrap_or_else(|| "-".into()),
            status_text(v),
            v.summary.clone(),
        ]
    }));
    human.push_str(&table(&rows));
    human.push_str("\nKim never installs or updates tools by itself. Nothing was changed.\n");
    Ok(Outcome {
        data: json!({
            "catalog": catalog_info(&loaded),
            "platform": platform,
            "channel": channel,
            "installed_only": installed_only,
            "tools": views,
        }),
        human,
        warnings: loaded.warnings,
    })
}

fn catalog_show(id: &str, channel: Channel) -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let platform = Platform::current();
    let Some(tool) = loaded.catalog.tool(id) else {
        let known: Vec<&str> = loaded.catalog.tools.iter().map(|t| t.id.as_str()).collect();
        return Err(KimError::new(
            ErrorCode::ToolNotFound,
            format!("no tool `{id}` in the catalog"),
        )
        .with_hint(format!("known tools: {}", known.join(", "))));
    };
    let kim = semver::Version::parse(kim::KIM_VERSION).expect("SemVer");
    let installed = status::installed(&paths, id);
    let view = status::evaluate_tool(tool, &platform, &kim, &installed, channel);
    let info = details::details(&paths, tool);
    let releases: Vec<Value> = tool
        .releases
        .iter()
        .map(|r| {
            let asset = status::matching_asset(r, &platform);
            json!({
                "version": r.version,
                "channel": r.channel,
                "published_at": r.published_at,
                "release_notes": r.release_notes,
                "compatible": status::incompatibility(r, &platform, &kim).is_none(),
                "incompatible_reason": status::incompatibility(r, &platform, &kim),
                "package_for_this_machine": asset,
                "dependencies": r.dependencies,
            })
        })
        .collect();
    let mut human = format!(
        "{} ({})\n{}\n\n{}\n\n",
        tool.name, tool.id, tool.summary, tool.description
    );
    human.push_str(&format!("Status:   {}\n", status_text(&view)));
    human.push_str(&format!(
        "Release:  {}\n",
        availability_text(view.availability)
    ));
    human.push_str(&format!(
        "Support:  {}\n",
        serde_json::to_value(tool.support)
            .unwrap_or_default()
            .as_str()
            .unwrap_or("")
    ));
    human.push_str(&format!("License:  {}\n", tool.license));
    if let Some(h) = &tool.homepage {
        human.push_str(&format!("Homepage: {h}\n"));
    }
    if let Some(r) = &view.incompatible_reason {
        human.push_str(&format!("Note:     newest release is incompatible: {r}\n"));
    }
    if let Some(n) = &view.note {
        human.push_str(&format!("Note:     {n}\n"));
    }
    human.push_str(&details_text(&info));
    if tool.releases.is_empty() {
        human.push_str("\nNo releases yet.\n");
    } else {
        human.push_str("\nReleases:\n");
        for r in &tool.releases {
            let compat = status::incompatibility(r, &platform, &kim).map_or_else(
                || "compatible".to_string(),
                |why| format!("incompatible: {why}"),
            );
            human.push_str(&format!(
                "  {} [{}] {} — {}\n",
                r.version,
                serde_json::to_value(r.channel)
                    .unwrap_or_default()
                    .as_str()
                    .unwrap_or(""),
                r.published_at.get(..10).unwrap_or(&r.published_at),
                compat
            ));
        }
    }
    let mut data = json!({
        "catalog": catalog_info(&loaded),
        "platform": platform,
        "tool": view,
        "description": tool.description,
        "license": tool.license,
        "homepage": tool.homepage,
        "releases": releases,
    });
    // features, runs, entry_points, links, permissions, shortcuts, install, manage (§7)
    if let (Value::Object(d), Ok(Value::Object(extra))) = (&mut data, serde_json::to_value(&info)) {
        d.extend(extra);
    }
    Ok(Outcome {
        data,
        human,
        warnings: loaded.warnings,
    })
}

fn catalog_status() -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let cache = paths.catalog_cache_dir();
    let mut human = catalog_header(&loaded);
    human.push_str(&format!(
        "Signer:  {} ({})\n",
        loaded.signer.label, loaded.signer.key_id
    ));
    human.push_str(&format!("Cache:   {}\n", cache.display()));
    human.push_str(&format!(
        "Source:  {} (used only by `kim catalog refresh`)\n",
        store::DEFAULT_SOURCE
    ));
    Ok(Outcome {
        data: json!({
            "catalog": catalog_info(&loaded),
            "cache_dir": cache,
            "default_source": store::DEFAULT_SOURCE,
            "trusted_keys": signature::TRUSTED_KEYS.iter().map(|k| json!({"key_id": k.key_id, "label": k.label})).collect::<Vec<_>>(),
        }),
        human,
        warnings: loaded.warnings,
    })
}

fn catalog_verify(
    file: &std::path::Path,
    sig: Option<&std::path::Path>,
) -> Result<Outcome, KimError> {
    let read = |p: &std::path::Path| std::fs::read(p).map_err(|e| KimError::io(p, &e));
    let bytes = read(file)?;
    let sig_path = sig.map_or_else(
        || {
            let mut s = file.as_os_str().to_owned();
            s.push(".sig");
            std::path::PathBuf::from(s)
        },
        std::path::Path::to_path_buf,
    );
    let sig_bytes = read(&sig_path)?;
    let (cat, signer) = store::verify_document(&bytes, &sig_bytes, &trusted_keys())?;
    let stale = store::is_stale(&cat, now_utc());
    let human = format!(
        "OK: {} is a valid Kim catalog v{} ({} tools), signed by {} ({}){}\n",
        file.display(),
        cat.catalog_version,
        cat.tools.len(),
        signer.label,
        signer.key_id,
        if stale {
            "; note: it is stale (past expires_at)"
        } else {
            ""
        }
    );
    Ok(Outcome {
        data: json!({
            "file": file,
            "signature_file": sig_path,
            "valid": true,
            "catalog_version": cat.catalog_version,
            "tools": cat.tools.len(),
            "stale": stale,
            "signed_by": signer,
        }),
        human,
        warnings: Vec::new(),
    })
}

fn catalog_refresh(source: &str, dry_run: bool) -> Result<Outcome, KimError> {
    let paths = Paths::from_env()?;
    ops::load_settings().0.ensure_online()?;
    diag(format!("fetching {source} and {source}.sig"));
    let fetched = store::fetch_verified(
        &paths,
        &trusted_keys(),
        now_utc(),
        source,
        &DefaultFetcher::default(),
    )?;
    if !dry_run {
        store::commit(&paths, &fetched)?;
    }
    let out = &fetched.outcome;
    let human = match (dry_run, out.updated) {
        (false, true) => format!(
            "Catalog updated: v{} → v{} (from {}). No tool was installed or updated.\n",
            out.previous_version, out.fetched_version, out.source_url
        ),
        (false, false) => format!(
            "Catalog is up to date (v{}, verified from {}). No tool was installed or updated.\n",
            out.fetched_version, out.source_url
        ),
        (true, true) => format!(
            "Dry run: the catalog would be updated v{} → v{} (verified from {}). The cache was not changed.\n",
            out.previous_version, out.fetched_version, out.source_url
        ),
        (true, false) => format!(
            "Dry run: the catalog is up to date (v{}, verified from {}). Nothing was changed.\n",
            out.fetched_version, out.source_url
        ),
    };
    let mut data = serde_json::to_value(out).unwrap_or_default();
    data["dry_run"] = json!(dry_run);
    Ok(Outcome {
        data,
        human,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Every leaf command path of the clap tree, plus `update.check` (a reserved word of
    /// `update`).
    fn leaf_commands() -> BTreeSet<String> {
        fn walk(cmd: &clap::Command, prefix: &str, out: &mut BTreeSet<String>) {
            let subs: Vec<_> = cmd
                .get_subcommands()
                .filter(|s| s.get_name() != "help")
                .collect();
            if subs.is_empty() {
                out.insert(prefix.to_string());
            }
            for s in subs {
                let name = if prefix.is_empty() {
                    s.get_name().to_string()
                } else {
                    format!("{prefix}.{}", s.get_name())
                };
                walk(s, &name, out);
            }
        }
        let mut out = BTreeSet::new();
        walk(&Cli::command(), "", &mut out);
        out.insert("update.check".into());
        out
    }

    fn clap_command(path: &str) -> clap::Command {
        let path = if path == "update.check" {
            "update"
        } else {
            path
        };
        let mut cmd = Cli::command();
        for part in path.split('.') {
            cmd = cmd.find_subcommand(part).expect(part).clone();
        }
        cmd
    }

    fn has_flag(cmd: &clap::Command, long: &str) -> bool {
        cmd.get_arguments().any(|a| a.get_long() == Some(long))
    }

    #[test]
    fn clap_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn capabilities_list_exactly_the_real_commands() {
        let caps = meta::capabilities();
        let listed: BTreeSet<String> = caps.commands.iter().map(|c| c.name.clone()).collect();
        assert_eq!(
            listed,
            leaf_commands(),
            "kim capabilities must match the CLI"
        );
    }

    #[test]
    fn every_state_changing_command_supports_dry_run_and_flags_match() {
        use kim::capabilities::Effect;
        for c in meta::capabilities().commands {
            let cmd = clap_command(&c.name);
            assert_eq!(
                c.dry_run,
                has_flag(&cmd, "dry-run"),
                "{}: dry_run flag",
                c.name
            );
            assert_eq!(
                c.requires_yes,
                has_flag(&cmd, "yes"),
                "{}: --yes flag",
                c.name
            );
            if c.effect != Effect::Read {
                assert!(c.dry_run, "{} changes state but has no --dry-run", c.name);
            }
            assert!(!c.opens_window, "kim never opens a window");
        }
    }

    #[test]
    fn command_names_cover_the_capabilities() {
        // Each capability name is what the envelope reports for that command.
        let parse = |args: &[&str]| {
            let cli = Cli::try_parse_from(std::iter::once("kim").chain(args.iter().copied()))
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            command_name(&cli.command.unwrap()).to_string()
        };
        let mut seen = BTreeSet::new();
        for args in [
            &["capabilities"][..],
            &["catalog", "list"],
            &["catalog", "show", "x"],
            &["catalog", "status"],
            &["catalog", "verify", "f"],
            &["catalog", "refresh"],
            &["install", "x"],
            &["update", "check"],
            &["update", "x"],
            &["uninstall", "x"],
            &["tools", "list"],
            &["tools", "describe", "x"],
            &["status"],
            &["version"],
            &["settings", "show"],
            &["settings", "set", "k", "v"],
            &["hotkeys", "list"],
            &["hotkeys", "show", "help.show"],
            &["hotkeys", "set", "help.show", "F2"],
            &["hotkeys", "disable", "--all"],
            &["hotkeys", "enable", "--all"],
            &["hotkeys", "reset", "--all"],
            &["hotkeys", "check"],
            &["hotkeys", "export"],
            &["hotkeys", "import", "f.json"],
        ] {
            seen.insert(parse(args));
        }
        assert_eq!(seen, leaf_commands());
    }

    #[test]
    fn failed_parses_report_the_command_path() {
        let p = |args: &[&str]| {
            let argv: Vec<OsString> = std::iter::once("kim")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect();
            parsed_path(&argv).join(".")
        };
        assert_eq!(p(&["catalog", "frobnicate", "--json"]), "catalog");
        assert_eq!(p(&["--json", "tools", "describe"]), "tools.describe");
        assert_eq!(p(&["update", "check", "--bogus"]), "update.check");
        assert_eq!(p(&["nope"]), "");
    }

    #[test]
    fn tables_align_columns() {
        let t = table(&[
            vec!["A".into(), "BB".into(), "C".into()],
            vec!["xxx".into(), "y".into(), "last column".into()],
        ]);
        assert_eq!(t, "A    BB  C\nxxx  y   last column\n");
    }
}
