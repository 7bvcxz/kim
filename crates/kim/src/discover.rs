//! Read-only discovery commands: `tools list|describe`, `status`, `version`, `capabilities`
//! (docs/CLI_CONTRACT.md §9, KIM-FR-020). They read install records, manifests and the local
//! catalog; they never use the network and never write anything. Only `tools describe --probe`
//! starts a process: the named tool's own `capabilities --json`, headless and time-limited.

use crate::{Outcome, build_kind, catalog_header, catalog_info, diag, load, meta, table};
use clap::{Args, Subcommand};
use kim::capabilities::{self, CLI_CONTRACT, Capabilities, Effect, JSON_SCHEMA_VERSION};
use kim::catalog::status::{self, ToolStatus};
use kim::catalog::{Channel, store::Loaded};
use kim::error::{ErrorCode, KimError};
use kim::install;
use kim::inventory::{self, InstalledTool};
use kim::paths::Paths;
use kim::platform::Platform;
use kim::settings::Settings;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Debug, Subcommand)]
pub enum ToolsCommand {
    /// List installed Kim tools with version, executable and capabilities (reads files only)
    List,
    /// Show one installed tool and its capabilities; optionally check requirements
    Describe(DescribeArgs),
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Tool id, e.g. `subak` (or `kim`)
    tool: String,
    /// Ask the tool itself (`<tool> capabilities --json`: headless, no input, time-limited)
    /// instead of reading the manifest shipped with its install
    #[arg(long)]
    probe: bool,
    /// Time limit for --probe, in seconds (1-60)
    #[arg(long, value_name = "SECONDS", default_value_t = 5,
          value_parser = clap::value_parser!(u64).range(1..=60))]
    timeout: u64,
    /// Fail with `tool_incompatible` unless the installed version matches this SemVer
    /// requirement, e.g. ">=0.3"
    #[arg(long, value_name = "REQ")]
    require_version: Option<String>,
    /// Fail with `capability_missing` unless the tool offers this command (repeatable)
    #[arg(long, value_name = "COMMAND")]
    require_command: Vec<String>,
}

pub fn tools(c: &ToolsCommand) -> Result<Outcome, KimError> {
    match c {
        ToolsCommand::List => tools_list(),
        ToolsCommand::Describe(a) => tools_describe(a),
    }
}

/// Where a capabilities document came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapsSource {
    /// Kim describing itself.
    Builtin,
    /// `capabilities.json` shipped with the installed version.
    Manifest,
    /// Asked the tool (`--probe`).
    Probe,
    /// No document available.
    None,
    /// A manifest exists but is not valid.
    Invalid,
}

impl CapsSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Manifest => "manifest",
            Self::Probe => "probe",
            Self::None => "none",
            Self::Invalid => "invalid",
        }
    }
}

fn kim_executable() -> Option<String> {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
}

fn caps_summary(source: CapsSource, caps: Option<&Capabilities>) -> Value {
    json!({
        "source": source.as_str(),
        "cli_contract": caps.map(|c| c.cli_contract),
        "commands": caps.map(|c| c.commands.len()),
    })
}

/// One row of `tools list` / the `tool` object of `tools describe`.
fn entry(
    id: &str,
    loaded: &Loaded,
    paths: &Paths,
    installed: Option<&InstalledTool>,
    caps: (CapsSource, Option<&Capabilities>),
) -> Value {
    let platform = Platform::current();
    let kim = semver::Version::parse(kim::KIM_VERSION).expect("SemVer");
    let in_catalog = loaded.catalog.tool(id);
    let view = in_catalog.map(|t| {
        status::evaluate_tool(
            t,
            &platform,
            &kim,
            &status::installed(paths, id),
            Channel::Stable,
        )
    });
    let (version, entrypoint, damaged, managed) = match installed {
        None => (
            Some(kim::KIM_VERSION.to_string()),
            kim_executable(),
            None,
            false,
        ),
        Some(t) => (
            t.version.clone(),
            t.entrypoint.as_ref().map(|p| p.display().to_string()),
            t.damaged.clone(),
            true,
        ),
    };
    let status = if damaged.is_some() {
        ToolStatus::Damaged
    } else {
        view.as_ref().map_or(ToolStatus::Installed, |v| v.status)
    };
    json!({
        "id": id,
        "name": in_catalog.map_or(id, |t| t.name.as_str()),
        "version": version,
        "managed": managed,
        "entrypoint": entrypoint,
        "status": status,
        "update_available": view.as_ref().and_then(|v| {
            (v.status == ToolStatus::UpdateAvailable).then(|| v.latest_compatible.clone()).flatten()
        }),
        "damaged": damaged,
        "capabilities": caps_summary(caps.0, caps.1),
    })
}

