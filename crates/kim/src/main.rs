//! `kim` — the Kim manager CLI (headless; never opens a window).
//!
//! Every state-changing command acts only on the tool the user named: `install`, `update`,
//! `uninstall`. Nothing runs in the background, nothing is installed or updated implicitly, and
//! the network is used only by `catalog refresh`, `update check --refresh`, `install` and
//! `update` (all explicit).

mod ops;

use clap::{Args, Parser, Subcommand, ValueEnum};
use kim::catalog::status::{self, ToolStatus, ToolView};
use kim::catalog::store::{self, Loaded};
use kim::catalog::{Channel, signature};
use kim::error::{ErrorCode, KimError};
use kim::fetch::DefaultFetcher;
use kim::paths::Paths;
use kim::platform::Platform;
use kim::time::{format_date, now_utc, parse_utc};
use serde_json::{Value, json};
use std::io::Write;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(
    name = "kim",
    bin_name = "kim",
    version,
    about = "Kim manager — explicit, user-controlled catalog of Kim tools",
    long_about = "Kim lists Kim tools and their status on this machine.\n\n\
        It never installs, updates, removes or starts a tool by itself, never runs in the \
        background, and only uses the network when you run `kim catalog refresh`.",
    after_help = "Exit codes: 0 ok · 1 internal/I-O · 2 usage · 3 not found · 4 integrity/trust · 6 network.\n\
        Use --json for machine-readable output (one JSON document on stdout)."
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
struct Global {
    /// Print one JSON document on stdout instead of human-readable text
    #[arg(long, global = true)]
    json: bool,
    /// Suppress warnings on stderr
    #[arg(long, short, global = true)]
    quiet: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Query the tool catalog (read-only, works offline)
    #[command(subcommand)]
    Catalog(CatalogCommand),
    /// Install a tool (explicit; downloads, verifies, then activates)
    Install(ops::InstallArgs),
    /// `update check` shows available updates; `update <tool>` installs one (both explicit)
    Update(ops::UpdateArgs),
    /// Remove a tool's program files (user data is kept unless --purge-data)
    Uninstall(ops::UninstallArgs),
    /// Show or change Kim settings
    #[command(subcommand)]
    Settings(ops::SettingsCommand),
}

#[derive(Debug, Subcommand)]
enum CatalogCommand {
    /// List tools and their status on this machine
    List(ChannelArg),
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

/// Result of a command: JSON data plus a human rendering.
pub(crate) struct Outcome {
    command: &'static str,
    data: Value,
    human: String,
    warnings: Vec<String>,
}

fn main() -> ExitCode {
    let json_requested = std::env::args().any(|a| a == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                let _ = e.print();
                return ExitCode::SUCCESS;
            }
            if json_requested {
                let err = KimError::new(ErrorCode::Usage, first_line(&e.to_string()));
                print_json(&error_doc("usage", &err));
            } else {
                let _ = e.print();
            }
            return ExitCode::from(ErrorCode::Usage.exit_code());
        }
    };
    let Some(command) = cli.command else {
        use clap::CommandFactory;
        let _ = Cli::command().print_help();
        return ExitCode::SUCCESS;
    };
    let result = match command {
        Command::Catalog(c) => run_catalog(c),
        Command::Install(a) => ops::install(&a, cli.global.json).map_err(|e| ("install", e)),
        Command::Update(a) => ops::update(&a, cli.global.json).map_err(|e| ("update", e)),
        Command::Uninstall(a) => ops::uninstall(&a, cli.global.json).map_err(|e| ("uninstall", e)),
        Command::Settings(c) => ops::settings(&c).map_err(|e| ("settings", e)),
    };
    match result {
        Ok(out) => {
            if cli.global.json {
                print_json(&json!({
                    "ok": true,
                    "command": out.command,
                    "schema_version": 1,
                    "data": out.data,
                    "warnings": out.warnings,
                }));
            } else {
                if !cli.global.quiet {
                    for w in &out.warnings {
                        eprintln!("kim: warning: {w}");
                    }
                }
                print!("{}", out.human);
            }
            ExitCode::SUCCESS
        }
        Err((command, err)) => {
            if cli.global.json {
                print_json(&error_doc(command, &err));
            } else {
                eprintln!("kim: error: {}", err.message);
                for d in &err.details {
                    eprintln!("  - {d}");
                }
                if let Some(h) = &err.hint {
                    eprintln!("hint: {h}");
                }
            }
            ExitCode::from(err.code.exit_code())
        }
    }
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
        "schema_version": 1,
        "error": {
            "code": err.code,
            "exit_code": err.code.exit_code(),
            "message": err.message,
            "hint": err.hint,
            "details": err.details,
        },
    })
}

