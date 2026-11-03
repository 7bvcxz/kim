//! Kim's own capabilities document (`kim capabilities --json`, docs/CLI_CONTRACT.md §9).
//!
//! Unit tests in `main.rs` keep this table identical to the real clap command tree, and check
//! that every command that is not `read` supports `--dry-run`.

use kim::capabilities::{
    CLI_CONTRACT, Capabilities, CommandInfo, Effect, Implementation, JSON_SCHEMA_VERSION, SCHEMA,
    SCHEMA_VERSION,
};

struct Spec {
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    effect: Effect,
    network: bool,
    dry_run: bool,
    requires_yes: bool,
}

const fn read(name: &'static str, usage: &'static str, summary: &'static str) -> Spec {
    Spec {
        name,
        usage,
        summary,
        effect: Effect::Read,
        network: false,
        dry_run: false,
        requires_yes: false,
    }
}

const COMMANDS: &[Spec] = &[
    read(
        "capabilities",
        "kim capabilities",
        "This document: Kim's commands and what each one may change",
    ),
    read(
        "catalog.list",
        "kim catalog list [--channel stable|preview] [--installed]",
        "Tools in the signed catalog and their status on this machine",
    ),
    read(
        "catalog.show",
        "kim catalog show <tool> [--channel stable|preview]",
        "One tool with its releases and the package for this machine",
    ),
    read(
        "catalog.status",
        "kim catalog status",
        "Catalog in use (bundled or cached), signer, freshness and cache location",
    ),
    read(
        "catalog.verify",
        "kim catalog verify <file> [--sig <file>]",
        "Verify any catalog file and its signature",
    ),
    Spec {
        name: "catalog.refresh",
        usage: "kim catalog refresh [--source <url>] [--dry-run]",
        summary: "Fetch and verify the catalog, then update the local cache",
        effect: Effect::Write,
        network: true,
        dry_run: true,
        requires_yes: false,
    },
    Spec {
        name: "install",
        usage: "kim install <tool> [--version <v>] [--channel stable|preview] [--reinstall] \
                [--allow-stale] [--dry-run]",
        summary: "Download, verify and activate a tool (explicit; never implicit)",
        effect: Effect::Install,
        network: true,
        dry_run: true,
        requires_yes: false,
    },
    Spec {
        name: "update.check",
        usage: "kim update check [--refresh [--source <url>]] [--channel stable|preview] \
                [--dry-run]",
        summary: "List available updates; never downloads a package (--refresh updates the \
                  catalog cache first)",
        effect: Effect::Write,
        network: true,
        dry_run: true,
        requires_yes: false,
    },
    Spec {
        name: "update",
        usage: "kim update <tool> [--channel stable|preview] [--allow-stale] [--dry-run]",
        summary: "Install the newest compatible release of an installed tool (explicit)",
        effect: Effect::Install,
        network: true,
        dry_run: true,
        requires_yes: false,
    },
    Spec {
        name: "uninstall",
        usage: "kim uninstall <tool> [--purge-data] [--yes] [--dry-run]",
        summary: "Remove a tool's program files; user data is kept unless --purge-data",
        effect: Effect::Destructive,
        network: false,
        dry_run: true,
        requires_yes: true,
    },
    read(
        "tools.list",
        "kim tools list",
        "Installed Kim tools with version, executable and capabilities (reads files only)",
    ),
    read(
        "tools.describe",
        "kim tools describe <tool> [--probe [--timeout <s>]] [--require-version <req>] \
         [--require-command <name>]...",
        "One installed tool and its capabilities; optionally checks requirements",
    ),
    read(
        "status",
        "kim status",
        "Kim version, catalog freshness, settings, installed tools and available updates",
    ),
    read(
        "version",
        "kim version",
        "Kim's version and the CLI contract and schema versions it speaks",
    ),
    read(
        "settings.show",
        "kim settings show",
        "Settings and the file they are stored in",
    ),
    Spec {
        name: "settings.set",
        usage: "kim settings set <check_updates_on_launch|offline_mode> <true|false> [--dry-run]",
        summary: "Change one setting",
        effect: Effect::Write,
        network: false,
        dry_run: true,
        requires_yes: false,
    },
];

/// Kim's capabilities document.
pub(crate) fn capabilities() -> Capabilities {
    Capabilities {
        schema: SCHEMA.into(),
        schema_version: SCHEMA_VERSION,
        tool: "kim".into(),
        version: kim::KIM_VERSION.into(),
        cli_contract: CLI_CONTRACT,
        json_schema_version: JSON_SCHEMA_VERSION,
        implementation: Implementation::Implemented,
        summary: "Explicit, user-controlled catalog and installer for Kim tools".into(),
        commands: COMMANDS
            .iter()
            .map(|s| CommandInfo {
                name: s.name.into(),
                // Normalize the line-continuation whitespace of long usage strings.
                usage: s.usage.split_whitespace().collect::<Vec<_>>().join(" "),
                summary: s.summary.split_whitespace().collect::<Vec<_>>().join(" "),
                effect: s.effect,
                network: s.network,
                dry_run: s.dry_run,
                requires_yes: s.requires_yes,
                opens_window: false,
            })
            .collect(),
    }
}