/// Capabilities of an installed tool from its manifest (never starts the tool).
fn manifest_of(t: &InstalledTool) -> (CapsSource, Option<Result<Capabilities, KimError>>) {
    let (Some(dir), Some(version)) = (&t.version_dir, &t.version) else {
        return (CapsSource::None, None);
    };
    match capabilities::read_manifest(dir, &t.id, version) {
        None => (CapsSource::None, None),
        Some(Ok(c)) => (CapsSource::Manifest, Some(Ok(c))),
        Some(Err(e)) => (CapsSource::Invalid, Some(Err(e))),
    }
}

fn tools_list() -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let mut warnings = loaded.warnings.clone();
    let kim_caps = meta::capabilities();
    let mut entries = vec![entry(
        "kim",
        &loaded,
        &paths,
        None,
        (CapsSource::Builtin, Some(&kim_caps)),
    )];
    let mut rows = vec![vec![
        "TOOL".to_string(),
        "VERSION".into(),
        "STATUS".into(),
        "CAPABILITIES".into(),
        "EXECUTABLE".into(),
    ]];
    rows.push(vec![
        "kim".into(),
        kim::KIM_VERSION.into(),
        "running".into(),
        format!("{} commands (built in)", kim_caps.commands.len()),
        kim_executable().unwrap_or_default(),
    ]);
    for t in inventory::installed_tools(&paths) {
        if t.id == "kim" {
            continue;
        }
        let (source, manifest) = manifest_of(&t);
        let caps = match manifest {
            Some(Ok(c)) => Some(c),
            Some(Err(e)) => {
                warnings.push(format!("{}: {}", t.id, e.message));
                None
            }
            None => None,
        };
        let e = entry(&t.id, &loaded, &paths, Some(&t), (source, caps.as_ref()));
        rows.push(vec![
            t.id.clone(),
            t.version.clone().unwrap_or_else(|| "?".into()),
            e["status"].as_str().unwrap_or("").to_string(),
            match (&caps, source) {
                (Some(c), _) => format!("{} commands (manifest)", c.commands.len()),
                (None, CapsSource::Invalid) => "invalid manifest".into(),
                _ => "not published (try --probe)".into(),
            },
            t.entrypoint
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "-".into()),
        ]);
        entries.push(e);
    }
    let mut human = table(&rows);
    if entries.len() == 1 {
        human.push_str(
            "\nNo other Kim tools are installed. `kim catalog list` shows what is available.\n",
        );
    }
    Ok(Outcome {
        data: json!({ "tools": entries }),
        human,
        warnings,
    })
}