fn print_json(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer_pretty(&mut out, v);
    let _ = writeln!(out);
}

type CmdResult = Result<Outcome, (&'static str, KimError)>;

fn run_catalog(c: CatalogCommand) -> CmdResult {
    match c {
        CatalogCommand::List(ch) => {
            catalog_list(ch.channel.into()).map_err(|e| ("catalog.list", e))
        }
        CatalogCommand::Show { tool, channel } => {
            catalog_show(&tool, channel.channel.into()).map_err(|e| ("catalog.show", e))
        }
        CatalogCommand::Status => catalog_status().map_err(|e| ("catalog.status", e)),
        CatalogCommand::Verify { file, sig } => {
            catalog_verify(&file, sig.as_deref()).map_err(|e| ("catalog.verify", e))
        }
        CatalogCommand::Refresh { source } => {
            catalog_refresh(&source).map_err(|e| ("catalog.refresh", e))
        }
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
    Ok((paths, loaded))
}

fn catalog_info(l: &Loaded) -> Value {
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

fn status_text(v: &ToolView) -> String {
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
        ToolStatus::Available => "available".into(),
        ToolStatus::Incompatible => "incompatible".into(),
        ToolStatus::Unreleased => "not released".into(),
        ToolStatus::Damaged => "damaged install".into(),
    }
}

fn catalog_list(channel: Channel) -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let platform = Platform::current();
    let views = status::evaluate(&loaded.catalog, &paths, &platform, channel);
    let mut human = catalog_header(&loaded);
    human.push('\n');
    let rows: Vec<[String; 4]> = views
        .iter()
        .map(|v| {
            [
                v.id.clone(),
                v.latest_compatible
                    .clone()
                    .or_else(|| v.latest.clone())
                    .unwrap_or_else(|| "-".into()),
                status_text(v),
                v.summary.clone(),
            ]
        })
        .collect();
    let header = [
        "TOOL".to_string(),
        "LATEST".into(),
        "STATUS".into(),
        "SUMMARY".into(),
    ];
    let width = |i: usize| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([header[i].len()])
            .max()
            .unwrap_or(0)
    };
    let (w0, w1, w2) = (width(0), width(1), width(2));
    for r in std::iter::once(&header).chain(rows.iter()) {
        human.push_str(&format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {}\n",
            r[0], r[1], r[2], r[3]
        ));
    }
    human.push_str("\nKim never installs or updates tools by itself. Nothing was changed.\n");
    Ok(Outcome {
        command: "catalog.list",
        data: json!({
            "catalog": catalog_info(&loaded),
            "platform": platform,
            "channel": channel,
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
    Ok(Outcome {
        command: "catalog.show",
        data: json!({
            "catalog": catalog_info(&loaded),
            "platform": platform,
            "tool": view,
            "description": tool.description,
            "license": tool.license,
            "homepage": tool.homepage,
            "releases": releases,
        }),
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
        command: "catalog.status",
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
    let read = |p: &std::path::Path| {
        std::fs::read(p)
            .map_err(|e| KimError::new(ErrorCode::Io, format!("cannot read {}: {e}", p.display())))
    };
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
        command: "catalog.verify",
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

fn catalog_refresh(source: &str) -> Result<Outcome, KimError> {
    let paths = Paths::from_env()?;
    ops::load_settings().0.ensure_online()?;
    let out = store::refresh(
        &paths,
        &trusted_keys(),
        now_utc(),
        source,
        &DefaultFetcher::default(),
    )?;
    let human = if out.updated {
        format!(
            "Catalog updated: v{} → v{} (from {}). No tool was installed or updated.\n",
            out.previous_version, out.fetched_version, out.source_url
        )
    } else {
        format!(
            "Catalog is up to date (v{}, verified from {}). No tool was installed or updated.\n",
            out.fetched_version, out.source_url
        )
    };
    Ok(Outcome {
        command: "catalog.refresh",
        data: serde_json::to_value(&out).unwrap_or_default(),
        human,
        warnings: Vec::new(),
    })
}