fn tools_describe(a: &DescribeArgs) -> Result<Outcome, KimError> {
    // Validate arguments before touching anything.
    let version_req = a
        .require_version
        .as_deref()
        .map(|r| {
            semver::VersionReq::parse(r).map_err(|e| {
                KimError::new(
                    ErrorCode::Usage,
                    format!("--require-version `{r}` is not a SemVer requirement: {e}"),
                )
                .with_hint("examples: \">=0.3\", \"^1.2\", \"=0.1.0\"")
            })
        })
        .transpose()?;
    let (paths, loaded) = load()?;
    let warnings = loaded.warnings.clone();
    let (installed, version, caps, source) = if a.tool == "kim" {
        (
            None,
            Some(kim::KIM_VERSION.to_string()),
            Some(meta::capabilities()),
            CapsSource::Builtin,
        )
    } else {
        let Some(t) = inventory::installed_tool(&paths, &a.tool) else {
            let hint = if loaded.catalog.tool(&a.tool).is_some() {
                format!("install it explicitly with `kim install {}`", a.tool)
            } else {
                let ids: Vec<String> = inventory::installed_tools(&paths)
                    .into_iter()
                    .map(|t| t.id)
                    .chain(["kim".to_string()])
                    .collect();
                format!("installed tools: {}", ids.join(", "))
            };
            return Err(KimError::new(
                ErrorCode::NotInstalled,
                format!("{} is not installed", a.tool),
            )
            .with_hint(hint));
        };
        let (source, caps) = if a.probe {
            let Some(exe) = &t.entrypoint else {
                return Err(KimError::new(
                    ErrorCode::NotInstalled,
                    format!(
                        "{} cannot be asked: its install is damaged ({})",
                        a.tool,
                        t.damaged.as_deref().unwrap_or_default()
                    ),
                )
                .with_hint(format!(
                    "repair it explicitly with `kim install {} --reinstall`",
                    a.tool
                )));
            };
            diag(format!(
                "probing {} capabilities --json (time limit {} s)",
                exe.display(),
                a.timeout
            ));
            let c = capabilities::probe(
                exe,
                &a.tool,
                t.version.as_deref(),
                Duration::from_secs(a.timeout),
            )?;
            (CapsSource::Probe, Some(c))
        } else {
            match manifest_of(&t) {
                (s, Some(Ok(c))) => (s, Some(c)),
                (_, Some(Err(e))) => return Err(e),
                (s, None) => (s, None),
            }
        };
        (Some(t.clone()), t.version, caps, source)
    };

    let requirements = check_requirements(
        &a.tool,
        version.as_deref(),
        caps.as_ref(),
        version_req
            .as_ref()
            .map(|r| (r, a.require_version.as_deref().unwrap_or(""))),
        &a.require_command,
    )?;

    let tool = entry(
        &a.tool,
        &loaded,
        &paths,
        installed.as_ref(),
        (source, caps.as_ref()),
    );
    let mut human = format!(
        "{} {} ({})\n",
        a.tool,
        version.as_deref().unwrap_or("(unknown version)"),
        if installed.is_some() {
            "installed by Kim"
        } else {
            "the running Kim"
        }
    );
    human.push_str(&format!(
        "  executable    {}\n",
        tool["entrypoint"].as_str().unwrap_or("-")
    ));
    match &caps {
        None => human.push_str(
            "  capabilities  not published with this install (`--probe` asks the tool itself)\n",
        ),
        Some(c) => {
            human.push_str(&format!(
                "  capabilities  {} · CLI contract v{} · JSON schema v{}{}\n\n",
                match source {
                    CapsSource::Builtin => "built in",
                    CapsSource::Manifest => "from the manifest shipped with the install",
                    _ => "from the tool itself (--probe)",
                },
                c.cli_contract,
                c.json_schema_version,
                if c.implementation == capabilities::Implementation::Placeholder {
                    " · PLACEHOLDER (not functional yet)"
                } else {
                    ""
                }
            ));
            let mut rows = vec![vec![
                "COMMAND".to_string(),
                "EFFECT".into(),
                "NETWORK".into(),
                "DRY-RUN".into(),
                "NEEDS --yes".into(),
                "SUMMARY".into(),
            ]];
            let yes_no = |b: bool| if b { "yes" } else { "-" }.to_string();
            rows.extend(c.commands.iter().map(|cmd| {
                vec![
                    cmd.name.clone(),
                    effect_name(cmd.effect).into(),
                    yes_no(cmd.network),
                    yes_no(cmd.dry_run),
                    yes_no(cmd.requires_yes),
                    cmd.summary.clone(),
                ]
            }));
            human.push_str(&table(&rows));
        }
    }
    if requirements.is_some() {
        human.push_str("\nAll requirements are satisfied.\n");
    }
    Ok(Outcome {
        data: json!({
            "tool": tool,
            "capabilities": caps,
            "capabilities_source": source.as_str(),
            "requirements": requirements,
        }),
        human,
        warnings,
    })
}

fn effect_name(e: Effect) -> &'static str {
    match e {
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Install => "install",
        Effect::Destructive => "destructive",
        Effect::Unknown => "unknown",
    }
}

/// Version negotiation (docs/CLI_CONTRACT.md §9.4): returns the satisfied requirements, or the
/// structured error for an old, incompatible or incomplete tool.
fn check_requirements(
    tool: &str,
    version: Option<&str>,
    caps: Option<&Capabilities>,
    version_req: Option<(&semver::VersionReq, &str)>,
    commands: &[String],
) -> Result<Option<Value>, KimError> {
    if version_req.is_none() && commands.is_empty() {
        return Ok(None);
    }
    if let Some((req, text)) = version_req {
        let Some(v) = version.and_then(|v| semver::Version::parse(v).ok()) else {
            return Err(KimError::new(
                ErrorCode::ToolIncompatible,
                format!("the version of {tool} is unknown (damaged install)"),
            )
            .with_hint(format!(
                "repair it explicitly with `kim install {tool} --reinstall`"
            )));
        };
        if !req.matches(&v) {
            return Err(KimError::new(
                ErrorCode::ToolIncompatible,
                format!("{tool} {v} does not satisfy the requirement `{text}`"),
            )
            .with_hint(if tool == "kim" {
                "Kim never updates itself: `kim version` shows whether the catalog lists a \
                 newer Kim; install it manually"
                    .to_string()
            } else {
                format!(
                    "`kim update check` shows whether a newer {tool} exists; updating is \
                     explicit (`kim update {tool}`)"
                )
            }));
        }
    }
    if !commands.is_empty() {
        let Some(c) = caps else {
            return Err(KimError::new(
                ErrorCode::CapabilitiesUnavailable,
                format!(
                    "{tool} publishes no capabilities, so `{}` cannot be confirmed",
                    commands.join("`, `")
                ),
            )
            .with_hint("retry with --probe to ask the tool itself, or update it explicitly"));
        };
        if c.cli_contract != CLI_CONTRACT {
            return Err(KimError::new(
                ErrorCode::ToolIncompatible,
                format!(
                    "{tool} implements CLI contract v{}; this Kim speaks v{CLI_CONTRACT}",
                    c.cli_contract
                ),
            ));
        }
        let missing: Vec<&str> = commands
            .iter()
            .filter(|name| c.command(name).is_none())
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(KimError::new(
                ErrorCode::CapabilityMissing,
                format!(
                    "{tool} {} does not offer: {}",
                    c.version,
                    missing.join(", ")
                ),
            )
            .with_hint(format!("`kim tools describe {tool}` lists what it offers")));
        }
    }
    Ok(Some(json!({
        "version": version_req.map(|(_, t)| t),
        "commands": commands,
        "satisfied": true,
    })))
}

pub fn status() -> Result<Outcome, KimError> {
    let (paths, loaded) = load()?;
    let platform = Platform::current();
    let (settings, settings_path, settings_warning) = match Settings::path() {
        Ok(p) => {
            let (s, w) = Settings::load_from(&p);
            (s, Some(p), w)
        }
        Err(e) => (Settings::default(), None, Some(e.message)),
    };
    let mut warnings = loaded.warnings.clone();
    warnings.extend(settings_warning);
    let installed = inventory::installed_tools(&paths);
    let views = status::evaluate(&loaded.catalog, &paths, &platform, Channel::Stable);
    let updates = install::check_updates(&loaded, &paths, &platform, Channel::Stable);
    let mut problems = Vec::new();
    if loaded.stale {
        problems.push(format!(
            "the catalog expired on {}; run `kim catalog refresh` when online",
            loaded.catalog.expires_at
        ));
    }
    let tools: Vec<Value> = installed
        .iter()
        .map(|t| {
            if let Some(why) = &t.damaged {
                problems.push(format!("{} is damaged: {why}", t.id));
            }
            let status = if t.damaged.is_some() {
                ToolStatus::Damaged
            } else {
                views
                    .iter()
                    .find(|v| v.id == t.id)
                    .map_or(ToolStatus::Installed, |v| v.status)
            };
            json!({ "id": t.id, "version": t.version, "status": status })
        })
        .collect();
    let leftovers = std::fs::read_dir(paths.staging_dir()).map_or(0, |rd| rd.flatten().count());
    if leftovers > 0 {
        problems.push(format!(
            "{leftovers} leftover staging item(s) in {} (removed by the next install/update of that tool)",
            paths.staging_dir().display()
        ));
    }

    let mut human = format!(
        "Kim {} ({} build) · CLI contract v{CLI_CONTRACT}\n",
        kim::KIM_VERSION,
        build_kind()
    );
    human.push_str(&catalog_header(&loaded));
    human.push_str(&format!(
        "Settings: offline_mode={} · check_updates_on_launch={}\n",
        settings.offline_mode, settings.check_updates_on_launch
    ));
    if installed.is_empty() {
        human.push_str("Installed tools: none (besides Kim)\n");
    } else {
        human.push_str("Installed tools:\n");
        for t in &tools {
            human.push_str(&format!(
                "  {} {} — {}\n",
                t["id"].as_str().unwrap_or(""),
                t["version"].as_str().unwrap_or("?"),
                t["status"].as_str().unwrap_or("")
            ));
        }
    }
    if updates.is_empty() {
        human.push_str("Updates: none known from the local catalog\n");
    } else {
        for u in &updates {
            human.push_str(&format!(
                "Update available: {} {} → {} (`kim update {}` installs it)\n",
                u.tool, u.installed, u.available, u.tool
            ));
        }
    }
    for p in &problems {
        human.push_str(&format!("Problem: {p}\n"));
    }
    human.push_str("Status is read-only: nothing was checked online and nothing was changed.\n");
    Ok(Outcome {
        data: json!({
            "kim": {
                "version": kim::KIM_VERSION,
                "build": build_kind(),
                "executable": kim_executable(),
                "cli_contract": CLI_CONTRACT,
            },
            "catalog": catalog_info(&loaded),
            "settings": {
                "check_updates_on_launch": settings.check_updates_on_launch,
                "offline_mode": settings.offline_mode,
                "path": settings_path,
            },
            "paths": {
                "programs": paths.programs,
                "kim_data": paths.kim_data,
            },
            "platform": platform,
            "tools": tools,
            "updates": updates,
            "problems": problems,
        }),
        human,
        warnings,
    })
}

pub fn version() -> Result<Outcome, KimError> {
    let platform = Platform::current();
    let mut warnings = Vec::new();
    // The local catalog says whether a newer Kim exists (offline; Kim never updates itself).
    let latest = match load() {
        Ok((paths, loaded)) => {
            warnings.extend(loaded.warnings.iter().cloned());
            loaded.catalog.tool("kim").and_then(|t| {
                let kim = semver::Version::parse(kim::KIM_VERSION).expect("SemVer");
                status::evaluate_tool(
                    t,
                    &platform,
                    &kim,
                    &status::installed(&paths, "kim"),
                    Channel::Stable,
                )
                .latest_compatible
            })
        }
        Err(e) => {
            warnings.push(format!("cannot read the catalog: {}", e.message));
            None
        }
    };
    let newer = latest.as_deref().is_some_and(|l| {
        matches!(
            (semver::Version::parse(l), semver::Version::parse(kim::KIM_VERSION)),
            (Ok(a), Ok(b)) if a > b
        )
    });
    let mut human = format!(
        "kim {} ({} build, {}/{})\nCLI contract v{CLI_CONTRACT} · JSON schema v{JSON_SCHEMA_VERSION} · catalog schema v{} · capabilities schema v{}\n",
        kim::KIM_VERSION,
        build_kind(),
        platform.os,
        platform.arch,
        kim::catalog::SCHEMA_VERSION,
        capabilities::SCHEMA_VERSION,
    );
    if newer {
        human.push_str(&format!(
            "A newer Kim ({}) is listed in the catalog. Kim never updates itself; install it manually.\n",
            latest.as_deref().unwrap_or("?")
        ));
    }
    Ok(Outcome {
        data: json!({
            "tool": "kim",
            "version": kim::KIM_VERSION,
            "build": build_kind(),
            "cli_contract": CLI_CONTRACT,
            "json_schema_version": JSON_SCHEMA_VERSION,
            "schemas": {
                "catalog": kim::catalog::SCHEMA_VERSION,
                "capabilities": capabilities::SCHEMA_VERSION,
                "install_record": 1,
            },
            "platform": platform,
            "catalog_latest": latest,
            "newer_available": newer,
        }),
        human,
        warnings,
    })
}

pub fn capabilities() -> Outcome {
    let caps = meta::capabilities();
    let mut human = format!(
        "kim {} — CLI contract v{} · JSON schema v{}\n{}\n\n",
        caps.version, caps.cli_contract, caps.json_schema_version, caps.summary
    );
    let mut rows = vec![vec![
        "COMMAND".to_string(),
        "EFFECT".into(),
        "NETWORK".into(),
        "DRY-RUN".into(),
        "NEEDS --yes".into(),
        "USAGE".into(),
    ]];
    let yes_no = |b: bool| if b { "yes" } else { "-" }.to_string();
    rows.extend(caps.commands.iter().map(|c| {
        vec![
            c.name.clone(),
            effect_name(c.effect).into(),
            yes_no(c.network),
            yes_no(c.dry_run),
            yes_no(c.requires_yes),
            c.usage.clone(),
        ]
    }));
    human.push_str(&table(&rows));
    Outcome {
        data: serde_json::to_value(&caps).unwrap_or_default(),
        human,
        warnings: Vec::new(),
    }
}
